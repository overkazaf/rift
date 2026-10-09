//! Shell integration marks: OSC 133 (FinalTerm semantic prompts), OSC 7 and
//! OSC 1337;CurrentDir. The shell side lives in `crate::shell_integration`.

use std::time::{Instant, SystemTime};

use super::{Cell, Terminal};

/// Maximum number of marks retained in `Terminal::marks`.
const MAX_MARKS: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum MarkKind {
    /// 133;A — prompt start
    PromptStart,
    /// 133;B — end of prompt / start of user input
    CommandStart,
    /// 133;C — command accepted, output starts
    OutputStart,
    /// 133;D[;exit] — command finished
    CommandEnd,
}

/// One semantic event (a read-only history API for consumers such as the
/// AI observer; `Terminal::blocks` is what the UI uses today), positioned at an absolute line
/// (`scrollback.len() + cursor_row` at the time it was received).
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct SemanticMark {
    pub kind: MarkKind,
    pub line: usize,
    pub col: usize,
    pub exit_code: Option<i32>,
    pub at: Instant,
    pub unix_time: u64,
}

impl Terminal {
    /// Absolute line of the cursor (scrollback + visible row).
    pub fn abs_cursor_line(&self) -> usize {
        self.scrollback.len() + self.cursor_row
    }

    fn abs_row(&self, abs: usize) -> Option<&Vec<Cell>> {
        let sb = self.scrollback.len();
        if abs < sb { self.scrollback.get(abs) } else { self.grid.get(abs - sb) }
    }

    /// Text from (`start_line`, `start_col`) up to (`end_line`, `end_col`)
    /// exclusive of the end position. Rows flagged as soft-wrapped (the
    /// terminal sets `Cell::wrap` on the last cell when auto-wrap moved on to
    /// the next row) are joined without a newline; every other row break is a
    /// real line break, so an exactly-full row followed by `\n` stays split.
    fn text_between(&self, start: (usize, usize), end: (usize, usize)) -> String {
        let mut out = String::new();
        let (sl, sc) = start;
        let (el, ec) = end;
        if el < sl { return out; }
        for line in sl..=el {
            let Some(row) = self.abs_row(line) else { break };
            let from = if line == sl { sc.min(row.len()) } else { 0 };
            let to = if line == el { ec.min(row.len()) } else { row.len() };
            if from >= to { continue; }
            let mut seg = String::new();
            for c in &row[from..to] {
                c.push_text(&mut seg);
            }
            let wrapped = line != el && row.last().map_or(false, |c| c.wrap());
            if wrapped {
                out.push_str(&seg);
            } else {
                out.push_str(seg.trim_end());
                if line != el { out.push('\n'); }
            }
        }
        out.trim().to_string()
    }

    /// The pending input as marked by OSC 133;B (first prompt) plus any
    /// continuation prompts (PS2) up to `end`: PS2 text is not part of the
    /// command, and the segments are joined with real newlines.
    fn marked_input(&self, end: (usize, usize)) -> String {
        let starts = self.blocks.input_starts();
        let mut parts = Vec::new();
        for (k, &start) in starts.iter().enumerate() {
            let seg_end = match starts.get(k + 1) {
                Some(&(l, _)) => (l, 0),
                None => end,
            };
            if seg_end < start { continue; }
            let t = self.text_between(start, seg_end);
            parts.push(strip_rprompt(&t).to_string());
        }
        parts.join("\n")
    }

    /// True while the shell sits at an editable prompt: OSC 133 is active, a
    /// `B` (end of prompt) mark is pending its `C`, no command is running and
    /// no full-screen program owns the screen.
    pub fn at_shell_prompt(&self) -> bool {
        !self.is_alt_screen()
            && self.blocks.osc_seen()
            && self.blocks.command_start_pos().is_some()
            && self.blocks.running_osc_elapsed_ms().is_none()
    }

