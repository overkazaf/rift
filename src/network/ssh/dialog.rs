use crate::config::Rgb;
use std::path::PathBuf;

// ── Public types ──

pub struct SshConnectRequest {
    pub alias: String,
    pub host: String,
    pub port: u16,
    pub user: String,
}

pub enum SshDialogKey {
    Char(char),
    Backspace,
    Delete,
    Enter,
    Tab,
    Up,
    Down,
    Escape,
}

// ── Saved host ──

#[derive(Clone)]
struct SavedHost {
    alias: String,
    host: String,
    user: String,
    port: u16,
}

// ── Dialog ──

enum Mode {
    List,
    Input,
}

struct InputField {
    label: &'static str,
    value: String,
    placeholder: String,
}

pub struct SshDialog {
    pub visible: bool,
    pub error_msg: Option<String>,
    mode: Mode,
    saved: Vec<SavedHost>,
    list_sel: usize,
    fields: Vec<InputField>,
    field_sel: usize,
}

impl SshDialog {
    pub fn new() -> Self {
        let saved = load_hosts();
        let user = whoami();
        Self {
            visible: false,
            error_msg: None,
            mode: if saved.is_empty() { Mode::Input } else { Mode::List },
            saved,
            list_sel: 0,
            fields: vec![
                InputField { label: "Alias", value: String::new(), placeholder: "dev-server".into() },
                InputField { label: "Host", value: String::new(), placeholder: "example.com".into() },
                InputField { label: "User", value: String::new(), placeholder: user },
                InputField { label: "Port", value: "22".into(), placeholder: "22".into() },
            ],
            field_sel: 0,
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.error_msg = None;
            self.saved = load_hosts();
            self.mode = if self.saved.is_empty() { Mode::Input } else { Mode::List };
            self.list_sel = 0;
            self.field_sel = 0;
        }
    }

    pub fn save_host(&mut self, req: &SshConnectRequest) {
        if self.saved.iter().any(|h| h.host == req.host && h.user == req.user && h.port == req.port) {
            return;
        }
        let alias = if req.alias.is_empty() {
            format!("{}@{}", req.user, req.host)
        } else {
            req.alias.clone()
        };
        self.saved.push(SavedHost {
            alias,
            host: req.host.clone(),
            user: req.user.clone(),
            port: req.port,
        });
        save_hosts(&self.saved);
    }

    pub fn handle_key(&mut self, key: SshDialogKey) -> Option<SshConnectRequest> {
        self.error_msg = None;
        match self.mode {
            Mode::List => self.handle_list_key(key),
            Mode::Input => self.handle_input_key(key),
        }
    }

    fn handle_list_key(&mut self, key: SshDialogKey) -> Option<SshConnectRequest> {
        let total = self.saved.len() + 1; // +1 for "New Connection"
        match key {
            SshDialogKey::Escape => { self.visible = false; None }
            SshDialogKey::Up => {
                self.list_sel = self.list_sel.checked_sub(1).unwrap_or(total - 1);
                None
            }
            SshDialogKey::Down | SshDialogKey::Tab => {
                self.list_sel = (self.list_sel + 1) % total;
                None
            }
            SshDialogKey::Enter => {
                if self.list_sel < self.saved.len() {
                    let h = &self.saved[self.list_sel];
                    self.visible = false;
                    Some(SshConnectRequest { alias: h.alias.clone(), host: h.host.clone(), port: h.port, user: h.user.clone() })
                } else {
                    self.mode = Mode::Input;
                    self.field_sel = 0;
                    for f in &mut self.fields { f.value.clear(); }
                    self.fields[3].value = "22".into();
                    None
                }
            }
            SshDialogKey::Delete | SshDialogKey::Backspace => {
                if self.list_sel < self.saved.len() {
                    self.saved.remove(self.list_sel);
                    save_hosts(&self.saved);
                    if self.list_sel > 0 && self.list_sel >= self.saved.len() {
                        self.list_sel = self.saved.len();
                    }
                    if self.saved.is_empty() {
                        self.mode = Mode::Input;
                    }
                }
                None
            }
            SshDialogKey::Char('n') | SshDialogKey::Char('N') => {
                self.mode = Mode::Input;
                self.field_sel = 0;
                None
            }
            _ => None,
        }
    }

