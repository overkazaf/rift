//! Ambient, context-aware AI: three entry points that live where the user
//! already is, instead of in a separate panel.
//!
//! 1. **Cmd+K "ask about this"** — a small popover anchored near whatever
//!    "this" is (selection, selected/hovered block, last block, screen; see
//!    [`context`]). Enter sends through [`crate::ai::hub::ask`].
//! 2. **Proactive fix** — when a command block fails, a background
//!    non-streaming request proposes a corrected command, shown as a
//!    suggestion bar next to the prompt (Tab inserts it, Esc dismisses).
//! 3. **`# natural language`** — Enter on a `# ...` prompt line is
//!    intercepted: the line is erased, a command is generated and typed back
//!    into the prompt (never executed) with a hint bar.
//!
//! Everything that can be decided without a window is a pure function in
//! [`context`], [`fix`], [`nl`] and [`json`] (unit-tested); this file owns the
//! state machine and the glue to `App`, [`ui`] owns the pixels.
//!
//! Hooks (all called from `app::*`): [`on_key_modal`], [`on_key`],
//! [`on_enter`], [`insert_text`], [`on_click`], [`tick`], [`draw`], [`open`].

pub mod context;
pub mod fix;
pub mod json;
pub mod line_edit;
pub mod nl;
mod ui;

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use winit::event::KeyEvent;
use winit::keyboard::{Key, NamedKey};

use crate::ai::local::Feature;
use crate::ai::LlmConfig;
use crate::app::App;
use crate::terminal::Terminal;
use crate::tools::exec_preview::ExecPreview;
use crate::ui::kit::Rect;
use crate::window::Pane;
use context::{resolve, BlockSnap, ContextModel, Resolved};
use fix::{FixKey, FixSuggestion};
use line_edit::LineEdit;

pub use ui::draw;

/// Ctrl+E (end of line) then Ctrl+U (kill line): clears whatever the shell is
/// editing, in readline, zsh and fish alike.
const ERASE_LINE: &[u8] = b"\x05\x15";
const TOAST_TTL: Duration = Duration::from_millis(4000);
const FIX_DEBOUNCE: Duration = Duration::from_secs(2);
/// A block counts as "just failed" if it finished within this many seconds.
const FRESH_SECS: u64 = 5;
const CACHE_CAP: usize = 64;
/// Lines of block output captured for context.
const MAX_OUTPUT_LINES: usize = 200;
const MAX_SCREEN_LINES: usize = 60;
pub const NO_ROUTE_MSG: &str = "No model matches this feature's provider setting ([ai] fix_provider / nl_provider)";
pub const UNCONFIGURED_MSG: &str = "Configure an AI provider in Preferences to enable AI";

// ── Pure helpers ──

/// Is an LLM provider usable (key present, or a local Ollama-style endpoint)?
///
/// This only says a provider is reachable. Whether the user *agreed* to use
/// it is [`crate::ai::consent::allowed`]; the inline entry points use [`ai_ok`].
pub fn llm_ready(c: &LlmConfig) -> bool {
    let local = crate::ai::local::usage::is_loopback_url(&c.api_url);
    c.enabled && (c.api_key.as_deref().is_some_and(|k| !k.is_empty()) || c.provider == "ollama" || local)
}

/// Did a block that started at `ts` and ran `dur_ms` finish within
/// [`FRESH_SECS`] of `now` (all unix seconds)?
pub fn finished_recently(ts: u64, dur_ms: u64, now: u64) -> bool {
    now.saturating_sub(ts + dur_ms / 1000) <= FRESH_SECS
}

/// Identity of the newest finished block of a pane; a change means a command
/// just completed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockSig {
    pub pane_id: usize,
    pub count: usize,
    pub ts: u64,
    pub dur_ms: u64,
    pub cmd_hash: u64,
}

