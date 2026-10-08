mod ansi;
pub mod grid;
pub mod images;
pub mod semantic;

pub use ansi::AnsiHandler;
pub use grid::{Attrs, Cell, Color};
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

pub enum ClipboardRequest {
    Set(String),
    Query,
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

    saved_cursor: (usize, usize),
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
}

impl Terminal {
    pub fn new(cols: usize, rows: usize) -> Self {
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
            saved_cursor: (0, 0),
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
            alt_scroll: false,
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
        }
    }

    // ── Character output ──

    pub fn put_char(&mut self, c: char) {
        use unicode_width::UnicodeWidthChar;

        // Apply charset mapping (G0/G1 line drawing)
        let charset = if self.active_charset == 0 { self.g0_charset } else { self.g1_charset };
        let c = if charset == Charset::LineDrawing { map_line_drawing(c) } else { c };

        self.scroll_offset = 0;
        if self.wrap_next {
            self.cursor_col = 0;
            self.linefeed();
            self.wrap_next = false;
        }

        let char_width = c.width().unwrap_or(1).max(1);

        // Wrap if wide char won't fit on current line
        if self.cursor_col + char_width > self.cols {
            self.cursor_col = 0;
            self.linefeed();
        }

        if self.cursor_col < self.cols {
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

            self.grid[self.cursor_row][self.cursor_col] = Cell {
                c,
                fg: self.fg,
                bg: self.bg,
                attrs: self.attrs,
            };

            // Wide char: mark next cell as continuation placeholder
            if char_width == 2 && self.cursor_col + 1 < self.cols {
                self.grid[self.cursor_row][self.cursor_col + 1] = Cell {
                    c: '\0',
                    fg: self.fg,
                    bg: self.bg,
                    attrs: self.attrs,
                };
            }

            self.cursor_col += char_width;
            if self.cursor_col >= self.cols {
                self.cursor_col = self.cols - 1;
                self.wrap_next = true;
            }
        }
    }

    // ── Cursor movement ──

    pub fn set_cursor(&mut self, row: usize, col: usize) {
        self.cursor_row = row.min(self.rows - 1);
        self.cursor_col = col.min(self.cols - 1);
        self.wrap_next = false;
    }

    pub fn cursor_up(&mut self, n: usize) {
        self.cursor_row = self.cursor_row.saturating_sub(n);
        self.wrap_next = false;
    }

    pub fn cursor_down(&mut self, n: usize) {
        self.cursor_row = (self.cursor_row + n).min(self.rows - 1);
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

    pub fn save_cursor(&mut self) {
        self.saved_cursor = (self.cursor_row, self.cursor_col);
    }

    pub fn restore_cursor(&mut self) {
        self.cursor_row = self.saved_cursor.0.min(self.rows - 1);
        self.cursor_col = self.saved_cursor.1.min(self.cols - 1);
        self.wrap_next = false;
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
        loop {
            self.cursor_col += 1;
            if self.cursor_col >= self.cols {
                self.cursor_col = self.cols - 1;
                break;
            }
            if self.tab_stops[self.cursor_col] {
                break;
            }
        }
        self.wrap_next = false;
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
            2 | 3 => {
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
        let row = &mut self.grid[self.cursor_row];
        for _ in 0..n.min(self.cols - self.cursor_col) {
            row.pop();
            row.insert(self.cursor_col, Cell::default());
        }
    }

    pub fn delete_chars(&mut self, n: usize) {
        let row = &mut self.grid[self.cursor_row];
        let n = n.min(self.cols - self.cursor_col);
        for _ in 0..n {
            if self.cursor_col < row.len() {
                row.remove(self.cursor_col);
                row.push(Cell::default());
            }
        }
    }

    pub fn insert_lines(&mut self, n: usize) {
        if self.cursor_row >= self.scroll_top && self.cursor_row <= self.scroll_bottom {
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
        let blank = Cell::blank_with(self.fg, self.bg);
        for _ in 0..n {
            if self.scroll_top < self.scroll_bottom {
                let removed = self.grid.remove(self.scroll_top);
                if !self.using_alt_screen {
                    self.scrollback.push_back(removed);
                    if self.scrollback.len() > self.max_scrollback {
                        self.scrollback.pop_front();
                        self.blocks.shift_lines(1);
                    }
                }
                self.grid.insert(self.scroll_bottom, vec![blank; self.cols]);
            }
        }
    }

    pub fn scroll_down(&mut self, n: usize) {
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
            self.set_cursor(0, 0);
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

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let blank = Cell::default();
        self.grid.resize(rows, vec![blank; cols]);
        for row in &mut self.grid {
            row.resize(cols, blank);
        }
        self.alt_grid.resize(rows, vec![blank; cols]);
        for row in &mut self.alt_grid {
            row.resize(cols, blank);
        }
        self.cols = cols;
        self.rows = rows;
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.cursor_row = self.cursor_row.min(rows - 1);
        self.cursor_col = self.cursor_col.min(cols - 1);
        self.tab_stops = vec![false; cols];
        for i in (0..cols).step_by(8) {
            self.tab_stops[i] = true;
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.cols, self.rows);
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
