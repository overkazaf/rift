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
        use crate::ui::kit::{Ctx, PanelSpec, Rect, Tokens};
        if !self.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(tk.backdrop);

        let want = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + tk.input_h;
        let rect = cx.centered_cols(56, want);
        let spec = PanelSpec::new("Open URL")
            .sub("in the built-in browser")
            .hints(&[("Enter", "open"), ("Esc", "cancel")]);
        let body = cx.panel(rect, &spec);
        let input = Rect::new(body.x, body.y, body.w, tk.input_h);
        cx.text_input(input, &self.url, self.url.chars().count(), None, "https://example.com", true);
    }
}
