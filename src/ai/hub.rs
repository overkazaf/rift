//! Single entry point for every "ask the AI" action in the app.
//!
//! Callers (context menu, block toolbar, Cmd+K, palette `?`, inline error
//! fixes, `#` natural-language prompts) build an [`AskRequest`] and call
//! [`ask`]. The docked chat sidebar owns how requests are displayed and
//! answered (see [`crate::ai::chat`]); callers never touch it directly.

use crate::app::App;

/// A piece of terminal context attached to a question.
#[derive(Clone, Debug)]
pub enum ContextItem {
    /// Text the user selected on screen.
    Selection(String),
    /// A finished or running command block.
    Block {
        command: String,
        exit_code: Option<i32>,
        output: String,
        cwd: Option<String>,
        running: bool,
    },
    /// Arbitrary visible terminal text (e.g. the current screen).
    Screen(String),
}

/// What kind of answer the caller wants; lets the chat pick a system prompt
/// and decide whether to offer Run/Insert actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    /// Free-form question / explanation.
    Explain,
    /// Produce a fix for a failure (first line = command).
    Fix,
    /// Translate natural language into a shell command.
    Command,
}

#[derive(Clone, Debug)]
pub struct AskRequest {
    /// The user's question (may be empty when context alone is the question).
    pub question: String,
    /// Short label shown in the chat for this turn.
    pub display: String,
    pub context: Vec<ContextItem>,
    pub intent: Intent,
    /// Send immediately (true) or just prefill the composer (false).
    pub submit: bool,
}

impl AskRequest {
    pub fn new(question: impl Into<String>, intent: Intent) -> Self {
        let question = question.into();
        Self { display: question.clone(), question, context: Vec::new(), intent, submit: true }
    }

    pub fn with(mut self, item: ContextItem) -> Self {
        self.context.push(item);
        self
    }

    pub fn display(mut self, d: impl Into<String>) -> Self {
        self.display = d.into();
        self
    }

    pub fn prefill_only(mut self) -> Self {
        self.submit = false;
        self
    }

    /// Full prompt text: context blocks followed by the question.
    pub fn prompt(&self) -> String {
        const MAX_OUTPUT_LINES: usize = 80;
        let mut s = String::new();
        for item in &self.context {
            match item {
                ContextItem::Selection(t) => {
                    s.push_str("Selected terminal text:\n```\n");
                    s.push_str(t.trim_end());
                    s.push_str("\n```\n\n");
                }
                ContextItem::Screen(t) => {
                    s.push_str("Current terminal screen:\n```\n");
                    s.push_str(t.trim_end());
                    s.push_str("\n```\n\n");
                }
                ContextItem::Block { command, exit_code, output, cwd, running } => {
                    let lines: Vec<&str> = output.lines().collect();
                    let skip = lines.len().saturating_sub(MAX_OUTPUT_LINES);
                    let status = if *running {
                        "still running".to_string()
                    } else {
                        exit_code.map_or("unknown exit code".into(), |c| format!("exit code {c}"))
                    };
                    s.push_str(&format!("Command: `{command}`\nResult: {status}\n"));
                    if let Some(cwd) = cwd {
                        s.push_str(&format!("Working directory: {cwd}\n"));
                    }
                    if skip > 0 {
                        s.push_str(&format!("Output (last {MAX_OUTPUT_LINES} lines, {skip} omitted):\n"));
                    } else {
                        s.push_str("Output:\n");
                    }
                    s.push_str("```\n");
                    s.push_str(&lines[skip..].join("\n"));
                    s.push_str("\n```\n\n");
                }
            }
        }
        let q = self.question.trim();
        if q.is_empty() {
            s.push_str(match self.intent {
                Intent::Fix => "This failed. Give the single shell command that fixes it on the first line, then a short explanation.",
                Intent::Command => "Give the shell command for this on the first line, then a short explanation.",
                Intent::Explain => "Explain what this means and what I should do next.",
            });
        } else {
            s.push_str(q);
        }
        s
    }
}

/// Route a request to the docked chat sidebar: opens it, and either sends
/// the question right away (`submit`) or prefills the composer with the
/// question and attached context chips.
pub fn ask(app: &mut App, req: AskRequest) {
    crate::ai::chat::handle_request(app, req);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_includes_block_context_and_default_fix_question() {
        let req = AskRequest::new("", Intent::Fix).with(ContextItem::Block {
            command: "cargo build".into(),
            exit_code: Some(101),
            output: (0..100).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"),
            cwd: Some("/tmp".into()),
            running: false,
        });
        let p = req.prompt();
        assert!(p.contains("`cargo build`"));
        assert!(p.contains("exit code 101"));
        assert!(p.contains("/tmp"));
        assert!(p.contains("20 omitted"));
        assert!(p.contains("line 99") && !p.contains("line 19\n"));
        assert!(p.ends_with("short explanation."));
    }

    #[test]
    fn prompt_uses_user_question_when_given() {
        let p = AskRequest::new("why?", Intent::Explain).with(ContextItem::Selection("boom".into())).prompt();
        assert!(p.contains("boom"));
        assert!(p.trim_end().ends_with("why?"));
    }
}
