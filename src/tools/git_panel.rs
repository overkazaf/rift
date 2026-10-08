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
        use crate::ui::kit::{Column, Ctx, ListItem, PanelSpec, Tokens, Tone, TableRow, Width};
        if !self.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(tk.backdrop);
        let Some(ref info) = self.info else {
            let rect = cx.centered_cols(48, cx.title_h() + cx.footer_h() + 2 * tk.sp.md + 4 * tk.row_h);
            let body = cx.panel(rect, &PanelSpec::new("Git").sub("not available").hints(&[("r", "refresh")]));
            cx.empty_state(body, "Not a git repository", "Run from inside a repo, then press r");
            return;
        };

        let rect = cx.centered_cols(64, height * 3 / 4);
        let title_sub = format!("on {}", info.branch);
        let (badge, tone) = if info.is_dirty { ("dirty", Tone::Warning) } else { ("clean", Tone::Success) };
        let spec = PanelSpec::new("Git")
            .sub(&title_sub)
            .badge(badge, tone)
            .hints(&[("Tab", "section"), ("Up/Down", "scroll"), ("r", "refresh"), ("Esc", "close")]);
        let body = cx.panel(rect, &spec);

        cx.tabs(body.x, body.y, body.w, &["Status", "Log", "Branches"], self.active_section);
        cx.divider(body.x, body.y + tk.row_h, body.w);
        let content = crate::ui::kit::Rect::new(body.x, body.y + tk.row_h + tk.sp.sm, body.w, body.h.saturating_sub(tk.row_h + tk.sp.sm));
        let vis_rows = cx.rows_fit(content.h);

        match self.active_section {
            0 => {
                if info.status.is_empty() {
                    cx.empty_state(content, "Clean working tree", "Nothing to commit");
                } else {
                    let rows: Vec<TableRow> = info
                        .status
                        .iter()
                        .map(|f| {
                            let t = match f.state {
                                'M' => Tone::Warning,
                                'A' => Tone::Success,
                                'D' => Tone::Danger,
                                '?' => Tone::Neutral,
                                _ => Tone::Accent,
                            };
                            TableRow::new(vec![f.state.to_string(), f.path.clone()]).tone(t)
                        })
                        .collect();
                    let scroll = self.scroll.min(rows.len().saturating_sub(vis_rows.saturating_sub(1)));
                    cx.table(content, &[Column::new("St", Width::Cols(2)), Column::new("Path", Width::Flex(1))], &rows, None, scroll);
                }
            }
            1 => {
                if info.log.is_empty() {
                    cx.empty_state(content, "No commits", "");
                } else {
                    let gw = info.log.iter().map(|c| c.graph.chars().count()).max().unwrap_or(0);
                    let rows: Vec<TableRow> = info
                        .log
                        .iter()
                        .map(|c| TableRow::new(vec![format!("{}{}", c.graph, c.hash), c.message.clone()]).tone(Tone::Warning))
                        .collect();
                    let scroll = self.scroll.min(rows.len().saturating_sub(vis_rows.saturating_sub(1)));
                    cx.table(content, &[Column::new("Graph / Hash", Width::Cols(gw + 8)), Column::new("Message", Width::Flex(1))], &rows, None, scroll);
                }
            }
            _ => {
                if info.branches.is_empty() {
                    cx.empty_state(content, "No branches", "");
                } else {
                    let labels: Vec<String> = info.branches.iter().map(|b| b.trim_start_matches('*').trim().to_string()).collect();
                    let items: Vec<ListItem> = info
                        .branches
                        .iter()
                        .zip(&labels)
                        .map(|(b, l)| {
                            if b.starts_with('*') { ListItem::new(l).meta("current").tone(Tone::Accent) } else { ListItem::new(l) }
                        })
                        .collect();
                    let scroll = self.scroll.min(items.len().saturating_sub(vis_rows));
                    cx.list(content, &items, None, scroll, None);
                }
            }
        }
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

#[cfg(test)]
mod qa_tests {
    use super::*;
    use crate::ui::kit::gallery::qa::each_theme;

    #[test]
    fn renders_all_sections_and_empty() {
        let info = || GitInfo {
            branch: "main".into(),
            status: vec![
                FileStatus { state: 'M', path: "src/main.rs".into() },
                FileStatus { state: 'A', path: "src/ui/kit/mod.rs".into() },
                FileStatus { state: '?', path: "notes.txt".into() },
            ],
            log: vec![CommitInfo { hash: "a1b2c3d".into(), message: "feat: ui kit".into(), graph: "* ".into() }],
            branches: vec!["* main".into(), "dev".into()],
            is_dirty: true,
        };
        for section in 0..3 {
            let g = GitPanel { visible: true, info: Some(info()), active_section: section, scroll: 0 };
            each_theme(&format!("git{section}"), |b, w, h, f, t| g.render(b, w, h, f, t));
        }
        let none = GitPanel { visible: true, info: None, active_section: 0, scroll: 0 };
        each_theme("git-none", |b, w, h, f, t| none.render(b, w, h, f, t));
    }
}
