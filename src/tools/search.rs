use crate::config::Theme;
use crate::renderer::font::FontManager;
use crate::terminal::grid::Cell;

/// Scrollback search overlay — Ctrl+Shift+F.
pub struct SearchOverlay {
    pub visible: bool,
    pub query: String,
    pub matches: Vec<SearchMatch>,
    pub current_match: usize,
}

/// Case folding that never changes the char count (so cell mapping stays 1:1):
/// first char of the Unicode lowercase mapping.
fn fold(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

pub struct SearchMatch {
    pub row: usize,
    pub col_start: usize,
    pub col_end: usize,
}

impl SearchOverlay {
    pub fn new() -> Self {
        Self {
            visible: false,
            query: String::new(),
            matches: Vec::new(),
            current_match: 0,
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.query.clear();
            self.matches.clear();
            self.current_match = 0;
        }
    }

    pub fn handle_key(&mut self, key: SearchKey) -> Option<SearchAction> {
        match key {
            SearchKey::Char(c) => {
                self.query.push(c);
                Some(SearchAction::UpdateSearch)
            }
            SearchKey::Space => {
                self.query.push(' ');
                Some(SearchAction::UpdateSearch)
            }
            SearchKey::Backspace => {
                self.query.pop();
                if self.query.is_empty() {
                    self.matches.clear();
                    self.current_match = 0;
                }
                Some(SearchAction::UpdateSearch)
            }
            SearchKey::Enter => {
                if !self.matches.is_empty() {
                    self.current_match = (self.current_match + 1) % self.matches.len();
                    Some(SearchAction::JumpToMatch(self.current_match))
                } else {
                    None
                }
            }
            SearchKey::ShiftEnter => {
                if !self.matches.is_empty() {
                    self.current_match = self
                        .current_match
                        .checked_sub(1)
                        .unwrap_or(self.matches.len() - 1);
                    Some(SearchAction::JumpToMatch(self.current_match))
                } else {
                    None
                }
            }
            SearchKey::Escape => {
                self.visible = false;
                None
            }
        }
    }

    /// Search through scrollback buffer and visible grid (case-insensitive).
    ///
    /// Works on *cells*, not bytes: reported columns are cell columns (so CJK and
    /// accented text highlight in the right place), wide-glyph continuation cells
    /// are skipped, and rows that soft-wrapped (`Cell::wrap` on the last cell) are
    /// searched as one logical line, so a match split across the wrap is found. A
    /// wrapped match is reported on its first row, highlighted up to that row's end.
    pub fn search(&mut self, scrollback: &std::collections::VecDeque<Vec<Cell>>, grid: &[Vec<Cell>]) {
        self.matches.clear();
        self.current_match = 0;
        let query: Vec<char> = self.query.chars().map(fold).collect();
        if query.is_empty() {
            return;
        }

        let sb = scrollback.len();
        let total = sb + grid.len();
        let row_at = |i: usize| -> &[Cell] { if i < sb { &scrollback[i] } else { &grid[i - sb] } };
        // Scrollback rows are stored without their trailing default blanks;
        // search them as if they were still full width.
        let cols = grid.first().map_or(0, |r| r.len());

        // (row, first cell col, cell col just past the glyph) per searchable char.
        let mut chars: Vec<char> = Vec::new();
        let mut pos: Vec<(usize, usize, usize)> = Vec::new();
        let mut i = 0;
        while i < total {
            chars.clear();
            pos.clear();
            loop {
                let row = row_at(i);
                let mut col = 0;
                while col < row.len() {
                    let cell = &row[col];
                    if cell.c == '\0' {
                        col += 1;
                        continue;
                    }
                    let mut next = col + 1;
                    while next < row.len() && row[next].c == '\0' {
                        next += 1;
                    }
                    chars.push(fold(cell.c));
                    pos.push((i, col, next));
                    col = next;
                }
                let wraps = row.last().map_or(false, |c| c.wrap());
                if !wraps {
                    for col in row.len()..cols {
                        chars.push(' ');
                        pos.push((i, col, col + 1));
                    }
                }
                i += 1;
                if !wraps || i >= total {
                    break;
                }
            }
            if chars.len() < query.len() {
                continue;
            }
            for st in 0..=chars.len() - query.len() {
                if chars[st] != query[0] || chars[st..st + query.len()] != query[..] {
                    continue;
                }
                let (row, col_start, _) = pos[st];
                let (end_row, _, end_col) = pos[st + query.len() - 1];
                let col_end = if end_row == row { end_col } else { row_at(row).len() };
                self.matches.push(SearchMatch { row, col_start, col_end });
            }
        }

        if !self.matches.is_empty() {
            self.current_match = self.matches.len() - 1;
        }
    }

    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut FontManager,
        theme: &Theme,
    ) {
        use crate::ui::kit::{Ctx, Rect, Tokens};
        if !self.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);

        // Floating find bar, top-right: [ input ........ n/m ] [Esc]
        let bar_w = (44 * tk.cw).min(width.saturating_sub(2 * tk.sp.lg));
        let bar_h = tk.input_h + 2 * tk.sp.sm;
        let bar = Rect::new(width.saturating_sub(bar_w + tk.sp.lg), tk.sp.lg, bar_w, bar_h);
        let inner = cx.float(bar);

        let count_str = format!(
            "{}/{}",
            if self.matches.is_empty() { 0 } else { self.current_match + 1 },
            self.matches.len()
        );
        let count_w = cx.tw(&count_str);
        let esc_w = cx.kbd_hint_w("Esc", "").saturating_sub(tk.sp.sm);

        // Right cluster (count + Esc cap), then the input fills the rest.
        let esc_x = inner.right().saturating_sub(esc_w);
        cx.kbd_chip(esc_x, inner.y, inner.h, "Esc");
        let count_x = esc_x.saturating_sub(count_w + tk.sp.md);
        let count_color = if self.matches.is_empty() && !self.query.is_empty() { tk.danger } else { tk.text_muted };
        let cy = cx.text_y(inner.y, inner.h);
        cx.text(count_x, cy, &count_str, count_color);

        let input = Rect::new(inner.x, inner.y, count_x.saturating_sub(inner.x + tk.sp.md), inner.h);
        cx.text_input(input, &self.query, self.query.chars().count(), None, "Find in scrollback", true);
    }
}

pub enum SearchKey {
    Char(char),
    Space,
    Backspace,
    Enter,
    ShiftEnter,
    Escape,
}

pub enum SearchAction {
    UpdateSearch,
    JumpToMatch(usize),
}
