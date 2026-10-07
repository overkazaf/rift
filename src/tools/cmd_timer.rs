pub struct CommandTimer {
    pub enabled: bool,
    current_start: Option<std::time::Instant>,
    last_duration: Option<std::time::Duration>,
}

impl CommandTimer {
    pub fn new() -> Self {
        Self { enabled: true, current_start: None, last_duration: None }
    }

    pub fn command_started(&mut self) {
        self.current_start = Some(std::time::Instant::now());
    }

    pub fn command_finished(&mut self) {
        if let Some(start) = self.current_start.take() {
            self.last_duration = Some(start.elapsed());
        }
    }

    pub fn last_duration_text(&self) -> Option<String> {
        self.last_duration.map(|d| {
            let ms = d.as_millis();
            if ms < 1000 { format!("{}ms", ms) }
            else if ms < 60000 { format!("{:.1}s", ms as f64 / 1000.0) }
            else { format!("{}m{:.0}s", ms / 60000, (ms % 60000) as f64 / 1000.0) }
        })
    }

    pub fn is_running(&self) -> bool {
        self.current_start.is_some()
    }

    pub fn running_elapsed(&self) -> Option<std::time::Duration> {
        self.current_start.map(|s| s.elapsed())
    }
}
