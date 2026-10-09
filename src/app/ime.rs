//! IME (input method editor) support: preedit tracking, commit routing,
//! candidate-window positioning and inline preedit rendering.

use unicode_width::UnicodeWidthChar;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::Ime;

use crate::network::{SshDialogKey, WvDialogKey};
use crate::tools::command_palette::PaletteKey;
use crate::tools::history::{HistoryAction, HistoryKey};
use crate::tools::regex_playground::RegexKey;
use crate::tools::search::{SearchAction, SearchKey};
use crate::renderer::Renderer;
use crate::window::{PaneRect, WindowManager};
use winit::window::Window;

use super::App;

pub fn handle_ime(app: &mut App, ime: Ime) {
    match ime {
        Ime::Enabled | Ime::Disabled => {
            if !app.ime_preedit.is_empty() {
                app.ime_preedit.clear();
                app.request_redraw();
            }
        }
        Ime::Preedit(text, _cursor) => {
            if app.ime_preedit != text {
                app.ime_preedit = text;
                app.request_redraw();
            }
        }
        Ime::Commit(text) => {
            app.ime_preedit.clear();
            if text.is_empty() {
                app.request_redraw();
                return;
            }
            commit_text(app, &text);
        }
    }
}

/// Route committed IME text to the topmost text-accepting overlay, or the PTY.
fn commit_text(app: &mut App, text: &str) {
    // A pending confirmation (consent / paste / host key) swallows all text.
    if app.confirm.visible() {
        return;
    }
    // The inline tab-rename field takes IME commits first.
    if super::tabs::insert_text(app, text) {
        return;
    }
    // The Cmd+K ask popover takes IME commits next.
    if crate::ai::inline::insert_text(app, text) {
        return;
    }
    // Modal overlays that sit above everything and take no free text: drop input.
    if app.exec_preview.visible
        || app.timewarp_browser.active
        || app.welcome.visible
        || app.prefs.visible
        || app.compare_view.visible
        || app.observer_summary.is_some()
    {
        return;
    }

    if app.browser.editing {
        app.browser.field.insert_str(text);
    } else if app.ssh_dialog.visible {
        for c in text.chars() {
            let _ = app.ssh_dialog.handle_key(SshDialogKey::Char(c));
        }
    } else if app.webview_dialog.visible {
        for c in text.chars() {
            let _ = app.webview_dialog.handle_key(WvDialogKey::Char(c));
        }
    } else if app.command_palette.visible {
        for c in text.chars() {
            let _ = app.command_palette.handle_key(PaletteKey::Char(c));
        }
    } else if app.search.visible {
        let mut update = false;
        for c in text.chars() {
            if let Some(SearchAction::UpdateSearch) = app.search.handle_key(SearchKey::Char(c)) {
                update = true;
            }
        }
        if update {
            let pane = app.wm.active_pane();
            app.search.search(&pane.terminal.scrollback, &pane.terminal.grid);
        }
    } else if app.regex_playground.visible {
        for c in text.chars() {
            app.regex_playground.handle_key(RegexKey::Char(c));
        }
    } else if app.chat.focused {
        crate::ai::chat::insert_text(app, text);
    } else if app.history.visible {
        for c in text.chars() {
            if let Some(action) = app.history.handle_key(HistoryKey::Char(c)) {
                match action {
                    HistoryAction::Execute(cmd) => {
                        app.wm.active_pane_mut().write(format!("{cmd}\n").as_bytes());
                    }
                    HistoryAction::Insert(cmd) => {
                        app.wm.active_pane_mut().write(cmd.as_bytes());
                    }
                }
            }
        }
    } else if other_panel_visible(app) {
        // Non-text tool panels (file manager, git, docker ...) swallow keys.
        return;
    } else {
        let bytes = text.as_bytes();
        if let Some(rec) = &mut app.recorder {
            rec.record_input(bytes);
        }
        if app.selection.active {
            app.selection.clear();
        }
        if app.broadcast {
            for pane in app.wm.active_tab_mut().panes_mut() {
                pane.write(bytes);
            }
        } else {
            app.wm.active_pane_mut().write(bytes);
        }
        if app.wm.process_all_output() {
            app.wm.flush_all_responses();
        }
    }
    app.request_redraw();
}

