mod ansi;
mod extras;
mod fastpath;
mod reflow;
#[cfg(test)]
mod tests;
pub mod grid;
pub mod images;
pub mod semantic;

pub use ansi::AnsiHandler;
pub use grid::{Attrs, Cell, Color};
pub use extras::{Hyperlink, Notification};
pub use reflow::{MAX_COLS, MAX_ROWS};
pub use images::{ImageCell, ImageStore, TermImage};
pub use semantic::SemanticMark;

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum MouseMode {
    None,
    Press,
    ButtonTrack,
    AnyEvent,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum MouseEncoding {
    Default,
    Sgr,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum CursorStyle {
    Block,
    Underline,
    Bar,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Charset {
    Ascii,
    LineDrawing,
}

/// DECSC / DECRC state.
#[derive(Clone, Copy)]
pub(crate) struct SavedCursor {
    row: usize,
    col: usize,
    fg: Color,
    bg: Color,
    attrs: Attrs,
    origin: bool,
    g0: Charset,
    g1: Charset,
    active_charset: usize,
    wrap_next: bool,
}

impl Default for SavedCursor {
    fn default() -> Self {
        Self {
            row: 0,
            col: 0,
            fg: Color::Default,
            bg: Color::Default,
            attrs: Attrs::default(),
            origin: false,
            g0: Charset::Ascii,
            g1: Charset::Ascii,
            active_charset: 0,
            wrap_next: false,
        }
    }
}

pub enum ClipboardRequest {
    Set(String),
    Query,
    /// OSC 52 refused by `[security] osc52` (or over the size cap). The app
    /// layer turns this into a one-time toast; `query` = it was a read.
    Blocked { query: bool, oversized: bool },
}

/// Tracks framing of an SOS/PM/APC string (`ESC X`/`ESC ^`/`ESC _` ... ST)
/// at the raw-byte level. This exists because `vte`'s state machine has no
/// `Perform` callback for this content — unlike DCS, which gets
/// `hook`/`put`/`unhook`, bytes inside one of these strings are silently
/// discarded by `vte`'s state table. We track the framing ourselves, in
/// parallel with (and harmlessly alongside) the normal `vte` feed, purely so
/// we can recover Kitty graphics protocol commands (`ESC _ G ... ST`).
#[derive(PartialEq, Eq, Clone, Copy)]
enum ApcScan {
    Idle,
    Esc,
    Apc,
    ApcEsc,
}

pub struct Terminal {
    pub grid: Vec<Vec<Cell>>,
    pub cols: usize,
    pub rows: usize,

    pub cursor_row: usize,
    pub cursor_col: usize,
    pub cursor_visible: bool,

    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,

    pub scroll_top: usize,
    pub scroll_bottom: usize,

    pub app_cursor_keys: bool,
    pub title: Option<String>,
    pub bell: bool,
    pub response_queue: Vec<Vec<u8>>,

    saved: SavedCursor,
    alt_grid: Vec<Vec<Cell>>,
    alt_cursor: (usize, usize),
    using_alt_screen: bool,
    wrap_next: bool,
    tab_stops: Vec<bool>,

    pub scrollback: std::collections::VecDeque<Vec<Cell>>,
    max_scrollback: usize,
    pub scroll_offset: usize,

    pub mouse_mode: MouseMode,
    pub mouse_encoding: MouseEncoding,
    pub cursor_style: CursorStyle,
    pub bracketed_paste: bool,
    pub focus_reporting: bool,
    pub insert_mode: bool,
    pub alt_scroll: bool,
    pub g0_charset: Charset,
    pub g1_charset: Charset,
    pub active_charset: usize,
    pub clipboard_request: Option<ClipboardRequest>,

    /// Shell-reported working directory (OSC 7 / OSC 1337;CurrentDir).
    pub cwd: Option<String>,
    /// Recent OSC 133 semantic marks (bounded).
    pub marks: Vec<SemanticMark>,
    /// Exact command blocks built from OSC 133 marks (per terminal/pane).
    pub blocks: crate::tools::blocks::BlockManager,

    pub image_store: ImageStore,
    apc_scan: ApcScan,
    apc_buffer: Vec<u8>,

    // ── Stage A hardening state ──
    /// DECOM (?6): cursor addressing is relative to the scroll region.
    pub origin_mode: bool,
    /// DECAWM (?7): auto-wrap at the right margin (default on).
    pub autowrap: bool,
    /// DECSCNM (?5): reverse the whole screen's fg/bg (renderer swaps).
    pub reverse_screen: bool,
    sync_output: bool,
    sync_since: Option<std::time::Instant>,
    mouse_1000: bool,
    mouse_1002: bool,
    mouse_1003: bool,
    kitty_main: Vec<u32>,
    kitty_alt: Vec<u32>,
    title_stack: Vec<Option<String>>,
    /// OSC 1 icon title.
    pub icon_title: Option<String>,
    /// OSC 8 link table; index 0 is a placeholder ("no link").
    pub hyperlinks: Vec<Hyperlink>,
    cur_link: u16,
    /// Desktop notifications requested via OSC 9 / OSC 777;notify. The app
    /// drains this (see [`Terminal::take_notifications`]).
    pub notifications: Vec<Notification>,
    reported_fg: (u8, u8, u8),
    reported_bg: (u8, u8, u8),
    reported_cursor: (u8, u8, u8),
    reported_palette: [(u8, u8, u8); 16],
    palette_overrides: Vec<Option<(u8, u8, u8)>>,
    /// Bumped whenever OSC 4 / 104 changes the palette overrides.
    pub palette_gen: u32,
    last_printed: Option<char>,
}

impl Terminal {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.clamp(1, MAX_COLS);
        let rows = rows.clamp(1, MAX_ROWS);
        let grid = vec![vec![Cell::default(); cols]; rows];
        let alt_grid = vec![vec![Cell::default(); cols]; rows];
        let mut tab_stops = vec![false; cols];
        for i in (0..cols).step_by(8) {
            tab_stops[i] = true;
        }
        Self {
            grid,
            cols,
            rows,
            cursor_row: 0,
            cursor_col: 0,
            cursor_visible: true,
            fg: Color::Default,
            bg: Color::Default,
            attrs: Attrs::default(),
            scroll_top: 0,
            scroll_bottom: rows - 1,
            app_cursor_keys: false,
            title: None,
            bell: false,
            response_queue: Vec::new(),
            saved: SavedCursor::default(),
            alt_grid,
            alt_cursor: (0, 0),
            using_alt_screen: false,
            wrap_next: false,
            tab_stops,
            scrollback: std::collections::VecDeque::new(),
            max_scrollback: 10000,
            scroll_offset: 0,
            mouse_mode: MouseMode::None,
            mouse_encoding: MouseEncoding::Default,
            cursor_style: CursorStyle::Block,
            bracketed_paste: false,
            focus_reporting: false,
            insert_mode: false,
            // DECSET 1007 defaults on (iTerm2 / Ghostty): the wheel scrolls
            // full-screen apps with arrow keys until they opt out.
            alt_scroll: true,
            g0_charset: Charset::Ascii,
            g1_charset: Charset::Ascii,
            active_charset: 0,
            clipboard_request: None,
            cwd: None,
            marks: Vec::new(),
            blocks: crate::tools::blocks::BlockManager::new(),
            image_store: ImageStore::new(),
            apc_scan: ApcScan::Idle,
            apc_buffer: Vec::new(),
            origin_mode: false,
            autowrap: true,
            reverse_screen: false,
            sync_output: false,
            sync_since: None,
            mouse_1000: false,
            mouse_1002: false,
            mouse_1003: false,
            kitty_main: Vec::new(),
            kitty_alt: Vec::new(),
            title_stack: Vec::new(),
            icon_title: None,
            hyperlinks: vec![Hyperlink::default()],
            cur_link: 0,
            notifications: Vec::new(),
            reported_fg: (0xd8, 0xd8, 0xd8),
            reported_bg: (0x10, 0x10, 0x14),
            reported_cursor: (0xd8, 0xd8, 0xd8),
            reported_palette: extras::XTERM_PALETTE16,
            palette_overrides: vec![None; 256],
            palette_gen: 0,
            last_printed: None,
        }
    }

    // ── Character output ──

    pub fn put_char(&mut self, c: char) {
        use unicode_width::UnicodeWidthChar;

        // Apply charset mapping (G0/G1 line drawing)
        let charset = if self.active_charset == 0 { self.g0_charset } else { self.g1_charset };
        let c = if charset == Charset::LineDrawing { map_line_drawing(c) } else { c };

        self.scroll_offset = 0;

        // Combining marks, variation selectors, ZWJ sequences, skin tones and
        // regional-indicator pairs join the previous cell instead of advancing.
        if self.try_attach(c) {
            return;
        }

        let char_width = c.width().unwrap_or(1).max(1).min(self.cols);

        if self.wrap_next {
            self.wrap_next = false;
            if self.autowrap {
                self.soft_wrap(false);
            }
            // DECAWM off: the next glyph overwrites the last column.
        }

        // Wrap if wide char won't fit on current line
        if self.cursor_col + char_width > self.cols {
            if self.autowrap {
                self.soft_wrap(true);
            } else {
                return;
            }
        }

        // Insert mode: shift existing chars right before writing
        if self.insert_mode {
            let row = &mut self.grid[self.cursor_row];
            for _ in 0..char_width {
                if self.cursor_col < row.len() {
                    row.pop();
                    row.insert(self.cursor_col, Cell::default());
                }
            }
        }

        let col = self.cursor_col;
        let mut cell = Cell::blank_with(self.fg, self.bg);
        cell.c = c;
        cell.attrs = self.attrs;
        cell.link = self.cur_link;
        let cols = self.cols;
        let row = &mut self.grid[self.cursor_row];
        // Overwriting the right half of a wide char orphans its left half.
        if row[col].c == '\0' && col > 0 {
            row[col - 1].c = ' ';
            row[col - 1].ext = 0;
        }
        row[col] = cell;
        // Wide char: mark next cell as continuation placeholder
        if char_width == 2 && col + 1 < cols {
            let mut cont = cell;
            cont.c = '\0';
            row[col + 1] = cont;
        }
        // Overwriting the left half of a wide char orphans its continuation.
        let after = col + char_width;
        if after < cols && row[after].c == '\0' && row[after].ext != u16::MAX {
            row[after].c = ' ';
        }
        self.last_printed = Some(c);

        self.cursor_col += char_width;
        if self.cursor_col >= self.cols {
            self.cursor_col = self.cols - 1;
            self.wrap_next = true;
        }
    }

    /// Auto-wrap: flag the current row as soft-wrapped (for reflow) and move
    /// to the start of the next line. `spacer` leaves a placeholder in the
    /// last column when a wide glyph did not fit.
    fn soft_wrap(&mut self, spacer: bool) {
        let last = self.cols - 1;
        let (fg, bg) = (self.fg, self.bg);
        let row = &mut self.grid[self.cursor_row];
        if spacer && self.cursor_col <= last {
            let mut sp = Cell::blank_with(fg, bg);
            sp.c = '\0';
            sp.ext = u16::MAX;
            row[last] = sp;
        }
        row[last].wrap = true;
        self.cursor_col = 0;
        self.linefeed();
    }

    /// Previous written cell (the one a combining mark would attach to).
    fn prev_cell_pos(&self) -> Option<(usize, usize)> {
        let row = self.cursor_row;
        let mut col = if self.wrap_next {
            self.cursor_col
        } else if self.cursor_col == 0 {
            return None;
        } else {
            self.cursor_col - 1
        };
        if self.grid[row][col].c == '\0' && col > 0 {
            col -= 1;
        }
        Some((row, col))
    }

    /// Try to attach `c` to the previous cell's grapheme cluster. Returns
    /// true when `c` was consumed (attached or swallowed).
    fn try_attach(&mut self, c: char) -> bool {
        use unicode_width::UnicodeWidthChar;
        let cp = c as u32;
        let zero_w = c.width() == Some(0) && cp >= 0x80;
        let tone = (0x1F3FB..=0x1F3FF).contains(&cp);
        let ri = (0x1F1E6..=0x1F1FF).contains(&cp);
        let maybe_joined = cp >= 0x80;
        if !(zero_w || tone || ri || maybe_joined) {
            return false;
        }
        let Some((r, col)) = self.prev_cell_pos() else {
            // A lone combining mark at the start of a line has nothing to join.
            return zero_w;
        };
        let prev = self.grid[r][col];
        if prev.c == '\0' {
            return zero_w;
        }
        let prev_extra = if prev.ext != 0 { grid::cluster_extra(prev.ext).unwrap_or_default() } else { String::new() };
        let joined = prev_extra.ends_with('\u{200d}');
        let attach = if zero_w || joined {
            true
        } else if ri {
            (0x1F1E6..=0x1F1FF).contains(&(prev.c as u32)) && prev.ext == 0
        } else if tone {
            prev.c as u32 >= 0x203C
        } else {
            false
        };
        if !attach {
            return false;
        }
        if prev_extra.chars().count() < 16 {
            let mut ne = prev_extra;
            ne.push(c);
            if let Some(id) = grid::intern_cluster(&ne) {
                self.grid[r][col].ext = id;
            }
        }
        // Emoji presentation (VS16) and regional-indicator pairs are wide.
        let widen = (c == '\u{fe0f}' && prev.c as u32 >= 0xA9) || ri;
        if widen
            && prev.c.width() == Some(1)
            && !self.wrap_next
            && r == self.cursor_row
            && self.cursor_col == col + 1
            && col + 1 < self.cols
        {
            let mut cont = prev;
            cont.c = '\0';
            cont.ext = 0;
            cont.wrap = false;
            self.grid[r][col + 1] = cont;
            self.cursor_col += 1;
            if self.cursor_col >= self.cols {
                self.cursor_col = self.cols - 1;
                self.wrap_next = true;
            }
        }
        true
    }

    // ── Cursor movement ──

    pub fn set_cursor(&mut self, row: usize, col: usize) {
        self.cursor_row = row.min(self.rows - 1);
        self.cursor_col = col.min(self.cols - 1);
        self.wrap_next = false;
    }

    /// CUP / HVP / VPA addressing: honours DECOM (relative to the scroll
    /// region, confined to it). Rows and columns are 0-based.
    pub fn set_cursor_addressed(&mut self, row: usize, col: usize) {
        let row = self.addressed_row(row);
        self.set_cursor(row, col);
    }

    /// VPA: move to an addressed row keeping the column.
    pub fn set_row_addressed(&mut self, row: usize) {
        let row = self.addressed_row(row);
        self.cursor_row = row;
        self.wrap_next = false;
    }

    fn addressed_row(&self, row: usize) -> usize {
        if self.origin_mode {
            (self.scroll_top + row).min(self.scroll_bottom)
        } else {
            row.min(self.rows - 1)
        }
    }

    pub fn cursor_up(&mut self, n: usize) {
        // Stops at the top margin when the cursor is inside the scroll region.
        let limit = if self.cursor_row >= self.scroll_top { self.scroll_top } else { 0 };
        self.cursor_row = self.cursor_row.saturating_sub(n).max(limit);
        self.wrap_next = false;
    }

    pub fn cursor_down(&mut self, n: usize) {
        let limit = if self.cursor_row <= self.scroll_bottom { self.scroll_bottom } else { self.rows - 1 };
        self.cursor_row = (self.cursor_row + n).min(limit);
        self.wrap_next = false;
    }

    pub fn cursor_forward(&mut self, n: usize) {
        self.cursor_col = (self.cursor_col + n).min(self.cols - 1);
        self.wrap_next = false;
    }

    pub fn cursor_back(&mut self, n: usize) {
        self.cursor_col = self.cursor_col.saturating_sub(n);
        self.wrap_next = false;
    }

    /// DECSC: cursor position, rendition, charsets, origin mode, pending wrap.
    pub fn save_cursor(&mut self) {
        self.saved = SavedCursor {
            row: self.cursor_row,
            col: self.cursor_col,
            fg: self.fg,
            bg: self.bg,
            attrs: self.attrs,
            origin: self.origin_mode,
            g0: self.g0_charset,
            g1: self.g1_charset,
            active_charset: self.active_charset,
            wrap_next: self.wrap_next,
        };
    }

    /// DECRC.
    pub fn restore_cursor(&mut self) {
        let s = self.saved;
        self.cursor_row = s.row.min(self.rows - 1);
        self.cursor_col = s.col.min(self.cols - 1);
        self.fg = s.fg;
        self.bg = s.bg;
        self.attrs = s.attrs;
        self.origin_mode = s.origin;
        self.g0_charset = s.g0;
        self.g1_charset = s.g1;
        self.active_charset = s.active_charset;
        self.wrap_next = s.wrap_next;
    }

    // ── Line control ──

    pub fn linefeed(&mut self) {
        if self.cursor_row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cursor_row < self.rows - 1 {
            self.cursor_row += 1;
        }
    }

    pub fn carriage_return(&mut self) {
        self.cursor_col = 0;
        self.wrap_next = false;
    }

    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
            self.wrap_next = false;
        }
    }

    pub fn tab(&mut self) {
        self.tab_forward(1);
    }

    /// CHT / HT: advance `n` tab stops.
    pub fn tab_forward(&mut self, n: usize) {
        for _ in 0..n.min(self.cols) {
            loop {
                self.cursor_col += 1;
                if self.cursor_col >= self.cols {
                    self.cursor_col = self.cols - 1;
                    break;
                }
                if self.tab_stops.get(self.cursor_col).copied().unwrap_or(false) {
                    break;
                }
            }
        }
        self.wrap_next = false;
    }

    /// CBT: retreat `n` tab stops.
    pub fn tab_backward(&mut self, n: usize) {
        for _ in 0..n.min(self.cols) {
            loop {
                if self.cursor_col == 0 {
                    break;
                }
                self.cursor_col -= 1;
                if self.tab_stops.get(self.cursor_col).copied().unwrap_or(false) {
                    break;
                }
            }
        }
        self.wrap_next = false;
    }

    /// HTS: set a tab stop at the cursor column.
    pub fn set_tab_stop(&mut self) {
        if let Some(t) = self.tab_stops.get_mut(self.cursor_col) {
            *t = true;
        }
    }

    /// TBC: 0 = clear stop at cursor, 3 = clear all.
    pub fn clear_tab_stops(&mut self, mode: u16) {
        match mode {
            0 => {
                if let Some(t) = self.tab_stops.get_mut(self.cursor_col) {
                    *t = false;
                }
            }
            3 => self.tab_stops.iter_mut().for_each(|t| *t = false),
            _ => {}
        }
    }

    pub fn reverse_index(&mut self) {
        if self.cursor_row == self.scroll_top {
            self.scroll_down(1);
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
        }
    }

    // ── Erase ──

    pub fn erase_display(&mut self, mode: u16) {
        let blank = Cell::blank_with(self.fg, self.bg);
        match mode {
            0 => {
                for col in self.cursor_col..self.cols {
                    self.grid[self.cursor_row][col] = blank;
                }
                for row in (self.cursor_row + 1)..self.rows {
                    self.grid[row].fill(blank);
                }
            }
            1 => {
                for row in 0..self.cursor_row {
                    self.grid[row].fill(blank);
                }
                for col in 0..=self.cursor_col.min(self.cols - 1) {
                    self.grid[self.cursor_row][col] = blank;
                }
            }
            3 => self.clear_scrollback(),
            2 => {
                for row in &mut self.grid {
                    row.fill(blank);
                }
            }
            _ => {}
        }
    }

    pub fn erase_line(&mut self, mode: u16) {
        let blank = Cell::blank_with(self.fg, self.bg);
        match mode {
            0 => {
                for col in self.cursor_col..self.cols {
                    self.grid[self.cursor_row][col] = blank;
                }
            }
            1 => {
                for col in 0..=self.cursor_col.min(self.cols - 1) {
                    self.grid[self.cursor_row][col] = blank;
                }
            }
            2 => self.grid[self.cursor_row].fill(blank),
            _ => {}
        }
    }

    pub fn erase_chars(&mut self, n: usize) {
        let blank = Cell::blank_with(self.fg, self.bg);
        let end = (self.cursor_col + n).min(self.cols);
        for col in self.cursor_col..end {
            self.grid[self.cursor_row][col] = blank;
        }
    }

    // ── Insert / Delete ──

    pub fn insert_chars(&mut self, n: usize) {
        let blank = Cell::blank_with(self.fg, self.bg);
        let col = self.cursor_col;
        let n = n.min(self.cols - col);
        if n == 0 {
            return;
        }
        let row = &mut self.grid[self.cursor_row];
        row[col..].rotate_right(n);
        row[col..col + n].fill(blank);
        self.wrap_next = false;
    }

    pub fn delete_chars(&mut self, n: usize) {
        let blank = Cell::blank_with(self.fg, self.bg);
        let col = self.cursor_col;
        let n = n.min(self.cols - col);
        if n == 0 {
            return;
        }
        let cols = self.cols;
        let row = &mut self.grid[self.cursor_row];
        row[col..].rotate_left(n);
        row[cols - n..].fill(blank);
        self.wrap_next = false;
    }

    pub fn insert_lines(&mut self, n: usize) {
        if self.cursor_row >= self.scroll_top && self.cursor_row <= self.scroll_bottom {
            let n = n.min(self.scroll_bottom - self.cursor_row + 1);
            self.cursor_col = 0;
            self.wrap_next = false;
            let blank = Cell::blank_with(self.fg, self.bg);
            for _ in 0..n {
                if self.cursor_row <= self.scroll_bottom {
                    self.grid.remove(self.scroll_bottom);
                    self.grid.insert(self.cursor_row, vec![blank; self.cols]);
                }
            }
        }
    }

    pub fn delete_lines(&mut self, n: usize) {
        if self.cursor_row >= self.scroll_top && self.cursor_row <= self.scroll_bottom {
            let n = n.min(self.scroll_bottom - self.cursor_row + 1);
            self.cursor_col = 0;
            self.wrap_next = false;
            let blank = Cell::blank_with(self.fg, self.bg);
            for _ in 0..n {
                if self.cursor_row <= self.scroll_bottom {
                    self.grid.remove(self.cursor_row);
                    self.grid.insert(self.scroll_bottom, vec![blank; self.cols]);
                }
            }
        }
    }

    // ── Scrolling ──

    pub fn scroll_up(&mut self, n: usize) {
        let n = n.min(self.scroll_bottom - self.scroll_top + 1);
        let blank = Cell::blank_with(self.fg, self.bg);
        for _ in 0..n {
            if self.scroll_top < self.scroll_bottom {
                let removed = self.grid.remove(self.scroll_top);
                // Recycle a row allocation instead of mallocing one per scrolled line:
                // the row evicted from a full scrollback (or the discarded alt-screen
                // row) becomes the new blank line.
                let mut spare: Option<Vec<Cell>> = None;
                if !self.using_alt_screen {
                    self.scrollback.push_back(removed);
                    if self.scrollback.len() > self.max_scrollback {
                        spare = self.scrollback.pop_front();
                        self.blocks.shift_lines(1);
                    }
                } else {
                    spare = Some(removed);
                }
                let new_row = match spare {
                    Some(mut r) => {
                        r.clear();
                        r.resize(self.cols, blank);
                        r
                    }
                    None => vec![blank; self.cols],
                };
                self.grid.insert(self.scroll_bottom, new_row);
            }
        }
    }

    pub fn scroll_down(&mut self, n: usize) {
        let n = n.min(self.scroll_bottom - self.scroll_top + 1);
        let blank = Cell::blank_with(self.fg, self.bg);
        for _ in 0..n {
            if self.scroll_top < self.scroll_bottom {
                self.grid.remove(self.scroll_bottom);
                self.grid.insert(self.scroll_top, vec![blank; self.cols]);
            }
        }
    }

    pub fn set_scroll_region(&mut self, top: usize, bottom: usize) {
        let top = top.min(self.rows - 1);
        let bottom = bottom.min(self.rows - 1);
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
            self.set_cursor_addressed(0, 0);
        }
    }

    // ── Alternate screen ──

    pub fn enter_alt_screen(&mut self) {
        if !self.using_alt_screen {
            self.alt_cursor = (self.cursor_row, self.cursor_col);
            std::mem::swap(&mut self.grid, &mut self.alt_grid);
            self.cursor_row = 0;
            self.cursor_col = 0;
            self.using_alt_screen = true;
            for row in &mut self.grid {
                row.fill(Cell::default());
            }
        }
    }

    pub fn exit_alt_screen(&mut self) {
        if self.using_alt_screen {
            std::mem::swap(&mut self.grid, &mut self.alt_grid);
            self.cursor_row = self.alt_cursor.0.min(self.rows - 1);
            self.cursor_col = self.alt_cursor.1.min(self.cols - 1);
            self.using_alt_screen = false;
        }
    }

    // ── Resize / Reset ──

    /// Full reset (RIS). Keeps the theme-reported colours and scrollback size
    /// the app configured.
    pub fn reset(&mut self) {
        let mut fresh = Self::new(self.cols, self.rows);
        fresh.max_scrollback = self.max_scrollback;
        fresh.reported_fg = self.reported_fg;
        fresh.reported_bg = self.reported_bg;
        fresh.reported_cursor = self.reported_cursor;
        fresh.reported_palette = self.reported_palette;
        *self = fresh;
    }

    // ── Scrollback ──

    pub fn visible_rows(&self) -> Vec<&Vec<Cell>> {
        if self.scroll_offset == 0 {
            self.grid.iter().collect()
        } else {
            let total = self.scrollback.len() + self.grid.len();
            let end = total.saturating_sub(self.scroll_offset);
            let start = end.saturating_sub(self.rows);
            let mut rows = Vec::with_capacity(self.rows);
            for i in start..end {
                if i < self.scrollback.len() {
                    rows.push(&self.scrollback[i]);
                } else {
                    let grid_idx = i - self.scrollback.len();
                    if grid_idx < self.grid.len() {
                        rows.push(&self.grid[grid_idx]);
                    }
                }
            }
            rows
        }
    }

    pub fn scroll_view_up(&mut self, n: usize) {
        self.scroll_offset = (self.scroll_offset + n).min(self.scrollback.len());
    }

    pub fn scroll_view_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn is_scrolled_back(&self) -> bool {
        self.scroll_offset > 0
    }

    /// True while the alternate screen (vim, less, ...) is showing.
    pub fn is_alt_screen(&self) -> bool {
        self.using_alt_screen
    }

    /// DECKPAM application keypad mode (`ESC =` / `ESC >`).
    /// TODO(terminal-core): track ESC = / ESC > in esc_dispatch.
    pub fn app_keypad(&self) -> bool {
        false
    }

    /// Row `abs` of `scrollback ++ grid` (absolute line index).
    pub fn abs_line(&self, abs: usize) -> Option<&[Cell]> {
        let sb = self.scrollback.len();
        if abs < sb {
            self.scrollback.get(abs).map(|r| r.as_slice())
        } else {
            self.grid.get(abs - sb).map(|r| r.as_slice())
        }
    }

    /// Absolute index of the first visible row for the current scroll offset.
    pub fn view_top_abs(&self) -> usize {
        self.scrollback.len().saturating_sub(self.scroll_offset)
    }

    /// Erase scrollback and the visible screen (Clear Buffer). The cursor
    /// keeps its position; the shell is expected to redraw its prompt.
    pub fn clear_buffer(&mut self) {
        // Absolute line numbers (command blocks) shift down by what we drop.
        let dropped = self.scrollback.len();
        self.scrollback.clear();
        self.scroll_offset = 0;
        let blank = Cell::blank_with(self.fg, self.bg);
        for row in &mut self.grid {
            row.fill(blank);
        }
        self.blocks.on_buffer_cleared(dropped);
    }

    /// ED 3: drop the scrollback only (the visible screen is untouched).
    pub fn clear_scrollback(&mut self) {
        let dropped = self.scrollback.len();
        self.scrollback.clear();
        self.scroll_offset = 0;
        self.blocks.shift_lines(dropped);
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    // ── Kitty graphics (APC) ──

    /// Feed one raw PTY byte to the out-of-band APC scanner. See [`ApcScan`]
    /// for why this has to happen outside of `vte`'s `Perform` callbacks.
    /// Safe (and meant) to call on every byte unconditionally, independently
    /// of also feeding the same byte to the `vte` parser — a well-formed
    /// `ESC _ ... ST` run is a no-op as far as `vte`/`Perform` is concerned.
    pub fn feed_apc_byte(&mut self, byte: u8) {
        const MAX_APC_LEN: usize = 64 * 1024 * 1024;
        match self.apc_scan {
            ApcScan::Idle => {
                if byte == 0x1b {
                    self.apc_scan = ApcScan::Esc;
                }
            }
            ApcScan::Esc => {
                if byte == b'_' {
                    self.apc_buffer.clear();
                    self.apc_scan = ApcScan::Apc;
                } else {
                    self.apc_scan = ApcScan::Idle;
                }
            }
            ApcScan::Apc => match byte {
                0x1b => self.apc_scan = ApcScan::ApcEsc,
                // Lenient: accept BEL as a terminator too, in addition to ST.
                0x07 => {
                    self.dispatch_apc();
                    self.apc_scan = ApcScan::Idle;
                }
                _ => {
                    if self.apc_buffer.len() < MAX_APC_LEN {
                        self.apc_buffer.push(byte);
                    }
                }
            },
            ApcScan::ApcEsc => {
                if byte == b'\\' {
                    self.dispatch_apc();
                    self.apc_scan = ApcScan::Idle;
                } else if byte != 0x1b {
                    // Not a valid ST after all — abandon this APC string.
                    self.apc_buffer.clear();
                    self.apc_scan = ApcScan::Idle;
                }
                // else: a stray extra ESC — keep waiting for `\`.
            }
        }
    }

    fn dispatch_apc(&mut self) {
        if self.apc_buffer.first() == Some(&b'G') {
            let payload = self.apc_buffer[1..].to_vec();
            let (row, col) = (self.cursor_row, self.cursor_col);
            self.image_store.handle_kitty_command(row, col, &payload);
        }
        self.apc_buffer.clear();
    }
}

fn map_line_drawing(c: char) -> char {
    match c {
        'j' => '┘', 'k' => '┐', 'l' => '┌', 'm' => '└',
        'n' => '┼', 'q' => '─', 't' => '├', 'u' => '┤',
        'v' => '┴', 'w' => '┬', 'x' => '│',
        'a' => '▒', 'f' => '°', 'g' => '±',
        'h' => '▒', 'i' => '▒',
        'o' => '⎺', 'p' => '⎻', 'r' => '⎼', 's' => '⎽',
        '`' => '◆', '~' => '·',
        'y' => '≤', 'z' => '≥',
        '{' => 'π', '|' => '≠', '}' => '£',
        _ => c,
    }
}
