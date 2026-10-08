use crate::config::Theme;
use crate::renderer::font::FontManager;

pub struct ErrorDetector {
    patterns: Vec<ErrorPattern>,
}

struct ErrorPattern {
    name: &'static str,
    detector: fn(&str) -> bool,
    context_lines: usize,
}

pub struct DetectedError {
    pub error_type: String,
    pub message: String,
    pub context: Vec<String>,
    pub line_number: usize,
}

impl ErrorDetector {
    pub fn new() -> Self {
        Self {
            patterns: vec![
                ErrorPattern {
                    name: "python_traceback",
                    detector: |line| {
                        line.starts_with("Traceback (most recent call last)")
                            || (line.starts_with("  File \"") && line.contains(", line "))
                    },
                    context_lines: 20,
                },
                ErrorPattern {
                    name: "rust_panic",
                    detector: |line| {
                        (line.contains("thread '") && line.contains("panicked at"))
                            || line.starts_with("stack backtrace:")
                    },
                    context_lines: 15,
                },
                ErrorPattern {
                    name: "node_error",
                    detector: |line| {
                        (line.contains("Error:") && (line.contains("at ") || line.contains("node:")))
                            || line.starts_with("    at ")
                    },
                    context_lines: 15,
                },
                ErrorPattern {
                    name: "command_not_found",
                    detector: |line| {
                        line.contains("command not found") || line.contains("not found: ")
                    },
                    context_lines: 3,
                },
                ErrorPattern {
                    name: "permission_denied",
                    detector: |line| {
                        line.contains("Permission denied") || line.contains("permission denied")
                    },
                    context_lines: 3,
                },
                ErrorPattern {
                    name: "compilation_error",
                    detector: |line| {
                        line.contains("error[E") || (line.contains("error:") && line.contains("-->"))
                    },
                    context_lines: 10,
                },
                ErrorPattern {
                    name: "no_such_file",
                    detector: |line| line.contains("No such file or directory"),
                    context_lines: 3,
                },
                ErrorPattern {
                    name: "connection_refused",
                    detector: |line| {
                        line.contains("Connection refused") || line.contains("connection timed out")
                    },
                    context_lines: 5,
                },
                ErrorPattern {
                    name: "segfault",
                    detector: |line| {
                        line.contains("Segmentation fault") || line.contains("SIGSEGV")
                    },
                    context_lines: 10,
                },
                ErrorPattern {
                    name: "oom",
                    detector: |line| {
                        line.contains("out of memory") || line.contains("Cannot allocate memory")
                    },
                    context_lines: 5,
                },
            ],
        }
    }

    pub fn matches_any(&self, line: &str) -> bool {
        self.patterns.iter().any(|p| (p.detector)(line))
    }

    pub fn check_line(
        &self,
        line: &str,
        line_number: usize,
        recent_lines: &[String],
    ) -> Option<DetectedError> {
        for pattern in &self.patterns {
            if (pattern.detector)(line) {
                let start = line_number.saturating_sub(pattern.context_lines);
                let context: Vec<String> = recent_lines
                    .iter()
                    .skip(start)
                    .take(pattern.context_lines * 2)
                    .cloned()
                    .collect();

                return Some(DetectedError {
                    error_type: pattern.name.to_string(),
                    message: line.trim().to_string(),
                    context,
                    line_number,
                });
            }
        }
        None
    }

    pub fn build_diagnosis_prompt(error: &DetectedError) -> String {
        let context_text = error.context.join("\n");
        format!(
            "Diagnose this terminal error and suggest a fix.\n\
             Error type: {}\n\
             Error message: {}\n\
             Context:\n```\n{}\n```\n\
             Respond with:\n\
             1. What went wrong (one sentence)\n\
             2. How to fix it (specific command or steps)\n\
             3. Prevention tip (if applicable)",
            error.error_type, error.message, context_text
        )
    }
}

pub struct ErrorNotification {
    pub visible: bool,
    pub error: Option<DetectedError>,
    pub diagnosis: Option<String>,
    pub show_until: std::time::Instant,
}

impl ErrorNotification {
    pub fn new() -> Self {
        Self {
            visible: false,
            error: None,
            diagnosis: None,
            show_until: std::time::Instant::now(),
        }
    }

    pub fn show(&mut self, error: DetectedError, duration_secs: u64) {
        self.error = Some(error);
        self.visible = true;
        self.show_until =
            std::time::Instant::now() + std::time::Duration::from_secs(duration_secs);
    }

    pub fn set_diagnosis(&mut self, text: String) {
        self.diagnosis = Some(text);
    }

    pub fn tick(&mut self) {
        if self.visible && std::time::Instant::now() >= self.show_until {
            self.visible = false;
        }
    }

    pub fn dismiss(&mut self) {
        self.visible = false;
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut FontManager,
        theme: &Theme,
    ) {
        use crate::ui::kit::{Ctx, Rect, Tokens, Tone};
        if !self.visible {
            return;
        }
        let Some(ref error) = self.error else {
            return;
        };
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);

        let diag_lines: Vec<&str> = self.diagnosis.as_deref().map(|d| d.lines().take(3).collect()).unwrap_or_default();
        let body_rows = 2 + diag_lines.len().max(1);
        let w = (46 * tk.cw + 2 * tk.sp.lg).min(width.saturating_sub(2 * tk.sp.lg));
        let h = body_rows * tk.row_h + 2 * tk.sp.sm + 2 * tk.sp.xs;
        let rect = Rect::new(
            width.saturating_sub(w + tk.sp.lg),
            height.saturating_sub(h + tk.sp.lg),
            w,
            h,
        );
        let inner = cx.float(rect);
        // Danger accent bar down the left edge.
        cx.fill_rrect(Rect::new(rect.x + tk.sp.sm, rect.y + tk.sp.sm, 3 * tk.scale, rect.h - 2 * tk.sp.sm), tk.scale + 1, tk.danger);
        let x = inner.x + 3 * tk.scale + tk.sp.sm;
        let avail = inner.right().saturating_sub(x);
        let mut y = inner.y + tk.sp.xs;

        // Error type badge
        cx.badge_line(x, y, &error.error_type, Tone::Danger);
        y += tk.row_h;
        cx.line_fit(x, y, avail, &error.message, tk.text);
        y += tk.row_h;
        if self.diagnosis.is_some() {
            for line in &diag_lines {
                cx.line_fit(x, y, avail, line, tk.success);
                y += tk.row_h;
            }
        } else {
            cx.line_fit(x, y, avail, "analyzing...", tk.text_muted);
        }
    }
}
