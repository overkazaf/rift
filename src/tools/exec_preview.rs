//! Preview-Then-Accept — an interceptor for dangerous shell commands.
//!
//! When the user presses Enter on a command that looks destructive, the
//! keystroke is intercepted *before* it reaches the PTY (see
//! `app/shortcuts.rs`, step 7). Instead of executing immediately, a modal
//! preview is shown describing exactly what the command would do; the user
//! must explicitly confirm (or, for `Severity::Critical` commands, type
//! "yes") before the already-buffered command line is actually submitted.
//!
//! The command's characters have already been streamed to the PTY as the
//! user typed them (this is a real terminal, not a local-echo simulation),
//! so they're already sitting in the shell's line-editing buffer, uncommitted.
//! Confirming just sends the trailing `\r` to submit that buffer; canceling
//! sends nothing, leaving the line exactly as the user left it so they can
//! edit or clear it themselves.

mod analysis;
mod rules;
mod shell_parse;

pub use rules::{Flat, FlatArg, FlatCmd};

/// Every simple command `cmd` would run, resolved through wrappers and quoting
/// tricks by the same parser the safety rules use (see [`FlatCmd`]). Never
/// executes or expands anything.
pub fn flatten_commands(cmd: &str) -> Flat {
    rules::flatten(&shell_parse::parse(cmd.trim()))
}

/// A single consequence of running the previewed command, shown as a
/// bulleted line in the modal (`description`), optionally followed by a
/// dimmer detail line (`detail`) — e.g. a concrete path, file count, or
/// mitigation tip.
#[derive(Debug, Clone)]
pub struct Impact {
    pub description: String,
    pub detail: String,
}

