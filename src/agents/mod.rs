//! Agent Mission Control: supervise AI coding agents (Claude Code, Codex CLI,
//! Gemini CLI, opencode, Aider, Cursor CLI) running in Rift panes.
//!
//! # Public API (stable; the change-review module `crate::review` builds on it)
//!
//! * [`AgentRegistry`] lives on `App` as `app.agents`.
//!   * [`AgentRegistry::sessions`] -> `&[AgentSession]`: one entry per pane that
//!     currently runs (or recently ran) an agent, across **all** tabs.
//!   * [`AgentRegistry::session`] looks one up by `pane_uid`.
//!   * [`AgentRegistry::drain_events`] -> `Vec<AgentEvent>`: lifecycle events since
//!     the last call. Every event is delivered once to whoever calls this; the
//!     mission-control UI keeps its own private tap, so draining never starves it.
//!   * [`AgentRegistry::attention_count`] / [`AgentRegistry::next_attention`].
//! * [`AgentSession`]: `pane_uid` (the stable `Pane::id`, never a tab/leaf index),
//!   `tab_index` (current position, may shift when tabs move), `kind`, `state`,
//!   `title`, `started_at`, `last_activity`, `cwd`, `git_root`, `needs_attention`,
//!   `turn_counter`, `last_turn_started_at`.
//! * [`AgentEvent`]: `TurnStarted`, `NeedsUser`, `TurnFinished`, `Exited`. Each
//!   carries `pane_uid` plus the session's `cwd` / `git_root` at that moment, which
//!   is all a reviewer needs to diff the working tree: snapshot on `TurnStarted`,
//!   compare on `TurnFinished`.
//!
//! A *turn* is one prompt-to-answer cycle: it starts when the user submits input
//! and the agent starts producing output (or a `working` hook arrives), and ends
//! when the output goes quiet, the agent reports `done`, or the agent exits.
//! Answering an approval prompt continues the same turn.
//!
//! # How state is derived (see `state.rs`)
//!
//! * Detection (`detect.rs`): OSC 633;E / block command text, then the PTY's
//!   foreground process (tcgetpgrp -> proc_pidpath / /proc), then the window title.
//! * Signals: OSC 9 / OSC 777 notifications, `rift agent-event` hooks (over the MCP
//!   socket, see `cli.rs`), output activity, screen heuristics for approval prompts
//!   (one pattern table, `state::APPROVAL_PATTERNS`), and block end / process exit.
//! * Everything time dependent takes an explicit `Instant`, so state transitions
//!   are tested with synthetic timelines.
//!
//! # Control console (the dock acts on agents, not just lists them)
//!
//! * `prompt.rs`: approval prompts parsed off a pane's screen (what is asked, the
//!   options with their roles), the option -> keystroke plan and the byte
//!   encoders (`crate::input`) for answers, ESC, Ctrl+C and replies.
//! * `metrics.rs`: model / tokens / cost / context / reset parsed from each
//!   agent's own UI, and same-worktree collision detection.
//! * `control.rs`: the dock's pure keyboard state machine (browse, answer with
//!   a second press for critical commands, reply, broadcast targeting,
//!   confirmations, context menu) and the per-agent `PaneInfo`.
//! * `console.rs`: glue to `App`: keeps `PaneInfo` fresh (screen scans,
//!   background command-risk checks, review timeline), carries out answers /
//!   interrupts / replies / restart / close, and handles the dock's mouse.
//!
//! The rest of the module is UI and glue: `runtime.rs` (polling `App`),
//! `ui.rs` + `dock.rs` (dock state, layout and drawing, tab badges, pane tint),
//! `notify.rs` (desktop notifications, dock badge), `launch.rs` ("New Agent",
//! worktrees, grid layouts).

pub mod cli;
pub mod console;
pub mod control;
pub mod detect;
pub mod dock;
pub mod git;
pub mod inbox;
pub mod launch;
pub mod metrics;
pub mod notify;
pub mod prompt;
pub mod registry;
pub mod runtime;
pub mod state;
pub mod ui;

use std::time::Instant;

pub use registry::AgentRegistry;

/// Which coding agent a session runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentKind {
    ClaudeCode,
    Codex,
    Gemini,
    OpenCode,
    Aider,
    CursorAgent,
}

