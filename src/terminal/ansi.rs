use super::grid::{Attrs, Color, UnderlineStyle};
use super::{CursorStyle, MouseEncoding, Terminal};
use vte::{Params, Perform};

pub struct AnsiHandler<'a> {
    pub terminal: &'a mut Terminal,
}

impl<'a> AnsiHandler<'a> {
    pub fn new(terminal: &'a mut Terminal) -> Self {
        Self { terminal }
    }

    /// Feed one raw PTY byte to the Kitty graphics (APC) scanner, in
    /// parallel with the normal `vte` feed in `Pane::process_output`. See
    /// `Terminal::feed_apc_byte` for why this can't be done via `Perform`.
    pub fn feed_apc_byte(&mut self, byte: u8) {
        self.terminal.feed_apc_byte(byte);
    }
}

/// First value of each parameter group, in a fixed stack buffer (vte caps a
/// sequence at 32 values), so CSI dispatch never allocates.
struct ParamBuf {
    v: [u16; 32],
    n: usize,
}

impl ParamBuf {
    #[inline]
    fn collect(params: &Params) -> Self {
        let mut b = ParamBuf { v: [0; 32], n: 0 };
        for p in params.iter() {
            if b.n == 32 {
                break;
            }
            b.v[b.n] = p.first().copied().unwrap_or(0);
            b.n += 1;
        }
        b
    }

    #[inline]
    fn as_slice(&self) -> &[u16] {
        &self.v[..self.n]
    }
}

fn arg(ps: &[u16], idx: usize, default: u16) -> u16 {
    let v = ps.get(idx).copied().unwrap_or(0);
    if v == 0 { default } else { v }
}

