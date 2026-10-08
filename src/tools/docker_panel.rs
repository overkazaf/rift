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
        use crate::ui::kit::{center_scroll, Column, Ctx, PanelSpec, Rect, TableRow, Tokens, Tone, Width};
        if !self.visible { return; }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(tk.backdrop);

        let items = if self.active_tab == 0 { self.containers.len() } else { self.images.len() };
        let want = cx.title_h() + cx.footer_h() + 3 * tk.sp.md + (items.max(3) + 2) * tk.row_h;
        let rect = cx.centered_cols(78, want.min(height * 3 / 4));
        let running = self.containers.iter().filter(|c| c.running).count();
        let badge = format!("{} running", running);
        let spec = PanelSpec::new("Docker")
            .sub(if self.active_tab == 0 { "containers" } else { "images" })
            .badge(&badge, if running > 0 { Tone::Success } else { Tone::Neutral })
            .hints(&[("Tab", "switch"), ("s", "start/stop"), ("l", "logs"), ("r", "refresh"), ("Esc", "close")]);
        let body = cx.panel(rect, &spec);

        cx.tabs(body.x, body.y, body.w, &["Containers", "Images"], self.active_tab);
        cx.divider(body.x, body.y + tk.row_h, body.w);
        let content = Rect::new(body.x, body.y + tk.row_h + tk.sp.sm, body.w, body.h.saturating_sub(tk.row_h + tk.sp.sm));
        let vis = cx.rows_fit(content.h.saturating_sub(tk.row_h));

        if self.active_tab == 0 {
            if self.containers.is_empty() {
                cx.empty_state(content, "No containers", "Is the Docker daemon running?");
                return;
            }
            let rows: Vec<TableRow> = self.containers.iter().map(|c| {
                TableRow::new(vec![
                    if c.running { "running".to_string() } else { "stopped".to_string() },
                    c.name.clone(), c.image.clone(), c.ports.clone(),
                ]).tone(if c.running { Tone::Success } else { Tone::Danger })
            }).collect();
            let scroll = center_scroll(self.selected, rows.len(), vis);
            cx.table(content, &[
                Column::new("State", Width::Cols(8)), Column::new("Name", Width::Flex(2)),
                Column::new("Image", Width::Flex(2)), Column::new("Ports", Width::Flex(2)),
            ], &rows, Some(self.selected), scroll);
        } else {
            if self.images.is_empty() {
                cx.empty_state(content, "No images", "");
                return;
            }
            let rows: Vec<TableRow> = self.images.iter().map(|i| {
                TableRow::new(vec![format!("{}:{}", i.repo, i.tag), i.id.clone(), i.size.clone()])
            }).collect();
            let scroll = center_scroll(self.selected, rows.len(), vis);
            cx.table(content, &[
                Column::new("Repository", Width::Flex(3)), Column::new("ID", Width::Cols(13)),
                Column::new("Size", Width::Cols(9)).right(),
            ], &rows, Some(self.selected), scroll);
        }
    }
}

pub enum DockerKey { Up, Down, Tab, Escape, Char(char) }
pub enum DockerAction { RunCommand(String) }

#[cfg(test)]
mod qa_tests {
    use super::*;
    use crate::ui::kit::gallery::qa::each_theme;

    #[test]
    fn renders_tabs_and_empty() {
        let mut d = DockerPanel::new();
        d.visible = true;
        d.containers = vec![
            Container { id: "abc".into(), name: "web".into(), image: "nginx:latest".into(), status: "Up 2h".into(), ports: "0.0.0.0:80->80/tcp".into(), running: true },
            Container { id: "def".into(), name: "db".into(), image: "postgres:16".into(), status: "Exited".into(), ports: String::new(), running: false },
        ];
        d.images = vec![DockerImage { id: "1234567890ab".into(), repo: "nginx".into(), tag: "latest".into(), size: "187MB".into() }];
        each_theme("docker-containers", |b, w, h, f, t| d.render(b, w, h, f, t));
        d.active_tab = 1;
        each_theme("docker-images", |b, w, h, f, t| d.render(b, w, h, f, t));
        d.images.clear();
        each_theme("docker-empty", |b, w, h, f, t| d.render(b, w, h, f, t));
    }
}
