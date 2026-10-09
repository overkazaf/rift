//! Tab bar interaction: click / hover / "+" / close "×", drag-to-reorder,
//! middle-click close, inline rename (double-click), and keeping outside tab
//! indexes (the webview's tab) in step with the tab list.

use std::time::Instant;

use winit::event::KeyEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};

use super::mouse::MULTI_CLICK_MS;
use super::{shortcuts, App};
use crate::ui::tabbar::{self, TabEditor, TabHit, TabRects};
use crate::ui::MenuAction;
use crate::window::manager::TabEvent;

/// Pixels the pointer must travel before a press becomes a tab drag.
const DRAG_THRESHOLD: usize = 6;

#[derive(Clone, Copy, Debug)]
pub struct TabDrag {
    pub from_x: usize,
    /// Where the dragged tab is now.
    pub cur: usize,
    pub active: bool,
}

#[derive(Default)]
pub struct TabUi {
    pub hover: Option<TabHit>,
    pub drag: Option<TabDrag>,
    last_click: Option<(Instant, usize)>,
    pub editor: Option<TabEditor>,
}

impl TabUi {
    /// Index of the tab being dragged (only once the drag has started).
    pub fn dragging(&self) -> Option<usize> {
        self.drag.filter(|d| d.active).map(|d| d.cur)
    }
}

fn bar_layout(app: &App) -> Option<TabRects> {
    let w = app.window.as_ref()?.inner_size().width as usize;
    Some(tabbar::layout(w, app.wm.tab_count(), app.tab_bar_height()))
}

fn close_w(app: &App) -> usize {
    app.renderer.cell_width() * 2
}

fn hit(app: &App, x: usize) -> Option<TabHit> {
    let l = bar_layout(app)?;
    tabbar::hit_test(&l, x, close_w(app), app.wm.tab_count() > 1)
}

// ── Mutations that keep outside indexes consistent ──────────────────────

/// Apply pending tab-list changes to the webview's tab index and the editor.
/// Returns true when anything changed.
pub fn sync_tab_events(app: &mut App) -> bool {
    let events = app.wm.take_tab_events();
    if events.is_empty() {
        return false;
    }
    for ev in events {
        match ev {
            TabEvent::Closed(c) => {
                if let Some(t) = app.webview_tab {
                    match tabbar::remap_after_close(t, c) {
                        Some(n) => app.webview_tab = Some(n),
                        // The browser lived in the tab that was closed.
                        None => crate::network::browser::close(app),
                    }
                }
                if let Some(mut ed) = app.mui.tabs.editor.take() {
                    if let Some(n) = tabbar::remap_after_close(ed.idx, c) {
                        ed.idx = n;
                        app.mui.tabs.editor = Some(ed);
                    }
                }
            }
            TabEvent::Moved { from, to } => {
                if let Some(t) = app.webview_tab {
                    app.webview_tab = Some(tabbar::remap_after_move(t, from, to));
                }
                if let Some(ed) = app.mui.tabs.editor.as_mut() {
                    ed.idx = tabbar::remap_after_move(ed.idx, from, to);
                }
            }
        }
    }
    app.mui.tabs.hover = None;
    shortcuts::sync_webview_for_tab(app);
    app.update_title();
    app.request_redraw();
    true
}

fn select_tab(app: &mut App, idx: usize) {
    if idx != app.wm.active_tab {
        app.selection.clear();
        app.mui.scrollbar.reset();
    }
    app.wm.switch_tab(idx);
    shortcuts::sync_webview_for_tab(app);
    app.update_title();
}

fn close_tab(app: &mut App, idx: usize, event_loop: &ActiveEventLoop) {
    if app.wm.tab_count() <= 1 {
        return;
    }
    app.selection.clear();
    app.mui.scrollbar.reset();
    if app.wm.close_tab_at(idx) {
        event_loop.exit();
    }
    sync_tab_events(app);
    app.update_title();
    app.request_redraw();
}

// ── Mouse ───────────────────────────────────────────────────────────────

/// Left press inside the tab bar.
pub fn press(app: &mut App, event_loop: &ActiveEventLoop) {
    let x = app.cursor_x;
    match hit(app, x) {
        Some(TabHit::Plus) => {
            shortcuts::handle_menu_action(app, MenuAction::NewTab, event_loop);
        }
        Some(TabHit::Close(i)) => close_tab(app, i, event_loop),
        Some(TabHit::Tab(i)) => {
            let now = Instant::now();
            let double = matches!(
                app.mui.tabs.last_click,
                Some((t, j)) if j == i && now.duration_since(t).as_millis() <= MULTI_CLICK_MS
            );
            if double {
                app.mui.tabs.last_click = None;
                app.mui.tabs.drag = None;
                start_edit(app, i);
                return;
            }
            app.mui.tabs.last_click = Some((now, i));
            select_tab(app, i);
            app.mui.tabs.drag = Some(TabDrag { from_x: x, cur: i, active: false });
        }
        None => {}
    }
    app.request_redraw();
}

/// Middle click in the tab bar closes the tab under the pointer.
pub fn middle_click(app: &mut App, event_loop: &ActiveEventLoop) {
    let x = app.cursor_x;
    if let Some(i) = hit(app, x).and_then(TabHit::tab) {
        close_tab(app, i, event_loop);
    }
}

