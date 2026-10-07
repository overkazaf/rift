pub struct SshTunnelView {
    pub visible: bool,
    tunnels: Vec<TunnelInfo>,
}

struct TunnelInfo {
    local_port: u16,
    remote_host: String,
    remote_port: u16,
    status: String,
}

impl SshTunnelView {
    pub fn new() -> Self { Self { visible: false, tunnels: Vec::new() } }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible { self.refresh(); }
    }

    pub fn refresh(&mut self) {
        self.tunnels.clear();
        if let Ok(out) = std::process::Command::new("lsof")
            .args(["-i", "-n", "-P"])
            .output()
        {
            if let Ok(text) = std::str::from_utf8(&out.stdout) {
                for line in text.lines() {
                    if line.contains("ssh") && line.contains("LISTEN") {
                        let cols: Vec<&str> = line.split_whitespace().collect();
                        if cols.len() >= 9 {
                            let addr = cols[8];
                            if let Some(port_str) = addr.rsplit(':').next() {
                                if let Ok(port) = port_str.parse::<u16>() {
                                    self.tunnels.push(TunnelInfo {
                                        local_port: port,
                                        remote_host: "localhost".into(),
                                        remote_port: port,
                                        status: "LISTENING".into(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn handle_key(&mut self, key: TunnelKey) {
        match key {
            TunnelKey::Escape => self.visible = false,
            TunnelKey::Char('r') => self.refresh(),
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
        let pw = (40 * cw).min(width - 40);
        let ph = ((self.tunnels.len() + 4) * (ch + 4) + 40).min(height - 40).max(ch * 6);
        let px = (width - pw) / 2;
        let py = (height - ph) / 2;

        crate::ui::fill_rect(buffer, width, px, py, pw, ph, crate::ui::pack_rgb(crate::ui::lighten(theme.bg, 6)));
        crate::ui::draw_border(buffer, width, px, py, pw, ph, crate::ui::pack_rgb(crate::ui::dim(theme.cursor, 0.4)));

        let mut ty = py + 12;
        crate::ui::render_text(buffer, width, font, "SSH Tunnels", px + 16, ty, theme.cursor);
        ty += ch + 8;

        if self.tunnels.is_empty() {
            crate::ui::render_text(buffer, width, font, "No active SSH tunnels detected", px + 16, ty, crate::ui::dim(theme.fg, 0.5));
        } else {
            for t in &self.tunnels {
                if ty + ch >= py + ph - 20 { break; }
                let line = format!(":{} -> {}:{} [{}]", t.local_port, t.remote_host, t.remote_port, t.status);
                crate::ui::render_text(buffer, width, font, &line, px + 16, ty, (166, 227, 161));
                ty += ch + 4;
            }
        }

        crate::ui::render_text(buffer, width, font, "r: refresh  Esc: close", px + 16, py + ph - ch - 10, crate::ui::dim(theme.fg, 0.3));
    }
}

pub enum TunnelKey { Escape, Char(char) }
