use crate::config::Theme;
use crate::renderer::font::FontManager;

/// Command Palette — Cmd+P. A searchable popup giving quick keyboard access
/// to every major feature, mirroring VS Code's "Cmd+P" launcher.
pub struct CommandPalette {
    pub visible: bool,
    pub query: String,
    pub items: Vec<PaletteItem>,
    pub filtered: Vec<usize>,
    pub selected: usize,
}

pub struct PaletteItem {
    pub name: String,
    pub shortcut: Option<String>,
    pub action: PaletteAction,
}

impl PaletteItem {
    fn new(name: &str, shortcut: Option<String>, action: PaletteAction) -> Self {
        Self { name: name.to_string(), shortcut, action }
    }
}

/// Visual shader effect choices exposed in the palette (mirrors `Ctrl+Shift+1..8/0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteEffect {
    Crt,
    Glitch,
    Neon,
    Matrix,
    Amber,
    Hologram,
    Pixelate,
    Thermal,
    Off,
}

/// Everything the palette can trigger. Dispatch lives in `app::overlays`,
/// which maps most of these onto the existing `ui::MenuAction` handlers.
#[derive(Clone, Debug)]
pub enum PaletteAction {
    NewTab,
    CloseTab,
    SplitH,
    SplitV,
    Search,
    Ssh,
    Ai,
    Hud,
    TimeWarp,
    Recording,
    FileManager,
    GitPanel,
    Docker,
    Cicd,
    Heatmap,
    SecretMask,
    AuditLog,
    Teaching,
    Observer,
    Welcome,
    Prefs,
    NetworkMonitor,
    ProcessTree,
    SystemInfo,
    WebView,
    Effect(PaletteEffect),
    Theme(String),
}

pub enum PaletteKey {
    Char(char),
    Backspace,
    Enter,
    Escape,
    Up,
    Down,
}

const MAX_VISIBLE: usize = 12;

impl CommandPalette {
    pub fn new() -> Self {
        let items = default_items();
        let filtered: Vec<usize> = (0..items.len()).collect();
        Self {
            visible: false,
            query: String::new(),
            items,
            filtered,
            selected: 0,
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.query.clear();
            self.refilter();
        }
    }

