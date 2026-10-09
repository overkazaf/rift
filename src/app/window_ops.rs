//! Window lifecycle on top of the swap-in model described on [`App`]:
//! activating a window, finding panes across windows, creating and closing
//! windows, quitting and saving the multi-window session.

use std::sync::Arc;

use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use super::windows::{pane_window, WinId};
use super::{lifecycle, App, WindowState};
use crate::tools::session::{Geometry, WindowSession};
use crate::window::{Pane, WindowManager};

// ── activation & cross-window access ────────────────────────────────────

impl App {
    /// Make window `id` the active one (see the type docs of [`App`]).
    /// No-op for the window that is already active or an unknown id.
    ///
    /// Rule for callers: anything that activates another window must put the
    /// previous one back before returning to code that keeps working on
    /// "the" window (see [`App::each_window`], [`App::with_window`]).
    pub fn activate(&mut self, id: WinId) {
        super::windows::activate(&mut self.win, &mut self.parked, id);
    }

    /// Run `f` with window `id` active, then re-activate the previous window.
    pub fn with_window<R>(&mut self, id: WinId, f: impl FnOnce(&mut App) -> R) -> Option<R> {
        if !self.windows.contains(id) {
            return None;
        }
        let back = self.win.id;
        self.activate(id);
        let r = f(self);
        // `f` may have closed the window we came from; then stay where we are.
        self.activate(back);
        Some(r)
    }

    /// Activate the OS-focused window (where menu actions and global work go).
    pub fn activate_focused(&mut self) {
        if let Some(f) = self.windows.focused() {
            self.activate(f);
        }
    }

    /// Run `f` once per window with that window active, then re-activate the
    /// window that was active before. `f` must not add or remove windows.
    pub fn each_window(&mut self, mut f: impl FnMut(&mut App)) {
        let back = self.win.id;
        for id in self.windows.ids().to_vec() {
            self.activate(id);
            f(self);
        }
        self.activate(back);
    }

    /// The active window is the one the OS has focused.
    pub fn is_focused_window(&self) -> bool {
        self.windows.focused() == Some(self.win.id)
    }

    pub fn window_state(&self, id: WinId) -> Option<&WindowState> {
        if self.win.id == id { Some(&self.win) } else { self.parked.get(&id) }
    }

    pub fn window_state_mut(&mut self, id: WinId) -> Option<&mut WindowState> {
        if self.win.id == id { Some(&mut self.win) } else { self.parked.get_mut(&id) }
    }

    /// All windows' states, active one first.
    pub fn all_window_states(&self) -> impl Iterator<Item = &WindowState> {
        std::iter::once(&self.win).chain(self.parked.values())
    }

    pub fn all_window_states_mut(&mut self) -> impl Iterator<Item = &mut WindowState> {
        std::iter::once(&mut self.win).chain(self.parked.values_mut())
    }

    pub fn wm_of_win(&self, id: WinId) -> Option<&WindowManager> {
        self.window_state(id).map(|w| &w.wm)
    }

    /// The window manager that owns pane `uid` (pane ids embed their window).
    pub fn wm_of_pane(&self, uid: usize) -> Option<&WindowManager> {
        self.wm_of_win(pane_window(uid)).filter(|wm| wm.locate_pane(uid).is_some())
    }

    pub fn wm_of_pane_mut(&mut self, uid: usize) -> Option<&mut WindowManager> {
        self.window_state_mut(pane_window(uid)).map(|w| &mut w.wm).filter(|wm| wm.locate_pane(uid).is_some())
    }

    pub fn pane_ref(&self, uid: usize) -> Option<&Pane> {
        let wm = self.wm_of_pane(uid)?;
        let (t, p) = wm.locate_pane(uid)?;
        wm.tabs[t].pane(p)
    }

    pub fn pane_mut(&mut self, uid: usize) -> Option<&mut Pane> {
        self.wm_of_pane_mut(uid)?.pane_by_id_mut(uid)
    }

    pub fn pane_exists(&self, uid: usize) -> bool {
        self.wm_of_pane(uid).is_some()
    }

