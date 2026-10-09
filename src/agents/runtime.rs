//! Glue between the pure agent supervisor and the running `App`: observing
//! panes, routing hooks / notifications, notifications + dock badge, the
//! dock's keyboard / mouse handling, and the "New Agent" workflows.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use winit::event::{KeyEvent, MouseScrollDelta};
use winit::keyboard::{Key, NamedKey};

use super::launch::{self, GridStep, Target};
use super::notify::{self, NotifyPolicy, Reason};
use super::registry::{AgentRegistry, PaneObs, Probe, SCAN_INTERVAL};
use super::ui::{self, AgentsUi, DockKey, UiAction};
use super::{detect, git, inbox, AgentEvent, AgentKind};
use crate::app::App;
use crate::renderer::Renderer;
use crate::terminal::{Notification, Terminal};
use crate::ui::kit::Rect;
use crate::window::manager::WindowManager;
use crate::window::pane::PtyKind;
use crate::window::tab::SplitDir;
use crate::window::{Pane, PaneRect};

/// Minimum spacing of full observation passes.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How long "New Agent" waits for the new shell's first prompt before typing.
const PROMPT_WAIT: Duration = Duration::from_millis(2500);
const PROMPT_GIVE_UP: Duration = Duration::from_secs(10);
const INSTALLED_TTL: Duration = Duration::from_secs(30);

struct PendingCmd {
    uid: usize,
    cmd: String,
    since: Instant,
}

/// Runtime bookkeeping that is not part of the pure registry.
#[derive(Default)]
pub struct Runtime {
    policy: NotifyPolicy,
    next_poll: Option<Instant>,
    badge: usize,
    jobs: Vec<launch::Job>,
    pending: Vec<PendingCmd>,
    installed: Option<(Instant, Vec<AgentKind>)>,
}

impl Runtime {
    /// Label for the progress toast while worktrees are being created.
    pub fn progress_label(&self) -> Option<&str> {
        self.jobs.first().map(|j| j.label.as_str())
    }
}

/// What "New Agent" should do.
#[derive(Clone, Debug, PartialEq)]
pub enum Launch {
    /// New tab in the current directory.
    Here(AgentKind),
    /// New tab in a fresh `git worktree`.
    Worktree(AgentKind),
    /// A `cols x rows` grid of agents, one worktree each (same directory outside git).
    Layout { kind: AgentKind, cols: usize, rows: usize },
}

// ───────────────────────────── geometry ─────────────────────────────

/// Width reserved for the Mission Control dock (0 when hidden).
pub fn dock_w(app: &App, win_w: usize) -> usize {
    if !app.agents_ui.visible || !app.config.agents.enabled || app.browser_visible() {
        0
    } else {
        ui::dock_width(win_w, app.renderer.cell_width())
    }
}

fn win_size(app: &App) -> (usize, usize) {
    app.window.as_ref().map_or((800, 600), |w| {
        let s = w.inner_size();
        (s.width as usize, s.height as usize)
    })
}

fn dock_rect(app: &App) -> Option<Rect> {
    let (w, h) = win_size(app);
    let dw = dock_w(app, w);
    if dw == 0 {
        return None;
    }
    let tbh = app.tab_bar_height();
    let ch = app.renderer.cell_height();
    let hud = if app.hud_visible { ch * 3 + 20 } else { 0 };
    Some(Rect::new(0, tbh, dw, h.saturating_sub(tbh + hud)))
}

pub fn contains(app: &App, x: usize, y: usize) -> bool {
    dock_rect(app).is_some_and(|r| r.contains(x, y))
}

fn layout_of(app: &App, dock: Rect) -> ui::Layout {
    let tk = crate::ui::kit::Tokens::new(&app.renderer.theme, app.renderer.cell_width(), app.renderer.cell_height());
    ui::layout(dock, &tk)
}

// ───────────────────────────── observation ─────────────────────────────

