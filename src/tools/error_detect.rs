use crate::config::{Rgb, Theme};
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
        if !self.visible {
            return;
        }
        let Some(ref error) = self.error else {
            return;
        };

        let cw = font.cell_width;
        let ch = font.cell_height;

        let notif_w = (35 * cw).min(width / 2);
        let notif_h = ch * 6 + 16;
        let nx = width.saturating_sub(notif_w + 16);
        let ny = height.saturating_sub(notif_h + 16);

        // Dark red-tinted background
        let bg_px = crate::ui::pack(40, 15, 15);
        crate::ui::fill_rect(buffer, width, nx, ny, notif_w, notif_h, bg_px);

        // Red left accent border (3px)
        let red: Rgb = (243, 139, 168);
        let red_px = crate::ui::pack(red.0, red.1, red.2);
        for y in ny..ny + notif_h {
            for dx in 0..3 {
                crate::ui::set_px(buffer, width, y, nx + dx, red_px);
            }
        }

        let mut ty = ny + 8;
        let max_chars = (notif_w - 20) / cw;

        // Error type header
        let header = format!("! {}", error.error_type);
        crate::ui::render_text(
            buffer,
            width,
            font,
            crate::ui::trunc(&header, max_chars),
            nx + 12,
            ty,
            red,
        );
        ty += ch + 4;

        // Error message (truncated)
        crate::ui::render_text(
            buffer,
            width,
            font,
            crate::ui::trunc(&error.message, max_chars),
            nx + 12,
            ty,
            theme.fg,
        );
        ty += ch + 4;

        // Diagnosis (if available)
        if let Some(ref diag) = self.diagnosis {
            for line in diag.lines().take(3) {
                if ty + ch >= ny + notif_h - 4 {
                    break;
                }
                let green: Rgb = (166, 227, 161);
                crate::ui::render_text(
                    buffer,
                    width,
                    font,
                    crate::ui::trunc(line, max_chars),
                    nx + 12,
                    ty,
                    green,
                );
                ty += ch + 2;
            }
        } else {
            let dim_text: Rgb = (108, 112, 134);
            crate::ui::render_text(
                buffer,
                width,
                font,
                "analyzing...",
                nx + 12,
                ty,
                dim_text,
            );
        }
    }
}
