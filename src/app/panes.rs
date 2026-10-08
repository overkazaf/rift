//! Pane management: keyboard shortcuts, menu / palette commands and the
//! mouse helpers (divider double-click, focus changes) for split panes.
//!
//! All geometry lives in `window::tab`; this module only wires it to the app.

use winit::event::KeyEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};

use super::App;
use crate::window::tab::{Direction, MinSize, PaneCmd, SplitDir};

/// Double-click window for divider equalize.
pub const DOUBLE_CLICK_MS: u128 = 300;

/// True while keyboard focus belongs to the embedded browser, in which case
/// Cmd+[ / Cmd+] / Cmd+W belong to it. The browser has no focus flag yet;
/// when it grows one (e.g. `app.browser_focused`), return it here.
fn browser_has_focus(_app: &App) -> bool {
    false
}

pub fn min_size(app: &App) -> MinSize {
    MinSize::from_cells(app.renderer.cell_width(), app.renderer.cell_height())
}

fn arrow(key: &Key) -> Option<Direction> {
    match key {
        Key::Named(NamedKey::ArrowLeft) => Some(Direction::Left),
        Key::Named(NamedKey::ArrowRight) => Some(Direction::Right),
        Key::Named(NamedKey::ArrowUp) => Some(Direction::Up),
        Key::Named(NamedKey::ArrowDown) => Some(Direction::Down),
        _ => None,
    }
}

/// Map a key press to a pane command. Returns None for unrelated keys.
fn shortcut_cmd(app: &App, event: &KeyEvent) -> Option<PaneCmd> {
    let m = app.modifiers;
    let (sup, ctrl, alt, shift) = (m.super_key(), m.control_key(), m.alt_key(), m.shift_key());

    if let Some(d) = arrow(&event.logical_key) {
        return if sup && alt && !ctrl && !shift {
            Some(PaneCmd::Focus(d))
        } else if sup && ctrl && shift && !alt {
            Some(PaneCmd::Swap(d))
        } else if sup && ctrl && !shift && !alt {
            Some(PaneCmd::Resize(d))
        } else if alt && !sup && !ctrl && !shift {
            // Plain Alt+Arrow keeps switching panes, but only when there is a
            // pane that way; otherwise the shell keeps its word-jump.
            let tab = app.wm.active_tab();
            tab.neighbor(app.content_area(), d).map(|_| PaneCmd::Focus(d))
        } else {
            None
        };
    }

    if sup && shift && !ctrl && !alt && matches!(event.logical_key, Key::Named(NamedKey::Enter)) {
        return Some(PaneCmd::Zoom);
    }
    if let Key::Character(s) = &event.logical_key {
        let s = s.as_str();
        if sup && ctrl && !alt && matches!(s, "=" | "+") {
            return Some(PaneCmd::Equalize);
        }
        if sup && !ctrl && !alt && !shift && !browser_has_focus(app) {
            match s {
                "[" => return Some(PaneCmd::FocusPrev),
                "]" => return Some(PaneCmd::FocusNext),
                "w" | "W" => return Some(PaneCmd::ClosePane),
                _ => {}
            }
        }
    }
    None
}

/// Handle a pane shortcut. Returns true when the key was consumed.
pub fn try_pane_shortcut(app: &mut App, event: &KeyEvent, event_loop: &ActiveEventLoop) -> bool {
    let Some(cmd) = shortcut_cmd(app, event) else { return false };
    // One-shot actions should not auto-repeat.
    let repeatable = matches!(cmd, PaneCmd::Resize(_) | PaneCmd::Focus(_));
    if event.repeat && !repeatable {
        return true;
    }
    run_pane_cmd(app, cmd, event_loop);
    true
}

/// Focus pane `idx` of the active tab, sending focus in/out reports and
/// dropping any selection. No-op when it is already focused.
pub fn focus_pane_idx(app: &mut App, idx: usize) {
    let old = app.wm.active_tab().active;
    if old == idx || idx >= app.wm.active_tab().pane_count() {
        return;
    }
    notify_focus(app, old, idx);
    app.wm.active_tab_mut().focus_pane(idx);
    app.selection.clear();
}

