use crate::pty::Pty;
use crate::tools::recording::Recorder;
use crate::network::ssh::SshPty;
use crate::terminal::{AnsiHandler, Terminal};
use std::time::{Duration, Instant};
use vte::{Params, Perform};
use winit::event_loop::EventLoopProxy;

/// Parse time one `process_output` call may spend before yielding to the event
/// loop (rendering, input). Remaining output stays queued (bounded by the PTY
/// channel, which throttles the child) and `about_to_wait` re-enters immediately.
#[allow(dead_code)] // used by Pane::process_output (audit/tests)
pub const PARSE_BUDGET: Duration = Duration::from_millis(6);
/// Hard cap on bytes parsed per call, a second guard for slow-clock environments.
const MAX_BYTES_PER_CALL: usize = 4 << 20;

/// Parse `data` in slices, stopping once `deadline` has passed; returns the
/// number of bytes consumed (the whole slice unless time ran out).
fn feed_until(terminal: &mut Terminal, parser: &mut vte::Parser, data: &[u8], deadline: Instant) -> usize {
    const SLICE: usize = 8 * 1024;
    let mut used = 0;
    while used < data.len() {
        let end = (used + SLICE).min(data.len());
        feed_into(terminal, parser, &data[used..end]);
        used = end;
        if used < data.len() && Instant::now() >= deadline {
            break;
        }
    }
    used
}

/// `Perform` wrapper that reports whether the VT parser is back in its ground
/// state after the byte just fed. That is what makes the bulk-ASCII fast path
/// sound: following a `print`/CSI/ESC dispatch the parser is in ground, so a
/// run of printable ASCII needs no per-byte state-machine work.
struct FeedHandler<'a> {
    inner: AnsiHandler<'a>,
    ground: bool,
}

impl Perform for FeedHandler<'_> {
    #[inline]
    fn print(&mut self, c: char) {
        self.ground = c.is_ascii();
        self.inner.print(c);
    }
    #[inline]
    fn execute(&mut self, byte: u8) {
        self.inner.execute(byte);
    }
    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.inner.hook(params, intermediates, ignore, action);
    }
    fn put(&mut self, byte: u8) {
        self.inner.put(byte);
    }
    fn unhook(&mut self) {
        self.inner.unhook();
    }
    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.inner.osc_dispatch(params, bell_terminated);
    }
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.ground = true;
        self.inner.csi_dispatch(params, intermediates, ignore, action);
    }
    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.ground = true;
        self.inner.esc_dispatch(intermediates, ignore, byte);
    }
}

#[inline]
fn is_printable_ascii(b: u8) -> bool {
    (0x20..0x7f).contains(&b)
}

/// Run `data` through the VT parser (+ kitty APC scanner) into `terminal`.
fn feed_into(terminal: &mut Terminal, parser: &mut vte::Parser, data: &[u8]) {
    // One handler per chunk (not per byte).
    let mut h = FeedHandler { inner: AnsiHandler::new(terminal), ground: false };
    let n = data.len();
    let mut i = 0;
    while i < n {
        let byte = data[i];
        i += 1;
        h.ground = false;
        // Kitty graphics protocol data travels inside an APC string
        // (`ESC _ ... ST`), which `vte`'s Perform trait has no
        // callback for. Scan for it in parallel — see
        // AnsiHandler::feed_apc_byte for details.
        if byte == 0x1b || h.inner.terminal.apc_active() {
            h.inner.feed_apc_byte(byte);
        }
        parser.advance(&mut h, byte);
        // Fast path: parser is back in ground state, so a run of printable
        // ASCII can be written into the row in bulk (the APC scanner ignores
        // non-ESC bytes while idle, which it is in ground state).
        if h.ground && i < n && is_printable_ascii(data[i]) && h.inner.terminal.ascii_fast_ok() {
            let mut j = i + 1;
            while j < n && is_printable_ascii(data[j]) {
                j += 1;
            }
            h.inner.terminal.print_ascii_run(&data[i..j]);
            i = j;
        }
    }
}

pub enum PtyKind {
    Local(Pty),
    Ssh(SshPty),
    /// No process behind the pane: reads yield nothing and writes are dropped.
    /// Used by the headless screenshot renderer, which feeds bytes via
    /// [`Pane::feed`].
    Inert,
}

impl PtyKind {
    pub fn try_read(&self) -> Option<Vec<u8>> {
        match self {
            PtyKind::Local(p) => p.try_read(),
            PtyKind::Ssh(s) => s.try_read(),
            PtyKind::Inert => None,
        }
    }

    /// Exit code of the local shell once it has exited (None while running / for SSH).
    pub fn exit_status(&self) -> Option<u32> {
        match self {
            PtyKind::Local(p) => p.exit_status(),
            _ => None,
        }
    }

