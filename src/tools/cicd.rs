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
        if !self.visible {
            return;
        }
        // Dim the background
        for px in buffer.iter_mut() {
            let r = ((*px >> 16) & 0xff) / 3;
            let g = ((*px >> 8) & 0xff) / 3;
            let b = (*px & 0xff) / 3;
            *px = (r << 16) | (g << 8) | b;
        }

        let cw = font.cell_width;
        let ch = font.cell_height;
        let pw = (45 * cw).min(width.saturating_sub(40));
        let item_h = ch + 4;
        let ph = ((self.runs.len() + 5) * item_h + 40).min(height.saturating_sub(40));
        let px = (width.saturating_sub(pw)) / 2;
        let py = (height.saturating_sub(ph)) / 2;

        let bg = crate::ui::lighten(theme.bg, 6);
        crate::ui::fill_rect(buffer, width, px, py, pw, ph, crate::ui::pack_rgb(bg));
        let border = crate::ui::dim(theme.cursor, 0.4);
        crate::ui::draw_border(buffer, width, px, py, pw, ph, crate::ui::pack_rgb(border));

        let mut ty = py + 12;
        crate::ui::render_text(buffer, width, font, "CI/CD Runs", px + 16, ty, theme.cursor);
        ty += ch + 10;

        if self.runs.is_empty() {
            crate::ui::render_text(
                buffer, width, font,
                "No CI runs found (needs gh or glab CLI)",
                px + 16, ty, crate::ui::dim(theme.fg, 0.5),
            );
        } else {
            for (i, run) in self.runs.iter().enumerate() {
                if ty + ch >= py + ph - 20 {
                    break;
                }
                let selected = i == self.selected;
                if selected {
                    crate::ui::fill_rect(
                        buffer, width,
                        px + 4, ty.saturating_sub(2), pw.saturating_sub(8), ch + 4,
                        crate::ui::pack_rgb(crate::ui::lighten(bg, 12)),
                    );
                }

                let (icon, icon_color) = match run.status {
                    CiStatus::Completed => ("v", (166u8, 227u8, 161u8)),
                    CiStatus::Failed => ("x", (243, 139, 168)),
                    CiStatus::InProgress => ("~", (249, 226, 175)),
                    CiStatus::Queued => (".", (108, 112, 134)),
                };
                crate::ui::render_text(buffer, width, font, icon, px + 16, ty, icon_color);

                let max_name = (pw / cw).saturating_sub(8);
                let name = crate::ui::trunc(&run.name, max_name);
                let tc = if selected {
                    theme.fg
                } else {
                    crate::ui::dim(theme.fg, 0.7)
                };
                crate::ui::render_text(buffer, width, font, name, px + 16 + 3 * cw, ty, tc);

                if !run.branch.is_empty() {
                    let bx = px + pw.saturating_sub((run.branch.len() + 2) * cw);
                    crate::ui::render_text(
                        buffer, width, font,
                        &run.branch, bx, ty,
                        crate::ui::dim(theme.fg, 0.3),
                    );
                }

                ty += item_h;
            }
        }

        let help = "r: refresh  Esc: close";
        let help_y = py + ph.saturating_sub(ch + 10);
        crate::ui::render_text(
            buffer, width, font,
            help, px + 16, help_y,
            crate::ui::dim(theme.fg, 0.3),
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