impl AgentKind {
    pub const ALL: [AgentKind; 6] = [
        AgentKind::ClaudeCode,
        AgentKind::Codex,
        AgentKind::Gemini,
        AgentKind::OpenCode,
        AgentKind::Aider,
        AgentKind::CursorAgent,
    ];

    /// Product name, e.g. "Claude Code".
    pub fn name(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "Claude Code",
            AgentKind::Codex => "Codex",
            AgentKind::Gemini => "Gemini CLI",
            AgentKind::OpenCode => "opencode",
            AgentKind::Aider => "Aider",
            AgentKind::CursorAgent => "Cursor CLI",
        }
    }

    /// Short lowercase slug used in branch names, tab titles and `--agent`.
    pub fn slug(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Gemini => "gemini",
            AgentKind::OpenCode => "opencode",
            AgentKind::Aider => "aider",
            AgentKind::CursorAgent => "cursor",
        }
    }

    /// Executable launched by "New Agent".
    pub fn binary(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Gemini => "gemini",
            AgentKind::OpenCode => "opencode",
            AgentKind::Aider => "aider",
            AgentKind::CursorAgent => "cursor-agent",
        }
    }

    /// One-letter glyph for the sidebar icon (ASCII: every font has it).
    pub fn glyph(self) -> char {
        match self {
            AgentKind::ClaudeCode => 'C',
            AgentKind::Codex => 'X',
            AgentKind::Gemini => 'G',
            AgentKind::OpenCode => 'O',
            AgentKind::Aider => 'A',
            AgentKind::CursorAgent => 'U',
        }
    }

    /// Brand-ish colour of the icon chip.
    pub fn color(self) -> (u8, u8, u8) {
        match self {
            AgentKind::ClaudeCode => (217, 119, 87),
            AgentKind::Codex => (16, 163, 127),
            AgentKind::Gemini => (66, 133, 244),
            AgentKind::OpenCode => (180, 180, 190),
            AgentKind::Aider => (84, 190, 100),
            AgentKind::CursorAgent => (130, 140, 255),
        }
    }

    /// Parse a slug / name / binary (`claude`, `Claude Code`, `cursor-agent`, ...).
    pub fn parse(s: &str) -> Option<AgentKind> {
        let s = s.trim().to_ascii_lowercase();
        AgentKind::ALL.into_iter().find(|k| {
            s == k.slug() || s == k.binary() || s == k.name().to_ascii_lowercase()
        }).or(match s.as_str() {
            "claude-code" | "claudecode" | "claude_code" => Some(AgentKind::ClaudeCode),
            "gemini-cli" | "gemini_cli" => Some(AgentKind::Gemini),
            "cursor-cli" | "cursor_agent" => Some(AgentKind::CursorAgent),
            _ => None,
        })
    }
}

/// Where a session is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    /// Just detected; the agent is drawing its UI.
    Starting,
    /// A turn is running (output is flowing).
    Working,
    /// Blocked on you: an approval prompt or a question.
    WaitingForUser,
    /// Ready for the next prompt.
    Idle,
    /// The agent process ended normally (exit code when known).
    Done { exit: Option<i32> },
    /// The agent ended with a failure (non-zero exit) or reported an error.
    Error,
}

impl AgentState {
    /// Still a live agent (not Done / Error).
    pub fn is_live(self) -> bool {
        !matches!(self, AgentState::Done { .. } | AgentState::Error)
    }
}

/// How the agent in a pane was recognised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetectSource {
    /// Command text (OSC 633;E / block).
    Command,
    /// Foreground process of the PTY.
    Process,
    /// Window title (OSC 0 / 2).
    Title,
    /// A `rift agent-event` hook arrived first.
    Hook,
}

