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
use crate::workflow::queue::TaskQueue;
use crate::window::pane::PtyKind;
use crate::window::tab::{PaneNode, SplitDir, Tab};
use crate::window::WindowManager;

/// Number of trailing terminal lines kept per tab when saving a session.
const MAX_SCROLLBACK_LINES: usize = 500;
/// How far back into scrollback to search for a working-directory hint
/// once the live grid has been scanned with no luck.
const CWD_SCROLLBACK_SCAN: usize = 100;

/// One window's tabs. Also the whole legacy (single-window) session file.
pub struct SessionState {
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub timestamp: u64,
}

/// Position and size of a window in logical (DPI independent) pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// A saved window: its tabs plus where it sat on screen.
pub struct WindowSession {
    pub geometry: Option<Geometry>,
    pub state: SessionState,
}

/// The whole session file: every window, in creation order.
///
/// On disk the FIRST window keeps the legacy top-level keys (`tabs`,
/// `active_tab`, `timestamp`) so older builds still restore it; its
/// `geometry` sits beside them. Further windows are listed under `windows`.
/// Files written by older builds simply have no `windows` / `geometry`.
pub struct SessionFile {
    pub windows: Vec<WindowSession>,
    /// Index into `windows` of the window that had focus.
    pub focused: usize,
}

pub struct TabState {
    pub title: String,
    pub working_dir: String,
    pub scrollback: Vec<String>,
    /// Split layout (structure, ratios, per-pane cwd); None for a single pane.
    pub layout: Option<PaneNode<Option<String>>>,
    /// In-order index of the focused pane within `layout`.
    pub active_pane: usize,
    /// Task queues of agents (workflows), by in-order pane index.
    pub queues: Vec<(usize, TaskQueue)>,
}

// ── Public API ──

/// Serialize the window manager's tabs to `~/.config/rift/session.json`.
/// Called on app exit so the next launch can restore where the user left
/// off.
pub fn save_session(wm: &WindowManager) -> Result<(), String> {
    save_session_with(wm, &[])
}

/// Like [`save_session`], also storing each tab's task queues
/// (`queues[tab]` = (in-order pane index, queue) pairs).
pub fn save_session_with(wm: &WindowManager, queues: &[Vec<(usize, TaskQueue)>]) -> Result<(), String> {
    write_session_file(&SessionFile { windows: vec![WindowSession { geometry: None, state: snapshot(wm, queues) }], focused: 0 })
}

/// Save every window (`windows` in creation order, `focused` indexing it).
pub fn save_windows(windows: Vec<WindowSession>, focused: usize) -> Result<(), String> {
    if windows.is_empty() {
        return Ok(());
    }
    let focused = focused.min(windows.len() - 1);
    write_session_file(&SessionFile { windows, focused })
}

fn write_session_file(file: &SessionFile) -> Result<(), String> {
    let path = session_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, file.to_json()).map_err(|e| format!("write {}: {e}", path.display()))?;
    let tabs: usize = file.windows.iter().map(|w| w.state.tabs.len()).sum();
    log::info!("Session saved: {} window(s), {tabs} tab(s) -> {}", file.windows.len(), path.display());
    Ok(())
}

/// Capture one window manager (`queues[tab]` = task queues by in-order pane index).
pub fn snapshot(wm: &WindowManager, queues: &[Vec<(usize, TaskQueue)>]) -> SessionState {
    let tabs: Vec<TabState> = wm.tabs.iter().enumerate().map(|(ti, tab)| {
        let terminal = &tab.active_pane().terminal;
        TabState {
            title: tab.title.clone(),
            working_dir: detect_working_dir(terminal),
            scrollback: extract_scrollback(terminal, MAX_SCROLLBACK_LINES),
            layout: (tab.pane_count() > 1).then(|| {
                tab.root.map_leaves(&mut |pane| match pane.pty {
                    // A remote shell's cwd means nothing locally.
                    PtyKind::Ssh(_) | PtyKind::Inert => None,
                    PtyKind::Local(_) => Some(detect_working_dir(&pane.terminal)).filter(|d| d != "~"),
                })
            }),
            active_pane: tab.active,
            queues: queues.get(ti).cloned().unwrap_or_default(),
        }
    }).collect();

    SessionState {
        tabs,
        active_tab: wm.active_tab,
        timestamp: now_secs(),
    }
}

