//! Docked AI chat sidebar (replaces the old bottom AI panel).
//!
//! # Layout rule
//!
//! The chat is docked to the **far right edge** of the window, full height
//! below the tab bar (above the HUD when visible). Everything else lays out
//! in the width that is left:
//!
//! ```text
//! | terminal panes | browser (if open) | chat (if open) |
//! ```
//!
//! i.e. when both the browser and the chat are open, the browser sits to the
//! left of the chat and is sized (via `BrowserLayout::compute`, the single
//! source of truth for browser geometry) inside `window_width - chat_width`.
//! A maximized browser fills everything left of the chat. [`dock_width`] is
//! the only place that decides how wide the chat is.
//!
//! # Module map
//! * [`session`]  messages, prompts, history trimming, persistence
//! * [`stream`]   SSE / NDJSON streaming backend + parsers
//! * [`markdown`] block/inline parser and CJK-aware wrapping
//! * [`composer`] multi-line input editor
//! * `render`     drawing + hit regions
//! * this file    state, input handling, actions

pub mod composer;
pub mod json;
pub mod markdown;
mod render;
pub mod session;
pub mod stream;

use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use winit::event::{KeyEvent, MouseScrollDelta};
use winit::keyboard::{Key, NamedKey};
use winit::window::CursorIcon;

use crate::ai::hub::{AskRequest, ContextItem, Intent};
use crate::app::App;
use crate::tools::exec_preview::ExecPreview;
use crate::ui::kit::Rect;
use crate::window::selection::{copy_to_clipboard, paste_from_clipboard};

use composer::Composer;
use session::{ChatSession, HISTORY_TOKEN_BUDGET};
use stream::{StreamEvent, StreamHandle};

pub const MIN_W: usize = 320;
pub const DEFAULT_RATIO: f32 = 0.38;
pub const MIN_RATIO: f32 = 0.15;
pub const MAX_RATIO: f32 = 0.7;
/// Half-width of the divider grab zone, in px.
const GRAB: usize = 3;
const TOAST_MS: u64 = 2500;

/// Width of the chat dock for a window `win_w` wide: `ratio` of the window,
/// at least [`MIN_W`], but never more than three quarters of the window so
/// the terminal can't be squeezed to nothing.
pub fn dock_width(ratio: f32, win_w: usize) -> usize {
    let want = (win_w as f32 * ratio.clamp(MIN_RATIO, MAX_RATIO)).round() as usize;
    want.max(MIN_W).min(win_w * 3 / 4)
}

/// Prompts offered on the empty state: (text, intent).
pub const EXAMPLES: [(&str, Intent); 4] = [
    ("Find files larger than 100MB", Intent::Command),
    ("Undo my last git commit but keep the changes", Intent::Command),
    ("What does `tar -xzf` do?", Intent::Explain),
    ("Why is port 8080 already in use?", Intent::Explain),
];

/// Something clickable in the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Close,
    NewChat,
    Send,
    Stop,
    ScrollBottom,
    Example(usize),
    /// (message index, code-block index within that message)
    Run(usize, usize),
    Insert(usize, usize),
    Copy(usize, usize),
    /// Remove pending context chip.
    ChipX(usize),
    Composer,
}

/// Geometry of the last rendered frame; used for hit-testing and scrolling.
#[derive(Default)]
pub struct Frame {
    pub rect: Rect,
    pub hits: Vec<(Rect, Target)>,
    pub msg_area: Rect,
    /// Top-left of the composer's text area and its visible row count.
    pub comp_origin: (usize, usize),
    pub comp_rows: usize,
    pub comp_cols: usize,
    pub max_scroll: usize,
    pub line_h: usize,
    /// IME candidate-window anchor (x, y, w, h).
    pub ime: Option<(i32, i32, u32, u32)>,
}