impl BlockSig {
    pub fn of(pane_id: usize, t: &Terminal) -> Self {
        let blocks = t.blocks.blocks();
        match blocks.last() {
            Some(b) => {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                b.command.hash(&mut h);
                Self { pane_id, count: blocks.len(), ts: b.timestamp, dur_ms: b.duration_ms, cmd_hash: h.finish() }
            }
            None => Self { pane_id, count: 0, ts: 0, dur_ms: 0, cmd_hash: 0 },
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── State ──

#[derive(Clone, Debug, PartialEq)]
pub enum PopoverMode {
    /// Ask the chat about the resolved context.
    Ask,
    /// Refine the `#` command currently sitting in the prompt.
    RefineNl { previous: String },
}

pub struct Popover {
    pub edit: LineEdit,
    pub resolved: Resolved,
    pub mode: PopoverMode,
}

struct PendingFix {
    rx: Receiver<Result<String, String>>,
    key: FixKey,
    sig: BlockSig,
    snap: BlockSnap,
}

pub struct ActiveFix {
    pub suggestion: FixSuggestion,
    pub dangerous: bool,
    pub sig: BlockSig,
    pub snap: BlockSnap,
}

#[derive(Default)]
pub struct FixUi {
    pub current: Option<ActiveFix>,
    pending: Option<PendingFix>,
}

pub struct NlPending {
    pub query: String,
    /// Text to put back if generation fails (what the user had typed).
    restore: String,
    rx: Receiver<Result<String, String>>,
    pub started: Instant,
    pub(crate) pane_id: usize,
}

pub struct NlGhost {
    pub query: String,
    pub command: String,
    pub dangerous: bool,
    pub pane_id: usize,
}

#[derive(Default)]
pub enum NlState {
    #[default]
    Idle,
    Pending(NlPending),
    Ghost(NlGhost),
}

/// Clickable areas from the last frame.
#[derive(Default, Clone, Debug)]
pub struct Hits {
    pub popover: Option<Rect>,
    pub bar: Option<Rect>,
    pub accept: Option<Rect>,
    pub dismiss: Option<Rect>,
    pub ask_more: Option<Rect>,
}

pub struct InlineAi {
    pub popover: Option<Popover>,
    pub fix: FixUi,
    pub nl: NlState,
    cache: HashMap<FixKey, Option<FixSuggestion>>,
    cache_order: VecDeque<FixKey>,
    watch: Option<BlockSig>,
    last_fix_request: Option<Instant>,
    unconfigured_toasted: bool,
    toast: Option<(String, Instant)>,
    last_spin: Instant,
    pub(crate) hits: Hits,
    pub(crate) ime_hint: Option<Rect>,
    pub(crate) started: Instant,
}

impl InlineAi {
    pub fn new() -> Self {
        Self {
            popover: None,
            fix: FixUi::default(),
            nl: NlState::Idle,
            cache: HashMap::new(),
            cache_order: VecDeque::new(),
            watch: None,
            last_fix_request: None,
            unconfigured_toasted: false,
            toast: None,
            last_spin: Instant::now(),
            hits: Hits::default(),
            ime_hint: None,
            started: Instant::now(),
        }
    }

    pub fn set_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    pub fn active_toast(&self) -> Option<&str> {
        match &self.toast {
            Some((m, t)) if t.elapsed() < TOAST_TTL => Some(m),
            _ => None,
        }
    }

    /// Where the OS IME candidate window should go while the popover is open.
    pub fn ime_hint(&self) -> Option<Rect> {
        self.ime_hint
    }

    fn cache_put(&mut self, key: FixKey, val: Option<FixSuggestion>) {
        if self.cache.insert(key.clone(), val).is_none() {
            self.cache_order.push_back(key);
            while self.cache_order.len() > CACHE_CAP {
                if let Some(old) = self.cache_order.pop_front() {
                    self.cache.remove(&old);
                }
            }
        }
    }
}

impl Default for InlineAi {
    fn default() -> Self {
        Self::new()
    }
}

// ── Environment helpers ──

/// Any modal overlay that should keep AI hotkeys out of the way. (The
/// docked chat is deliberately not listed: it can stay open while you work.)
pub fn overlay_open(app: &App) -> bool {
    app.win.exec_preview.visible
        || app.win.timewarp_browser.active
        || app.win.mui.menu.visible
        || app.win.mui.tabs.editor.is_some()
        || app.win.browser.editing
        || app.win.observer_summary.is_some()
        || app.win.welcome.visible
        || app.win.prefs.visible
        || app.win.ssh_dialog.visible
        || app.win.webview_dialog.visible
        || app.win.compare_view.visible
        || app.win.command_palette.visible
        || app.win.search.visible
        || app.win.file_manager.visible
        || app.win.git_panel.visible
        || app.win.cicd.visible
        || app.win.heatmap.visible
        || app.win.docker.visible
        || app.win.network_monitor.visible
        || app.win.process_tree.visible
        || app.mcp.overlay.visible
        || app.win.agents_ui.policy_log.visible
        || app.win.system_info.visible
        || app.win.port_dashboard.visible
        || app.win.regex_playground.visible
        || app.win.history.visible
        || app.win.autocomplete.visible
        || crate::ui::kit::gallery::visible()
}

/// Gate for the inline AI paths (`#`, auto-fix, refine): provider usable AND
/// consent granted. Nothing is sent (and a `#` line stays an ordinary shell
/// comment) otherwise; `nag` shows the one-time "not configured" toast.
///
/// "Provider usable" is judged per feature: auto-fix and `#` may be routed to
/// a local model even when the selected default is not usable.
fn ai_ok(app: &mut App, nag: bool, feature: Feature) -> bool {
    if crate::ai::local::routed(app, feature).is_some() {
        return true;
    }
    if nag {
        if !llm_ready(&app.llm.config) {
            toast_unconfigured(app);
        } else if crate::ai::consent::allowed(app) {
            // Usable default, but the feature is pinned to a provider kind we lack.
            app.win.inline_ai.set_toast(NO_ROUTE_MSG);
            app.request_redraw();
        }
    }
    false
}

fn toast_unconfigured(app: &mut App) {
    if !app.win.inline_ai.unconfigured_toasted {
        app.win.inline_ai.unconfigured_toasted = true;
        app.win.inline_ai.set_toast(UNCONFIGURED_MSG);
        app.request_redraw();
    }
}

fn env_os_shell() -> (String, String) {
    (
        std::env::consts::OS.to_string(),
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
    )
}

fn cwd_of(t: &Terminal) -> String {
    t.cwd
        .clone()
        .or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()))
        .unwrap_or_default()
}

/// Type `text` at the prompt without executing it.
fn paste_into_prompt(pane: &mut Pane, text: &str) {
    // Model output is untrusted: no ESC / paste-end markers / control bytes.
    let text = &crate::window::selection::sanitize_paste(text, pane.terminal.bracketed_paste);
    pane.terminal.scroll_to_bottom();
    if pane.terminal.bracketed_paste {
        pane.write(b"\x1b[200~");
        pane.write(text.as_bytes());
        pane.write(b"\x1b[201~");
    } else {
        pane.write(text.as_bytes());
    }
}

fn find_pane_mut(app: &mut App, id: usize) -> Option<&mut Pane> {
    for tab in &mut app.win.wm.tabs {
        for pane in tab.panes_mut() {
            if pane.id == id {
                return Some(pane);
            }
        }
    }
    None
}

fn prompt_is_empty(t: &Terminal) -> bool {
    t.typed_input().is_some_and(|s| s.is_empty())
}

fn is_dangerous(cmd: &str, cwd: Option<&str>) -> bool {
    ExecPreview::check_command_in(cmd, cwd).is_some()
}

// ── Context snapshots ──

fn screen_text(t: &Terminal) -> String {
    let mut lines: Vec<String> = t
        .visible_rows()
        .iter()
        .map(|row| crate::terminal::grid::cells_text(row).trim_end().to_string())
        .collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let skip = lines.len().saturating_sub(MAX_SCREEN_LINES);
    lines[skip..].join("\n")
}

fn snap_block_of(t: &Terminal, idx: usize) -> Option<BlockSnap> {
    let b = t.blocks.get(idx)?;
    let output = if b.running || b.output_end < b.output_start {
        String::new()
    } else {
        let start = b.output_start.max(b.output_end.saturating_sub(MAX_OUTPUT_LINES));
        crate::blocks_ui::output_text(t, start, b.output_end)
    };
    Some(BlockSnap {
        command: b.command.clone(),
        exit_code: b.exit_code,
        output,
        cwd: t.cwd.clone(),
        running: b.running,
        line: b.command_line,
    })
}

fn snap_block(app: &App, pane: usize, idx: usize) -> Option<BlockSnap> {
    snap_block_of(&app.win.wm.active_tab().pane(pane)?.terminal, idx)
}

fn build_model(app: &App) -> ContextModel {
    let tab = app.win.wm.active_tab;
    let t = &app.win.wm.active_pane().terminal;
    let selection = if app.win.selection.active {
        let text = crate::app::mouse::selection_text(app);
        let end_line = app.win.selection.normalized().1 .0;
        (!text.trim().is_empty()).then_some((text, end_line))
    } else {
        None
    };
    let selected_block = app
        .win
        .blocks_ui
        .selected
        .filter(|(t, _, _)| *t == tab)
        .and_then(|(_, p, b)| snap_block(app, p, b));
    let hovered_block = app
        .win
        .blocks_ui
        .hover
        .filter(|h| h.tab == tab && app.win.cursor_y >= app.tab_bar_height())
        .and_then(|h| snap_block(app, h.pane, h.block));
    let last_block = t.blocks.blocks().len().checked_sub(1).and_then(|i| snap_block_of(t, i));
    ContextModel { selection, selected_block, hovered_block, last_block, screen: screen_text(t) }
}

// ── Cmd+K ──

/// Open (or toggle) the ask popover. With a `#` command in the prompt this
/// refines that command instead.
pub fn open(app: &mut App) {
    if overlay_open(app) {
        return;
    }
    if app.win.inline_ai.popover.take().is_some() {
        app.request_redraw();
        return;
    }
    if matches!(app.win.inline_ai.nl, NlState::Ghost(_)) {
        if let NlState::Ghost(g) = std::mem::take(&mut app.win.inline_ai.nl) {
            app.win.inline_ai.popover = Some(Popover {
                edit: LineEdit::with_text(&g.query),
                resolved: Resolved::Nothing,
                mode: PopoverMode::RefineNl { previous: g.command },
            });
        }
        app.request_redraw();
        return;
    }
    if !llm_ready(&app.llm.config) {
        toast_unconfigured(app);
    }
    let resolved = resolve(&build_model(app));
    app.win.inline_ai.popover = Some(Popover { edit: LineEdit::default(), resolved, mode: PopoverMode::Ask });
    app.request_redraw();
}

fn submit_popover(app: &mut App) {
    let Some(pop) = app.win.inline_ai.popover.as_ref() else { return };
    let text = pop.edit.text();
    let q = text.trim();
    match pop.mode.clone() {
        PopoverMode::Ask => {
            let Some(req) = pop.resolved.to_request(q) else { return };
            app.win.inline_ai.popover = None;
            crate::ai::hub::ask(app, req);
        }
        PopoverMode::RefineNl { previous } => {
            if q.is_empty() {
                return;
            }
            let query = q.to_string();
            app.win.inline_ai.popover = None;
            if ai_ok(app, true, Feature::Nl) {
                start_nl(app, query, previous.clone(), Some(previous));
            }
        }
    }
    app.request_redraw();
}

/// Keys while the popover is open (called before overlay routing). Consumes
/// every key while open.
pub fn on_key_modal(app: &mut App, event: &KeyEvent) -> bool {
    if app.win.inline_ai.popover.is_none() {
        return false;
    }
    if overlay_open(app) {
        app.win.inline_ai.popover = None;
        return false;
    }
    let m = app.win.modifiers;
    let (mut close, mut submit) = (false, false);
    let mut paste = false;
    {
        let Some(pop) = app.win.inline_ai.popover.as_mut() else { return false };
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => close = true,
            Key::Named(NamedKey::Enter) => submit = true,
            Key::Named(NamedKey::Backspace) => pop.edit.backspace(),
            Key::Named(NamedKey::Delete) => pop.edit.delete(),
            Key::Named(NamedKey::ArrowLeft) => pop.edit.left(),
            Key::Named(NamedKey::ArrowRight) => pop.edit.right(),
            Key::Named(NamedKey::Home) => pop.edit.home(),
            Key::Named(NamedKey::End) => pop.edit.end(),
            Key::Named(NamedKey::Space) if !m.super_key() && !m.control_key() => pop.edit.insert_str(" "),
            Key::Character(s) => {
                let c = s.to_lowercase();
                if m.super_key() {
                    match c.as_str() {
                        "k" => close = true,
                        "v" => paste = true,
                        "a" => pop.edit.home(),
                        _ => {}
                    }
                } else if m.control_key() {
                    match c.as_str() {
                        "a" => pop.edit.home(),
                        "e" => pop.edit.end(),
                        "u" => pop.edit.kill_to_start(),
                        "c" => close = true,
                        _ => {}
                    }
                } else {
                    pop.edit.insert_str(s);
                }
            }
            _ => {}
        }
    }
    if paste {
        if let Some(text) = crate::window::selection::paste_from_clipboard() {
            if let Some(pop) = app.win.inline_ai.popover.as_mut() {
                pop.edit.insert_str(&text);
            }
        }
    }
    if close {
        app.win.inline_ai.popover = None;
    } else if submit {
        submit_popover(app);
    }
    app.request_redraw();
    true
}