fn pane_obs(ti: usize, pane: &Pane) -> PaneObs {
    let t = &pane.terminal;
    let osc = t.blocks.osc_seen();
    let running_cmd = if osc { t.blocks.running_command().map(str::to_string) } else { None };
    PaneObs {
        uid: pane.id,
        tab_index: ti,
        block_running: running_cmd.is_some(),
        running_cmd,
        osc_seen: osc,
        title: t.title.clone(),
        cwd: if matches!(pane.pty, PtyKind::Ssh(_)) { None } else { t.cwd.clone() },
        shell_exited: pane.exited.is_some(),
        last_exit: t.blocks.last_exit_code(),
        bytes: pane.act.bytes,
        last_output: pane.act.last_output,
        last_input: pane.act.last_input,
        last_submit: pane.act.last_submit,
    }
}

fn probe_pane(pane: &Pane) -> Probe {
    match pane.pty.shell_and_foreground() {
        Some((shell, fg)) if shell == fg => Probe::AtPrompt,
        Some((_, fg)) => detect::proc_info(fg).map_or(Probe::Unknown, Probe::Foreground),
        None => Probe::Unknown,
    }
}

fn screen_lines(t: &Terminal) -> Vec<String> {
    let from = t.grid.len().saturating_sub(40);
    t.grid[from..].iter().map(|row| crate::terminal::grid::cells_text(row)).collect()
}

/// Is the user looking at this pane right now?
fn pane_seen(app: &App, uid: usize) -> bool {
    if !app.window_focused {
        return false;
    }
    match app.wm.locate_pane(uid) {
        Some((ti, pi)) => ti == app.wm.active_tab && (!app.wm.tabs[ti].is_zoomed() || pi == app.wm.tabs[ti].active),
        None => false,
    }
}

/// Route OSC 9 / 777 notifications of a pane to its agent session. Returns true
/// when the pane is an agent pane and Rift posts its own notifications, so the
/// generic desktop notification must be skipped.
pub fn route_notifications(reg: &mut AgentRegistry, notify_on: bool, uid: usize, notes: &[Notification], now: Instant) -> bool {
    if !reg.has_live(uid) {
        return false;
    }
    for n in notes {
        reg.observe_notification(uid, &n.title, &n.body, now);
    }
    notify_on
}

/// One supervision pass; call from `about_to_wait`. Lowers `wake_at` to the
/// next moment something time-based may happen.
pub fn poll(app: &mut App, wake_at: &mut Instant) {
    if !app.agents.enabled() {
        return;
    }
    let now = Instant::now();
    flush_pending(app, now);
    finish_jobs(app);
    let hooks = inbox::drain();
    if app.agents_rt.next_poll.is_some_and(|t| now < t) && hooks.is_empty() {
        if let Some(t) = app.agents_rt.next_poll {
            *wake_at = (*wake_at).min(t);
        }
        return;
    }
    app.agents_rt.next_poll = Some(now + POLL_INTERVAL);

    let mut alive = Vec::new();
    let mut scan_soon = false;
    for (ti, tab) in app.wm.tabs.iter().enumerate() {
        for pane in tab.panes() {
            alive.push(pane.id);
            let obs = pane_obs(ti, pane);
            app.agents.observe_pane(&obs, &mut || probe_pane(pane), now);
            if app.agents.wants_scan(pane.id, pane.act.bytes, now) {
                let lines = screen_lines(&pane.terminal);
                app.agents.observe_screen(pane.id, &lines, pane.act.bytes, now);
            } else if app.agents.scan_pending(pane.id, pane.act.bytes) {
                scan_soon = true;
            }
        }
    }
    {
        let wm = &app.wm;
        for ev in &hooks {
            app.agents.observe_hook(
                ev,
                &|id| wm.locate_pane(id).is_some(),
                &|id| wm.locate_pane(id).and_then(|(ti, pi)| wm.tabs[ti].pane(pi).map(|p| pane_obs(ti, p))),
                now,
            );
        }
    }
    app.agents.retain_panes(&alive, now);
    app.agents.tick(now);

    // Public events: one drain, dispatched to the change-review module.
    for ev in app.agents.drain_events() {
        crate::review::on_agent_event(app, &ev);
    }
    notify_events(app, now);
    update_badge(app);

    if let Some(d) = app.agents.next_deadline() {
        *wake_at = (*wake_at).min(d + Duration::from_millis(5));
    }
    if scan_soon {
        *wake_at = (*wake_at).min(now + SCAN_INTERVAL);
    }
    if app.agents.live_count() > 0 {
        *wake_at = (*wake_at).min(now + Duration::from_millis(500));
    }
    if animating(app) {
        app.request_redraw();
    }
}