    /// Case-insensitive substring match on item name.
    fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        self.filtered = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| q.is_empty() || item.name.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect();
        self.selected = 0;
    }

    pub fn handle_key(&mut self, key: PaletteKey) -> Option<PaletteAction> {
        match key {
            PaletteKey::Char(c) => {
                self.query.push(c);
                self.refilter();
                None
            }
            PaletteKey::Backspace => {
                self.query.pop();
                self.refilter();
                None
            }
            PaletteKey::Up => {
                if !self.filtered.is_empty() {
                    self.selected = if self.selected == 0 {
                        self.filtered.len() - 1
                    } else {
                        self.selected - 1
                    };
                }
                None
            }
            PaletteKey::Down => {
                if !self.filtered.is_empty() {
                    self.selected = (self.selected + 1) % self.filtered.len();
                }
                None
            }
            PaletteKey::Enter => {
                let action = self
                    .filtered
                    .get(self.selected)
                    .and_then(|&idx| self.items.get(idx))
                    .map(|item| item.action.clone());
                if action.is_some() {
                    self.visible = false;
                }
                action
            }
            PaletteKey::Escape => {
                self.visible = false;
                None
            }
        }
    }

    // ── Rendering ──

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

        // Dim backdrop
        for px in buffer.iter_mut() {
            let r = ((*px >> 16) & 0xff) / 3;
            let g = ((*px >> 8) & 0xff) / 3;
            let b = (*px & 0xff) / 3;
            *px = (r << 16) | (g << 8) | b;
        }

        let cw = font.cell_width.max(1);
        let ch = font.cell_height;

        let total = self.filtered.len();
        let visible_count = total.min(MAX_VISIBLE).max(1);

        // ~60% width, never wider than the window allows.
        let ideal_w = (width * 6) / 10;
        let panel_w = ideal_w.max(30 * cw).min(width.saturating_sub(16).max(1));

        let input_h = ch + 20;
        let row_h = ch + 10;
        let footer_h = ch + 14;
        let panel_h = (input_h + 4 + visible_count * row_h + footer_h)
            .min(height.saturating_sub(16).max(1));

        let px0 = width.saturating_sub(panel_w) / 2;
        let py0 = height.saturating_sub(panel_h) / 3;
        let radius = 10usize.min(panel_w / 4).min(panel_h / 4);

        let bg = crate::ui::lighten(theme.bg, 8);
        let bg_px = crate::ui::pack_rgb(bg);
        let border_px = crate::ui::pack_rgb(crate::ui::dim(theme.cursor, 0.5));

        fill_rounded_rect(buffer, width, px0, py0, panel_w, panel_h, radius, bg_px);
        draw_rounded_border(buffer, width, px0, py0, panel_w, panel_h, radius, border_px);

        // ── Input row ──
        let input_y = py0 + (input_h.saturating_sub(ch)) / 2;
        crate::ui::render_text(buffer, width, font, ">", px0 + 16, input_y, theme.cursor);

        let text_x = px0 + 16 + 2 * cw;
        let max_query_chars = (panel_w.saturating_sub(64)) / cw;
        let query_display = crate::ui::trunc(&self.query, max_query_chars);
        crate::ui::render_text(buffer, width, font, query_display, text_x, input_y, theme.fg);

        // Steady text cursor after the query
        let cursor_x = text_x + query_display.chars().count() * cw;
        let cursor_px = crate::ui::pack_rgb(theme.cursor);
        for y in input_y..(input_y + ch).min(height) {
            crate::ui::set_px(buffer, width, y, cursor_x, cursor_px);
            crate::ui::set_px(buffer, width, y, cursor_x + 1, cursor_px);
        }

        // Divider under the input row
        let sep_y = py0 + input_h;
        if sep_y < height {
            let sep_px = crate::ui::pack_rgb(crate::ui::dim(theme.fg, 0.12));
            let off = sep_y * width + px0 + 8;
            let end = (off + panel_w.saturating_sub(16)).min(buffer.len());
            if off < buffer.len() {
                buffer[off..end].fill(sep_px);
            }
        }

        // ── Results list ──
        let list_top = py0 + input_h + 4;

        if total == 0 {
            let ty = list_top + (row_h.saturating_sub(ch)) / 2;
            crate::ui::render_text(
                buffer, width, font, "No matching commands",
                px0 + 18, ty, crate::ui::dim(theme.fg, 0.4),
            );
        } else {
            let start = if total <= MAX_VISIBLE {
                0
            } else {
                self.selected.saturating_sub(MAX_VISIBLE / 2).min(total - MAX_VISIBLE)
            };
            let end = (start + MAX_VISIBLE).min(total);

            for (row, i) in (start..end).enumerate() {
                let idx = self.filtered[i];
                let Some(item) = self.items.get(idx) else { continue };
                let is_sel = i == self.selected;
                let ry = list_top + row * row_h;

                if is_sel {
                    let hl = crate::ui::lighten(bg, 14);
                    crate::ui::fill_rect(
                        buffer, width, px0 + 4, ry, panel_w.saturating_sub(8), row_h,
                        crate::ui::pack_rgb(hl),
                    );
                    let accent_px = crate::ui::pack_rgb(theme.cursor);
                    for y in (ry + 2)..(ry + row_h.saturating_sub(2)).min(height) {
                        crate::ui::set_px(buffer, width, y, px0 + 6, accent_px);
                        crate::ui::set_px(buffer, width, y, px0 + 7, accent_px);
                    }
                }

                let ty = ry + (row_h.saturating_sub(ch)) / 2;
                let name_color = if is_sel { theme.fg } else { crate::ui::dim(theme.fg, 0.65) };
                let max_name = (panel_w.saturating_sub(48)) / cw;
                crate::ui::render_text(
                    buffer, width, font, crate::ui::trunc(&item.name, max_name),
                    px0 + 22, ty, name_color,
                );

                if let Some(ref sc) = item.shortcut {
                    let sc_color = if is_sel { crate::ui::dim(theme.fg, 0.55) } else { crate::ui::dim(theme.fg, 0.3) };
                    let sc_w = sc.chars().count() * cw;
                    let sx = (px0 + panel_w).saturating_sub(sc_w + 16);
                    crate::ui::render_text(buffer, width, font, sc, sx, ty, sc_color);
                }
            }
        }

        // ── Footer ──
        let footer_y = py0 + panel_h.saturating_sub(footer_h) + (footer_h.saturating_sub(ch)) / 2;
        crate::ui::render_text(
            buffer, width, font,
            "\u{2191}\u{2193} Navigate   Enter Select   Esc Close",
            px0 + 16, footer_y, crate::ui::dim(theme.fg, 0.3),
        );
        let count_str = format!("{}/{}", if total == 0 { 0 } else { self.selected + 1 }, total);
        let count_w = count_str.chars().count() * cw;
        crate::ui::render_text(
            buffer, width, font, &count_str,
            (px0 + panel_w).saturating_sub(count_w + 16), footer_y,
            crate::ui::dim(theme.fg, 0.3),
        );
    }
}

