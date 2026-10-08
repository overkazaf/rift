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
    /// exclusive of the end position. Rows that are filled to the last column
    /// are treated as soft-wrapped and joined without a newline.
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
            let seg: String = row[from..to].iter().filter(|c| c.c != '\0').map(|c| c.c).collect();
            let wrapped = line != el && row.last().map_or(false, |c| c.c != ' ' && c.c != '\0');
            if wrapped {
                out.push_str(&seg);
            } else {
                out.push_str(seg.trim_end());
                if line != el { out.push('\n'); }
            }
        }
        out.trim().to_string()
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
                self.push_mark(MarkKind::OutputStart, None);
                let start = self.blocks.command_start_pos();
                let command = start
                    .map(|s| self.text_between(s, (line, self.cursor_col)))
                    .unwrap_or_default();
                self.blocks.on_command_output(line, command);
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
}