    /// Text typed at the prompt, from the `B` mark up to the cursor (so
    /// ghost text such as zsh autosuggestions after the cursor is excluded).
    /// Multi-line input (continuation prompts) comes back as one string with
    /// `\n` separators and without the PS2 prompts. `None` when not at a prompt.
    pub fn typed_input(&self) -> Option<String> {
        if !self.at_shell_prompt() {
            return None;
        }
        let start = self.blocks.command_start_pos()?;
        let end = (self.abs_cursor_line(), self.cursor_col);
        if end < start {
            return Some(String::new());
        }
        Some(self.marked_input(end))
    }

    /// Real (non-ghost) buffer text to the right of the cursor on its row
    /// (and on the rows it soft-wraps into). Non-empty only when the user
    /// moved the cursor back into the line, or for non-dim trailing text.
    fn cursor_tail(&self) -> String {
        let mut out = String::new();
        let mut line = self.abs_cursor_line();
        let mut col = self.cursor_col;
        for _ in 0..8 {
            let Some(row) = self.abs_row(line) else { break };
            let mut gap = 0usize;
            let mut seg = String::new();
            let mut stopped = false;
            for c in row.iter().skip(col) {
                if is_ghost(c) { stopped = true; break; }
                if c.c == ' ' { gap += 1; if gap >= 4 { stopped = true; break; } } else { gap = 0; }
                c.push_text(&mut seg);
            }
            out.push_str(seg.trim_end());
            if stopped || !row.last().map_or(false, |c| c.wrap()) { break; }
            line += 1;
            col = 0;
        }
        out
    }

    /// What pressing Enter would submit, for the Preview-Then-Accept safety
    /// check. `None` means "do not intercept": full-screen program, command
    /// running, not at a prompt, or nothing typed.
    ///
    /// With shell integration (OSC 133) the marked input is authoritative.
    /// Without it the screen is scraped heuristically, and only if the cursor
    /// row really looks like a shell prompt.
    pub fn pending_command_line(&self) -> Option<String> {
        if self.is_alt_screen() {
            return None;
        }
        let text = if self.blocks.osc_seen() {
            if !self.at_shell_prompt() {
                return None;
            }
            let mut t = self.typed_input()?;
            t.push_str(&self.cursor_tail());
            t
        } else {
            self.scrape_command_line()?
        };
        let t = text.trim();
        if t.is_empty() { None } else { Some(t.to_string()) }
    }

    /// Fallback for shells without integration: reconstruct the command from
    /// the screen. Walks up through continuation prompts to the PS1 row.
    fn scrape_command_line(&self) -> Option<String> {
        let sb = self.scrapable_row_base();
        let mut abs = sb + self.cursor_row.min(self.grid.len().saturating_sub(1));
        let mut lines: Vec<String> = Vec::new();
        let mut cursor_row = true;
        for _ in 0..32 {
            // The logical (soft-wrap joined) line ending at `abs`.
            let mut first = abs;
            while first > 0 && self.abs_row(first - 1).map_or(false, |r| r.last().map_or(false, |c| c.wrap())) {
                first -= 1;
            }
            let mut text = String::new();
            for l in first..=abs {
                let Some(row) = self.abs_row(l) else { break };
                let upto = if cursor_row && l == abs { self.cursor_col.min(row.len()) } else { row.len() };
                for c in &row[..upto] {
                    c.push_text(&mut text);
                }
                if l != abs && !row.last().map_or(false, |c| c.wrap()) { text.push('\n'); }
            }
            if cursor_row {
                let tail = self.cursor_tail();
                text.push_str(&tail);
            }
            let text = strip_rprompt(text.trim_end()).to_string();
            if let Some(rest) = strip_ps2(&text) {
                lines.insert(0, rest.to_string());
                if first == 0 { return None; }
                abs = first - 1;
                cursor_row = false;
                continue;
            }
            return match strip_ps1(&text) {
                Some(cmd) => {
                    lines.insert(0, cmd.to_string());
                    Some(lines.join("\n"))
                }
                None => None,
            };
        }
        None
    }

    fn scrapable_row_base(&self) -> usize {
        self.scrollback.len()
    }

    fn push_mark(&mut self, kind: MarkKind, exit_code: Option<i32>) {
        self.marks.push(SemanticMark {
            kind,
            line: self.abs_cursor_line(),
            col: self.cursor_col,
            exit_code,
            at: Instant::now(),
            unix_time: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        });
        if self.marks.len() > MAX_MARKS {
            let excess = self.marks.len() - MAX_MARKS;
            self.marks.drain(..excess);
        }
    }

