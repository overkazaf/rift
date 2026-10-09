//! Stage A hardening features that hang off `Terminal` (child module so it can
//! reach private state): synchronized output, kitty keyboard protocol state,
//! title stack, OSC 8 hyperlinks, OSC 9/777 notifications, OSC 4/10/11/12/104
//! colour queries and overrides, DECRQM, DECSTR, mouse mode tracking.

use super::{Attrs, Cell, Charset, Color, MouseMode, Terminal};
use std::time::{Duration, Instant};

/// Safety timeout for synchronized output (mode 2026): rendering resumes even
/// if the application never sends the end marker.
pub const SYNC_TIMEOUT: Duration = Duration::from_millis(150);

const MAX_URI: usize = 8192;
const MAX_NOTIFICATIONS: usize = 32;
const MAX_TITLE_STACK: usize = 10;
const KITTY_STACK_MAX: usize = 16;

/// OSC 8 link target. Index 0 of `Terminal::hyperlinks` is an empty placeholder.
#[derive(Clone, Default, Debug)]
pub struct Hyperlink {
    pub id: Option<String>,
    pub uri: String,
}

/// Desktop notification requested by OSC 9 / OSC 777;notify.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
}

/// Default xterm colours 0..16 (used for OSC 4 queries until the app sets the theme).
pub const XTERM_PALETTE16: [(u8, u8, u8); 16] = [
    (0, 0, 0), (205, 0, 0), (0, 205, 0), (205, 205, 0),
    (0, 0, 238), (205, 0, 205), (0, 205, 205), (229, 229, 229),
    (127, 127, 127), (255, 0, 0), (0, 255, 0), (255, 255, 0),
    (92, 92, 255), (255, 0, 255), (0, 255, 255), (255, 255, 255),
];

impl Terminal {
    // ── Synchronized output (DEC 2026) ──

    pub fn set_sync_output(&mut self, on: bool) {
        if on {
            if !self.sync_output {
                self.sync_since = Some(Instant::now());
            }
        } else {
            self.sync_since = None;
        }
        self.sync_output = on;
    }

    /// True while the app asked to buffer rendering (mode 2026 set) and the
    /// 150 ms safety timeout has not elapsed. The renderer / app should skip
    /// presenting a frame while this is true.
    pub fn sync_pending(&self) -> bool {
        self.sync_output && self.sync_since.map_or(false, |t| t.elapsed() < SYNC_TIMEOUT)
    }

    /// Time left until a pending synchronized-output frame is force-presented.
    pub fn sync_remaining(&self) -> Option<Duration> {
        if !self.sync_output {
            return None;
        }
        self.sync_since.map(|t| SYNC_TIMEOUT.saturating_sub(t.elapsed())).filter(|d| !d.is_zero())
    }

    // ── Mouse tracking (1000 / 1002 / 1003 are independent flags) ──

    pub(super) fn set_mouse_flag(&mut self, mode: u16, on: bool) {
        match mode {
            1000 => self.mouse_1000 = on,
            1002 => self.mouse_1002 = on,
            1003 => self.mouse_1003 = on,
            _ => {}
        }
        self.mouse_mode = if self.mouse_1003 {
            MouseMode::AnyEvent
        } else if self.mouse_1002 {
            MouseMode::ButtonTrack
        } else if self.mouse_1000 {
            MouseMode::Press
        } else {
            MouseMode::None
        };
    }

    // ── Kitty keyboard protocol state ──

    fn kitty_stack(&mut self) -> &mut Vec<u32> {
        if self.using_alt_screen { &mut self.kitty_alt } else { &mut self.kitty_main }
    }

    /// Active kitty keyboard enhancement flags for the current screen
    /// (bit 0 disambiguate, 1 report event types, 2 alternate keys,
    /// 3 all keys as escapes, 4 associated text). 0 = legacy encoding.
    pub fn kitty_keyboard_flags(&self) -> u32 {
        let st = if self.using_alt_screen { &self.kitty_alt } else { &self.kitty_main };
        st.last().copied().unwrap_or(0)
    }

    /// Depth of the current screen's kitty flag stack.
    pub fn kitty_keyboard_stack_depth(&self) -> usize {
        if self.using_alt_screen { self.kitty_alt.len() } else { self.kitty_main.len() }
    }

    /// `CSI > flags u`
    pub fn kitty_push(&mut self, flags: u32) {
        let st = self.kitty_stack();
        if st.len() >= KITTY_STACK_MAX {
            st.remove(0);
        }
        st.push(flags & 0x1f);
    }

