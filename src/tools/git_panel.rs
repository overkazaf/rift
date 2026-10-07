use std::process::Command;

pub struct GitPanel {
    pub visible: bool,
    info: Option<GitInfo>,
    active_section: usize,
    scroll: usize,
}

struct GitInfo {
    branch: String,
    status: Vec<FileStatus>,
    log: Vec<CommitInfo>,
    branches: Vec<String>,
    is_dirty: bool,
}

struct FileStatus {
    state: char,
    path: String,
}

struct CommitInfo {
    hash: String,
    message: String,
    graph: String,
}

pub enum GitPanelKey {
    Up,
    Down,
    Tab,
    Enter,
    Escape,
    Char(char),
}

impl GitPanel {
    pub fn new() -> Self {
        Self {
            visible: false,
            info: None,
            active_section: 0,
            scroll: 0,
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.refresh();
        }
    }

    pub fn refresh(&mut self) {
        self.info = GitInfo::collect();
    }

    pub fn handle_key(&mut self, key: GitPanelKey) {
        match key {
            GitPanelKey::Escape => self.visible = false,
            GitPanelKey::Tab => {
                self.active_section = (self.active_section + 1) % 3;
                self.scroll = 0;
            }
            GitPanelKey::Up => {
                self.scroll = self.scroll.saturating_sub(1);
            }
            GitPanelKey::Down => {
                self.scroll += 1;
            }
            GitPanelKey::Char('r') => self.refresh(),
            GitPanelKey::Enter | GitPanelKey::Char(_) => {}
        }
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut crate::renderer::font::FontManager,
        theme: &crate::config::Theme,
    ) {
        if !self.visible {
            return;
        }
        let Some(ref info) = self.info else {
            return;
        };

        // Dim backdrop
        for px in buffer.iter_mut() {
            let r = ((*px >> 16) & 0xff) / 3;
            let g = ((*px >> 8) & 0xff) / 3;
            let b = (*px & 0xff) / 3;
            *px = (r << 16) | (g << 8) | b;
        }

        let cw = font.cell_width;
        let ch = font.cell_height;
        let panel_w = (50 * cw).min(width.saturating_sub(40));
        let panel_h = (height * 3 / 4).min(height.saturating_sub(40));
        let px = (width.saturating_sub(panel_w)) / 2;
        let py = (height.saturating_sub(panel_h)) / 2;

        let bg = crate::ui::lighten(theme.bg, 6);
        crate::ui::fill_rect(buffer, width, px, py, panel_w, panel_h, crate::ui::pack_rgb(bg));
        let border = crate::ui::dim(theme.cursor, 0.4);
        crate::ui::draw_border(
            buffer,
            width,
            px,
            py,
            panel_w,
            panel_h,
            crate::ui::pack_rgb(border),
        );

        let content_x = px + 16;
        let mut cy = py + 12;
        let max_chars = (panel_w.saturating_sub(32)) / cw.max(1);

        // Title
        let dirty = if info.is_dirty { "*" } else { "" };
        let title = format!("Git: {}{}", info.branch, dirty);
        crate::ui::render_text(buffer, width, font, &title, content_x, cy, theme.cursor);
        cy += ch + 8;

        // Section tabs
        let sections = ["Status", "Log", "Branches"];
        let mut tx = content_x;
        for (i, name) in sections.iter().enumerate() {
            let color = if i == self.active_section {
                theme.cursor
            } else {
                crate::ui::dim(theme.fg, 0.4)
            };
            crate::ui::render_text(buffer, width, font, name, tx, cy, color);
            if i == self.active_section {
                let uw = name.len() * cw;
                for x in tx..tx + uw {
                    crate::ui::set_px(
                        buffer,
                        width,
                        cy + ch + 2,
                        x,
                        crate::ui::pack_rgb(theme.cursor),
                    );
                }
            }
            tx += name.len() * cw + 20;
        }
        cy += ch + 8;

        // Separator
        let sep = crate::ui::dim(theme.fg, 0.1);
        let sep_end = (content_x + panel_w).saturating_sub(48);
        for x in content_x..sep_end.min(width) {
            crate::ui::set_px(buffer, width, cy, x, crate::ui::pack_rgb(sep));
        }
        cy += 8;

        // Content
        let bottom_limit = py + panel_h - ch * 2;
        match self.active_section {
            0 => {
                if info.status.is_empty() {
                    crate::ui::render_text(
                        buffer,
                        width,
                        font,
                        "Clean working tree",
                        content_x,
                        cy,
                        crate::ui::dim(theme.fg, 0.5),
                    );
                } else {
                    for file in info.status.iter().skip(self.scroll) {
                        if cy + ch >= bottom_limit {
                            break;
                        }
                        let state_color = match file.state {
                            'M' => (255, 180, 60),
                            'A' => (80, 200, 120),
                            'D' => (255, 80, 80),
                            '?' => (120, 120, 140),
                            _ => theme.fg,
                        };
                        let state_str = format!("{} ", file.state);
                        crate::ui::render_text(
                            buffer,
                            width,
                            font,
                            &state_str,
                            content_x,
                            cy,
                            state_color,
                        );
                        let path_trunc =
                            crate::ui::trunc(&file.path, max_chars.saturating_sub(3));
                        crate::ui::render_text(
                            buffer,
                            width,
                            font,
                            path_trunc,
                            content_x + 3 * cw,
                            cy,
                            crate::ui::dim(theme.fg, 0.7),
                        );
                        cy += ch + 2;
                    }
                }
            }
            1 => {
                for commit in info.log.iter().skip(self.scroll) {
                    if cy + ch >= bottom_limit {
                        break;
                    }
                    let graph_color = crate::ui::dim(theme.cursor, 0.6);
                    crate::ui::render_text(
                        buffer,
                        width,
                        font,
                        &commit.graph,
                        content_x,
                        cy,
                        graph_color,
                    );

                    let hash_x = content_x + (commit.graph.len() + 1) * cw;
                    crate::ui::render_text(
                        buffer,
                        width,
                        font,
                        &commit.hash,
                        hash_x,
                        cy,
                        (255, 180, 60),
                    );

                    let msg_x = hash_x + 9 * cw;
                    let max_msg = max_chars.saturating_sub(commit.graph.len() + 10);
                    let msg = crate::ui::trunc(&commit.message, max_msg);
                    crate::ui::render_text(buffer, width, font, msg, msg_x, cy, theme.fg);
                    cy += ch + 2;
                }
            }
            2 => {
                for branch in info.branches.iter().skip(self.scroll) {
                    if cy + ch >= bottom_limit {
                        break;
                    }
                    let is_current = branch.starts_with('*');
                    let color = if is_current {
                        theme.cursor
                    } else {
                        crate::ui::dim(theme.fg, 0.7)
                    };
                    let display = crate::ui::trunc(branch, max_chars);
                    crate::ui::render_text(buffer, width, font, display, content_x, cy, color);
                    cy += ch + 2;
                }
            }
            _ => {}
        }

        // Help
        let help = "Tab: section  Up/Down: scroll  r: refresh  Esc: close";
        let help_y = py + panel_h - ch - 10;
        let help_trunc = crate::ui::trunc(help, max_chars);
        crate::ui::render_text(
            buffer,
            width,
            font,
            help_trunc,
            content_x,
            help_y,
            crate::ui::dim(theme.fg, 0.3),
        );
    }
}