/// Committed IME text. Returns true when the popover took it; any other
/// input also retires a `#` ghost hint (the user is editing the command).
pub fn insert_text(app: &mut App, text: &str) -> bool {
    if let Some(pop) = app.win.inline_ai.popover.as_mut() {
        pop.edit.insert_str(text);
        app.request_redraw();
        return true;
    }
    if matches!(app.win.inline_ai.nl, NlState::Ghost(_)) {
        app.win.inline_ai.nl = NlState::Idle;
        app.request_redraw();
    }
    false
}

fn is_modifier_key(k: &Key) -> bool {
    matches!(
        k,
        Key::Named(
            NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::CapsLock
                | NamedKey::Fn
                | NamedKey::FnLock
                | NamedKey::AltGraph
        )
    )
}

/// Ambient keys (run after overlay routing, so no overlay is open): Cmd+K,
/// Tab to accept a fix, Esc to dismiss a suggestion / cancel or clear a `#`
/// command. Returns true when consumed.
pub fn on_key(app: &mut App, event: &KeyEvent) -> bool {
    let m = app.win.modifiers;
    if let Key::Character(s) = &event.logical_key {
        if s.eq_ignore_ascii_case("k") && m.super_key() && !m.shift_key() && !m.alt_key() && !m.control_key() {
            if !event.repeat {
                open(app);
            }
            return true;
        }
    }
    let plain = !m.super_key() && !m.control_key() && !m.alt_key();
    match &event.logical_key {
        Key::Named(NamedKey::Tab) if plain && !m.shift_key() && accept_fix(app) => return true,
        Key::Named(NamedKey::Escape) if plain && escape(app) => return true,
        _ => {}
    }
    // Typing anything else edits the generated command: retire its hint.
    if matches!(app.win.inline_ai.nl, NlState::Ghost(_))
        && !is_modifier_key(&event.logical_key)
        && !matches!(event.logical_key, Key::Named(NamedKey::Enter | NamedKey::Escape))
    {
        app.win.inline_ai.nl = NlState::Idle;
        app.request_redraw();
    }
    false
}

