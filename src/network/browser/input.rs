//! Keyboard and mouse handling for the browser chrome.

use std::time::{Duration, Instant};

use winit::event::KeyEvent;
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
use winit::window::CursorIcon;

use crate::app::App;
use crate::window::selection::{copy_to_clipboard, paste_from_clipboard};

use super::chrome::{Hit, MAX_RATIO, MIN_RATIO};
use super::{begin_edit, end_edit, focus_page, focus_terminal, relayout, toolbar_click};

const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Lowercase letter for Cmd/Ctrl shortcuts, with a physical-key fallback
/// (the logical key can change under Cmd on some macOS layouts).
fn letter(event: &KeyEvent) -> Option<char> {
    if let Key::Character(s) = &event.logical_key {
        return s.chars().next().map(|c| c.to_ascii_lowercase());
    }
    if let PhysicalKey::Code(code) = event.physical_key {
        return Some(match code {
            KeyCode::KeyA => 'a',
            KeyCode::KeyC => 'c',
            KeyCode::KeyV => 'v',
            KeyCode::KeyX => 'x',
            KeyCode::KeyL => 'l',
            KeyCode::KeyE => 'e',
            _ => return None,
        });
    }
    None
}

/// Key handling while the address field has focus. Always consumes the event.
pub fn handle_key(app: &mut App, event: &KeyEvent) -> bool {
    let m = app.win.modifiers;
    let (cmd, alt, shift, ctrl) = (m.super_key(), m.alt_key(), m.shift_key(), m.control_key());

    match &event.logical_key {
        Key::Named(NamedKey::Escape) => {
            end_edit(app, false);
            return true;
        }
        Key::Named(NamedKey::Enter) => {
            end_edit(app, true);
            return true;
        }
        Key::Named(NamedKey::ArrowLeft) => {
            let f = &mut app.win.browser.field;
            if cmd {
                f.move_home(shift)
            } else if alt {
                f.move_word_left(shift)
            } else {
                f.move_left(shift)
            }
        }
        Key::Named(NamedKey::ArrowRight) => {
            let f = &mut app.win.browser.field;
            if cmd {
                f.move_end(shift)
            } else if alt {
                f.move_word_right(shift)
            } else {
                f.move_right(shift)
            }
        }
        Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::Home) => app.win.browser.field.move_home(shift),
        Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::End) => app.win.browser.field.move_end(shift),
        Key::Named(NamedKey::Backspace) => {
            let f = &mut app.win.browser.field;
            if cmd {
                f.delete_to_start()
            } else if alt {
                f.delete_word_back()
            } else {
                f.backspace()
            }
        }
        Key::Named(NamedKey::Delete) => app.win.browser.field.delete_forward(),
        _ if cmd => match letter(event) {
            Some('a') => app.win.browser.field.select_all(),
            Some('c') => {
                let f = &app.win.browser.field;
                copy_to_clipboard(&f.selected_text().unwrap_or_else(|| f.text()));
            }
            Some('x') => {
                if let Some(t) = app.win.browser.field.cut() {
                    copy_to_clipboard(&t);
                }
            }
            Some('v') => {
                if let Some(t) = paste_from_clipboard() {
                    app.win.browser.field.insert_str(&t);
                }
            }
            Some('l') => app.win.browser.field.select_all(),
            _ => {} // swallow other Cmd combos so they don't reach the terminal
        },
        _ if ctrl => match letter(event) {
            Some('a') => app.win.browser.field.move_home(shift),
            Some('e') => app.win.browser.field.move_end(shift),
            _ => {}
        },
        Key::Named(NamedKey::Space) | Key::Character(_) => {
            let typed = match (&event.text, &event.logical_key) {
                (Some(t), _) => t.to_string(),
                (None, Key::Character(s)) => s.to_string(),
                _ => " ".to_string(),
            };
            app.win.browser.field.insert_str(&typed);
        }
        _ => {}
    }
    app.request_redraw();
    true
}

