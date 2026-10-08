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
        _height: usize,
        font: &mut FontManager,
        theme: &Theme,
    ) {
        if !self.visible {
            return;
        }

        let cw = font.cell_width;
        let ch = font.cell_height;

        let bar_w = (32 * cw).min(width / 2);
        let bar_h = ch + 14;
        let bar_x = width.saturating_sub(bar_w + 12);
        let bar_y = 8;

        // Background
        let bg = crate::ui::darken(theme.bg, 8);
        crate::ui::fill_rect(buffer, width, bar_x, bar_y, bar_w, bar_h, crate::ui::pack_rgb(bg));
        crate::ui::draw_border(
            buffer,
            width,
            bar_x,
            bar_y,
            bar_w,
            bar_h,
            crate::ui::pack_rgb(crate::ui::dim(theme.cursor, 0.5)),
        );

        let text_y = bar_y + 7;

        // Search icon
        crate::ui::render_text(buffer, width, font, "/", bar_x + 8, text_y, theme.cursor);

        // Query text
        let max_input = (bar_w / cw).saturating_sub(10);
        let input_display = crate::ui::trunc(&self.query, max_input);
        crate::ui::render_text(
            buffer,
            width,
            font,
            input_display,
            bar_x + 8 + 2 * cw,
            text_y,
            theme.fg,
        );

        // Cursor
        let cursor_x = bar_x + 8 + 2 * cw + input_display.chars().count() * cw;
        let cpx = crate::ui::pack_rgb(theme.cursor);
        for y in text_y..text_y + ch {
            crate::ui::set_px(buffer, width, y, cursor_x, cpx);
            crate::ui::set_px(buffer, width, y, cursor_x + 1, cpx);
        }

        // Match count
        let count_str = format!(
            "{}/{}",
            if self.matches.is_empty() {
                0
            } else {
                self.current_match + 1
            },
            self.matches.len()
        );
        let count_w = count_str.len() * cw;
        let count_x = bar_x + bar_w - count_w - 8;
        crate::ui::render_text(
            buffer,
            width,
            font,
            &count_str,
            count_x,
            text_y,
            crate::ui::dim(theme.fg, 0.5),
        );
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