    pub fn write(&mut self, data: &[u8]) {
        match self {
            PtyKind::Local(p) => p.write(data),
            PtyKind::Ssh(s) => s.write(data),
            PtyKind::Inert => {}
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        match self {
            PtyKind::Local(p) => p.resize(cols, rows, 0, 0),
            PtyKind::Ssh(s) => s.resize(cols, rows),
            PtyKind::Inert => {}
        }
    }
}

#[allow(dead_code)]
pub struct Pane {
    pub id: usize,
    pub terminal: Terminal,
    pub pty: PtyKind,
    pub vt_parser: vte::Parser,
    pub recorder: Option<Recorder>,
    pub label: Option<String>,
    /// Output is still queued after the last `process_output` hit its budget.
    pub backlog: bool,
    /// Unparsed remainder of a PTY chunk when the time budget ran out mid-chunk.
    carry: Vec<u8>,
    carry_pos: usize,
    /// Set once the child exited and "[process exited N]" was printed.
    pub exited: Option<u32>,
}

impl Pane {
    pub fn new(id: usize, cols: usize, rows: usize, proxy: EventLoopProxy<()>) -> Self {
        Self {
            id,
            terminal: Terminal::new(cols, rows),
            pty: PtyKind::Local(Pty::spawn(cols as u16, rows as u16, proxy)),
            vt_parser: vte::Parser::new(),
            recorder: None,
            label: None,
            backlog: false,
            carry: Vec::new(),
            carry_pos: 0,
            exited: None,
        }
    }

    /// Local pane whose shell starts in `cwd` (when it is an existing directory).
    pub fn new_in(id: usize, cols: usize, rows: usize, proxy: EventLoopProxy<()>, cwd: Option<&str>) -> Self {
        Self {
            id,
            terminal: Terminal::new(cols, rows),
            pty: PtyKind::Local(Pty::spawn_in(cols as u16, rows as u16, proxy, cwd)),
            vt_parser: vte::Parser::new(),
            recorder: None,
            label: None,
            backlog: false,
            carry: Vec::new(),
            carry_pos: 0,
            exited: None,
        }
    }

    pub fn from_ssh(id: usize, cols: usize, rows: usize, ssh: SshPty) -> Self {
        let label = ssh.config.display_name();
        Self {
            id,
            terminal: Terminal::new(cols, rows),
            pty: PtyKind::Ssh(ssh),
            vt_parser: vte::Parser::new(),
            recorder: None,
            label: Some(label),
            backlog: false,
            carry: Vec::new(),
            carry_pos: 0,
            exited: None,
        }
    }

    /// Pane with no process behind it (see [`PtyKind::Inert`]). Content is
    /// supplied with [`Pane::feed`]; anything written to it is dropped.
    pub fn scripted(id: usize, cols: usize, rows: usize) -> Self {
        Self {
            id,
            terminal: Terminal::new(cols, rows),
            pty: PtyKind::Inert,
            vt_parser: vte::Parser::new(),
            recorder: None,
            label: None,
            backlog: false,
            carry: Vec::new(),
            carry_pos: 0,
            exited: None,
        }
    }

    /// Run `data` through the VT parser exactly as PTY output would be.
    pub fn feed(&mut self, data: &[u8]) {
        feed_into(&mut self.terminal, &mut self.vt_parser, data);
    }

    /// Parse queued PTY output for at most [`PARSE_BUDGET`]. Returns true if
    /// anything was processed; check [`Pane::backlog`] to see whether more is waiting.
    #[allow(dead_code)]
    pub fn process_output(&mut self) -> bool {
        self.process_output_budget(PARSE_BUDGET)
    }

    pub fn process_output_budget(&mut self, budget: Duration) -> bool {
        let start = Instant::now();
        let deadline = start + budget;
        let mut bytes = 0usize;
        let mut more = false;
        loop {
            // Unparsed tail of the previous pass first, then fresh PTY data.
            let (data, fresh) = if self.carry_pos < self.carry.len() {
                (std::mem::take(&mut self.carry), false)
            } else {
                match self.pty.try_read() {
                    Some(d) => (d, true),
                    None => break,
                }
            };
            let from = if fresh { 0 } else { self.carry_pos };
            self.carry_pos = 0;
            if fresh {
                if let Some(rec) = &mut self.recorder {
                    rec.record_output(&data);
                }
            }
            let used = feed_until(&mut self.terminal, &mut self.vt_parser, &data[from..], deadline);
            bytes += used;
            if from + used < data.len() {
                // Out of time mid-chunk: keep the rest for the next pass.
                self.carry = data;
                self.carry_pos = from + used;
                more = true;
                break;
            }
            if bytes >= MAX_BYTES_PER_CALL || Instant::now() >= deadline {
                more = true;
                break;
            }
        }
        self.backlog = more;
        let mut changed = bytes > 0;

        // Child exit: announce once the output is fully drained.
        if !more && self.exited.is_none() {
            if let Some(code) = self.pty.exit_status() {
                self.exited = Some(code);
                let msg = format!("\r\n\x1b[0m\x1b[2m[process exited {code}] press Enter to close\x1b[0m\r\n");
                self.feed(msg.as_bytes());
                changed = true;
            }
        }
        changed
    }

