//! The agent state machine and the screen / notification heuristics feeding it.
//!
//! [`Machine`] is pure: every input carries the current `Instant`, so tests
//! drive it with synthetic timelines. Signals, strongest first:
//!
//! * hooks (`rift agent-event`): authoritative, e.g. Claude Code's
//!   `UserPromptSubmit` / `Notification` / `Stop`;
//! * OSC 9 / OSC 777 notifications (Claude Code with `preferredNotifChannel`,
//!   Codex with `tui.notifications`): classified by [`classify_notification`];
//! * an approval prompt visible on a *quiet* screen ([`APPROVAL_PATTERNS`]);
//! * output activity: a submitted prompt followed by output = a turn started;
//!   output that stops for [`Timing::quiet_idle`] = the turn ended;
//! * block end / process exit.

use std::time::{Duration, Instant};

use super::{AgentKind, AgentState};

// ───────────────────────────── approval prompts ─────────────────────────────

/// One screen pattern that means "the agent is asking you something".
pub struct ApprovalPattern {
    /// Lowercase substring searched in the (lowercased, box-stripped) screen tail.
    pub needle: &'static str,
    /// A second lowercase substring that must also appear in the tail (cuts
    /// false positives of short needles).
    pub also: Option<&'static str>,
    /// Agents this pattern is known from (empty = any). Documentation and
    /// tests; matching is agent-independent so a wrapper script still works.
    pub agents: &'static [AgentKind],
    /// Short human label, shown as the "needs you" reason.
    pub label: &'static str,
}

use AgentKind::{Aider, ClaudeCode, Codex, CursorAgent, Gemini, OpenCode};

/// The single table of approval-prompt patterns. Add new agents' prompts here
/// and extend the tests below; user additions come from `[agents] approval_patterns`.
pub const APPROVAL_PATTERNS: &[ApprovalPattern] = &[
    // Claude Code
    ApprovalPattern { needle: "do you want to proceed?", also: None, agents: &[ClaudeCode], label: "approve tool use" },
    ApprovalPattern { needle: "do you want to make this edit", also: None, agents: &[ClaudeCode], label: "approve edit" },
    ApprovalPattern { needle: "do you want to create", also: Some("yes"), agents: &[ClaudeCode], label: "approve new file" },
    ApprovalPattern { needle: "do you want to overwrite", also: Some("yes"), agents: &[ClaudeCode], label: "approve overwrite" },
    ApprovalPattern { needle: "do you trust the files in this folder", also: None, agents: &[ClaudeCode], label: "trust this folder" },
    ApprovalPattern { needle: "ready to code?", also: Some("yes"), agents: &[ClaudeCode], label: "approve plan" },
    ApprovalPattern { needle: "would you like to proceed?", also: Some("yes"), agents: &[ClaudeCode], label: "approve plan" },
    // Numbered yes/no menus (Claude Code, Gemini CLI, Codex, Cursor CLI)
    ApprovalPattern { needle: "1. yes", also: Some("2. "), agents: &[ClaudeCode, Gemini, Codex], label: "choose an option" },
    // Codex CLI
    ApprovalPattern { needle: "allow command?", also: None, agents: &[Codex], label: "approve command" },
    ApprovalPattern { needle: "would you like to run the following command", also: None, agents: &[Codex], label: "approve command" },
    ApprovalPattern { needle: "would you like to make the following edits", also: None, agents: &[Codex], label: "approve edits" },
    ApprovalPattern { needle: "yes, proceed", also: Some("esc"), agents: &[Codex], label: "approve" },
    ApprovalPattern { needle: "allow codex to work in this folder", also: None, agents: &[Codex], label: "trust this folder" },
    ApprovalPattern { needle: "press enter to confirm or esc", also: None, agents: &[Codex], label: "confirm" },
    // Gemini CLI
    ApprovalPattern { needle: "allow execution of", also: None, agents: &[Gemini], label: "approve command" },
    ApprovalPattern { needle: "apply this change?", also: None, agents: &[Gemini], label: "approve edit" },
    ApprovalPattern { needle: "yes, allow once", also: None, agents: &[Gemini], label: "approve" },
    ApprovalPattern { needle: "do you trust this folder", also: None, agents: &[Gemini], label: "trust this folder" },
    ApprovalPattern { needle: "do you trust the contents of this directory", also: None, agents: &[Codex, Gemini], label: "trust this folder" },
    // opencode
    ApprovalPattern { needle: "allow always", also: Some("allow once"), agents: &[OpenCode], label: "grant permission" },
    ApprovalPattern { needle: "permission required", also: None, agents: &[OpenCode], label: "grant permission" },
    // Cursor CLI
    ApprovalPattern { needle: "run (once)", also: None, agents: &[CursorAgent], label: "approve command" },
    ApprovalPattern { needle: "add to allowlist", also: None, agents: &[CursorAgent], label: "approve command" },
    // Aider and generic prompts
    ApprovalPattern { needle: "(y)es/(n)o", also: None, agents: &[Aider], label: "answer y/n" },
    ApprovalPattern { needle: "[y/n]", also: None, agents: &[], label: "answer y/n" },
    ApprovalPattern { needle: "(y/n)", also: None, agents: &[], label: "answer y/n" },
    ApprovalPattern { needle: "[yes/no]", also: None, agents: &[], label: "answer yes/no" },
    ApprovalPattern { needle: "allow this", also: Some("?"), agents: &[], label: "grant permission" },
    ApprovalPattern { needle: "approve?", also: None, agents: &[], label: "approve" },
    ApprovalPattern { needle: "approve this", also: Some("?"), agents: &[], label: "approve" },
    ApprovalPattern { needle: "waiting for your approval", also: None, agents: &[], label: "approve" },
    ApprovalPattern { needle: "needs your approval", also: None, agents: &[], label: "approve" },
    ApprovalPattern { needle: "needs your permission", also: None, agents: &[], label: "grant permission" },
];