    /// (window, tab index, leaf index) of pane `uid`.
    pub fn locate_pane_global(&self, uid: usize) -> Option<(WinId, usize, usize)> {
        let (t, p) = self.wm_of_pane(uid)?.locate_pane(uid)?;
        Some((pane_window(uid), t, p))
    }

    /// Every live pane id of every window.
    pub fn all_pane_ids(&self) -> Vec<usize> {
        self.all_window_states().flat_map(|w| w.wm.tabs.iter().flat_map(|t| t.panes()).map(|p| p.id)).collect()
    }

    /// Is the user looking at pane `uid`: its window is the OS-focused one and
    /// the pane is shown in that window's active tab?
    pub fn pane_visible_to_user(&self, uid: usize) -> bool {
        let Some((win, ti, pi)) = self.locate_pane_global(uid) else { return false };
        let Some(ws) = self.window_state(win) else { return false };
        ws.window_focused
            && self.windows.focused() == Some(win)
            && ti == ws.wm.active_tab
            && (!ws.wm.tabs[ti].is_zoomed() || pi == ws.wm.tabs[ti].active)
    }

    /// Show pane `uid` to the user: switch to its tab and focus the pane in
    /// its window, then raise + focus that window (possibly another one).
    /// The active window does not change. False when the pane is gone.
    pub fn reveal_pane(&mut self, uid: usize) -> bool {
        let Some((win, _, _)) = self.locate_pane_global(uid) else { return false };
        let ok = self.with_window(win, |app| super::panes::focus_pane_uid(app, uid)).unwrap_or(false);
        self.focus_window(win);
        ok
    }

    /// Bring window `id` to the front: the registry marks it focused right
    /// away (menu actions and global overlays follow) and the OS is asked to
    /// focus it. The active window does not change.
    pub fn focus_window(&mut self, id: WinId) {
        if !self.windows.focus(id) {
            return;
        }
        if let Some(w) = self.window_state(id).and_then(|ws| ws.window.as_ref()) {
            w.focus_window();
        }
        self.request_redraw_all();
    }

    /// Apply a theme to every window's renderer (the committed theme is global).
    pub fn set_theme_all(&mut self, theme: crate::config::Theme) {
        for ws in self.all_window_states_mut() {
            ws.renderer.set_theme(theme.clone());
        }
        self.request_redraw_all();
    }

    /// Repaint every window (used when something global changed: theme,
    /// which window owns the global overlays, ...).
    pub fn request_redraw_all(&mut self) {
        for ws in self.all_window_states_mut() {
            ws.needs_render = true;
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
        }
    }
}

impl super::windows::WindowSlot for WindowState {
    fn id(&self) -> WinId {
        self.id
    }

    /// The agent dock's process-wide data (card info, autopilot switches)
    /// lives inside `AgentsUi` for historical reasons; it rides along with the
    /// active window so every window's dock sees the same data.
    fn take_shared_from(&mut self, prev: &mut Self) {
        self.agents_ui.take_shared_from(&mut prev.agents_ui);
    }
}

/// The window manager that owns pane `uid`, looked up on the window fields
/// directly so callers can keep other `App` fields (`agents`, `review` ...)
/// mutably borrowed at the same time.
pub fn wm_in<'a>(active: &'a WindowState, parked: &'a std::collections::HashMap<WinId, WindowState>, uid: usize) -> Option<&'a WindowManager> {
    let id = pane_window(uid);
    let ws = if active.id == id { Some(active) } else { parked.get(&id) }?;
    ws.wm.locate_pane(uid).is_some().then_some(&ws.wm)
}

/// The OS gave window `id` focus.
pub fn note_focused(app: &mut App, id: WinId) {
    let changed = app.windows.focused() != Some(id);
    app.windows.focus(id);
    if changed {
        // Global overlays (review, MCP activity, workflows ...) follow the focus.
        app.request_redraw_all();
    }
}

// ── running processes ───────────────────────────────────────────────────

