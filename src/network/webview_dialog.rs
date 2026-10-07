use crate::renderer::font::FontManager;

pub struct WebViewDialog {
    pub visible: bool,
    pub url: String,
}

pub enum WvDialogKey {
    Char(char),
    Backspace,
    Enter,
    Escape,
}

impl WebViewDialog {
    pub fn new() -> Self {
        Self {
            visible: false,
            url: "https://".to_string(),
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.url = "https://".to_string();
        }
    }

    pub fn handle_key(&mut self, key: WvDialogKey) -> Option<String> {
        match key {
            WvDialogKey::Char(c) => {
                self.url.push(c);
                None
            }
            WvDialogKey::Backspace => {
                self.url.pop();
                None
            }
            WvDialogKey::Escape => {
                self.visible = false;
                None
            }
            WvDialogKey::Enter => {
                let url = self.url.trim().to_string();
                if url.is_empty() || url == "https://" {
                    return None;
                }
                self.visible = false;
                Some(url)
            }
        }
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut FontManager,
        theme: &crate::config::Theme,
    ) {
        if !self.visible {
            return;
        }

        // Dim background
        for px in buffer.iter_mut() {
            let r = ((*px >> 16) & 0xff) / 3;
            let g = ((*px >> 8) & 0xff) / 3;
            let b = (*px & 0xff) / 3;
            *px = (r << 16) | (g << 8) | b;
        }

        let cw = font.cell_width;
        let ch = font.cell_height;

        let panel_w = (40 * cw).max(400).min(width.saturating_sub(40));
        let panel_h = ch * 5 + 40;
        let px0 = (width - panel_w) / 2;
        let py0 = (height - panel_h) / 2;

        // Panel background
        let bg = lighten(theme.bg, 8);
        let bg_px = pack(bg.0, bg.1, bg.2);
        for y in py0..py0 + panel_h {
            let off = y * width + px0;
            let end = (off + panel_w).min(buffer.len());
            if off < buffer.len() {
                buffer[off..end].fill(bg_px);
            }
        }

        // Border
        let border = dim(theme.cursor, 0.5);
        let bp = pack(border.0, border.1, border.2);
        for x in px0..px0 + panel_w {
            set_px(buffer, width, py0, x, bp);
            set_px(buffer, width, py0 + panel_h - 1, x, bp);
        }
        for y in py0..py0 + panel_h {
            set_px(buffer, width, y, px0, bp);
            set_px(buffer, width, y, px0 + panel_w - 1, bp);
        }

        // Title
        render_text(
            buffer, width, font, "Open URL", px0 + 16, py0 + 10, theme.cursor,
        );

        // URL input box
        let input_y = py0 + 10 + ch + 12;
        let input_x = px0 + 16;
        let input_w = panel_w - 32;

        // Input background
        let input_bg = lighten(bg, 10);
        let ibp = pack(input_bg.0, input_bg.1, input_bg.2);
        for y in input_y.saturating_sub(4)..input_y + ch + 6 {
            let off = y * width + input_x;
            let end = (off + input_w).min(buffer.len());
            if off < buffer.len() {
                buffer[off..end].fill(ibp);
            }
        }

        // Input underline
        let ul_y = input_y + ch + 4;
        let ul_px = pack(theme.cursor.0, theme.cursor.1, theme.cursor.2);
        for x in input_x..input_x + input_w {
            set_px(buffer, width, ul_y, x, ul_px);
        }

        // URL text
        let max_chars = input_w / cw;
        let display = if self.url.len() > max_chars {
            &self.url[self.url.len() - max_chars..]
        } else {
            &self.url
        };
        render_text(buffer, width, font, display, input_x + 4, input_y, theme.fg);

        // Cursor
        let cursor_x = input_x + 4 + display.len() * cw;
        let cp = pack(theme.cursor.0, theme.cursor.1, theme.cursor.2);
        for y in input_y..input_y + ch {
            set_px(buffer, width, y, cursor_x, cp);
            if cursor_x + 1 < width {
                set_px(buffer, width, y, cursor_x + 1, cp);
            }
        }

        // Help text
        let help = "Enter: open  Esc: cancel";
        let help_y = py0 + panel_h - ch - 10;
        render_text(
            buffer, width, font, help, px0 + 16, help_y, dim(theme.fg, 0.3),
        );
    }
}

use crate::ui::{render_text, set_px, pack, dim, lighten};