pub struct ChatUi {
    pub visible: bool,
    pub focused: bool,
    pub ratio: f32,
    /// Model name shown in the header.
    pub model: String,
    pub session: ChatSession,
    pub composer: Composer,
    /// Context attached to the composer (shown as removable chips).
    pub pending_ctx: Vec<ContextItem>,
    pub pending_intent: Intent,
    stream: Option<StreamHandle>,
    pub started: Instant,
    /// Scroll offset from the top of the message list, in px.
    pub scroll: usize,
    /// Follow the bottom while content grows.
    pub stick: bool,
    /// Multi-line block armed for execution by a first Run click.
    pub confirm_run: Option<(usize, usize)>,
    pub toast: Option<(String, Instant)>,
    /// First composer row shown when the input is taller than its box.
    pub comp_top: usize,
    /// Command currently under Advisor review.
    pub advised: Option<String>,
    pub hover: Option<Target>,
    pub divider_hover: bool,
    pub divider_drag: bool,
    cursor_custom: bool,
    ime_sent: Option<(i32, i32, u32, u32)>,
    pub frame: Frame,
}

impl ChatUi {
    pub fn new() -> Self {
        Self {
            visible: false,
            focused: false,
            ratio: DEFAULT_RATIO,
            model: String::new(),
            session: ChatSession::new(),
            composer: Composer::new(),
            pending_ctx: Vec::new(),
            pending_intent: Intent::Explain,
            stream: None,
            started: Instant::now(),
            scroll: 0,
            stick: true,
            confirm_run: None,
            toast: None,
            comp_top: 0,
            advised: None,
            hover: None,
            divider_hover: false,
            divider_drag: false,
            cursor_custom: false,
            ime_sent: None,
            frame: Frame::default(),
        }
    }

    /// Show the "answering" state without a network stream (screenshots).
    pub fn show_streaming(&mut self) {
        self.stream = Some(stream::StreamHandle::detached());
        self.started = Instant::now();
    }

    pub fn is_streaming(&self) -> bool {
        self.stream.is_some()
    }

    /// Width the dock takes from a window `win_w` wide (0 when closed).
    pub fn dock_w(&self, win_w: usize) -> usize {
        if self.visible {
            dock_width(self.ratio, win_w)
        } else {
            0
        }
    }

    /// The dock rectangle for the given window/tab-bar/HUD sizes.
    pub fn dock_rect(&self, win_w: usize, win_h: usize, tab_h: usize, hud_h: usize) -> Option<Rect> {
        let w = self.dock_w(win_w);
        (w > 0).then(|| Rect::new(win_w - w, tab_h, w, win_h.saturating_sub(tab_h + hud_h)))
    }

    /// Needs periodic redraws (streaming cursor, toast expiry).
    pub fn animating(&self) -> bool {
        self.visible && (self.stream.is_some() || self.toast.is_some())
    }

    pub fn set_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    /// IME cursor area to publish, only when it changed since last call.
    pub fn take_ime_area(&mut self) -> Option<(i32, i32, u32, u32)> {
        let a = self.frame.ime?;
        if self.ime_sent == Some(a) {
            return None;
        }
        self.ime_sent = Some(a);
        Some(a)
    }

    fn scroll_by(&mut self, delta_px: isize) {
        let max = self.frame.max_scroll;
        let cur = if self.stick { max } else { self.scroll.min(max) } as isize;
        let next = (cur + delta_px).clamp(0, max as isize) as usize;
        self.scroll = next;
        self.stick = next >= max;
    }

    fn scroll_to_bottom(&mut self) {
        self.stick = true;
    }

    fn hit_at(&self, x: usize, y: usize) -> Option<Target> {
        self.frame.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, t)| *t)
    }
}

impl Default for ChatUi {
    fn default() -> Self {
        Self::new()
    }
}

// ── App glue ──────────────────────────────────────────────────────────────

impl App {
    /// Chat dock rectangle in window pixels, when the chat is open.
    pub fn chat_rect(&self) -> Option<Rect> {
        let (w, h) = self.window.as_ref().map_or((800, 600), |win| {
            let s = win.inner_size();
            (s.width as usize, s.height as usize)
        });
        let ca = self.content_area();
        self.chat.dock_rect(w, h, ca.y, h.saturating_sub(ca.y + ca.height))
    }
}

fn relayout(app: &mut App) {
    crate::network::browser::relayout(app);
}

/// Cmd+Shift+A / menu: open+focus, focus, or close.
pub fn toggle(app: &mut App) {
    if !app.chat.visible {
        open(app);
    } else if !app.chat.focused {
        app.chat.focused = true;
        app.request_redraw();
    } else {
        close(app);
    }
}

