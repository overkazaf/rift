use crate::config::Theme;
use crate::renderer::font::FontManager;

pub struct Welcome {
    pub visible: bool,
    page: usize,
    total_pages: usize,
}

impl Welcome {
    pub fn new_auto() -> Self {
        let marker = dirs::config_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("rift")
            .join(".welcome_shown");
        let is_first = !marker.exists();
        if is_first {
            if let Some(parent) = marker.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&marker, "shown");
        }
        Self {
            visible: is_first,
            page: 0,
            total_pages: 4,
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        self.page = 0;
    }

    pub fn handle_key(&mut self, key: WelcomeKey) {
        match key {
            WelcomeKey::Escape | WelcomeKey::Q => self.visible = false,
            WelcomeKey::Right | WelcomeKey::Enter | WelcomeKey::Space => {
                if self.page + 1 < self.total_pages {
                    self.page += 1;
                } else {
                    self.visible = false;
                }
            }
            WelcomeKey::Left => {
                if self.page > 0 {
                    self.page -= 1;
                }
            }
        }
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut FontManager,
        theme: &Theme,
    ) {
        use super::kit::{Ctx, PanelSpec, Rect, Tokens};
        if !self.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(0.7);

        let want = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + 11 * tk.row_h;
        let rect = cx.centered_cols(56, want);
        if rect.w < 100 || rect.h < 100 {
            return;
        }
        let mk = crate::config::mod_key();
        let page_title = ["Welcome", "Tabs & Panes", "Effects & Recording", "Tools & Config"][self.page.min(3)];
        let last = self.page + 1 == self.total_pages;
        let hints: &[(&str, &str)] = if last {
            &[("Enter", "start"), ("Esc", "skip")]
        } else {
            &[("Left/Right", "page"), ("Enter", "next"), ("Esc", "skip")]
        };
        let counter = format!("{} / {}", self.page + 1, self.total_pages);
        let spec = PanelSpec::new("Welcome to rift")
            .sub(page_title)
            .badge(&counter, super::kit::Tone::Neutral)
            .hints(hints);
        let body = cx.panel(rect, &spec);
        // Reserve the last row for the page dots.
        let content = Rect::new(body.x, body.y, body.w, body.h.saturating_sub(tk.row_h));

        match self.page {
            0 => Self::page_welcome(&mut cx, content),
            1 => {
                let items: Vec<(String, &str)> = vec![
                    (format!("{}+Shift+T", mk), "New tab"),
                    (format!("{}+Shift+W", mk), "Close pane/tab"),
                    (format!("{}+Shift+[", mk), "Prev tab"),
                    (format!("{}+Shift+]", mk), "Next tab"),
                    (format!("{}+D", mk), "Split vertical"),
                    (format!("{}+Shift+D", mk), "Split horizontal"),
                    ("Alt+Arrow".into(), "Switch pane"),
                ];
                Self::page_shortcuts(&mut cx, content, &items, &["Active pane has accent border."]);
            }
            2 => {
                let items: Vec<(String, &str)> = vec![
                    ("Ctrl+Shift+1".into(), "CRT"),
                    ("Ctrl+Shift+2".into(), "Glitch"),
                    ("Ctrl+Shift+3".into(), "Neon Glow"),
                    ("Ctrl+Shift+4".into(), "Matrix Rain"),
                    ("Ctrl+Shift+5".into(), "Amber"),
                    ("Ctrl+Shift+6".into(), "Hologram"),
                    ("Ctrl+Shift+0".into(), "Effects off"),
                    (format!("{}+Shift+R", mk), "Record toggle"),
                ];
                Self::page_shortcuts(&mut cx, content, &items, &["Recordings: .cast (asciinema v2)"]);
            }
            _ => Self::page_features(&mut cx, content),
        }

        // Page dots, centred on the last body row.
        let dot = 2 * tk.sp.xs;
        let gap = tk.sp.sm;
        let total_w = self.total_pages * dot + self.total_pages.saturating_sub(1) * gap;
        let mut dx = body.x + body.w.saturating_sub(total_w) / 2;
        let dy = body.bottom().saturating_sub(tk.row_h) + (tk.row_h - dot) / 2;
        for i in 0..self.total_pages {
            let c = if i == self.page { tk.accent } else { tk.border_strong };
            cx.fill_rrect(Rect::new(dx, dy, dot, dot), dot / 2, c);
            dx += dot + gap;
        }
    }