/// Panes of `wm` with something still running in them: a foreground job
/// besides the shell, or a live SSH session.
pub fn running_processes(wm: &WindowManager) -> usize {
    use crate::window::pane::PtyKind;
    wm.tabs
        .iter()
        .flat_map(|t| t.panes())
        .filter(|p| p.exited.is_none())
        .filter(|p| match &p.pty {
            PtyKind::Local(_) => p.pty.shell_and_foreground().is_some_and(|(shell, fg)| shell != fg),
            PtyKind::Ssh(_) => true,
            PtyKind::Inert => false,
        })
        .count()
}

/// "Close window with 2 running processes?" wording.
pub fn close_prompt(n: usize) -> String {
    format!("Close window with {n} running process{}?", if n == 1 { "" } else { "es" })
}

// ── closing ─────────────────────────────────────────────────────────────

/// Ask for window `id` to be closed. With `confirm`, a window that still has
/// running processes first shows a confirmation in that window. Closing is
/// deferred until the current event handler returns (see [`flush_closes`]).
/// `save_if_last`: when this is the last window, store the session before
/// quitting (the close button / Close Window do; closing the last pane
/// does not).
pub fn request_close(app: &mut App, id: WinId, confirm: bool, save_if_last: bool) {
    if !app.windows.contains(id) {
        return;
    }
    if confirm {
        let n = app.wm_of_win(id).map_or(0, running_processes);
        if n > 0 {
            app.activate(id);
            crate::ui::confirm::show_close_window(app, id, n, save_if_last);
            app.request_redraw();
            return;
        }
    }
    if !app.close_queue.iter().any(|(w, _)| *w == id) {
        app.close_queue.push((id, save_if_last));
    }
}

/// The close button of the active window.
pub fn request_close_active(app: &mut App, event_loop: &ActiveEventLoop) {
    request_close(app, app.win.id, true, true);
    flush_closes(app, event_loop);
}

/// Close the active pane; when it is the window's last one, close the window
/// (after confirming if something is still running).
pub fn close_active_pane(app: &mut App) {
    let wm = &app.win.wm;
    if wm.tab_count() == 1 && wm.active_tab().pane_count() == 1 {
        request_close(app, app.win.id, true, false);
    } else {
        app.win.wm.close_current();
    }
}

/// Execute the queued window closes. Call at the end of event handlers.
pub fn flush_closes(app: &mut App, event_loop: &ActiveEventLoop) {
    while let Some((id, save)) = app.close_queue.pop() {
        close_now(app, event_loop, id, save);
    }
}

fn close_now(app: &mut App, event_loop: &ActiveEventLoop, id: WinId, save_if_last: bool) {
    if !app.windows.contains(id) {
        return;
    }
    if app.windows.len() <= 1 {
        // Closing the last window quits the app.
        if save_if_last {
            save_all(app);
        }
        event_loop.exit();
        return;
    }
    // The registry decides who takes over; the dock's shared data moves along.
    let Some(dropped) = super::windows::remove_window(&mut app.windows, &mut app.win, &mut app.parked, id) else {
        log::error!("close window {id}: no such window");
        return;
    };
    log::info!("Window {id} closed ({} left)", app.windows.len());
    // Dropping the state kills its shells, drops its webview and GPU surface.
    drop(dropped);
    if let Some(f) = app.windows.focused() {
        app.activate(f);
        if let Some(w) = &app.win.window {
            w.focus_window();
        }
    }
    app.request_redraw_all();
}

// ── quit & session ──────────────────────────────────────────────────────

/// Save config + the whole multi-window session and exit.
pub fn quit(app: &mut App, event_loop: &ActiveEventLoop) {
    save_all(app);
    event_loop.exit();
}

pub fn save_all(app: &mut App) {
    crate::config::toml::save_config(&app.config);
    let windows = session_windows(app);
    let focused = app.windows.focused().and_then(|f| app.windows.ids().iter().position(|w| *w == f)).unwrap_or(0);
    if let Err(e) = crate::tools::session::save_windows(windows, focused) {
        log::warn!("Failed to save session: {e}");
    }
}