    fn handle_input_key(&mut self, key: SshDialogKey) -> Option<SshConnectRequest> {
        match key {
            SshDialogKey::Escape => {
                if !self.saved.is_empty() {
                    self.mode = Mode::List;
                } else {
                    self.visible = false;
                }
                None
            }
            SshDialogKey::Tab | SshDialogKey::Down => {
                self.field_sel = (self.field_sel + 1) % self.fields.len();
                None
            }
            SshDialogKey::Up => {
                self.field_sel = self.field_sel.checked_sub(1).unwrap_or(self.fields.len() - 1);
                None
            }
            SshDialogKey::Char(c) => {
                self.fields[self.field_sel].value.push(c);
                None
            }
            SshDialogKey::Backspace => {
                self.fields[self.field_sel].value.pop();
                None
            }
            SshDialogKey::Enter => {
                if self.field_sel < self.fields.len() - 1 {
                    self.field_sel += 1;
                    return None;
                }
                let alias = self.fields[0].value.trim().to_string();
                let host = self.fields[1].value.trim().to_string();
                if host.is_empty() {
                    self.error_msg = Some("Host is required".into());
                    self.field_sel = 1;
                    return None;
                }
                let user = {
                    let v = self.fields[2].value.trim();
                    if v.is_empty() { self.fields[2].placeholder.clone() } else { v.to_string() }
                };
                let port = self.fields[3].value.trim().parse::<u16>().unwrap_or(22);
                self.visible = false;
                Some(SshConnectRequest { alias, host, port, user })
            }
            SshDialogKey::Delete => None,
        }
    }