/// Does the agent UI need frames (pulsing pills, badges)?
pub fn animating(app: &App) -> bool {
    app.agents.enabled()
        && ((app.agents_ui.visible && app.agents.animating()) || app.agents.attention_count() > 0)
}

fn notify_events(app: &mut App, now: Instant) {
    let evs = app.agents.drain_tap();
    if !app.config.agents.notify {
        return;
    }
    for ev in evs {
        let (uid, reason, detail, ok) = match &ev {
            AgentEvent::NeedsUser { pane_uid, reason, .. } => (*pane_uid, Reason::NeedsUser, Some(reason.clone()), true),
            AgentEvent::TurnFinished { pane_uid, elapsed, .. } => (*pane_uid, Reason::TurnFinished, None, *elapsed >= notify::MIN_TURN),
            _ => continue,
        };
        if !ok || pane_seen(app, uid) {
            continue;
        }
        let Some(s) = app.agents.session(uid) else { continue };
        let body = notify::compose(&s.title, &s.place(), reason, detail.as_deref());
        if app.agents_rt.policy.allow(now, uid, reason) {
            notify::post("Rift", &body, app.config.agents.sound);
        }
    }
}

/// Dock badge = agents waiting for you that you are not looking at.
fn update_badge(app: &mut App) {
    let n = if app.config.agents.notify {
        app.agents.sessions().iter().filter(|s| s.needs_attention && !pane_seen(app, s.pane_uid)).count()
    } else {
        0
    };
    if n != app.agents_rt.badge {
        app.agents_rt.badge = n;
        notify::set_dock_badge(n);
    }
}

// ───────────────────────────── drawing ─────────────────────────────

/// Dock, tab badges and amber pane borders on the output buffer.
pub fn draw(
    reg: &AgentRegistry,
    ui_state: &mut AgentsUi,
    wm: &WindowManager,
    renderer: &mut Renderer,
    buf: &mut [u32],
    w: usize,
    h: usize,
    content_area: PaneRect,
    dock: Option<Rect>,
    now: Instant,
) {
    if !reg.enabled() || reg.sessions().is_empty() && dock.is_none() {
        return;
    }
    let t = renderer.start_time.elapsed().as_secs_f32();
    let tbh = content_area.y;
    ui::draw_tab_badges(buf, w, h, &mut renderer.font, &renderer.theme, tbh, reg, wm.tab_count(), t);
    if let Some(d) = dock {
        ui::draw_dock(buf, w, h, &mut renderer.font, &renderer.theme, d, reg, ui_state, now, t);
    }
    let tab = wm.active_tab();
    let rects: Vec<Rect> = tab
        .layouts(content_area)
        .into_iter()
        .filter(|(i, _, _)| tab.pane(*i).is_some_and(|p| reg.session(p.id).is_some_and(|s| s.needs_attention)))
        .map(|(_, r, _)| Rect::new(r.x, r.y, r.width, r.height))
        .collect();
    if !rects.is_empty() {
        let th = (renderer.cell_height() / 9).max(2);
        ui::draw_attention_borders(buf, w, h, &rects, &renderer.theme, th, t);
    }
}

pub fn current_dock_rect(app: &App) -> Option<Rect> {
    dock_rect(app)
}

// ───────────────────────────── actions ─────────────────────────────

/// Show / hide the dock and re-flow the terminal.
pub fn toggle_dock(app: &mut App) {
    app.agents_ui.toggle();
    if app.agents_ui.visible {
        app.agents_ui.selected = app.agents_ui.selected.or_else(|| app.agents.sessions().first().map(|s| s.pane_uid));
    }
    crate::app::shortcuts::resize_from_window(app);
    app.request_redraw();
}

/// Focus the pane of agent `uid` (switching tabs).
pub fn jump(app: &mut App, uid: usize) {
    if crate::app::panes::focus_pane_uid(app, uid) {
        app.agents_ui.selected = Some(uid);
        app.agents_ui.focused = false;
    } else {
        app.blocks_ui.show_toast("That agent's pane is gone");
    }
    app.request_redraw();
}