fn escape(app: &mut App) -> bool {
    match std::mem::take(&mut app.win.inline_ai.nl) {
        NlState::Pending(p) => {
            // Cancel: put back what the user typed.
            if let Some(pane) = find_pane_mut(app, p.pane_id) {
                if prompt_is_empty(&pane.terminal) {
                    paste_into_prompt(pane, &p.restore);
                }
            }
            app.request_redraw();
            return true;
        }
        NlState::Ghost(g) => {
            if let Some(pane) = find_pane_mut(app, g.pane_id) {
                if pane.terminal.at_shell_prompt() {
                    pane.write(ERASE_LINE);
                }
            }
            app.request_redraw();
            return true;
        }
        NlState::Idle => {}
    }
    if active_fix(app).is_some() {
        app.win.inline_ai.fix.current = None;
        app.request_redraw();
        return true;
    }
    false
}

fn active_fix(app: &App) -> Option<&ActiveFix> {
    let pane = app.win.wm.active_pane();
    app.win.inline_ai
        .fix
        .current
        .as_ref()
        .filter(|f| f.sig.pane_id == pane.id && !pane.terminal.is_alt_screen())
}

/// Tab: type the suggested command at an empty prompt.
fn accept_fix(app: &mut App) -> bool {
    let Some(f) = active_fix(app) else { return false };
    if !prompt_is_empty(&app.win.wm.active_pane().terminal) {
        return false; // leave Tab to the shell's completion
    }
    let cmd = f.suggestion.command.clone();
    app.win.inline_ai.fix.current = None;
    paste_into_prompt(app.win.wm.active_pane_mut(), &cmd);
    app.request_redraw();
    true
}

