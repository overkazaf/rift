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
        if !self.visible {
            return;
        }

        let cw = font.cell_width;
        let ch = font.cell_height;

        // Dark overlay
        for px in buffer.iter_mut() {
            let r = ((*px >> 16) & 0xff) / 4;
            let g = ((*px >> 8) & 0xff) / 4;
            let b = (*px & 0xff) / 4;
            *px = (r << 16) | (g << 8) | b;
        }

        // Auto-size panel from font metrics
        let panel_w = (48 * cw).max(480).min(width.saturating_sub(60));
        let panel_h = (20 * ch).max(360).min(height.saturating_sub(60));
        if panel_w < 100 || panel_h < 100 {
            return;
        }
        let px0 = (width - panel_w) / 2;
        let py0 = (height - panel_h) / 2;
        let max_chars = (panel_w - 48) / cw; // max text chars per line

        // Panel background
        let bg = lighten(theme.bg, 6);
        let bg_px = pack(bg.0, bg.1, bg.2);
        for y in py0..py0 + panel_h {
            let offset = y * width + px0;
            let end = (offset + panel_w).min(buffer.len());
            if offset < buffer.len() {
                buffer[offset..end].fill(bg_px);
            }
        }

        // Border
        let border_color = dim(theme.cursor, 0.4);
        let border_px = pack(border_color.0, border_color.1, border_color.2);
        for x in px0..px0 + panel_w {
            set_px(buffer, width, py0, x, border_px);
            set_px(buffer, width, py0 + panel_h - 1, x, border_px);
        }
        for y in py0..py0 + panel_h {
            set_px(buffer, width, y, px0, border_px);
            set_px(buffer, width, y, px0 + panel_w - 1, border_px);
        }

        let cx = px0 + 24;
        let cy_start = py0 + 20;
        let key_col = cx;
        let val_col = cx + 16.min(max_chars / 2) * cw;

        match self.page {
            0 => self.page_welcome(buffer, width, font, theme, cx, cy_start, max_chars),
            1 => {
                let mk = crate::config::mod_key();
                let items: Vec<(String, &str)> = vec![
                    (format!("{}+Shift+T", mk), "New tab"),
                    (format!("{}+Shift+W", mk), "Close pane/tab"),
                    (format!("{}+Shift+[", mk), "Prev tab"),
                    (format!("{}+Shift+]", mk), "Next tab"),
                    (format!("{}+D", mk), "Split vertical"),
                    (format!("{}+Shift+D", mk), "Split horizontal"),
                    ("Alt+Arrow".into(), "Switch pane"),
                ];
                let pairs: Vec<(&str, &str)> = items.iter().map(|(k, v)| (k.as_str(), *v)).collect();
                self.page_shortcuts(
                    buffer, width, font, theme, key_col, val_col, cy_start, max_chars,
                    "Tabs & Panes", &pairs, &["Active pane has accent border."],
                );
            }
            2 => {
                let mk = crate::config::mod_key();
                let items: Vec<(String, &str)> = vec![
                    (format!("{}+Shift+1", mk), "CRT effect"),
                    (format!("{}+Shift+2", mk), "Glitch"),
                    (format!("{}+Shift+3", mk), "NeonGlow"),
                    (format!("{}+Shift+4", mk), "MatrixRain"),
                    (format!("{}+Shift+0", mk), "Effects off"),
                    (format!("{}+Shift+R", mk), "Record toggle"),
                ];
                let pairs: Vec<(&str, &str)> = items.iter().map(|(k, v)| (k.as_str(), *v)).collect();
                self.page_shortcuts(
                    buffer, width, font, theme, key_col, val_col, cy_start, max_chars,
                    "Effects & Recording", &pairs, &["Recordings: .cast (asciinema v2)"],
                );
            }
            3 => self.page_features(buffer, width, font, theme, cx, cy_start, max_chars),
            _ => {}
        }

        // Page indicator: "1 / 4"
        let indicator = format!("{} / {}", self.page + 1, self.total_pages);
        let ind_x = px0 + (panel_w.saturating_sub(indicator.len() * cw)) / 2;
        let ind_y = py0 + panel_h - ch - 12;
        render_text(buffer, width, font, &indicator, ind_x, ind_y, dim(theme.fg, 0.4));

        // Nav hint
        let nav = if self.page == self.total_pages - 1 {
            "Enter: start  Esc: skip"
        } else {
            "Arrow: page  Enter: next  Esc: skip"
        };
        let nav_trunc = trunc(nav, max_chars);
        let nav_x = px0 + (panel_w.saturating_sub(nav_trunc.len() * cw)) / 2;
        let nav_y = ind_y - ch - 6;
        render_text(buffer, width, font, nav_trunc, nav_x, nav_y, dim(theme.fg, 0.25));
    }

    fn page_welcome(
        &self, buffer: &mut [u32], buf_w: usize,
        font: &mut FontManager, theme: &Theme,
        x: usize, y: usize, max_c: usize,
    ) {
        let ch = font.cell_height;
        let mut cy = y;

        // Simple text logo — no ASCII art that breaks at large fonts
        render_text(buffer, buf_w, font, "rift", x, cy, theme.cursor);
        cy += ch + 4;

        let sub = "Rust Terminal Emulator v0.3.0";
        render_text(buffer, buf_w, font, trunc(sub, max_c), x, cy, dim(theme.fg, 0.5));
        cy += ch * 2;

        render_text(buffer, buf_w, font, "Welcome!", x, cy, theme.fg);
        cy += ch + 6;

        let lines = [
            "A programmable terminal with SSH,",
            "visual effects, WASM plugins,",
            "session recording, and more.",
            "",
            "Cmd/Ctrl+Shift+? reopens this guide.",
        ];
        for line in &lines {
            render_text(buffer, buf_w, font, trunc(line, max_c), x, cy, dim(theme.fg, 0.7));
            cy += ch + 2;
        }
    }

    fn page_shortcuts(
        &self, buffer: &mut [u32], buf_w: usize,
        font: &mut FontManager, theme: &Theme,
        key_x: usize, val_x: usize, y: usize, max_c: usize,
        title: &str,
        shortcuts: &[(&str, &str)],
        tips: &[&str],
    ) {
        let ch = font.cell_height;
        let mut cy = y;

        render_text(buffer, buf_w, font, trunc(title, max_c), key_x, cy, theme.cursor);
        cy += ch + 10;

        let key_max = (val_x.saturating_sub(key_x)) / font.cell_width;
        let val_max = max_c.saturating_sub(key_max + 2);

        for (key, desc) in shortcuts {
            render_text(buffer, buf_w, font, trunc(key, key_max), key_x, cy, theme.cursor);
            render_text(buffer, buf_w, font, trunc(desc, val_max), val_x, cy, dim(theme.fg, 0.7));
            cy += ch + 4;
        }

        if !tips.is_empty() {
            cy += ch / 2;
            for tip in tips {
                render_text(buffer, buf_w, font, trunc(tip, max_c), key_x, cy, dim(theme.fg, 0.45));
                cy += ch + 2;
            }
        }
    }

    fn page_features(
        &self, buffer: &mut [u32], buf_w: usize,
        font: &mut FontManager, theme: &Theme,
        x: usize, y: usize, max_c: usize,
    ) {
        let ch = font.cell_height;
        let cw = font.cell_width;
        let mut cy = y;

        render_text(buffer, buf_w, font, trunc("Tools & Config", max_c), x, cy, theme.cursor);
        cy += ch + 10;

        let mk = crate::config::mod_key();
        let shortcuts = [
            (format!("{}+Shift+,", mk), "Preferences"),
            (format!("{}+Shift+S", mk), "SSH connect"),
            (format!("{}+Shift+?", mk), "This guide"),
        ];
        let val_x = x + 16.min(max_c / 2) * cw;
        let key_max = 16.min(max_c / 2);
        let val_max = max_c.saturating_sub(key_max + 2);
        for (key, desc) in &shortcuts {
            render_text(buffer, buf_w, font, trunc(&key, key_max), x, cy, theme.cursor);
            render_text(buffer, buf_w, font, trunc(desc, val_max), val_x, cy, dim(theme.fg, 0.7));
            cy += ch + 4;
        }

        cy += ch;
        render_text(buffer, buf_w, font, "Built-in:", x, cy, dim(theme.fg, 0.8));
        cy += ch + 4;

        let features = [
            "9 themes, WASM plugins, SSH,",
            "Hex viewer, Base64 codec,",
            "Snippets, Fuzzy search,",
            "Time warp, HUD status",
        ];
        for line in &features {
            render_text(buffer, buf_w, font, trunc(line, max_c), x + cw, cy, dim(theme.fg, 0.55));
            cy += ch + 3;
        }

        cy += ch;
        let cfg = "~/.config/rift/config.toml";
        render_text(buffer, buf_w, font, trunc(cfg, max_c), x, cy, dim(theme.fg, 0.35));
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

use super::primitives::{render_text, pack, lighten, dim, set_px, trunc};
