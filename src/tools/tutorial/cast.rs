//! Tutorial demo format: **asciicast v2** (the format the session recorder
//! writes, see [`crate::tools::recording`]) plus Rift *marker* events.
//!
//! ```text
//! {"version":2,"width":80,"height":18,"title":"Split panes"}
//! [0.400, "m", "caption:Press {key:split_right} to split the pane"]
//! [0.900, "m", "key:split_right|Split right"]
//! [0.950, "m", "pane:split-right"]
//! [1.200, "o", "\u001b]133;A\u0007..."]
//! ```
//!
//! * `"o"` events are terminal output, fed through the real VT parser into
//!   the demo's own scripted panes (never a PTY).
//! * `"i"` events (keyboard input in user recordings) are **dropped** at parse
//!   time: playback has no input channel at all.
//! * `"m"` events are asciicast markers. asciinema shows the label as a
//!   chapter; Rift interprets a `kind:payload` label as one of the [`Mark`]s
//!   below. A label without a known prefix is shown as a caption, so plain
//!   asciinema markers in user recordings still work.
//!
//! Shortcuts are never written as literal text: captions and panels use
//! `{key:<action>}` placeholders resolved against the user's effective keymap
//! (`{key:split_right}`, `{key:@ask_inline}` for chords owned by other
//! modules, `{key:=Tab}` for a plain key). See [`super::keys`].

use std::fmt::Write as _;

/// A parsed demo / recording.
#[derive(Clone, Debug)]
pub struct Cast {
    pub cols: usize,
    pub rows: usize,
    pub title: String,
    pub events: Vec<Event>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// Seconds since the start.
    pub t: f64,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Output(Vec<u8>),
    Mark(Mark),
}

/// Where a mock panel sits on the demo stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Center,
    Left,
    Right,
    Bottom,
}