// ── Default feature catalog ──

fn default_items() -> Vec<PaletteItem> {
    let m = crate::config::mod_key();

    let mut items = vec![
        PaletteItem::new("New Tab", Some(format!("{m}+Shift+T")), PaletteAction::NewTab),
        PaletteItem::new("Close Tab", Some(format!("{m}+Shift+W")), PaletteAction::CloseTab),
        PaletteItem::new("Split Right", Some(format!("{m}+D")), PaletteAction::SplitH),
        PaletteItem::new("Split Down", Some(format!("{m}+Shift+D")), PaletteAction::SplitV),
        PaletteItem::new("Find in Terminal", Some(format!("{m}+F")), PaletteAction::Search),
        PaletteItem::new("SSH Connect...", Some(format!("{m}+Shift+S")), PaletteAction::Ssh),
        PaletteItem::new("AI Assistant", Some(format!("{m}+Shift+A")), PaletteAction::Ai),
        PaletteItem::new("Toggle HUD", Some(format!("{m}+Shift+H")), PaletteAction::Hud),
        PaletteItem::new("Time Warp", Some(format!("{m}+Shift+Z")), PaletteAction::TimeWarp),
        PaletteItem::new("Toggle Recording", Some(format!("{m}+Shift+R")), PaletteAction::Recording),
        PaletteItem::new("File Manager", Some(format!("{m}+Shift+E")), PaletteAction::FileManager),
        PaletteItem::new("Git Panel", Some(format!("{m}+Shift+G")), PaletteAction::GitPanel),
        PaletteItem::new("Docker Panel", Some(format!("{m}+Shift+O")), PaletteAction::Docker),
        PaletteItem::new("CI/CD Panel", Some(format!("{m}+Shift+I")), PaletteAction::Cicd),
        PaletteItem::new("Command Heatmap", Some(format!("{m}+Shift+Y")), PaletteAction::Heatmap),
        PaletteItem::new("Secret Masking", Some(format!("{m}+Shift+M")), PaletteAction::SecretMask),
        PaletteItem::new("Audit Log", Some(format!("{m}+Shift+U")), PaletteAction::AuditLog),
        PaletteItem::new("Teaching Mode", Some(format!("{m}+Shift+L")), PaletteAction::Teaching),
        PaletteItem::new("Observer Mode", Some("Ctrl+Shift+V".to_string()), PaletteAction::Observer),
        PaletteItem::new("Welcome Guide", Some(format!("{m}+Shift+/")), PaletteAction::Welcome),
        PaletteItem::new("Preferences", Some(format!("{m}+,")), PaletteAction::Prefs),
        PaletteItem::new("Network Monitor", None, PaletteAction::NetworkMonitor),
        PaletteItem::new("Process Tree", None, PaletteAction::ProcessTree),
        PaletteItem::new("System Info", None, PaletteAction::SystemInfo),
        PaletteItem::new("Toggle WebView", Some(format!("{m}+Shift+B")), PaletteAction::WebView),
    ];

    let effects: [(&str, PaletteEffect, &str); 9] = [
        ("Effect: CRT", PaletteEffect::Crt, "Ctrl+Shift+1"),
        ("Effect: Glitch", PaletteEffect::Glitch, "Ctrl+Shift+2"),
        ("Effect: Neon Glow", PaletteEffect::Neon, "Ctrl+Shift+3"),
        ("Effect: Matrix Rain", PaletteEffect::Matrix, "Ctrl+Shift+4"),
        ("Effect: Amber", PaletteEffect::Amber, "Ctrl+Shift+5"),
        ("Effect: Hologram", PaletteEffect::Hologram, "Ctrl+Shift+6"),
        ("Effect: Pixelate", PaletteEffect::Pixelate, "Ctrl+Shift+7"),
        ("Effect: Thermal", PaletteEffect::Thermal, "Ctrl+Shift+8"),
        ("Effect: Off", PaletteEffect::Off, "Ctrl+Shift+0"),
    ];
    for (name, eff, sc) in effects {
        items.push(PaletteItem::new(name, Some(sc.to_string()), PaletteAction::Effect(eff)));
    }

    for &name in crate::config::Config::available_themes() {
        let label = format!("Theme: {}", titlecase(name));
        items.push(PaletteItem::new(&label, None, PaletteAction::Theme(name.to_string())));
    }

    items
}

