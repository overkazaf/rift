use std::time::Instant;

/// A completed command block — one command + its output.
pub struct CommandBlock {
    pub command: String,
    /// First line of the prompt (OSC 133;A). Equals `output_start` for heuristic blocks.
    pub prompt_line: usize,
    /// Line where the user's command text begins (OSC 133;B).
    pub command_line: usize,
    pub output_start: usize,
    pub output_end: usize,
    /// True while the command is still executing (OSC 133;C seen, no D yet).
    pub running: bool,
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
    /// Exact tracking driven by OSC 133 marks (per terminal).
    osc: OscState,
}

#[derive(Default)]
struct OscState {
    /// Set once any OSC 133 sequence has been seen on this terminal.
    seen: bool,
    prompt_line: Option<usize>,
    cmd_start: Option<(usize, usize)>,
    running: Option<(CommandBlock, Instant)>,
}

const MAX_BLOCKS: usize = 2000;

struct PendingBlock {
    command: String,
    start_line: usize,
    started_at: Instant,
}

impl BlockManager {
    pub fn new() -> Self {
        Self { blocks: Vec::new(), current: None, enabled: true, osc: OscState::default() }
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

    /// Mutable access to the finished blocks (the headless screenshot
    /// renderer sets timing fields directly instead of waiting in real time).
    pub fn blocks_mut(&mut self) -> &mut [CommandBlock] {
        &mut self.blocks
    }

    /// Is a command currently running?
    pub fn is_running(&self) -> bool {
        self.current.is_some() || self.osc.running.is_some()
    }

    /// Elapsed time of the currently running command.
    pub fn running_elapsed_ms(&self) -> Option<u64> {
        if let Some((_, t)) = &self.osc.running {
            return Some(t.elapsed().as_millis() as u64);
        }
        self.current.as_ref().map(|p| p.started_at.elapsed().as_millis() as u64)
    }

    pub fn toggle_collapse(&mut self, index: usize) {
        if let Some(b) = self.blocks.get_mut(index) {
            b.collapsed = !b.collapsed;
        }
    }

    /// Block by index as used by `block_at_line`: finished blocks first, the
    /// currently running OSC 133 block (if any) at `blocks().len()`.
    pub fn get(&self, index: usize) -> Option<&CommandBlock> {
        match self.blocks.get(index) {
            Some(b) => Some(b),
            None if index == self.blocks.len() => self.osc.running.as_ref().map(|(b, _)| b),
            None => None,
        }
    }

    /// Elapsed milliseconds of the running OSC 133 block (live duration).
    pub fn running_osc_elapsed_ms(&self) -> Option<u64> {
        self.osc.running.as_ref().map(|(_, t)| t.elapsed().as_millis() as u64)
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len() + self.osc.running.is_some() as usize
    }

    /// Find which block (if any) a given scrollback line belongs to.
    pub fn block_at_line(&self, line: usize) -> Option<(usize, &CommandBlock)> {
        if let Some((b, _)) = &self.osc.running {
            if line >= b.prompt_line {
                return Some((self.blocks.len(), b));
            }
        }
        for (i, b) in self.blocks.iter().enumerate().rev() {
            if line >= b.prompt_line.min(b.output_start) && line <= b.output_end {
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

    // ── OSC 133 (exact, shell-integration driven) ──

    /// True once the shell has emitted at least one OSC 133 mark; the
    /// heuristic tracker should then be ignored for this terminal.
    pub fn osc_seen(&self) -> bool {
        self.osc.seen
    }

    /// 133;A — prompt starts at `line`.
    pub fn on_prompt_start(&mut self, line: usize) {
        self.osc.seen = true;
        self.osc.prompt_line = Some(line);
        self.osc.cmd_start = None;
    }

    /// 133;B — prompt ended, user input starts at (`line`, `col`).
    pub fn on_command_start(&mut self, line: usize, col: usize) {
        self.osc.seen = true;
        self.osc.cmd_start = Some((line, col));
        // No (or a stale, later) A mark, e.g. a prompt drawn before hooks loaded.
        if self.osc.prompt_line.map_or(true, |p| p > line) {
            self.osc.prompt_line = Some(line);
        }
    }

    /// Position recorded by the last 133;B, if any.
    pub fn command_start_pos(&self) -> Option<(usize, usize)> {
        self.osc.cmd_start
    }

    /// 133;C — command accepted; its output starts at `line`.
    /// `command` is the text the terminal extracted between B and C.
    pub fn on_command_output(&mut self, line: usize, command: String) {
        self.osc.seen = true;
        if !self.enabled { return; }
        // A new C without a D: close the previous block unfinished.
        if self.osc.running.is_some() {
            self.on_command_finished(line, None);
        }
        let (cmd_line, _) = self.osc.cmd_start.take().unwrap_or((line.saturating_sub(1), 0));
        let prompt_line = self.osc.prompt_line.take().unwrap_or(cmd_line);
        let block = CommandBlock {
            command: command.trim().to_string(),
            prompt_line: prompt_line.min(cmd_line),
            command_line: cmd_line,
            output_start: line,
            output_end: line,
            running: true,
            exit_code: None,
            duration_ms: 0,
            collapsed: false,
            timestamp: unix_now(),
        };
        self.osc.running = Some((block, Instant::now()));
    }

    /// 133;D[;exit] — command finished. `cursor_line`/`cursor_col` give the
    /// cursor position at that moment so the last output line can be derived.
    pub fn on_command_finished(&mut self, end_line: usize, exit_code: Option<i32>) {
        self.osc.seen = true;
        if let Some((mut b, t)) = self.osc.running.take() {
            b.running = false;
            b.exit_code = exit_code;
            b.duration_ms = t.elapsed().as_millis() as u64;
            b.output_end = end_line.max(b.output_start.saturating_sub(1));
            self.blocks.push(b);
            if self.blocks.len() > MAX_BLOCKS {
                self.blocks.remove(0);
            }
        }
    }

    /// The terminal dropped `n` lines from the top of scrollback; keep all
    /// absolute line numbers in sync.
    pub fn shift_lines(&mut self, n: usize) {
        let sub = |v: &mut usize| *v = v.saturating_sub(n);
        for b in &mut self.blocks {
            sub(&mut b.prompt_line);
            sub(&mut b.command_line);
            sub(&mut b.output_start);
            sub(&mut b.output_end);
        }
        if let Some((b, _)) = &mut self.osc.running {
            sub(&mut b.prompt_line);
            sub(&mut b.command_line);
            sub(&mut b.output_start);
            sub(&mut b.output_end);
        }
        if let Some(p) = &mut self.osc.prompt_line { sub(p); }
        if let Some((l, _)) = &mut self.osc.cmd_start { sub(l); }
    }

    // ── Private ──

    fn finalize_current(&mut self, end_line: usize) {
        if let Some(pending) = self.current.take() {
            let block = CommandBlock {
                command: pending.command,
                prompt_line: pending.start_line,
                command_line: pending.start_line.saturating_sub(1),
                output_start: pending.start_line,
                output_end: end_line.saturating_sub(1),
                running: false,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc_block_lifecycle() {
        let mut m = BlockManager::new();
        m.on_prompt_start(3);
        m.on_command_start(3, 2);
        m.on_command_output(4, "ls".into());
        assert!(m.is_running());
        assert!(m.block_at_line(4).unwrap().1.running);
        m.on_command_finished(6, Some(2));
        let b = &m.blocks()[0];
        assert_eq!((b.prompt_line, b.command_line, b.output_start, b.output_end), (3, 3, 4, 6));
        assert!(!b.is_success());
        assert!(!m.is_running());
    }

    #[test]
    fn shift_keeps_lines_in_sync() {
        let mut m = BlockManager::new();
        m.on_prompt_start(10);
        m.on_command_start(10, 0);
        m.on_command_output(11, "x".into());
        m.on_command_finished(12, Some(0));
        m.shift_lines(5);
        assert_eq!(m.blocks()[0].output_start, 6);
    }
}