    /// Handle `OSC 133 ; <kind> [; args…]`. `args` are the params after `133`.
    pub fn handle_osc133(&mut self, args: &[&[u8]]) {
        let Some(kind) = args.first().and_then(|a| a.first()).copied() else { return };
        let line = self.abs_cursor_line();
        match kind {
            b'A' => {
                self.push_mark(MarkKind::PromptStart, None);
                self.blocks.on_prompt_start(line);
            }
            b'B' => {
                self.push_mark(MarkKind::CommandStart, None);
                self.blocks.on_command_start(line, self.cursor_col);
            }
            b'C' => {
                // A block starts only if a `B` is pending (see on_command_output).
                if self.blocks.command_start_pos().is_some() {
                    let command = self.marked_input((line, self.cursor_col));
                    self.push_mark(MarkKind::OutputStart, None);
                    self.blocks.on_command_output(line, command);
                }
            }
            b'D' => {
                let exit = args
                    .get(1)
                    .and_then(|a| std::str::from_utf8(a).ok())
                    .and_then(|s| s.trim().parse::<i32>().ok());
                self.push_mark(MarkKind::CommandEnd, exit);
                // Output normally ends with a newline, leaving the cursor at
                // column 0 of the next line; the last output line is above it.
                let end = if self.cursor_col == 0 { line.saturating_sub(1) } else { line };
                self.blocks.on_command_finished(end, exit);
            }
            _ => {}
        }
    }

    /// Handle `OSC 633 ; E ; <escaped command>` (VS Code style): the shell
    /// states the exact command line it is executing. Escapes: `\\` for a
    /// backslash and `\xNN` for `;`, control characters and anything else.
    pub fn handle_osc633(&mut self, args: &[&[u8]]) {
        let Some(kind) = args.first() else { return };
        if *kind != b"E" {
            return;
        }
        let mut raw = Vec::new();
        for (i, a) in args.iter().skip(1).enumerate() {
            if i > 0 {
                raw.push(b';');
            }
            raw.extend_from_slice(a);
        }
        self.blocks.on_command_text(unescape_command(&raw));
    }

    /// Handle `OSC 7 ; file://host/path`.
    pub fn handle_osc7(&mut self, uri: &[u8]) {
        if let Some(path) = parse_file_uri(uri) {
            self.cwd = Some(path);
        }
    }

    /// Handle `OSC 1337 ; CurrentDir=/path` (iTerm2).
    pub fn handle_osc1337(&mut self, args: &[&[u8]]) {
        let Some(first) = args.first() else { return };
        if let Some(rest) = first.strip_prefix(b"CurrentDir=") {
            // Re-join in case the path itself contained ';'.
            let mut path = rest.to_vec();
            for extra in &args[1..] {
                path.push(b';');
                path.extend_from_slice(extra);
            }
            if path.first() == Some(&b'/') {
                self.cwd = Some(String::from_utf8_lossy(&path).into_owned());
            }
        }
    }
}