/// A kit-styled panel drawn over the demo (for UI that has no terminal
/// output: the AI chat answer, the Mission Control dock, ...). Lines may
/// start with `## ` (section), `~ ` (muted), `! ` (warning), `+ ` (success)
/// or `> ` (accent).
#[derive(Clone, Debug, PartialEq)]
pub struct Panel {
    pub place: Place,
    pub title: String,
    pub badge: String,
    pub lines: Vec<String>,
    /// Footer hints: (key, label). Keys may use `{key:...}` placeholders.
    pub hints: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaneOp {
    SplitRight,
    SplitDown,
    Focus(usize),
    Zoom,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TabOp {
    /// Open a tab with this title and make it active.
    New(String),
    Select(usize),
    /// Rename the active tab.
    Title(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum BlocksOp {
    /// Toggle folding of block N of the active pane.
    Fold(usize),
    /// Show the hover toolbar of block N (optionally a hot button).
    Hover(usize, Option<String>),
    Select(usize),
    /// Scroll so block N's command line is at the top (`Cmd+Shift+Up/Down`).
    Jump(usize),
    /// Back to the live bottom of the pane.
    Bottom,
    Clear,
}

/// Inline AI state, always with sample content (nothing is sent anywhere).
#[derive(Clone, Debug, PartialEq)]
pub enum AiOp {
    /// Fix bar for the last block of the active pane: (command, explanation).
    Fix(String, String),
    /// `# natural language` ghost: (query, generated command).
    Nl(String, String),
    /// Cmd+K popover about the last block with this question typed.
    Ask(String),
    Clear,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiOp {
    /// The real command palette with this query typed.
    Palette(String),
    /// The real Preview-Then-Accept modal for this command (classified by
    /// the real safety engine at playback time; nothing runs), with the
    /// text typed into its "yes" field so far.
    Preview(String, String),
    Panel(Panel),
    Clear,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Mark {
    /// Caption text (empty clears). Each caption is a "step" for ←/→.
    Caption(String),
    /// Key-press overlay: (key reference, label).
    Key(String, String),
    Pane(PaneOp),
    Tab(TabOp),
    Blocks(BlocksOp),
    Ai(AiOp),
    Ui(UiOp),
}

// ───────────────────────────── markers ─────────────────────────────

fn place_name(p: Place) -> &'static str {
    match p {
        Place::Center => "center",
        Place::Left => "left",
        Place::Right => "right",
        Place::Bottom => "bottom",
    }
}

impl Mark {
    /// The marker label stored in the cast file.
    pub fn encode(&self) -> String {
        match self {
            Mark::Caption(s) => format!("caption:{s}"),
            Mark::Key(k, l) => format!("key:{}|{}", esc(k), esc(l)),
            Mark::Pane(op) => match op {
                PaneOp::SplitRight => "pane:split-right".into(),
                PaneOp::SplitDown => "pane:split-down".into(),
                PaneOp::Focus(i) => format!("pane:focus:{i}"),
                PaneOp::Zoom => "pane:zoom".into(),
            },
            Mark::Tab(op) => match op {
                TabOp::New(t) => format!("tab:new:{t}"),
                TabOp::Select(i) => format!("tab:select:{i}"),
                TabOp::Title(t) => format!("tab:title:{t}"),
            },
            Mark::Blocks(op) => match op {
                BlocksOp::Fold(i) => format!("blocks:fold:{i}"),
                BlocksOp::Hover(i, None) => format!("blocks:hover:{i}"),
                BlocksOp::Hover(i, Some(b)) => format!("blocks:hover:{i}:{b}"),
                BlocksOp::Select(i) => format!("blocks:select:{i}"),
                BlocksOp::Jump(i) => format!("blocks:jump:{i}"),
                BlocksOp::Bottom => "blocks:bottom".into(),
                BlocksOp::Clear => "blocks:clear".into(),
            },
            Mark::Ai(op) => match op {
                AiOp::Fix(c, e) => format!("ai:fix|{}|{}", esc(c), esc(e)),
                AiOp::Nl(q, c) => format!("ai:nl|{}|{}", esc(q), esc(c)),
                AiOp::Ask(q) => format!("ai:ask|{}", esc(q)),
                AiOp::Clear => "ai:clear".into(),
            },
            Mark::Ui(op) => match op {
                UiOp::Palette(q) => format!("ui:palette|{}", esc(q)),
                UiOp::Preview(c, typed) => format!("ui:preview|{}|{}", esc(c), esc(typed)),
                UiOp::Clear => "ui:clear".into(),
                UiOp::Panel(p) => {
                    let mut s = format!("ui:panel|{}|{}|{}", place_name(p.place), esc(&p.title), esc(&p.badge));
                    for l in &p.lines {
                        s.push('|');
                        s.push_str(&esc(l));
                    }
                    for (k, l) in &p.hints {
                        s.push_str(&format!("|hint:{}={}", esc(k), esc(l)));
                    }
                    s
                }
            },
        }
    }

    /// Parse a marker label. Unknown labels become captions.
    pub fn parse(label: &str) -> Mark {
        Self::parse_known(label).unwrap_or_else(|| Mark::Caption(label.to_string()))
    }

    fn parse_known(label: &str) -> Option<Mark> {
        let (kind, rest) = label.split_once(':')?;
        let num = |s: &str| s.trim().parse::<usize>().ok();
        Some(match kind {
            "caption" => Mark::Caption(rest.to_string()),
            "key" => {
                let f = fields(rest);
                Mark::Key(f[0].clone(), f.get(1).cloned().unwrap_or_default())
            }
            "pane" => Mark::Pane(match rest {
                "split-right" => PaneOp::SplitRight,
                "split-down" => PaneOp::SplitDown,
                "zoom" => PaneOp::Zoom,
                _ => PaneOp::Focus(num(rest.strip_prefix("focus:")?)?),
            }),
            "tab" => {
                let (op, arg) = rest.split_once(':')?;
                Mark::Tab(match op {
                    "new" => TabOp::New(arg.to_string()),
                    "select" => TabOp::Select(num(arg)?),
                    "title" => TabOp::Title(arg.to_string()),
                    _ => return None,
                })
            }
            "blocks" => {
                let mut it = rest.split(':');
                let op = it.next()?;
                Mark::Blocks(match op {
                    "fold" => BlocksOp::Fold(num(it.next()?)?),
                    "hover" => BlocksOp::Hover(num(it.next()?)?, it.next().map(str::to_string)),
                    "select" => BlocksOp::Select(num(it.next()?)?),
                    "jump" => BlocksOp::Jump(num(it.next()?)?),
                    "bottom" => BlocksOp::Bottom,
                    "clear" => BlocksOp::Clear,
                    _ => return None,
                })
            }
            "ai" => {
                let f = fields(rest);
                let parts: Vec<&str> = f.iter().map(String::as_str).collect();
                Mark::Ai(match parts.as_slice() {
                    ["fix", c, e] => AiOp::Fix(c.to_string(), e.to_string()),
                    ["nl", q, c] => AiOp::Nl(q.to_string(), c.to_string()),
                    ["ask", q] => AiOp::Ask(q.to_string()),
                    ["clear"] => AiOp::Clear,
                    _ => return None,
                })
            }
            "ui" => {
                let f = fields(rest);
                let parts: Vec<&str> = f.iter().map(String::as_str).collect();
                Mark::Ui(match parts.as_slice() {
                    ["palette", q] => UiOp::Palette(q.to_string()),
                    ["preview", c] => UiOp::Preview(c.to_string(), String::new()),
                    ["preview", c, typed] => UiOp::Preview(c.to_string(), typed.to_string()),
                    ["clear"] => UiOp::Clear,
                    ["panel", place, title, badge, lines @ ..] => {
                        let place = match *place {
                            "center" => Place::Center,
                            "left" => Place::Left,
                            "right" => Place::Right,
                            "bottom" => Place::Bottom,
                            _ => return None,
                        };
                        let mut ls = Vec::new();
                        let mut hints = Vec::new();
                        for l in lines {
                            if let Some(h) = l.strip_prefix("hint:") {
                                let (k, v) = h.split_once('=').unwrap_or((h, ""));
                                hints.push((k.to_string(), v.to_string()));
                            } else {
                                ls.push(l.to_string());
                            }
                        }
                        UiOp::Panel(Panel { place, title: title.to_string(), badge: badge.to_string(), lines: ls, hints })
                    }
                    _ => return None,
                })
            }
            _ => return None,
        })
    }

    /// Every user-visible text of this marker (captions, labels, panel
    /// lines and hints) - for the "no literal shortcuts" lint.
    pub fn texts(&self) -> Vec<&str> {
        match self {
            Mark::Caption(s) => vec![s],
            Mark::Key(_, l) => vec![l],
            Mark::Ai(AiOp::Fix(c, e)) => vec![c, e],
            Mark::Ai(AiOp::Nl(q, c)) => vec![q, c],
            Mark::Ai(AiOp::Ask(q)) => vec![q],
            Mark::Ui(UiOp::Panel(p)) => {
                let mut v: Vec<&str> = vec![&p.title, &p.badge];
                v.extend(p.lines.iter().map(String::as_str));
                v.extend(p.hints.iter().map(|(_, l)| l.as_str()));
                v
            }
            _ => vec![],
        }
    }

    /// Every key reference of this marker: key overlays, panel hint keys and
    /// `{key:...}` placeholders in its texts.
    pub fn key_refs(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Mark::Key(k, _) = self {
            out.push(k.clone());
        }
        if let Mark::Ui(UiOp::Panel(p)) = self {
            for (k, _) in &p.hints {
                match k.strip_prefix("{key:").and_then(|s| s.strip_suffix('}')) {
                    Some(r) => out.push(r.to_string()),
                    None => out.push(format!("={k}")),
                }
            }
        }
        for t in self.texts() {
            out.extend(placeholders(t).into_iter().map(str::to_string));
        }
        out
    }
}

/// Escape a marker field (`|` separates fields).
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('|', "\\|")
}

/// Split marker fields on unescaped `|` and unescape them.
fn fields(s: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => {
                if let Some(n) = it.next() {
                    out.last_mut().unwrap().push(n);
                }
            }
            '|' => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out
}

/// `{key:xyz}` references inside a text.
pub fn placeholders(s: &str) -> Vec<&str> {
    let mut v = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find("{key:") {
        let after = &rest[i + 5..];
        match after.find('}') {
            Some(j) => {
                v.push(&after[..j]);
                rest = &after[j + 1..];
            }
            None => break,
        }
    }
    v
}

// ───────────────────────────── JSON (just enough) ─────────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at byte {}", c as char, self.i))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > 32 {
            return Err("nesting too deep".into());
        }
        self.ws();
        match self.s.get(self.i) {
            Some(b'"') => self.string().map(Json::Str),
            Some(b'[') => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Json::Arr(v));
                }
                loop {
                    v.push(self.value(depth + 1)?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Json::Arr(v));
                        }
                        _ => return Err(format!("bad array at byte {}", self.i)),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Json::Obj(v));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.eat(b':')?;
                    let val = self.value(depth + 1)?;
                    v.push((k, val));
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Json::Obj(v));
                        }
                        _ => return Err(format!("bad object at byte {}", self.i)),
                    }
                }
            }
            Some(b't') if self.s[self.i..].starts_with(b"true") => {
                self.i += 4;
                Ok(Json::Bool(true))
            }
            Some(b'f') if self.s[self.i..].starts_with(b"false") => {
                self.i += 5;
                Ok(Json::Bool(false))
            }
            Some(b'n') if self.s[self.i..].starts_with(b"null") => {
                self.i += 4;
                Ok(Json::Null)
            }
            Some(c) if *c == b'-' || c.is_ascii_digit() => {
                let st = self.i;
                while self.i < self.s.len() && matches!(self.s[self.i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                    self.i += 1;
                }
                let txt = std::str::from_utf8(&self.s[st..self.i]).map_err(|e| e.to_string())?;
                txt.parse::<f64>().map(Json::Num).map_err(|_| format!("bad number '{txt}'"))
            }
            _ => Err(format!("unexpected input at byte {}", self.i)),
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let h = self.s.get(self.i..self.i + 4).ok_or("short \\u escape")?;
        self.i += 4;
        u32::from_str_radix(std::str::from_utf8(h).map_err(|e| e.to_string())?, 16).map_err(|_| "bad \\u escape".to_string())
    }

    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.s.get(self.i).ok_or("unterminated string")?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.s.get(self.i).ok_or("bad escape")?;
                    self.i += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'/' => out.push(b'/'),
                        b'\\' => out.push(b'\\'),
                        b'"' => out.push(b'"'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.s.get(self.i..self.i + 2) == Some(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                            let mut b = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
                        }
                        _ => return Err(format!("bad escape '\\{}'", e as char)),
                    }
                }
                _ => out.push(c),
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