    // ── Rendering ──

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut crate::renderer::font::FontManager,
        theme: &crate::config::Theme,
    ) {
        if !self.visible { return; }

        // Dim overlay
        for px in buffer.iter_mut() {
            let r = ((*px >> 16) & 0xff) / 3;
            let g = ((*px >> 8) & 0xff) / 3;
            let b = (*px & 0xff) / 3;
            *px = (r << 16) | (g << 8) | b;
        }

        let cw = font.cell_width;
        let ch = font.cell_height;

        match self.mode {
            Mode::List => self.render_list(buffer, width, height, font, theme, cw, ch),
            Mode::Input => self.render_input(buffer, width, height, font, theme, cw, ch),
        }
    }

    fn render_list(
        &self, buffer: &mut [u32], width: usize, height: usize,
        font: &mut crate::renderer::font::FontManager,
        theme: &crate::config::Theme, cw: usize, ch: usize,
    ) {
        let row_h = ch + 10;
        let item_count = self.saved.len() + 1;
        let panel_w = (38 * cw).max(400).min(width.saturating_sub(40));
        let panel_h = (ch + 20 + ch + 12 + item_count * row_h + 8 + row_h + 16 + ch + 20)
            .min(height.saturating_sub(40));
        let px0 = (width.saturating_sub(panel_w)) / 2;
        let py0 = (height.saturating_sub(panel_h)) / 2;

        let bg = darken(theme.bg, 5);
        fill_rect(buffer, width, px0, py0, panel_w, panel_h, pack(bg.0, bg.1, bg.2));
        draw_border(buffer, width, px0, py0, panel_w, panel_h, pack_dim(theme.cursor, 0.5));

        let mut cy = py0 + 14;

        // Title
        render_text(buffer, width, font, "SSH Connect", px0 + 18, cy, theme.cursor);
        cy += ch + 14;

        // "Saved Connections" label
        render_text(buffer, width, font, "Saved Connections", px0 + 18, cy, dim(theme.fg, 0.45));
        cy += ch + 8;

        // Saved hosts list
        for (i, host) in self.saved.iter().enumerate() {
            let is_sel = i == self.list_sel;
            if is_sel {
                let hl = lighten(bg, 16);
                fill_rect(buffer, width, px0 + 4, cy, panel_w - 8, row_h, pack(hl.0, hl.1, hl.2));
                // Accent left bar
                let accent_px = pack(theme.cursor.0, theme.cursor.1, theme.cursor.2);
                for y in (cy + 2)..(cy + row_h - 2) {
                    set_px(buffer, y * width + px0 + 6, accent_px);
                    set_px(buffer, y * width + px0 + 7, accent_px);
                }
            }

            let ty = cy + (row_h.saturating_sub(ch)) / 2;
            let text_c = if is_sel { theme.fg } else { dim(theme.fg, 0.6) };
            let detail = format!("{}@{}:{}", host.user, host.host, host.port);

            // Alias on left (or fallback to user@host)
            let display_name = if host.alias.is_empty() { &detail } else { &host.alias };
            render_text(buffer, width, font, display_name, px0 + 22, ty, text_c);
            // Detail on right, dimmer (only if alias is set, to avoid duplication)
            if !host.alias.is_empty() {
                let detail_w = detail.chars().count() * cw;
                let dx = (px0 + panel_w).saturating_sub(detail_w + 18);
                render_text(buffer, width, font, &detail, dx, ty, dim(theme.fg, 0.3));
            }

            cy += row_h;
        }

        // Separator
        cy += 4;
        let sep_c = dim(theme.fg, 0.1);
        for x in (px0 + 14)..(px0 + panel_w - 14) {
            set_px(buffer, cy * width + x, pack(sep_c.0, sep_c.1, sep_c.2));
        }
        cy += 8;

        // "+ New Connection"
        let new_sel = self.list_sel == self.saved.len();
        if new_sel {
            let hl = lighten(bg, 16);
            fill_rect(buffer, width, px0 + 4, cy, panel_w - 8, row_h, pack(hl.0, hl.1, hl.2));
            let accent_px = pack(theme.cursor.0, theme.cursor.1, theme.cursor.2);
            for y in (cy + 2)..(cy + row_h - 2) {
                set_px(buffer, y * width + px0 + 6, accent_px);
                set_px(buffer, y * width + px0 + 7, accent_px);
            }
        }
        let ty = cy + (row_h.saturating_sub(ch)) / 2;
        render_text(buffer, width, font, "+", px0 + 22, ty, theme.cursor);
        let nc = if new_sel { theme.fg } else { dim(theme.fg, 0.5) };
        render_text(buffer, width, font, "New Connection", px0 + 22 + cw * 2, ty, nc);
        let _ = cy; // suppress unused assignment warning

        // Help
        let help_y = (py0 + panel_h).saturating_sub(ch + 12);
        render_text(buffer, width, font, "Enter:connect  Del:remove  N:new  Esc:close", px0 + 18, help_y, dim(theme.fg, 0.25));
    }

    fn render_input(
        &self, buffer: &mut [u32], width: usize, height: usize,
        font: &mut crate::renderer::font::FontManager,
        theme: &crate::config::Theme, cw: usize, ch: usize,
    ) {
        let field_row_h = ch + 16;
        let panel_w = (38 * cw).max(400).min(width.saturating_sub(40));
        let panel_h = ch + 20 + self.fields.len() * field_row_h + 20 + ch + 24;
        let px0 = (width.saturating_sub(panel_w)) / 2;
        let py0 = (height.saturating_sub(panel_h)) / 2;

        let bg = darken(theme.bg, 5);
        fill_rect(buffer, width, px0, py0, panel_w, panel_h, pack(bg.0, bg.1, bg.2));
        draw_border(buffer, width, px0, py0, panel_w, panel_h, pack_dim(theme.cursor, 0.5));

        let mut cy = py0 + 14;

        // Title
        render_text(buffer, width, font, "New SSH Connection", px0 + 18, cy, theme.cursor);

        // Error
        if let Some(ref err) = self.error_msg {
            let ex = px0 + 18 + cw * 20;
            render_text(buffer, width, font, err, ex, cy, (255, 100, 100));
        }
        cy += ch + 14;

        // Fields
        let label_w = 6 * cw;
        let input_x = px0 + 18 + label_w + cw;
        let input_w = panel_w.saturating_sub(36 + label_w + cw);

        for (i, field) in self.fields.iter().enumerate() {
            let is_act = i == self.field_sel;
            let fy = cy + (field_row_h.saturating_sub(ch)) / 2;

            // Label
            let lc = if is_act { theme.fg } else { dim(theme.fg, 0.45) };
            render_text(buffer, width, font, field.label, px0 + 18, fy, lc);

            // Input box
            let box_bg = if is_act { lighten(bg, 14) } else { lighten(bg, 6) };
            let box_top = fy.saturating_sub(4);
            let box_h = ch + 8;
            fill_rect(buffer, width, input_x, box_top, input_w, box_h, pack(box_bg.0, box_bg.1, box_bg.2));

            // Active underline
            if is_act {
                let ul_y = box_top + box_h - 1;
                let ul_px = pack_dim(theme.cursor, 0.7);
                for x in input_x..input_x + input_w {
                    set_px(buffer, ul_y * width + x, ul_px);
                }
            }

            // Text or placeholder
            let (display, tc) = if field.value.is_empty() {
                (&field.placeholder as &str, dim(theme.fg, 0.2))
            } else {
                (field.value.as_str(), if is_act { theme.fg } else { dim(theme.fg, 0.65) })
            };
            render_text(buffer, width, font, display, input_x + 6, fy, tc);

            // Cursor
            if is_act {
                let cx_pos = input_x + 6 + field.value.len() * cw;
                let cursor_px = pack(theme.cursor.0, theme.cursor.1, theme.cursor.2);
                for y in fy..fy + ch {
                    set_px(buffer, y * width + cx_pos, cursor_px);
                    if cx_pos + 1 < width { set_px(buffer, y * width + cx_pos + 1, cursor_px); }
                }
            }

            cy += field_row_h;
        }

        // Help
        let back = if !self.saved.is_empty() { "Esc:back" } else { "Esc:close" };
        let help = format!("Tab:next  Enter:connect  {}", back);
        let help_y = (py0 + panel_h).saturating_sub(ch + 12);
        render_text(buffer, width, font, &help, px0 + 18, help_y, dim(theme.fg, 0.25));
    }
}

