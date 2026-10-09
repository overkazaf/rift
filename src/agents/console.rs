//! The dock as a control console: keeps the per-agent view (approval prompt,
//! metrics, command risk, turn timeline) fresh, and carries out what the user
//! asks from the dock: answer, interrupt, reply, broadcast, review, restart,
//! close, plus the dock's mouse handling and edge resize.
//!
//! The keyboard state machine is pure (`control.rs`); this file is the glue to
//! `App`: reading pane screens, writing bytes to panes (through the terminal's
//! own key encoder), and checking commands with the Preview-Then-Accept rules.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use winit::event::KeyEvent;
use winit::keyboard::{Key, NamedKey};
use winit::window::CursorIcon;

use super::control::{Action, DockKey, MenuItem, Mode, Risk};
use super::runtime::{self, dock_rect, screen_lines};
use super::ui::{self, DockEnv, Hit};
use super::{metrics, prompt, AgentKind, AgentState};
use crate::app::App;
use crate::input::EncodeOpts;
use crate::tools::exec_preview::{ExecPreview, Severity};

/// Minimum spacing of info refreshes.
const REFRESH_EVERY: Duration = Duration::from_millis(250);
/// Two clicks on a card within this long jump to the agent.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Where "Setup: README" goes.
pub const HOOKS_URL: &str = "https://github.com/overkazaf/rift#claude-code-hooks";

// ───────────────────────────── command risk ─────────────────────────────

/// Classify a shell command with the Preview-Then-Accept rules.
pub fn classify_command(cmd: &str, cwd: Option<&str>) -> Risk {
    match ExecPreview::check_command_in(cmd, cwd) {
        Some(p) => {
            let impacts: Vec<String> = p.impacts.iter().map(|i| i.description.clone()).collect();
            match p.severity {
                Severity::Critical => Risk::Critical(impacts),
                Severity::Warning => Risk::Risky(impacts),
                Severity::Info => Risk::Safe,
            }
        }
        None => Risk::Safe,
    }
}

type RiskKey = (String, Option<String>);

struct RiskJob {
    key: RiskKey,
    rx: Receiver<Risk>,
}

#[derive(Default)]
struct Scan {
    bytes: u64,
    waiting: bool,
    at: Option<Instant>,
}

/// Runtime bookkeeping of the console (lives in `Runtime`).
#[derive(Default)]
pub struct Console {
    risk: HashMap<RiskKey, Option<Risk>>,
    jobs: Vec<RiskJob>,
    scans: HashMap<usize, Scan>,
    launch: HashMap<usize, String>,
    next_refresh: Option<Instant>,
    last_click: Option<(usize, Instant)>,
    cursor_custom: bool,
}

impl Console {
    /// The risk of `cmd` (starts a background check the first time).
    fn risk_for(&mut self, cmd: &str, cwd: Option<&str>) -> Risk {
        let key: RiskKey = (cmd.to_string(), cwd.map(str::to_string));
        match self.risk.get(&key) {
            Some(Some(r)) => return r.clone(),
            Some(None) => return Risk::Pending,
            None => {}
        }
        if self.risk.len() > 64 {
            self.risk.retain(|_, v| v.is_none());
        }
        let (tx, rx) = channel();
        let (c, d) = key.clone();
        std::thread::spawn(move || {
            let _ = tx.send(classify_command(&c, d.as_deref()));
        });
        self.risk.insert(key.clone(), None);
        self.jobs.push(RiskJob { key, rx });
        Risk::Pending
    }

    /// Collect finished checks; true when any arrived.
    fn poll_jobs(&mut self) -> bool {
        let mut arrived = false;
        let mut i = 0;
        while i < self.jobs.len() {
            match self.jobs[i].rx.try_recv() {
                Ok(r) => {
                    let job = self.jobs.remove(i);
                    self.risk.insert(job.key, Some(r));
                    arrived = true;
                }
                Err(TryRecvError::Empty) => i += 1,
                Err(TryRecvError::Disconnected) => {
                    // The check died: treat the command as unchecked-but-safe is wrong; keep it pending-free as Safe only when unflagged.
                    let job = self.jobs.remove(i);
                    self.risk.insert(job.key, Some(Risk::Risky(vec!["could not be checked".into()])));
                    arrived = true;
                }
            }
        }
        arrived
    }