impl GitInfo {
    fn collect() -> Option<Self> {
        let branch = run_git(&["branch", "--show-current"])?;

        let status_raw = run_git(&["status", "--porcelain"]).unwrap_or_default();
        let status: Vec<FileStatus> = status_raw
            .lines()
            .filter(|l| l.len() >= 3)
            .map(|l| {
                let first = l.chars().next().unwrap_or(' ');
                let second = l.chars().nth(1).unwrap_or(' ');
                let state = if second != ' ' { second } else { first };
                FileStatus {
                    state,
                    path: l[3..].to_string(),
                }
            })
            .collect();

        let log_raw = run_git(&["log", "--oneline", "--graph", "-30"]).unwrap_or_default();
        let log: Vec<CommitInfo> = log_raw
            .lines()
            .map(|l| {
                let graph_end = l.find(|c: char| c.is_alphanumeric()).unwrap_or(l.len());
                let graph = l[..graph_end].to_string();
                let rest = &l[graph_end..];
                let parts: Vec<&str> = rest.splitn(2, ' ').collect();
                CommitInfo {
                    hash: parts.first().unwrap_or(&"").to_string(),
                    message: parts.get(1).unwrap_or(&"").to_string(),
                    graph,
                }
            })
            .collect();

        let branches_raw = run_git(&["branch"]).unwrap_or_default();
        let branches: Vec<String> = branches_raw
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        let is_dirty = !status.is_empty();
        Some(GitInfo {
            branch,
            status,
            log,
            branches,
            is_dirty,
        })
    }
}

fn run_git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}
