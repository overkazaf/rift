//! Approval flow for `run_command`: pre-flight vetting plus a race-free
//! decision cell shared between the UI thread (which owns the modal) and the
//! client thread (which waits, with a timeout, for the answer).
//!
//! Invariants, all unit-tested:
//! * a command can only run if the user explicitly chose Run (never a default);
//! * the decision is made exactly once;
//! * if the waiting client gave up first, a late "Run" click does nothing.

use std::sync::{Arc, Mutex};

use crate::tools::exec_preview::{ExecPreview, Severity};

/// Longest command the user is asked to review (the modal shows all of it).
pub const MAX_REVIEWABLE_COMMAND: usize = 1000;
/// How long a client waits for the human.
pub const APPROVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
/// Approval modals that may be queued at once.
pub const MAX_PENDING: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Pending,
    Approved,
    Denied,
    /// The requester stopped waiting before the user decided.
    Abandoned,
}

/// One approval request's state machine: `Pending` -> exactly one of the others.
#[derive(Debug)]
pub struct ApprovalCell {
    phase: Mutex<Phase>,
}

impl ApprovalCell {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { phase: Mutex::new(Phase::Pending) })
    }

    #[cfg(test)]
    pub fn phase(&self) -> Phase {
        *self.phase.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn transition(&self, to: Phase) -> bool {
        let mut p = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        if *p == Phase::Pending {
            *p = to;
            true
        } else {
            false
        }
    }

    /// The user answered. Returns `true` if this answer counts; `false` if the
    /// request was already decided or abandoned (then nothing may run).
    pub fn decide(&self, approve: bool) -> bool {
        self.transition(if approve { Phase::Approved } else { Phase::Denied })
    }

    /// The requester's wait expired. Returns `true` if it was still pending
    /// (the client reports "expired"); `false` if a decision won the race (the
    /// client must then collect that decision's reply).
    pub fn abandon(&self) -> bool {
        self.transition(Phase::Abandoned)
    }
}

/// What the UI thread knows about the target pane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaneState {
    pub exited: bool,
    pub alt_screen: bool,
    /// A command is executing (OSC 133 C seen, no D yet).
    pub busy: bool,
    /// The user has text typed at the prompt that a new command would corrupt.
    pub typing: bool,
}

/// Characters that make a command look different from what it does.
fn is_deceptive(c: char) -> bool {
    matches!(c,
        '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

/// Pre-flight checks. `Err` is shown to the agent verbatim; no modal appears.
pub fn vet(command: &str, pane: Option<PaneState>, pending: usize) -> Result<(), String> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return Err("command is empty".into());
    }
    if cmd.chars().count() > MAX_REVIEWABLE_COMMAND {
        return Err(format!("command is longer than {MAX_REVIEWABLE_COMMAND} characters; split it up so the user can review it"));
    }
    if command.chars().any(|c| (c.is_control() && c != '\t') || is_deceptive(c)) {
        return Err("command must be a single line without control or invisible characters".into());
    }
    let Some(p) = pane else { return Err("no such pane".into()) };
    if p.exited {
        return Err("the pane's process has exited".into());
    }
    if p.alt_screen {
        return Err("the pane is running a full-screen program; refusing to type into it".into());
    }
    if p.busy {
        return Err("a command is still running in that pane; wait for it to finish (list_blocks)".into());
    }
    if p.typing {
        return Err("the user has text typed at the prompt; try again in a moment".into());
    }
    if pending >= MAX_PENDING {
        return Err("too many run_command approvals are already waiting for the user".into());
    }
    Ok(())
}

/// How dangerous `ExecPreview` thinks a command is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Danger {
    pub critical: bool,
    /// Human-readable consequences (description, then detail).
    pub impacts: Vec<String>,
}

/// Classify with the same engine as the Enter-key interceptor
/// (`ExecPreview::check_for_enter`): routine commands return `None`.
pub fn assess(command: &str, cwd: Option<&str>) -> Option<Danger> {
    let p = ExecPreview::check_for_enter(command, cwd)?;
    let mut impacts = Vec::new();
    for i in &p.impacts {
        impacts.push(if i.detail.is_empty() { i.description.clone() } else { format!("{} ({})", i.description, i.detail) });
    }
    Some(Danger { critical: p.severity == Severity::Critical, impacts })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_pane() -> Option<PaneState> {
        Some(PaneState::default())
    }

    #[test]
    fn decision_is_made_exactly_once() {
        let c = ApprovalCell::new();
        assert_eq!(c.phase(), Phase::Pending);
        assert!(c.decide(true));
        assert_eq!(c.phase(), Phase::Approved);
        assert!(!c.decide(false), "second answer is ignored");
        assert!(!c.abandon(), "cannot abandon a decided request");
        assert_eq!(c.phase(), Phase::Approved);
    }

    #[test]
    fn denial_is_final() {
        let c = ApprovalCell::new();
        assert!(c.decide(false));
        assert_eq!(c.phase(), Phase::Denied);
        assert!(!c.decide(true), "cannot flip a denial into an approval");
    }

    #[test]
    fn late_run_click_after_the_client_gave_up_does_nothing() {
        let c = ApprovalCell::new();
        assert!(c.abandon());
        assert_eq!(c.phase(), Phase::Abandoned);
        assert!(!c.decide(true), "caller must not run the command");
        assert_eq!(c.phase(), Phase::Abandoned);
    }

    #[test]
    fn decide_and_abandon_race_has_one_winner() {
        for _ in 0..200 {
            let c = ApprovalCell::new();
            let c2 = c.clone();
            let t = std::thread::spawn(move || c2.abandon());
            let decided = c.decide(true);
            let abandoned = t.join().unwrap();
            assert!(decided ^ abandoned, "exactly one side wins");
        }
    }

    #[test]
    fn vet_rejects_unsafe_requests_before_any_modal() {
        assert!(vet("ls -la", ok_pane(), 0).is_ok());
        assert!(vet("   ", ok_pane(), 0).is_err());
        assert!(vet("ls\nrm -rf ~", ok_pane(), 0).is_err(), "newline would run a second command");
        assert!(vet("ls\r", ok_pane(), 0).is_err());
        assert!(vet("echo \x1b[2J", ok_pane(), 0).is_err());
        assert!(vet("ls \u{202e}gnp.txt", ok_pane(), 0).is_err(), "bidi override hides the real command");
        assert!(vet("ls\u{200b}", ok_pane(), 0).is_err());
        assert!(vet(&"a".repeat(MAX_REVIEWABLE_COMMAND + 1), ok_pane(), 0).is_err());
        assert!(vet("ls", None, 0).is_err());
        let st = |f: fn(&mut PaneState)| {
            let mut p = PaneState::default();
            f(&mut p);
            Some(p)
        };
        assert!(vet("ls", st(|p| p.exited = true), 0).is_err());
        assert!(vet("ls", st(|p| p.alt_screen = true), 0).is_err());
        assert!(vet("ls", st(|p| p.busy = true), 0).is_err());
        assert!(vet("ls", st(|p| p.typing = true), 0).is_err());
        assert!(vet("ls", ok_pane(), MAX_PENDING).is_err());
        assert!(vet("echo\ttab", ok_pane(), 0).is_ok());
    }

    #[test]
    fn dangerous_commands_are_classified_like_the_enter_key_interceptor() {
        let d = assess("rm -rf /", None).expect("rm -rf / is dangerous");
        assert!(d.critical);
        assert!(!d.impacts.is_empty());
        assert!(assess("echo hello", None).is_none());
        assert!(assess("ls -la", None).is_none());
    }
}