/// Jump to the next agent waiting for the user (cycling).
pub fn next_attention(app: &mut App) {
    let current = Some(app.wm.active_pane().id);
    match app.agents.next_attention(current) {
        Some(uid) => jump(app, uid),
        None => {
            app.blocks_ui.show_toast("No agent needs you right now");
            app.request_redraw();
        }
    }
}

fn dock_key(event: &KeyEvent, app: &App) -> DockKey {
    if app.modifiers.super_key() || app.modifiers.control_key() || app.modifiers.alt_key() {
        return DockKey::Chord;
    }
    match &event.logical_key {
        Key::Named(NamedKey::ArrowUp) => DockKey::Up,
        Key::Named(NamedKey::ArrowDown) => DockKey::Down,
        Key::Named(NamedKey::Home) => DockKey::Home,
        Key::Named(NamedKey::End) => DockKey::End,
        Key::Named(NamedKey::Enter) => DockKey::Enter,
        Key::Named(NamedKey::Escape) => DockKey::Escape,
        Key::Character(s) => s.chars().next().map_or(DockKey::Chord, |c| DockKey::Char(c.to_ascii_lowercase())),
        _ => DockKey::Chord,
    }
}

/// Keyboard while the dock has focus. Returns true when the key was consumed.
pub fn handle_key(app: &mut App, event: &KeyEvent) -> bool {
    if !app.agents_ui.visible || !app.agents_ui.focused {
        return false;
    }
    let key = dock_key(event, app);
    if key == DockKey::Chord {
        return false; // global chords still work
    }
    let uids: Vec<usize> = app.agents.sessions().iter().map(|s| s.pane_uid).collect();
    let action = app.agents_ui.on_key(key, &uids);
    match action {
        UiAction::None => {
            if let (Some(dock), Some(i)) = (dock_rect(app), app.agents_ui.selected_index(&uids)) {
                let l = layout_of(app, dock);
                app.agents_ui.scroll = l.scroll_to(i, app.agents_ui.scroll);
            }
        }
        UiAction::Jump(uid) => jump(app, uid),
        UiAction::NextAttention => next_attention(app),
        UiAction::Blur => app.agents_ui.focused = false,
        UiAction::PassThrough => {
            app.agents_ui.focused = false;
            app.request_redraw();
            return false;
        }
    }
    app.request_redraw();
    true
}

pub fn on_cursor_moved(app: &mut App) -> bool {
    let Some(dock) = dock_rect(app) else { return false };
    let (x, y) = (app.cursor_x, app.cursor_y);
    let inside = dock.contains(x, y);
    let hover = if inside && !app.mouse_pressed {
        let l = layout_of(app, dock);
        l.hit(app.agents.sessions().len(), app.agents_ui.scroll, x, y)
    } else {
        None
    };
    if hover != app.agents_ui.hover {
        app.agents_ui.hover = hover;
        app.request_redraw();
    }
    inside && !app.mouse_pressed
}

pub fn on_mouse_press(app: &mut App) -> bool {
    let (x, y) = (app.cursor_x, app.cursor_y);
    let Some(dock) = dock_rect(app).filter(|d| d.contains(x, y)) else {
        if app.agents_ui.focused {
            app.agents_ui.focused = false;
            app.request_redraw();
        }
        return false;
    };
    app.agents_ui.focused = true;
    let l = layout_of(app, dock);
    let sessions = app.agents.sessions();
    if let Some(i) = l.hit(sessions.len(), app.agents_ui.scroll, x, y) {
        let uid = sessions[i].pane_uid;
        app.agents_ui.selected = Some(uid);
        jump(app, uid);
    }
    app.request_redraw();
    true
}

pub fn on_wheel(app: &mut App, delta: MouseScrollDelta) -> bool {
    let Some(dock) = dock_rect(app).filter(|d| d.contains(app.cursor_x, app.cursor_y)) else { return false };
    let y = match delta {
        MouseScrollDelta::LineDelta(_, y) => y as f64,
        MouseScrollDelta::PixelDelta(p) => p.y / app.renderer.cell_height().max(1) as f64 / 3.0,
    };
    let steps = if y > 0.0 { -1isize } else if y < 0.0 { 1 } else { 0 };
    let l = layout_of(app, dock);
    let max = l.max_scroll(app.agents.sessions().len());
    app.agents_ui.scroll = (app.agents_ui.scroll as isize + steps).clamp(0, max as isize) as usize;
    app.request_redraw();
    true
}

