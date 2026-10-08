//! Session persistence — save and restore terminal state (tabs, titles,
//! working directories, scrollback) across application restarts.
//!
//! The session is stored as JSON at `~/.config/rift/session.json`. Rift has
//! no `serde`/`serde_json` dependency (the project already hand-rolls its
//! own minimal TOML reader/writer for `config.toml`, see `config::toml`),
//! so this module includes a small, self-contained JSON encoder/decoder
//! that is just enough to round-trip `SessionState`.

use std::path::PathBuf;

use crate::terminal::{Cell, Terminal};
use crate::window::tab::Tab;
use crate::window::WindowManager;

/// Number of trailing terminal lines kept per tab when saving a session.
const MAX_SCROLLBACK_LINES: usize = 500;
/// How far back into scrollback to search for a working-directory hint
/// once the live grid has been scanned with no luck.
const CWD_SCROLLBACK_SCAN: usize = 100;

pub struct SessionState {
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub timestamp: u64,
}

pub struct TabState {
    pub title: String,
    pub working_dir: String,
    pub scrollback: Vec<String>,
}

// ── Public API ──

/// Serialize the window manager's tabs to `~/.config/rift/session.json`.
/// Called on app exit so the next launch can restore where the user left
/// off.
pub fn save_session(wm: &WindowManager) -> Result<(), String> {
    let tabs: Vec<TabState> = wm.tabs.iter().map(|tab| {
        let terminal = &tab.active_pane().terminal;
        TabState {
            title: tab.title.clone(),
            working_dir: detect_working_dir(terminal),
            scrollback: extract_scrollback(terminal, MAX_SCROLLBACK_LINES),
        }
    }).collect();

    let state = SessionState {
        tabs,
        active_tab: wm.active_tab,
        timestamp: now_secs(),
    };

    let path = session_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, state.to_json()).map_err(|e| format!("write {}: {e}", path.display()))?;
    log::info!("Session saved: {} tab(s) -> {}", state.tabs.len(), path.display());
    Ok(())
}

/// Read back a previously saved session, if any. Returns `None` if there is
/// no session file, or if it fails to parse (treated as "no session" rather
/// than a hard error, since a corrupt session should never block startup).
pub fn load_session() -> Option<SessionState> {
    let path = session_path();
    let content = std::fs::read_to_string(&path).ok()?;
    match SessionState::from_json(&content) {
        Some(state) => {
            log::info!("Session loaded: {} tab(s) from {}", state.tabs.len(), path.display());
            Some(state)
        }
        None => {
            log::warn!("Session file at {} is malformed, ignoring", path.display());
            None
        }
    }
}