    /// `CSI < n u`
    pub fn kitty_pop(&mut self, n: usize) {
        let st = self.kitty_stack();
        let k = n.min(st.len());
        st.truncate(st.len() - k);
    }

    /// `CSI = flags ; mode u` (mode 1 set, 2 or, 3 and-not).
    pub fn kitty_set(&mut self, flags: u32, mode: u32) {
        let flags = flags & 0x1f;
        let st = self.kitty_stack();
        if st.is_empty() {
            st.push(0);
        }
        let top = st.last_mut().unwrap();
        match mode {
            2 => *top |= flags,
            3 => *top &= !flags,
            _ => *top = flags,
        }
    }

    /// `CSI ? u`
    pub(super) fn kitty_query(&mut self) {
        let r = format!("\x1b[?{}u", self.kitty_keyboard_flags());
        self.queue_response(r.into_bytes());
    }

    // ── Title stack (CSI 22 / 23 t) ──

    /// `which`: 0 = icon + window, 1 = icon, 2 = window.
    pub(super) fn push_title(&mut self, which: u16) {
        if self.title_stack.len() >= MAX_TITLE_STACK {
            self.title_stack.remove(0);
        }
        let t = if which == 1 { self.icon_title.clone() } else { self.title.clone() };
        self.title_stack.push(t);
    }

    pub(super) fn pop_title(&mut self, which: u16) {
        if let Some(t) = self.title_stack.pop() {
            if which == 1 {
                self.icon_title = t;
            } else {
                self.title = t;
            }
        }
    }

    // ── OSC 8 hyperlinks ──

    /// Start (non-empty `uri`) or end (empty) a hyperlink run.
    pub(super) fn set_hyperlink(&mut self, id: Option<String>, uri: &str) {
        if uri.is_empty() || uri.len() > MAX_URI {
            self.cur_link = 0;
            return;
        }
        let n = self.hyperlinks.len();
        let lo = n.saturating_sub(64).max(1);
        if let Some(p) = (lo..n).rev().find(|&i| self.hyperlinks[i].uri == uri && self.hyperlinks[i].id == id) {
            self.cur_link = p as u16;
            return;
        }
        if n >= u16::MAX as usize {
            // Table full: forget every stored link and start over.
            for row in self.grid.iter_mut().chain(self.alt_grid.iter_mut()) {
                row.iter_mut().for_each(|c| c.link = 0);
            }
            for row in self.scrollback.iter_mut() {
                row.iter_mut().for_each(|c| c.link = 0);
            }
            self.hyperlinks.truncate(1);
        }
        self.hyperlinks.push(Hyperlink { id, uri: uri.to_string() });
        self.cur_link = (self.hyperlinks.len() - 1) as u16;
    }

    /// URI of a link id stored in `Cell::link` (None for 0 / unknown).
    pub fn hyperlink_uri(&self, link: u16) -> Option<&str> {
        if link == 0 {
            return None;
        }
        self.hyperlinks.get(link as usize).map(|h| h.uri.as_str())
    }

    /// OSC 8 URI under a cell of the *viewport* (row 0 = top visible row,
    /// honouring scrollback offset).
    pub fn hyperlink_at(&self, row: usize, col: usize) -> Option<&str> {
        self.hyperlink_at_abs(self.view_top_abs() + row, col)
    }

    /// OSC 8 URI under absolute line `abs` (scrollback + grid) and column.
    pub fn hyperlink_at_abs(&self, abs: usize, col: usize) -> Option<&str> {
        let line = self.abs_line(abs)?;
        let mut cell = line.get(col)?;
        if cell.c == '\0' && col > 0 {
            cell = &line[col - 1];
        }
        self.hyperlink_uri(cell.link)
    }

    // ── Notifications (OSC 9 / OSC 777) ──

    fn push_notification(&mut self, title: String, body: String) {
        if self.notifications.len() >= MAX_NOTIFICATIONS {
            self.notifications.remove(0);
        }
        self.notifications.push(Notification { title, body });
    }

    /// Drain queued notifications.
    pub fn take_notifications(&mut self) -> Vec<Notification> {
        std::mem::take(&mut self.notifications)
    }

    // ── Colours ──

    /// Theme colours reported to OSC 10 / 11 / 12 queries.
    pub fn set_reported_colors(&mut self, fg: (u8, u8, u8), bg: (u8, u8, u8), cursor: (u8, u8, u8)) {
        self.reported_fg = fg;
        self.reported_bg = bg;
        self.reported_cursor = cursor;
    }

