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
            if !app.win.ime_preedit.is_empty() {
                app.win.ime_preedit.clear();
                app.request_redraw();
            }
        }
        Ime::Preedit(text, _cursor) => {
            if app.win.ime_preedit != text {
                app.win.ime_preedit = text;
                app.request_redraw();
            }
        }
        Ime::Commit(text) => {
            app.win.ime_preedit.clear();
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
    // A pending confirmation (consent / paste / host key) swallows all text;
    // so does a tutorial (nothing reaches the shell while it is open).
    if app.win.confirm.visible() || app.win.tutorial.visible() {
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
    // Workflow overlays: the wizard's text fields and the queue composer.
    if app.workflows.overlay_visible() {
        app.workflows.ui.insert_text(text);
        app.request_redraw();
        return;
    }
    // Modal overlays that sit above everything and take no free text: drop input.
    if app.win.exec_preview.visible
        || app.win.timewarp_browser.active
        || app.win.welcome.visible
        || app.win.prefs.visible
        || app.win.compare_view.visible
        || app.win.observer_summary.is_some()
    {
        return;
    }

    if app.win.browser.editing {
        app.win.browser.field.insert_str(text);
    } else if app.win.ssh_dialog.visible {
        for c in text.chars() {
            let _ = app.win.ssh_dialog.handle_key(SshDialogKey::Char(c));
        }
    } else if app.win.webview_dialog.visible {
        for c in text.chars() {
            let _ = app.win.webview_dialog.handle_key(WvDialogKey::Char(c));
        }
    } else if app.win.command_palette.visible {
        for c in text.chars() {
            let _ = app.win.command_palette.handle_key(PaletteKey::Char(c));
        }
    } else if app.win.search.visible {
        let mut update = false;
        for c in text.chars() {
            if let Some(SearchAction::UpdateSearch) = app.win.search.handle_key(SearchKey::Char(c)) {
                update = true;
            }
        }
        if update {
            let pane = app.win.wm.active_pane();
            app.win.search.search(&pane.terminal.scrollback, &pane.terminal.grid);
        }
    } else if app.win.regex_playground.visible {
        for c in text.chars() {
            app.win.regex_playground.handle_key(RegexKey::Char(c));
        }
    } else if app.win.chat.focused {
        crate::ai::chat::insert_text(app, text);
    } else if app.win.history.visible {
        for c in text.chars() {
            if let Some(action) = app.win.history.handle_key(HistoryKey::Char(c)) {
                match action {
                    HistoryAction::Execute(cmd) => {
                        app.win.wm.active_pane_mut().write(format!("{cmd}\n").as_bytes());
                    }
                    HistoryAction::Insert(cmd) => {
                        app.win.wm.active_pane_mut().write(cmd.as_bytes());
                    }
                }
            }
        }
    } else if other_panel_visible(app) {
        // Non-text tool panels (file manager, git, docker ...) swallow keys.
        return;
    } else if crate::agents::runtime::insert_text(app, text) {
        // The Mission Control dock's reply composer took it.
    } else {
        let bytes = text.as_bytes();
        if let Some(rec) = &mut app.win.recorder {
            rec.record_input(bytes);
        }
        if app.win.selection.active {
            app.win.selection.clear();
        }
        if app.win.broadcast {
            for pane in app.win.wm.active_tab_mut().panes_mut() {
                pane.write(bytes);
            }
        } else {
            app.win.wm.active_pane_mut().write(bytes);
        }
        if app.win.wm.process_all_output() {
            app.win.wm.flush_all_responses();
        }
    }
    app.request_redraw();
}

fn other_panel_visible(app: &App) -> bool {
    app.win.file_manager.visible
        || app.win.git_panel.visible
        || app.win.cicd.visible
        || app.win.heatmap.visible
        || app.win.docker.visible
        || app.win.network_monitor.visible
        || app.win.process_tree.visible
        || app.mcp.overlay.visible
        || app.win.agents_ui.policy_log.visible
        || app.review.ui.visible
        || app.workflows.overlay_visible()
        || app.win.system_info.visible
        || app.win.port_dashboard.visible
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