impl Perform for AnsiHandler<'_> {
    fn print(&mut self, c: char) {
        self.terminal.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => self.terminal.bell = true,
            0x08 => self.terminal.backspace(),
            0x09 => self.terminal.tab(),
            0x0A | 0x0B | 0x0C => self.terminal.linefeed(),
            0x0D => self.terminal.carriage_return(),
            0x0E => self.terminal.active_charset = 1, // SO → switch to G1
            0x0F => self.terminal.active_charset = 0, // SI → switch to G0
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        // SGR is by far the most frequent CSI: handle it before collecting.
        if action == 'm' && intermediates.is_empty() {
            return self.apply_sgr(params);
        }
        let pb = ParamBuf::collect(params);
        let ps = pb.as_slice();
        let n1 = |ps: &[u16]| arg(ps, 0, 1) as usize;
        let t = &mut *self.terminal;

        match (intermediates, action) {
            // ── Cursor movement ──
            ([], 'A') => t.cursor_up(n1(ps)),
            ([], 'B') => t.cursor_down(n1(ps)),
            ([], 'C') | ([], 'a') => t.cursor_forward(n1(ps)),
            ([], 'D') => t.cursor_back(n1(ps)),
            ([], 'e') => t.cursor_down(n1(ps)), // VPR
            ([], 'E') => {
                t.cursor_down(n1(ps));
                t.carriage_return();
            }
            ([], 'F') => {
                t.cursor_up(n1(ps));
                t.carriage_return();
            }
            ([], 'G') | ([], '`') => {
                // CHA / HPA
                let row = t.cursor_row;
                t.set_cursor(row, n1(ps).saturating_sub(1));
            }
            ([], 'H') | ([], 'f') => {
                let row = arg(ps, 0, 1) as usize;
                let col = arg(ps, 1, 1) as usize;
                t.set_cursor_addressed(row.saturating_sub(1), col.saturating_sub(1));
            }
            ([], 'd') => t.set_row_addressed(n1(ps).saturating_sub(1)),
            ([], 'I') => t.tab_forward(n1(ps)),
            ([], 'Z') => t.tab_backward(n1(ps)),
            ([], 'g') => t.clear_tab_stops(arg(ps, 0, 0)),

            // ── Erase / insert / delete ──
            ([], 'J') | ([b'?'], 'J') => t.erase_display(arg(ps, 0, 0)),
            ([], 'K') | ([b'?'], 'K') => t.erase_line(arg(ps, 0, 0)),
            ([], 'L') => t.insert_lines(n1(ps)),
            ([], 'M') => t.delete_lines(n1(ps)),
            ([], 'P') => t.delete_chars(n1(ps)),
            ([], 'S') => t.scroll_up(n1(ps)),
            ([], 'T') => t.scroll_down(n1(ps)),
            ([], 'X') => t.erase_chars(n1(ps)),
            ([], '@') => t.insert_chars(n1(ps)),
            ([], 'b') => t.repeat_last(n1(ps)),

            // ── Modes ──
            ([b'?'], 'h') => self.set_dec_modes(ps, true),
            ([b'?'], 'l') => self.set_dec_modes(ps, false),
            ([], 'h') => {
                for &mode in ps {
                    if mode == 4 {
                        t.insert_mode = true;
                    }
                }
            }
            ([], 'l') => {
                for &mode in ps {
                    if mode == 4 {
                        t.insert_mode = false;
                    }
                }
            }
            // DECRQM
            ([b'?', b'$'], 'p') => {
                let mode = arg(ps, 0, 0);
                let st = t.dec_mode_status(mode);
                t.queue_response(format!("\x1b[?{mode};{st}$y").into_bytes());
            }
            ([b'$'], 'p') => {
                let mode = arg(ps, 0, 0);
                let st = t.ansi_mode_status(mode);
                t.queue_response(format!("\x1b[{mode};{st}$y").into_bytes());
            }
            // DECSTR
            ([b'!'], 'p') => t.soft_reset(),

            // ── SGR ──
            ([], 'm') => {} // handled above

            // ── Reports ──
            ([], 'n') => match arg(ps, 0, 0) {
                5 => t.queue_response(b"\x1b[0n".to_vec()),
                6 => {
                    let row = if t.origin_mode {
                        t.cursor_row.saturating_sub(t.scroll_top)
                    } else {
                        t.cursor_row
                    };
                    let r = format!("\x1b[{};{}R", row + 1, t.cursor_col + 1);
                    t.queue_response(r.into_bytes());
                }
                _ => {}
            },
            ([b'?'], 'n') if arg(ps, 0, 0) == 6 => {
                let row = if t.origin_mode { t.cursor_row.saturating_sub(t.scroll_top) } else { t.cursor_row };
                let r = format!("\x1b[?{};{}R", row + 1, t.cursor_col + 1);
                t.queue_response(r.into_bytes());
            }
            // Primary / Secondary Device Attributes
            ([], 'c') if arg(ps, 0, 0) == 0 => {
                // VT220 with ANSI colour
                t.queue_response(b"\x1b[?62;22c".to_vec());
            }
            ([b'>'], 'c') if arg(ps, 0, 0) == 0 => {
                t.queue_response(b"\x1b[>0;0;0c".to_vec());
            }
            // XTVERSION
            ([b'>'], 'q') => {
                let r = format!("\x1bP>|Rift {}\x1b\\", env!("CARGO_PKG_VERSION"));
                t.queue_response(r.into_bytes());
            }
            // DECSCUSR
            ([b' '], 'q') => {
                t.cursor_style = match arg(ps, 0, 0) {
                    0 | 1 | 2 => CursorStyle::Block,
                    3 | 4 => CursorStyle::Underline,
                    5 | 6 => CursorStyle::Bar,
                    _ => CursorStyle::Block,
                };
            }

            // ── Scroll region / save-restore ──
            ([], 'r') => {
                let top = arg(ps, 0, 1) as usize;
                let bot = arg(ps, 1, t.rows as u16) as usize;
                t.set_scroll_region(top.saturating_sub(1), bot.saturating_sub(1));
            }
            ([], 's') if ps.is_empty() || ps == [0] => t.save_cursor(),
            ([], 'u') => t.restore_cursor(),

            // ── Kitty keyboard protocol ──
            ([b'>'], 'u') => t.kitty_push(arg(ps, 0, 0) as u32),
            ([b'<'], 'u') => t.kitty_pop(n1(ps)),
            ([b'='], 'u') => t.kitty_set(arg(ps, 0, 0) as u32, arg(ps, 1, 1) as u32),
            ([b'?'], 'u') => t.kitty_query(),

            // ── Window ops (title stack, size report) ──
            ([], 't') => match arg(ps, 0, 0) {
                22 => t.push_title(arg(ps, 1, 0)),
                23 => t.pop_title(arg(ps, 1, 0)),
                18 => {
                    let r = format!("\x1b[8;{};{}t", t.rows, t.cols);
                    t.queue_response(r.into_bytes());
                }
                _ => {}
            },
            _ => {}
        }
        self.terminal.note_sync_cursor();
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        // Charset designation: ESC ( 0 → G0=LineDrawing, ESC ( B → G0=ASCII, etc.
        if intermediates.first() == Some(&b'(') {
            match byte {
                b'0' => { self.terminal.g0_charset = super::Charset::LineDrawing; return; }
                b'B' => { self.terminal.g0_charset = super::Charset::Ascii; return; }
                _ => return,
            }
        }
        if intermediates.first() == Some(&b')') {
            match byte {
                b'0' => { self.terminal.g1_charset = super::Charset::LineDrawing; return; }
                b'B' => { self.terminal.g1_charset = super::Charset::Ascii; return; }
                _ => return,
            }
        }
        // DECALN: ESC # 8 fills the screen with 'E'.
        if intermediates.first() == Some(&b'#') {
            if byte == b'8' {
                let t = &mut *self.terminal;
                for row in &mut t.grid {
                    for c in row.iter_mut() {
                        *c = super::Cell::default();
                        c.c = 'E';
                    }
                }
                t.set_cursor(0, 0);
            }
            return;
        }
        if !intermediates.is_empty() {
            return;
        }

        match byte {
            b'7' => self.terminal.save_cursor(),
            b'8' => self.terminal.restore_cursor(),
            b'D' => self.terminal.linefeed(),
            b'E' => {
                self.terminal.carriage_return();
                self.terminal.linefeed();
            }
            b'H' => self.terminal.set_tab_stop(),
            b'M' => self.terminal.reverse_index(),
            b'c' => self.terminal.reset(),
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        if params.is_empty() { return; }
        match params[0] {
            b"0" | b"2" => {
                if params.len() >= 2 {
                    if let Ok(title) = std::str::from_utf8(params[1]) {
                        self.terminal.title = Some(title.to_string());
                        if params[0] == b"0" {
                            self.terminal.icon_title = Some(title.to_string());
                        }
                    }
                }
            }
            b"52" => {
                // OSC 52 — clipboard access
                if params.len() >= 3 {
                    use crate::config::Osc52Policy;
                    let policy = crate::config::osc52_policy();
                    let data = std::str::from_utf8(params[2]).unwrap_or("");
                    if data == "?" {
                        // Reading the clipboard leaks it to whatever is on the other end.
                        self.terminal.clipboard_request = Some(if policy == Osc52Policy::Allow {
                            super::ClipboardRequest::Query
                        } else {
                            super::ClipboardRequest::Blocked { query: true, oversized: false }
                        });
                    } else if !data.is_empty() {
                        self.terminal.clipboard_request = Some(if policy == Osc52Policy::Deny {
                            super::ClipboardRequest::Blocked { query: false, oversized: false }
                        } else if data.len() > crate::config::OSC52_MAX_B64 {
                            super::ClipboardRequest::Blocked { query: false, oversized: true }
                        } else {
                            super::ClipboardRequest::Set(data.to_string())
                        });
                    }
                }
            }
            b"133" => self.terminal.handle_osc133(&params[1..]),
            b"633" => self.terminal.handle_osc633(&params[1..]),
            b"7" => {
                if params.len() >= 2 {
                    // A URI may legally contain ';' — re-join defensively.
                    let uri = params[1..].join(&b';');
                    self.terminal.handle_osc7(&uri);
                }
            }
            b"1337" => self.terminal.handle_osc1337(&params[1..]),
            _ => {
                self.terminal.handle_osc_extra(params, bell_terminated);
            }
        }
    }

    fn hook(&mut self, _: &Params, _: &[u8], _: bool, _: char) {}
    fn put(&mut self, _: u8) {}
    fn unhook(&mut self) {}
}