/// How many non-empty bottom rows of the screen are searched.
pub const SCREEN_TAIL_ROWS: usize = 14;

/// Lowercase, drop box-drawing / block characters and collapse whitespace so a
/// pattern matches regardless of the frame an agent draws around its prompt.
pub fn normalize_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut space = true;
    for c in line.chars() {
        let c = match c {
            '\u{2500}'..='\u{259f}' | '\u{00a0}' => ' ',
            c => c,
        };
        if c.is_whitespace() {
            if !space {
                out.push(' ');
                space = true;
            }
        } else {
            for l in c.to_lowercase() {
                out.push(l);
            }
            space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// The label of the first approval pattern found in the bottom rows of
/// `screen_lines` (top to bottom), if any. `extra` are user patterns.
pub fn match_approval(screen_lines: &[String], extra: &[String]) -> Option<String> {
    let tail: Vec<String> = screen_lines
        .iter()
        .map(|l| normalize_line(l))
        .filter(|l| !l.is_empty())
        .rev()
        .take(SCREEN_TAIL_ROWS)
        .collect();
    if tail.is_empty() {
        return None;
    }
    let joined = tail.iter().rev().cloned().collect::<Vec<_>>().join("\n");
    for p in APPROVAL_PATTERNS {
        if joined.contains(p.needle) && p.also.map_or(true, |a| joined.contains(a)) {
            return Some(p.label.to_string());
        }
    }
    for e in extra {
        let e = e.trim().to_lowercase();
        if !e.is_empty() && joined.contains(&e) {
            return Some(format!("matches \"{e}\""));
        }
    }
    None
}

// ───────────────────────────── notifications ─────────────────────────────

/// What an OSC 9 / OSC 777 notification from an agent means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoteClass {
    /// Needs approval / permission / an answer (carries the reason text).
    Approval(String),
    /// Waiting for the next prompt (Claude Code's idle reminder).
    InputWait,
    /// The turn finished.
    Finished,
    Other,
}

/// Classify notification text. Claude Code: "Claude needs your permission to
/// use Bash", "Claude is waiting for your input"; Codex: "Approval requested:
/// ...", "Agent turn complete: ...".
pub fn classify_notification(title: &str, body: &str) -> NoteClass {
    let text = format!("{title} {body}").to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
    if has(&["waiting for your input", "waiting for input", "waiting for you", "your turn"]) {
        return NoteClass::InputWait;
    }
    if has(&[
        "permission", "approval", "approve", "needs your", "requires your", "confirm", "allow ", "attention", "needs input",
        "question",
    ]) {
        let reason: String = if body.trim().is_empty() { title } else { body }.trim().chars().take(120).collect();
        return NoteClass::Approval(reason);
    }
    if has(&["turn complete", "complete", "completed", "finished", "done", "ready for"]) {
        return NoteClass::Finished;
    }
    NoteClass::Other
}

// ───────────────────────────── timing + machine ─────────────────────────────

/// Thresholds. Public so tests (and later a config) can shorten them.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// Working -> Idle after this long without output (no hooks).
    pub quiet_idle: Duration,
    /// An approval prompt must stay on a quiet screen this long before it counts.
    pub prompt_debounce: Duration,
    /// Starting -> Idle after the first paint settles.
    pub startup_quiet: Duration,
    /// Starting with sustained output this long = the agent began a turn on its own
    /// (e.g. `claude "fix the tests"`).
    pub startup_max: Duration,
    /// With hooks, the quiet fallback is much longer (hooks announce the end).
    pub hooked_idle: Duration,
    /// A Waiting state raised from the screen alone clears this long after the
    /// prompt left the screen with no output.
    pub waiting_clear: Duration,
    /// Output burst that starts a turn without a seen submit.
    pub burst_bytes: u64,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            quiet_idle: Duration::from_millis(2500),
            prompt_debounce: Duration::from_millis(600),
            startup_quiet: Duration::from_millis(1500),
            startup_max: Duration::from_secs(8),
            hooked_idle: Duration::from_secs(90),
            waiting_clear: Duration::from_secs(3),
            burst_bytes: 16 * 1024,
        }
    }
}

