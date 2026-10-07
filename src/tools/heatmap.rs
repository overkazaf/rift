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
        if !self.visible || self.data.is_empty() {
            return;
        }

        // Dim backdrop
        for px in buffer.iter_mut() {
            let r = ((*px >> 16) & 0xff) / 3;
            let g = ((*px >> 8) & 0xff) / 3;
            let b = (*px & 0xff) / 3;
            *px = (r << 16) | (g << 8) | b;
        }

        let cw = font.cell_width;
        let ch = font.cell_height;
        let cell_size = (cw * 2 / 3).max(6);
        let gap = 2;
        let weeks = (self.data.len() + 6) / 7;

        let pw = (weeks * (cell_size + gap) + 80).min(width.saturating_sub(40));
        let ph = (7 * (cell_size + gap) + ch * 3 + 40).min(height.saturating_sub(40));
        let px = (width.saturating_sub(pw)) / 2;
        let py = (height.saturating_sub(ph)) / 2;

        let bg = crate::ui::lighten(theme.bg, 6);
        crate::ui::fill_rect(buffer, width, px, py, pw, ph, crate::ui::pack_rgb(bg));
        let border = crate::ui::dim(theme.cursor, 0.4);
        crate::ui::draw_border(buffer, width, px, py, pw, ph, crate::ui::pack_rgb(border));

        // Title
        crate::ui::render_text(
            buffer, width, font, "Command Heatmap", px + 16, py + 10, theme.cursor,
        );

        // Stats
        let total: u32 = self.data.iter().map(|(_, c)| *c).sum();
        let info = format!("{} commands in {} days", total, self.data.len());
        crate::ui::render_text(
            buffer, width, font, &info, px + 16, py + 10 + ch + 4,
            crate::ui::dim(theme.fg, 0.5),
        );

        let grid_x = px + 40;
        let grid_y = py + ch * 2 + 30;

        // GitHub-style color scale
        let colors: [(u8, u8, u8); 5] = [
            crate::ui::darken(theme.bg, 5),
            crate::ui::dim(theme.cursor, 0.2),
            crate::ui::dim(theme.cursor, 0.4),
            crate::ui::dim(theme.cursor, 0.7),
            theme.cursor,
        ];

        for (i, (_, count)) in self.data.iter().enumerate() {
            let week = i / 7;
            let day = i % 7;
            let cx = grid_x + week * (cell_size + gap);
            let cy = grid_y + day * (cell_size + gap);
            if cx + cell_size >= px + pw || cy + cell_size >= py + ph {
                continue;
            }

            let intensity = if self.max_count == 0 {
                0
            } else {
                ((*count as f32 / self.max_count as f32) * 4.0).ceil() as usize
            };
            let color = colors[intensity.min(4)];
            crate::ui::fill_rect(
                buffer, width, cx, cy, cell_size, cell_size,
                crate::ui::pack_rgb(color),
            );
        }

        // Legend
        let legend_y = py + ph - ch - 10;
        crate::ui::render_text(
            buffer, width, font, "Less", px + 16, legend_y,
            crate::ui::dim(theme.fg, 0.4),
        );
        let lx = px + 16 + 5 * cw;
        for (i, color) in colors.iter().enumerate() {
            crate::ui::fill_rect(
                buffer, width,
                lx + i * (cell_size + 2), legend_y + 2,
                cell_size, cell_size,
                crate::ui::pack_rgb(*color),
            );
        }
        crate::ui::render_text(
            buffer, width, font, "More",
            lx + 5 * (cell_size + 2) + 4, legend_y,
            crate::ui::dim(theme.fg, 0.4),
        );

        // Help
        let help_x = pw.saturating_sub(12 * cw);
        crate::ui::render_text(
            buffer, width, font, "Esc: close",
            px + help_x, legend_y,
            crate::ui::dim(theme.fg, 0.3),
        );
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
