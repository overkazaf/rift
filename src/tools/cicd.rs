#[allow(dead_code)]

pub struct CicdPanel {
    pub visible: bool,
    runs: Vec<CiRun>,
    selected: usize,
    scroll: usize,
    last_fetch: std::time::Instant,
}

struct CiRun {
    name: String,
    branch: String,
    status: CiStatus,
    conclusion: String,
    #[allow(dead_code)]
    duration: String,
    #[allow(dead_code)]
    started_at: String,
    #[allow(dead_code)]
    url: String,
}

#[derive(PartialEq)]
enum CiStatus {
    InProgress,
    Completed,
    Failed,
    Queued,
}

impl CicdPanel {
    pub fn new() -> Self {
        Self {
            visible: false,
            runs: Vec::new(),
            selected: 0,
            scroll: 0,
            last_fetch: std::time::Instant::now() - std::time::Duration::from_secs(999),
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.fetch();
        }
    }

    pub fn fetch(&mut self) {
        self.runs.clear();
        if let Some(runs) = Self::fetch_github() {
            self.runs = runs;
        } else if let Some(runs) = Self::fetch_gitlab() {
            self.runs = runs;
        }
        self.last_fetch = std::time::Instant::now();
    }

    fn fetch_github() -> Option<Vec<CiRun>> {
        let output = std::process::Command::new("gh")
            .args([
                "run", "list", "--limit", "10", "--json",
                "name,headBranch,status,conclusion,createdAt,databaseId",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Self::parse_gh_runs(&text)
    }

    fn fetch_gitlab() -> Option<Vec<CiRun>> {
        let output = std::process::Command::new("glab")
            .args(["ci", "list", "--per-page", "10"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut runs = Vec::new();
        for line in text.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() >= 3 {
                runs.push(CiRun {
                    name: cols.get(1).unwrap_or(&"").to_string(),
                    branch: String::new(),
                    status: if cols.get(2) == Some(&"passed") {
                        CiStatus::Completed
                    } else if cols.get(2) == Some(&"running") {
                        CiStatus::InProgress
                    } else {
                        CiStatus::Failed
                    },
                    conclusion: cols.get(2).unwrap_or(&"").to_string(),
                    duration: String::new(),
                    started_at: String::new(),
                    url: String::new(),
                });
            }
        }
        if runs.is_empty() { None } else { Some(runs) }
    }

    fn parse_gh_runs(json: &str) -> Option<Vec<CiRun>> {
        if !json.trim().starts_with('[') {
            return None;
        }
        let mut runs = Vec::new();
        for obj_str in json.split("},{") {
            let name = extract_json_str(obj_str, "name").unwrap_or_default();
            let branch = extract_json_str(obj_str, "headBranch").unwrap_or_default();
            let status_str = extract_json_str(obj_str, "status").unwrap_or_default();
            let conclusion = extract_json_str(obj_str, "conclusion").unwrap_or_default();

            let status = match status_str.as_str() {
                "in_progress" => CiStatus::InProgress,
                "queued" => CiStatus::Queued,
                "completed" if conclusion == "failure" => CiStatus::Failed,
                _ => CiStatus::Completed,
            };

            if !name.is_empty() {
                runs.push(CiRun {
                    name,
                    branch,
                    status,
                    conclusion,
                    duration: String::new(),
                    started_at: String::new(),
                    url: String::new(),
                });
            }
        }
        if runs.is_empty() { None } else { Some(runs) }
    }

    pub fn handle_key(&mut self, key: CicdKey) {
        match key {
            CicdKey::Escape => self.visible = false,
            CicdKey::Up => self.selected = self.selected.saturating_sub(1),
            CicdKey::Down => {
                if self.selected + 1 < self.runs.len() {
                    self.selected += 1;
                }
            }
            CicdKey::Char('r') => self.fetch(),
            CicdKey::Char(_) => {}
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
        use crate::ui::kit::{center_scroll, Column, Ctx, PanelSpec, TableRow, Tokens, Tone, Width};
        if !self.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(tk.backdrop);

        let want = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + (self.runs.len().max(3) + 1) * tk.row_h;
        let rect = cx.centered_cols(64, want.min(height * 3 / 4));
        let count = format!("{} runs", self.runs.len());
        let spec = PanelSpec::new("CI/CD")
            .sub("recent pipeline runs")
            .badge(&count, Tone::Neutral)
            .hints(&[("Up/Down", "move"), ("r", "refresh"), ("Esc", "close")]);
        let body = cx.panel(rect, &spec);

        if self.runs.is_empty() {
            cx.empty_state(body, "No CI runs found", "Needs the gh or glab CLI");
            return;
        }
        let rows: Vec<TableRow> = self
            .runs
            .iter()
            .map(|run| {
                let (label, tone) = match run.status {
                    CiStatus::Completed => ("passed", Tone::Success),
                    CiStatus::Failed => ("failed", Tone::Danger),
                    CiStatus::InProgress => ("running", Tone::Warning),
                    CiStatus::Queued => ("queued", Tone::Neutral),
                };
                TableRow::new(vec![label.to_string(), run.name.clone(), run.branch.clone()]).tone(tone)
            })
            .collect();
        let vis = cx.rows_fit(body.h.saturating_sub(tk.row_h));
        let scroll = center_scroll(self.selected, rows.len(), vis);
        cx.table(
            body,
            &[Column::new("Status", Width::Cols(8)), Column::new("Workflow", Width::Flex(2)), Column::new("Branch", Width::Flex(1))],
            &rows,
            Some(self.selected),
            scroll,
        );
    }
}

pub enum CicdKey {
    Up,
    Down,
    Escape,
    Char(char),
}

fn extract_json_str(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{}\":\"", key);
    let start = json.find(&pat)? + pat.len();
    let rest = &json[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod qa_tests {
    use super::*;
    use crate::ui::kit::gallery::qa::each_theme;

    #[test]
    fn renders_runs_and_empty() {
        let mut c = CicdPanel::new();
        c.visible = true;
        let run = |n: &str, s: CiStatus| CiRun { name: n.into(), branch: "main".into(), status: s, conclusion: String::new(), duration: String::new(), started_at: String::new(), url: String::new() };
        c.runs = vec![run("build", CiStatus::Completed), run("test-suite-with-long-name", CiStatus::Failed), run("deploy", CiStatus::InProgress), run("lint", CiStatus::Queued)];
        c.selected = 1;
        each_theme("cicd", |b, w, h, f, t| c.render(b, w, h, f, t));
        c.runs.clear();
        each_theme("cicd-empty", |b, w, h, f, t| c.render(b, w, h, f, t));
    }
}
