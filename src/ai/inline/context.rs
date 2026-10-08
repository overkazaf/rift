//! Cmd+K context resolution: what is "this" the user is asking about?
//!
//! Order: (a) active text selection, (b) selected block, (c) hovered block,
//! (d) last finished block of the active pane, (e) visible screen text.
//! The resolver is a pure function over a small snapshot model so the
//! priority rules are unit-testable without a window.

use crate::ai::hub::{AskRequest, ContextItem, Intent};

/// Owned snapshot of a command block.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockSnap {
    pub command: String,
    pub exit_code: Option<i32>,
    pub output: String,
    pub cwd: Option<String>,
    pub running: bool,
    /// Absolute line of the command (popover anchor).
    pub line: usize,
}

impl BlockSnap {
    pub fn failed(&self) -> bool {
        self.exit_code.is_some_and(|c| c != 0)
    }

    pub fn to_item(&self) -> ContextItem {
        ContextItem::Block {
            command: self.command.clone(),
            exit_code: self.exit_code,
            output: self.output.clone(),
            cwd: self.cwd.clone(),
            running: self.running,
        }
    }

    /// Short label such as `npm install (exit 1)`.
    pub fn label(&self) -> String {
        let cmd = if self.command.is_empty() { "(command)" } else { self.command.as_str() };
        format!(
            "{}{}",
            clip(cmd, 48),
            self.exit_code.map_or(String::new(), |c| format!(" (exit {c})")),
        )
    }
}