/// "Ask more": hand the failed block to the chat as a Fix request.
fn ask_more(app: &mut App) {
    use crate::ai::hub::{AskRequest, Intent};
    let Some(f) = app.win.inline_ai.fix.current.take() else { return };
    let req = AskRequest::new("", Intent::Fix).with(f.snap.to_item()).display(format!("Fix: {}", f.snap.label()));
    crate::ai::hub::ask(app, req);
    app.request_redraw();
}

/// Enter pressed with a plain `\r` bound for the shell. Returns true when
/// intercepted (a `# ...` line at the prompt).
pub fn on_enter(app: &mut App) -> bool {
    match &app.win.inline_ai.nl {
        NlState::Ghost(_) => {
            // Running the generated command: the hint has served its purpose.
            app.win.inline_ai.nl = NlState::Idle;
            app.request_redraw();
            return false;
        }
        NlState::Pending(_) => return true, // swallow: the prompt is empty while we generate
        NlState::Idle => {}
    }
    if !app.config.ai_nl_hash || app.win.broadcast {
        return false;
    }
    let Some(typed) = app.win.wm.active_pane().terminal.typed_input() else { return false };
    let Some(query) = nl::detect_query(&typed) else { return false };
    // No consent / no provider: the line goes to the shell untouched.
    if !ai_ok(app, false, Feature::Nl) {
        return false;
    }
    start_nl(app, query, typed, None);
    true
}

