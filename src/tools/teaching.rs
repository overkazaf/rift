use crate::config::Theme;
use crate::renderer::font::FontManager;

pub struct TeachingMode {
    pub enabled: bool,
    /// Also request an explanation automatically whenever a command is
    /// submitted. Off by default: that request is sent while (not before) the
    /// command runs and uploads every command, so it is explicit opt-in
    /// (`RIFT_TEACHING_ON_SUBMIT=1`). The default is on demand, see
    /// [`TeachingMode::explain_now`].
    pub on_submit: bool,
    last_explanation: Option<String>,
    rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
}

impl TeachingMode {
    pub fn new() -> Self {
        Self {
            enabled: false,
            on_submit: std::env::var("RIFT_TEACHING_ON_SUBMIT").is_ok_and(|v| v == "1"),
            last_explanation: None,
            rx: None,
        }
    }

    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
        self.last_explanation = None;
        log::info!("Teaching mode: {}", if self.enabled { "ON" } else { "OFF" });
    }

    /// Prompt for explaining `command`; `None` when the command contains
    /// something that looks like a secret (it is never uploaded).
    pub fn prompt_for(command: &str) -> Option<String> {
        use crate::ai::chat::guard::{sanitize_line, wrap_untrusted, UNTRUSTED_NOTICE};
        let (clean, found) = sanitize_line(command, 2048);
        if found.redacted > 0 || clean.trim().is_empty() {
            return None;
        }
        Some(format!(
            "Explain this shell command in simple terms, one line per argument/flag.\n\
             {UNTRUSTED_NOTICE}\n\
             Command:\n{}\n\
             Format: first line = what the command does overall, then each flag/argument on its own line.\n\
             Keep it concise (max 5 lines). Use plain language.",
            wrap_untrusted("command", &clean)
        ))
    }

    /// Same prompt, secrets redacted instead of skipped (kept for callers
    /// that need a String); prefer [`prompt_for`](Self::prompt_for).
    pub fn explain_prompt(command: &str) -> String {
        let (redacted, _) = crate::tools::secret_mask::redact(command);
        Self::prompt_for(&redacted).unwrap_or_default()
    }

    /// Ask the model to explain `command` now (the on-demand path). Returns
    /// false when nothing was sent because the command looks like it holds a secret.
    pub fn explain_now(&mut self, command: &str, config: &crate::ai::LlmConfig) -> bool {
        let Some(prompt) = Self::prompt_for(command) else {
            self.last_explanation = Some("(not sent: the command looks like it contains a secret)".into());
            self.rx = None;
            return false;
        };
        let config = config.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::ai::backend::complete_simple(&config, &prompt));
            crate::wake::wake();
        });
        self.last_explanation = Some("explaining...".into());
        self.rx = Some(rx);
        true
    }

    /// Hook for "a command was just submitted": explains it only when
    /// [`on_submit`](Self::on_submit) is enabled.
    pub fn on_submit(&mut self, command: &str, config: &crate::ai::LlmConfig) {
        if self.enabled && self.on_submit {
            self.explain_now(command, config);
        }
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
mod prompt_tests {
    use super::*;

    #[test]
    fn explains_plain_commands_and_skips_secrets() {
        let p = TeachingMode::prompt_for("ls -la \x1b[31m/tmp").unwrap();
        assert!(p.contains("ls -la /tmp") && p.contains("untrusted") && !p.contains('\x1b'));
        assert!(TeachingMode::prompt_for("curl -H 'Authorization: Bearer sk-live-abcdef1234567890' x").is_none());
        assert!(TeachingMode::prompt_for("export API_TOKEN=abcdef123456").is_none());
        assert!(TeachingMode::prompt_for("   ").is_none());
        assert!(!TeachingMode::explain_prompt("mysql -p --password hunter2 db").contains("hunter2"));
    }

    #[test]
    fn submit_hook_is_off_by_default() {
        let mut t = TeachingMode::new();
        t.on_submit = false;
        t.enabled = true;
        t.on_submit("ls", &crate::ai::LlmConfig::default());
        assert!(!t.has_explanation());
        assert!(!t.explain_now("export TOKEN=abcdef123456", &crate::ai::LlmConfig::default()));
        assert!(t.has_explanation());
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