/// What a hook (`rift agent-event <state>`) reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookKind {
    /// A prompt was submitted / the agent is working (Claude `UserPromptSubmit`).
    Working,
    /// The agent needs you (Claude `Notification`).
    Waiting,
    /// The turn is over (Claude `Stop`).
    Done,
    Idle,
    Error,
}

impl HookKind {
    pub fn parse(s: &str) -> Option<HookKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "working" | "busy" | "prompt" | "running" => Some(HookKind::Working),
            "waiting" | "wait" | "needs-user" | "needs_user" | "attention" | "input" | "notification" | "notify" => Some(HookKind::Waiting),
            "done" | "stop" | "stopped" | "finished" | "complete" | "completed" | "turn-complete" => Some(HookKind::Done),
            "idle" | "ready" => Some(HookKind::Idle),
            "error" | "failed" | "fail" => Some(HookKind::Error),
            _ => None,
        }
    }
}

/// What a state change means to the outside.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    TurnStarted,
    NeedsUser(String),
    TurnFinished(Duration),
    Exited(Option<i32>),
}

#[derive(Clone, Debug)]
pub struct Machine {
    pub state: AgentState,
    pub since: Instant,
    pub turn: u32,
    pub turn_started: Option<Instant>,
    /// Start of the most recent turn (kept after it ends).
    pub last_turn_started: Option<Instant>,
    pub last_activity: Instant,
    pub waiting_reason: Option<String>,
    timing: Timing,
    activity_seen: bool,
    hooked: bool,
    waiting_from_screen: bool,
    submit_pending: bool,
    last_input: Option<Instant>,
    /// Approval prompt currently on screen: (first seen, label).
    prompt: Option<(Instant, String)>,
    /// Moment the prompt disappeared while Waiting (screen-raised only).
    prompt_gone_at: Option<Instant>,
}

impl Machine {
    pub fn new(now: Instant) -> Self {
        Self::with_timing(now, Timing::default())
    }

    pub fn with_timing(now: Instant, timing: Timing) -> Self {
        Self {
            state: AgentState::Starting,
            since: now,
            turn: 0,
            turn_started: None,
            last_turn_started: None,
            last_activity: now,
            waiting_reason: None,
            timing,
            activity_seen: false,
            hooked: false,
            waiting_from_screen: false,
            submit_pending: false,
            last_input: None,
            prompt: None,
            prompt_gone_at: None,
        }
    }

    pub fn is_live(&self) -> bool {
        self.state.is_live()
    }

    fn set(&mut self, s: AgentState, now: Instant) {
        if self.state != s {
            self.state = s;
            self.since = now;
        }
        if s != AgentState::WaitingForUser {
            self.waiting_reason = None;
            self.waiting_from_screen = false;
            self.prompt_gone_at = None;
        }
    }

    fn start_turn(&mut self, now: Instant, out: &mut Vec<Effect>) {
        self.turn += 1;
        self.turn_started = Some(now);
        self.last_turn_started = Some(now);
        self.submit_pending = false;
        self.set(AgentState::Working, now);
        out.push(Effect::TurnStarted);
    }

    fn finish_turn(&mut self, now: Instant, to: AgentState, out: &mut Vec<Effect>) {
        if let Some(t) = self.turn_started.take() {
            out.push(Effect::TurnFinished(now.saturating_duration_since(t)));
        }
        self.set(to, now);
    }

    fn enter_waiting(&mut self, reason: String, from_screen: bool, now: Instant, out: &mut Vec<Effect>) {
        if !self.is_live() {
            return;
        }
        if self.state == AgentState::WaitingForUser {
            self.waiting_reason = Some(reason);
            return;
        }
        self.set(AgentState::WaitingForUser, now);
        self.waiting_reason = Some(reason.clone());
        self.waiting_from_screen = from_screen;
        out.push(Effect::NeedsUser(reason));
    }

