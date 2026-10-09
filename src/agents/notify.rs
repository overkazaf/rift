//! Desktop notifications and the dock badge for agents that need you.
//!
//! Policy (pure, tested): a notification is only worth posting when the user
//! cannot see the pane, and it is rate limited per pane and globally so a
//! chatty agent can never spam the desktop.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Why a notification fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    NeedsUser,
    TurnFinished,
}

/// Same pane + same reason at most this often.
pub const MIN_GAP: Duration = Duration::from_secs(20);
/// Overall budget: at most `MAX_IN_WINDOW` notifications per `WINDOW`.
pub const WINDOW: Duration = Duration::from_secs(60);
pub const MAX_IN_WINDOW: usize = 6;
/// Turns shorter than this finish too quickly to be worth a ping.
pub const MIN_TURN: Duration = Duration::from_secs(8);

#[derive(Default)]
pub struct NotifyPolicy {
    last: HashMap<(usize, Reason), Instant>,
    recent: VecDeque<Instant>,
}

impl NotifyPolicy {
    /// May a `reason` notification for `pane` go out now? Records it when yes.
    pub fn allow(&mut self, now: Instant, pane: usize, reason: Reason) -> bool {
        if let Some(t) = self.last.get(&(pane, reason)) {
            if now.saturating_duration_since(*t) < MIN_GAP {
                return false;
            }
        }
        while self.recent.front().is_some_and(|t| now.saturating_duration_since(*t) >= WINDOW) {
            self.recent.pop_front();
        }
        if self.recent.len() >= MAX_IN_WINDOW {
            return false;
        }
        self.last.insert((pane, reason), now);
        self.recent.push_back(now);
        if self.last.len() > 256 {
            self.last.retain(|_, t| now.saturating_duration_since(*t) < WINDOW);
        }
        true
    }
}

/// True when `reason` text asks for an approval / permission.
pub fn is_approval(reason: &str) -> bool {
    let r = reason.to_lowercase();
    ["approve", "approval", "permission", "trust", "confirm", "choose an option", "answer y"].iter().any(|w| r.contains(w))
}

/// "Claude Code in rift/main needs your approval".
pub fn compose(agent: &str, place: &str, reason: Reason, detail: Option<&str>) -> String {
    let who = if place.is_empty() { agent.to_string() } else { format!("{agent} in {place}") };
    match reason {
        Reason::NeedsUser => {
            if detail.map_or(true, is_approval) {
                format!("{who} needs your approval")
            } else {
                format!("{who} needs your input")
            }
        }
        Reason::TurnFinished => format!("{who} finished its turn"),
    }
}

fn escape_applescript(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect::<String>().replace('\\', "\\\\").replace('"', "\\\"")
}

/// Post a desktop notification (macOS: Notification Center via osascript;
/// Linux: notify-send). Clicking it is not wired up.
pub fn post(title: &str, body: &str, sound: bool) {
    log::info!("Agent notification: {title} - {body}");
    #[cfg(target_os = "macos")]
    {
        let snd = if sound { " sound name \"Glass\"" } else { "" };
        let script = format!(
            "display notification \"{}\" with title \"{}\"{snd}",
            escape_applescript(body),
            escape_applescript(title)
        );
        let _ = std::process::Command::new("osascript").arg("-e").arg(script).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = sound;
        let _ = std::process::Command::new("notify-send").arg(title).arg(body).spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (title, body, sound, escape_applescript(""));
    }
}

/// Show `count` on the dock icon (0 clears it). macOS only.
pub fn set_dock_badge(count: usize) {
    #[cfg(target_os = "macos")]
    crate::platform::macos::set_dock_badge(count);
    #[cfg(not(target_os = "macos"))]
    let _ = count;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn dedupes_per_pane_and_reason() {
        let t0 = Instant::now();
        let mut p = NotifyPolicy::default();
        assert!(p.allow(t0, 1, Reason::NeedsUser));
        assert!(!p.allow(t0 + ms(5000), 1, Reason::NeedsUser), "same pane + reason inside the gap");
        assert!(p.allow(t0 + ms(5000), 2, Reason::NeedsUser), "other pane is independent");
        assert!(p.allow(t0 + ms(5000), 1, Reason::TurnFinished), "other reason is independent");
        assert!(p.allow(t0 + MIN_GAP + ms(1), 1, Reason::NeedsUser), "gap elapsed");
    }

    #[test]
    fn global_budget() {
        let t0 = Instant::now();
        let mut p = NotifyPolicy::default();
        for pane in 0..MAX_IN_WINDOW {
            assert!(p.allow(t0 + ms(pane as u64), pane, Reason::NeedsUser));
        }
        assert!(!p.allow(t0 + ms(100), 99, Reason::NeedsUser), "budget exhausted");
        assert!(p.allow(t0 + WINDOW + ms(10), 99, Reason::NeedsUser), "window slid");
    }

    #[test]
    fn messages() {
        assert_eq!(compose("Claude Code", "rift/main", Reason::NeedsUser, Some("approve tool use")), "Claude Code in rift/main needs your approval");
        assert_eq!(compose("Codex", "api/fix", Reason::NeedsUser, Some("waiting for your input")), "Codex in api/fix needs your input");
        assert_eq!(compose("Aider", "", Reason::NeedsUser, None), "Aider needs your approval");
        assert_eq!(compose("Gemini CLI", "x/y", Reason::TurnFinished, None), "Gemini CLI in x/y finished its turn");
        assert!(is_approval("Claude needs your permission to use Bash"));
        assert!(!is_approval("waiting for your input"));
    }

    #[test]
    fn applescript_escaping() {
        assert_eq!(escape_applescript("a \"b\" \\ c\n"), "a \\\"b\\\" \\\\ c");
    }
}