/// Pointer motion: hover tracking and drag-reorder. Returns true while a
/// tab press is in progress (the motion belongs to the tab bar).
pub fn on_cursor_moved(app: &mut App) -> bool {
    let (x, y) = (app.cursor_x, app.cursor_y);

    if let Some(mut d) = app.mui.tabs.drag {
        if !d.active && x.abs_diff(d.from_x) >= DRAG_THRESHOLD {
            d.active = true;
        }
        if d.active {
            if let Some(l) = bar_layout(app) {
                let target = tabbar::index_at(&l, x);
                if target != d.cur && app.wm.move_tab(d.cur, target) {
                    d.cur = target;
                    sync_tab_events(app);
                }
            }
            app.mui.tabs.hover = Some(TabHit::Tab(d.cur));
            app.request_redraw();
        }
        app.mui.tabs.drag = Some(d);
        return true;
    }

    let new = if y < app.tab_bar_height() { hit(app, x) } else { None };
    if new != app.mui.tabs.hover {
        app.mui.tabs.hover = new;
        app.request_redraw();
    }
    false
}

/// Left button released: ends a tab drag. True when one was in progress.
pub fn end_drag(app: &mut App) -> bool {
    let was = app.mui.tabs.drag.take().is_some();
    if was {
        app.request_redraw();
    }
    was
}

// ── Rename ──────────────────────────────────────────────────────────────

fn start_edit(app: &mut App, idx: usize) {
    let Some(tab) = app.wm.tabs.get(idx) else { return };
    app.mui.tabs.editor = Some(TabEditor::new(idx, &tab.title));
    // Put the IME candidate window next to the field.
    if let (Some(l), Some(w)) = (bar_layout(app), app.window.as_ref()) {
        if let Some(&slot) = l.tabs.get(idx) {
            let (x, y, fw, fh) = tabbar::editor_rect(slot, app.tab_bar_height());
            app.ime_area = None;
            w.set_ime_cursor_area(
                winit::dpi::PhysicalPosition::new(x as i32, y as i32),
                winit::dpi::PhysicalSize::new(fw.max(1) as u32, fh.max(1) as u32),
            );
        }
    }
    app.request_redraw();
}

pub fn commit_edit(app: &mut App) {
    if let Some(ed) = app.mui.tabs.editor.take() {
        app.wm.rename_tab(ed.idx, &ed.text);
        app.ime_preedit.clear();
        app.ime_area = None;
        app.update_title();
        app.request_redraw();
    }
}

pub fn cancel_edit(app: &mut App) {
    if app.mui.tabs.editor.take().is_some() {
        app.ime_preedit.clear();
        app.ime_area = None;
        app.request_redraw();
    }
}

/// A press that is not on the editor's own field commits the rename.
pub fn press_outside_editor(app: &mut App) {
    let Some(ed) = &app.mui.tabs.editor else { return };
    let inside = bar_layout(app)
        .and_then(|l| l.tabs.get(ed.idx).copied())
        .map_or(false, |slot| {
            let (x, y, w, h) = tabbar::editor_rect(slot, app.tab_bar_height());
            let (px, py) = (app.cursor_x, app.cursor_y);
            px >= x && px < x + w && py >= y && py < y + h
        });
    if !inside {
        commit_edit(app);
    }
}

/// Insert committed IME text into the rename field. False when not editing.
pub fn insert_text(app: &mut App, text: &str) -> bool {
    match app.mui.tabs.editor.as_mut() {
        Some(ed) => {
            ed.insert_str(text);
            app.request_redraw();
            true
        }
        None => false,
    }
}

/// Key handling while renaming (consumes every key).
pub fn handle_key(app: &mut App, event: &KeyEvent) -> bool {
    let m = app.modifiers;
    let Some(ed) = app.mui.tabs.editor.as_mut() else { return false };
    match &event.logical_key {
        Key::Named(NamedKey::Enter) => {
            commit_edit(app);
            return true;
        }
        Key::Named(NamedKey::Escape) => {
            cancel_edit(app);
            return true;
        }
        Key::Named(NamedKey::Backspace) => ed.backspace(),
        Key::Named(NamedKey::Delete) => ed.delete(),
        Key::Named(NamedKey::ArrowLeft) => ed.left(),
        Key::Named(NamedKey::ArrowRight) => ed.right(),
        Key::Named(NamedKey::Home) => ed.home(),
        Key::Named(NamedKey::End) => ed.end(),
        Key::Named(NamedKey::Space) if !m.super_key() && !m.control_key() => ed.insert_str(" "),
        Key::Character(s) if m.super_key() => {
            if s.eq_ignore_ascii_case("v") {
                if let Some(text) = crate::window::selection::paste_from_clipboard() {
                    // One line only.
                    ed.insert_str(&crate::window::selection::sanitize_paste(text.lines().next().unwrap_or(""), false));
                }
            }
        }
        Key::Character(s) if !m.control_key() => ed.insert_str(s),
        _ => {}
    }
    app.request_redraw();
    true
}
