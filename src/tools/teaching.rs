use crate::config::Theme;
use crate::renderer::font::FontManager;

pub struct TeachingMode {
    pub enabled: bool,
    last_explanation: Option<String>,
    rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
}

impl TeachingMode {
    pub fn new() -> Self {
        Self {
            enabled: false,
            last_explanation: None,
            rx: None,
        }
    }

    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
        self.last_explanation = None;
        log::info!("Teaching mode: {}", if self.enabled { "ON" } else { "OFF" });
    }

    pub fn explain_prompt(command: &str) -> String {
        format!(
            "Explain this shell command in simple terms, one line per argument/flag:\n\
             Command: `{}`\n\
             Format: first line = what the command does overall, then each flag/argument on its own line.\n\
             Keep it concise (max 5 lines). Use plain language.",
            command
        )
    }

    pub fn set_receiver(&mut self, rx: std::sync::mpsc::Receiver<Result<String, String>>) {
        self.rx = Some(rx);
    }

    pub fn poll(&mut self) {
        if let Some(rx) = &self.rx {
            if let Ok(result) = rx.try_recv() {
                self.last_explanation = match result {
                    Ok(text) => Some(text),
                    Err(e) => Some(format!("(explanation failed: {})", e)),
                };
                self.rx = None;
            }
        }
    }

    pub fn clear(&mut self) {
        self.last_explanation = None;
    }

    pub fn has_explanation(&self) -> bool {
        self.last_explanation.is_some()
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut FontManager,
        theme: &Theme,
        cursor_x: usize,
        cursor_y: usize,
    ) {
        use crate::ui::kit::{Ctx, Rect, Tokens};
        if !self.enabled {
            return;
        }
        let Some(ref text) = self.last_explanation else {
            return;
        };
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);

        let lines: Vec<&str> = text.lines().take(6).collect();
        let max_line_len = lines.iter().map(|l| l.chars().count()).max().unwrap_or(20);

        let tip_w = ((max_line_len + 2) * tk.cw + 2 * tk.sp.md + 2 * tk.sp.sm).clamp(20 * tk.cw, (width / 2).max(20 * tk.cw)).min(width);
        let tip_h = lines.len().max(1) * tk.row_h + 2 * tk.sp.sm;
        let tip_x = cursor_x.min(width.saturating_sub(tip_w + tk.sp.md));
        // Prefer above the cursor line; fall back to below.
        let tip_y = if cursor_y > tip_h + tk.sp.xl {
            cursor_y - tip_h - tk.sp.sm
        } else {
            cursor_y + tk.ch + tk.sp.md
        };
        let rect = Rect::new(tip_x, tip_y.min(height.saturating_sub(tip_h)), tip_w, tip_h);
        let inner = cx.float(rect);
        for (i, line) in lines.iter().enumerate() {
            let c = if i == 0 { tk.accent } else { tk.text };
            cx.line_fit(inner.x + tk.sp.xs, inner.y + i * tk.row_h, inner.w.saturating_sub(2 * tk.sp.xs), line, c);
        }
    }
}

#[cfg(test)]
mod qa_tests {
    use super::*;
    use crate::ui::kit::gallery::qa::each_theme;

    #[test]
    fn renders_bubble() {
        let mut t = TeachingMode::new();
        t.enabled = true;
        t.last_explanation = Some("Lists files in long format.\n-l  use a long listing format\n-a  include hidden entries".into());
        each_theme("teaching", |b, w, h, f, th| t.render(b, w, h, f, th, 300, 400));
    }
}