    /// Theme ANSI colours 0..16 reported to OSC 4 queries.
    pub fn set_reported_palette(&mut self, pal: &[(u8, u8, u8)]) {
        for (i, c) in pal.iter().take(16).enumerate() {
            self.reported_palette[i] = *c;
        }
    }

    /// Palette entry overridden by OSC 4 (renderer: check before using the theme).
    pub fn palette_override(&self, idx: u8) -> Option<(u8, u8, u8)> {
        self.palette_overrides[idx as usize]
    }

    fn palette_color(&self, idx: usize) -> (u8, u8, u8) {
        if let Some(c) = self.palette_overrides[idx] {
            return c;
        }
        if idx < 16 {
            return self.reported_palette[idx];
        }
        if idx < 232 {
            let i = idx - 16;
            let lv = |v: usize| if v == 0 { 0 } else { (55 + 40 * v) as u8 };
            return (lv(i / 36), lv((i / 6) % 6), lv(i % 6));
        }
        let g = (8 + 10 * (idx - 232)) as u8;
        (g, g, g)
    }

    fn osc_reply(&mut self, body: String, bell: bool) {
        let term = if bell { "\x07" } else { "\x1b\\" };
        self.queue_response(format!("\x1b]{body}{term}").into_bytes());
    }

    /// OSC handlers beyond the basics. Returns true when `params[0]` was ours.
    pub(super) fn handle_osc_extra(&mut self, params: &[&[u8]], bell: bool) -> bool {
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        match params[0] {
            b"1" => {
                if let Some(t) = params.get(1) {
                    self.icon_title = Some(text(t));
                }
            }
            b"4" => {
                let mut i = 1;
                while i + 1 < params.len() {
                    let idx = std::str::from_utf8(params[i]).ok().and_then(|s| s.parse::<usize>().ok());
                    if let Some(idx) = idx.filter(|&n| n < 256) {
                        if params[i + 1] == b"?" {
                            let (r, g, b) = self.palette_color(idx);
                            self.osc_reply(format!("4;{idx};{}", rgb_spec(r, g, b)), bell);
                        } else if let Some(c) = parse_color_spec(params[i + 1]) {
                            self.palette_overrides[idx] = Some(c);
                            self.palette_gen = self.palette_gen.wrapping_add(1);
                        }
                    }
                    i += 2;
                }
            }
            b"104" => {
                if params.len() <= 1 || params[1].is_empty() {
                    self.palette_overrides.iter_mut().for_each(|c| *c = None);
                } else {
                    for p in &params[1..] {
                        if let Some(idx) = std::str::from_utf8(p).ok().and_then(|s| s.parse::<usize>().ok()) {
                            if idx < 256 {
                                self.palette_overrides[idx] = None;
                            }
                        }
                    }
                }
                self.palette_gen = self.palette_gen.wrapping_add(1);
            }
            b"10" | b"11" | b"12" => {
                let first = match params[0] {
                    b"10" => 10u8,
                    b"11" => 11,
                    _ => 12,
                };
                for (k, p) in params[1..].iter().enumerate() {
                    let which = first + k as u8;
                    if which > 12 {
                        break;
                    }
                    if *p == b"?" {
                        let (r, g, b) = match which {
                            10 => self.reported_fg,
                            11 => self.reported_bg,
                            _ => self.reported_cursor,
                        };
                        self.osc_reply(format!("{which};{}", rgb_spec(r, g, b)), bell);
                    }
                }
            }
            b"8" => {
                // OSC 8 ; params ; URI  (the URI may itself contain ';')
                let opts = params.get(1).map(|p| text(p)).unwrap_or_default();
                let uri = if params.len() > 2 { params[2..].join(&b';') } else { Vec::new() };
                let uri = text(&uri);
                let id = opts.split(':').find_map(|kv| kv.strip_prefix("id=")).map(|s| s.to_string());
                self.set_hyperlink(id, &uri);
            }
            b"9" => {
                if params.len() >= 2 {
                    let body = text(&params[1..].join(&b';'));
                    // ConEmu extensions (9;4 progress, 9;9 cwd, ...) are not notifications.
                    let conemu = body.split(';').next().map_or(false, |h| !h.is_empty() && h.len() <= 2 && h.bytes().all(|b| b.is_ascii_digit()))
                        && body.contains(';');
                    if !body.is_empty() && !conemu {
                        self.push_notification(String::new(), body);
                    }
                }
            }
            b"777" => {
                if params.get(1) == Some(&&b"notify"[..]) {
                    let title = params.get(2).map(|t| text(t)).unwrap_or_default();
                    let body = if params.len() > 3 { text(&params[3..].join(&b';')) } else { String::new() };
                    if !title.is_empty() || !body.is_empty() {
                        self.push_notification(title, body);
                    }
                }
            }
            _ => return false,
        }
        true
    }