/// "tokyo-night" -> "Tokyo Night"
fn titlecase(s: &str) -> String {
    s.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ── Rounded-rect primitives (local to the palette's "rounded panel" look) ──

/// Quarter-circle inset at a row `dist_from_edge` cells away from the nearest
/// top/bottom edge, for a corner radius `r`. Same construction the tab bar
/// uses for its rounded-top tabs, generalized to all four corners.
fn rounded_inset(dist_from_edge: i32, r: i32) -> i32 {
    if r <= 0 || dist_from_edge >= r {
        return 0;
    }
    let dy = r - dist_from_edge;
    let under = (r * r - dy * dy).max(0);
    (r as f32 - (under as f32).sqrt()).round().max(0.0) as i32
}

fn fill_rounded_rect(buffer: &mut [u32], buf_w: usize, x: usize, y: usize, w: usize, h: usize, radius: usize, color: u32) {
    if w == 0 || h == 0 {
        return;
    }
    let r = (radius as i32).min(w as i32 / 2).min(h as i32 / 2).max(0);
    for row in 0..h as i32 {
        let d = row.min(h as i32 - 1 - row);
        let inset = rounded_inset(d, r).max(0) as usize;
        if inset * 2 >= w {
            continue;
        }
        let xl = x + inset;
        let xr = x + w - inset;
        let yy = y + row as usize;
        let off = yy * buf_w + xl;
        let end = (off + (xr - xl)).min(buffer.len());
        if off < buffer.len() {
            buffer[off..end].fill(color);
        }
    }
}

fn draw_rounded_border(buffer: &mut [u32], buf_w: usize, x: usize, y: usize, w: usize, h: usize, radius: usize, color: u32) {
    if w == 0 || h == 0 {
        return;
    }
    let r = (radius as i32).min(w as i32 / 2).min(h as i32 / 2).max(0);

    // Left/right sides, including the curved corners.
    for row in 0..h as i32 {
        let d = row.min(h as i32 - 1 - row);
        let inset = rounded_inset(d, r).max(0) as usize;
        if inset * 2 >= w {
            continue;
        }
        let yy = y + row as usize;
        crate::ui::set_px(buffer, buf_w, yy, x + inset, color);
        crate::ui::set_px(buffer, buf_w, yy, x + w - inset - 1, color);
    }

    // Flat top/bottom edges between the corner arcs.
    let r_usize = r.max(0) as usize;
    if r_usize * 2 < w {
        let flat_l = x + r_usize;
        let flat_r = x + w - r_usize;
        let off_top = y * buf_w + flat_l;
        let end_top = (off_top + (flat_r - flat_l)).min(buffer.len());
        if off_top < buffer.len() {
            buffer[off_top..end_top].fill(color);
        }
        let off_bot = (y + h - 1) * buf_w + flat_l;
        let end_bot = (off_bot + (flat_r - flat_l)).min(buffer.len());
        if off_bot < buffer.len() {
            buffer[off_bot..end_bot].fill(color);
        }
    }
}
