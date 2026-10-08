use crate::config::Theme;
use crate::renderer::font::FontManager;

pub struct AiPanel {
    pub visible: bool,
    pub input: String,
    pub response: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
}

impl AiPanel {
    pub fn new() -> Self {
        Self {
            visible: false,
            input: String::new(),
            response: None,
            loading: false,
            error: None,
            rx: None,
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.input.clear();
            self.response = None;
            self.error = None;
            self.loading = false;
        }
    }

    pub fn handle_key(&mut self, key: AiPanelKey) -> Option<AiAction> {
        match key {
            AiPanelKey::Char(c) => {
                self.input.push(c);
                None
            }
            AiPanelKey::Backspace => {
                self.input.pop();
                None
            }
            AiPanelKey::Enter => {
                // If we have a response, Enter executes the command
                if let Some(ref resp) = self.response {
                    let cmd = resp.lines().next().unwrap_or("").trim().to_string();
                    if !cmd.is_empty() {
                        self.visible = false;
                        return Some(AiAction::Execute(cmd));
                    }
                }
                // Otherwise, Enter sends the question
                if !self.input.is_empty() && !self.loading {
                    self.loading = true;
                    self.response = None;
                    self.error = None;
                    Some(AiAction::Ask(self.input.clone()))
                } else {
                    None
                }
            }
            AiPanelKey::ShiftEnter => {
                // Shift+Enter always sends a new question (even if response exists)
                if !self.input.is_empty() && !self.loading {
                    self.loading = true;
                    self.response = None;
                    self.error = None;
                    Some(AiAction::Ask(self.input.clone()))
                } else {
                    None
                }
            }
            AiPanelKey::Escape => {
                self.visible = false;
                None
            }
            AiPanelKey::Tab => {
                // Tab copies command to terminal without executing
                if let Some(ref resp) = self.response {
                    let cmd = resp.lines().next().unwrap_or("").trim().to_string();
                    if !cmd.is_empty() {
                        self.visible = false;
                        return Some(AiAction::CopyToTerminal(cmd));
                    }
                }
                None
            }
        }
    }

    /// Poll for a completed AI response. Returns the suggested command
    /// (first line of a fresh, successful response) so callers can trigger
    /// follow-up work — e.g. an Advisor Mode safety review — exactly once
    /// per new response, without re-triggering on every subsequent poll.
    pub fn poll(&mut self) -> Option<String> {
        let mut fresh_cmd = None;
        if let Some(rx) = &self.rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok(text) => {
                        let cmd = text.lines().next().unwrap_or("").trim().to_string();
                        if !cmd.is_empty() {
                            fresh_cmd = Some(cmd);
                        }
                        self.response = Some(text);
                        self.loading = false;
                    }
                    Err(e) => {
                        self.error = Some(e);
                        self.loading = false;
                    }
                }
                self.rx = None;
            }
        }
        fresh_cmd
    }

    pub fn set_receiver(&mut self, rx: std::sync::mpsc::Receiver<Result<String, String>>) {
        self.rx = Some(rx);
        self.loading = true;
    }

    pub fn is_waiting(&self) -> bool {
        self.loading
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut FontManager,
        theme: &Theme,
    ) {
        use crate::ui::kit::{Ctx, PanelSpec, Rect, Tokens};
        if !self.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(tk.backdrop);

        let sheet = sheet_rect(&cx);
        let status = if self.loading {
            "thinking..."
        } else if self.error.is_some() {
            "request failed"
        } else if self.response.is_some() {
            "ready"
        } else {
            "natural language to shell command"
        };
        let hints: &[(&str, &str)] = if self.response.is_some() {
            &[("Enter", "run"), ("Tab", "paste"), ("Shift+Enter", "new question"), ("Esc", "close")]
        } else if self.loading {
            &[("Esc", "close")]
        } else {
            &[("Enter", "ask"), ("Esc", "close")]
        };
        let body = cx.panel(sheet, &PanelSpec::new("rift AI").sub(status).hints(hints));

        // Input
        let input = Rect::new(body.x, body.y, body.w, tk.input_h);
        cx.text_input(input, &self.input, self.input.chars().count(), None, "Ask anything, e.g. \"find files larger than 100MB\"", !self.loading);
        let mut y = body.y + tk.input_h + tk.sp.sm;
        let bottom = body.bottom();

        if self.loading {
            let elapsed = ui_clock().elapsed().as_secs_f32();
            let ty = cx.text_y(y, tk.row_h);
            crate::ui::render_spinner(cx.buf, cx.w, cx.font, body.x + tk.sp.xs, ty, "Thinking", tk.accent, tk.text_muted, elapsed);
            crate::ui::render_dots(cx.buf, cx.w, cx.font, body.x + tk.sp.xs + 11 * tk.cw, ty, tk.accent, elapsed);
            return;
        }
        if let Some(ref err) = self.error {
            cx.line_fit(body.x, y, body.w, &format!("Error: {}", err), tk.danger);
        } else if let Some(ref resp) = self.response {
            for (i, line) in resp.lines().enumerate() {
                if y + tk.row_h > bottom {
                    break;
                }
                if i == 0 {
                    // First line = the suggested command.
                    cx.line(body.x + tk.sp.xs, y, "$", tk.text_muted);
                    let cx0 = body.x + tk.sp.xs + 2 * tk.cw;
                    cx.line_fit(cx0, y, body.right().saturating_sub(cx0), line, tk.accent);
                } else {
                    cx.line_fit(body.x + tk.sp.xs, y, body.w, line, tk.text_muted);
                }
                y += tk.row_h;
            }
        }
    }
}

/// Monotonic clock for the in-panel spinner animation.
fn ui_clock() -> std::time::Instant {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *START.get_or_init(std::time::Instant::now)
}

/// Bottom-sheet rectangle of the AI panel. Fixed height (independent of the
/// response) so annotations such as the Advisor badge can align with it.
pub fn sheet_rect(cx: &crate::ui::kit::Ctx) -> crate::ui::kit::Rect {
    let tk = cx.tk;
    let rows = 8;
    let h = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + tk.input_h + tk.sp.sm + rows * tk.row_h;
    let h = h.min(cx.h * 55 / 100).max(cx.title_h() + cx.footer_h() + tk.input_h);
    let sheet = cx.bottom_sheet(h + tk.sp.sm);
    crate::ui::kit::Rect::new(sheet.x + tk.sp.sm, sheet.y, sheet.w.saturating_sub(2 * tk.sp.sm), h)
}

pub enum AiPanelKey {
    Char(char),
    Backspace,
    Enter,
    ShiftEnter,
    Escape,
    Tab,
}

pub enum AiAction {
    Ask(String),
    Execute(String),
    CopyToTerminal(String),
}