/// Undo the shell-side escaping of OSC 633;E payloads.
pub fn unescape_command(raw: &[u8]) -> String {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\\' {
            match raw.get(i + 1) {
                Some(b'\\') => {
                    out.push(b'\\');
                    i += 2;
                    continue;
                }
                Some(b'x') => {
                    if let (Some(h), Some(l)) = (raw.get(i + 2).and_then(|b| hex(*b)), raw.get(i + 3).and_then(|b| hex(*b))) {
                        out.push(h * 16 + l);
                        i += 4;
                        continue;
                    }
                }
                _ => {}
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Drop a right-hand prompt's trailing box-drawing remains (`─╯`) and blanks.
fn strip_rprompt(s: &str) -> &str {
    let t = s.trim_end();
    t.trim_end_matches(|c| ('\u{2500}'..='\u{257f}').contains(&c)).trim_end()
}

/// Dim or gray text is autosuggestion ghost text, not part of the buffer.
fn is_ghost(c: &Cell) -> bool {
    use super::Color;
    c.dim()
        || match c.fg {
            Color::Indexed(n) => n == 8 || (240..=248).contains(&n),
            Color::Rgb(r, g, b) => r == g && g == b && r < 0xA0,
            Color::Default => false,
        }
}

/// `"cmdand> rm -rf \\"` -> `Some("rm -rf \\")` for zsh/bash secondary prompts.
fn strip_ps2(line: &str) -> Option<&str> {
    let i = line.find("> ").or_else(|| line.strip_suffix('>').map(|_| line.len() - 1))?;
    let prefix = &line[..i];
    if prefix.len() <= 20 && prefix.chars().all(|c| c.is_ascii_lowercase() || c == ' ') {
        Some(line.get(i + 2..).unwrap_or(""))
    } else {
        None
    }
}

/// Command text after the primary prompt terminator on `line`, if the line
/// looks like a shell prompt at all.
fn strip_ps1(line: &str) -> Option<&str> {
    const TERMS: &[&str] = &["$ ", "% ", "# ", "❯ ", "➜ ", "› ", "» ", "λ ", "→ ", "▶ ", "➤ ", "⟩ "];
    let best = TERMS.iter().filter_map(|t| line.find(t).map(|i| i + t.len())).min()?;
    Some(line[best..].trim())
}

/// Parse `file://host/path` (or a bare absolute path) into a decoded path.
pub fn parse_file_uri(uri: &[u8]) -> Option<String> {
    let rest = if let Some(r) = uri.strip_prefix(b"file://") {
        let slash = r.iter().position(|&b| b == b'/')?;
        &r[slash..]
    } else if uri.first() == Some(&b'/') {
        uri
    } else {
        return None;
    };
    let decoded = percent_decode(rest);
    String::from_utf8(decoded).ok().filter(|s| !s.is_empty())
}

pub fn percent_decode(input: &[u8]) -> Vec<u8> {
    let hex = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    };
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' && i + 2 < input.len() {
            if let (Some(h), Some(l)) = (hex(input[i + 1]), hex(input[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(input[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::AnsiHandler;

    fn feed(term: &mut Terminal, bytes: &[u8]) {
        let mut parser = vte::Parser::new();
        let mut h = AnsiHandler::new(term);
        for &b in bytes {
            parser.advance(&mut h, b);
        }
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode(b"/a%20b/%E4%B8%AD"), "/a b/中".as_bytes());
        assert_eq!(percent_decode(b"100%"), b"100%");
        assert_eq!(percent_decode(b"%zz%4"), b"%zz%4");
        assert_eq!(percent_decode(b"%41"), b"A");
    }

    #[test]
    fn file_uri_parsing() {
        assert_eq!(parse_file_uri(b"file://host.local/Users/me/My%20Dir").unwrap(), "/Users/me/My Dir");
        assert_eq!(parse_file_uri(b"file:///tmp/x").unwrap(), "/tmp/x");
        assert_eq!(parse_file_uri(b"/plain").unwrap(), "/plain");
        assert!(parse_file_uri(b"http://x/y").is_none());
        assert!(parse_file_uri(b"file://hostonly").is_none());
    }

    #[test]
    fn osc7_and_osc1337_set_cwd() {
        let mut t = Terminal::new(40, 6);
        feed(&mut t, b"\x1b]7;file://mac/Users/a%20b/c\x07");
        assert_eq!(t.cwd.as_deref(), Some("/Users/a b/c"));
        feed(&mut t, b"\x1b]7;file:///tmp\x1b\\");
        assert_eq!(t.cwd.as_deref(), Some("/tmp"));
        feed(&mut t, b"\x1b]1337;CurrentDir=/var/log\x07");
        assert_eq!(t.cwd.as_deref(), Some("/var/log"));
    }

    #[test]
    fn osc133_builds_exact_blocks() {
        let mut t = Terminal::new(40, 6);
        feed(&mut t, b"\x1b]133;A\x07$ \x1b]133;B\x07echo hi\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07");
        assert_eq!(t.marks.len(), 4);
        assert_eq!(t.marks[0].kind, MarkKind::PromptStart);
        assert_eq!(t.marks[3].exit_code, Some(0));
        let b = &t.blocks.blocks()[0];
        assert_eq!(b.command, "echo hi");
        assert_eq!((b.prompt_line, b.command_line, b.output_start, b.output_end), (0, 0, 1, 1));
        assert_eq!(b.exit_code, Some(0));
        assert!(!b.running);

        // Second command: failing, still running while no D has arrived.
        feed(&mut t, b"\x1b]133;A\x07$ \x1b]133;B\x07false\r\n\x1b]133;C\x07");
        assert!(t.blocks.is_running());
        assert!(t.blocks.block_at_line(3).unwrap().1.running);
        feed(&mut t, b"\x1b]133;D;1\x07");
        let b = &t.blocks.blocks()[1];
        assert_eq!(b.command, "false");
        assert_eq!(b.exit_code, Some(1));
        assert!(!b.is_success());
        assert!(t.blocks.osc_seen());
    }

    #[test]
    fn osc133_d_without_command_is_ignored() {
        let mut t = Terminal::new(40, 6);
        feed(&mut t, b"\x1b]133;D;0\x07\x1b]133;A\x07");
        assert_eq!(t.blocks.block_count(), 0);
    }

    #[test]
    fn blocks_survive_scrollback_scroll() {
        let mut t = Terminal::new(20, 4);
        feed(&mut t, b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07a\r\nb\r\nc\r\nd\r\n\x1b]133;D;0\x07");
        let b = &t.blocks.blocks()[0];
        assert_eq!(b.command, "ls");
        assert!(t.scrollback.len() > 0);
        assert_eq!(b.output_start, 1);
        assert_eq!(b.output_end, 4);
    }

    #[test]
    fn typed_input_tracks_prompt_state() {
        let mut t = Terminal::new(40, 6);
        assert_eq!(t.typed_input(), None, "no OSC 133 yet");
        feed(&mut t, b"\x1b]133;A\x07$ \x1b]133;B\x07");
        assert!(t.at_shell_prompt());
        assert_eq!(t.typed_input().as_deref(), Some(""));
        feed(&mut t, b"# find big files");
        assert_eq!(t.typed_input().as_deref(), Some("# find big files"));
        // Ghost text after the cursor (autosuggestion) is not "typed".
        feed(&mut t, b"\x1b[2m and more\x1b[0m\x1b[9D");
        assert_eq!(t.typed_input().as_deref(), Some("# find big files"));
        // Command accepted: no longer at a prompt.
        feed(&mut t, b"\r\n\x1b]133;C\x07");
        assert!(!t.at_shell_prompt());
        assert_eq!(t.typed_input(), None);
        feed(&mut t, b"\x1b]133;D;0\x07\x1b]133;A\x07$ ");
        assert!(!t.at_shell_prompt(), "between A and B the prompt is still drawing");
        // Alt screen never counts.
        feed(&mut t, b"\x1b]133;B\x07");
        assert!(t.at_shell_prompt());
        feed(&mut t, b"\x1b[?1049h");
        assert!(!t.at_shell_prompt());
    }

    /// Byte-for-byte shape of what zsh emits (PROMPT_SP included), captured
    /// from a real PTY run of the injected integration.
    #[test]
    fn real_zsh_stream_with_prompt_sp() {
        let mut t = Terminal::new(40, 12);
        let sp = " ".repeat(39); // PROMPT_SP: "%" + (COLUMNS-1) spaces
        let prompt = |cwd: &str| format!(
            "\x1b[1m\x1b[7m%\x1b[27m\x1b[1m\x1b[0m{sp}\r \r\x1b]133;A\x07\x1b]7;file://h{cwd}\x07\x1b[0m\x1b[27m\x1b[24m\x1b[J{cwd} % \x1b[K\x1b]133;B\x07\x1b[?2004h");
        let mut stream = String::new();
        stream += &prompt("/tmp");
        stream += "echo hi\x1b[?2004l\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07";
        stream += &prompt("/tmp");
        stream += "false\x1b[?2004l\r\n\x1b]133;C\x07\x1b]133;D;1\x07";
        stream += &prompt("/tmp");
        feed(&mut t, stream.as_bytes());
        assert_eq!(t.cwd.as_deref(), Some("/tmp"));
        let bl = t.blocks.blocks();
        assert_eq!(bl.len(), 2);
        assert_eq!(bl[0].command, "echo hi");
        assert_eq!(bl[0].exit_code, Some(0));
        assert_eq!((bl[0].command_line, bl[0].output_start, bl[0].output_end), (0, 1, 1));
        assert_eq!(bl[1].command, "false");
        assert_eq!(bl[1].exit_code, Some(1));
        // `false` printed nothing: output range is empty (end < start).
        assert!(bl[1].output_end < bl[1].output_start);
        assert_eq!(bl[1].command_line, 2);
    }

    #[test]
    fn explicit_command_text_wins_over_screen_text() {
        let mut t = Terminal::new(60, 6);
        // p10k-style: right prompt remnants on the command row.
        feed(&mut t, b"\x1b]133;A\x07~ % \x1b]133;B\x07echo hi                      \xe2\x94\x80\xe2\x95\xaf\x1b[1G\x1b[7C");
        feed(&mut t, b"\x1b]633;E;echo hi\x07\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07");
        let b = &t.blocks.blocks()[0];
        assert_eq!(b.command, "echo hi");
        // Escapes: `;` and control characters / backslashes round-trip.
        assert_eq!(unescape_command(br"echo \x3b a\\b\x0a"), "echo ; a\\b\n");
        assert_eq!(unescape_command(br"100%"), "100%");
    }

    #[test]
    fn rprompt_remnants_are_stripped_without_explicit_text() {
        let mut t = Terminal::new(60, 6);
        feed(&mut t, "\x1b]133;A\x07~ % \x1b]133;B\x07false                    ─╯\r\n\x1b]133;C\x07\x1b]133;D;1\x07".as_bytes());
        assert_eq!(t.blocks.blocks()[0].command, "false");
    }

    #[test]
    fn duplicate_marks_from_two_integrations_make_one_block() {
        // iTerm2's integration next to ours: B, B, C;, C, D, D and an empty-Enter C;/D pair.
        let mut t = Terminal::new(40, 8);
        feed(&mut t, b"\x1b]133;A\x07$ \x1b]133;B\x07\x1b]133;B\x07echo hi\r\n\x1b]133;C;\x07\x1b]633;E;echo hi\x07\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07\x1b]133;A\x07\x1b]133;D;0\x07");
        assert_eq!(t.blocks.blocks().len(), 1);
        assert_eq!(t.blocks.blocks()[0].command, "echo hi");
        assert_eq!(t.blocks.blocks()[0].exit_code, Some(0));
        feed(&mut t, b"$ \x1b]133;B\x07\r\n\x1b]133;C;\x07\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
        // The empty Enter above had a B before C;, but no command: it must not
        // leave a running block behind, and the prompt must be live again.
        assert!(!t.blocks.is_running());
        assert!(t.at_shell_prompt());
        // C;/D pair arriving right after A (no B) is ignored.
        let n = t.blocks.blocks().len();
        feed(&mut t, b"\r\n\x1b]133;A\x07$ \x1b]133;C;\x07\x1b]133;D;0\x07\x1b]133;B\x07");
        assert_eq!(t.blocks.blocks().len(), n);
        assert!(t.at_shell_prompt());
    }

    #[test]
    fn continuation_prompts_are_excluded_from_typed_input() {
        let mut t = Terminal::new(40, 8);
        feed(&mut t, b"\x1b]133;A\x07$ \x1b]133;B\x07rm -rf \\\r\ncmdand> \x1b]133;B\x07/tmp/x");
        assert_eq!(t.typed_input().as_deref(), Some("rm -rf \\\n/tmp/x"));
        assert_eq!(t.pending_command_line().as_deref(), Some("rm -rf \\\n/tmp/x"));
        feed(&mut t, b"\r\n\x1b]133;C\x07");
        assert_eq!(t.blocks.blocks().len(), 0);
        assert_eq!(t.blocks.get(0).unwrap().command, "rm -rf \\\n/tmp/x");
        // A prompt redraw on the same line replaces B instead of adding a segment.
        let mut t = Terminal::new(40, 8);
        feed(&mut t, b"\x1b]133;A\x07$ \x1b]133;B\x07ls\x1b[2D\x1b]133;B\x07");
        assert_eq!(t.blocks.input_starts().len(), 1);
    }

    #[test]
    fn soft_wrapped_input_is_one_logical_line() {
        let mut t = Terminal::new(10, 6);
        feed(&mut t, b"\x1b]133;A\x07$ \x1b]133;B\x07rm -rf /tmp/abcdefgh");
        assert_eq!(t.typed_input().as_deref(), Some("rm -rf /tmp/abcdefgh"));
        // A full row followed by a real newline is NOT merged.
        let mut t = Terminal::new(10, 6);
        feed(&mut t, b"\x1b]133;A\x07$ \x1b]133;B\x07aaaaaaaa\r\nbbb");
        assert_eq!(t.typed_input().as_deref(), Some("aaaaaaaa\nbbb"));
    }

    #[test]
    fn pending_command_line_uses_osc133_and_refuses_non_prompts() {
        let mut t = Terminal::new(60, 6);
        // Alternate screen (vim) with "rm -rf /" in insert mode: never a command.
        feed(&mut t, b"\x1b[?1049h\x1b[Hrm -rf /");
        assert_eq!(t.pending_command_line(), None);
        feed(&mut t, b"\x1b[?1049l");
        // Integrated shell, command running: no.
        feed(&mut t, b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 5\r\n\x1b]133;C\x07rm -rf /");
        assert_eq!(t.pending_command_line(), None);
        feed(&mut t, b"\x1b]133;D;0\x07\x1b]133;A\x07\r\n$ \x1b]133;B\x07rm -rf ~");
        assert_eq!(t.pending_command_line().as_deref(), Some("rm -rf ~"));
        // Cursor moved back into the buffer: the whole line is what gets submitted.
        feed(&mut t, b"\x1b[8D");
        assert_eq!(t.pending_command_line().as_deref(), Some("rm -rf ~"));
        // Dim ghost text (autosuggestion) after the cursor is not part of it.
        let mut t = Terminal::new(60, 6);
        feed(&mut t, b"\x1b]133;A\x07$ \x1b]133;B\x07rm\x1b[2m -rf /\x1b[0m\x1b[7D");
        assert_eq!(t.pending_command_line().as_deref(), Some("rm"));
    }

    fn scrape(rows: &[&str], cursor_col: usize) -> Option<String> {
        let mut t = Terminal::new(80, rows.len().max(2));
        let text = rows.join("\r\n");
        feed(&mut t, text.as_bytes());
        feed(&mut t, format!("\x1b[{}G", cursor_col + 1).as_bytes());
        t.pending_command_line()
    }

    #[test]
    fn screen_scraping_fallback_without_osc133() {
        assert_eq!(scrape(&["~/proj % rm -rf build"], 21).as_deref(), Some("rm -rf build"));
        assert_eq!(scrape(&["user@host:~$ echo 100% && rm -rf /tmp/x"], 41).as_deref(), Some("echo 100% && rm -rf /tmp/x"));
        assert_eq!(scrape(&["~ % rm -rf \"$HOME/x\""], 21).as_deref(), Some("rm -rf \"$HOME/x\""));
        // p10k two-line prompt with right prompt remnants.
        let row = format!("╰─❯ rm -rf /tmp/x{}─╯", " ".repeat(40));
        assert_eq!(scrape(&["╭─ ~/proj", &row], 18).as_deref(), Some("rm -rf /tmp/x"));
        // Continuation prompts are stripped and rows joined with newlines.
        assert_eq!(scrape(&["~ % rm -rf \\", "> /tmp/x"], 8).as_deref(), Some("rm -rf \\\n/tmp/x"));
        assert_eq!(scrape(&["~ % for i in 1 2; do", "for> echo $i", "for> done"], 9).as_deref(), Some("for i in 1 2; do\necho $i\ndone"));
        // Not a prompt (editor insert mode on the main screen, plain text): never.
        assert_eq!(scrape(&["rm -rf /"], 8), None);
        assert_eq!(scrape(&["-- INSERT -- rm -rf /"], 21), None);
        assert_eq!(scrape(&["~ % "], 4), None);
    }
}
