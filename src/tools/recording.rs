use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::Instant;

/// Asciinema v2 compatible recording format.
/// Header: {"version":2,"width":W,"height":H,"timestamp":T}
/// Events: [elapsed_seconds, "o"|"i", "data"]
pub struct Recorder {
    file: File,
    start: Instant,
    /// Trailing bytes of an incomplete UTF-8 sequence, per stream ([o, i]):
    /// PTY reads can split a multi-byte character across two events.
    carry: [Vec<u8>; 2],
}

impl Recorder {
    pub fn start(path: &PathBuf, cols: usize, rows: usize) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = File::create(path)?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        writeln!(
            file,
            r#"{{"version":2,"width":{},"height":{},"timestamp":{}}}"#,
            cols, rows, ts
        )?;

        Ok(Self { file, start: Instant::now(), carry: [Vec::new(), Vec::new()] })
    }

    pub fn record_output(&mut self, data: &[u8]) {
        self.write_event("o", data);
    }

    pub fn record_input(&mut self, data: &[u8]) {
        self.write_event("i", data);
    }

    fn write_event(&mut self, kind: &str, data: &[u8]) {
        let elapsed = self.start.elapsed().as_secs_f64();
        let slot = &mut self.carry[usize::from(kind != "o")];
        slot.extend_from_slice(data);
        let text = take_utf8(slot);
        if text.is_empty() {
            return;
        }
        let escaped = escape_json_str(&text);
        let _ = writeln!(self.file, r#"[{:.6}, "{}", "{}"]"#, elapsed, kind, escaped);
    }

    pub fn finish(mut self) {
        let _ = self.file.flush();
        log::info!("Recording finished");
    }
}

/// Plays back a recording, yielding (delay_secs, event_kind, data) per event.
pub struct Player {
    events: Vec<PlayEvent>,
    index: usize,
}

pub struct PlayEvent {
    pub time: f64,
    pub kind: EventKind,
    pub data: Vec<u8>,
}

#[derive(PartialEq, Eq)]
pub enum EventKind {
    Output,
    Input,
}

pub struct RecordingMeta {
    pub width: usize,
    pub height: usize,
}

impl Player {
    pub fn load(path: &PathBuf) -> std::io::Result<(RecordingMeta, Self)> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();

        let header = lines.next().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "empty recording")
        })??;

        let meta = parse_header(&header)?;
        let mut events = Vec::new();

        for line in lines {
            let line = line?;
            if let Some(evt) = parse_event(&line) {
                events.push(evt);
            }
        }

        Ok((meta, Self { events, index: 0 }))
    }

    pub fn next_event(&mut self) -> Option<&PlayEvent> {
        if self.index < self.events.len() {
            let evt = &self.events[self.index];
            self.index += 1;
            Some(evt)
        } else {
            None
        }
    }

    pub fn total_duration(&self) -> f64 {
        self.events.last().map_or(0.0, |e| e.time)
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    pub fn reset(&mut self) {
        self.index = 0;
    }
}

fn parse_header(s: &str) -> std::io::Result<RecordingMeta> {
    let width = extract_json_num(s, "width").unwrap_or(80);
    let height = extract_json_num(s, "height").unwrap_or(24);
    Ok(RecordingMeta { width, height })
}

fn extract_json_num(s: &str, key: &str) -> Option<usize> {
    let pattern = format!("\"{}\":", key);
    let start = s.find(&pattern)? + pattern.len();
    let rest = &s[start..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].trim().parse().ok()
}

fn parse_event(line: &str) -> Option<PlayEvent> {
    let line = line.trim();
    if !line.starts_with('[') { return None; }
    let inner = &line[1..line.len().checked_sub(1)?];

    let first_comma = inner.find(',')?;
    let time: f64 = inner[..first_comma].trim().parse().ok()?;

    let rest = &inner[first_comma + 1..];
    let kind_start = rest.find('"')? + 1;
    let kind_end = kind_start + rest[kind_start..].find('"')?;
    let kind = match &rest[kind_start..kind_end] {
        "o" => EventKind::Output,
        "i" => EventKind::Input,
        _ => return None,
    };

    let data_start = rest[kind_end + 1..].find('"')? + kind_end + 2;
    let data_end = rest.len() - 1;
    let data = unescape_json(&rest[data_start..data_end]);

    Some(PlayEvent { time, kind, data })
}

/// Decode the complete UTF-8 prefix of `buf` (invalid bytes become U+FFFD)
/// and leave an incomplete trailing sequence in `buf` for the next event.
fn take_utf8(buf: &mut Vec<u8>) -> String {
    let mut out = String::new();
    let mut rest: &[u8] = buf;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                rest = &[];
                break;
            }
            Err(e) => {
                let (ok, tail) = rest.split_at(e.valid_up_to());
                out.push_str(std::str::from_utf8(ok).unwrap_or_default());
                match e.error_len() {
                    Some(n) => {
                        out.push('\u{FFFD}');
                        rest = &tail[n..];
                    }
                    None => {
                        rest = tail;
                        break;
                    }
                }
            }
        }
    }
    *buf = rest.to_vec();
    out
}

fn escape_json_str(data: &str) -> String {
    let mut out = String::with_capacity(data.len() * 2);
    for c in data.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

fn unescape_json(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push(b'\n'),
                Some('r') => out.push(b'\r'),
                Some('t') => out.push(b'\t'),
                Some('b') => out.push(0x08),
                Some('f') => out.push(0x0C),
                Some('\\') => out.push(b'\\'),
                Some('"') => out.push(b'"'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(n) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(n) {
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                    }
                }
                Some(other) => {
                    out.push(b'\\');
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
                }
                None => out.push(b'\\'),
            }
        } else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    out
}

#[cfg(test)]
mod utf8_tests {
    use super::*;

    #[test]
    fn split_multibyte_chars_round_trip() {
        let src = "中文 ✓ 🦀 ok\n".as_bytes();
        let mut carry = Vec::new();
        let mut json = String::new();
        // Feed one byte at a time: every multi-byte char is split.
        for b in src {
            carry.push(*b);
            json.push_str(&escape_json_str(&take_utf8(&mut carry)));
        }
        assert!(carry.is_empty());
        assert_eq!(unescape_json(&json), src);
    }

    #[test]
    fn invalid_bytes_become_replacement_char() {
        let mut buf = vec![b'a', 0xff, b'b'];
        assert_eq!(take_utf8(&mut buf), "a\u{FFFD}b");
        assert!(buf.is_empty());
    }
}