    fn page_welcome(cx: &mut super::kit::Ctx, r: super::kit::Rect) {
        let tk = cx.tk;
        let mut y = r.y;
        cx.line(r.x, y, "rift", tk.accent);
        y += tk.row_h;
        cx.line_fit(r.x, y, r.w, "Rust Terminal Emulator v0.3.0", tk.text_muted);
        y += tk.row_h * 2;
        cx.line(r.x, y, "Welcome!", tk.text);
        y += tk.row_h;
        for line in [
            "A programmable terminal with SSH,",
            "visual effects, WASM plugins,",
            "session recording, and more.",
        ] {
            cx.line_fit(r.x, y, r.w, line, tk.text_muted);
            y += tk.row_h;
        }
        y += tk.row_h / 2;
        let hint = format!("{}+Shift+? reopens this guide.", crate::config::mod_key());
        cx.line_fit(r.x, y, r.w, &hint, tk.text_faint);
    }

    /// Key-cap column + description column.
    fn page_shortcuts(cx: &mut super::kit::Ctx, r: super::kit::Rect, items: &[(String, &str)], tips: &[&str]) {
        let tk = cx.tk;
        let mut y = r.y;
        let key_w = items.iter().map(|(k, _)| cx.tw(k)).max().unwrap_or(0) + 2 * (tk.sp.xs + 2 * tk.scale);
        let desc_x = r.x + key_w + tk.sp.lg;
        for (key, desc) in items {
            if y + tk.row_h > r.bottom() {
                break;
            }
            cx.kbd_chip(r.x, y, tk.row_h, key);
            cx.line_fit(desc_x, y, r.right().saturating_sub(desc_x), desc, tk.text);
            y += tk.row_h;
        }
        y += tk.row_h / 2;
        for tip in tips {
            if y + tk.row_h > r.bottom() {
                break;
            }
            cx.line_fit(r.x, y, r.w, tip, tk.text_muted);
            y += tk.row_h;
        }
    }

    fn page_features(cx: &mut super::kit::Ctx, r: super::kit::Rect) {
        let tk = cx.tk;
        let mk = crate::config::mod_key();
        let items: Vec<(String, &str)> = vec![
            (format!("{}+Shift+,", mk), "Preferences"),
            (format!("{}+Shift+S", mk), "SSH connect"),
            (format!("{}+Shift+?", mk), "This guide"),
        ];
        let key_w = items.iter().map(|(k, _)| cx.tw(k)).max().unwrap_or(0) + 2 * (tk.sp.xs + 2 * tk.scale);
        let desc_x = r.x + key_w + tk.sp.lg;
        let mut y = r.y;
        for (key, desc) in &items {
            cx.kbd_chip(r.x, y, tk.row_h, key);
            cx.line_fit(desc_x, y, r.right().saturating_sub(desc_x), desc, tk.text);
            y += tk.row_h;
        }
        y += tk.row_h / 2;
        cx.section(r.x, y, r.w, "Built-in");
        y += tk.row_h;
        for line in [
            "10 themes, WASM plugins, SSH,",
            "Hex viewer, Base64 codec,",
            "Snippets, Fuzzy search,",
            "Time warp, HUD status",
        ] {
            if y + tk.row_h > r.bottom() {
                break;
            }
            cx.line_fit(r.x + tk.sp.xs, y, r.w, line, tk.text_muted);
            y += tk.row_h;
        }
        if y + tk.row_h <= r.bottom() {
            cx.line_fit(r.x, y + tk.row_h / 2, r.w, "~/.config/rift/config.toml", tk.text_faint);
        }
    }
}

pub enum WelcomeKey {
    Left,
    Right,
    Enter,
    Space,
    Escape,
    Q,
}


#[cfg(test)]
mod qa_tests {
    use super::*;
    use crate::ui::kit::gallery::qa::each_theme;

    #[test]
    fn renders_every_page() {
        for page in 0..4 {
            let w = Welcome { visible: true, page, total_pages: 4 };
            each_theme(&format!("welcome{page}"), |b, ww, hh, f, t| w.render(b, ww, hh, f, t));
        }
    }
}
