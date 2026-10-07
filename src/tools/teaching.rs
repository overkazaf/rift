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
        _height: usize,
        font: &mut FontManager,
        theme: &Theme,
        cursor_x: usize,
        cursor_y: usize,
    ) {
        if !self.enabled {
            return;
        }
        let Some(ref text) = self.last_explanation else {
            return;
        };

        let cw = font.cell_width;
        let ch = font.cell_height;
        let lines: Vec<&str> = text.lines().take(6).collect();
        let max_line_len = lines.iter().map(|l| l.chars().count()).max().unwrap_or(20);

        let tip_w = ((max_line_len + 4) * cw).min(width / 2);
        let tip_h = lines.len() * (ch + 2) + 12;
        let tip_x = cursor_x.min(width.saturating_sub(tip_w + 10));
        let tip_y = if cursor_y > tip_h + 20 {
            cursor_y - tip_h - 10
        } else {
            cursor_y + ch + 10
        };

        let bg = crate::ui::darken(theme.bg, 5);
        crate::ui::fill_rect(buffer, width, tip_x, tip_y, tip_w, tip_h, crate::ui::pack_rgb(bg));
        let border = crate::ui::dim(theme.cursor, 0.4);
        crate::ui::draw_border(
            buffer,
            width,
            tip_x,
            tip_y,
            tip_w,
            tip_h,
            crate::ui::pack_rgb(border),
        );

        let mut ty = tip_y + 6;
        for (i, line) in lines.iter().enumerate() {
            let color = if i == 0 {
                theme.cursor
            } else {
                crate::ui::dim(theme.fg, 0.7)
            };
            let display = crate::ui::trunc(line, (tip_w.saturating_sub(16)) / cw.max(1));
            crate::ui::render_text(buffer, width, font, display, tip_x + 8, ty, color);
            ty += ch + 2;
        }
    }
}
