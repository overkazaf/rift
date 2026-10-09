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
pub use grid::{Attrs, Cell, Color, Grid};
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
    pub grid: Grid,
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
    alt_grid: Grid,
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
    /// Interned (link, underline colour) id stamped on new cells, and the
    /// inputs it was computed from.
    pen_x: u16,
    pen_link: u16,
    pen_ulc: Option<Color>,
    /// Spare scrollback row buffers by capacity class (see `take_row_buf`).
    row_pool: Vec<Vec<Vec<Cell>>>,
}

/// Scrollback rows are allocated in multiples of this many cells.
const ROW_GRANULE: usize = 8;
/// Spare buffers kept per capacity class.
const ROW_POOL_PER_CLASS: usize = 32;

impl Terminal {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.clamp(1, MAX_COLS);
        let rows = rows.clamp(1, MAX_ROWS);
        let grid = Grid::blank(cols, rows);
        let alt_grid = Grid::blank(cols, rows);
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
            pen_x: 0,
            pen_link: 0,
            pen_ulc: None,
            row_pool: Vec::new(),
        }
    }

    /// Extra id (hyperlink + SGR 58 colour) for newly written cells.
    #[inline]
    fn pen_extra(&mut self) -> u16 {
        if self.pen_link != self.cur_link || self.pen_ulc != self.attrs.underline_color {
            self.pen_link = self.cur_link;
            self.pen_ulc = self.attrs.underline_color;
            self.pen_x = grid::intern_extra("", self.cur_link, self.pen_ulc).unwrap_or(0);
        }
        self.pen_x
    }

    // ── Character output ──

    pub fn put_char(&mut self, c: char) {
        use unicode_width::UnicodeWidthChar;

        // Apply charset mapping (G0/G1 line drawing)
        let charset = if self.active_charset == 0 { self.g0_charset } else { self.g1_charset };
        let c = if charset == Charset::LineDrawing { map_line_drawing(c) } else { c };
        let ascii = (' '..='~').contains(&c);

        self.scroll_offset = 0;

        // Combining marks, variation selectors, ZWJ sequences, skin tones and
        // regional-indicator pairs join the previous cell instead of advancing.
        if !ascii && self.try_attach(c) {
            return;
        }

        let char_width = if ascii { 1 } else { c.width().unwrap_or(1).max(1).min(self.cols) };

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
            let r = self.cursor_row;
            let row = &mut self.grid.rows[r];
            for _ in 0..char_width {
                if self.cursor_col < row.len() {
                    row.pop();
                    row.insert(self.cursor_col, Cell::default());
                }
            }
            self.grid.raise(r, self.cols);
        }

        let col = self.cursor_col;
        let x = self.pen_extra();
        let cell = Cell::with_pen(c, self.fg, self.bg, self.attrs.bits(), x);
        let cols = self.cols;
        let r = self.cursor_row;
        let row = &mut self.grid.rows[r];
        // Overwriting the right half of a wide char orphans its left half.
        // (An end-of-row spacer is not the right half of anything.)
        if row[col].c == '\0' && !row[col].is_spacer() && col > 0 {
            row[col - 1].c = ' ';
            row[col - 1].clear_cluster();
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
        if after < cols && row[after].c == '\0' && !row[after].is_spacer() {
            row[after].c = ' ';
        }
        self.grid.raise(r, (col + char_width).min(cols));
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
        let r = self.cursor_row;
        let row = &mut self.grid.rows[r];
        if spacer && self.cursor_col <= last {
            let mut sp = Cell::blank_with(fg, bg);
            sp.c = '\0';
            sp.set_spacer(true);
            row[last] = sp;
        }
        row[last].set_wrap(true);
        self.grid.raise(r, last + 1);
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
        let prev_extra = prev.extra();
        let joined = prev_extra.ends_with('\u{200d}');
        let attach = if zero_w || joined {
            true
        } else if ri {
            (0x1F1E6..=0x1F1FF).contains(&(prev.c as u32)) && !prev.has_cluster()
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
            self.grid[r][col].set_cluster(&ne);
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
            cont.clear_cluster();
            cont.set_wrap(false);
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
        let cols = self.cols;
        match mode {
            0 => {
                self.grid.fill_span(self.cursor_row, self.cursor_col, cols, blank);
                for row in (self.cursor_row + 1)..self.rows {
                    self.grid.fill_row(row, blank);
                }
            }
            1 => {
                for row in 0..self.cursor_row {
                    self.grid.fill_row(row, blank);
                }
                let end = self.cursor_col.min(cols - 1) + 1;
                self.grid.fill_span(self.cursor_row, 0, end, blank);
            }
            3 => self.clear_scrollback(),
            2 => {
                for row in 0..self.grid.rows.len() {
                    self.grid.fill_row(row, blank);
                }
            }
            _ => {}
        }
    }

    pub fn erase_line(&mut self, mode: u16) {
        let blank = Cell::blank_with(self.fg, self.bg);
        let (r, cols) = (self.cursor_row, self.cols);
        match mode {
            0 => self.grid.fill_span(r, self.cursor_col, cols, blank),
            1 => {
                let end = self.cursor_col.min(cols - 1) + 1;
                self.grid.fill_span(r, 0, end, blank);
            }
            2 => self.grid.fill_row(r, blank),
            _ => {}
        }
    }

    pub fn erase_chars(&mut self, n: usize) {
        let blank = Cell::blank_with(self.fg, self.bg);
        let end = (self.cursor_col + n).min(self.cols);
        self.grid.fill_span(self.cursor_row, self.cursor_col, end, blank);
    }

    // ── Insert / Delete ──

    pub fn insert_chars(&mut self, n: usize) {
        let blank = Cell::blank_with(self.fg, self.bg);
        let col = self.cursor_col;
        let n = n.min(self.cols - col);
        if n == 0 {
            return;
        }
        let r = self.cursor_row;
        let row = &mut self.grid.rows[r];
        row[col..].rotate_right(n);
        row[col..col + n].fill(blank);
        self.grid.raise(r, self.cols);
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
        let r = self.cursor_row;
        let row = &mut self.grid.rows[r];
        row[col..].rotate_left(n);
        row[cols - n..].fill(blank);
        if !blank.is_default_blank() {
            self.grid.raise(r, cols);
        }
        self.wrap_next = false;
    }

    pub fn insert_lines(&mut self, n: usize) {
        if self.cursor_row >= self.scroll_top && self.cursor_row <= self.scroll_bottom {
            let n = n.min(self.scroll_bottom - self.cursor_row + 1);
            self.cursor_col = 0;
            self.wrap_next = false;
            let blank = Cell::blank_with(self.fg, self.bg);
            // The bottom `n` rows fall off; their allocations become the new blank rows.
            let (top, bottom) = (self.cursor_row, self.scroll_bottom);
            self.grid.refresh_hints();
            self.grid.rows[top..=bottom].rotate_right(n);
            self.grid.hi[top..=bottom].rotate_right(n);
            for r in top..top + n {
                self.grid.fill_row(r, blank);
            }
        }
    }

    pub fn delete_lines(&mut self, n: usize) {
        if self.cursor_row >= self.scroll_top && self.cursor_row <= self.scroll_bottom {
            let n = n.min(self.scroll_bottom - self.cursor_row + 1);
            self.cursor_col = 0;
            self.wrap_next = false;
            let blank = Cell::blank_with(self.fg, self.bg);
            let (top, bottom) = (self.cursor_row, self.scroll_bottom);
            self.grid.refresh_hints();
            self.grid.rows[top..=bottom].rotate_left(n);
            self.grid.hi[top..=bottom].rotate_left(n);
            for r in bottom + 1 - n..=bottom {
                self.grid.fill_row(r, blank);
            }
        }
    }

    // ── Scrolling ──

    pub fn scroll_up(&mut self, n: usize) {
        let n = n.min(self.scroll_bottom - self.scroll_top + 1);
        if self.scroll_top >= self.scroll_bottom {
            return;
        }
        let (top, bottom) = (self.scroll_top, self.scroll_bottom);
        let blank = Cell::blank_with(self.fg, self.bg);
        let blank_is_default = blank.is_default_blank();
        let keep_history = !self.using_alt_screen && self.max_scrollback > 0;
        self.grid.refresh_hints();
        for _ in 0..n {
            // Rotate the top row to the bottom: only the row headers move, the
            // cell storage stays put and is recycled as the new blank line.
            self.grid.rows[top..=bottom].rotate_left(1);
            self.grid.hi[top..=bottom].rotate_left(1);
            if !keep_history {
                if !self.using_alt_screen {
                    self.blocks.shift_lines(1);
                }
                self.grid.fill_row(bottom, blank);
                continue;
            }
            // Scrollback keeps only the used part of the row (`hi` bounds it, so
            // the blank tail is never even read), in a buffer recycled from the
            // line evicted from the other end: no malloc/free per scrolled line.
            let hi = self.grid.hi[bottom] as usize;
            let keep = grid::trimmed_len(&self.grid.rows[bottom][..hi]);
            if self.scrollback.len() >= self.max_scrollback {
                self.blocks.shift_lines(1);
                if let Some(old) = self.scrollback.pop_front() {
                    self.recycle_row(old);
                }
            }
            let mut stored = self.take_row_buf(keep);
            stored.extend_from_slice(&self.grid.rows[bottom][..keep]);
            self.scrollback.push_back(stored);
            // Cells at `hi..` are already default blanks.
            if blank_is_default {
                self.grid.rows[bottom][..hi].fill(blank);
                self.grid.hi[bottom] = 0;
            } else {
                self.grid.fill_row(bottom, blank);
            }
        }
    }

    /// Capacity class (granule of 8 cells) a scrollback row of `len` cells is allocated in.
    #[inline]
    fn row_class(len: usize) -> usize {
        len.div_ceil(ROW_GRANULE)
    }

    /// An empty buffer able to hold `len` cells, from the spare pool when possible.
    fn take_row_buf(&mut self, len: usize) -> Vec<Cell> {
        let class = Self::row_class(len);
        if let Some(v) = self.row_pool.get_mut(class).and_then(|p| p.pop()) {
            return v;
        }
        Vec::with_capacity(class * ROW_GRANULE)
    }

    /// Return an evicted scrollback row's buffer to the spare pool.
    fn recycle_row(&mut self, mut v: Vec<Cell>) {
        v.clear();
        // Only buffers allocated by `take_row_buf` (capacity an exact class
        // multiple) are pooled; others (reflow, session restore) are freed.
        let cap = v.capacity();
        if cap == 0 || cap % ROW_GRANULE != 0 {
            return;
        }
        let class = cap / ROW_GRANULE;
        if self.row_pool.len() <= class {
            self.row_pool.resize_with(class + 1, Vec::new);
        }
        if self.row_pool[class].len() < ROW_POOL_PER_CLASS {
            self.row_pool[class].push(v);
        }
    }

    pub fn scroll_down(&mut self, n: usize) {
        let n = n.min(self.scroll_bottom - self.scroll_top + 1);
        if self.scroll_top >= self.scroll_bottom {
            return;
        }
        let blank = Cell::blank_with(self.fg, self.bg);
        self.grid.refresh_hints();
        for _ in 0..n {
            self.grid.rows[self.scroll_top..=self.scroll_bottom].rotate_right(1);
            self.grid.hi[self.scroll_top..=self.scroll_bottom].rotate_right(1);
            self.grid.fill_row(self.scroll_top, blank);
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
            for r in 0..self.grid.rows.len() {
                self.grid.fill_row(r, Cell::default());
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
        for r in 0..self.grid.rows.len() {
            self.grid.fill_row(r, blank);
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