/// One supervised agent.
#[derive(Clone, Debug)]
pub struct AgentSession {
    /// Stable id of the pane (`Pane::id`). Unlike a tab / leaf index it survives
    /// tab reordering, closing other tabs and splitting.
    pub pane_uid: usize,
    /// Current index of the tab holding the pane (refreshed every poll).
    pub tab_index: usize,
    pub kind: AgentKind,
    pub state: AgentState,
    /// Display title, e.g. "Claude Code".
    pub title: String,
    pub started_at: Instant,
    /// Last time the pane produced agent output (not typing echo).
    pub last_activity: Instant,
    pub cwd: Option<String>,
    /// Root of the git work tree containing `cwd`, if any.
    pub git_root: Option<String>,
    /// Repository name (the main repository's name for linked worktrees).
    pub repo: Option<String>,
    pub branch: Option<String>,
    /// True while the agent waits for you.
    pub needs_attention: bool,
    /// Number of turns started so far.
    pub turn_counter: u32,
    pub last_turn_started_at: Option<Instant>,
    /// Why the agent is waiting (matched prompt / notification text).
    pub waiting_reason: Option<String>,
    /// Last non-empty line of output, for the sidebar preview.
    pub preview: String,
    pub source: DetectSource,
    /// When `state` last changed.
    pub state_since: Instant,
    pub(crate) machine: state::Machine,
}

impl AgentSession {
    /// "repo/branch" (or whichever part is known, else the cwd's last component).
    pub fn place(&self) -> String {
        match (&self.repo, &self.branch) {
            (Some(r), Some(b)) => format!("{r}/{b}"),
            (Some(r), None) => r.clone(),
            (None, Some(b)) => b.clone(),
            (None, None) => self
                .cwd
                .as_deref()
                .and_then(|c| std::path::Path::new(c).file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }
}

/// Lifecycle events for consumers such as change review.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    /// The user sent a prompt and the agent started working.
    TurnStarted {
        pane_uid: usize,
        kind: AgentKind,
        turn: u32,
        cwd: Option<String>,
        git_root: Option<String>,
    },
    /// The agent is blocked on you (`reason` is the prompt / notification text).
    NeedsUser {
        pane_uid: usize,
        kind: AgentKind,
        reason: String,
        cwd: Option<String>,
        git_root: Option<String>,
    },
    /// The turn ended (agent idle, reported done, or exited mid-turn).
    TurnFinished {
        pane_uid: usize,
        kind: AgentKind,
        turn: u32,
        elapsed: std::time::Duration,
        cwd: Option<String>,
        git_root: Option<String>,
    },
    /// The agent process ended.
    Exited {
        pane_uid: usize,
        kind: AgentKind,
        exit: Option<i32>,
        cwd: Option<String>,
        git_root: Option<String>,
    },
}

impl AgentEvent {
    pub fn pane_uid(&self) -> usize {
        match self {
            AgentEvent::TurnStarted { pane_uid, .. }
            | AgentEvent::NeedsUser { pane_uid, .. }
            | AgentEvent::TurnFinished { pane_uid, .. }
            | AgentEvent::Exited { pane_uid, .. } => *pane_uid,
        }
    }
}

/// `[agents]` section of config.toml.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentsConfig {
    /// Supervise agents at all (detection, sidebar, notifications).
    pub enabled: bool,
    /// Desktop notification + dock badge when an agent needs you / finishes a
    /// turn while you are not looking at its pane.
    pub notify: bool,
    /// Play the system sound with those notifications.
    pub sound: bool,
    /// Extra case-insensitive substrings that mark an approval prompt.
    pub approval_patterns: Vec<String>,
    /// Agent used by "Agent Layout" (slug); first installed one when empty.
    pub default_agent: String,
    /// Width of the Mission Control dock in character cells (0 = default 34);
    /// saved when you drag the dock's edge.
    pub dock_cols: usize,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self { enabled: true, notify: true, sound: false, approval_patterns: Vec::new(), default_agent: String::new(), dock_cols: 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_parse_roundtrip() {
        for k in AgentKind::ALL {
            assert_eq!(AgentKind::parse(k.slug()), Some(k));
            assert_eq!(AgentKind::parse(k.binary()), Some(k));
            assert_eq!(AgentKind::parse(k.name()), Some(k));
        }
        assert_eq!(AgentKind::parse("Claude-Code"), Some(AgentKind::ClaudeCode));
        assert_eq!(AgentKind::parse("vim"), None);
    }

    #[test]
    fn state_liveness() {
        assert!(AgentState::Working.is_live());
        assert!(!AgentState::Done { exit: Some(0) }.is_live());
        assert!(!AgentState::Error.is_live());
    }
}