fn spawn_request(config: LlmConfig, prompt: String, feature: Feature) -> Receiver<Result<String, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // Both prompts ask for one small JSON object.
        let _ = tx.send(crate::ai::backend::complete_structured(&config, &prompt, feature));
        crate::wake::wake();
    });
    rx
}

/// Erase the prompt line and ask the model for a command.
fn start_nl(app: &mut App, query: String, restore: String, previous: Option<String>) {
    // Strict: never fall back to the default model when routing says no.
    let Some(config) = crate::ai::local::routed(app, Feature::Nl) else { return };
    let (os, shell) = env_os_shell();
    let pane = app.win.wm.active_pane_mut();
    let prompt = nl::nl_prompt(&query, previous.as_deref(), &cwd_of(&pane.terminal), &os, &shell);
    pane.terminal.scroll_to_bottom();
    pane.write(ERASE_LINE);
    let pane_id = pane.id;
    app.win.inline_ai.nl = NlState::Pending(NlPending {
        query,
        restore,
        rx: spawn_request(config, prompt, Feature::Nl),
        started: Instant::now(),
        pane_id,
    });
    app.request_redraw();
}

fn poll_nl(app: &mut App) {
    let result = match &app.win.inline_ai.nl {
        NlState::Pending(p) => match p.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("request failed".to_string()),
        },
        _ => return,
    };
    let NlState::Pending(p) = std::mem::take(&mut app.win.inline_ai.nl) else { return };
    let parsed = result.and_then(|text| {
        nl::parse_nl_response(&text).ok_or_else(|| "the model did not return a usable command".to_string())
    });
    let can_place = find_pane_mut(app, p.pane_id).is_some_and(|pane| prompt_is_empty(&pane.terminal));
    match parsed {
        Ok(cmd) if can_place => {
            let cwd = find_pane_mut(app, p.pane_id).and_then(|pane| pane.terminal.cwd.clone());
            let dangerous = is_dangerous(&cmd.command, cwd.as_deref());
            if let Some(pane) = find_pane_mut(app, p.pane_id) {
                paste_into_prompt(pane, &cmd.command);
            }
            app.win.inline_ai.nl =
                NlState::Ghost(NlGhost { query: p.query, command: cmd.command, dangerous, pane_id: p.pane_id });
        }
        Ok(_) => app.win.inline_ai.set_toast("AI command discarded: the prompt changed"),
        Err(e) => {
            if can_place {
                if let Some(pane) = find_pane_mut(app, p.pane_id) {
                    paste_into_prompt(pane, &p.restore);
                }
            }
            app.win.inline_ai.set_toast(format!("AI: {}", crate::ui::trunc(&e, 120)));
        }
    }
    app.request_redraw();
}

// ── Proactive fix ──

fn poll_fix(app: &mut App) {
    let result = match &app.win.inline_ai.fix.pending {
        Some(p) => match p.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("worker died".to_string()),
        },
        None => return,
    };
    let Some(p) = app.win.inline_ai.fix.pending.take() else { return };
    let suggestion = match result {
        Ok(text) => fix::parse_fix_response(&text, &p.snap.command),
        Err(e) => {
            log::debug!("inline fix: request failed: {e}");
            return; // transient: do not cache failures
        }
    };
    app.win.inline_ai.cache_put(p.key.clone(), suggestion.clone());
    show_fix(app, suggestion, p.sig, p.snap);
}

/// Display `suggestion` if the block it was computed for is still the
/// newest one of its pane.
fn show_fix(app: &mut App, suggestion: Option<FixSuggestion>, sig: BlockSig, snap: BlockSnap) {
    let Some(suggestion) = suggestion else { return };
    let still_current = app
        .win
        .wm
        .tabs
        .iter()
        .flat_map(|t| t.panes())
        .find(|p| p.id == sig.pane_id)
        .is_some_and(|p| BlockSig::of(p.id, &p.terminal) == sig);
    if !still_current {
        return;
    }
    let dangerous = is_dangerous(&suggestion.command, snap.cwd.as_deref());
    app.win.inline_ai.fix.current = Some(ActiveFix { suggestion, dangerous, sig, snap });
    app.request_redraw();
}