// ── Private helpers ──

impl AnsiHandler<'_> {
    fn set_dec_modes(&mut self, ps: &[u16], on: bool) {
        for &mode in ps {
            let t = &mut *self.terminal;
            match mode {
                1 => t.app_cursor_keys = on,
                5 => t.reverse_screen = on,
                6 => {
                    t.origin_mode = on;
                    t.set_cursor_addressed(0, 0);
                }
                7 => {
                    t.autowrap = on;
                    if !on {
                        t.wrap_next = false;
                    }
                }
                25 => t.cursor_visible = on,
                47 | 1047 => {
                    if on { t.enter_alt_screen(); } else { t.exit_alt_screen(); }
                }
                1048 => {
                    if on { t.save_cursor(); } else { t.restore_cursor(); }
                }
                1049 => {
                    if on {
                        t.save_cursor();
                        t.enter_alt_screen();
                    } else {
                        t.exit_alt_screen();
                        t.restore_cursor();
                    }
                }
                1000 | 1002 | 1003 => t.set_mouse_flag(mode, on),
                1006 => t.mouse_encoding = if on { MouseEncoding::Sgr } else { MouseEncoding::Default },
                1004 => t.focus_reporting = on,
                1007 => t.alt_scroll = on,
                2004 => t.bracketed_paste = on,
                2026 => t.set_sync_output(on),
                _ => {}
            }
        }
    }

    /// SGR. Works on the raw `Params` so colon sub-parameters
    /// (`38:2::r:g:b`, `4:3`, ...) can be told apart from `;`-separated ones.
    fn apply_sgr(&mut self, params: &Params) {
        let mut groups: [&[u16]; 32] = [&[]; 32];
        let mut n = 0;
        for g in params.iter() {
            if n == 32 {
                break;
            }
            groups[n] = g;
            n += 1;
        }
        apply_sgr_groups(self.terminal, &groups[..n]);
    }
}

