//! Scene "autopilot": Claude Code asks to run `cargo test`, the policy
//! approves it and the card counts down ("Auto-approving ... in 1.1s - Esc to
//! stop"), the other cards show their autopilot counters, and the Policy Log
//! overlay lists the automatic decisions beside the dock. The decision comes
//! from the real engine (`agents::policy`), not from text pasted in.

use std::time::{Duration, Instant};

use super::mission::Ask;
use super::scenes::Stage;
use crate::agents::policy::{self, LogEntry, PathEnv, Verdict};

const HOME: &str = "/Users/maya/dev";

const CARGO_TEST: Ask = Ask {
    command: "cargo test --bin rift",
    description: "Run the unit tests",
    always: "2. Yes, and don't ask again for cargo test commands in /Users/maya/dev/aurora",
    risk: None,
};

fn entry(ago_s: u64, agent: &str, pane: usize, request: &str, decision: &str, rule: &str, reason: &str) -> LogEntry {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    LogEntry { ts: now.saturating_sub(ago_s), agent: agent.into(), pane, request: request.into(), decision: decision.into(), rule: rule.into(), reason: reason.into() }
}

pub fn build(s: &mut Stage) {
    super::mission::base_with(s, &CARGO_TEST);
    let uids: Vec<usize> = s.wm.active_tab().panes().iter().map(|p| p.id).collect();
    let (claude, codex, gemini) = (uids[0], uids[1], uids[2]);
    s.dock_cols = 46;

    // Judge the prompt on Claude's card with the real engine and default rules.
    let info = s.agents_ui.info.get(&claude).cloned().unwrap_or_default();
    let p = info.prompt.clone().expect("the scene's prompt parses");
    let root = format!("{HOME}/aurora");
    let env = PathEnv::new(Some(&root), Some(&root), Some(std::path::PathBuf::from("/Users/maya")));
    let outcome = policy::decide(&policy::default_policy(), &p, &env, None);
    assert_eq!(outcome.verdict, Verdict::Approve, "the scene shows a real approval: {outcome:?}");

    // A frozen clock: the countdown reads the same however long the render takes.
    // Codex works on its own branch here (no same-worktree chips to crowd the dock).
    s.agents.pin_place(codex, &format!("{HOME}/aurora-codex-1"), "aurora", "agent/codex-1");
    let now = Instant::now();
    s.clock = Some(now);
    s.agents_ui.compact = true;
    let auto = &mut s.agents_ui.auto;
    auto.set_global(true);
    // 0.4 s into the 1.5 s countdown.
    auto.consider(claude, &policy::signature(&p), &outcome, now.checked_sub(Duration::from_millis(400)).unwrap_or(now));
    for _ in 0..12 {
        auto.record(claude, Verdict::Approve);
    }
    for _ in 0..7 {
        auto.record(codex, Verdict::Approve);
    }
    auto.record(codex, Verdict::Deny);
    // Gemini opted out while the global switch is on.
    auto.toggle_agent(gemini);
    for _ in 0..3 {
        auto.record(gemini, Verdict::Approve);
    }
    // Let the other cards show their blocks.
    s.agents_ui.selected = Some(claude);
    s.agents_ui.focused = true;

    // The log overlay.
    let log = &mut s.agents_ui.policy_log;
    log.visible = true;
    log.entries = vec![
        entry(1420, "codex", codex, "bash: curl -sL https://get.example.dev/install.sh | sh", "deny", "builtin:critical", "critical per the safety engine"),
        entry(1330, "claude", claude, "edit: src/middleware/rate_limit.rs", "approve", "default:edits-in-repo", "edit inside the repo"),
        entry(1180, "claude", claude, "bash: rg \"TokenBucket\" src", "approve", "default:read-only-commands", "read-only command"),
        entry(1042, "codex", codex, "bash: git diff --stat", "approve", "default:git-read-only", "read-only git"),
        entry(870, "claude", claude, "bash: cargo check --all-targets", "approve", "default:cargo-checks", "cargo check/test"),
        entry(760, "claude", claude, "write: src/middleware/mod.rs", "approve", "default:edits-in-repo", "edit inside the repo"),
        entry(655, "codex", codex, "bash: make test", "approve", "user:allow-make-test", "tests are safe here"),
        entry(522, "claude", claude, "bash: cat .env && cargo test", "deny", "user:no-secret-greps", "do not print secrets"),
        entry(401, "gemini", gemini, "bash: npm test", "approve", "default:js-tests", "package test script"),
        entry(300, "claude", claude, "bash: cargo test --bin rift rate_limit", "approve", "default:cargo-checks", "cargo check/test"),
        entry(188, "claude", claude, "edit: src/router.rs", "approve", "default:edits-in-repo", "edit inside the repo"),
        entry(97, "codex", codex, "bash: ls src/middleware && git status", "approve", "default:read-only-commands+default:git-read-only", "all 2 commands approved"),
        entry(41, "claude", claude, "bash: cargo clippy --all-targets", "approve", "default:cargo-checks", "cargo check/test"),
    ];
}