/// Mouse move. Returns true when the browser consumed it (cursor inside the
/// browser region or a drag is in progress).
pub fn on_cursor_moved(app: &mut App) -> bool {
    let (x, y) = (app.win.cursor_x, app.win.cursor_y);

    if app.win.browser.divider_drag {
        // The browser's ratio is relative to the width left of the chat dock.
        let full_w = app.win.window.as_ref().map_or(1, |w| w.inner_size().width as usize).max(1);
        let win_w = full_w.saturating_sub(app.win.chat.dock_w(full_w)).max(1);
        let ratio = ((win_w.saturating_sub(x)) as f32 / win_w as f32).clamp(MIN_RATIO, MAX_RATIO);
        if (ratio - app.win.browser.ratio).abs() * win_w as f32 >= 1.0 {
            app.win.browser.ratio = ratio;
            relayout(app);
        }
        return true;
    }
    if app.win.browser.field_drag {
        if let Some(l) = app.browser_layout() {
            let cw = app.win.renderer.cell_width().max(1);
            let (tx, _) = l.field_text_origin(cw);
            let col = (x.saturating_sub(tx) + cw / 2) / cw;
            let pos = app.win.browser.field.pos_from_col(col);
            app.win.browser.field.set_cursor(pos, true);
            app.request_redraw();
        }
        return true;
    }

    let Some(l) = app.browser_layout() else { return false };
    let hit = l.hit(x, y).filter(|_| y >= app.tab_bar_height());

    let hover = hit.filter(|h| matches!(h, Hit::Back | Hit::Forward | Hit::Reload | Hit::Maximize | Hit::Close));
    let divider = hit == Some(Hit::Divider);
    if hover != app.win.browser.hover || divider != app.win.browser.divider_hover {
        app.win.browser.hover = hover;
        app.win.browser.divider_hover = divider;
        app.request_redraw();
    }
    let icon = match hit {
        None => return false,
        Some(Hit::Divider) => CursorIcon::ColResize,
        Some(Hit::Field) => CursorIcon::Text,
        Some(Hit::Toolbar) => CursorIcon::Default,
        Some(_) => CursorIcon::Pointer,
    };
    if let Some(w) = &app.win.window {
        w.set_cursor(icon);
    }
    true
}

/// Left button press. Returns true when the click was handled by the browser.
pub fn on_mouse_press(app: &mut App) -> bool {
    let Some(l) = app.browser_layout() else { return false };
    let (x, y) = (app.win.cursor_x, app.win.cursor_y);

    let hit = if y < app.tab_bar_height() { None } else { l.hit(x, y) };
    match hit {
        Some(Hit::Divider) => {
            app.win.browser.divider_drag = true;
            app.request_redraw();
            true
        }
        Some(Hit::Field) => {
            field_click(app, &l, x, y);
            true
        }
        Some(Hit::Toolbar) => {
            drop_edit(app);
            focus_page(app);
            app.request_redraw();
            true
        }
        Some(h) => {
            drop_edit(app);
            toolbar_click(app, h);
            if app.win.webview.is_some() && h != Hit::Close {
                focus_page(app);
            }
            app.request_redraw();
            true
        }
        None => {
            // Click on the terminal or tab bar: the terminal takes the keyboard back.
            drop_edit(app);
            let was_focused = app.win.browser.focused;
            if let Some(wv) = &app.win.webview {
                wv.focus_parent();
            }
            if was_focused {
                focus_terminal(app);
                app.request_redraw();
            }
            false
        }
    }
}

pub fn on_mouse_release(app: &mut App) -> bool {
    if app.win.browser.divider_drag {
        app.win.browser.divider_drag = false;
        app.request_redraw();
        return true;
    }
    if app.win.browser.field_drag {
        app.win.browser.field_drag = false;
        return true;
    }
    false
}

/// Abandon address editing without navigating or moving focus.
fn drop_edit(app: &mut App) {
    if app.win.browser.editing {
        app.win.browser.editing = false;
        app.win.browser.field_drag = false;
        if let Some(wv) = &app.win.webview {
            app.win.browser.field.set_text(&wv.url);
        }
    }
}

fn field_click(app: &mut App, l: &super::chrome::BrowserLayout, x: usize, y: usize) {
    let now = Instant::now();
    let double = app
        .win
        .browser
        .last_click
        .is_some_and(|(t, px, py)| now.duration_since(t) < DOUBLE_CLICK && px.abs_diff(x) <= 4 && py.abs_diff(y) <= 4);

    if !app.win.browser.editing {
        // First click focuses the field with everything selected (browser convention).
        begin_edit(app, true);
        app.win.browser.last_click = Some((now, x, y));
        return;
    }
    if double {
        app.win.browser.field.select_all();
        app.win.browser.last_click = None;
    } else {
        let cw = app.win.renderer.cell_width().max(1);
        let (tx, _) = l.field_text_origin(cw);
        let col = (x.saturating_sub(tx) + cw / 2) / cw;
        let pos = app.win.browser.field.pos_from_col(col);
        app.win.browser.field.set_cursor(pos, false);
        app.win.browser.field_drag = true;
        app.win.browser.last_click = Some((now, x, y));
    }
    app.request_redraw();
}