/// Send CSI O / CSI I to the panes losing / gaining focus (DECSET 1004).
fn notify_focus(app: &mut App, old: usize, new: usize) {
    let tab = app.wm.active_tab_mut();
    if let Some(p) = tab.pane_mut(old) {
        if p.terminal.focus_reporting {
            p.write(b"\x1b[O");
        }
    }
    if let Some(p) = tab.pane_mut(new) {
        if p.terminal.focus_reporting {
            p.write(b"\x1b[I");
        }
    }
}

/// Execute a pane command from any entry point (keyboard, menu, palette).
pub fn run_pane_cmd(app: &mut App, cmd: PaneCmd, event_loop: &ActiveEventLoop) {
    let area = app.content_area();
    let min = min_size(app);
    let old_active = app.wm.active_tab().active;
    let old_tab = app.wm.active_tab;

    match cmd {
        PaneCmd::SplitRight => {
            app.wm.split_active(SplitDir::Horizontal, area, min);
        }
        PaneCmd::SplitDown => {
            app.wm.split_active(SplitDir::Vertical, area, min);
        }
        PaneCmd::ClosePane => {
            if app.wm.close_current() {
                event_loop.exit();
            }
        }
        PaneCmd::Zoom => {
            let on = app.wm.active_tab_mut().toggle_zoom();
            log::info!("Pane zoom: {}", if on { "on" } else { "off" });
        }
        PaneCmd::Equalize => {
            let tab = app.wm.active_tab_mut();
            tab.unzoom();
            tab.equalize();
        }
        PaneCmd::Focus(d) => {
            app.wm.active_tab_mut().focus_dir(area, d);
        }
        PaneCmd::Swap(d) => {
            app.wm.active_tab_mut().swap_dir(area, d);
        }
        PaneCmd::Resize(d) => {
            app.wm.active_tab_mut().resize_dir(area, d, min);
        }
        PaneCmd::FocusNext => app.wm.focus_next_pane(),
        PaneCmd::FocusPrev => app.wm.focus_prev_pane(),
    }

    // Focus reports for pure focus moves within the same tab.
    if app.wm.active_tab == old_tab && matches!(cmd, PaneCmd::Focus(_) | PaneCmd::FocusNext | PaneCmd::FocusPrev) {
        let new_active = app.wm.active_tab().active;
        if new_active != old_active {
            notify_focus(app, old_active, new_active);
        }
    }
    if new_selection_stale(cmd) {
        app.selection.clear();
    }

    if let Some(win) = &app.window {
        let s = win.inner_size();
        super::lifecycle::handle_resize(app, s.width, s.height);
    }
    app.update_title();
    app.request_redraw();
}

/// Commands after which a selection (stored in pane-local cells) is invalid.
fn new_selection_stale(cmd: PaneCmd) -> bool {
    !matches!(cmd, PaneCmd::Equalize | PaneCmd::Resize(_))
}

/// Double-click on divider `border` (within `DOUBLE_CLICK_MS` of the last
/// click on the same divider) resets it to 0.5. Returns true when equalized.
pub fn register_border_click(app: &mut App, border: usize) -> bool {
    let now = std::time::Instant::now();
    let is_double = matches!(
        app.last_border_click,
        Some((t, b)) if b == border && now.duration_since(t).as_millis() <= DOUBLE_CLICK_MS
    );
    if is_double {
        app.last_border_click = None;
        if app.wm.active_tab_mut().equalize_border(border) {
            if let Some(win) = &app.window {
                let s = win.inner_size();
                super::lifecycle::handle_resize(app, s.width, s.height);
            }
            app.request_redraw();
            return true;
        }
    } else {
        app.last_border_click = Some((now, border));
    }
    false
}
