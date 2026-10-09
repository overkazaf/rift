//! Per-agent task queue: tasks wait here and are sent one at a time when the
//! agent is idle after a turn. Pure data (editing, reordering) plus the
//! countdown that gives the user 3 seconds to cancel the next send.

use std::time::{Duration, Instant};

/// Time between "next task" appearing and it being sent.
pub const COUNTDOWN: Duration = Duration::from_secs(3);
/// Tasks kept per agent.
pub const MAX_TASKS: usize = 50;
/// Characters kept per task (a guard against pasting a whole file).
pub const MAX_TASK_CHARS: usize = 8000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskQueue {
    pub tasks: Vec<String>,
    /// Auto-send is off (cancelled countdown or the user paused it).
    pub paused: bool,
}

impl TaskQueue {
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Add a task (trimmed). Returns false for blank text or a full queue.
    pub fn push(&mut self, text: &str) -> bool {
        let t: String = text.trim().chars().take(MAX_TASK_CHARS).collect();
        if t.is_empty() || self.tasks.len() >= MAX_TASKS {
            return false;
        }
        self.tasks.push(t);
        true
    }

    /// Replace task `i`.
    pub fn set(&mut self, i: usize, text: &str) -> bool {
        let t: String = text.trim().chars().take(MAX_TASK_CHARS).collect();
        match self.tasks.get_mut(i) {
            Some(slot) if !t.is_empty() => {
                *slot = t;
                true
            }
            _ => false,
        }
    }

    pub fn remove(&mut self, i: usize) -> Option<String> {
        (i < self.tasks.len()).then(|| self.tasks.remove(i))
    }

    /// Move task `i` one place earlier; returns its new index.
    pub fn move_up(&mut self, i: usize) -> usize {
        if i > 0 && i < self.tasks.len() {
            self.tasks.swap(i, i - 1);
            i - 1
        } else {
            i
        }
    }

    pub fn move_down(&mut self, i: usize) -> usize {
        if i + 1 < self.tasks.len() {
            self.tasks.swap(i, i + 1);
            i + 1
        } else {
            i
        }
    }

    pub fn pop_front(&mut self) -> Option<String> {
        (!self.tasks.is_empty()).then(|| self.tasks.remove(0))
    }

    pub fn clear(&mut self) {
        self.tasks.clear();
    }
}

/// First line of a task plus how many more there are: "fix the login test (+2 lines)".
pub fn preview(task: &str, max_chars: usize) -> String {
    let mut lines = task.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().unwrap_or("").trim();
    let more = lines.count();
    let suffix = if more > 0 { format!(" (+{more} line{})", if more == 1 { "" } else { "s" }) } else { String::new() };
    let room = max_chars.saturating_sub(suffix.chars().count()).max(1);
    let head = crate::ui::kit::ellipsize(first, room);
    format!("{head}{suffix}")
}

/// A running 3 second countdown before a queued task is sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Countdown {
    pub uid: usize,
    pub task: String,
    pub fires_at: Instant,
}

impl Countdown {
    pub fn start(uid: usize, task: &str, now: Instant) -> Countdown {
        Countdown { uid, task: task.to_string(), fires_at: now + COUNTDOWN }
    }

    pub fn due(&self, now: Instant) -> bool {
        now >= self.fires_at
    }

    /// Whole seconds left, rounded up (3, 2, 1).
    pub fn secs_left(&self, now: Instant) -> u64 {
        let left = self.fires_at.saturating_duration_since(now);
        (left.as_millis() as u64 + 999) / 1000
    }
}

// ───────────────────────────── persistence ─────────────────────────────

/// Queues of a tab by pane position (in-order leaf index): what the session
/// file stores, since pane ids change between runs.
pub type LeafQueues = Vec<(usize, TaskQueue)>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_edit_remove_reorder() {
        let mut q = TaskQueue::default();
        assert!(q.push("  first  "));
        assert!(q.push("second\nwith two lines"));
        assert!(q.push("third"));
        assert!(!q.push("   \n "), "blank tasks are refused");
        assert_eq!(q.tasks, ["first", "second\nwith two lines", "third"]);
        assert_eq!(q.move_up(2), 1);
        assert_eq!(q.tasks, ["first", "third", "second\nwith two lines"]);
        assert_eq!(q.move_up(0), 0, "first stays first");
        assert_eq!(q.move_down(1), 2);
        assert_eq!(q.move_down(2), 2, "last stays last");
        assert!(q.set(0, " FIRST "));
        assert!(!q.set(0, "  "));
        assert!(!q.set(9, "x"));
        assert_eq!(q.remove(1).as_deref(), Some("second\nwith two lines"));
        assert_eq!(q.remove(5), None);
        assert_eq!(q.pop_front().as_deref(), Some("FIRST"));
        assert_eq!(q.len(), 1);
        q.clear();
        assert!(q.is_empty() && q.pop_front().is_none());
    }

    #[test]
    fn queue_is_bounded() {
        let mut q = TaskQueue::default();
        for i in 0..MAX_TASKS {
            assert!(q.push(&format!("t{i}")));
        }
        assert!(!q.push("one too many"));
        let mut q = TaskQueue::default();
        q.push(&"x".repeat(MAX_TASK_CHARS * 2));
        assert_eq!(q.tasks[0].chars().count(), MAX_TASK_CHARS);
    }

    #[test]
    fn previews() {
        assert_eq!(preview("fix it", 40), "fix it");
        assert_eq!(preview("fix it\n\nthen test it\nand ship", 40), "fix it (+2 lines)");
        assert_eq!(preview("a\nb", 40), "a (+1 line)");
        let p = preview(&"w".repeat(100), 20);
        assert!(p.chars().count() <= 20, "{p}");
        assert_eq!(preview("", 10), "");
    }

    #[test]
    fn countdown_runs_three_seconds() {
        let t0 = Instant::now();
        let c = Countdown::start(7, "do it", t0);
        assert!(!c.due(t0));
        assert_eq!(c.secs_left(t0), 3);
        assert_eq!(c.secs_left(t0 + Duration::from_millis(1100)), 2);
        assert_eq!(c.secs_left(t0 + Duration::from_millis(2999)), 1);
        assert!(!c.due(t0 + Duration::from_millis(2999)));
        assert!(c.due(t0 + COUNTDOWN));
        assert_eq!(c.secs_left(t0 + Duration::from_secs(9)), 0);
    }
}
