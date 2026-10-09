use crate::tools::blocks::{format_duration, FinishedCommand};

/// Decides whether a finished command deserves a desktop notification and
/// posts it. Fed from OSC 133 blocks (see `BlockManager::take_finished`).
pub struct Notifier {
    /// Minimum command duration that triggers a notification
    /// (`notify_after_secs`; `<= 0` disables).
    threshold_secs: f64,
}

impl Notifier {
    pub fn new(threshold_secs: f64) -> Self {
        Self { threshold_secs }
    }

    /// Notify when the command ran at least the threshold and the user cannot
    /// already see its pane (window unfocused, or pane not visible).
    pub fn should_notify(&self, done: &FinishedCommand, user_sees_pane: bool) -> bool {
        self.threshold_secs > 0.0
            && !user_sees_pane
            && done.duration_ms as f64 >= self.threshold_secs * 1000.0
    }

    /// "Command finished: <cmd> (exit N, 12.3s)".
    pub fn message(done: &FinishedCommand) -> String {
        let cmd = done.command.trim();
        let cmd: String = if cmd.chars().count() > 80 {
            cmd.chars().take(79).chain(std::iter::once('\u{2026}')).collect()
        } else {
            cmd.to_string()
        };
        let cmd = if cmd.is_empty() { "(unknown)".to_string() } else { cmd };
        let exit = done.exit_code.map_or("?".to_string(), |c| c.to_string());
        format!("Command finished: {cmd} (exit {exit}, {})", format_duration(done.duration_ms))
    }

    /// Send a desktop notification.
    pub fn send(title: &str, message: &str) {
        log::info!("Notification: {} — {}", title, message);

        #[cfg(target_os = "macos")]
        {
            let script = format!(
                "display notification \"{}\" with title \"{}\" sound name \"Glass\"",
                message.replace('\\', "\\\\").replace('"', "\\\""),
                title.replace('\\', "\\\\").replace('"', "\\\""),
            );
            let _ = std::process::Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .spawn();
        }

        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("notify-send")
                .arg(title)
                .arg(message)
                .spawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(cmd: &str, exit: Option<i32>, ms: u64) -> FinishedCommand {
        FinishedCommand { command: cmd.into(), exit_code: exit, duration_ms: ms }
    }

    #[test]
    fn threshold_and_visibility() {
        let n = Notifier::new(10.0);
        assert!(n.should_notify(&done("make", Some(0), 10_000), false));
        assert!(!n.should_notify(&done("make", Some(0), 9_999), false));
        assert!(!n.should_notify(&done("make", Some(0), 60_000), true), "visible pane: no toast");
        assert!(!Notifier::new(0.0).should_notify(&done("make", Some(0), 60_000), false), "0 disables");
    }

    #[test]
    fn message_format() {
        assert_eq!(Notifier::message(&done("cargo build", Some(101), 12_300)), "Command finished: cargo build (exit 101, 12.3s)");
        assert_eq!(Notifier::message(&done("  ", None, 15_000)), "Command finished: (unknown) (exit ?, 15.0s)");
        let long = "x".repeat(200);
        assert!(Notifier::message(&done(&long, Some(0), 15_000)).chars().count() < 120);
    }

    #[test]
    fn osc_block_completion_feeds_notifier() {
        let mut m = crate::tools::blocks::BlockManager::new();
        m.on_prompt_start(0);
        m.on_command_start(0, 2);
        m.on_command_text("sleep 20".into());
        m.on_command_output(1, "sleep 20".into());
        m.on_command_finished(3, Some(0));
        let f = m.take_finished();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].command, "sleep 20");
        assert!(m.take_finished().is_empty());
    }
}
