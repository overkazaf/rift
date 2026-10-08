use crate::config::Theme;
use crate::renderer::font::FontManager;
use std::collections::HashMap;

pub struct Heatmap {
    pub visible: bool,
    data: Vec<(String, u32)>,
    max_count: u32,
}

impl Heatmap {
    pub fn new() -> Self {
        Self {
            visible: false,
            data: Vec::new(),
            max_count: 0,
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.load();
        }
    }

    fn load(&mut self) {
        self.data.clear();
        self.max_count = 0;

        let history_paths = [
            dirs::home_dir().map(|h| h.join(".zsh_history")),
            dirs::home_dir().map(|h| h.join(".bash_history")),
        ];

        let mut date_counts: HashMap<String, u32> = HashMap::new();

        for path in history_paths.iter().flatten() {
            if let Ok(content) = std::fs::read_to_string(path) {
                for line in content.lines() {
                    // zsh extended_history format: ": 1696581234:0;command"
                    if line.starts_with(": ") {
                        let rest = &line[2..];
                        if let Some(colon_pos) = rest.find(':') {
                            if let Ok(ts) = rest[..colon_pos].parse::<u64>() {
                                let date = timestamp_to_date(ts);
                                *date_counts.entry(date).or_insert(0) += 1;
                            }
                        }
                    }
                }
                break;
            }
        }

        let mut sorted: Vec<(String, u32)> = date_counts.into_iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        self.max_count = sorted.iter().map(|(_, c)| *c).max().unwrap_or(1);
        if sorted.len() > 365 {
            sorted = sorted[sorted.len() - 365..].to_vec();
        }
        self.data = sorted;
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut FontManager,
        theme: &Theme,
    ) {
        use crate::ui::kit::{mix, Ctx, PanelSpec, Rect, Tokens, Tone};
        if !self.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(tk.backdrop);

        let weeks = (self.data.len() + 6) / 7;
        let gap = 2 * tk.scale;
        let cell = 12 * tk.scale;
        let grid_w = weeks.max(8) * (cell + gap);
        let grid_h = 7 * (cell + gap);
        let want_h = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + grid_h + 3 * tk.row_h;
        let rect = cx.centered_px(grid_w.max(48 * tk.cw) + 2 * tk.sp.lg + 2, want_h);

        let total: u32 = self.data.iter().map(|(_, c)| *c).sum();
        let info = format!("{} commands in {} days", total, self.data.len());
        let spec = PanelSpec::new("Command Heatmap")
            .sub(if self.data.is_empty() { "" } else { &info })
            .hints(&[("Esc", "close")]);
        let body = cx.panel(rect, &spec);
        if self.data.is_empty() {
            cx.empty_state(body, "No history found", "Needs zsh extended history (~/.zsh_history)");
            return;
        }

        // 5-step intensity ramp from surface to accent.
        let ramp: [_; 5] = [
            tk.border,
            mix(tk.surface, tk.accent, 0.30),
            mix(tk.surface, tk.accent, 0.55),
            mix(tk.surface, tk.accent, 0.80),
            tk.accent,
        ];

        // Cells shrink to fit when the window is narrower than the grid.
        let cell = ((body.w.saturating_sub(weeks * gap)) / weeks.max(1)).min(cell).max(3);
        let grid_y = body.y + tk.row_h;
        cx.section(body.x, body.y, body.w, "Daily activity");
        for (i, (_, count)) in self.data.iter().enumerate() {
            let week = i / 7;
            let day = i % 7;
            let x = body.x + week * (cell + gap);
            let y = grid_y + day * (cell + gap);
            if x + cell > body.right() || y + cell > body.bottom() {
                continue;
            }
            let level = if self.max_count == 0 {
                0
            } else {
                ((*count as f32 / self.max_count as f32) * 4.0).ceil() as usize
            };
            cx.fill_rrect(Rect::new(x, y, cell, cell), tk.scale.max(1), ramp[level.min(4)]);
        }

        // Legend
        let ly = (grid_y + grid_h + tk.sp.sm).min(body.bottom().saturating_sub(tk.row_h));
        let mut x = body.x;
        cx.line(x, ly, "Less", tk.text_muted);
        x += cx.tw("Less") + tk.sp.sm;
        for c in ramp {
            cx.fill_rrect(Rect::new(x, ly + (tk.row_h - cell) / 2, cell, cell), tk.scale.max(1), c);
            x += cell + gap;
        }
        x += tk.sp.sm;
        cx.line(x, ly, "More", tk.text_muted);
        let busiest = format!("busiest day: {} commands", self.max_count);
        let bw = cx.tw(&busiest);
        if body.right() > x + cx.tw("More") + bw + tk.sp.lg {
            cx.badge_line(body.right() - cx.badge_w(&busiest), ly, &busiest, Tone::Neutral);
        }
    }

    pub fn handle_key(&mut self, key: HeatmapKey) {
        match key {
            HeatmapKey::Escape => self.visible = false,
        }
    }
}

pub enum HeatmapKey {
    Escape,
}

fn timestamp_to_date(ts: u64) -> String {
    let days = ts / 86400;
    let (y, m, d) = days_to_ymd(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let days = days + 719468;
    let era = days / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod qa_tests {
    use super::*;
    use crate::ui::kit::gallery::qa::each_theme;

    #[test]
    fn renders_grid_and_empty() {
        let mut hm = Heatmap::new();
        hm.visible = true;
        hm.data = (0..200).map(|i| (format!("d{i}"), (i * 7 % 23) as u32)).collect();
        hm.max_count = 22;
        each_theme("heatmap", |b, w, h, f, t| hm.render(b, w, h, f, t));
        hm.data.clear();
        each_theme("heatmap-empty", |b, w, h, f, t| hm.render(b, w, h, f, t));
    }
}
