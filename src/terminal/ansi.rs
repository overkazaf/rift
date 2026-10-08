use super::grid::{Attrs, Color};
use super::{CursorStyle, MouseEncoding, MouseMode, Terminal};
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

fn collect(params: &Params) -> Vec<u16> {
    params.iter().map(|p| p.first().copied().unwrap_or(0)).collect()
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

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let ps = collect(params);
        let private = intermediates.first() == Some(&b'?');

        match action {
            'A' => self.terminal.cursor_up(arg(&ps, 0, 1) as usize),
            'B' => self.terminal.cursor_down(arg(&ps, 0, 1) as usize),
            'C' => self.terminal.cursor_forward(arg(&ps, 0, 1) as usize),
            'D' => self.terminal.cursor_back(arg(&ps, 0, 1) as usize),
            'E' => {
                self.terminal.cursor_down(arg(&ps, 0, 1) as usize);
                self.terminal.carriage_return();
            }
            'F' => {
                self.terminal.cursor_up(arg(&ps, 0, 1) as usize);
                self.terminal.carriage_return();
            }
            'G' => {
                let col = arg(&ps, 0, 1) as usize;
                self.terminal.set_cursor(self.terminal.cursor_row, col.saturating_sub(1));
            }
            'H' | 'f' => {
                let row = arg(&ps, 0, 1) as usize;
                let col = arg(&ps, 1, 1) as usize;
                self.terminal.set_cursor(row.saturating_sub(1), col.saturating_sub(1));
            }
            'J' => self.terminal.erase_display(arg(&ps, 0, 0)),
            'K' => self.terminal.erase_line(arg(&ps, 0, 0)),
            'L' => self.terminal.insert_lines(arg(&ps, 0, 1) as usize),
            'M' => self.terminal.delete_lines(arg(&ps, 0, 1) as usize),
            'P' => self.terminal.delete_chars(arg(&ps, 0, 1) as usize),
            'S' => self.terminal.scroll_up(arg(&ps, 0, 1) as usize),
            'T' => self.terminal.scroll_down(arg(&ps, 0, 1) as usize),
            'X' => self.terminal.erase_chars(arg(&ps, 0, 1) as usize),
            '@' => self.terminal.insert_chars(arg(&ps, 0, 1) as usize),
            'd' => {
                let row = arg(&ps, 0, 1) as usize;
                self.terminal.set_cursor(row.saturating_sub(1), self.terminal.cursor_col);
            }
            'h' if private => self.set_dec_modes(&ps, true),
            'l' if private => self.set_dec_modes(&ps, false),
            'm' => self.apply_sgr(&ps),
            'n' if arg(&ps, 0, 0) == 6 => {
                let r = format!(
                    "\x1b[{};{}R",
                    self.terminal.cursor_row + 1,
                    self.terminal.cursor_col + 1
                );
                self.terminal.response_queue.push(r.into_bytes());
            }
            'r' => {
                let top = arg(&ps, 0, 1) as usize;
                let bot = arg(&ps, 1, self.terminal.rows as u16) as usize;
                self.terminal.set_scroll_region(top.saturating_sub(1), bot.saturating_sub(1));
            }
            'q' if intermediates.first() == Some(&b' ') => {
                self.terminal.cursor_style = match arg(&ps, 0, 0) {
                    0 | 1 | 2 => CursorStyle::Block,
                    3 | 4 => CursorStyle::Underline,
                    5 | 6 => CursorStyle::Bar,
                    _ => CursorStyle::Block,
                };
            }
            // Primary/Secondary Device Attributes
            'c' => {
                if intermediates.first() == Some(&b'>') {
                    // Secondary DA — report as "rift" terminal
                    self.terminal.response_queue.push(b"\x1b[>0;0;0c".to_vec());
                } else {
                    // Primary DA — report as VT220 with ANSI color
                    self.terminal.response_queue.push(b"\x1b[?62;22c".to_vec());
                }
            }
            // ANSI Save/Restore Cursor
            's' if intermediates.is_empty() && ps.is_empty() => {
                self.terminal.save_cursor();
            }
            'u' if intermediates.is_empty() => {
                self.terminal.restore_cursor();
            }
            // Standard (non-DEC) Set/Reset Mode
            'h' if !private => {
                for &mode in &ps {
                    match mode {
                        4 => self.terminal.insert_mode = true,
                        _ => {}
                    }
                }
            }
            'l' if !private => {
                for &mode in &ps {
                    match mode {
                        4 => self.terminal.insert_mode = false,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
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

        match byte {
            b'7' => self.terminal.save_cursor(),
            b'8' => self.terminal.restore_cursor(),
            b'D' => self.terminal.linefeed(),
            b'E' => {
                self.terminal.carriage_return();
                self.terminal.linefeed();
            }
            b'M' => self.terminal.reverse_index(),
            b'c' => self.terminal.reset(),
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() { return; }
        match params[0] {
            b"0" | b"2" => {
                if params.len() >= 2 {
                    if let Ok(title) = std::str::from_utf8(params[1]) {
                        self.terminal.title = Some(title.to_string());
                    }
                }
            }
            b"52" => {
                // OSC 52 — clipboard access
                if params.len() >= 3 {
                    let data = std::str::from_utf8(params[2]).unwrap_or("");
                    if data == "?" {
                        self.terminal.clipboard_request = Some(super::ClipboardRequest::Query);
                    } else if !data.is_empty() {
                        // Decode base64 and set clipboard
                        self.terminal.clipboard_request = Some(super::ClipboardRequest::Set(data.to_string()));
                    }
                }
            }
            b"133" => self.terminal.handle_osc133(&params[1..]),
            b"7" => {
                if params.len() >= 2 {
                    // A URI may legally contain ';' — re-join defensively.
                    let uri = params[1..].join(&b';');
                    self.terminal.handle_osc7(&uri);
                }
            }
            b"1337" => self.terminal.handle_osc1337(&params[1..]),
            _ => {}
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
            match mode {
                1 => self.terminal.app_cursor_keys = on,
                25 => self.terminal.cursor_visible = on,
                47 | 1047 | 1049 => {
                    if on { self.terminal.enter_alt_screen(); }
                    else  { self.terminal.exit_alt_screen(); }
                }
                1000 => self.terminal.mouse_mode = if on { MouseMode::Press } else { MouseMode::None },
                1002 => self.terminal.mouse_mode = if on { MouseMode::ButtonTrack } else { MouseMode::None },
                1003 => self.terminal.mouse_mode = if on { MouseMode::AnyEvent } else { MouseMode::None },
                1006 => self.terminal.mouse_encoding = if on { MouseEncoding::Sgr } else { MouseEncoding::Default },
                1004 => self.terminal.focus_reporting = on,
                1007 => self.terminal.alt_scroll = on,
                2004 => self.terminal.bracketed_paste = on,
                _ => {}
            }
        }
    }

    fn apply_sgr(&mut self, ps: &[u16]) {
        if ps.is_empty() {
            self.terminal.fg = Color::Default;
            self.terminal.bg = Color::Default;
            self.terminal.attrs = Attrs::default();
            return;
        }
        let mut i = 0;
        while i < ps.len() {
            match ps[i] {
                0 => {
                    self.terminal.fg = Color::Default;
                    self.terminal.bg = Color::Default;
                    self.terminal.attrs = Attrs::default();
                }
                1 => self.terminal.attrs.bold = true,
                2 => self.terminal.attrs.dim = true,
                3 => self.terminal.attrs.italic = true,
                4 => self.terminal.attrs.underline = true,
                7 => self.terminal.attrs.reverse = true,
                8 => self.terminal.attrs.hidden = true,
                21 | 22 => {
                    self.terminal.attrs.bold = false;
                    self.terminal.attrs.dim = false;
                }
                23 => self.terminal.attrs.italic = false,
                24 => self.terminal.attrs.underline = false,
                27 => self.terminal.attrs.reverse = false,
                28 => self.terminal.attrs.hidden = false,
                30..=37 => self.terminal.fg = Color::Indexed((ps[i] - 30) as u8),
                38 => { i += 1; self.parse_extended_color(ps, &mut i, true); }
                39 => self.terminal.fg = Color::Default,
                40..=47 => self.terminal.bg = Color::Indexed((ps[i] - 40) as u8),
                48 => { i += 1; self.parse_extended_color(ps, &mut i, false); }
                49 => self.terminal.bg = Color::Default,
                90..=97 => self.terminal.fg = Color::Indexed((ps[i] - 90 + 8) as u8),
                100..=107 => self.terminal.bg = Color::Indexed((ps[i] - 100 + 8) as u8),
                _ => {}
            }
            i += 1;
        }
    }

    fn parse_extended_color(&mut self, ps: &[u16], i: &mut usize, is_fg: bool) {
        if *i >= ps.len() { return; }
        let color = match ps[*i] {
            5 if *i + 1 < ps.len() => {
                *i += 1;
                Color::Indexed(ps[*i] as u8)
            }
            2 if *i + 3 < ps.len() => {
                let c = Color::Rgb(ps[*i + 1] as u8, ps[*i + 2] as u8, ps[*i + 3] as u8);
                *i += 3;
                c
            }
            _ => return,
        };
        if is_fg { self.terminal.fg = color; }
        else     { self.terminal.bg = color; }
    }
}
