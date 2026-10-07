use std::time::Instant;

/// Tracks command execution and sends desktop notifications when long-running
/// commands complete while the terminal window is not focused.
pub struct Notifier {
    last_output: Instant,
    command_started: Option<Instant>,
    threshold_secs: f64,
}

impl Notifier {
    pub fn new() -> Self {
        Self {
            last_output: Instant::now(),
            command_started: None,
            threshold_secs: 5.0,
        }
    }

    /// Call whenever the PTY produces output.
    pub fn on_output(&mut self) {
        if self.command_started.is_none() {
            self.command_started = Some(Instant::now());
        }
        self.last_output = Instant::now();
    }

    /// Periodically check whether a notification should fire.
    /// Returns `true` if a notification was triggered (caller should call `send`).
    pub fn check(&mut self, is_window_focused: bool) -> bool {
        if is_window_focused {
            self.command_started = None;
            return false;
        }

        if let Some(start) = self.command_started {
            let elapsed = start.elapsed().as_secs_f64();
            let idle = self.last_output.elapsed().as_secs_f64();

            if elapsed > self.threshold_secs && idle > 1.5 {
                self.command_started = None;
                return true;
            }
        }
        false
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