/// Drop the visible suggestion once its block is stale (new command running
/// or finished, alt screen).
fn validate_fix(app: &mut App) {
    let Some(f) = &app.win.inline_ai.fix.current else { return };
    let Some(pane) = app.win.wm.tabs.iter().flat_map(|t| t.panes()).find(|p| p.id == f.sig.pane_id) else {
        app.win.inline_ai.fix.current = None;
        return;
    };
    let t = &pane.terminal;
    if BlockSig::of(pane.id, t) != f.sig || t.blocks.running_osc_elapsed_ms().is_some() || t.is_alt_screen() {
        app.win.inline_ai.fix.current = None;
        app.request_redraw();
    }
}

fn detect_failure(app: &mut App) {
    let pane = app.win.wm.active_pane();
    let t = &pane.terminal;
    if t.is_alt_screen() || !t.blocks.osc_seen() {
        return;
    }
    let sig = BlockSig::of(pane.id, t);
    let prev = app.win.inline_ai.watch.replace(sig.clone());
    let Some(prev) = prev else { return };
    if prev.pane_id != sig.pane_id || prev == sig || sig.count == 0 {
        return; // baseline (first look / pane switch) or nothing new
    }
    if !app.config.ai_auto_fix {
        return;
    }
    let Some(last) = t.blocks.blocks().last() else { return };
    let Some(exit) = last.exit_code else { return };
    if !finished_recently(last.timestamp, last.duration_ms, unix_now()) || fix::should_skip(&last.command, exit) {
        return;
    }
    if !ai_ok(app, true, Feature::Fix) {
        return;
    }
    let Some(snap) = snap_block_of(&app.win.wm.active_pane().terminal, sig.count - 1) else { return };
    let key = fix::fix_key(&snap.command, exit, &snap.output);
    if let Some(cached) = app.win.inline_ai.cache.get(&key).cloned() {
        show_fix(app, cached, sig, snap);
        return;
    }
    if app.win.inline_ai.fix.pending.is_some() {
        return;
    }
    if app.win.inline_ai.last_fix_request.is_some_and(|t| t.elapsed() < FIX_DEBOUNCE) {
        return;
    }
    app.win.inline_ai.last_fix_request = Some(Instant::now());
    let (os, shell) = env_os_shell();
    let cwd = snap.cwd.clone().unwrap_or_else(|| cwd_of(&app.win.wm.active_pane().terminal));
    let prompt = fix::fix_prompt(&snap.command, exit, &snap.output, &cwd, &os, &shell);
    let Some(config) = crate::ai::local::routed(app, Feature::Fix) else { return };
    let rx = spawn_request(config, prompt, Feature::Fix);
    app.win.inline_ai.fix.pending = Some(PendingFix { rx, key, sig, snap });
}

// ── Mouse ──

/// Left press. Returns true when the click landed on AI chrome.
pub fn on_click(app: &mut App) -> bool {
    let (x, y) = (app.win.cursor_x, app.win.cursor_y);
    let hits = app.win.inline_ai.hits.clone();
    if app.win.inline_ai.popover.is_some() {
        if hits.popover.is_some_and(|r| r.contains(x, y)) {
            return true;
        }
        app.win.inline_ai.popover = None;
        app.request_redraw();
        return false;
    }
    if !hits.bar.is_some_and(|r| r.contains(x, y)) {
        return false;
    }
    if hits.accept.is_some_and(|r| r.contains(x, y)) {
        accept_fix(app);
    } else if hits.dismiss.is_some_and(|r| r.contains(x, y)) {
        app.win.inline_ai.fix.current = None;
    } else if hits.ask_more.is_some_and(|r| r.contains(x, y)) {
        ask_more(app);
    }
    app.request_redraw();
    true
}

// ── Menu toggles ──

pub fn set_auto_fix(app: &mut App, on: bool) {
    app.config.ai_auto_fix = on;
    if !on {
        app.win.inline_ai.fix.current = None;
        app.win.inline_ai.fix.pending = None;
    }
    app.menubar.set_ai_checks(app.config.ai_auto_fix, app.config.ai_nl_hash);
    app.win.inline_ai.set_toast(format!("Auto Fix Suggestions {}", if on { "on" } else { "off" }));
    crate::config::toml::save_config(&app.config);
    app.request_redraw();
}

pub fn set_nl_hash(app: &mut App, on: bool) {
    app.config.ai_nl_hash = on;
    app.menubar.set_ai_checks(app.config.ai_auto_fix, app.config.ai_nl_hash);
    app.win.inline_ai.set_toast(format!("# Natural Language {}", if on { "on" } else { "off" }));
    crate::config::toml::save_config(&app.config);
    app.request_redraw();
}

// ── Frame tick ──