// ── Persistence ──

fn hosts_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rift")
        .join("ssh_hosts.json")
}

fn save_hosts(hosts: &[SavedHost]) {
    let path = hosts_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut json = String::from("[\n");
    for (i, h) in hosts.iter().enumerate() {
        if i > 0 { json.push_str(",\n"); }
        json.push_str(&format!(
            "  {{\"alias\":\"{}\",\"host\":\"{}\",\"user\":\"{}\",\"port\":{}}}",
            esc(&h.alias), esc(&h.host), esc(&h.user), h.port
        ));
    }
    json.push_str("\n]");
    let _ = std::fs::write(&path, json);
}

fn load_hosts() -> Vec<SavedHost> {
    let path = hosts_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut hosts = Vec::new();
    for chunk in content.split('{').skip(1) {
        let Some(end) = chunk.find('}') else { continue };
        let obj = &chunk[..end];
        let alias = field_str(obj, "alias")
            .or_else(|| field_str(obj, "label")) // backward compat
            .unwrap_or_default();
        let host = field_str(obj, "host").unwrap_or_default();
        let user = field_str(obj, "user").unwrap_or_default();
        let port = field_num(obj, "port").unwrap_or(22) as u16;
        if !host.is_empty() {
            hosts.push(SavedHost { alias, host, user, port });
        }
    }
    hosts
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn field_str(obj: &str, key: &str) -> Option<String> {
    let pat = format!("\"{}\":\"", key);
    let start = obj.find(&pat)? + pat.len();
    let rest = &obj[start..];
    let mut end = 0;
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        if escaped { escaped = false; continue; }
        if c == '\\' { escaped = true; continue; }
        if c == '"' { end = i; break; }
    }
    Some(rest[..end].to_string())
}

fn field_num(obj: &str, key: &str) -> Option<u64> {
    let pat = format!("\"{}\":", key);
    let start = obj.find(&pat)? + pat.len();
    let rest = &obj[start..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].trim().parse().ok()
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "root".to_string())
}

// ── Drawing helpers (delegated to crate::ui) ──

use crate::ui::{render_text, fill_rect, draw_border, pack, darken, dim, lighten};

fn set_px(buffer: &mut [u32], idx: usize, val: u32) {
    if idx < buffer.len() { buffer[idx] = val; }
}
fn pack_dim(c: Rgb, f: f32) -> u32 { let d = dim(c, f); pack(d.0, d.1, d.2) }
