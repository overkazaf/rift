use crate::config::{Rgb, Theme};
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
        if !self.visible {
            return;
        }

        let cw = font.cell_width;
        let ch = font.cell_height;
        let panel_h = (ch * 10 + 40).min(height / 3).max(ch * 6);
        let panel_y = height.saturating_sub(panel_h);

        // Dim the entire screen first (dark overlay)
        for px in buffer.iter_mut() {
            let r = ((*px >> 16) & 0xff) / 3;
            let g = ((*px >> 8) & 0xff) / 3;
            let b = (*px & 0xff) / 3;
            *px = (r << 16) | (g << 8) | b;
        }

        // Solid panel background
        let bg = darken(theme.bg, 15);
        let bg_px = pack(bg.0, bg.1, bg.2);
        for y in panel_y..height {
            let off = y * width;
            let end = (off + width).min(buffer.len());
            if off < buffer.len() {
                buffer[off..end].fill(bg_px);
            }
        }

        // Top separator
        let sep = lighten(bg, 20);
        let sep_px = pack(sep.0, sep.1, sep.2);
        if panel_y > 0 {
            let off = panel_y * width;
            let end = (off + width).min(buffer.len());
            if off < buffer.len() {
                buffer[off..end].fill(sep_px);
            }
        }

        let pad = 16;
        let max_chars = (width - pad * 2) / cw;
        let mut cy = panel_y + 10;

        // Title + provider info
        let title = if self.loading {
            "rift AI  [thinking...]"
        } else {
            "rift AI"
        };
        render_text(buffer, width, font, title, pad, cy, theme.cursor);
        cy += ch + 8;

        // Input line
        let input_bg = lighten(bg, 8);
        let input_bg_px = pack(input_bg.0, input_bg.1, input_bg.2);
        let input_h = ch + 8;
        for y in cy..(cy + input_h).min(height) {
            let off = y * width + pad;
            let end = (off + width - pad * 2).min(buffer.len());
            if off < buffer.len() {
                buffer[off..end].fill(input_bg_px);
            }
        }

        // Input prompt
        let prompt = "> ";
        render_text(buffer, width, font, prompt, pad + 4, cy + 4, theme.cursor);
        let input_x = pad + 4 + prompt.len() * cw;

        // Input text
        let display_input = trunc_str(&self.input, max_chars.saturating_sub(4));
        render_text(buffer, width, font, display_input, input_x, cy + 4, theme.fg);

        // Cursor
        if !self.loading {
            let cursor_x = input_x + display_input.chars().count() * cw;
            let cursor_px = pack(theme.cursor.0, theme.cursor.1, theme.cursor.2);
            for y in (cy + 4)..(cy + 4 + ch).min(height) {
                set_px(buffer, width, y, cursor_x, cursor_px);
                set_px(buffer, width, y, cursor_x + 1, cursor_px);
            }
        }
        cy += input_h + 8;

        // Response or error
        if let Some(ref err) = self.error {
            let err_color: Rgb = (240, 80, 80);
            let msg = format!("Error: {}", trunc_str(err, max_chars));
            render_text(buffer, width, font, &msg, pad, cy, err_color);
        } else if let Some(ref resp) = self.response {
            for (i, line) in resp.lines().enumerate() {
                if cy + ch >= height.saturating_sub(ch + 10) {
                    break;
                }
                let color = if i == 0 {
                    // First line = command, use accent
                    theme.cursor
                } else {
                    dim(theme.fg, 0.6)
                };
                let display_line = trunc_str(line, max_chars);
                render_text(buffer, width, font, display_line, pad, cy, color);
                cy += ch + 2;
            }
        }

        // Bottom help
        let help = if self.response.is_some() {
            "Enter: run cmd | Tab: paste cmd | Shift+Enter: new Q | Esc: close"
        } else if self.loading {
            "waiting for response..."
        } else {
            "Enter: ask | Esc: close"
        };
        let help_y = height.saturating_sub(ch + 8);
        render_text(
            buffer,
            width,
            font,
            trunc_str(help, max_chars),
            pad,
            help_y,
            dim(theme.fg, 0.3),
        );
    }
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

use crate::ui::{pack, darken, lighten, dim, render_text, set_px, trunc as trunc_str};