/// Show the sidebar and focus the composer.
pub fn open(app: &mut App) {
    let was_visible = app.chat.visible;
    app.chat.model = app.llm.config.model.clone();
    app.chat.visible = true;
    app.chat.focused = true;
    if !was_visible {
        relayout(app);
    }
    app.request_redraw();
}

pub fn close(app: &mut App) {
    if !app.chat.visible {
        return;
    }
    app.chat.visible = false;
    app.chat.focused = false;
    app.chat.divider_drag = false;
    app.chat.divider_hover = false;
    app.chat.hover = None;
    app.chat.frame = Frame::default();
    app.chat.ime_sent = None;
    // Streaming keeps running in the background; the answer is still saved.
    relayout(app);
}

/// Entry point used by `hub::ask`.
pub fn handle_request(app: &mut App, req: AskRequest) {
    open(app);
    if req.submit {
        send(app, req);
    } else {
        let text = if req.question.trim().is_empty() { req.display.clone() } else { req.question.clone() };
        app.chat.composer.set_text(&text);
        app.chat.pending_ctx = req.context;
        app.chat.pending_intent = req.intent;
        app.chat.focused = true;
        app.request_redraw();
    }
}

/// Start a new, empty conversation (the old one is already on disk).
pub fn new_chat(app: &mut App) {
    cancel(app);
    app.chat.session.save();
    app.chat.session = ChatSession::new();
    app.chat.composer.clear();
    app.chat.pending_ctx.clear();
    app.chat.pending_intent = Intent::Explain;
    app.chat.scroll = 0;
    app.chat.stick = true;
    app.chat.confirm_run = None;
    app.chat.advised = None;
    app.advisor.clear();
    app.request_redraw();
}

/// Append the turn and start streaming the answer.
pub fn send(app: &mut App, req: AskRequest) {
    // A new question supersedes an answer that is still streaming.
    if app.chat.stream.is_some() {
        cancel(app);
    }
    app.chat.session.push_user(&req);
    app.chat.session.push_assistant_placeholder();
    app.chat.stick = true;
    app.chat.confirm_run = None;
    app.chat.advised = None;
    app.advisor.clear();

    let intent = req.intent;
    let history = app.chat.session.history();
    let cwd = app.wm.active_pane().terminal.cwd.clone();
    let profile = app.llm.profile.summary();
    let proxy = app.wm.get_proxy();
    let cfg = app.llm.config.clone();
    app.chat.stream = Some(stream::spawn(
        &cfg,
        move || {
            let mut ctx = crate::ai::context::TermContext::collect();
            if let Some(c) = cwd {
                ctx.cwd = c;
            }
            let system = session::system_prompt(intent, &ctx, &profile);
            session::build_api_messages(system, history, HISTORY_TOKEN_BUDGET)
        },
        proxy,
    ));
    app.chat.started = Instant::now();
    app.request_redraw();
}

/// Stop the running stream (Esc / Stop button). Keeps what has arrived.
pub fn cancel(app: &mut App) {
    if app.chat.stream.take().is_none() {
        return;
    }
    finish_turn(app, true);
    app.request_redraw();
}

fn submit_composer(app: &mut App) {
    let text = app.chat.composer.text();
    let q = text.trim().to_string();
    if q.is_empty() && app.chat.pending_ctx.is_empty() {
        return;
    }
    if app.chat.is_streaming() {
        app.chat.set_toast("Still answering - press Esc to stop");
        app.request_redraw();
        return;
    }
    app.chat.composer.clear();
    app.chat.comp_top = 0;
    let mut req = AskRequest::new(q, app.chat.pending_intent);
    req.context = std::mem::take(&mut app.chat.pending_ctx);
    app.chat.pending_intent = Intent::Explain;
    send(app, req);
}