/// Everything the resolver may choose from.
#[derive(Clone, Debug, Default)]
pub struct ContextModel {
    /// Selected text and the absolute line where the selection ends.
    pub selection: Option<(String, usize)>,
    pub selected_block: Option<BlockSnap>,
    pub hovered_block: Option<BlockSnap>,
    pub last_block: Option<BlockSnap>,
    pub screen: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockSource {
    Selected,
    Hovered,
    Last,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Resolved {
    Selection { text: String, anchor: usize },
    Block { snap: BlockSnap, source: BlockSource },
    Screen(String),
    Nothing,
}

pub fn resolve(m: &ContextModel) -> Resolved {
    if let Some((text, anchor)) = &m.selection {
        if !text.trim().is_empty() {
            return Resolved::Selection { text: text.clone(), anchor: *anchor };
        }
    }
    let blocks = [
        (&m.selected_block, BlockSource::Selected),
        (&m.hovered_block, BlockSource::Hovered),
        (&m.last_block, BlockSource::Last),
    ];
    for (b, source) in blocks {
        if let Some(snap) = b {
            return Resolved::Block { snap: snap.clone(), source };
        }
    }
    if !m.screen.trim().is_empty() {
        return Resolved::Screen(m.screen.clone());
    }
    Resolved::Nothing
}

fn clip(s: &str, max: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= max {
        one
    } else {
        let mut t: String = one.chars().take(max.saturating_sub(1)).collect();
        t.push('\u{2026}');
        t
    }
}

impl Resolved {
    /// Absolute line the popover should sit near (None = near the prompt).
    pub fn anchor_line(&self) -> Option<usize> {
        match self {
            Resolved::Selection { anchor, .. } => Some(*anchor),
            Resolved::Block { snap, .. } => Some(snap.line),
            _ => None,
        }
    }

    pub fn has_context(&self) -> bool {
        !matches!(self, Resolved::Nothing)
    }

    /// Does the empty question mean "fix this"?
    pub fn is_failure(&self) -> bool {
        matches!(self, Resolved::Block { snap, .. } if snap.failed())
    }

    /// Caption for the popover header.
    pub fn label(&self) -> String {
        match self {
            Resolved::Selection { text, .. } => {
                let n = text.lines().count().max(1);
                format!("Selection \u{00B7} {n} line{}", if n == 1 { "" } else { "s" })
            }
            Resolved::Block { snap, source } => {
                let what = match source {
                    BlockSource::Selected => "Selected block",
                    BlockSource::Hovered => "Block",
                    BlockSource::Last => "Last command",
                };
                format!("{what}: {}", snap.label())
            }
            Resolved::Screen(_) => "Visible screen".to_string(),
            Resolved::Nothing => "No context".to_string(),
        }
    }

    /// Intent for `question`: an empty question on a failed block asks for a
    /// fix; everything else is an explanation.
    pub fn intent(&self, question: &str) -> Intent {
        if question.trim().is_empty() && self.is_failure() {
            Intent::Fix
        } else {
            Intent::Explain
        }
    }

    /// Build the hub request. `None` when there is neither context nor a
    /// question (nothing to send).
    pub fn to_request(&self, question: &str) -> Option<AskRequest> {
        let q = question.trim();
        if q.is_empty() && !self.has_context() {
            return None;
        }
        let mut req = AskRequest::new(q, self.intent(q));
        let subject = match self {
            Resolved::Selection { text, .. } => {
                req = req.with(ContextItem::Selection(text.clone()));
                clip(text, 60)
            }
            Resolved::Block { snap, .. } => {
                req = req.with(snap.to_item());
                snap.label()
            }
            Resolved::Screen(text) => {
                req = req.with(ContextItem::Screen(text.clone()));
                "visible screen".to_string()
            }
            Resolved::Nothing => String::new(),
        };
        let display = match (q.is_empty(), subject.is_empty()) {
            (true, false) => {
                let verb = if req.intent == Intent::Fix { "Fix" } else { "Explain" };
                format!("{verb}: {subject}")
            }
            (false, false) => format!("{q}  [{subject}]"),
            _ => q.to_string(),
        };
        Some(req.display(display))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(cmd: &str, exit: i32) -> BlockSnap {
        BlockSnap {
            command: cmd.into(),
            exit_code: Some(exit),
            output: "out".into(),
            cwd: Some("/w".into()),
            running: false,
            line: 7,
        }
    }

    fn full() -> ContextModel {
        ContextModel {
            selection: Some(("sel text".into(), 3)),
            selected_block: Some(snap("selected", 0)),
            hovered_block: Some(snap("hovered", 0)),
            last_block: Some(snap("last", 1)),
            screen: "screen".into(),
        }
    }

    #[test]
    fn selection_wins() {
        assert!(matches!(resolve(&full()), Resolved::Selection { anchor: 3, .. }));
    }

    #[test]
    fn blank_selection_falls_through_to_selected_block() {
        let mut m = full();
        m.selection = Some(("  \n ".into(), 0));
        match resolve(&m) {
            Resolved::Block { snap, source } => {
                assert_eq!(snap.command, "selected");
                assert_eq!(source, BlockSource::Selected);
            }
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn order_selected_hovered_last_screen_nothing() {
        let mut m = full();
        m.selection = None;
        let cmd = |r: Resolved| match r {
            Resolved::Block { snap, .. } => snap.command,
            Resolved::Screen(s) => s,
            Resolved::Nothing => "nothing".into(),
            Resolved::Selection { .. } => "selection".into(),
        };
        assert_eq!(cmd(resolve(&m)), "selected");
        m.selected_block = None;
        assert_eq!(cmd(resolve(&m)), "hovered");
        m.hovered_block = None;
        assert_eq!(cmd(resolve(&m)), "last");
        m.last_block = None;
        assert_eq!(cmd(resolve(&m)), "screen");
        m.screen = " \n ".into();
        assert_eq!(cmd(resolve(&m)), "nothing");
    }

    #[test]
    fn intent_follows_exit_code_and_question() {
        let failed = Resolved::Block { snap: snap("x", 2), source: BlockSource::Last };
        let ok = Resolved::Block { snap: snap("x", 0), source: BlockSource::Last };
        assert_eq!(failed.intent(""), Intent::Fix);
        assert_eq!(failed.intent("  "), Intent::Fix);
        assert_eq!(failed.intent("why?"), Intent::Explain);
        assert_eq!(ok.intent(""), Intent::Explain);
        assert_eq!(Resolved::Screen("s".into()).intent(""), Intent::Explain);
    }

    #[test]
    fn request_building() {
        let failed = Resolved::Block { snap: snap("npm i", 1), source: BlockSource::Last };
        let r = failed.to_request("").unwrap();
        assert_eq!(r.intent, Intent::Fix);
        assert_eq!(r.display, "Fix: npm i (exit 1)");
        assert_eq!(r.context.len(), 1);
        assert!(r.prompt().contains("exit code 1"));

        let r = failed.to_request(" why? ").unwrap();
        assert_eq!(r.question, "why?");
        assert!(r.display.starts_with("why?"));

        let sel = Resolved::Selection { text: "boom".into(), anchor: 0 };
        assert_eq!(sel.to_request("").unwrap().display, "Explain: boom");

        assert!(Resolved::Nothing.to_request("").is_none());
        let r = Resolved::Nothing.to_request("what is a pty").unwrap();
        assert!(r.context.is_empty());
        assert_eq!(r.display, "what is a pty");
    }

    #[test]
    fn labels_and_anchor() {
        let sel = Resolved::Selection { text: "a\nb\nc".into(), anchor: 9 };
        assert_eq!(sel.label(), "Selection \u{00B7} 3 lines");
        assert_eq!(sel.anchor_line(), Some(9));
        let b = Resolved::Block { snap: snap("ls", 0), source: BlockSource::Hovered };
        assert_eq!(b.label(), "Block: ls (exit 0)");
        assert_eq!(b.anchor_line(), Some(7));
        assert_eq!(Resolved::Screen("x".into()).anchor_line(), None);
    }
}