/// Snapshot of every window in creation order.
fn session_windows(app: &App) -> Vec<WindowSession> {
    app.windows
        .ids()
        .iter()
        .filter_map(|id| app.window_state(*id))
        .map(|ws| WindowSession {
            geometry: ws.window.as_ref().and_then(|w| geometry_of(w)),
            state: crate::tools::session::snapshot(&ws.wm, &app.workflows.queues_for_session(&ws.wm)),
        })
        .collect()
}

/// Where `WindowAttributes::with_position` would put a window to land exactly
/// here. winit places the client area (macOS: the content rect), so reading
/// the inner position makes save -> restore a fixed point (the outer position
/// would creep up by the title bar height on every restart).
fn window_position(w: &Window) -> Option<winit::dpi::PhysicalPosition<i32>> {
    w.inner_position().or_else(|_| w.outer_position()).ok()
}

/// Logical position and size of an OS window (None where the platform
/// cannot tell the position, e.g. Wayland).
fn geometry_of(w: &Window) -> Option<Geometry> {
    let scale = w.scale_factor();
    let pos = window_position(w)?.to_logical::<f64>(scale);
    let size = w.inner_size().to_logical::<f64>(scale);
    Some(Geometry { x: pos.x.round() as i32, y: pos.y.round() as i32, w: size.width.round() as u32, h: size.height.round() as u32 })
}

/// A saved window is worth placing where it was only while a decent part of
/// it is still on a screen (monitors given as logical x, y, w, h). When the
/// platform reports no monitors at all there is nothing to judge by, so the
/// saved position is trusted.
pub fn geometry_visible(g: &Geometry, monitors: &[(i32, i32, u32, u32)]) -> bool {
    const MIN_VISIBLE: i64 = 80;
    monitors.is_empty() || monitors.iter().any(|&(mx, my, mw, mh)| {
        let ox = (g.x as i64 + g.w as i64).min(mx as i64 + mw as i64) - (g.x as i64).max(mx as i64);
        let oy = (g.y as i64 + g.h as i64).min(my as i64 + mh as i64) - (g.y as i64).max(my as i64);
        ox >= MIN_VISIBLE.min(g.w as i64) && oy >= MIN_VISIBLE.min(g.h as i64)
    })
}

fn monitors_logical(event_loop: &ActiveEventLoop) -> Vec<(i32, i32, u32, u32)> {
    event_loop
        .available_monitors()
        .map(|m| {
            let s = m.scale_factor();
            let p = m.position().to_logical::<f64>(s);
            let z = m.size().to_logical::<f64>(s);
            (p.x as i32, p.y as i32, z.width as u32, z.height as u32)
        })
        .collect()
}

// ── creating ────────────────────────────────────────────────────────────

/// How to open a window.
#[derive(Default)]
pub struct OpenSpec {
    /// Working directory of the first shell.
    pub cwd: Option<String>,
    /// Saved tabs / layout to restore (session restore).
    pub session: Option<WindowSession>,
    /// Inner size in logical px (default: the configured cols x rows).
    pub size: Option<(u32, u32)>,
    /// Position in logical px; ignored when it is no longer on any screen.
    pub position: Option<(i32, i32)>,
}

impl OpenSpec {
    pub fn from_session(ws: WindowSession) -> Self {
        let (size, position) = match ws.geometry {
            Some(g) => (Some((g.w, g.h)), Some((g.x, g.y))),
            None => (None, None),
        };
        Self { cwd: None, session: Some(ws), size, position }
    }
}

/// File > New Window / Cmd+N: a fresh window whose shell starts in the
/// focused pane's working directory (OSC 7), as big as the current one.
pub fn new_window(app: &mut App, event_loop: &ActiveEventLoop) {
    use crate::window::pane::PtyKind;
    let p = app.win.wm.active_pane();
    let cwd = match p.pty {
        PtyKind::Local(_) | PtyKind::Inert => p.terminal.cwd.clone(),
        // A remote shell's directory means nothing locally.
        PtyKind::Ssh(_) => None,
    };
    // As big as the current window, one title-bar step down and to the right so
    // it does not hide the original.
    let (size, position) = match app.win.window.as_ref() {
        Some(w) => {
            let scale = w.scale_factor();
            let s = w.inner_size().to_logical::<f64>(scale);
            let pos = window_position(w).map(|p| {
                let p = p.to_logical::<f64>(scale);
                (p.x as i32 + CASCADE_STEP, p.y as i32 + CASCADE_STEP)
            });
            (Some((s.width as u32, s.height as u32)), pos)
        }
        None => (None, None),
    };
    open_window(app, event_loop, OpenSpec { cwd, size, position, ..Default::default() });
}