    // ── Mode reporting (DECRQM) ──

    /// Status for a DEC private mode: 0 unknown, 1 set, 2 reset.
    pub(super) fn dec_mode_status(&self, mode: u16) -> u8 {
        let b = |v: bool| if v { 1 } else { 2 };
        match mode {
            1 => b(self.app_cursor_keys),
            5 => b(self.reverse_screen),
            6 => b(self.origin_mode),
            7 => b(self.autowrap),
            25 => b(self.cursor_visible),
            47 | 1047 | 1049 => b(self.using_alt_screen),
            1000 => b(self.mouse_1000),
            1002 => b(self.mouse_1002),
            1003 => b(self.mouse_1003),
            1004 => b(self.focus_reporting),
            1006 => b(self.mouse_encoding == super::MouseEncoding::Sgr),
            1007 => b(self.alt_scroll),
            1048 => 2,
            2004 => b(self.bracketed_paste),
            2026 => b(self.sync_output),
            _ => 0,
        }
    }

    /// Status for an ANSI mode (`CSI Ps $ p`).
    pub(super) fn ansi_mode_status(&self, mode: u16) -> u8 {
        match mode {
            4 => if self.insert_mode { 1 } else { 2 },
            _ => 0,
        }
    }

    /// Queue a reply for the PTY. Bounded so a query flood cannot grow
    /// memory without limit (excess replies are dropped).
    pub(super) fn queue_response(&mut self, r: Vec<u8>) {
        if self.response_queue.len() < 1024 {
            self.response_queue.push(r);
        }
    }

    // ── Soft reset (DECSTR) ──

    pub fn soft_reset(&mut self) {
        self.cursor_visible = true;
        self.insert_mode = false;
        self.origin_mode = false;
        self.autowrap = true;
        self.reverse_screen = false;
        self.app_cursor_keys = false;
        self.fg = Color::Default;
        self.bg = Color::Default;
        self.attrs = Attrs::default();
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
        self.g0_charset = Charset::Ascii;
        self.g1_charset = Charset::Ascii;
        self.active_charset = 0;
        self.saved = super::SavedCursor::default();
        self.cur_link = 0;
        self.wrap_next = false;
    }

    // ── REP (CSI b) ──

    /// Repeat the last printed graphic character `n` times (capped to one screen).
    pub fn repeat_last(&mut self, n: usize) {
        if let Some(c) = self.last_printed {
            for _ in 0..n.min(self.cols * self.rows) {
                self.put_char(c);
            }
        }
    }

    /// Cell-level helper for tests and consumers: text of a viewport row.
    pub fn row_text(&self, row: usize) -> String {
        self.grid.get(row).map(|r| super::grid::cells_text(r)).unwrap_or_default()
    }
}

fn rgb_spec(r: u8, g: u8, b: u8) -> String {
    format!("rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}")
}

/// Parse `rgb:R/G/B` (1-4 hex digits per channel) or `#RGB` / `#RRGGBB` / ...
pub fn parse_color_spec(spec: &[u8]) -> Option<(u8, u8, u8)> {
    let s = std::str::from_utf8(spec).ok()?;
    let scale = |h: &str| -> Option<u8> {
        if h.is_empty() || h.len() > 4 {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        let max = (1u32 << (4 * h.len())) - 1;
        Some(((v * 255 + max / 2) / max) as u8)
    };
    if let Some(rest) = s.strip_prefix("rgb:") {
        let mut it = rest.split('/');
        let (r, g, b) = (it.next()?, it.next()?, it.next()?);
        if it.next().is_some() {
            return None;
        }
        return Some((scale(r)?, scale(g)?, scale(b)?));
    }
    if let Some(hex) = s.strip_prefix('#') {
        let n = hex.len();
        if n == 0 || n % 3 != 0 || n > 12 || !hex.is_ascii() {
            return None;
        }
        let d = n / 3;
        return Some((scale(&hex[..d])?, scale(&hex[d..2 * d])?, scale(&hex[2 * d..])?));
    }
    None
}

#[allow(dead_code)]
fn _assert_cell_small() {
    const _: () = assert!(std::mem::size_of::<Cell>() <= 40);
}
