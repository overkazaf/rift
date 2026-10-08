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

    /// Search through scrollback buffer and visible grid.
    pub fn search(&mut self, scrollback: &std::collections::VecDeque<Vec<Cell>>, grid: &[Vec<Cell>]) {
        self.matches.clear();
        if self.query.is_empty() {
            return;
        }

        let query_lower = self.query.to_lowercase();

        for (row_idx, row) in scrollback.iter().enumerate() {
            self.search_row(row, row_idx, &query_lower);
        }

        let offset = scrollback.len();
        for (row_idx, row) in grid.iter().enumerate() {
            self.search_row(row, offset + row_idx, &query_lower);
        }

        if !self.matches.is_empty() {
            self.current_match = self.matches.len() - 1;
        } else {
            self.current_match = 0;
        }
    }

    fn search_row(&mut self, row: &[Cell], abs_row: usize, query: &str) {
        let line: String = row.iter().map(|c| c.c).collect();
        let line_lower = line.to_lowercase();
        let mut start = 0;
        while let Some(pos) = line_lower[start..].find(query) {
            let abs_pos = start + pos;
            self.matches.push(SearchMatch {
                row: abs_row,
                col_start: abs_pos,
                col_end: abs_pos + query.len(),
            });
            start = abs_pos + 1;
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
