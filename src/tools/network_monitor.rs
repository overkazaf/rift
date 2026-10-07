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
        if !self.visible { return; }
        crate::ui::dim_backdrop(buffer, 3);
        let cw = font.cell_width;
        let ch = font.cell_height;
        let pw = (55 * cw).min(width - 40);
        let items = self.connections.len().min(20);
        let ph = ((items + 4) * (ch + 3) + 40).min(height - 40).max(ch * 6);
        let px = (width - pw) / 2;
        let py = (height - ph) / 2;

        crate::ui::fill_rect(buffer, width, px, py, pw, ph, crate::ui::pack_rgb(crate::ui::lighten(theme.bg, 6)));
        crate::ui::draw_border(buffer, width, px, py, pw, ph, crate::ui::pack_rgb(crate::ui::dim(theme.cursor, 0.4)));

        let mut ty = py + 12;
        let total = format!("Network ({} connections)", self.connections.len());
        crate::ui::render_text(buffer, width, font, &total, px + 16, ty, theme.cursor);
        ty += ch + 8;

        for conn in self.connections.iter().take(items) {
            if ty + ch >= py + ph - 20 { break; }
            let color = match conn.state.as_str() {
                "ESTABLISHED" => (166, 227, 161),
                "LISTEN" => (137, 180, 250),
                "TIME_WAIT" => (108, 112, 134),
                _ => crate::ui::dim(theme.fg, 0.5),
            };
            let line = format!("{:<5} {:<22} {:<22} {}",
                conn.protocol,
                crate::ui::trunc(&conn.local_addr, 20),
                crate::ui::trunc(&conn.remote_addr, 20),
                conn.state);
            crate::ui::render_text(buffer, width, font, crate::ui::trunc(&line, (pw - 32) / cw), px + 16, ty, color);
            ty += ch + 3;
        }

        crate::ui::render_text(buffer, width, font, "r: refresh  Esc: close", px + 16, py + ph - ch - 10, crate::ui::dim(theme.fg, 0.3));
    }
}

pub enum NetMonKey { Escape, Char(char) }
