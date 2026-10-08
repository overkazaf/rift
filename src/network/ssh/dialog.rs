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

    /// Saved hosts as ready-to-connect requests (used by the command palette).
    pub fn saved_hosts(&self) -> Vec<SshConnectRequest> {
        self.saved
            .iter()
            .map(|h| SshConnectRequest {
                alias: if h.alias.is_empty() { format!("{}@{}", h.user, h.host) } else { h.alias.clone() },
                host: h.host.clone(),
                port: h.port,
                user: h.user.clone(),
            })
            .collect()
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
        use crate::ui::kit::{Ctx, ListItem, PanelSpec, Rect, Tokens, Tone};
        if !self.visible { return; }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(tk.backdrop);

        match self.mode {
            Mode::List => {
                let rows = self.saved.len() + 1;
                let want = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + rows.min(12) * tk.row_h;
                let rect = cx.centered_cols(52, want);
                let count = format!("{} saved", self.saved.len());
                let spec = PanelSpec::new("SSH Connect")
                    .sub("saved connections")
                    .badge(&count, Tone::Neutral)
                    .hints(&[("Enter", "connect"), ("Del", "remove"), ("N", "new"), ("Esc", "close")]);
                let body = cx.panel(rect, &spec);

                let details: Vec<String> = self.saved.iter().map(|h| format!("{}@{}:{}", h.user, h.host, h.port)).collect();
                let mut items: Vec<ListItem> = self.saved.iter().zip(&details).map(|(h, d)| {
                    if h.alias.is_empty() { ListItem::new(d) } else { ListItem::new(&h.alias).meta(d) }
                }).collect();
                items.push(ListItem::new("+ New Connection").tone(Tone::Accent));
                let vis = cx.rows_fit(body.h);
                let scroll = crate::ui::kit::center_scroll(self.list_sel, items.len(), vis);
                cx.list(body, &items, Some(self.list_sel), scroll, None);
            }
            Mode::Input => {
                let field_h = tk.input_h + tk.sp.sm;
                let want = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + self.fields.len() * field_h + tk.row_h;
                let rect = cx.centered_cols(52, want);
                let back = if !self.saved.is_empty() { "back" } else { "close" };
                let hints = [("Tab", "next field"), ("Enter", "connect"), ("Esc", back)];
                let mut spec = PanelSpec::new("New SSH Connection")
                    .sub("host details")
                    .hints(&hints);
                if let Some(ref err) = self.error_msg {
                    spec = spec.badge(err, Tone::Danger);
                }
                let body = cx.panel(rect, &spec);

                let label_w = 7 * tk.cw;
                for (i, field) in self.fields.iter().enumerate() {
                    let y = body.y + i * field_h;
                    let is_act = i == self.field_sel;
                    let ty = cx.text_y(y, tk.input_h);
                    cx.text(body.x, ty, field.label, if is_act { tk.text } else { tk.text_muted });
                    let ix = body.x + label_w;
                    let input = Rect::new(ix, y, body.right().saturating_sub(ix), tk.input_h);
                    cx.text_input(input, &field.value, field.value.chars().count(), None, &field.placeholder, is_act);
                }
            }
        }
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
