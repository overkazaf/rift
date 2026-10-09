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
                        PrefItem::Info { label: "Complete", value: if cfg!(target_os = "macos") { "Cmd+.".into() } else { "Ctrl+Shift+Space".into() } },
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
        use super::kit::{center_scroll, Ctx, PanelSpec, Rect, Tokens, Tone};
        if !self.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(tk.backdrop);

        // Stable panel size across sections: fits the busiest one, capped at 10 rows.
        let max_rows = self.sections.iter().map(|s| s.items.len()).max().unwrap_or(1).min(10);
        let want = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + (max_rows + 1) * tk.row_h + tk.sp.sm;
        let rect = cx.centered_cols(76, want);
        let section = &self.sections[self.active_section];
        let spec = PanelSpec::new("Preferences")
            .sub(section.name)
            .hints(&[("Up/Down", "select"), ("Left/Right", "change"), ("Tab", "section"), ("S", "save"), ("Esc", "close")]);
        let body = cx.panel(rect, &spec);

        let names: Vec<&str> = self.sections.iter().map(|s| s.name).collect();
        cx.tabs(body.x, body.y, body.w, &names, self.active_section);
        cx.divider(body.x, body.y + tk.row_h, body.w);

        let list = Rect::new(body.x, body.y + tk.row_h + tk.sp.sm, body.w, body.h.saturating_sub(tk.row_h + tk.sp.sm));
        let vis = cx.rows_fit(list.h);
        let scroll = center_scroll(self.active_item, section.items.len(), vis);
        let sb = if section.items.len() > vis { tk.sp.sm } else { 0 };
        let rw = list.w.saturating_sub(sb);
        let value_x = list.x + rw * 45 / 100;

        for (n, item) in section.items.iter().skip(scroll).take(vis).enumerate() {
            let i = scroll + n;
            let row = Rect::new(list.x, list.y + n * tk.row_h, rw, tk.row_h);
            let selected = i == self.active_item;
            cx.row_bg(row, selected, false);

            let label_x = row.x + tk.sp.md + 2 * tk.scale;
            let value_w = row.right().saturating_sub(value_x + tk.sp.md);
            let label = match item {
                PrefItem::ThemeSelect { label, .. }
                | PrefItem::FontSize { label, .. }
                | PrefItem::Number { label, .. }
                | PrefItem::Toggle { label, .. }
                | PrefItem::Info { label, .. } => *label,
            };
            let label_color = if selected { tk.text } else { tk.text_muted };
            cx.line_fit(label_x, row.y, value_x.saturating_sub(label_x + tk.sp.md), label, label_color);

            // Adjustable values show "< v >" and light up when selected.
            let adj_color = if selected { tk.accent } else { tk.text };
            match item {
                PrefItem::ThemeSelect { current, .. } => {
                    cx.line_fit(value_x, row.y, value_w, &format!("< {} >", current), adj_color);
                }
                PrefItem::FontSize { label, current, .. } => {
                    let val = if *label == "Opacity" {
                        format!("< {:.0}% >", current * 100.0)
                    } else {
                        format!("< {:.0}px >", current)
                    };
                    cx.line_fit(value_x, row.y, value_w, &val, adj_color);
                }
                PrefItem::Number { current, .. } => {
                    cx.line_fit(value_x, row.y, value_w, &format!("< {} >", current), adj_color);
                }
                PrefItem::Toggle { current, .. } => {
                    let (t, tone) = if *current { ("ON", Tone::Success) } else { ("OFF", Tone::Neutral) };
                    cx.badge_line(value_x, row.y, t, tone);
                }
                PrefItem::Info { value, .. } => {
                    cx.line_fit(value_x, row.y, value_w, value, tk.text_muted);
                }
            }
        }
        cx.scrollbar(list, section.items.len(), vis, scroll);
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