impl Impact {
    fn new(description: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { description: description.into(), detail: detail.into() }
    }
    fn plain(description: impl Into<String>) -> Self {
        Self::new(description, String::new())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

/// State for the "Preview-Then-Accept" confirmation modal.
#[derive(Debug, Clone)]
pub struct ExecPreview {
    pub visible: bool,
    pub command: String,
    pub impacts: Vec<Impact>,
    pub severity: Severity,
    /// For `Severity::Critical`, the user must type "yes" before Enter is
    /// accepted. This buffers what they've typed so far.
    pub confirm_input: String,
}

pub enum ExecPreviewKey {
    Enter,
    Escape,
    Backspace,
    Char(char),
}

pub enum ExecPreviewAction {
    /// User confirmed — caller should submit the pending command (send `\r`).
    Execute,
    /// User backed out — caller should leave the PTY untouched.
    Cancel,
}

impl ExecPreview {
    pub fn hidden() -> Self {
        Self {
            visible: false,
            command: String::new(),
            impacts: Vec::new(),
            severity: Severity::Info,
            confirm_input: String::new(),
        }
    }

    /// Does `cmd` look dangerous enough to intercept? Returns a populated
    /// (but not-yet-visible) preview if so. Callers are expected to set
    /// `.visible = true` themselves before showing it.
    pub fn check_command(cmd: &str) -> Option<ExecPreview> {
        Self::check_command_in(cmd, None)
    }

    /// Like `check_command`, but live introspection (git, relative `rm`
    /// targets) runs in `cwd` — the shell's real directory as reported by
    /// OSC 7 — instead of Rift's own process directory.
    ///
    /// The command line is parsed as shell (see `shell_parse`), every simple
    /// command in it is evaluated, and impact analysis (file counts, git
    /// state) runs on worker threads under a shared ~300 ms budget, so this
    /// never blocks for long no matter how big the target tree is.
    /// `Severity::Info` results (routine build-dir deletes) are returned too;
    /// the Enter-key path uses [`ExecPreview::check_for_enter`] to skip them.
    pub fn check_command_in(cmd: &str, cwd: Option<&str>) -> Option<ExecPreview> {
        Self::run(cmd, cwd, std::env::var("HOME").ok().as_deref(), true)
    }

    /// Enter-key variant: no modal for `Info`-level commands, and the
    /// (cheap, analysis-free) classification runs first so routine commands
    /// never pay for a file scan.
    pub fn check_for_enter(cmd: &str, cwd: Option<&str>) -> Option<ExecPreview> {
        let home = std::env::var("HOME").ok();
        let quick = Self::run(cmd, cwd, home.as_deref(), false)?;
        if quick.severity == Severity::Info {
            return None;
        }
        Self::run(cmd, cwd, home.as_deref(), true).filter(|p| p.severity != Severity::Info)
    }

    fn run(cmd: &str, cwd: Option<&str>, home: Option<&str>, analyze: bool) -> Option<ExecPreview> {
        let trimmed = cmd.trim();
        if trimmed.is_empty() {
            return None;
        }
        let script = shell_parse::parse(trimmed);
        let env = rules::Env {
            cwd: cwd.map(std::path::PathBuf::from),
            home: home.filter(|h| h.starts_with('/')).map(|h| h.trim_end_matches('/').to_string()),
            budget: analysis::Budget::new(analyze),
        };
        let findings = rules::evaluate(&script, &env);
        let rank = |s: Severity| match s {
            Severity::Critical => 2,
            Severity::Warning => 1,
            Severity::Info => 0,
        };
        let top = findings.iter().map(|f| rank(f.severity)).max()?;
        let mut severity = Severity::Info;
        let mut impacts: Vec<Impact> = Vec::new();
        for f in findings.into_iter().filter(|f| rank(f.severity) == top) {
            severity = f.severity;
            for i in f.impacts {
                if !impacts.iter().any(|x| x.description == i.description && x.detail == i.detail) {
                    impacts.push(i);
                }
            }
        }
        Some(ExecPreview {
            visible: false,
            command: trimmed.to_string(),
            impacts: cap_impacts(impacts, 8),
            severity,
            confirm_input: String::new(),
        })
    }

    /// Does this severity require the user to type "yes" rather than a bare
    /// Enter/Y keypress?
    fn needs_typed_confirm(&self) -> bool {
        matches!(self.severity, Severity::Critical)
    }

    pub fn handle_key(&mut self, key: ExecPreviewKey) -> Option<ExecPreviewAction> {
        if !self.visible {
            return None;
        }
        let typed_confirm = self.needs_typed_confirm();
        match key {
            ExecPreviewKey::Escape => {
                *self = ExecPreview::hidden();
                Some(ExecPreviewAction::Cancel)
            }
            ExecPreviewKey::Enter => {
                if typed_confirm {
                    if self.confirm_input.trim().eq_ignore_ascii_case("yes") {
                        *self = ExecPreview::hidden();
                        Some(ExecPreviewAction::Execute)
                    } else {
                        None // Not confirmed yet — stay open.
                    }
                } else {
                    *self = ExecPreview::hidden();
                    Some(ExecPreviewAction::Execute)
                }
            }
            ExecPreviewKey::Backspace => {
                self.confirm_input.pop();
                None
            }
            ExecPreviewKey::Char(c) => {
                if typed_confirm {
                    if c.is_ascii_alphabetic() && self.confirm_input.chars().count() < 10 {
                        self.confirm_input.push(c.to_ascii_lowercase());
                    }
                    None
                } else {
                    match c {
                        'y' | 'Y' => {
                            *self = ExecPreview::hidden();
                            Some(ExecPreviewAction::Execute)
                        }
                        'n' | 'N' => {
                            *self = ExecPreview::hidden();
                            Some(ExecPreviewAction::Cancel)
                        }
                        _ => None,
                    }
                }
            }
        }
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut crate::renderer::font::FontManager,
        theme: &crate::config::Theme,
    ) {
        use crate::ui::kit::{Ctx, PanelSpec, Rect, Tokens, Tone};
        if !self.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        // Blocking, modal decision: dim everything behind it.
        cx.backdrop(0.7);

        let (tone, label) = match self.severity {
            Severity::Critical => (Tone::Danger, "CRITICAL"),
            Severity::Warning => (Tone::Warning, "WARNING"),
            Severity::Info => (Tone::Accent, "NOTICE"),
        };
        let typed_confirm = self.needs_typed_confirm();
        let accent = tk.tone(tone);

        let impact_lines: usize = self
            .impacts
            .iter()
            .map(|i| 1 + if i.detail.is_empty() { 0 } else { 1 })
            .sum::<usize>()
            .max(1);
        let typed_h = if typed_confirm { tk.row_h + tk.input_h + tk.sp.sm } else { 0 };
        let want_h = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + tk.input_h + tk.sp.md + impact_lines * tk.row_h + typed_h;
        let rect = cx.centered_cols(72, want_h);

        let hints: &[(&str, &str)] = if typed_confirm {
            &[("Enter", "execute (after typing yes)"), ("Esc", "cancel")]
        } else {
            &[("Enter/Y", "execute"), ("Esc/N", "cancel")]
        };
        let spec = PanelSpec::new("Confirm command")
            .sub("review before running")
            .badge(label, tone)
            .edge(tone)
            .hints(hints);
        let body = cx.panel(rect, &spec);

        // Command line, boxed
        let boxr = Rect::new(body.x, body.y, body.w, tk.input_h);
        cx.well(boxr);
        let ty = cx.text_y(boxr.y, boxr.h);
        cx.text(boxr.x + tk.sp.md, ty, "$", tk.text_muted);
        let cmd_x = boxr.x + tk.sp.md + 2 * tk.cw;
        cx.text_fit(cmd_x, ty, boxr.right().saturating_sub(cmd_x + tk.sp.md), &self.command, tk.text);

        // Impacts
        let mut y = body.y + tk.input_h + tk.sp.md;
        let impacts_bottom = body.bottom().saturating_sub(typed_h);
        let marker = if matches!(self.severity, Severity::Critical) { "!!" } else { "!" };
        for impact in &self.impacts {
            let need = if impact.detail.is_empty() { tk.row_h } else { 2 * tk.row_h };
            if y + need > impacts_bottom {
                break;
            }
            cx.line(body.x + tk.sp.xs, y, marker, accent);
            let dx = body.x + tk.sp.xs + 3 * tk.cw;
            cx.line_fit(dx, y, body.right().saturating_sub(dx), &impact.description, tk.text);
            y += tk.row_h;
            if !impact.detail.is_empty() {
                cx.line_fit(dx, y, body.right().saturating_sub(dx), &impact.detail, tk.text_muted);
                y += tk.row_h;
            }
        }

        // Extra safety for Critical: require the literal word "yes".
        if typed_confirm {
            let y = body.bottom().saturating_sub(typed_h) + tk.sp.xs;
            cx.line(body.x, y, "Type \"yes\" to confirm", accent);
            let inp = Rect::new(body.x, y + tk.row_h, body.w, tk.input_h);
            cx.text_input(inp, &self.confirm_input, self.confirm_input.chars().count(), None, "yes", true);
        }
    }
}


fn cap_impacts(mut impacts: Vec<Impact>, max: usize) -> Vec<Impact> {
    if impacts.len() > max {
        let remaining = impacts.len() - max;
        impacts.truncate(max);
        impacts.push(Impact::plain(format!("...and {remaining} more")));
    }
    impacts
}

#[cfg(test)]
mod tests;