fn parse_json(line: &str) -> Result<Json, String> {
    let mut p = P { s: line.as_bytes(), i: 0 };
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err(format!("trailing data at byte {}", p.i));
    }
    Ok(v)
}

/// JSON string literal (UTF-8 kept as is, control characters escaped).
pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ───────────────────────────── cast ─────────────────────────────

impl Cast {
    /// Parse an asciicast v2 document. Lines that are not events (blank
    /// lines, unknown event codes) are skipped; a malformed event line or a
    /// missing / non-v2 header is an error.
    pub fn parse(src: &str) -> Result<Cast, String> {
        let mut lines = src.lines().enumerate().filter(|(_, l)| !l.trim().is_empty());
        let (_, header) = lines.next().ok_or("empty cast")?;
        let Json::Obj(h) = parse_json(header.trim()).map_err(|e| format!("header: {e}"))? else {
            return Err("header is not a JSON object".into());
        };
        let get = |k: &str| h.iter().find(|(n, _)| n == k).map(|(_, v)| v);
        match get("version") {
            Some(Json::Num(v)) if *v == 2.0 => {}
            _ => return Err("not an asciicast v2 file (\"version\": 2)".into()),
        }
        let num = |k: &str, d: usize| match get(k) {
            Some(Json::Num(n)) if *n >= 1.0 && *n <= 1000.0 => *n as usize,
            _ => d,
        };
        let cols = num("width", 80);
        let rows = num("height", 24);
        let title = match get("title") {
            Some(Json::Str(s)) => s.clone(),
            _ => String::new(),
        };
        let mut events = Vec::new();
        for (n, line) in lines {
            let v = parse_json(line.trim()).map_err(|e| format!("line {}: {e}", n + 1))?;
            let Json::Arr(a) = v else { return Err(format!("line {}: event is not an array", n + 1)) };
            let (Some(Json::Num(t)), Some(Json::Str(code)), Some(Json::Str(data))) = (a.first(), a.get(1), a.get(2)) else {
                return Err(format!("line {}: expected [time, code, data]", n + 1));
            };
            if !t.is_finite() || *t < 0.0 {
                return Err(format!("line {}: bad time {t}", n + 1));
            }
            let kind = match code.as_str() {
                "o" => Kind::Output(data.as_bytes().to_vec()),
                "m" => Kind::Mark(Mark::parse(data)),
                // "i" (input) and resize / unknown codes: never replayed.
                _ => continue,
            };
            events.push(Event { t: *t, kind });
        }
        Ok(Cast { cols, rows, title, events })
    }

    /// Serialize as asciicast v2 (the generator's output).
    pub fn to_asciicast(&self) -> String {
        let mut s = format!("{{\"version\":2,\"width\":{},\"height\":{},\"title\":{}}}\n", self.cols, self.rows, json_str(&self.title));
        for e in &self.events {
            let (code, data) = match &e.kind {
                Kind::Output(b) => ("o", String::from_utf8_lossy(b).into_owned()),
                Kind::Mark(m) => ("m", m.encode()),
            };
            let _ = writeln!(s, "[{:.3}, \"{code}\", {}]", e.t, json_str(&data));
        }
        s
    }

    pub fn duration(&self) -> f64 {
        self.events.last().map_or(0.0, |e| e.t)
    }

    /// Times of the non-empty captions (the steps ←/→ move between).
    pub fn steps(&self) -> Vec<f64> {
        self.events
            .iter()
            .filter(|e| matches!(&e.kind, Kind::Mark(Mark::Caption(c)) if !c.is_empty()))
            .map(|e| e.t)
            .collect()
    }

    pub fn marks(&self) -> impl Iterator<Item = &Mark> {
        self.events.iter().filter_map(|e| match &e.kind {
            Kind::Mark(m) => Some(m),
            _ => None,
        })
    }
}
