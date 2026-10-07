use std::time::Instant;

/// A completed command block — one command + its output.
pub struct CommandBlock {
    pub command: String,
    pub output_start: usize,
    pub output_end: usize,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub collapsed: bool,
    pub timestamp: u64,
}

/// Tracks command boundaries in the terminal output stream.
pub struct BlockManager {
    blocks: Vec<CommandBlock>,
    current: Option<PendingBlock>,
    pub enabled: bool,
}

struct PendingBlock {
    command: String,
    start_line: usize,
    started_at: Instant,
}

impl BlockManager {
    pub fn new() -> Self {
        Self { blocks: Vec::new(), current: None, enabled: true }
    }

    /// Called when the user submits input to the PTY.
    pub fn on_input(&mut self, command: &str, scrollback_line: usize) {
        if !self.enabled { return; }
        let cmd = command.trim();
        if cmd.is_empty() { return; }

        // If there's already a pending block, finalize it (no prompt detected).
        self.finalize_current(scrollback_line);

        self.current = Some(PendingBlock {
            command: cmd.to_string(),
            start_line: scrollback_line + 1,
            started_at: Instant::now(),
        });
    }

    /// Called for each line of PTY output. Detects prompt lines to finalize blocks.
    pub fn on_output_line(&mut self, line: &str, scrollback_line: usize) {
        if !self.enabled { return; }
        if self.current.is_none() { return; }

        if Self::looks_like_prompt(line) {
            self.finalize_current(scrollback_line);
        }
    }

    /// Retrieve all completed blocks.
    pub fn blocks(&self) -> &[CommandBlock] {
        &self.blocks
    }

    /// Get the most recent block (for HUD display).
    pub fn last_block(&self) -> Option<&CommandBlock> {
        self.blocks.last()
    }

    /// Is a command currently running?
    pub fn is_running(&self) -> bool {
        self.current.is_some()
    }

    /// Elapsed time of the currently running command.
    pub fn running_elapsed_ms(&self) -> Option<u64> {
        self.current.as_ref().map(|p| p.started_at.elapsed().as_millis() as u64)
    }

    pub fn toggle_collapse(&mut self, index: usize) {
        if let Some(b) = self.blocks.get_mut(index) {
            b.collapsed = !b.collapsed;
        }
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Find which block (if any) a given scrollback line belongs to.
    pub fn block_at_line(&self, line: usize) -> Option<(usize, &CommandBlock)> {
        for (i, b) in self.blocks.iter().enumerate().rev() {
            if line >= b.output_start && line <= b.output_end {
                return Some((i, b));
            }
        }
        None
    }

    /// Find the block whose command starts at the given line.
    pub fn block_starting_at(&self, line: usize) -> Option<(usize, &CommandBlock)> {
        for (i, b) in self.blocks.iter().enumerate() {
            if b.output_start == line + 1 {
                return Some((i, b));
            }
        }
        None
    }

    // ── Private ──

    fn finalize_current(&mut self, end_line: usize) {
        if let Some(pending) = self.current.take() {
            let block = CommandBlock {
                command: pending.command,
                output_start: pending.start_line,
                output_end: end_line.saturating_sub(1),
                exit_code: None,
                duration_ms: pending.started_at.elapsed().as_millis() as u64,
                collapsed: false,
                timestamp: unix_now(),
            };
            self.blocks.push(block);
        }
    }

    fn looks_like_prompt(line: &str) -> bool {
        let t = line.trim();
        if t.is_empty() { return false; }

        // Common prompt endings
        if t.ends_with("$ ") || t.ends_with("% ")
            || t.ends_with("❯ ") || t.ends_with("> ")
            || t.ends_with("# ")
            || t.ends_with("$") || t.ends_with("%")
            || t.ends_with("❯") || t.ends_with(">")
        {
            return true;
        }

        // user@host patterns
        if t.contains('@') && (t.ends_with('$') || t.ends_with('%') || t.ends_with('#')) {
            return true;
        }

        false
    }
}

impl CommandBlock {
    pub fn duration_display(&self) -> String {
        format_duration(self.duration_ms)
    }

    pub fn is_success(&self) -> bool {
        self.exit_code.map_or(true, |c| c == 0)
    }
}

pub fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < 3_600_000 {
        format!("{}m {}s", ms / 60_000, (ms % 60_000) / 1000)
    } else {
        let h = ms / 3_600_000;
        let m = (ms % 3_600_000) / 60_000;
        format!("{}h {}m", h, m)
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