/// Called from `about_to_wait`: poll background work, retire stale UI.
pub fn tick(app: &mut App, wake_at: &mut Instant) {
    if let Some((_, t)) = &app.win.inline_ai.toast {
        let end = *t + TOAST_TTL;
        if Instant::now() >= end {
            app.win.inline_ai.toast = None;
            app.request_redraw();
        } else {
            *wake_at = (*wake_at).min(end);
        }
    }
    if app.win.inline_ai.popover.is_some() && overlay_open(app) {
        app.win.inline_ai.popover = None;
        app.request_redraw();
    }

    poll_fix(app);
    poll_nl(app);
    validate_fix(app);
    detect_failure(app);

    // Ghost hint only lives while its command sits at that pane's prompt.
    if let NlState::Ghost(g) = &app.win.inline_ai.nl {
        let alive = app.win.wm.tabs.iter().flat_map(|t| t.panes()).find(|p| p.id == g.pane_id).is_some_and(|p| p.terminal.at_shell_prompt());
        if !alive {
            app.win.inline_ai.nl = NlState::Idle;
            app.request_redraw();
        }
    }

    // Spinner animation while generating.
    if matches!(app.win.inline_ai.nl, NlState::Pending(_)) {
        let next = app.win.inline_ai.last_spin + Duration::from_millis(100);
        if Instant::now() >= next {
            app.win.inline_ai.last_spin = Instant::now();
            app.request_redraw();
        }
        *wake_at = (*wake_at).min(app.win.inline_ai.last_spin + Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(enabled: bool, provider: &str, url: &str, key: Option<&str>) -> LlmConfig {
        LlmConfig {
            provider: provider.into(),
            model: "m".into(),
            api_url: url.into(),
            api_key: key.map(Into::into),
            enabled,
        }
    }

    #[test]
    fn llm_readiness() {
        assert!(!llm_ready(&cfg(false, "ollama", "http://localhost:11434", None)));
        assert!(llm_ready(&cfg(true, "ollama", "http://localhost:11434", None)));
        assert!(llm_ready(&cfg(true, "openai", "https://api.openai.com", Some("sk-x"))));
        assert!(!llm_ready(&cfg(true, "openai", "https://api.openai.com", None)));
        assert!(!llm_ready(&cfg(true, "openai", "https://api.openai.com", Some(""))));
        assert!(llm_ready(&cfg(true, "custom", "http://127.0.0.1:8080", None)));
    }

    #[test]
    fn freshness_window() {
        assert!(finished_recently(100, 2000, 103));
        assert!(finished_recently(100, 0, 105));
        assert!(!finished_recently(100, 0, 106));
        assert!(!finished_recently(100, 1000, 107));
        assert!(finished_recently(100, 0, 90)); // clock skew never panics
    }

    #[test]
    fn block_sig_changes_when_a_command_finishes() {
        use crate::terminal::AnsiHandler;
        let mut t = Terminal::new(40, 6);
        let feed = |t: &mut Terminal, b: &[u8]| {
            let mut parser = vte::Parser::new();
            let mut h = AnsiHandler::new(t);
            for &x in b {
                parser.advance(&mut h, x);
            }
        };
        let empty = BlockSig::of(1, &t);
        assert_eq!(empty.count, 0);
        feed(&mut t, b"\x1b]133;A\x07$ \x1b]133;B\x07false\r\n\x1b]133;C\x07\x1b]133;D;1\x07");
        let one = BlockSig::of(1, &t);
        assert_eq!(one.count, 1);
        assert_ne!(one, empty);
        assert_ne!(one, BlockSig::of(2, &t), "pane id is part of the identity");
        feed(&mut t, b"\x1b]133;A\x07$ \x1b]133;B\x07false\r\n\x1b]133;C\x07\x1b]133;D;1\x07");
        assert_ne!(BlockSig::of(1, &t), one, "same command again is still a new block");
    }

    #[test]
    fn cache_is_bounded_and_remembers_misses() {
        let mut ai = InlineAi::new();
        for i in 0..(CACHE_CAP + 10) {
            ai.cache_put(fix::fix_key("c", i as i32, ""), None);
        }
        assert_eq!(ai.cache.len(), CACHE_CAP);
        assert!(ai.cache.contains_key(&fix::fix_key("c", (CACHE_CAP + 9) as i32, "")));
        assert!(!ai.cache.contains_key(&fix::fix_key("c", 0, "")));
        // a cached miss is a cached value
        assert_eq!(ai.cache.get(&fix::fix_key("c", 20, "")), Some(&None));
    }
}