    /// The agent produced output (not typing echo) at `now`.
    pub fn on_activity(&mut self, now: Instant, bytes: u64) -> Vec<Effect> {
        let mut out = Vec::new();
        if !self.is_live() {
            return out;
        }
        self.last_activity = now;
        self.activity_seen = true;
        match self.state {
            AgentState::Starting => {
                if self.submit_pending {
                    self.start_turn(now, &mut out);
                }
            }
            AgentState::Idle => {
                if self.submit_pending || bytes >= self.timing.burst_bytes {
                    self.start_turn(now, &mut out);
                }
            }
            AgentState::WaitingForUser => {
                // Output after the user answered: the agent carries on.
                if self.last_input.is_some_and(|t| t > self.since) {
                    if self.turn_started.is_none() {
                        self.start_turn(now, &mut out);
                    } else {
                        self.set(AgentState::Working, now);
                    }
                }
            }
            AgentState::Working | AgentState::Done { .. } | AgentState::Error => {}
        }
        out
    }

    /// The user typed into the pane. `submit` = the input contained Enter.
    pub fn on_user_input(&mut self, now: Instant, submit: bool) {
        self.last_input = Some(now);
        if submit && matches!(self.state, AgentState::Starting | AgentState::Idle) {
            self.submit_pending = true;
        }
    }

    /// Result of scanning the screen: the label of an approval prompt, or `None`.
    pub fn on_screen(&mut self, now: Instant, prompt: Option<String>) {
        match prompt {
            Some(label) => match &mut self.prompt {
                Some((_, l)) => *l = label,
                None => self.prompt = Some((now, label)),
            },
            None => {
                if self.prompt.take().is_some() && self.state == AgentState::WaitingForUser && self.waiting_from_screen {
                    self.prompt_gone_at = Some(now);
                }
            }
        }
    }

    pub fn on_notification(&mut self, now: Instant, class: NoteClass) -> Vec<Effect> {
        let mut out = Vec::new();
        if !self.is_live() {
            return out;
        }
        match class {
            NoteClass::Approval(reason) => self.enter_waiting(reason, false, now, &mut out),
            NoteClass::Finished => {
                if matches!(self.state, AgentState::Working | AgentState::WaitingForUser | AgentState::Starting) {
                    self.finish_turn(now, AgentState::Idle, &mut out);
                }
            }
            NoteClass::InputWait => {
                if self.state == AgentState::Working {
                    self.enter_waiting("waiting for your input".into(), false, now, &mut out);
                }
            }
            NoteClass::Other => {}
        }
        out
    }

    pub fn on_hook(&mut self, now: Instant, hook: HookKind, message: Option<&str>) -> Vec<Effect> {
        let mut out = Vec::new();
        if !self.is_live() {
            return out;
        }
        self.hooked = true;
        match hook {
            HookKind::Working => match self.state {
                AgentState::Working => {}
                AgentState::WaitingForUser if self.turn_started.is_some() => self.set(AgentState::Working, now),
                _ => self.start_turn(now, &mut out),
            },
            HookKind::Waiting => {
                let reason = message.map(str::trim).filter(|m| !m.is_empty()).unwrap_or("needs your attention");
                let reason: String = reason.chars().take(120).collect();
                self.enter_waiting(reason, false, now, &mut out);
            }
            HookKind::Done | HookKind::Idle => {
                if self.state != AgentState::Idle || self.turn_started.is_some() {
                    self.finish_turn(now, AgentState::Idle, &mut out);
                }
            }
            HookKind::Error => {
                self.finish_turn(now, AgentState::Error, &mut out);
            }
        }
        self.submit_pending = false;
        out
    }

    /// The agent process / command ended.
    pub fn on_exit(&mut self, now: Instant, exit: Option<i32>) -> Vec<Effect> {
        let mut out = Vec::new();
        if !self.is_live() {
            return out;
        }
        let failed = exit.is_some_and(|c| c != 0 && c != 130 && c != 143);
        let to = if failed { AgentState::Error } else { AgentState::Done { exit } };
        self.finish_turn(now, to, &mut out);
        out.push(Effect::Exited(exit));
        out
    }

    /// Time-based transitions; call regularly.
    pub fn tick(&mut self, now: Instant) -> Vec<Effect> {
        let mut out = Vec::new();
        if !self.is_live() {
            return out;
        }
        let quiet = now.saturating_duration_since(self.last_activity);
        // An approval prompt sitting on a quiet screen beats everything else.
        if let Some((seen, label)) = self.prompt.clone() {
            if self.state != AgentState::WaitingForUser
                && quiet >= self.timing.prompt_debounce
                && now.saturating_duration_since(seen) >= self.timing.prompt_debounce
            {
                self.enter_waiting(label, true, now, &mut out);
                return out;
            }
        }
        match self.state {
            AgentState::Working => {
                let limit = if self.hooked { self.timing.hooked_idle } else { self.timing.quiet_idle };
                if quiet >= limit {
                    self.finish_turn(now, AgentState::Idle, &mut out);
                }
            }
            AgentState::Starting => {
                let age = now.saturating_duration_since(self.since);
                if self.activity_seen && quiet >= self.timing.startup_quiet {
                    self.set(AgentState::Idle, now);
                } else if age >= self.timing.startup_max {
                    if self.activity_seen && quiet < self.timing.startup_quiet {
                        // Still streaming after the UI should have settled: a turn is running.
                        self.start_turn(now, &mut out);
                    } else {
                        self.set(AgentState::Idle, now);
                    }
                }
            }
            AgentState::WaitingForUser => {
                if let Some(gone) = self.prompt_gone_at {
                    if now.saturating_duration_since(gone) >= self.timing.waiting_clear && quiet >= self.timing.waiting_clear {
                        // The prompt was dismissed without further output.
                        self.finish_turn(now, AgentState::Idle, &mut out);
                    }
                }
            }
            AgentState::Idle | AgentState::Done { .. } | AgentState::Error => {}
        }
        out
    }