// ───────────────────────────── New Agent ─────────────────────────────

/// Agent CLIs on this machine (cached briefly).
pub fn installed(app: &mut App) -> Vec<AgentKind> {
    let now = Instant::now();
    if let Some((at, v)) = &app.agents_rt.installed {
        if now.duration_since(*at) < INSTALLED_TTL {
            return v.clone();
        }
    }
    let v = launch::installed_agents();
    app.agents_rt.installed = Some((now, v.clone()));
    v
}

/// Agent used by layouts: `[agents] default_agent`, else the first installed.
pub fn default_agent(app: &mut App) -> Option<AgentKind> {
    AgentKind::parse(&app.config.agents.default_agent).or_else(|| installed(app).first().copied())
}

fn active_cwd(app: &App) -> PathBuf {
    let p = app.wm.active_pane();
    let cwd = match p.pty {
        PtyKind::Ssh(_) => None,
        _ => p.terminal.cwd.clone(),
    };
    cwd.map(PathBuf::from).filter(|p| p.is_dir()).or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from("/"))
}

fn queue_command(app: &mut App, uid: usize, kind: AgentKind) {
    app.agents_rt.pending.push(PendingCmd { uid, cmd: launch::launch_command(kind), since: Instant::now() });
}

/// Type queued launch commands once the new shell shows its first prompt.
fn flush_pending(app: &mut App, now: Instant) {
    if app.agents_rt.pending.is_empty() {
        return;
    }
    let mut keep = Vec::new();
    for p in std::mem::take(&mut app.agents_rt.pending) {
        let age = now.duration_since(p.since);
        let Some(pane) = app.wm.pane_by_id_mut(p.uid) else { continue };
        if pane.terminal.at_shell_prompt() || age >= PROMPT_WAIT {
            pane.write(format!("{}\r", p.cmd).as_bytes());
        } else if age < PROMPT_GIVE_UP {
            keep.push(p);
        }
    }
    app.agents_rt.pending = keep;
}

fn dir_label(dir: &Path) -> String {
    match git::repo_info(dir) {
        Some(i) => i.branch.unwrap_or(i.name),
        None => dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "~".into()),
    }
}

fn pane_dims(app: &App) -> (usize, usize) {
    let p = app.wm.active_pane();
    (p.terminal.cols, p.terminal.rows)
}

fn open_tab(app: &mut App, kind: AgentKind, dir: &Path, label: &str) -> usize {
    let (c, r) = pane_dims(app);
    let title = launch::tab_title(kind, label);
    let uid = app.wm.new_tab_in(c, r, dir.to_str(), Some(&title));
    queue_command(app, uid, kind);
    uid
}

pub fn launch(app: &mut App, l: Launch) {
    match l {
        Launch::Here(kind) => {
            let dir = active_cwd(app);
            let label = dir_label(&dir);
            open_tab(app, kind, &dir, &label);
            crate::app::panes::after_layout_change(app);
        }
        Launch::Worktree(kind) => start_worktrees(app, kind, Target::Tab),
        Launch::Layout { kind, cols, rows } => {
            let target = Target::Grid { cols, rows };
            if git::repo_info(&active_cwd(app)).is_some() {
                start_worktrees(app, kind, target);
            } else {
                app.blocks_ui.show_toast("Not a git repository: the agents will share this directory");
                let dir = active_cwd(app);
                let label = dir_label(&dir);
                let slots = vec![(dir, label); target.count()];
                build_grid(app, kind, cols, rows, slots);
            }
        }
    }
}

fn start_worktrees(app: &mut App, kind: AgentKind, target: Target) {
    let dir = active_cwd(app);
    let Some(info) = git::repo_info(&dir) else {
        app.blocks_ui.show_toast("Not inside a git repository: cannot create a worktree");
        app.request_redraw();
        return;
    };
    let job = launch::start_job(info.root, info.name, kind, target);
    app.agents_rt.jobs.push(job);
    app.request_redraw();
}

