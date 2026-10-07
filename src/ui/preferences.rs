use crate::config::{Config, Theme};
use crate::renderer::font::FontManager;

pub struct Preferences {
    pub visible: bool,
    sections: Vec<Section>,
    active_section: usize,
    active_item: usize,
}

struct Section {
    name: &'static str,
    items: Vec<PrefItem>,
}

enum PrefItem {
    ThemeSelect {
        label: &'static str,
        current: String,
        options: Vec<&'static str>,
    },
    FontSize {
        label: &'static str,
        current: f32,
        min: f32,
        max: f32,
        step: f32,
    },
    Number {
        label: &'static str,
        current: u16,
        min: u16,
        max: u16,
    },
    Toggle {
        label: &'static str,
        current: bool,
    },
    Info {
        label: &'static str,
        value: String,
    },
}

impl Preferences {
    pub fn new(config: &Config) -> Self {
        let theme_names: Vec<&'static str> = Config::available_themes().to_vec();
        let mk = crate::config::mod_key();

        Self {
            visible: false,
            sections: vec![
                Section {
                    name: "Look",
                    items: vec![
                        PrefItem::ThemeSelect {
                            label: "Theme",
                            current: config.theme_name.clone(),
                            options: theme_names,
                        },
                        PrefItem::FontSize {
                            label: "Font Size",
                            current: config.font_size,
                            min: 8.0,
                            max: 32.0,
                            step: 1.0,
                        },
                        PrefItem::FontSize {
                            label: "Opacity",
                            current: config.opacity,
                            min: 0.3,
                            max: 1.0,
                            step: 0.05,
                        },
                    ],
                },
                Section {
                    name: "Term",
                    items: vec![
                        PrefItem::Number { label: "Columns", current: config.cols, min: 40, max: 320 },
                        PrefItem::Number { label: "Rows", current: config.rows, min: 10, max: 100 },
                        PrefItem::Toggle { label: "Cursor Blink", current: false },
                        PrefItem::Number { label: "Scrollback", current: 10000, min: 1000, max: 50000 },
                    ],
                },
                Section {
                    name: "FX",
                    items: vec![
                        PrefItem::Info { label: "CRT", value: format!("{}+Shift+1", mk) },
                        PrefItem::Info { label: "Glitch", value: format!("{}+Shift+2", mk) },
                        PrefItem::Info { label: "NeonGlow", value: format!("{}+Shift+3", mk) },
                        PrefItem::Info { label: "MatrixRain", value: format!("{}+Shift+4", mk) },
                        PrefItem::Info { label: "Off", value: format!("{}+Shift+0", mk) },
                    ],
                },
                Section {
                    name: "AI",
                    items: vec![
                        PrefItem::Info { label: "Provider", value: config.llm.provider.clone() },
                        PrefItem::Info { label: "Model", value: config.llm.model.clone() },
                        PrefItem::Info { label: "API URL", value: config.llm.api_url.clone() },
                        PrefItem::Info { label: "Status", value: if config.llm.enabled { "Active".into() } else { "Off".into() } },
                        PrefItem::Info { label: "Panel", value: format!("{}+Shift+A", mk) },
                    ],
                },
                Section {
                    name: "Keys",
                    items: {
                        let mk = mk;
                        vec![
                        PrefItem::Info { label: "New Tab", value: format!("{}+Shift+T", mk) },
                        PrefItem::Info { label: "Close", value: format!("{}+Shift+W", mk) },
                        PrefItem::Info { label: "Prev Tab", value: format!("{}+Shift+[", mk) },
                        PrefItem::Info { label: "Next Tab", value: format!("{}+Shift+]", mk) },
                        PrefItem::Info { label: "Split V", value: format!("{}+D", mk) },
                        PrefItem::Info { label: "Split H", value: format!("{}+Shift+D", mk) },
                        PrefItem::Info { label: "Focus Pane", value: "Alt+Arrow".into() },
                        PrefItem::Info { label: "SSH", value: format!("{}+Shift+S", mk) },
                        PrefItem::Info { label: "Search", value: format!("{}+F", mk) },
                        PrefItem::Info { label: "Broadcast", value: format!("{}+Shift+P", mk) },
                        PrefItem::Info { label: "Compare", value: format!("{}+Shift+K", mk) },
                        PrefItem::Info { label: "Record", value: format!("{}+Shift+R", mk) },
                        PrefItem::Info { label: "TimeWarp", value: format!("{}+Shift+Z", mk) },
                        PrefItem::Info { label: "HUD", value: format!("{}+Shift+H", mk) },
                        PrefItem::Info { label: "AI Panel", value: format!("{}+Shift+A", mk) },
                        PrefItem::Info { label: "Git Panel", value: format!("{}+Shift+G", mk) },
                        PrefItem::Info { label: "File Mgr", value: format!("{}+Shift+E", mk) },
                        PrefItem::Info { label: "CI/CD", value: format!("{}+Shift+I", mk) },
                        PrefItem::Info { label: "Docker", value: format!("{}+Shift+O", mk) },
                        PrefItem::Info { label: "Teaching", value: format!("{}+Shift+L", mk) },
                        PrefItem::Info { label: "Heatmap", value: format!("{}+Shift+Y", mk) },
                        PrefItem::Info { label: "SecretMask", value: format!("{}+Shift+M", mk) },
                        PrefItem::Info { label: "Audit Log", value: format!("{}+Shift+U", mk) },
                        PrefItem::Info { label: "Complete", value: "Ctrl+Space".into() },
                        PrefItem::Info { label: "Settings", value: format!("{}+Shift+,", mk) },
                        PrefItem::Info { label: "Welcome", value: format!("{}+Shift+?", mk) },
                        PrefItem::Info { label: "Zoom In", value: format!("{}+=", mk) },
                        PrefItem::Info { label: "Zoom Out", value: format!("{}+-", mk) },
                        PrefItem::Info { label: "Zoom Reset", value: format!("{}+0", mk) },
                        PrefItem::Info { label: "Copy", value: format!("{}+C", mk) },
                        PrefItem::Info { label: "Paste", value: format!("{}+V", mk) },
                        PrefItem::Info { label: "Select All", value: format!("{}+A", mk) },
                        PrefItem::Info { label: "Scroll Up", value: "Shift+PgUp".into() },
                        PrefItem::Info { label: "Scroll Dn", value: "Shift+PgDn".into() },
                    ]},
                },
                Section {
                    name: "About",
                    items: vec![
                        PrefItem::Info { label: "Version", value: "rift v0.3.0".into() },
                        PrefItem::Info { label: "Renderer", value: "softbuffer (CPU)".into() },
                        PrefItem::Info { label: "Font", value: "System Monospace".into() },
                        PrefItem::Info { label: "Language", value: "Rust".into() },
                        PrefItem::Info { label: "Config", value: "~/.config/rift/".into() },
                    ],
                },
            ],
            active_section: 0,
            active_item: 0,
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    pub fn handle_key(&mut self, key: PrefsKey) -> Option<PrefsAction> {
        match key {
            PrefsKey::Escape => {
                self.visible = false;
                None
            }
            PrefsKey::Up => {
                if self.active_item > 0 {
                    self.active_item -= 1;
                } else if self.active_section > 0 {
                    self.active_section -= 1;
                    self.active_item =
                        self.sections[self.active_section].items.len().saturating_sub(1);
                }
                None
            }
            PrefsKey::Down => {
                let section = &self.sections[self.active_section];
                if self.active_item + 1 < section.items.len() {
                    self.active_item += 1;
                } else if self.active_section + 1 < self.sections.len() {
                    self.active_section += 1;
                    self.active_item = 0;
                }
                None
            }
            PrefsKey::Left => self.adjust_current(-1),
            PrefsKey::Right => self.adjust_current(1),
            PrefsKey::Enter => self.adjust_current(1),
            PrefsKey::Tab => {
                self.active_section = (self.active_section + 1) % self.sections.len();
                self.active_item = 0;
                None
            }
            PrefsKey::Save => Some(PrefsAction::SaveConfig),
        }
    }

    fn adjust_current(&mut self, delta: i32) -> Option<PrefsAction> {
        let item = &mut self.sections[self.active_section].items[self.active_item];
        match item {
            PrefItem::ThemeSelect {
                current, options, ..
            } => {
                let idx = options
                    .iter()
                    .position(|&o| o == current.as_str())
                    .unwrap_or(0);
                let new_idx = (idx as i32 + delta).rem_euclid(options.len() as i32) as usize;
                *current = options[new_idx].to_string();
                Some(PrefsAction::ThemeChanged(current.clone()))
            }
            PrefItem::FontSize {
                label,
                current,
                min,
                max,
                step,
            } => {
                *current = (*current + *step * delta as f32).clamp(*min, *max);
                match *label {
                    "Opacity" => Some(PrefsAction::OpacityChanged(*current)),
                    _ => Some(PrefsAction::FontSizeChanged(*current)),
                }
            }
            PrefItem::Number {
                current, min, max, ..
            } => {
                *current = (*current as i32 + delta).clamp(*min as i32, *max as i32) as u16;
                None
            }
            PrefItem::Toggle { current, .. } => {
                *current = !*current;
                None
            }
            PrefItem::Info { .. } => None,
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

        // Dim the background
        for px in buffer.iter_mut() {
            let r = ((*px >> 16) & 0xff) / 3;
            let g = ((*px >> 8) & 0xff) / 3;
            let b = (*px & 0xff) / 3;
            *px = (r << 16) | (g << 8) | b;
        }

        let cw = font.cell_width;
        let ch = font.cell_height;
        let row_h = ch + 12;

        // Auto-size panel: at least wide enough for section tabs + padding
        let tabs_w: usize = self.sections.iter().map(|s| s.name.chars().count() * cw + 20).sum();
        let panel_w = (tabs_w + 60).max(45 * cw).max(520).min(width.saturating_sub(40));
        let section = &self.sections[self.active_section];
        let content_rows = section.items.len();
        let panel_h = (ch * 3 + 50 + content_rows * row_h + ch + 30)
            .max(200)
            .min(height.saturating_sub(40));
        let px0 = (width - panel_w) / 2;
        let py0 = (height - panel_h) / 2;

        // Panel background
        let panel_bg = lighten(theme.bg, 8);
        let panel_bg_px = pack(panel_bg.0, panel_bg.1, panel_bg.2);
        for y in py0..py0 + panel_h {
            let offset = y * width + px0;
            let end = (offset + panel_w).min(buffer.len());
            if offset < buffer.len() {
                buffer[offset..end].fill(panel_bg_px);
            }
        }

        // Panel border (accent / cursor color, slightly dimmed)
        let border_color = dim(theme.cursor, 0.5);
        let border_px = pack(border_color.0, border_color.1, border_color.2);
        for x in px0..px0 + panel_w {
            set_px(buffer, width, py0, x, border_px);
            set_px(buffer, width, py0 + panel_h - 1, x, border_px);
        }
        for y in py0..py0 + panel_h {
            set_px(buffer, width, y, px0, border_px);
            set_px(buffer, width, y, px0 + panel_w - 1, border_px);
        }

        // Title
        let title_y = py0 + 14;
        render_text(buffer, width, font, "Preferences", px0 + 20, title_y, theme.cursor);

        // Section tabs
        let tab_y = title_y + ch + 12;
        let mut tx = px0 + 20;
        for (i, section) in self.sections.iter().enumerate() {
            let is_active = i == self.active_section;
            let color = if is_active { theme.cursor } else { dim(theme.fg, 0.4) };

            render_text(buffer, width, font, section.name, tx, tab_y, color);

            let name_w = section.name.chars().count() * cw;
            if is_active {
                let underline_y = tab_y + ch + 2;
                let accent_px = pack(theme.cursor.0, theme.cursor.1, theme.cursor.2);
                for x in tx..tx + name_w {
                    set_px(buffer, width, underline_y, x, accent_px);
                    set_px(buffer, width, underline_y + 1, x, accent_px);
                }
            }

            tx += name_w + 20;
        }

        // Separator line
        let sep_y = tab_y + ch + 8;
        let sep_color = dim(theme.fg, 0.1);
        let sep_px = pack(sep_color.0, sep_color.1, sep_color.2);
        for x in (px0 + 16)..(px0 + panel_w - 16) {
            set_px(buffer, width, sep_y, x, sep_px);
        }

        // Items — two-column layout: label on left, value on right
        let label_col = px0 + 24;
        let value_col = px0 + panel_w / 2; // value starts at halfway
        let mut iy = sep_y + 14;

        for (i, item) in section.items.iter().enumerate() {
            if iy + row_h >= py0 + panel_h - 30 {
                break;
            }

            let is_selected = i == self.active_item;

            // Selected row highlight — stronger contrast
            if is_selected {
                let hl_color = lighten(panel_bg, 16);
                let hl_px = pack(hl_color.0, hl_color.1, hl_color.2);
                for y in iy..(iy + row_h) {
                    let offset = y * width + px0 + 10;
                    let end = (offset + panel_w - 20).min(buffer.len());
                    if offset < buffer.len() {
                        buffer[offset..end].fill(hl_px);
                    }
                }

                // Left accent bar for selected item
                let accent_px = pack(theme.cursor.0, theme.cursor.1, theme.cursor.2);
                for y in (iy + 2)..(iy + row_h - 2) {
                    set_px(buffer, width, y, px0 + 12, accent_px);
                    set_px(buffer, width, y, px0 + 13, accent_px);
                }
            }

            let label_color = if is_selected {
                theme.fg
            } else {
                dim(theme.fg, 0.6)
            };

            let text_y = iy + (row_h - ch) / 2;

            // Label on left, value on right (never overlap)
            match item {
                PrefItem::ThemeSelect { label, current, .. } => {
                    render_text(buffer, width, font, label, label_col, text_y, label_color);
                    let val = format!("< {} >", current);
                    let vc = if is_selected { theme.cursor } else { dim(theme.fg, 0.5) };
                    render_text(buffer, width, font, &val, value_col, text_y, vc);
                }
                PrefItem::FontSize { label, current, .. } => {
                    render_text(buffer, width, font, label, label_col, text_y, label_color);
                    let val = if *label == "Opacity" {
                        format!("< {:.0}% >", current * 100.0)
                    } else {
                        format!("< {:.0}px >", current)
                    };
                    let vc = if is_selected { theme.cursor } else { dim(theme.fg, 0.5) };
                    render_text(buffer, width, font, &val, value_col, text_y, vc);
                }
                PrefItem::Number { label, current, .. } => {
                    render_text(buffer, width, font, label, label_col, text_y, label_color);
                    let val = format!("< {} >", current);
                    let vc = if is_selected { theme.cursor } else { dim(theme.fg, 0.5) };
                    render_text(buffer, width, font, &val, value_col, text_y, vc);
                }
                PrefItem::Toggle { label, current, .. } => {
                    render_text(buffer, width, font, label, label_col, text_y, label_color);
                    let (val, vc) = if *current {
                        ("ON", theme.cursor)
                    } else {
                        ("OFF", dim(theme.fg, 0.3))
                    };
                    render_text(buffer, width, font, val, value_col, text_y, vc);
                }
                PrefItem::Info { label, value } => {
                    render_text(buffer, width, font, label, label_col, text_y, label_color);
                    render_text(buffer, width, font, value, value_col, text_y, dim(theme.fg, 0.4));
                }
            }

            iy += row_h;
        }

        // Bottom help text — clamp to panel width
        let help = "Up/Down  Left/Right  Tab  S:save  Esc";
        let help_max_chars = (panel_w - 40) / cw;
        let help_str = truncate(help, help_max_chars);
        let help_y = py0 + panel_h - ch - 14;
        render_text(buffer, width, font, help_str, px0 + 20, help_y, dim(theme.fg, 0.3));
    }
}

pub enum PrefsKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Escape,
    Tab,
    Save,
}

pub enum PrefsAction {
    ThemeChanged(String),
    FontSizeChanged(f32),
    OpacityChanged(f32),
    SaveConfig,
}

use super::primitives::{render_text, pack, dim, lighten, set_px, trunc as truncate};