/// Apply SGR parameter groups (each group: the value plus its `:` sub-values).
pub(super) fn apply_sgr_groups(t: &mut Terminal, groups: &[&[u16]]) {
    if groups.is_empty() {
        t.fg = Color::Default;
        t.bg = Color::Default;
        t.attrs = Attrs::default();
        return;
    }
    let mut i = 0;
    while i < groups.len() {
        let g = groups[i];
        let code = g.first().copied().unwrap_or(0);
        match code {
            0 => {
                t.fg = Color::Default;
                t.bg = Color::Default;
                t.attrs = Attrs::default();
            }
            1 => t.attrs.bold = true,
            2 => t.attrs.dim = true,
            3 => t.attrs.italic = true,
            4 => {
                let style = if g.len() > 1 {
                    match g[1] {
                        0 => UnderlineStyle::None,
                        2 => UnderlineStyle::Double,
                        3 => UnderlineStyle::Curly,
                        4 => UnderlineStyle::Dotted,
                        5 => UnderlineStyle::Dashed,
                        _ => UnderlineStyle::Single,
                    }
                } else {
                    UnderlineStyle::Single
                };
                t.attrs.underline = style != UnderlineStyle::None;
                t.attrs.underline_style = style;
            }
            5 | 6 => t.attrs.blink = true,
            7 => t.attrs.reverse = true,
            8 => t.attrs.hidden = true,
            9 => t.attrs.strikethrough = true,
            21 => {
                t.attrs.underline = true;
                t.attrs.underline_style = UnderlineStyle::Double;
            }
            22 => {
                t.attrs.bold = false;
                t.attrs.dim = false;
            }
            23 => t.attrs.italic = false,
            24 => {
                t.attrs.underline = false;
                t.attrs.underline_style = UnderlineStyle::None;
            }
            25 => t.attrs.blink = false,
            27 => t.attrs.reverse = false,
            28 => t.attrs.hidden = false,
            29 => t.attrs.strikethrough = false,
            30..=37 => t.fg = Color::Indexed((code - 30) as u8),
            38 | 48 | 58 => {
                if let Some((color, extra)) = parse_extended_color(groups, i) {
                    match code {
                        38 => t.fg = color,
                        48 => t.bg = color,
                        _ => t.attrs.underline_color = Some(color),
                    }
                    i += extra;
                }
            }
            39 => t.fg = Color::Default,
            40..=47 => t.bg = Color::Indexed((code - 40) as u8),
            49 => t.bg = Color::Default,
            53 => t.attrs.overline = true,
            55 => t.attrs.overline = false,
            59 => t.attrs.underline_color = None,
            90..=97 => t.fg = Color::Indexed((code - 90 + 8) as u8),
            100..=107 => t.bg = Color::Indexed((code - 100 + 8) as u8),
            _ => {}
        }
        i += 1;
    }
}

/// Parse the colour after SGR 38 / 48 / 58 at `groups[i]`. Returns the colour
/// and how many *additional* parameter groups it consumed (`;` form).
/// Handles `38;5;n`, `38;2;r;g;b`, `38:5:n`, `38:2:r:g:b` and
/// `38:2:<colorspace>:r:g:b`.
fn parse_extended_color(groups: &[&[u16]], i: usize) -> Option<(Color, usize)> {
    let g = groups[i];
    let b = |v: u16| v.min(255) as u8;
    if g.len() > 1 {
        return match g[1] {
            5 => g.get(2).map(|&n| (Color::Indexed(b(n)), 0)),
            2 => {
                let rgb = match g.len() {
                    n if n >= 6 => &g[3..6],
                    5 => &g[2..5],
                    _ => return None,
                };
                Some((Color::Rgb(b(rgb[0]), b(rgb[1]), b(rgb[2])), 0))
            }
            _ => None,
        };
    }
    let first = |k: usize| groups.get(k).and_then(|g| g.first().copied());
    match first(i + 1)? {
        5 => Some((Color::Indexed(b(first(i + 2)?)), 2)),
        2 => {
            let (r, gr, bl) = (first(i + 2)?, first(i + 3)?, first(i + 4)?);
            Some((Color::Rgb(b(r), b(gr), b(bl)), 4))
        }
        _ => None,
    }
}