fn finish_jobs(app: &mut App) {
    let mut i = 0;
    while i < app.agents_rt.jobs.len() {
        let res = app.agents_rt.jobs[i].rx.try_recv();
        match res {
            Err(std::sync::mpsc::TryRecvError::Empty) => i += 1,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                app.agents_rt.jobs.remove(i);
            }
            Ok(res) => {
                let job = app.agents_rt.jobs.remove(i);
                match res {
                    Ok(plans) => {
                        let slots: Vec<(PathBuf, String)> = plans.iter().map(|p| (p.path.clone(), p.branch.clone())).collect();
                        match job.target {
                            Target::Tab => {
                                if let Some((dir, branch)) = slots.first() {
                                    open_tab(app, job.kind, dir, branch);
                                    app.blocks_ui.show_toast(format!("Created {}", plans[0].shell_command().replace("git worktree add ", "worktree ")));
                                }
                            }
                            Target::Grid { cols, rows } => build_grid(app, job.kind, cols, rows, slots),
                        }
                        crate::app::panes::after_layout_change(app);
                    }
                    Err(e) => {
                        log::error!("agent worktree: {e}");
                        app.blocks_ui.show_toast(format!("Worktree failed: {e}"));
                        app.request_redraw();
                    }
                }
            }
        }
    }
}

/// One tab holding a `cols x rows` grid; pane `i` runs the agent in `slots[i]`.
fn build_grid(app: &mut App, kind: AgentKind, cols: usize, rows: usize, slots: Vec<(PathBuf, String)>) {
    if slots.is_empty() {
        return;
    }
    let (c, r) = pane_dims(app);
    let title = format!("{} \u{b7} grid {}x{}", kind.slug(), cols, rows);
    let first = app.wm.new_tab_in(c, r, slots[0].0.to_str(), Some(&title));
    let mut panes = vec![first];
    let mut columns: Vec<Vec<usize>> = vec![vec![first]];
    let mut refused = false;
    for step in launch::grid_steps(cols, rows) {
        let n = panes.len();
        if n >= slots.len() {
            break;
        }
        let area = app.content_area();
        let min = crate::app::panes::min_size(app);
        let cwd = slots[n].0.to_str().map(str::to_string);
        let (target, dir) = match step {
            GridStep::NewColumn => (columns.last().and_then(|c| c.last()).copied(), SplitDir::Horizontal),
            GridStep::NewRow { col } => (columns.get(col).and_then(|c| c.last()).copied(), SplitDir::Vertical),
        };
        let Some(target) = target else { break };
        match app.wm.split_pane_in(target, dir, area, min, cwd.as_deref()) {
            Some(id) => {
                match step {
                    GridStep::NewColumn => columns.push(vec![id]),
                    GridStep::NewRow { col } => columns[col].push(id),
                }
                panes.push(id);
            }
            None => {
                refused = true;
                break;
            }
        }
    }
    for (i, uid) in panes.iter().enumerate() {
        let _ = i;
        queue_command(app, *uid, kind);
    }
    // Whatever did not fit gets its own tab.
    for (dir, label) in slots.iter().skip(panes.len()) {
        open_tab(app, kind, dir, label);
    }
    if refused {
        app.blocks_ui.show_toast("Window too small for the whole grid: extra agents opened in tabs");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(title: &str, body: &str) -> Notification {
        Notification { title: title.into(), body: body.into() }
    }

    #[test]
    fn notifications_only_route_for_agent_panes() {
        let t0 = Instant::now();
        let mut reg = AgentRegistry::new();
        // No agent in pane 1: the generic notifier keeps handling it.
        assert!(!route_notifications(&mut reg, true, 1, &[note("x", "y")], t0));
        let o = PaneObs { uid: 1, osc_seen: true, block_running: true, running_cmd: Some("claude".into()), ..Default::default() };
        reg.observe_pane(&o, &mut || Probe::Unknown, t0);
        assert!(route_notifications(&mut reg, true, 1, &[note("Claude Code", "needs your permission")], t0));
        assert_eq!(reg.attention_count(), 1);
        // With agent notifications off, the generic path stays in charge.
        assert!(!route_notifications(&mut reg, false, 1, &[], t0));
    }

    #[test]
    fn dir_labels() {
        let d = std::env::temp_dir();
        assert!(!dir_label(&d).is_empty());
    }
}