/// Read back a previously saved session's FIRST window, if any (the whole
/// legacy session). Returns `None` if there is no session file, or if it fails
/// to parse (treated as "no session" rather than a hard error, since a
/// corrupt session should never block startup).
pub fn load_session() -> Option<SessionState> {
    load_session_file().and_then(|f| f.windows.into_iter().next()).map(|w| w.state)
}

/// Read back every saved window.
pub fn load_session_file() -> Option<SessionFile> {
    let path = session_path();
    let content = std::fs::read_to_string(&path).ok()?;
    match SessionFile::from_json(&content) {
        Some(file) => {
            log::info!("Session loaded: {} window(s) from {}", file.windows.len(), path.display());
            Some(file)
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
/// [`WindowManager`] (the first window of the session). See [`restore_window`].
pub fn restore_session(wm: &mut WindowManager) -> Vec<(usize, TaskQueue)> {
    let Some(file) = load_session_file() else { return Vec::new() };
    match file.windows.first() {
        Some(w) => restore_window(wm, w),
        None => Vec::new(),
    }
}

/// Apply one saved window onto a freshly constructed [`WindowManager`] (which
/// starts with exactly one tab/pane running a brand new shell). Creates
/// additional tabs as needed, restores titles and scrollback, and best-effort
/// `cd`s each shell back to its previous working directory. A no-op if there
/// is nothing to restore.
pub fn restore_window(wm: &mut WindowManager, window: &WindowSession) -> Vec<(usize, TaskQueue)> {
    let state = &window.state;
    if state.tabs.is_empty() {
        return Vec::new();
    }

    let (cols, rows) = {
        let terminal = &wm.active_pane().terminal;
        (terminal.cols, terminal.rows)
    };

    restore_tab(wm, 0, &state.tabs[0]);
    for tab_state in state.tabs.iter().skip(1) {
        wm.new_tab(cols, rows);
        let idx = wm.tabs.len() - 1;
        restore_tab(wm, idx, tab_state);
    }

    let last = wm.tabs.len() - 1;
    wm.active_tab = state.active_tab.min(last);
    wm.renumber_tabs();
    log::info!("Window restored: {} tab(s)", wm.tabs.len());
    state.tabs.iter().enumerate().flat_map(|(ti, t)| queues_to_panes(wm, ti, t)).collect()
}

/// Queues of a restored tab keyed by the new pane ids (positions that no
/// longer exist are dropped).
pub fn queues_to_panes(wm: &WindowManager, tab_idx: usize, state: &TabState) -> Vec<(usize, TaskQueue)> {
    let Some(tab) = wm.tabs.get(tab_idx) else { return Vec::new() };
    let panes = tab.panes();
    state.queues.iter().filter_map(|(leaf, q)| panes.get(*leaf).map(|p| (p.id, q.clone()))).collect()
}

/// Rebuild one tab: the split layout first (spawning extra panes in their
/// saved directories), then title / scrollback / cwd on the focused pane.
fn restore_tab(wm: &mut WindowManager, idx: usize, state: &TabState) {
    let Some(layout) = state.layout.as_ref().filter(|l| count_leaves(l) > 1) else {
        apply_tab_state(&mut wm.tabs[idx], state, true);
        return;
    };
    let expanded = layout.map_leaves(&mut |cwd: &Option<String>| cwd.as_deref().map(expand_tilde));
    wm.restore_layout(idx, &expanded, state.active_pane);
    // The pre-existing pane became leaf 0; spawned panes already start in
    // their cwd, so only leaf 0 needs a `cd`.
    if let Some(dir) = first_leaf(&expanded) {
        if let Some(pane) = wm.tabs[idx].pane_mut(0) {
            pane.write(format!("cd {} 2>/dev/null\n", shell_quote(dir)).as_bytes());
        }
    }
    apply_tab_state(&mut wm.tabs[idx], state, false);
}

fn count_leaves<T>(node: &PaneNode<T>) -> usize {
    match node {
        PaneNode::Leaf(_) => 1,
        PaneNode::Split { first, second, .. } => count_leaves(first) + count_leaves(second),
        PaneNode::Empty => 0,
    }
}

fn first_leaf(node: &PaneNode<Option<String>>) -> Option<&str> {
    match node {
        PaneNode::Leaf(cwd) => cwd.as_deref(),
        PaneNode::Split { first, .. } => first_leaf(first),
        PaneNode::Empty => None,
    }
}

/// `~` / `~/x` -> absolute path (prompt scraping yields home-relative paths).
fn expand_tilde(path: &str) -> String {
    match (path, dirs::home_dir()) {
        ("~", Some(home)) => home.display().to_string(),
        (p, Some(home)) if p.starts_with("~/") => home.join(&p[2..]).display().to_string(),
        (p, _) => p.to_string(),
    }
}

fn apply_tab_state(tab: &mut Tab, state: &TabState, change_dir: bool) {
    tab.title = state.title.clone();

    // Populate scrollback with the saved lines so the user can scroll up
    // and see previous output; the live grid is left alone so the fresh
    // shell's own prompt stays intact.
    let cols = tab.active_pane().terminal.cols;
    let pane = tab.active_pane_mut();
    for line in &state.scrollback {
        pane.terminal.scrollback.push_back(text_to_row(line, cols));
    }

    // Best-effort: return the shell to its previous working directory.
    if change_dir && !state.working_dir.is_empty() && state.working_dir != "~" {
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
    // Exact path reported by the shell (OSC 7) beats scraping the prompt.
    if let Some(cwd) = terminal.cwd.as_ref().filter(|c| !c.is_empty()) {
        return cwd.clone();
    }
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
    // Scrollback rows are stored trimmed (no trailing default cells).
    let mut row: Vec<Cell> = line.chars().take(cols).map(Cell::plain).collect();
    while row.last().map_or(false, Cell::is_default_blank) {
        row.pop();
    }
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
    /// JSON of a single-window session (the legacy file format).
    #[allow(dead_code)]
    fn to_json(&self) -> String {
        file_json(self, &[])
    }

    #[allow(dead_code)]
    fn from_json(input: &str) -> Option<Self> {
        SessionFile::from_json(input)?.windows.into_iter().next().map(|w| w.state)
    }
}

/// Top-level object: window 0's legacy keys (`active_tab`, `timestamp`,
/// `tabs`), followed by the pre-rendered `extra` members.
fn file_json(first: &SessionState, extra: &[String]) -> String {
    let mut s = String::new();
    s.push_str("{\n");
    s.push_str(&format!("  \"active_tab\": {},\n", first.active_tab));
    s.push_str(&format!("  \"timestamp\": {},\n", first.timestamp));
    s.push_str("  \"tabs\": [\n");
    tabs_to_json(&first.tabs, &mut s);
    s.push_str("  ]");
    for member in extra {
        s.push_str(",\n  ");
        s.push_str(member);
    }
    s.push_str("\n}\n");
    s
}

impl SessionFile {
    fn to_json(&self) -> String {
        let Some((first, rest)) = self.windows.split_first() else { return "{}\n".to_string() };
        let mut extra = Vec::new();
        if let Some(g) = &first.geometry {
            extra.push(format!("\"geometry\": {}", geometry_json(g)));
        }
        if self.focused > 0 && self.focused < self.windows.len() {
            extra.push(format!("\"focused_window\": {}", self.focused));
        }
        if !rest.is_empty() {
            let objs: Vec<String> = rest.iter().map(extra_window_json).collect();
            extra.push(format!("\"windows\": [\n{}\n  ]", objs.join(",\n")));
        }
        file_json(&first.state, &extra)
    }

    fn from_json(input: &str) -> Option<Self> {
        let mut p = JsonParser::new(input);
        let value = p.parse_value()?;
        let obj = value.as_object()?;

        // Window 0: the legacy top-level keys.
        let mut windows = vec![window_from_json(obj)?];
        // Further windows (absent in legacy files; damaged entries are skipped).
        if let Some(list) = obj_get(obj, "windows").and_then(JsonValue::as_array) {
            for w in list {
                if let Some(ws) = w.as_object().and_then(window_from_json) {
                    windows.push(ws);
                }
            }
        }
        let focused = obj_get(obj, "focused_window").and_then(JsonValue::as_u64).unwrap_or(0) as usize;
        Some(SessionFile { focused: focused.min(windows.len() - 1), windows })
    }
}

/// An entry of the `windows` array (every window after the first).
fn extra_window_json(w: &WindowSession) -> String {
    let mut s = String::from("    {\n");
    if let Some(g) = &w.geometry {
        s.push_str(&format!("      \"geometry\": {},\n", geometry_json(g)));
    }
    s.push_str(&format!("      \"active_tab\": {},\n", w.state.active_tab));
    s.push_str(&format!("      \"timestamp\": {},\n", w.state.timestamp));
    s.push_str("      \"tabs\": [\n");
    tabs_to_json(&w.state.tabs, &mut s);
    s.push_str("      ]\n    }");
    s
}

fn geometry_json(g: &Geometry) -> String {
    format!("{{\"x\": {}, \"y\": {}, \"w\": {}, \"h\": {}}}", g.x, g.y, g.w, g.h)
}

fn geometry_from_json(v: &JsonValue) -> Option<Geometry> {
    let o = v.as_object()?;
    let num = |k: &str| match obj_get(o, k) {
        Some(JsonValue::Num(n)) if n.is_finite() => Some(*n),
        _ => None,
    };
    let (w, h) = (num("w")?, num("h")?);
    (w >= 1.0 && h >= 1.0).then(|| Geometry {
        x: num("x").unwrap_or(0.0) as i32,
        y: num("y").unwrap_or(0.0) as i32,
        w: w as u32,
        h: h as u32,
    })
}

fn tabs_to_json(tabs: &[TabState], s: &mut String) {
    for (i, tab) in tabs.iter().enumerate() {
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
        s.push_str("      ]");
        if !tab.queues.is_empty() {
            s.push_str(",\n      \"queues\": [");
            for (k, (leaf, q)) in tab.queues.iter().enumerate() {
                if k > 0 {
                    s.push_str(", ");
                }
                s.push_str(&format!("{{\"pane\": {leaf}, \"paused\": {}, \"tasks\": [", q.paused));
                for (j, t) in q.tasks.iter().enumerate() {
                    if j > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&format!("\"{}\"", json_escape(t)));
                }
                s.push_str("]}");
            }
            s.push(']');
        }
        if let Some(layout) = &tab.layout {
            s.push_str(&format!(",\n      \"active_pane\": {},\n      \"layout\": ", tab.active_pane));
            layout_to_json(layout, s);
        }
        s.push_str("\n    }");
        if i + 1 < tabs.len() { s.push(','); }
        s.push('\n');
    }
}

/// One window object (`active_tab`, `timestamp`, `tabs`, optional `geometry`).
fn window_from_json(obj: &[(String, JsonValue)]) -> Option<WindowSession> {
    let active_tab = obj_get(obj, "active_tab").and_then(JsonValue::as_u64)? as usize;
    let timestamp = obj_get(obj, "timestamp").and_then(JsonValue::as_u64).unwrap_or(0);
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
        let layout = obj_get(tobj, "layout").and_then(|v| layout_from_json(v, 0));
        let active_pane = obj_get(tobj, "active_pane").and_then(JsonValue::as_u64).unwrap_or(0) as usize;
        let queues = obj_get(tobj, "queues").and_then(JsonValue::as_array).map(queues_from_json).unwrap_or_default();
        tabs.push(TabState { title, working_dir, scrollback, layout, active_pane, queues });
    }
    let geometry = obj_get(obj, "geometry").and_then(geometry_from_json);
    Some(WindowSession { geometry, state: SessionState { tabs, active_tab, timestamp } })
}

fn queues_from_json(items: &[JsonValue]) -> Vec<(usize, TaskQueue)> {
    items
        .iter()
        .filter_map(|v| {
            let o = v.as_object()?;
            let pane = obj_get(o, "pane").and_then(JsonValue::as_u64)? as usize;
            let mut q = TaskQueue::default();
            for t in obj_get(o, "tasks").and_then(JsonValue::as_array)?.iter().filter_map(JsonValue::as_str) {
                q.push(t);
            }
            q.paused = matches!(obj_get(o, "paused"), Some(JsonValue::Bool(true)));
            (!q.is_empty()).then_some((pane, q))
        })
        .collect()
}

fn layout_to_json(node: &PaneNode<Option<String>>, out: &mut String) {
    match node {
        PaneNode::Leaf(Some(cwd)) => out.push_str(&format!("{{\"cwd\": \"{}\"}}", json_escape(cwd))),
        PaneNode::Leaf(None) | PaneNode::Empty => out.push_str("{}"),
        PaneNode::Split { dir, ratio, first, second } => {
            let d = if *dir == SplitDir::Horizontal { "h" } else { "v" };
            out.push_str(&format!("{{\"dir\": \"{d}\", \"ratio\": {ratio:.4}, \"first\": "));
            layout_to_json(first, out);
            out.push_str(", \"second\": ");
            layout_to_json(second, out);
            out.push('}');
        }
    }
}

/// Maximum nesting accepted when reading a layout (guards a corrupt file).
const MAX_LAYOUT_DEPTH: usize = 256;

fn layout_from_json(v: &JsonValue, depth: usize) -> Option<PaneNode<Option<String>>> {
    if depth > MAX_LAYOUT_DEPTH {
        return None;
    }
    let obj = v.as_object()?;
    let Some(dir) = obj_get(obj, "dir").and_then(JsonValue::as_str) else {
        let cwd = obj_get(obj, "cwd").and_then(JsonValue::as_str).map(str::to_string);
        return Some(PaneNode::Leaf(cwd));
    };
    let dir = match dir {
        "h" => SplitDir::Horizontal,
        "v" => SplitDir::Vertical,
        _ => return None,
    };
    let ratio = match obj_get(obj, "ratio") {
        Some(JsonValue::Num(n)) => (*n as f32).clamp(0.05, 0.95),
        _ => 0.5,
    };
    Some(PaneNode::Split {
        dir,
        ratio,
        first: Box::new(layout_from_json(obj_get(obj, "first")?, depth + 1)?),
        second: Box::new(layout_from_json(obj_get(obj, "second")?, depth + 1)?),
    })
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

/// Maximum JSON nesting (objects/arrays) accepted when parsing a session file.
/// Layouts nest two JSON levels per split, so this comfortably fits MAX_LAYOUT_DEPTH.
const MAX_JSON_DEPTH: usize = 2 * MAX_LAYOUT_DEPTH + 16;

struct JsonParser {
    chars: Vec<char>,
    pos: usize,
    depth: usize,
}

impl JsonParser {
    fn new(src: &str) -> Self {
        Self { chars: src.chars().collect(), pos: 0, depth: 0 }
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
            '{' | '[' => {
                // Bound recursion so a corrupt/hostile session file can't overflow the stack.
                if self.depth >= MAX_JSON_DEPTH {
                    return None;
                }
                self.depth += 1;
                let v = if self.peek() == Some('{') { self.parse_object() } else { self.parse_array() };
                self.depth -= 1;
                v
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(c: Option<&str>) -> Box<PaneNode<Option<String>>> {
        Box::new(PaneNode::Leaf(c.map(str::to_string)))
    }

    #[test]
    fn scrollback_extract_and_restore_handle_trimmed_rows() {
        let mut t = Terminal::new(20, 3);
        let mut p = vte::Parser::new();
        let mut h = crate::terminal::AnsiHandler::new(&mut t);
        for b in b"one\r\ntwo  \r\n\r\nfour\r\nfive\r\nsix\r\n" {
            p.advance(&mut h, *b);
        }
        assert!(t.scrollback.iter().all(|r| r.len() < 20));
        let lines = extract_scrollback(&t, 100);
        assert_eq!(&lines[..4], ["one", "two", "", "four"]);
        // Restoring produces trimmed rows that read back identically.
        for l in &lines {
            let row = text_to_row(l, 20);
            assert_eq!(row_to_string(&row), *l);
            assert!(row.last().map_or(true, |c| !c.is_default_blank()));
        }
        assert!(text_to_row("", 20).is_empty());
        assert_eq!(text_to_row("abcdefghijklmnopqrstuvwxyz", 20).len(), 20);
    }

    #[test]
    fn layout_round_trips_through_session_json() {
        let layout = PaneNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.3,
            first: leaf(Some("/tmp/a \"b\"")),
            second: Box::new(PaneNode::Split {
                dir: SplitDir::Vertical,
                ratio: 0.75,
                first: leaf(None),
                second: leaf(Some("/var")),
            }),
        };
        let state = SessionState {
            tabs: vec![
                TabState {
                    title: "t".into(),
                    working_dir: "/x".into(),
                    scrollback: vec!["hi".into()],
                    layout: Some(layout),
                    active_pane: 2,
                    queues: Vec::new(),
                },
                TabState { title: "u".into(), working_dir: "~".into(), scrollback: vec![], layout: None, active_pane: 0, queues: Vec::new() },
            ],
            active_tab: 1,
            timestamp: 7,
        };
        let back = SessionState::from_json(&state.to_json()).expect("parses");
        assert_eq!(back.tabs.len(), 2);
        assert!(back.tabs[1].layout.is_none());
        assert_eq!(back.tabs[0].active_pane, 2);
        let l = back.tabs[0].layout.as_ref().expect("layout kept");
        assert_eq!(count_leaves(l), 3);
        assert_eq!(first_leaf(l), Some("/tmp/a \"b\""));
        match l {
            PaneNode::Split { dir, ratio, second, .. } => {
                assert_eq!(*dir, SplitDir::Horizontal);
                assert!((ratio - 0.3).abs() < 1e-4);
                match &**second {
                    PaneNode::Split { dir, ratio, second, .. } => {
                        assert_eq!(*dir, SplitDir::Vertical);
                        assert!((ratio - 0.75).abs() < 1e-4);
                        assert!(matches!(&**second, PaneNode::Leaf(Some(p)) if p == "/var"));
                    }
                    _ => panic!("inner split lost"),
                }
            }
            _ => panic!("root split lost"),
        }
    }

    #[test]
    fn old_sessions_without_layout_still_load() {
        let json = r#"{"active_tab":0,"timestamp":1,"tabs":[{"title":"a","working_dir":"~","scrollback":[]}]}"#;
        let s = SessionState::from_json(json).expect("parses");
        assert!(s.tabs[0].layout.is_none());
    }

    #[test]
    fn expand_tilde_handles_home_relative_paths() {
        assert_eq!(expand_tilde("/abs"), "/abs");
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expand_tilde("~"), home.display().to_string());
            assert_eq!(expand_tilde("~/code"), home.join("code").display().to_string());
        }
    }

    #[test]
    fn hostile_deeply_nested_json_does_not_overflow() {
        let deep = "[".repeat(100_000) + &"]".repeat(100_000);
        assert!(JsonParser::new(&deep).parse_value().is_none());
    }


    fn queue(tasks: &[&str], paused: bool) -> TaskQueue {
        let mut q = TaskQueue::default();
        for t in tasks {
            q.push(t);
        }
        q.paused = paused;
        q
    }

    #[test]
    fn task_queues_round_trip_through_session_json() {
        let state = SessionState {
            tabs: vec![
                TabState {
                    title: "agents".into(),
                    working_dir: "~".into(),
                    scrollback: vec![],
                    layout: None,
                    active_pane: 0,
                    queues: vec![
                        (0, queue(&["fix the \"login\" test", "two\nlines\twith \\ and \u{4e2d}\u{6587} \u{1f642}", "third"], false)),
                        (2, queue(&["only one"], true)),
                    ],
                },
                TabState { title: "plain".into(), working_dir: "~".into(), scrollback: vec![], layout: None, active_pane: 0, queues: Vec::new() },
            ],
            active_tab: 0,
            timestamp: 9,
        };
        let json = state.to_json();
        assert!(json.contains("\"queues\""));
        let back = SessionState::from_json(&json).expect("parses");
        assert_eq!(back.tabs[0].queues, state.tabs[0].queues);
        assert!(back.tabs[1].queues.is_empty());
        assert!(!json.split("\"title\": \"plain\"").nth(1).unwrap_or("").contains("queues"), "tabs without queues write no key");
        // Old files, and damaged queue entries, load without queues.
        let old = r#"{"active_tab":0,"timestamp":1,"tabs":[{"title":"a","working_dir":"~","scrollback":[]}]}"#;
        assert!(SessionState::from_json(old).unwrap().tabs[0].queues.is_empty());
        let bad = r#"{"active_tab":0,"timestamp":1,"tabs":[{"title":"a","working_dir":"~","scrollback":[],"queues":[{"pane":"x"},{"pane":1,"tasks":[]},{"pane":3,"tasks":["ok",5]}]}]}"#;
        let q = &SessionState::from_json(bad).unwrap().tabs[0].queues;
        assert_eq!(q.len(), 1);
        assert_eq!((q[0].0, q[0].1.tasks.as_slice()), (3, &["ok".to_string()][..]));
    }

    #[test]
    fn restored_queues_attach_to_the_new_pane_ids() {
        let mut wm = WindowManager::headless(80, 24);
        let a = crate::window::PaneRect { x: 0, y: 30, width: 2400, height: 1400 };
        let min = crate::window::tab::MinSize::from_cells(10, 20);
        wm.split_active(SplitDir::Horizontal, a, min);
        let ids: Vec<usize> = wm.tabs[0].panes().iter().map(|p| p.id).collect();
        assert!(ids.len() >= 2, "{ids:?}");
        let st = TabState {
            title: "t".into(),
            working_dir: "~".into(),
            scrollback: vec![],
            layout: None,
            active_pane: 0,
            queues: vec![(1, queue(&["x"], false)), (7, queue(&["gone"], false))],
        };
        let mapped = queues_to_panes(&wm, 0, &st);
        assert_eq!(mapped.len(), 1, "position 7 no longer exists");
        assert_eq!(mapped[0].0, ids[1]);
        assert_eq!(mapped[0].1.tasks, ["x"]);
        assert!(queues_to_panes(&wm, 5, &st).is_empty());
    }

    // ── multiple windows ──

    fn tab(title: &str, cwd: &str) -> TabState {
        TabState { title: title.into(), working_dir: cwd.into(), scrollback: vec![], layout: None, active_pane: 0, queues: Vec::new() }
    }

    fn two_window_file() -> SessionFile {
        SessionFile {
            windows: vec![
                WindowSession {
                    geometry: Some(Geometry { x: 40, y: 60, w: 900, h: 700 }),
                    state: SessionState { tabs: vec![tab("main", "/a"), tab("logs", "/var/log")], active_tab: 1, timestamp: 5 },
                },
                WindowSession {
                    geometry: Some(Geometry { x: -300, y: 20, w: 640, h: 480 }),
                    state: SessionState { tabs: vec![tab("second \"win\"", "/b")], active_tab: 0, timestamp: 5 },
                },
            ],
            focused: 1,
        }
    }

    #[test]
    fn two_windows_round_trip_with_geometry_and_focus() {
        let file = two_window_file();
        let back = SessionFile::from_json(&file.to_json()).expect("parses");
        assert_eq!(back.windows.len(), 2);
        assert_eq!(back.focused, 1);
        assert_eq!(back.windows[0].geometry, Some(Geometry { x: 40, y: 60, w: 900, h: 700 }));
        assert_eq!(back.windows[1].geometry, Some(Geometry { x: -300, y: 20, w: 640, h: 480 }));
        assert_eq!(back.windows[0].state.active_tab, 1);
        let titles: Vec<&str> = back.windows[0].state.tabs.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["main", "logs"]);
        assert_eq!(back.windows[1].state.tabs[0].title, "second \"win\"");
        assert_eq!(back.windows[1].state.tabs[0].working_dir, "/b");
    }

    #[test]
    fn split_layouts_survive_in_a_second_window() {
        let layout = PaneNode::Split { dir: SplitDir::Vertical, ratio: 0.4, first: leaf(Some("/x")), second: leaf(Some("/y")) };
        let mut file = two_window_file();
        file.windows[1].state.tabs[0].layout = Some(layout);
        file.windows[1].state.tabs[0].active_pane = 1;
        let back = SessionFile::from_json(&file.to_json()).unwrap();
        let t = &back.windows[1].state.tabs[0];
        assert_eq!(t.active_pane, 1);
        assert_eq!(count_leaves(t.layout.as_ref().expect("layout")), 2);
    }

    #[test]
    fn legacy_single_window_files_load_as_one_window() {
        let old = r#"{"active_tab":1,"timestamp":1,"tabs":[{"title":"a","working_dir":"~","scrollback":[]},{"title":"b","working_dir":"/x","scrollback":[]}]}"#;
        let f = SessionFile::from_json(old).expect("parses");
        assert_eq!(f.windows.len(), 1);
        assert_eq!(f.focused, 0);
        assert!(f.windows[0].geometry.is_none());
        assert_eq!(f.windows[0].state.tabs.len(), 2);
        assert_eq!(f.windows[0].state.active_tab, 1);
    }

    #[test]
    fn a_single_window_file_is_byte_compatible_with_the_legacy_format() {
        let one = SessionFile { windows: vec![two_window_file().windows.remove(1)], focused: 0 };
        let mut no_geo = one;
        no_geo.windows[0].geometry = None;
        let json = no_geo.to_json();
        assert_eq!(json, no_geo.windows[0].state.to_json());
        assert!(!json.contains("windows") && !json.contains("geometry") && !json.contains("focused"));
    }

    #[test]
    fn first_window_keeps_the_legacy_top_level_keys() {
        // An older build only reads active_tab / timestamp / tabs of the top level
        // and ignores everything else: it must find window 0 there.
        let json = two_window_file().to_json();
        let legacy = SessionState::from_json(&json).expect("legacy reader parses");
        assert_eq!(legacy.tabs.len(), 2);
        assert_eq!(legacy.active_tab, 1);
        let v = JsonParser::new(&json).parse_value().unwrap();
        let o = v.as_object().unwrap();
        assert!(obj_get(o, "tabs").is_some() && obj_get(o, "active_tab").is_some() && obj_get(o, "windows").is_some());
    }

    #[test]
    fn damaged_extra_windows_are_skipped_not_fatal() {
        let json = r#"{"active_tab":0,"timestamp":1,"tabs":[{"title":"a","working_dir":"~","scrollback":[]}],
            "focused_window": 9,
            "windows":[{"tabs":"nope"},{"active_tab":0,"timestamp":1,"geometry":{"w":0,"h":0},"tabs":[{"title":"ok","working_dir":"~","scrollback":[]}]}]}"#;
        let f = SessionFile::from_json(json).expect("parses");
        assert_eq!(f.windows.len(), 2);
        assert_eq!(f.windows[1].state.tabs[0].title, "ok");
        assert!(f.windows[1].geometry.is_none(), "zero-sized geometry is ignored");
        assert_eq!(f.focused, 1, "focused index is clamped");
    }

    #[test]
    fn each_window_restores_into_its_own_manager_with_distinct_pane_ids() {
        let file = two_window_file();
        let mut w0 = WindowManager::headless_for_window(80, 24, 0);
        let mut w1 = WindowManager::headless_for_window(80, 24, 1);
        restore_window(&mut w0, &file.windows[0]);
        restore_window(&mut w1, &file.windows[1]);
        assert_eq!(w0.tabs.len(), 2);
        assert_eq!(w0.active_tab, 1);
        assert_eq!(w1.tabs.len(), 1);
        let ids0: Vec<usize> = w0.tabs.iter().flat_map(|t| t.panes()).map(|p| p.id).collect();
        let ids1: Vec<usize> = w1.tabs.iter().flat_map(|t| t.panes()).map(|p| p.id).collect();
        assert!(ids0.iter().all(|i| !ids1.contains(i)), "{ids0:?} vs {ids1:?}");
        assert!(ids0.iter().all(|i| crate::app::windows::pane_window(*i) == 0));
        assert!(ids1.iter().all(|i| crate::app::windows::pane_window(*i) == 1));
    }

    #[test]
    fn snapshot_of_a_manager_matches_what_restore_expects() {
        let mut wm = WindowManager::headless_for_window(80, 24, 3);
        wm.new_tab(80, 24);
        wm.rename_tab(1, "work");
        let st = snapshot(&wm, &[]);
        assert_eq!(st.tabs.len(), 2);
        assert_eq!(st.tabs[1].title, "work");
        assert_eq!(st.active_tab, 1);
    }
}