    /// Next instant at which [`Machine::tick`] could change something.
    pub fn next_deadline(&self) -> Option<Instant> {
        let base = match self.state {
            AgentState::Working => {
                let limit = if self.hooked { self.timing.hooked_idle } else { self.timing.quiet_idle };
                Some(self.last_activity + limit)
            }
            AgentState::Starting => Some(self.last_activity + self.timing.startup_quiet),
            AgentState::WaitingForUser => self.prompt_gone_at.map(|g| g + self.timing.waiting_clear),
            AgentState::Idle => None,
            AgentState::Done { .. } | AgentState::Error => return None,
        };
        // A prompt on screen turns into Waiting once the screen has been still long enough.
        let prompt = match (&self.prompt, self.state) {
            (Some((seen, _)), s) if s != AgentState::WaitingForUser => Some((*seen).max(self.last_activity) + self.timing.prompt_debounce),
            _ => None,
        };
        match (base, prompt) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(str::to_string).collect()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    // ── approval pattern table ──

    #[test]
    fn claude_code_prompts() {
        let tool = lines("╭──────────────────────────────╮\n│ Bash command                 │\n│   rm -rf build               │\n╰──────────────────────────────╯\n Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, and don't ask again for rm commands\n   3. No, and tell Claude what to do differently (esc)");
        assert_eq!(match_approval(&tool, &[]).as_deref(), Some("approve tool use"));
        let edit = lines("│ Do you want to make this edit to main.rs? │\n│ ❯ 1. Yes │\n│   2. Yes, allow all edits during this session │\n│   3. No, keep asking │");
        assert_eq!(match_approval(&edit, &[]).as_deref(), Some("approve edit"));
        let trust = lines(" Do you trust the files in this folder?\n ❯ 1. Yes, proceed\n   2. No, exit");
        assert_eq!(match_approval(&trust, &[]).as_deref(), Some("trust this folder"));
    }

    #[test]
    fn codex_prompts() {
        let a = lines("Would you like to run the following command?\n  $ cargo test\n› 1. Yes, proceed (y)\n  2. Yes, and don't ask again (a)\n  3. No, and tell Codex what to do differently (esc)");
        assert!(match_approval(&a, &[]).is_some());
        assert_eq!(match_approval(&lines("Allow command? [y/N]"), &[]).as_deref(), Some("approve command"));
        let e = lines("Would you like to make the following edits?\n  src/lib.rs (+3 -1)");
        assert_eq!(match_approval(&e, &[]).as_deref(), Some("approve edits"));
    }

    #[test]
    fn gemini_prompts() {
        let a = lines("Allow execution of: 'git status'?\n● 1. Yes, allow once\n  2. Yes, allow always\n  3. Modify with external editor\n  4. No, suggest changes (esc)");
        assert!(match_approval(&a, &[]).is_some());
        assert_eq!(match_approval(&lines("Apply this change?"), &[]).as_deref(), Some("approve edit"));
    }

    #[test]
    fn opencode_cursor_aider_prompts() {
        assert!(match_approval(&lines("Permission required: bash\n  Allow once   Allow always   Reject"), &[]).is_some());
        assert_eq!(match_approval(&lines("Run (once) (y)\nAdd to allowlist\nSkip (esc or n)"), &[]).as_deref(), Some("approve command"));
        assert_eq!(match_approval(&lines("Run shell command? (Y)es/(N)o/(D)on't ask again [Yes]:"), &[]).as_deref(), Some("answer y/n"));
        assert_eq!(match_approval(&lines("Add file to the chat? [Y/n]"), &[]).as_deref(), Some("answer y/n"));
    }

    #[test]
    fn generic_prompts() {
        assert!(match_approval(&lines("Overwrite config.toml? (y/n)"), &[]).is_some());
        assert!(match_approval(&lines("Approve?"), &[]).is_some());
        assert!(match_approval(&lines("Continue [yes/no]"), &[]).is_some());
    }

    #[test]
    fn prose_and_normal_output_do_not_match() {
        for t in [
            "I will approve the PR after the tests pass.",
            "Allow list updated for 3 hosts",
            "Working on it... (esc to interrupt)",
            "1. Yes it compiles",
            "The user can answer yes/no",
            "$ cargo build\n   Compiling rift v0.3.0\n    Finished dev profile",
            "To approve a change, edit the file.",
            "",
        ] {
            assert_eq!(match_approval(&lines(t), &[]), None, "{t:?}");
        }
    }

    #[test]
    fn only_the_screen_tail_counts() {
        let mut v = vec!["Do you want to proceed?".to_string()];
        for i in 0..40 {
            v.push(format!("log line {i}"));
        }
        assert_eq!(match_approval(&v, &[]), None, "stale prompt far above the tail");
        v.insert(0, String::new());
        let near: Vec<String> = ["a", "b", "Do you want to proceed?", "❯ 1. Yes"].iter().map(|s| s.to_string()).collect();
        assert!(match_approval(&near, &[]).is_some());
    }

    #[test]
    fn user_patterns_extend_the_table() {
        let l = lines("Ship it to production? type CONFIRM-DEPLOY");
        assert_eq!(match_approval(&l, &[]), None);
        let extra = vec!["confirm-deploy".to_string()];
        assert!(match_approval(&l, &extra).unwrap().contains("confirm-deploy"));
        assert_eq!(match_approval(&l, &["  ".to_string()]), None, "blank pattern ignored");
    }

    #[test]
    fn pattern_table_is_well_formed() {
        for p in APPROVAL_PATTERNS {
            assert_eq!(p.needle, p.needle.to_lowercase(), "needle must be lowercase: {}", p.needle);
            assert!(p.needle.len() >= 4);
            assert!(!p.label.is_empty());
            if let Some(a) = p.also {
                assert_eq!(a, a.to_lowercase());
            }
        }
        // Every supported agent has at least one specific pattern.
        for k in AgentKind::ALL {
            assert!(APPROVAL_PATTERNS.iter().any(|p| p.agents.contains(&k)), "{k:?} has no pattern");
        }
    }

    #[test]
    fn normalize_strips_frames() {
        assert_eq!(normalize_line("│  Do   you want to PROCEED?  │"), "do you want to proceed?");
        assert_eq!(normalize_line("╭────╮"), "");
    }

    // ── notifications ──

    #[test]
    fn notification_classes() {
        assert!(matches!(classify_notification("Claude Code", "Claude needs your permission to use Bash"), NoteClass::Approval(_)));
        assert_eq!(classify_notification("Claude Code", "Claude is waiting for your input"), NoteClass::InputWait);
        assert!(matches!(classify_notification("Codex", "Approval requested: cargo test"), NoteClass::Approval(_)));
        assert_eq!(classify_notification("Codex", "Agent turn complete: fixed 3 tests"), NoteClass::Finished);
        assert_eq!(classify_notification("", "hello"), NoteClass::Other);
    }

    #[test]
    fn hook_names() {
        assert_eq!(HookKind::parse("waiting"), Some(HookKind::Waiting));
        assert_eq!(HookKind::parse("Stop"), Some(HookKind::Done));
        assert_eq!(HookKind::parse("done"), Some(HookKind::Done));
        assert_eq!(HookKind::parse("working"), Some(HookKind::Working));
        assert_eq!(HookKind::parse("nope"), None);
    }

    // ── state machine on synthetic timelines ──

    #[test]
    fn typical_turn_without_hooks() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        assert_eq!(m.state, AgentState::Starting);
        // The agent paints its UI, then goes quiet: ready.
        assert!(m.on_activity(t0 + ms(100), 4000).is_empty());
        assert!(m.tick(t0 + ms(1000)).is_empty());
        assert!(m.tick(t0 + ms(1700)).is_empty());
        assert_eq!(m.state, AgentState::Idle);
        // User types a prompt (echo is filtered upstream) and presses Enter.
        m.on_user_input(t0 + ms(5000), false);
        m.on_user_input(t0 + ms(5200), true);
        assert_eq!(m.state, AgentState::Idle);
        // Output starts: turn 1.
        assert_eq!(m.on_activity(t0 + ms(5400), 300), vec![Effect::TurnStarted]);
        assert_eq!(m.state, AgentState::Working);
        assert_eq!(m.turn, 1);
        // Spinner keeps output flowing.
        for i in 1..20 {
            assert!(m.on_activity(t0 + ms(5400 + i * 300), 80).is_empty());
            assert!(m.tick(t0 + ms(5400 + i * 300 + 100)).is_empty());
        }
        assert_eq!(m.state, AgentState::Working);
        // Output stops: after the quiet window the turn is over.
        let last = t0 + ms(5400 + 19 * 300);
        assert!(m.tick(last + ms(2000)).is_empty());
        let fx = m.tick(last + ms(2600));
        assert!(matches!(fx.as_slice(), [Effect::TurnFinished(d)] if *d > Duration::from_secs(5)));
        assert_eq!(m.state, AgentState::Idle);
        // Second prompt: a new turn.
        m.on_user_input(last + ms(9000), true);
        assert_eq!(m.on_activity(last + ms(9100), 200), vec![Effect::TurnStarted]);
        assert_eq!(m.turn, 2);
    }

