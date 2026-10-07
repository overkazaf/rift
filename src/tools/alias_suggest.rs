use std::collections::HashMap;

#[derive(Clone)]
pub struct AliasSuggestion {
    pub command: String,
    pub suggested_alias: String,
    pub times_used: u32,
}

pub struct AliasSuggestor {
    patterns: HashMap<String, u32>,
    suggestions: Vec<AliasSuggestion>,
    threshold: u32,
}

impl AliasSuggestor {
    pub fn new() -> Self {
        Self { patterns: HashMap::new(), suggestions: Vec::new(), threshold: 5 }
    }

    pub fn track(&mut self, command: &str) {
        let cmd = command.trim().to_string();
        if cmd.len() < 5 || cmd.split_whitespace().count() < 2 { return; }
        *self.patterns.entry(cmd).or_insert(0) += 1;
    }

    pub fn check_suggestions(&mut self) -> Vec<AliasSuggestion> {
        self.suggestions.clear();
        for (cmd, count) in &self.patterns {
            if *count >= self.threshold {
                let alias = Self::generate_alias_name(cmd);
                self.suggestions.push(AliasSuggestion {
                    command: cmd.clone(),
                    suggested_alias: alias,
                    times_used: *count,
                });
            }
        }
        self.suggestions.sort_by(|a, b| b.times_used.cmp(&a.times_used));
        self.suggestions.clone()
    }

    fn generate_alias_name(cmd: &str) -> String {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if parts.len() <= 1 {
            return parts[0][..3.min(parts[0].len())].to_string();
        }
        parts.iter().map(|p| p.chars().next().unwrap_or('x')).collect::<String>()
    }

    pub fn render(
        &self, buffer: &mut [u32], width: usize, height: usize,
        font: &mut crate::renderer::font::FontManager, theme: &crate::config::Theme,
    ) {
        if self.suggestions.is_empty() { return; }
        let cw = font.cell_width;
        let ch = font.cell_height;
        let items = self.suggestions.len().min(3);
        let tip_w = (40 * cw).min(width / 2);
        let tip_h = (items + 2) * (ch + 4) + 16;
        let tx = width.saturating_sub(tip_w + 16);
        let ty = height.saturating_sub(tip_h + 16);

        let bg = crate::ui::darken(theme.bg, 8);
        crate::ui::fill_rect(buffer, width, tx, ty, tip_w, tip_h, crate::ui::pack_rgb(bg));
        crate::ui::draw_border(buffer, width, tx, ty, tip_w, tip_h, crate::ui::pack_rgb(crate::ui::dim(theme.cursor, 0.4)));

        let mut cy = ty + 8;
        crate::ui::render_text(buffer, width, font, "Alias suggestions", tx + 8, cy, theme.cursor);
        cy += ch + 6;

        for s in self.suggestions.iter().take(items) {
            let line = format!("alias {}='{}' ({}x)", s.suggested_alias, crate::ui::trunc(&s.command, 25), s.times_used);
            crate::ui::render_text(buffer, width, font, crate::ui::trunc(&line, (tip_w - 16) / cw), tx + 8, cy, crate::ui::dim(theme.fg, 0.7));
            cy += ch + 4;
        }
    }
}