fn other_panel_visible(app: &App) -> bool {
    app.file_manager.visible
        || app.git_panel.visible
        || app.cicd.visible
        || app.heatmap.visible
        || app.docker.visible
        || app.network_monitor.visible
        || app.process_tree.visible
        || app.mcp.overlay.visible
        || app.system_info.visible
        || app.port_dashboard.visible
}

/// Pixel rect (x, y, w, h) of the active pane's cursor cell.
fn cursor_cell_rect(
    wm: &WindowManager,
    renderer: &Renderer,
    content_area: PaneRect,
) -> (usize, usize, usize, usize) {
    let cw = renderer.cell_width();
    let ch = renderer.cell_height();
    let rect = wm
        .pane_layouts(content_area)
        .into_iter()
        .find(|(_, _, active)| *active)
        .map(|(_, r, _)| r)
        .unwrap_or(content_area);
    let term = &wm.active_pane().terminal;
    // Fold- and scroll-aware: find the cursor's absolute line in the view.
    let cursor_abs = term.scrollback.len() + term.cursor_row;
    let screen_row = crate::blocks_ui::view::view_abs_rows(term)
        .iter()
        .position(|r| *r == Some(cursor_abs))
        .unwrap_or(term.cursor_row);
    (rect.x + term.cursor_col * cw, rect.y + screen_row * ch, cw, ch)
}

/// Tell the OS where the cursor is so the candidate window follows it.
/// Only calls into winit when the area changed. Takes disjoint `App` fields so
/// it can be called while the softbuffer surface is mutably borrowed.
pub fn update_cursor_area(
    wm: &WindowManager,
    renderer: &Renderer,
    window: &Window,
    last_area: &mut Option<(i32, i32, u32, u32)>,
    content_area: PaneRect,
) {
    set_cursor_area(window, last_area, cursor_cell_rect(wm, renderer, content_area));
}

/// Like [`update_cursor_area`] for an explicit pixel rect (x, y, w, h), e.g.
/// the caret of an overlay text field.
pub fn set_cursor_area(
    window: &Window,
    last_area: &mut Option<(i32, i32, u32, u32)>,
    (x, y, w, h): (usize, usize, usize, usize),
) {
    let area = (x as i32, y as i32, w as u32, h as u32);
    if *last_area == Some(area) {
        return;
    }
    *last_area = Some(area);
    window.set_ime_cursor_area(
        PhysicalPosition::new(area.0, area.1),
        PhysicalSize::new(area.2.max(1), area.3.max(1)),
    );
}

/// Draw the in-progress composition inline at the terminal cursor.
pub fn render_preedit(
    wm: &WindowManager,
    renderer: &mut Renderer,
    preedit: &str,
    buffer: &mut [u32],
    buf_w: usize,
    content_area: PaneRect,
) {
    if preedit.is_empty() || buf_w == 0 {
        return;
    }
    let (cx, cy, cw, ch) = cursor_cell_rect(wm, renderer, content_area);
    let cells: usize = preedit.chars().map(|c| c.width().unwrap_or(1).max(1)).sum();
    let box_w = cells * cw;
    if box_w > buf_w {
        return;
    }
    // Keep the box inside the window horizontally.
    let x0 = cx.min(buf_w - box_w);
    let theme = &renderer.theme;
    let bg = crate::ui::pack_rgb(crate::ui::lighten(theme.bg, 24));
    let fg = theme.fg;
    let ul = crate::ui::pack_rgb(theme.cursor);

    crate::ui::fill_rect(buffer, buf_w, x0, cy, box_w, ch, bg);
    let mut col = 0usize;
    for c in preedit.chars() {
        let w = c.width().unwrap_or(1).max(1);
        let mut tmp = [0u8; 4];
        let s: &str = c.encode_utf8(&mut tmp);
        crate::ui::render_text(buffer, buf_w, &mut renderer.font, s, x0 + col * cw, cy, fg);
        col += w;
    }
    // 2px underline beneath the preedit text.
    let uy = (cy + ch).saturating_sub(2);
    crate::ui::fill_rect(buffer, buf_w, x0, uy, box_w, 2, ul);
}