/// Delete the saved session file, if it exists.
pub fn clear_session() {
    let path = session_path();
    match std::fs::remove_file(&path) {
        Ok(()) => log::info!("Session cleared: {}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("Failed to clear session {}: {e}", path.display()),
    }
}

/// Apply a previously saved session onto a freshly constructed
/// [`WindowManager`] (which starts with exactly one tab/pane running a
/// brand new shell). Creates additional tabs as needed, restores titles
/// and scrollback, and best-effort `cd`s each shell back to its previous
/// working directory. A no-op if there is nothing to restore.
pub fn restore_session(wm: &mut WindowManager) {
    let Some(state) = load_session() else { return };
    if state.tabs.is_empty() {
        return;
    }

    let (cols, rows) = {
        let terminal = &wm.active_pane().terminal;
        (terminal.cols, terminal.rows)
    };

    apply_tab_state(&mut wm.tabs[0], &state.tabs[0]);
    for tab_state in state.tabs.iter().skip(1) {
        wm.new_tab(cols, rows);
        let idx = wm.tabs.len() - 1;
        apply_tab_state(&mut wm.tabs[idx], tab_state);
    }

    let last = wm.tabs.len() - 1;
    wm.active_tab = state.active_tab.min(last);
    wm.renumber_tabs();
    log::info!("Session restored: {} tab(s)", wm.tabs.len());
}

fn apply_tab_state(tab: &mut Tab, state: &TabState) {
    tab.title = state.title.clone();

    // Populate scrollback with the saved lines so the user can scroll up
    // and see previous output; the live grid is left alone so the fresh
    // shell's own prompt stays intact.
    let cols = tab.active_pane().terminal.cols;
    let pane = tab.active_pane_mut();
    for line in &state.scrollback {
        pane.terminal.scrollback.push(text_to_row(line, cols));
    }

    // Best-effort: return the shell to its previous working directory.
    if !state.working_dir.is_empty() && state.working_dir != "~" {
        let cmd = format!("cd {} 2>/dev/null\n", shell_quote(&state.working_dir));
        pane.write(cmd.as_bytes());
    }
}

// ── Working directory detection ──
//
// `Pty`/`Pane` don't currently expose the shell's OS pid, so the more
// precise `readlink /proc/<pid>/cwd` (Linux) / `lsof -p <pid>` (macOS)
// lookup isn't available without extending that plumbing. Instead we scan
// the terminal's own text for a prompt line that embeds an absolute or
// home-relative path — this covers the large majority of shell prompts
// (bash, zsh, fish, starship, oh-my-zsh, ...) with no dependency on any
// particular shell or pane internals.

fn detect_working_dir(terminal: &Terminal) -> String {
    for row in terminal.grid.iter().rev() {
        let line = row_to_string(row);
        if line.trim().is_empty() { continue; }
        if let Some(path) = extract_path_from_line(&line) {
            return path;
        }
    }
    for row in terminal.scrollback.iter().rev().take(CWD_SCROLLBACK_SCAN) {
        let line = row_to_string(row);
        if line.trim().is_empty() { continue; }
        if let Some(path) = extract_path_from_line(&line) {
            return path;
        }
    }
    "~".to_string()
}

/// Find the right-most path-looking token in a line of terminal text, e.g.
/// `user@host ~/code/rift %` -> `Some("~/code/rift")`. Returns the
/// *last* match on the line since prompts generally place the cwd just
/// before the trailing prompt glyph.
fn extract_path_from_line(line: &str) -> Option<String> {
    let chars: Vec<char> = line.trim_end().chars().collect();
    let n = chars.len();
    let mut best: Option<String> = None;
    let mut i = 0;
    while i < n {
        let c = chars[i];
        let boundary = i == 0 || {
            let prev = chars[i - 1];
            prev.is_whitespace() || prev == ':' || prev == '[' || prev == '('
        };
        if boundary && (c == '~' || c == '/') {
            let start = i;
            let mut j = i + 1;
            while j < n {
                let cj = chars[j];
                if cj.is_alphanumeric() || matches!(cj, '/' | '.' | '_' | '-' | '~') {
                    j += 1;
                } else {
                    break;
                }
            }
            let token: String = chars[start..j].iter().collect();
            if token == "~" || token == "/" || token.len() > 1 {
                best = Some(token);
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    best
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ── Scrollback <-> text conversion ──

fn row_to_string(row: &[Cell]) -> String {
    row.iter()
        .map(|cell| if cell.c == '\0' { ' ' } else { cell.c })
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn extract_scrollback(terminal: &Terminal, max_lines: usize) -> Vec<String> {
    let mut lines = Vec::with_capacity(max_lines.min(terminal.scrollback.len() + terminal.grid.len()));
    for row in &terminal.scrollback {
        lines.push(row_to_string(row));
    }
    for row in &terminal.grid {
        lines.push(row_to_string(row));
    }
    if lines.len() > max_lines {
        let skip = lines.len() - max_lines;
        lines.drain(0..skip);
    }
    lines
}

fn text_to_row(line: &str, cols: usize) -> Vec<Cell> {
    let mut row: Vec<Cell> = line.chars().take(cols).map(|c| Cell { c, ..Cell::default() }).collect();
    let width = cols.max(row.len());
    row.resize(width, Cell::default());
    row
}

// ── Paths ──

fn session_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".config")
        .join("rift")
        .join("session.json")
}

fn now_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

// ── Minimal JSON encode/decode ──
//
// Just enough JSON to round-trip `SessionState` — see the module doc
// comment for why this is hand-rolled instead of pulling in `serde_json`.

impl SessionState {
    fn to_json(&self) -> String {
        let mut s = String::new();
        s.push_str("{\n");
        s.push_str(&format!("  \"active_tab\": {},\n", self.active_tab));
        s.push_str(&format!("  \"timestamp\": {},\n", self.timestamp));
        s.push_str("  \"tabs\": [\n");
        for (i, tab) in self.tabs.iter().enumerate() {
            s.push_str("    {\n");
            s.push_str(&format!("      \"title\": \"{}\",\n", json_escape(&tab.title)));
            s.push_str(&format!("      \"working_dir\": \"{}\",\n", json_escape(&tab.working_dir)));
            s.push_str("      \"scrollback\": [\n");
            for (j, line) in tab.scrollback.iter().enumerate() {
                s.push_str("        \"");
                s.push_str(&json_escape(line));
                s.push('"');
                if j + 1 < tab.scrollback.len() { s.push(','); }
                s.push('\n');
            }
            s.push_str("      ]\n");
            s.push_str("    }");
            if i + 1 < self.tabs.len() { s.push(','); }
            s.push('\n');
        }
        s.push_str("  ]\n");
        s.push_str("}\n");
        s
    }

    fn from_json(input: &str) -> Option<Self> {
        let mut p = JsonParser::new(input);
        let value = p.parse_value()?;
        let obj = value.as_object()?;

        let active_tab = obj_get(obj, "active_tab").and_then(JsonValue::as_u64)? as usize;
        let timestamp = obj_get(obj, "timestamp").and_then(JsonValue::as_u64)?;
        let tabs_val = obj_get(obj, "tabs").and_then(JsonValue::as_array)?;

        let mut tabs = Vec::with_capacity(tabs_val.len());
        for t in tabs_val {
            let tobj = t.as_object()?;
            let title = obj_get(tobj, "title").and_then(JsonValue::as_str).unwrap_or_default().to_string();
            let working_dir = obj_get(tobj, "working_dir").and_then(JsonValue::as_str).unwrap_or("~").to_string();
            let scrollback = obj_get(tobj, "scrollback")
                .and_then(JsonValue::as_array)
                .map(|arr| arr.iter().filter_map(JsonValue::as_str).map(str::to_string).collect::<Vec<String>>())
                .unwrap_or_default();
            tabs.push(TabState { title, working_dir, scrollback });
        }

        Some(SessionState { tabs, active_tab, timestamp })
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

enum JsonValue {
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    fn as_object(&self) -> Option<&[(String, JsonValue)]> {
        match self { JsonValue::Object(o) => Some(o), _ => None }
    }
    fn as_array(&self) -> Option<&[JsonValue]> {
        match self { JsonValue::Array(a) => Some(a), _ => None }
    }
    fn as_str(&self) -> Option<&str> {
        match self { JsonValue::Str(s) => Some(s), _ => None }
    }
    fn as_u64(&self) -> Option<u64> {
        match self { JsonValue::Num(n) if *n >= 0.0 => Some(*n as u64), _ => None }
    }
}

// Free function rather than a trait on `Vec`/`[T]`: a trait method named
// `get` would be shadowed by the inherent `[T]::get` (index-based) method
// at every call site, so it's named as a plain helper instead.
fn obj_get<'a>(obj: &'a [(String, JsonValue)], key: &str) -> Option<&'a JsonValue> {
    obj.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

struct JsonParser {
    chars: Vec<char>,
    pos: usize,
}

impl JsonParser {
    fn new(src: &str) -> Self {
        Self { chars: src.chars().collect(), pos: 0 }
    }

    fn skip_ws(&mut self) {
        while let Some(&c) = self.chars.get(self.pos) {
            if c.is_whitespace() { self.pos += 1; } else { break; }
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() { self.pos += 1; }
        c
    }

    fn expect(&mut self, c: char) -> Option<()> {
        if self.peek() == Some(c) { self.pos += 1; Some(()) } else { None }
    }

    fn parse_value(&mut self) -> Option<JsonValue> {
        self.skip_ws();
        match self.peek()? {
            '{' => self.parse_object(),
            '[' => self.parse_array(),
            '"' => self.parse_string().map(JsonValue::Str),
            't' => self.parse_lit("true", JsonValue::Bool(true)),
            'f' => self.parse_lit("false", JsonValue::Bool(false)),
            'n' => self.parse_lit("null", JsonValue::Null),
            _ => self.parse_number(),
        }
    }

    fn parse_lit(&mut self, lit: &str, value: JsonValue) -> Option<JsonValue> {
        for expected in lit.chars() {
            if self.bump()? != expected { return None; }
        }
        Some(value)
    }

    fn parse_object(&mut self) -> Option<JsonValue> {
        self.expect('{')?;
        let mut entries = Vec::new();
        self.skip_ws();
        if self.peek() == Some('}') { self.pos += 1; return Some(JsonValue::Object(entries)); }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(':')?;
            let value = self.parse_value()?;
            entries.push((key, value));
            self.skip_ws();
            match self.bump()? {
                ',' => continue,
                '}' => break,
                _ => return None,
            }
        }
        Some(JsonValue::Object(entries))
    }

    fn parse_array(&mut self) -> Option<JsonValue> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') { self.pos += 1; return Some(JsonValue::Array(items)); }
        loop {
            let value = self.parse_value()?;
            items.push(value);
            self.skip_ws();
            match self.bump()? {
                ',' => continue,
                ']' => break,
                _ => return None,
            }
        }
        Some(JsonValue::Array(items))
    }

    fn parse_string(&mut self) -> Option<String> {
        self.skip_ws();
        self.expect('"')?;
        let mut s = String::new();
        loop {
            let c = self.bump()?;
            match c {
                '"' => break,
                '\\' => {
                    let esc = self.bump()?;
                    match esc {
                        '"' => s.push('"'),
                        '\\' => s.push('\\'),
                        '/' => s.push('/'),
                        'n' => s.push('\n'),
                        'r' => s.push('\r'),
                        't' => s.push('\t'),
                        'b' => s.push('\u{0008}'),
                        'f' => s.push('\u{000C}'),
                        'u' => {
                            let mut code = 0u32;
                            for _ in 0..4 {
                                let h = self.bump()?;
                                code = code * 16 + h.to_digit(16)?;
                            }
                            s.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        _ => return None,
                    }
                }
                c => s.push(c),
            }
        }
        Some(s)
    }

    fn parse_number(&mut self) -> Option<JsonValue> {
        let start = self.pos;
        if self.peek() == Some('-') { self.pos += 1; }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) { self.pos += 1; }
        if self.peek() == Some('.') {
            self.pos += 1;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) { self.pos += 1; }
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            self.pos += 1;
            if matches!(self.peek(), Some('+') | Some('-')) { self.pos += 1; }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) { self.pos += 1; }
        }
        if self.pos == start { return None; }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.parse::<f64>().ok().map(JsonValue::Num)
    }
}