/// Logical px a new window is offset from the window it was opened from.
const CASCADE_STEP: i32 = 28;

/// Create a window and make it the active, focused one. `None` if the OS
/// refused to create it.
pub fn open_window(app: &mut App, event_loop: &ActiveEventLoop, spec: OpenSpec) -> Option<WinId> {
    let back = app.win.id;
    let id = app.windows.reserve();
    let mut attrs = lifecycle::window_attributes();
    if let Some((w, h)) = spec.size {
        attrs = attrs.with_inner_size(winit::dpi::LogicalSize::new(w as f64, h as f64));
    }
    if let (Some((x, y)), Some((w, h))) = (spec.position, spec.size) {
        if geometry_visible(&Geometry { x, y, w, h }, &monitors_logical(event_loop)) {
            attrs = attrs.with_position(winit::dpi::LogicalPosition::new(x as f64, y as f64));
        }
    }
    let window = match event_loop.create_window(attrs) {
        Ok(w) => Arc::new(w),
        Err(e) => {
            log::error!("Could not create a window: {e}");
            app.windows.remove(id);
            return None;
        }
    };
    app.windows.bind(id, window.id());

    let cols = app.config.cols as usize;
    let rows = app.config.rows as usize;
    let proxy = app.win.wm.get_proxy();
    let mut wm = WindowManager::for_window(proxy, cols, rows, id, spec.cwd.as_deref());
    wm.set_scrollback_lines(app.config.scrollback_lines);
    let mut queues = Vec::new();
    if let Some(ws) = &spec.session {
        queues = crate::tools::session::restore_window(&mut wm, ws);
    }
    app.workflows.queues.extend(queues);

    let renderer = lifecycle::build_renderer(&app.config);
    let state = WindowState::new(id, &app.config, renderer, wm, false);
    app.parked.insert(id, state);
    app.windows.focus(id);
    app.activate(id);
    lifecycle::attach_window(app, window, spec.size);
    app.update_title();
    app.request_redraw();
    // The caller keeps working on the window it was running in.
    app.activate(back);
    log::info!("Window {id} opened");
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: [(i32, i32, u32, u32); 1] = [(0, 0, 1440, 900)];

    #[test]
    fn saved_geometry_on_screen_is_kept() {
        assert!(geometry_visible(&Geometry { x: 100, y: 80, w: 800, h: 600 }, &SCREEN));
        // Partly off the edge is fine while enough of it shows.
        assert!(geometry_visible(&Geometry { x: 1000, y: 700, w: 800, h: 600 }, &SCREEN));
    }

    #[test]
    fn geometry_on_an_unplugged_monitor_is_dropped() {
        assert!(!geometry_visible(&Geometry { x: 3000, y: 100, w: 800, h: 600 }, &SCREEN));
        assert!(!geometry_visible(&Geometry { x: -2000, y: 0, w: 800, h: 600 }, &SCREEN));
        // No monitor information: trust the saved position.
        assert!(geometry_visible(&Geometry { x: 100, y: 100, w: 800, h: 600 }, &[]));
    }

    #[test]
    fn geometry_spanning_a_second_monitor_counts() {
        let two = [(0, 0, 1440, 900), (1440, 0, 1920, 1080)];
        assert!(geometry_visible(&Geometry { x: 1500, y: 50, w: 800, h: 600 }, &two));
    }

    #[test]
    fn close_prompt_wording() {
        assert_eq!(close_prompt(1), "Close window with 1 running process?");
        assert_eq!(close_prompt(2), "Close window with 2 running processes?");
    }

    #[test]
    fn inert_panes_are_not_running_processes() {
        let wm = WindowManager::headless(80, 24);
        assert_eq!(running_processes(&wm), 0);
    }
}