/// Drain streaming events. Returns true when the UI should redraw.
pub fn poll(app: &mut App) -> bool {
    let mut changed = false;
    if let Some((_, t)) = &app.chat.toast {
        if t.elapsed() > Duration::from_millis(TOAST_MS) {
            app.chat.toast = None;
            changed = true;
        }
    }
    let mut events = Vec::new();
    let mut disconnected = false;
    if let Some(h) = &app.chat.stream {
        loop {
            match h.rx.try_recv() {
                Ok(e) => events.push(e),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
    }
    for ev in events {
        if app.chat.stream.is_none() {
            break;
        }
        changed = true;
        match ev {
            StreamEvent::Delta(t) => {
                if let Some(m) = app.chat.session.last_assistant_mut() {
                    m.content.push_str(&t);
                }
            }
            StreamEvent::Done => {
                app.chat.stream = None;
                finish_turn(app, false);
            }
            StreamEvent::Error(e) => {
                app.chat.stream = None;
                fail_turn(app, e);
            }
        }
    }
    if disconnected && app.chat.stream.is_some() {
        app.chat.stream = None;
        finish_turn(app, false);
        changed = true;
    }
    changed
}

/// Wrap up the assistant message: actions, persistence, Advisor review.
fn finish_turn(app: &mut App, cancelled: bool) {
    let empty = app.chat.session.messages.last().is_some_and(|m| m.role == session::Role::Assistant && m.content.is_empty());
    if empty {
        // Nothing arrived: drop the placeholder rather than leave a blank bubble.
        app.chat.session.messages.pop();
    } else if let Some(m) = app.chat.session.last_assistant_mut() {
        m.refresh_actions();
    }
    app.chat.session.save();
    if cancelled {
        return;
    }
    if app.advisor.enabled {
        if let Some(cmd) = app.chat.session.last_commands().first().cloned() {
            let ctx = crate::ai::context::TermContext::collect();
            let context = format!(
                "OS: {}, Shell: {}, CWD: {}{}",
                ctx.os,
                ctx.shell,
                app.wm.active_pane().terminal.cwd.clone().unwrap_or(ctx.cwd),
                ctx.git_branch.map(|b| format!(", git branch: {b}")).unwrap_or_default(),
            );
            app.advisor.review_command(&cmd, &context, &app.llm.config);
            app.chat.advised = Some(cmd);
        }
    }
}

fn fail_turn(app: &mut App, err: String) {
    let partial = app.chat.session.last_assistant_mut().is_some_and(|m| !m.content.is_empty());
    if partial {
        app.chat.set_toast(format!("Stream interrupted: {err}"));
        finish_turn(app, true);
    } else if let Some(m) = app.chat.session.last_assistant_mut() {
        m.content = err;
        m.error = true;
        m.actions.clear();
        app.chat.session.save();
    }
}

// ── Actions on code blocks ────────────────────────────────────────────────

/// Raw text of code block `block` (0-based among code blocks) of message `msg`.
fn code_block(app: &App, msg: usize, block: usize) -> Option<(String, String)> {
    let m = app.chat.session.messages.get(msg)?;
    markdown::parse_blocks(&m.content)
        .into_iter()
        .filter_map(|b| match b {
            markdown::Block::Code { lang, code, .. } => Some((lang, code)),
            _ => None,
        })
        .nth(block)
}

/// Write `cmd` to the active pane; `run` also submits it. Multi-line text
/// goes in as a bracketed paste when the shell supports it. Dangerous
/// commands are routed through the Preview-Then-Accept modal: the text is
/// typed but Enter is withheld until the user confirms.
pub fn send_to_terminal(app: &mut App, cmd: &str, run: bool) {
    let lines: Vec<&str> = cmd.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return;
    }
    let cwd = app.wm.active_pane().terminal.cwd.clone();
    let bracketed = app.wm.active_pane().terminal.bracketed_paste;
    let text = if lines.len() == 1 {
        lines[0].trim().to_string()
    } else if bracketed {
        format!("\x1b[200~{}\x1b[201~", lines.join("\n"))
    } else {
        lines.iter().map(|l| l.trim()).collect::<Vec<_>>().join("; ")
    };
    let preview = if run { lines.iter().find_map(|l| ExecPreview::check_command_in(l.trim(), cwd.as_deref())) } else { None };

    app.wm.active_pane_mut().write(text.as_bytes());
    if run {
        match preview {
            Some(mut p) => {
                p.visible = true;
                app.exec_preview = p;
            }
            None => {
                app.wm.active_pane_mut().write(b"\r");
                if app.audit.enabled {
                    app.audit.log_command(cmd, 0);
                }
            }
        }
    }
    if app.wm.process_all_output() {
        app.wm.flush_all_responses();
    }
    // Hand the keyboard back so the user sees / edits the result.
    app.chat.focused = false;
    app.request_redraw();
}

fn run_block(app: &mut App, msg: usize, block: usize, run: bool) {
    let Some((lang, code)) = code_block(app, msg, block) else { return };
    if !session::is_shell_lang(&lang) {
        return;
    }
    let cmd = session::block_command(&code);
    if cmd.is_empty() {
        return;
    }
    let multi = cmd.lines().filter(|l| !l.trim().is_empty()).count() > 1;
    if run && multi && app.chat.confirm_run != Some((msg, block)) {
        // First click only arms the button; the block is already on screen.
        app.chat.confirm_run = Some((msg, block));
        app.chat.set_toast("Multi-line block: click Run again to execute all lines");
        app.request_redraw();
        return;
    }
    app.chat.confirm_run = None;
    send_to_terminal(app, &cmd, run);
}

fn copy_block(app: &mut App, msg: usize, block: usize) {
    if let Some((_, code)) = code_block(app, msg, block) {
        copy_to_clipboard(code.trim_end());
        app.chat.set_toast("Copied to clipboard");
        app.request_redraw();
    }
}

/// Index of the last assistant message with a runnable command.
fn first_command_of_last_answer(app: &App) -> Option<String> {
    app.chat.session.last_commands().first().cloned()
}

fn activate(app: &mut App, t: Target) {
    match t {
        Target::Close => close(app),
        Target::NewChat => new_chat(app),
        Target::Send => submit_composer(app),
        Target::Stop => cancel(app),
        Target::ScrollBottom => {
            app.chat.scroll_to_bottom();
            app.request_redraw();
        }
        Target::Example(i) => {
            if let Some((text, intent)) = EXAMPLES.get(i) {
                if !app.chat.is_streaming() {
                    send(app, AskRequest::new(*text, *intent));
                }
            }
        }
        Target::Run(m, b) => run_block(app, m, b, true),
        Target::Insert(m, b) => run_block(app, m, b, false),
        Target::Copy(m, b) => copy_block(app, m, b),
        Target::ChipX(i) => {
            if i < app.chat.pending_ctx.len() {
                app.chat.pending_ctx.remove(i);
                app.request_redraw();
            }
        }
        Target::Composer => {}
    }
}

// ── Keyboard ──────────────────────────────────────────────────────────────

/// Key handling while the composer has focus. Returns true when consumed;
/// unhandled Cmd/Ctrl+Shift chords fall through to the global shortcuts.
pub fn handle_key(app: &mut App, event: &KeyEvent) -> bool {
    let m = app.modifiers;
    let (cmd, ctrl, alt, shift) = (m.super_key(), m.control_key(), m.alt_key(), m.shift_key());
    let letter = match &event.logical_key {
        Key::Character(s) => s.chars().next().map(|c| c.to_ascii_lowercase()),
        _ => None,
    };

    // Cmd+Shift+A: toggle (closes while focused).
    if cmd && shift && letter == Some('a') {
        toggle(app);
        return true;
    }

    match &event.logical_key {
        Key::Named(NamedKey::Escape) => {
            if app.chat.is_streaming() {
                cancel(app);
            } else {
                app.chat.focused = false;
                app.request_redraw();
            }
            return true;
        }
        // Cmd+Enter runs, Cmd+Shift+Enter inserts the first command of the
        // last answer. Only while the chat is focused, so pane zoom
        // (Cmd+Shift+Enter) keeps working everywhere else.
        Key::Named(NamedKey::Enter) if cmd => {
            match first_command_of_last_answer(app) {
                Some(c) => send_to_terminal(app, &c, !shift),
                None => {
                    app.chat.set_toast("No command in the last answer");
                    app.request_redraw();
                }
            }
            return true;
        }
        Key::Named(NamedKey::Enter) => {
            if shift || alt {
                app.chat.composer.insert_newline();
            } else {
                submit_composer(app);
            }
            app.request_redraw();
            return true;
        }
        _ => {}
    }

    if cmd {
        match letter {
            Some('n') => new_chat(app),
            Some('v') => {
                if let Some(t) = paste_from_clipboard() {
                    app.chat.composer.insert_str(&t);
                }
            }
            Some('c') => {
                let text = if app.chat.composer.is_empty() {
                    app.chat.session.messages.iter().rev().find(|m| m.role == session::Role::Assistant && !m.error).map(|m| m.content.clone()).unwrap_or_default()
                } else {
                    app.chat.composer.text()
                };
                copy_to_clipboard(&text);
            }
            Some('x') => {
                copy_to_clipboard(&app.chat.composer.text());
                app.chat.composer.clear();
            }
            Some('a') => {}
            _ => match &event.logical_key {
                Key::Named(NamedKey::ArrowLeft) => app.chat.composer.home(),
                Key::Named(NamedKey::ArrowRight) => app.chat.composer.end(),
                Key::Named(NamedKey::ArrowUp) => {
                    app.chat.stick = false;
                    app.chat.scroll = 0;
                }
                Key::Named(NamedKey::ArrowDown) => app.chat.scroll_to_bottom(),
                Key::Named(NamedKey::Backspace) => app.chat.composer.delete_to_line_start(),
                // Other Cmd chords belong to the app (new tab, palette, ...).
                _ => return false,
            },
        }
        app.request_redraw();
        return true;
    }

    if ctrl {
        if shift {
            return false;
        }
        match letter {
            Some('a') => app.chat.composer.home(),
            Some('e') => app.chat.composer.end(),
            Some('k') => app.chat.composer.delete_to_line_end(),
            Some('u') => app.chat.composer.delete_to_line_start(),
            Some('w') => app.chat.composer.delete_word_back(),
            Some('c') => {
                if app.chat.is_streaming() {
                    cancel(app);
                }
            }
            _ => {}
        }
        app.request_redraw();
        return true; // never leak Ctrl chords to the shell from the chat
    }

    let page = app.chat.frame.msg_area.h.saturating_sub(app.chat.frame.line_h * 2).max(1) as isize;
    let line = (app.chat.frame.line_h.max(1) * 3) as isize;
    match &event.logical_key {
        Key::Named(NamedKey::Backspace) => {
            if alt {
                app.chat.composer.delete_word_back();
            } else if app.chat.composer.is_empty() && !app.chat.pending_ctx.is_empty() {
                app.chat.pending_ctx.pop();
            } else {
                app.chat.composer.backspace();
            }
        }
        Key::Named(NamedKey::Delete) => app.chat.composer.delete(),
        Key::Named(NamedKey::ArrowLeft) => {
            if alt {
                app.chat.composer.word_left()
            } else {
                app.chat.composer.left()
            }
        }
        Key::Named(NamedKey::ArrowRight) => {
            if alt {
                app.chat.composer.word_right()
            } else {
                app.chat.composer.right()
            }
        }
        Key::Named(NamedKey::ArrowUp) => {
            if !app.chat.composer.move_vertical(false) {
                app.chat.scroll_by(-line);
            }
        }
        Key::Named(NamedKey::ArrowDown) => {
            if !app.chat.composer.move_vertical(true) {
                app.chat.scroll_by(line);
            }
        }
        Key::Named(NamedKey::Home) => app.chat.composer.home(),
        Key::Named(NamedKey::End) => app.chat.composer.end(),
        Key::Named(NamedKey::PageUp) => app.chat.scroll_by(-page),
        Key::Named(NamedKey::PageDown) => app.chat.scroll_by(page),
        Key::Named(NamedKey::Tab) => {}
        Key::Named(NamedKey::Space) => app.chat.composer.insert_str(" "),
        Key::Character(s) => {
            let typed = event.text.as_ref().map_or(s.as_str(), |t| t.as_str());
            app.chat.composer.insert_str(typed);
        }
        _ => return false,
    }
    app.request_redraw();
    true
}

/// IME commit while the chat is focused.
pub fn insert_text(app: &mut App, text: &str) {
    app.chat.composer.insert_str(text);
    app.request_redraw();
}

// ── Mouse ─────────────────────────────────────────────────────────────────

fn win_width(app: &App) -> usize {
    app.window.as_ref().map_or(800, |w| w.inner_size().width as usize).max(1)
}

fn divider_zone(rect: Rect) -> Rect {
    Rect::new(rect.x.saturating_sub(GRAB), rect.y, 2 * GRAB, rect.h)
}

fn set_cursor(app: &mut App, icon: Option<CursorIcon>) {
    if let (Some(w), Some(icon)) = (&app.window, icon) {
        w.set_cursor(icon);
    }
    app.chat.cursor_custom = icon.is_some();
}

/// Mouse move. Returns true when the chat consumed it.
pub fn on_cursor_moved(app: &mut App) -> bool {
    if !app.chat.visible {
        return false;
    }
    let (x, y) = (app.cursor_x, app.cursor_y);
    if app.chat.divider_drag {
        let win_w = win_width(app);
        let ratio = (win_w.saturating_sub(x) as f32 / win_w as f32).clamp(MIN_RATIO, MAX_RATIO);
        if (ratio - app.chat.ratio).abs() * win_w as f32 >= 1.0 {
            app.chat.ratio = ratio;
            relayout(app);
        }
        return true;
    }
    let Some(rect) = app.chat_rect() else { return false };
    let on_div = divider_zone(rect).contains(x, y);
    let inside = rect.contains(x, y);
    let hover = if inside { app.chat.hit_at(x, y) } else { None };
    let mut redraw = false;
    if hover != app.chat.hover || on_div != app.chat.divider_hover {
        app.chat.hover = hover;
        app.chat.divider_hover = on_div;
        redraw = true;
    }
    if on_div {
        set_cursor(app, Some(CursorIcon::ColResize));
    } else if inside {
        let icon = match hover {
            Some(Target::Composer) => CursorIcon::Text,
            Some(_) => CursorIcon::Pointer,
            None => CursorIcon::Default,
        };
        set_cursor(app, Some(icon));
    } else if app.chat.cursor_custom {
        set_cursor(app, Some(CursorIcon::Default));
        app.chat.cursor_custom = false;
    }
    if redraw {
        app.request_redraw();
    }
    inside || on_div
}

/// Left press. Returns true when the chat consumed the click.
pub fn on_mouse_press(app: &mut App) -> bool {
    if !app.chat.visible {
        return false;
    }
    let (x, y) = (app.cursor_x, app.cursor_y);
    let Some(rect) = app.chat_rect() else { return false };
    if divider_zone(rect).contains(x, y) {
        app.chat.divider_drag = true;
        app.chat.focused = true;
        app.request_redraw();
        return true;
    }
    if !rect.contains(x, y) {
        if app.chat.focused {
            app.chat.focused = false;
            app.request_redraw();
        }
        return false;
    }
    app.chat.focused = true;
    match app.chat.hit_at(x, y) {
        Some(Target::Composer) => place_composer_cursor(app, x, y),
        Some(t) => activate(app, t),
        None => {}
    }
    app.request_redraw();
    true
}

pub fn on_mouse_release(app: &mut App) -> bool {
    if app.chat.divider_drag {
        app.chat.divider_drag = false;
        app.request_redraw();
        return true;
    }
    false
}

/// True when the point is over the chat dock (for swallowing other buttons).
pub fn contains(app: &App, x: usize, y: usize) -> bool {
    app.chat.visible && app.chat_rect().is_some_and(|r| r.contains(x, y))
}

/// Wheel over the message list. Returns true when consumed.
pub fn on_wheel(app: &mut App, delta: MouseScrollDelta) -> bool {
    if !contains(app, app.cursor_x, app.cursor_y) {
        return false;
    }
    let lh = app.chat.frame.line_h.max(1) as f64;
    let px = match delta {
        MouseScrollDelta::LineDelta(_, y) => -(y as f64) * lh * 3.0,
        MouseScrollDelta::PixelDelta(p) => -p.y,
    };
    app.chat.scroll_by(px.round() as isize);
    app.request_redraw();
    true
}

fn place_composer_cursor(app: &mut App, x: usize, y: usize) {
    let f = &app.chat.frame;
    let (ox, oy) = f.comp_origin;
    let cw = app.renderer.cell_width().max(1);
    let row = y.saturating_sub(oy) / f.line_h.max(1) + app.chat.comp_top;
    let col = x.saturating_sub(ox) / cw;
    let comp = &mut app.chat.composer;
    let lines = comp.visual_lines(f.comp_cols.max(1));
    let l = lines[row.min(lines.len() - 1)];
    let mut w = 0;
    let mut pos = l.start;
    while pos < l.end {
        let cwid = markdown::cells(comp.chars()[pos]);
        if col < w + cwid.div_ceil(2) {
            break;
        }
        w += cwid;
        pos += 1;
    }
    comp.set_cursor(pos);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_width_respects_min_ratio_and_cap() {
        assert_eq!(dock_width(0.38, 1600), 608);
        assert_eq!(dock_width(0.38, 700), 320); // min width
        assert_eq!(dock_width(0.38, 400), 300); // never more than 3/4 of the window
        assert_eq!(dock_width(0.99, 1000), 700); // ratio clamped
        assert_eq!(dock_width(0.0, 2000), 320); // ratio clamped to MIN_RATIO (300), then MIN_W
    }

    #[test]
    fn closed_dock_takes_no_width_and_rect_hugs_right_edge() {
        let mut c = ChatUi::new();
        assert_eq!(c.dock_w(1600), 0);
        assert!(c.dock_rect(1600, 900, 40, 0).is_none());
        c.visible = true;
        let r = c.dock_rect(1600, 900, 40, 60).unwrap();
        assert_eq!((r.x, r.y, r.w, r.h), (1600 - 608, 40, 608, 800));
        assert_eq!(r.right(), 1600);
    }

    #[test]
    fn scroll_by_sticks_only_at_bottom() {
        let mut c = ChatUi::new();
        c.frame.max_scroll = 500;
        c.scroll_by(-100);
        assert!(!c.stick);
        assert_eq!(c.scroll, 400);
        c.scroll_by(1000);
        assert!(c.stick);
        c.scroll_by(-50);
        assert_eq!(c.scroll, 450);
        assert!(!c.stick);
    }

    #[test]
    fn layout_terminal_browser_chat_share_the_window() {
        use crate::network::browser::chrome::BrowserLayout;
        let (win_w, win_h, cw, ch) = (1600usize, 900usize, 9usize, 18usize);
        let mut chat = ChatUi::new();

        // Neither open: the terminal owns the window.
        assert_eq!(win_w - chat.dock_w(win_w), 1600);

        // Chat only: terminal gets the rest.
        chat.visible = true;
        let dock = chat.dock_w(win_w);
        assert_eq!(dock, 608);
        assert_eq!(win_w - dock, 992);

        // Browser + chat: browser lays out inside `win_w - dock`, so the three
        // regions tile the window left to right with no overlap.
        let avail = win_w - dock;
        let l = BrowserLayout::compute(avail, win_h, 34, cw, ch, 1.0, 0.5, false);
        assert_eq!(l.terminal_w, avail - (avail as f32 * 0.5).round() as usize);
        assert_eq!(l.web.x + l.web.w, avail, "browser ends where the chat begins");
        assert_eq!(l.terminal_w + l.gutter.w + l.web.w, avail);
        assert!(l.terminal_w > 0 && l.terminal_w + l.gutter.w + l.web.w + dock == win_w);

        // Maximized browser: fills everything left of the chat, terminal 0.
        let m = BrowserLayout::compute(avail, win_h, 34, cw, ch, 1.0, 0.5, true);
        assert_eq!(m.terminal_w, 0);
        assert_eq!(m.web.x + m.web.w, avail);

        // Narrow window: chat is capped at 3/4, browser still fits in the rest.
        let narrow = 400;
        let dock = dock_width(DEFAULT_RATIO, narrow);
        assert_eq!(dock, 300);
        let l = BrowserLayout::compute(narrow - dock, win_h, 34, cw, ch, 1.0, 0.5, false);
        assert_eq!(l.web.x + l.web.w, narrow - dock);
    }
}
