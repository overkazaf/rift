pub struct NetworkMonitor {
    pub visible: bool,
    connections: Vec<NetConnection>,
}

struct NetConnection {
    protocol: String,
    local_addr: String,
    remote_addr: String,
    state: String,
}

impl NetworkMonitor {
    pub fn new() -> Self { Self { visible: false, connections: Vec::new() } }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible { self.refresh(); }
    }

    pub fn refresh(&mut self) {
        self.connections.clear();
        let output = std::process::Command::new("netstat")
            .args(["-an"])
            .output();

        if let Ok(out) = output {
            if let Ok(text) = std::str::from_utf8(&out.stdout) {
                for line in text.lines().skip(2).take(50) {
                    let cols: Vec<&str> = line.split_whitespace().collect();
                    if cols.len() >= 4 {
                        let proto = cols[0].to_string();
                        if proto.starts_with("tcp") || proto.starts_with("udp") {
                            self.connections.push(NetConnection {
                                protocol: proto,
                                local_addr: cols.get(3).unwrap_or(&"").to_string(),
                                remote_addr: cols.get(4).unwrap_or(&"").to_string(),
                                state: cols.last().unwrap_or(&"").to_string(),
                            });
                        }
                    }
                }
            }
        }
    }

    pub fn handle_key(&mut self, key: NetMonKey) {
        match key {
            NetMonKey::Escape => self.visible = false,
            NetMonKey::Char('r') => self.refresh(),
            _ => {}
        }
    }

    pub fn render(
        &self, buffer: &mut [u32], width: usize, height: usize,
        font: &mut crate::renderer::font::FontManager, theme: &crate::config::Theme,
    ) {
        use crate::ui::kit::{Column, Ctx, PanelSpec, TableRow, Tokens, Tone, Width};
        if !self.visible { return; }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(tk.backdrop);

        let items = self.connections.len().min(20);
        let want = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + (items.max(3) + 1) * tk.row_h;
        let rect = cx.centered_cols(80, want.min(height * 3 / 4));
        let count = format!("{} connections", self.connections.len());
        let spec = PanelSpec::new("Network")
            .sub("netstat")
            .badge(&count, Tone::Neutral)
            .hints(&[("r", "refresh"), ("Esc", "close")]);
        let body = cx.panel(rect, &spec);
        if self.connections.is_empty() {
            cx.empty_state(body, "No connections", "netstat returned nothing");
            return;
        }
        let rows: Vec<TableRow> = self.connections.iter().map(|c| {
            let tone = match c.state.as_str() {
                "ESTABLISHED" => Tone::Success,
                "LISTEN" => Tone::Accent,
                "TIME_WAIT" | "CLOSE_WAIT" => Tone::Neutral,
                _ => Tone::Warning,
            };
            TableRow::new(vec![c.state.clone(), c.protocol.clone(), c.local_addr.clone(), c.remote_addr.clone()]).tone(tone)
        }).collect();
        cx.table(body, &[
            Column::new("State", Width::Cols(12)), Column::new("Proto", Width::Cols(5)),
            Column::new("Local", Width::Flex(1)), Column::new("Remote", Width::Flex(1)),
        ], &rows, None, 0);
    }
}

pub enum NetMonKey { Escape, Char(char) }
