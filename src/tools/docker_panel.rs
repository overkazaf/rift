#[allow(dead_code)]

pub struct DockerPanel {
    pub visible: bool,
    containers: Vec<Container>,
    images: Vec<DockerImage>,
    selected: usize,
    active_tab: usize,
}

struct Container {
    id: String,
    name: String,
    image: String,
    status: String,
    ports: String,
    running: bool,
}

struct DockerImage {
    id: String,
    repo: String,
    tag: String,
    size: String,
}

impl DockerPanel {
    pub fn new() -> Self {
        Self { visible: false, containers: Vec::new(), images: Vec::new(), selected: 0, active_tab: 0 }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible { self.fetch(); }
    }

    pub fn fetch(&mut self) {
        self.fetch_containers();
        self.fetch_images();
    }

    fn fetch_containers(&mut self) {
        self.containers.clear();
        if let Ok(out) = std::process::Command::new("docker")
            .args(["ps", "-a", "--format", "{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.Status}}\t{{.Ports}}"])
            .output()
        {
            if out.status.success() {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    let cols: Vec<&str> = line.split('\t').collect();
                    if cols.len() >= 4 {
                        self.containers.push(Container {
                            id: cols[0].to_string(),
                            name: cols[1].to_string(),
                            image: cols[2].to_string(),
                            status: cols[3].to_string(),
                            ports: cols.get(4).unwrap_or(&"").to_string(),
                            running: cols[3].starts_with("Up"),
                        });
                    }
                }
            }
        }
    }

    fn fetch_images(&mut self) {
        self.images.clear();
        if let Ok(out) = std::process::Command::new("docker")
            .args(["images", "--format", "{{.ID}}\t{{.Repository}}\t{{.Tag}}\t{{.Size}}"])
            .output()
        {
            if out.status.success() {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    let cols: Vec<&str> = line.split('\t').collect();
                    if cols.len() >= 4 {
                        self.images.push(DockerImage {
                            id: cols[0].to_string(),
                            repo: cols[1].to_string(),
                            tag: cols[2].to_string(),
                            size: cols[3].to_string(),
                        });
                    }
                }
            }
        }
    }

    pub fn handle_key(&mut self, key: DockerKey) -> Option<DockerAction> {
        match key {
            DockerKey::Escape => { self.visible = false; None }
            DockerKey::Up => { self.selected = self.selected.saturating_sub(1); None }
            DockerKey::Down => {
                let max = if self.active_tab == 0 { self.containers.len() } else { self.images.len() };
                if self.selected + 1 < max { self.selected += 1; }
                None
            }
            DockerKey::Tab => { self.active_tab = 1 - self.active_tab; self.selected = 0; None }
            DockerKey::Char('r') => { self.fetch(); None }
            DockerKey::Char('s') => {
                if self.active_tab == 0 {
                    if let Some(c) = self.containers.get(self.selected) {
                        let action = if c.running { "stop" } else { "start" };
                        return Some(DockerAction::RunCommand(format!("docker {} {}", action, c.name)));
                    }
                }
                None
            }
            DockerKey::Char('l') => {
                if self.active_tab == 0 {
                    if let Some(c) = self.containers.get(self.selected) {
                        return Some(DockerAction::RunCommand(format!("docker logs -f --tail 100 {}", c.name)));
                    }
                }
                None
            }
            _ => None,
        }
    }

    pub fn render(&self, buffer: &mut [u32], width: usize, height: usize,
                  font: &mut crate::renderer::font::FontManager, theme: &crate::config::Theme) {
        if !self.visible { return; }
        for px in buffer.iter_mut() {
            let r = ((*px >> 16) & 0xff) / 3;
            let g = ((*px >> 8) & 0xff) / 3;
            let b = (*px & 0xff) / 3;
            *px = (r << 16) | (g << 8) | b;
        }

        let cw = font.cell_width;
        let ch = font.cell_height;
        let pw = (50 * cw).min(width.saturating_sub(40));
        let items = if self.active_tab == 0 { self.containers.len() } else { self.images.len() };
        let ph = ((items + 6) * (ch + 4) + 40).min(height.saturating_sub(40)).max(ch * 8);
        let px = (width.saturating_sub(pw)) / 2;
        let py = (height.saturating_sub(ph)) / 2;

        let bg = crate::ui::lighten(theme.bg, 6);
        crate::ui::fill_rect(buffer, width, px, py, pw, ph, crate::ui::pack_rgb(bg));
        crate::ui::draw_border(buffer, width, px, py, pw, ph, crate::ui::pack_rgb(crate::ui::dim(theme.cursor, 0.4)));

        let mut ty = py + 12;
        let max_chars = (pw.saturating_sub(32)) / cw.max(1);

        // Title
        crate::ui::render_text(buffer, width, font, "Docker", px + 16, ty, theme.cursor);
        ty += ch + 8;

        // Tab selector
        let tabs = ["Containers", "Images"];
        let mut tx = px + 16;
        for (i, tab) in tabs.iter().enumerate() {
            let color = if i == self.active_tab { theme.cursor } else { crate::ui::dim(theme.fg, 0.4) };
            crate::ui::render_text(buffer, width, font, tab, tx, ty, color);
            tx += (tab.len() + 2) * cw;
        }
        ty += ch + 8;

        // Items
        if self.active_tab == 0 {
            if self.containers.is_empty() {
                crate::ui::render_text(buffer, width, font, "No containers", px + 16, ty, crate::ui::dim(theme.fg, 0.4));
            }
            for (i, c) in self.containers.iter().enumerate() {
                if ty + ch >= py + ph.saturating_sub(30) { break; }
                let sel = i == self.selected;
                if sel {
                    crate::ui::fill_rect(buffer, width, px + 4, ty.saturating_sub(2), pw.saturating_sub(8), ch + 4,
                        crate::ui::pack_rgb(crate::ui::lighten(bg, 12)));
                }
                let icon_color = if c.running { (166, 227, 161) } else { (243, 139, 168) };
                let icon = if c.running { ">" } else { "x" };
                crate::ui::render_text(buffer, width, font, icon, px + 16, ty, icon_color);
                let label = format!("{} ({})", c.name, crate::ui::trunc(&c.image, 20));
                crate::ui::render_text(buffer, width, font,
                    crate::ui::trunc(&label, max_chars.saturating_sub(4)),
                    px + 16 + 3 * cw, ty,
                    if sel { theme.fg } else { crate::ui::dim(theme.fg, 0.7) });
                ty += ch + 4;
            }
        } else {
            if self.images.is_empty() {
                crate::ui::render_text(buffer, width, font, "No images", px + 16, ty, crate::ui::dim(theme.fg, 0.4));
            }
            for (i, img) in self.images.iter().enumerate() {
                if ty + ch >= py + ph.saturating_sub(30) { break; }
                let sel = i == self.selected;
                if sel {
                    crate::ui::fill_rect(buffer, width, px + 4, ty.saturating_sub(2), pw.saturating_sub(8), ch + 4,
                        crate::ui::pack_rgb(crate::ui::lighten(bg, 12)));
                }
                let label = format!("{}:{} ({})", img.repo, img.tag, img.size);
                crate::ui::render_text(buffer, width, font,
                    crate::ui::trunc(&label, max_chars),
                    px + 16, ty,
                    if sel { theme.fg } else { crate::ui::dim(theme.fg, 0.7) });
                ty += ch + 4;
            }
        }

        // Help
        let help = "Tab:switch s:start/stop l:logs r:refresh Esc:close";
        crate::ui::render_text(buffer, width, font,
            crate::ui::trunc(help, max_chars),
            px + 16, py + ph.saturating_sub(ch + 10),
            crate::ui::dim(theme.fg, 0.3));
    }
}

pub enum DockerKey { Up, Down, Tab, Escape, Char(char) }
pub enum DockerAction { RunCommand(String) }