    pub fn checking(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// Remember how an agent was started (restart re-runs it).
    pub fn note_launch(&mut self, uid: usize, cmd: &str) {
        let c = cmd.trim();
        if !c.is_empty() {
            self.launch.insert(uid, c.to_string());
        }
    }

    pub fn launch_of(&self, uid: usize) -> Option<&str> {
        self.launch.get(&uid).map(String::as_str)
    }
}

// ───────────────────────────── refreshing the view ─────────────────────────────

/// Called every supervision pass: re-reads the screens of live agents (prompt,
/// metrics), checks commands, and copies review data. Throttled and only while
/// the dock is open.
pub fn refresh(app: &mut App, now: Instant) {
    let arrived = app.agents_rt.console.poll_jobs();
    if !app.agents_ui.visible {
        return;
    }
    let due = app.agents_rt.console.next_refresh.map_or(true, |t| now >= t);
    if !due && !arrived {
        return;
    }
    app.agents_rt.console.next_refresh = Some(now + REFRESH_EVERY);

    struct S {
        uid: usize,
        kind: AgentKind,
        state: AgentState,
        cwd: Option<String>,
    }
    let sessions: Vec<S> = app.agents.sessions().iter().map(|s| S { uid: s.pane_uid, kind: s.kind, state: s.state, cwd: s.cwd.clone() }).collect();
    let uids: Vec<usize> = sessions.iter().map(|s| s.uid).collect();
    let mut changed = arrived;

    for s in &sessions {
        let waiting = s.state == AgentState::WaitingForUser;
        let mut parsed: Option<(metrics::Metrics, Option<prompt::ApprovalPrompt>, Vec<String>)> = None;
        if s.state.is_live() {
            let scan = app.agents_rt.console.scans.entry(s.uid).or_default();
            if let Some((ti, pi)) = app.wm.locate_pane(s.uid) {
                if let Some(pane) = app.wm.tabs[ti].pane(pi) {
                    let bytes = pane.act.bytes;
                    let stale = scan.at.map_or(true, |t| now.saturating_duration_since(t) >= REFRESH_EVERY);
                    if (scan.bytes != bytes || scan.waiting != waiting) && stale || scan.at.is_none() {
                        scan.bytes = bytes;
                        scan.waiting = waiting;
                        scan.at = Some(now);
                        let lines = screen_lines(&pane.terminal);
                        let m = metrics::parse_screen(Some(s.kind), &lines);
                        let (p, raw) = if waiting {
                            let p = prompt::parse(Some(s.kind), &lines);
                            let raw = if p.is_none() { prompt::raw_tail(&lines, 3) } else { Vec::new() };
                            (p, raw)
                        } else {
                            (None, Vec::new())
                        };
                        parsed = Some((m, p, raw));
                    }
                }
            }
        }
        let turns = app.review.turn_digests(s.uid);
        let files = app.review.files_changed(s.uid);
        let launch = app.agents_rt.console.launch_of(s.uid).map(str::to_string);
        let old_info = app.agents_ui.info.get(&s.uid).cloned().unwrap_or_default();
        // Command of the prompt on screen: the fresh parse if there is one, else the last one.
        let cmd = match &parsed {
            Some((_, p, _)) => p.as_ref().and_then(|p| p.command.clone()),
            None => old_info.prompt.as_ref().and_then(|p| p.command.clone()),
        };
        let old = old_info;
        let mut info = old.clone();
        if let Some((m, p, raw)) = parsed {
            info.metrics = metrics::merge(&info.metrics, &m);
            info.prompt = p;
            info.raw_tail = raw;
        }
        if !waiting {
            info.prompt = None;
            info.raw_tail.clear();
        }
        info.risk = match cmd {
            Some(c) if waiting && info.prompt.is_some() => app.agents_rt.console.risk_for(&c, s.cwd.as_deref()),
            _ => Risk::Safe,
        };
        info.turns = turns;
        info.files = files;
        info.launch = launch;
        if info != old {
            changed = true;
        }
        app.agents_ui.info.insert(s.uid, info);
    }
    app.agents_ui.info.retain(|u, _| uids.contains(u));
    app.agents_rt.console.scans.retain(|u, _| uids.contains(u));
    app.agents_ui.ctl.prune(&uids);
    if changed {
        app.request_redraw();
    }
}

// ───────────────────────────── sending bytes ─────────────────────────────

fn encode_opts(app: &App, uid: usize) -> EncodeOpts {
    let mut o = EncodeOpts { shift_enter: app.config.input.shift_enter, option_as_meta: app.config.input.option_as_meta, mac: cfg!(target_os = "macos"), ..EncodeOpts::default() };
    if let Some((ti, pi)) = app.wm.locate_pane(uid) {
        if let Some(p) = app.wm.tabs[ti].pane(pi) {
            let t = &p.terminal;
            o.app_cursor = t.app_cursor_keys;
            o.app_keypad = t.app_keypad();
            o.kitty_flags = t.kitty_keyboard_flags();
        }
    }
    o
}

fn send(app: &mut App, uid: usize, bytes: &[u8]) -> bool {
    match app.wm.pane_by_id_mut(uid) {
        Some(p) if !bytes.is_empty() => {
            p.write(bytes);
            true
        }
        _ => false,
    }
}

fn toast(app: &mut App, msg: impl Into<String>) {
    app.blocks_ui.show_toast(msg);
    app.request_redraw();
}

// ───────────────────────────── actions ─────────────────────────────

/// Answer option `option` of agent `uid`'s approval prompt.
pub fn answer(app: &mut App, uid: usize, option: usize) {
    let Some(p) = app.agents_ui.info.get(&uid).and_then(|i| i.prompt.clone()) else {
        return toast(app, "No approval prompt to answer");
    };
    let Some(kind) = app.agents.session(uid).map(|s| s.kind) else { return };
    // The screen may have moved on since the last refresh: only answer what is still there.
    let fresh = app.wm.locate_pane(uid).and_then(|(ti, pi)| app.wm.tabs[ti].pane(pi)).and_then(|pane| prompt::parse(Some(kind), &screen_lines(&pane.terminal)));
    if !fresh.as_ref().is_some_and(|f| f.question == p.question && f.options == p.options) {
        if let Some(i) = app.agents_ui.info.get_mut(&uid) {
            i.prompt = None;
        }
        return toast(app, "That prompt changed: look again before answering");
    }
    let Some(keys) = prompt::plan_answer(&p, option) else {
        return toast(app, "Cannot answer this prompt from the dock: open the pane");
    };
    let bytes = prompt::encode_keys(&keys, &encode_opts(app, uid));
    if send(app, uid, &bytes) {
        if let Some(i) = app.agents_ui.info.get_mut(&uid) {
            // Drop the answered prompt at once; the next scan confirms the state.
            i.prompt = None;
            i.raw_tail.clear();
            i.risk = Risk::Safe;
        }
        app.agents_ui.ctl.mode = Mode::Browse;
        app.agents_rt.console.next_refresh = None;
    } else {
        toast(app, "That agent's pane is gone");
    }
    app.request_redraw();
}

pub fn interrupt(app: &mut App, uid: usize) {
    let b = prompt::interrupt_bytes(&encode_opts(app, uid));
    if !send(app, uid, &b) {
        toast(app, "That agent's pane is gone");
    }
}

pub fn ctrl_c(app: &mut App, uid: usize) {
    let b = prompt::ctrl_c_bytes(&encode_opts(app, uid));
    if !send(app, uid, &b) {
        toast(app, "That agent's pane is gone");
    }
}

/// Type `text` and press Enter in every pane of `uids`.
pub fn send_text(app: &mut App, uids: &[usize], text: &str) {
    let mut sent = 0;
    for &uid in uids {
        let b = prompt::reply_bytes(text, &encode_opts(app, uid));
        if send(app, uid, &b) {
            sent += 1;
        }
    }
    if uids.len() > 1 {
        toast(app, format!("Sent to {sent} agents"));
    }
    app.request_redraw();
}

/// Re-run the agent's launch command in its pane (stopping it first if needed).
pub fn restart(app: &mut App, uid: usize) {
    let Some((kind, live)) = app.agents.session(uid).map(|s| (s.kind, s.state.is_live())) else { return };
    let cmd = app.agents_ui.info.get(&uid).and_then(|i| i.launch.clone()).or_else(|| app.agents_rt.console.launch_of(uid).map(str::to_string)).unwrap_or_else(|| super::launch::launch_command(kind));
    if live {
        // Two Ctrl+C quit every supported CLI back to the shell.
        let one = prompt::ctrl_c_bytes(&encode_opts(app, uid));
        let mut two = one.clone();
        two.extend(&one);
        send(app, uid, &two);
    }
    runtime::queue_restart(app, uid, cmd);
    toast(app, format!("Restarting {}\u{2026}", kind.name()));
}

/// Close the pane of `uid` (after the dock's confirmation).
pub fn close_pane(app: &mut App, uid: usize) {
    if app.wm.locate_pane(uid).is_none() {
        return toast(app, "That agent's pane is gone");
    }
    let only = app.wm.tabs.len() == 1 && app.wm.tabs[0].panes().len() == 1;
    if only {
        return toast(app, "This is the last pane: close the window instead");
    }
    let prev = app.wm.active_pane().id;
    if !crate::app::panes::focus_pane_uid(app, uid) {
        return;
    }
    if app.wm.close_current() {
        return;
    }
    if prev != uid {
        let _ = crate::app::panes::focus_pane_uid(app, prev);
    }
    app.agents_ui.info.remove(&uid);
    crate::app::panes::after_layout_change(app);
}

/// Carry out what the key handler (or a click) decided.
/// Returns false when the key should go on to the terminal.
pub fn apply(app: &mut App, action: Action) -> bool {
    match action {
        Action::None => {}
        Action::Jump(uid) => runtime::jump(app, uid),
        Action::NextAttention => runtime::next_attention(app),
        Action::Blur => app.agents_ui.focused = false,
        Action::PassThrough => {
            app.agents_ui.focused = false;
            app.request_redraw();
            return false;
        }
        Action::Answer { uid, option } => answer(app, uid, option),
        Action::Interrupt(uid) => interrupt(app, uid),
        Action::CtrlC(uid) => ctrl_c(app, uid),
        Action::Send { uids, text } => send_text(app, &uids, &text),
        Action::Review(uid) => {
            crate::review::open_pane(app, uid);
            app.agents_ui.focused = false;
        }
        Action::Restart(uid) => restart(app, uid),
        Action::Close(uid) => close_pane(app, uid),
        Action::ToggleDensity => {
            app.agents_ui.compact = !app.agents_ui.compact;
            app.agents_ui.follow = true;
        }
        Action::Notice(msg) => toast(app, msg),
    }
    app.request_redraw();
    true
}

// ───────────────────────────── keyboard ─────────────────────────────

fn dock_key(event: &KeyEvent, app: &App) -> DockKey {
    let m = app.modifiers;
    if m.super_key() || m.alt_key() {
        return DockKey::Chord;
    }
    if m.control_key() {
        return match &event.logical_key {
            Key::Character(s) => s.chars().next().map_or(DockKey::Chord, |c| DockKey::Ctrl(c.to_ascii_lowercase())),
            _ => DockKey::Chord,
        };
    }
    match &event.logical_key {
        Key::Named(NamedKey::ArrowUp) => DockKey::Up,
        Key::Named(NamedKey::ArrowDown) => DockKey::Down,
        Key::Named(NamedKey::ArrowLeft) => DockKey::Left,
        Key::Named(NamedKey::ArrowRight) => DockKey::Right,
        Key::Named(NamedKey::Home) => DockKey::Home,
        Key::Named(NamedKey::End) => DockKey::End,
        Key::Named(NamedKey::Enter) if m.shift_key() => DockKey::ShiftEnter,
        Key::Named(NamedKey::Enter) => DockKey::Enter,
        Key::Named(NamedKey::Escape) => DockKey::Escape,
        Key::Named(NamedKey::Tab) => DockKey::Tab,
        Key::Named(NamedKey::Space) => DockKey::Space,
        Key::Named(NamedKey::Backspace) => DockKey::Backspace,
        Key::Named(NamedKey::Delete) => DockKey::Delete,
        Key::Character(s) => s.chars().next().map_or(DockKey::Chord, DockKey::Char),
        _ => DockKey::Chord,
    }
}

/// Keyboard while the dock has focus. Returns true when the key was consumed.
pub fn handle_key(app: &mut App, event: &KeyEvent) -> bool {
    if !app.agents_ui.visible || !app.agents_ui.focused {
        return false;
    }
    let composing = app.agents_ui.composing();
    // Cmd+V in the composer pastes into it (not into the terminal behind).
    if composing && app.modifiers.super_key() {
        if let Key::Character(s) = &event.logical_key {
            if s.eq_ignore_ascii_case("v") {
                if let Some(text) = crate::window::selection::paste_from_clipboard() {
                    app.agents_ui.ctl.insert_text(&text);
                    app.request_redraw();
                }
                return true;
            }
        }
    }
    let key = dock_key(event, app);
    if key == DockKey::Chord || (matches!(key, DockKey::Ctrl(_)) && !composing) {
        return false; // global chords still work
    }
    // Typed text goes in whole (dead keys and compose sequences yield several chars).
    if composing && !app.modifiers.control_key() {
        if let Key::Character(s) = &event.logical_key {
            if s.chars().count() > 1 {
                app.agents_ui.ctl.insert_text(s);
                app.request_redraw();
                return true;
            }
        }
    }
    let now = Instant::now();
    let action = {
        let ui = &mut app.agents_ui;
        let env = DockEnv::new(&app.agents, &ui.info, now);
        ui.ctl.on_key(key, &mut ui.selected, &env)
    };
    app.agents_ui.follow = true;
    apply(app, action)
}

/// IME commit / paste text into the composer. True when consumed.
pub fn insert_text(app: &mut App, text: &str) -> bool {
    if !app.agents_ui.composing() {
        return false;
    }
    let ok = app.agents_ui.ctl.insert_text(text);
    app.request_redraw();
    ok
}

/// Caret rectangle for the OS candidate window, while composing.
pub fn ime_rect(app: &App) -> Option<(usize, usize, usize, usize)> {
    if !app.agents_ui.composing() {
        return None;
    }
    app.agents_ui.ime_rect.map(|r| (r.x, r.y, r.w, r.h))
}

// ───────────────────────────── mouse ─────────────────────────────

fn set_cursor(app: &mut App, icon: Option<CursorIcon>) {
    if let (Some(w), Some(icon)) = (&app.window, icon) {
        w.set_cursor(icon);
    }
    app.agents_rt.console.cursor_custom = icon.is_some();
}

fn pointer_in_dock(app: &App) -> bool {
    dock_rect(app).is_some_and(|d| d.contains(app.cursor_x, app.cursor_y) || ui::edge_zone(d).contains(app.cursor_x, app.cursor_y))
}

fn clickable(h: &Hit) -> bool {
    !matches!(h, Hit::MenuDismiss | Hit::Edge)
}

pub fn on_cursor_moved(app: &mut App) -> bool {
    let Some(dock) = dock_rect(app) else { return false };
    let (x, y) = (app.cursor_x, app.cursor_y);
    if app.agents_ui.resizing {
        let cols = ui::cols_for_drag(x, dock.x, app.renderer.cell_width());
        if cols != app.config.agents.dock_cols {
            app.config.agents.dock_cols = cols;
            crate::app::shortcuts::resize_from_window(app);
            app.request_redraw();
        }
        return true;
    }
    let inside = pointer_in_dock(app);
    let hit = if inside && !app.mouse_pressed { app.agents_ui.hit_at(x, y) } else { None };
    if hit != app.agents_ui.hover {
        app.agents_ui.hover = hit.clone();
        app.request_redraw();
    }
    match &hit {
        Some(Hit::Edge) => set_cursor(app, Some(CursorIcon::ColResize)),
        Some(h) if clickable(h) => set_cursor(app, Some(CursorIcon::Pointer)),
        _ if app.agents_rt.console.cursor_custom => {
            set_cursor(app, Some(CursorIcon::Default));
            app.agents_rt.console.cursor_custom = false;
        }
        _ => {}
    }
    inside && !app.mouse_pressed
}

/// Run a menu item or confirmation through the state machine.
fn run_item(app: &mut App, item: MenuItem, uid: usize) -> bool {
    let now = Instant::now();
    let action = {
        let ui = &mut app.agents_ui;
        let env = DockEnv::new(&app.agents, &ui.info, now);
        ui.ctl.run_item(item, uid, &mut ui.selected, &env)
    };
    apply(app, action)
}

fn press_key(app: &mut App, key: DockKey) -> bool {
    let now = Instant::now();
    let action = {
        let ui = &mut app.agents_ui;
        let env = DockEnv::new(&app.agents, &ui.info, now);
        ui.ctl.on_key(key, &mut ui.selected, &env)
    };
    apply(app, action)
}

pub fn on_mouse_press(app: &mut App) -> bool {
    let (x, y) = (app.cursor_x, app.cursor_y);
    if !app.agents_ui.visible || !pointer_in_dock(app) {
        if app.agents_ui.focused {
            app.agents_ui.focused = false;
            app.request_redraw();
        }
        return false;
    }
    app.agents_ui.focused = true;
    let hit = app.agents_ui.hit_at(x, y);
    let now = Instant::now();
    match hit {
        Some(Hit::Edge) => app.agents_ui.resizing = true,
        Some(Hit::Card(uid)) => {
            app.agents_ui.selected = Some(uid);
            app.agents_ui.follow = true;
            if app.modifiers.super_key() {
                app.agents_ui.ctl.toggle_mark(uid);
            } else if app.agents_rt.console.last_click.is_some_and(|(u, t)| u == uid && now.saturating_duration_since(t) <= DOUBLE_CLICK) {
                app.agents_rt.console.last_click = None;
                runtime::jump(app, uid);
            } else {
                app.agents_rt.console.last_click = Some((uid, now));
            }
        }
        Some(Hit::Mark(uid)) => {
            app.agents_ui.selected = Some(uid);
            app.agents_ui.ctl.toggle_mark(uid);
        }
        Some(Hit::Answer { uid, option }) => {
            app.agents_ui.selected = Some(uid);
            let action = {
                let ui = &mut app.agents_ui;
                let env = DockEnv::new(&app.agents, &ui.info, now);
                ui.ctl.click_answer(uid, option, &env)
            };
            apply(app, action);
        }
        Some(Hit::Interrupt(uid)) => {
            run_item(app, MenuItem::Interrupt, uid);
        }
        Some(Hit::Reply(uid)) => {
            run_item(app, MenuItem::Reply, uid);
        }
        Some(Hit::Review(uid)) => {
            run_item(app, MenuItem::Review, uid);
        }
        Some(Hit::Restart(uid)) => {
            run_item(app, MenuItem::Restart, uid);
        }
        Some(Hit::Close(uid)) => {
            run_item(app, MenuItem::Close, uid);
        }
        Some(Hit::More(uid)) => {
            app.agents_ui.selected = Some(uid);
            app.agents_ui.ctl.open_menu(uid);
        }
        Some(Hit::Menu { uid, item }) => {
            run_item(app, item, uid);
        }
        Some(Hit::MenuDismiss) => app.agents_ui.ctl.mode = Mode::Browse,
        Some(Hit::Turn { uid, id }) => {
            app.agents_ui.selected = Some(uid);
            app.review.open_turn(uid, id);
            app.agents_ui.focused = false;
        }
        Some(Hit::Confirm(yes)) => {
            press_key(app, if yes { DockKey::Enter } else { DockKey::Escape });
        }
        Some(Hit::SendReply) => {
            press_key(app, DockKey::Enter);
        }
        Some(Hit::CancelReply) => {
            press_key(app, DockKey::Escape);
        }
        Some(Hit::NewAgent) => {
            crate::app::overlays::open_command_palette(app);
            app.command_palette.set_query("agent ");
        }
        Some(Hit::Hooks) => crate::tools::url_detect::open_url(HOOKS_URL),
        Some(Hit::Density) => {
            app.agents_ui.compact = !app.agents_ui.compact;
            app.agents_ui.follow = true;
        }
        None => {}
    }
    app.request_redraw();
    true
}

/// Right click on a card opens its context menu.
pub fn on_right_press(app: &mut App) -> bool {
    if !app.agents_ui.visible || !pointer_in_dock(app) {
        return false;
    }
    let uid = match app.agents_ui.hit_at(app.cursor_x, app.cursor_y) {
        Some(Hit::Card(u) | Hit::Mark(u) | Hit::Interrupt(u) | Hit::Reply(u) | Hit::Review(u) | Hit::Restart(u) | Hit::Close(u) | Hit::More(u)) => Some(u),
        Some(Hit::Answer { uid, .. } | Hit::Turn { uid, .. }) => Some(uid),
        _ => None,
    };
    if let Some(uid) = uid {
        app.agents_ui.focused = true;
        app.agents_ui.selected = Some(uid);
        app.agents_ui.ctl.open_menu(uid);
    }
    app.request_redraw();
    true
}

/// End of an edge drag: remember the width.
pub fn on_mouse_release(app: &mut App) -> bool {
    if !app.agents_ui.resizing {
        return false;
    }
    app.agents_ui.resizing = false;
    crate::config::toml::save_config(&app.config);
    app.request_redraw();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_uses_preview_then_accept_rules() {
        assert_eq!(classify_command("cargo test --bin rift", None), Risk::Safe);
        assert_eq!(classify_command("ls -la", None), Risk::Safe);
        assert_eq!(classify_command("", None), Risk::Safe);
        let r = classify_command("rm -rf /", None);
        assert!(matches!(r, Risk::Critical(ref v) if !v.is_empty()), "{r:?}");
        let r = classify_command("sudo rm -rf /var/tmp/build", None);
        assert!(r.is_flagged(), "{r:?}");
    }

    #[test]
    fn risk_checks_run_in_the_background_and_are_cached() {
        let mut c = Console::default();
        assert_eq!(c.risk_for("rm -rf /", None), Risk::Pending);
        assert!(c.checking());
        let t = Instant::now();
        while c.checking() {
            c.poll_jobs();
            assert!(t.elapsed() < Duration::from_secs(10), "check never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(c.risk_for("rm -rf /", None), Risk::Critical(_)));
        assert!(!c.checking(), "second lookup is a cache hit");
        // A different working directory is a different question.
        assert_eq!(c.risk_for("rm -rf /", Some("/tmp")), Risk::Pending);
    }

    #[test]
    fn launch_commands_are_remembered() {
        let mut c = Console::default();
        assert_eq!(c.launch_of(1), None);
        c.note_launch(1, "  claude --resume \n");
        assert_eq!(c.launch_of(1), Some("claude --resume"));
        c.note_launch(1, "  ");
        assert_eq!(c.launch_of(1), Some("claude --resume"), "blank text does not erase it");
    }

    #[test]
    fn only_dock_hits_are_clickable() {
        assert!(clickable(&Hit::Card(1)) && clickable(&Hit::NewAgent));
        assert!(!clickable(&Hit::Edge) && !clickable(&Hit::MenuDismiss));
    }
}