    #[test]
    fn activity_without_submit_does_not_start_a_turn() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        m.on_activity(t0, 100);
        m.tick(t0 + ms(2000));
        assert_eq!(m.state, AgentState::Idle);
        // Cursor blinks / status line refreshes: small output, no submit.
        assert!(m.on_activity(t0 + ms(3000), 40).is_empty());
        assert_eq!(m.state, AgentState::Idle);
        // A huge burst does count (e.g. a resumed session replaying output).
        assert_eq!(m.on_activity(t0 + ms(4000), 64 * 1024), vec![Effect::TurnStarted]);
    }

    #[test]
    fn approval_prompt_on_quiet_screen_waits_then_continues_the_turn() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        m.on_activity(t0, 100);
        m.tick(t0 + ms(2000));
        m.on_user_input(t0 + ms(3000), true);
        m.on_activity(t0 + ms(3100), 500);
        assert_eq!(m.state, AgentState::Working);
        // Prompt appears while output is still arriving: not yet.
        m.on_activity(t0 + ms(4000), 900);
        m.on_screen(t0 + ms(4000), Some("approve tool use".into()));
        assert!(m.tick(t0 + ms(4300)).is_empty());
        assert_eq!(m.state, AgentState::Working);
        // Screen stays still for the debounce: waiting.
        m.on_screen(t0 + ms(4650), Some("approve tool use".into()));
        let fx = m.tick(t0 + ms(4700));
        assert_eq!(fx, vec![Effect::NeedsUser("approve tool use".into())]);
        assert_eq!(m.state, AgentState::WaitingForUser);
        assert_eq!(m.turn, 1, "no new turn");
        // Staying quiet does not time out into Idle.
        assert!(m.tick(t0 + ms(60_000)).is_empty());
        assert_eq!(m.state, AgentState::WaitingForUser);
        // User presses "1": the agent resumes in the same turn.
        m.on_user_input(t0 + ms(61_000), true);
        m.on_screen(t0 + ms(61_050), None);
        assert!(m.on_activity(t0 + ms(61_100), 400).is_empty());
        assert_eq!(m.state, AgentState::Working);
        assert_eq!(m.turn, 1);
        // Finishes.
        let fx = m.tick(t0 + ms(64_000));
        assert!(matches!(fx.as_slice(), [Effect::TurnFinished(_)]));
        assert_eq!(m.state, AgentState::Idle);
    }

    #[test]
    fn output_before_the_user_answers_keeps_waiting() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        m.on_notification(t0, NoteClass::Approval("x".into()));
        assert_eq!(m.state, AgentState::WaitingForUser);
        // The agent redraws its prompt (e.g. cursor blink) without user input.
        m.on_activity(t0 + ms(500), 50);
        assert_eq!(m.state, AgentState::WaitingForUser);
    }

    #[test]
    fn trust_prompt_at_startup() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        m.on_activity(t0 + ms(100), 2000);
        m.on_screen(t0 + ms(150), Some("trust this folder".into()));
        assert!(m.tick(t0 + ms(400)).is_empty());
        let fx = m.tick(t0 + ms(800));
        assert_eq!(fx, vec![Effect::NeedsUser("trust this folder".into())]);
        // Answered, then the agent starts: a turn is created lazily.
        m.on_user_input(t0 + ms(5000), true);
        assert_eq!(m.on_activity(t0 + ms(5100), 100), vec![Effect::TurnStarted]);
    }

    #[test]
    fn dismissed_prompt_returns_to_idle() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        m.on_activity(t0, 10);
        m.tick(t0 + ms(2000));
        m.on_user_input(t0 + ms(2500), true);
        m.on_activity(t0 + ms(2600), 100);
        m.on_screen(t0 + ms(2700), Some("answer y/n".into()));
        m.tick(t0 + ms(3400));
        assert_eq!(m.state, AgentState::WaitingForUser);
        // Esc: the prompt vanishes, nothing else is printed.
        m.on_screen(t0 + ms(5000), None);
        assert!(m.tick(t0 + ms(6000)).is_empty());
        let fx = m.tick(t0 + ms(8500));
        assert!(matches!(fx.as_slice(), [Effect::TurnFinished(_)]));
        assert_eq!(m.state, AgentState::Idle);
    }

    #[test]
    fn long_startup_stream_means_a_turn_began() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        // `claude "fix the tests"`: output never settles.
        for i in 0..40 {
            m.on_activity(t0 + ms(i * 250), 200);
            let fx = m.tick(t0 + ms(i * 250 + 50));
            if i < 31 {
                assert!(fx.is_empty());
            }
        }
        assert_eq!(m.state, AgentState::Working);
        assert_eq!(m.turn, 1);
    }

    #[test]
    fn hooks_are_authoritative() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        assert_eq!(m.on_hook(t0 + ms(10), HookKind::Working, None), vec![Effect::TurnStarted]);
        // A silent tool run longer than the heuristic window does not end the turn.
        assert!(m.tick(t0 + ms(30_000)).is_empty());
        assert_eq!(m.state, AgentState::Working);
        assert_eq!(
            m.on_hook(t0 + ms(31_000), HookKind::Waiting, Some("Claude needs your permission to use Bash")),
            vec![Effect::NeedsUser("Claude needs your permission to use Bash".into())]
        );
        assert_eq!(m.state, AgentState::WaitingForUser);
        // Duplicate hook: no second NeedsUser.
        assert!(m.on_hook(t0 + ms(31_500), HookKind::Waiting, None).is_empty());
        // Working again (UserPromptSubmit / PostToolUse) continues the turn.
        assert!(m.on_hook(t0 + ms(40_000), HookKind::Working, None).is_empty());
        assert_eq!(m.state, AgentState::Working);
        assert_eq!(m.turn, 1);
        let fx = m.on_hook(t0 + ms(50_000), HookKind::Done, None);
        assert!(matches!(fx.as_slice(), [Effect::TurnFinished(d)] if *d > Duration::from_secs(40)));
        assert_eq!(m.state, AgentState::Idle);
        // A second `done` is a no-op.
        assert!(m.on_hook(t0 + ms(51_000), HookKind::Done, None).is_empty());
    }

    #[test]
    fn hook_done_without_a_known_turn_still_goes_idle() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        m.on_activity(t0, 100);
        let fx = m.on_hook(t0 + ms(100), HookKind::Done, None);
        assert!(fx.is_empty(), "no turn was open");
        assert_eq!(m.state, AgentState::Idle);
    }

    #[test]
    fn osc_notifications_drive_state() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        m.on_activity(t0, 10);
        m.tick(t0 + ms(2000));
        m.on_user_input(t0 + ms(2100), true);
        m.on_activity(t0 + ms(2200), 100);
        let c = classify_notification("Claude Code", "Claude needs your permission to use Edit");
        let fx = m.on_notification(t0 + ms(3000), c);
        assert!(matches!(fx.as_slice(), [Effect::NeedsUser(r)] if r.contains("permission")));
        let fx = m.on_notification(t0 + ms(9000), classify_notification("Codex", "Agent turn complete: done"));
        assert!(matches!(fx.as_slice(), [Effect::TurnFinished(_)]));
        assert_eq!(m.state, AgentState::Idle);
        // The idle reminder while already idle changes nothing.
        assert!(m.on_notification(t0 + ms(70_000), NoteClass::InputWait).is_empty());
        assert_eq!(m.state, AgentState::Idle);
    }

    #[test]
    fn exit_codes() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        m.on_user_input(t0, true);
        m.on_activity(t0 + ms(10), 10);
        let fx = m.on_exit(t0 + ms(5000), Some(0));
        assert!(matches!(fx.as_slice(), [Effect::TurnFinished(_), Effect::Exited(Some(0))]));
        assert_eq!(m.state, AgentState::Done { exit: Some(0) });
        // Nothing moves a finished session.
        assert!(m.on_activity(t0 + ms(6000), 10).is_empty());
        assert!(m.on_exit(t0 + ms(7000), Some(1)).is_empty());

        let mut m = Machine::new(t0);
        m.on_exit(t0, Some(1));
        assert_eq!(m.state, AgentState::Error);
        let mut m = Machine::new(t0);
        m.on_exit(t0, Some(130));
        assert_eq!(m.state, AgentState::Done { exit: Some(130) }, "Ctrl-C is not an error");
        let mut m = Machine::new(t0);
        m.on_exit(t0, None);
        assert_eq!(m.state, AgentState::Done { exit: None });
    }

    #[test]
    fn deadlines() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        assert!(m.next_deadline().is_some());
        m.on_hook(t0, HookKind::Working, None);
        assert!(m.next_deadline().unwrap() > t0 + Duration::from_secs(60));
        m.on_exit(t0, Some(0));
        assert_eq!(m.next_deadline(), None);
    }
}