    pub fn write(&mut self, data: &[u8]) {
        if let Some(rec) = &mut self.recorder {
            rec.record_input(data);
        }
        self.pty.write(data);
    }

    pub fn flush_responses(&mut self) {
        for response in self.terminal.response_queue.drain(..) {
            self.pty.write(&response);
        }
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        if cols != self.terminal.cols || rows != self.terminal.rows {
            self.terminal.resize(cols, rows);
            self.pty.resize(cols as u16, rows as u16);
        }
    }

    pub fn title(&self) -> Option<&str> {
        if let Some(ref l) = self.label {
            return Some(l);
        }
        self.terminal.title.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Reference path: straight per-byte parse, no bulk-ASCII fast path.
    fn feed_reference(p: &mut Pane, data: &[u8]) {
        let mut h = AnsiHandler::new(&mut p.terminal);
        for &b in data {
            h.feed_apc_byte(b);
            p.vt_parser.advance(&mut h, b);
        }
    }

    fn grid_text(p: &Pane) -> Vec<String> {
        p.terminal.grid.iter().map(|r| r.iter().map(|c| c.c).collect()).collect()
    }

    fn snapshot(p: &Pane) -> (Vec<String>, usize, usize, usize) {
        (grid_text(p), p.terminal.cursor_row, p.terminal.cursor_col, p.terminal.scrollback.len())
    }

    #[test]
    fn ascii_fast_path_matches_per_byte_parse() {
        // xorshift soup of printable runs, wide chars, CR/LF/BS/TAB, SGR and cursor moves, at several widths.
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let pieces: [&[u8]; 14] = [
            b"hello world", b"x", b"0123456789abcdefghijklmnopqrstuvwxyz", b"\r\n", b"\r", b"\n", b"\x08", b"\t",
            "中文".as_bytes(), b"\x1b[31m", b"\x1b[0m", b"\x1b[3;7H", b"\x1b[2K", b"\x1b[1;3r",
        ];
        for (cols, rows) in [(1usize, 1usize), (2, 3), (7, 4), (20, 5), (80, 24)] {
            let mut data = Vec::new();
            for _ in 0..3000 {
                data.extend_from_slice(pieces[(next() % pieces.len() as u64) as usize]);
            }
            let mut fast = Pane::scripted(0, cols, rows);
            let mut slow = Pane::scripted(1, cols, rows);
            // Feed in odd-sized slices so run boundaries land mid-sequence.
            for chunk in data.chunks(37) {
                fast.feed(chunk);
                feed_reference(&mut slow, chunk);
            }
            assert_eq!(snapshot(&fast), snapshot(&slow), "diverged at {cols}x{rows}");
        }
    }

    #[test]
    fn real_shell_exit_is_reaped_and_announced() {
        let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("echo hi-from-child; exit 3");
        let pty = Pty::spawn_cmd(80, 24, cmd, Arc::new(|| {})).unwrap();
        let mut pane = Pane::scripted(0, 80, 24);
        pane.pty = PtyKind::Local(pty);
        let t0 = Instant::now();
        while pane.exited.is_none() && t0.elapsed() < Duration::from_secs(10) {
            pane.process_output();
            std::thread::sleep(Duration::from_millis(10));
        }
        let text = grid_text(&pane).join("\n");
        assert_eq!(pane.exited, Some(3), "screen:\n{text}");
        assert!(text.contains("hi-from-child"), "{text}");
        assert!(text.contains("[process exited 3]"), "{text}");
        match &pane.pty {
            PtyKind::Local(p) => assert!(p.is_reaped()),
            _ => unreachable!(),
        }
    }

    #[test]
    fn budgeted_pass_yields_and_resumes_without_losing_bytes() {
        // A flood that cannot be parsed within 1 ms must be split across passes, in order.
        let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("i=0; while [ $i -lt 4000 ]; do echo line-$i; i=$((i+1)); done; echo END-MARK");
        let pty = Pty::spawn_cmd(100, 30, cmd, Arc::new(|| {})).unwrap();
        let mut pane = Pane::scripted(0, 100, 30);
        pane.pty = PtyKind::Local(pty);
        let t0 = Instant::now();
        let mut passes = 0;
        while pane.exited.is_none() && t0.elapsed() < Duration::from_secs(20) {
            pane.process_output_budget(Duration::from_micros(200));
            passes += 1;
        }
        let mut all: Vec<String> = pane.terminal.scrollback.iter().map(|r| r.iter().map(|c| c.c).collect::<String>()).collect();
        all.extend(grid_text(&pane));
        let joined = all.join("\n");
        assert!(joined.contains("line-0\n") || joined.contains("line-0 "), "first line lost");
        assert!(joined.contains("line-3999"), "last numbered line lost");
        assert!(joined.contains("END-MARK"));
        assert!(passes > 1);
    }
}
