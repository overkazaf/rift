//! Mouse behaviour that goes beyond plain PTY forwarding: wheel / trackpad
//! scrolling, scrollbar interaction, word / line / block selection with
//! auto-scroll, and the right-click context menu.
//!
//! `window_event` calls into the small set of entry points here; all state
//! lives in [`MouseUi`] (`App::mui`) so `App` itself stays untouched.

use std::time::{Duration, Instant};

use winit::event::{KeyEvent, MouseScrollDelta};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};

use super::{panes, tabs, App};
use crate::terminal::{MouseEncoding, MouseMode};
use crate::ui::context_menu::{self, ContextMenu, MenuCmd};
use crate::ui::scrollbar::{self, Drag, ScrollbarState, TrackHit};
use crate::ui::tabbar::ChromeUi;
use crate::window::selection::{self, SelMode};
use crate::window::tab::PaneCmd;
use crate::window::PaneRect;

/// Max gap between clicks of a double / triple click.
pub const MULTI_CLICK_MS: u128 = 400;
/// Lines per wheel notch (`LineDelta`); trackpads use pixel deltas instead.
const LINES_PER_NOTCH: f32 = 3.0;
/// Auto-scroll cadence while drag-selecting outside the pane.
const AUTOSCROLL: Duration = Duration::from_millis(50);
const WEB_SEARCH_URL: &str = "https://www.google.com/search?q=";
/// Longest text forwarded to a web search / the AI prompt.
const MAX_QUERY_CHARS: usize = 300;

#[derive(Default)]
pub struct MouseUi {
    pub scrollbar: ScrollbarState,
    /// Sub-line pixel remainder of trackpad scrolling.
    scroll_accum: f64,
    /// Last left click: (time, x, y, count).
    last_click: Option<(Instant, usize, usize, u8)>,
    pub menu: ContextMenu,
    pub tabs: tabs::TabUi,
    last_autoscroll: Option<Instant>,
}

impl MouseUi {
    /// Chrome state for the renderer this frame.
    pub fn chrome(&self, now: Instant) -> ChromeUi {
        ChromeUi {
            tab_hover: self.tabs.hover,
            dragging_tab: self.tabs.dragging(),
            scrollbar: self.scrollbar.bar(now),
            mcp_clients: 0, // set by lifecycle::redraw from the MCP server
        }
    }
}

// ── Pure helpers (unit-tested) ──────────────────────────────────────────

/// Click count (1..=3) for a press given the previous click.
pub fn next_click_count(prev_count: u8, elapsed_ms: u128, dist_px: usize, max_dist: usize) -> u8 {
    if prev_count == 0 || elapsed_ms > MULTI_CLICK_MS || dist_px > max_dist || prev_count >= 3 {
        1
    } else {
        prev_count + 1
    }
}

/// Whole lines for a discrete wheel notch (at least one line for any motion).
pub fn notch_lines(y: f32) -> i32 {
    let l = (y * LINES_PER_NOTCH).round() as i32;
    if l == 0 && y != 0.0 { y.signum() as i32 } else { l }
}

/// Accumulate pixel scrolling; returns the whole lines to scroll now and keeps
/// the sub-line remainder in `accum`. A direction change drops the remainder.
pub fn pixel_lines(accum: &mut f64, dy: f64, cell_h: f64) -> i32 {
    if dy != 0.0 && *accum != 0.0 && accum.signum() != dy.signum() {
        *accum = 0.0;
    }
    *accum += dy;
    let lines = (*accum / cell_h.max(1.0)).trunc();
    *accum -= lines * cell_h.max(1.0);
    lines as i32
}

/// Mouse-wheel report for mouse-tracking apps (button 64 up / 65 down).
pub fn wheel_report(enc: MouseEncoding, up: bool, row: usize, col: usize) -> Vec<u8> {
    let b: u8 = if up { 64 } else { 65 };
    match enc {
        MouseEncoding::Sgr => format!("\x1b[<{b};{};{}M", col + 1, row + 1).into_bytes(),
        MouseEncoding::Default => {
            let c = |v: usize| (32 + v + 1).min(255) as u8;
            vec![0x1b, b'[', b'M', 32 + b, c(col), c(row)]
        }
    }
}

// ── Geometry helpers ────────────────────────────────────────────────────

fn win_size(app: &App) -> (usize, usize) {
    app.window
        .as_ref()
        .map_or((800, 600), |w| {
            let s = w.inner_size();
            (s.width as usize, s.height as usize)
        })
}

fn active_rect(app: &App) -> PaneRect {
    let area = app.content_area();
    app.wm
        .pane_layouts(area)
        .into_iter()
        .find(|(_, _, active)| *active)
        .map(|(_, r, _)| r)
        .unwrap_or(area)
}

/// Window pixel -> (absolute row, col) in the active pane, clamped inside it.
pub fn pixel_to_abs(app: &App, px: usize, py: usize) -> (usize, usize) {
    let (row, col) = app.pixel_to_cell(px, py);
    let t = &app.wm.active_pane().terminal;
    let view = crate::blocks_ui::view::pane_view(t);
    let row = row.min(view.len().saturating_sub(1));
    let abs = view.get(row).map(|r| r.abs()).unwrap_or_else(|| t.view_top_abs() + row);
    (abs, col.min(t.cols.saturating_sub(1)))
}

/// URL under the mouse cursor in the active pane's *visible* rows.
pub fn url_at_cursor(app: &App) -> Option<String> {
    let (row, col) = app.pixel_to_cell(app.cursor_x, app.cursor_y);
    let t = &app.wm.active_pane().terminal;
    let folded = crate::blocks_ui::view::folded_view(t);
    let rows = match &folded {
        Some(fv) => fv.cell_rows(t),
        None => t.visible_rows(),
    };
    let cells = rows.get(row)?;
    // OSC 8 hyperlink under the pointer wins over text-based URL detection.
    if let Some(mut cell) = cells.get(col) {
        if cell.c == '\0' && col > 0 {
            cell = &cells[col - 1];
        }
        if let Some(uri) = t.hyperlink_uri(cell.link()) {
            let scheme_ok = ["http://", "https://", "mailto:", "file://", "ftp://"]
                .iter()
                .any(|p| uri.as_bytes().len() >= p.len() && uri.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes()));
            if scheme_ok {
                return Some(uri.to_string());
            }
        }
    }
    // '\0' (wide-glyph continuation) is kept: URL detection treats it as part of the URL.
    let line: String = cells.iter().map(|c| c.c).collect();
    crate::tools::url_detect::url_at_col(&line, col)
}

// ── Clipboard / selection actions ───────────────────────────────────────

pub fn selection_text(app: &App) -> String {
    let t = &app.wm.active_pane().terminal;
    app.selection.extract_text(|r| t.abs_line(r))
}

pub fn copy_selection(app: &mut App) -> bool {
    if !app.selection.active {
        return false;
    }
    let text = selection_text(app);
    if text.is_empty() {
        return false;
    }
    selection::copy_to_clipboard(&text);
    true
}

pub fn paste_clipboard(app: &mut App) {
    if let Some(text) = selection::paste_from_clipboard() {
        paste_text(app, &text);
    }
}

/// User paste (clipboard, middle click): sanitise, and when the shell is not
/// in bracketed-paste mode ask before pasting anything with a newline, since
/// every line would execute immediately.
pub fn paste_text(app: &mut App, text: &str) {
    let bracketed = app.wm.active_pane().terminal.bracketed_paste;
    let clean = selection::sanitize_paste(text, bracketed);
    if clean.is_empty() {
        return;
    }
    if !bracketed && clean.contains(['\n', '\r']) {
        crate::ui::confirm::show_paste_confirm(app, clean, bracketed);
        app.request_redraw();
        return;
    }
    write_paste(app, &clean, bracketed);
}

/// Write already-sanitised paste text to the active pane.
pub fn write_paste(app: &mut App, clean: &str, bracketed: bool) {
    let pane = app.wm.active_pane_mut();
    pane.terminal.scroll_to_bottom();
    if bracketed {
        pane.write(b"\x1b[200~");
        pane.write(clean.as_bytes());
        pane.write(b"\x1b[201~");
    } else {
        pane.write(clean.as_bytes());
    }
    app.request_redraw();
}

/// Select the whole buffer, scrollback included.
pub fn select_all(app: &mut App) {
    let t = &app.wm.active_pane().terminal;
    let (rows, cols) = (t.scrollback.len() + t.grid.len(), t.cols);
    app.selection.select_all(rows, cols);
    app.request_redraw();
}

/// Erase scrollback and screen (Cmd+Alt+K / menu). The shell is asked to
/// redraw its prompt with Ctrl+L unless a full-screen app owns the terminal.
pub fn clear_buffer(app: &mut App) {
    app.selection.clear();
    let pane = app.wm.active_pane_mut();
    let shell_prompt = !pane.terminal.is_alt_screen() && pane.terminal.mouse_mode == MouseMode::None;
    pane.terminal.clear_buffer();
    if shell_prompt {
        pane.write(b"\x0c");
    }
    app.request_redraw();
}

/// Entry point for "Ask AI about Selection". P2 replaces this function.
fn ask_ai_about(app: &mut App, text: &str) {
    use crate::ai::hub::{AskRequest, ContextItem, Intent};
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let label: String = one_line.chars().take(MAX_QUERY_CHARS).collect();
    let req = AskRequest::new("", Intent::Explain)
        .with(ContextItem::Selection(text.to_string()))
        .display(format!("Explain: {label}"));
    crate::ai::hub::ask(app, req);
}

// ── Wheel / trackpad ────────────────────────────────────────────────────

pub fn handle_wheel(app: &mut App, delta: MouseScrollDelta) {
    if app.mui.menu.visible {
        return;
    }
    let lines = match delta {
        MouseScrollDelta::LineDelta(_, y) => {
            app.mui.scroll_accum = 0.0;
            notch_lines(y)
        }
        MouseScrollDelta::PixelDelta(pos) => {
            let ch = app.renderer.cell_height().max(1) as f64;
            pixel_lines(&mut app.mui.scroll_accum, pos.y, ch)
        }
    };
    if lines == 0 {
        return;
    }
    let up = lines > 0;
    let n = lines.unsigned_abs() as usize;

    let (mode, enc, alt, alt_scroll, app_cursor) = {
        let t = &app.wm.active_pane().terminal;
        (t.mouse_mode, t.mouse_encoding, t.is_alt_screen(), t.alt_scroll, t.app_cursor_keys)
    };

    // 1. Mouse-tracking apps get wheel events (Shift bypasses to the terminal).
    if mode != MouseMode::None && !app.modifiers.shift_key() {
        let (row, col) = app.pixel_to_cell(app.cursor_x, app.cursor_y);
        let seq = wheel_report(enc, up, row, col);
        let pane = app.wm.active_pane_mut();
        for _ in 0..n.min(20) {
            pane.write(&seq);
        }
        return;
    }
    // 2. Alternate screen (less, vim, ...): the wheel becomes arrow keys.
    if alt {
        if alt_scroll {
            let key: &[u8] = match (up, app_cursor) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            let pane = app.wm.active_pane_mut();
            for _ in 0..n.min(30) {
                pane.write(key);
            }
        }
        return;
    }
    // 3. Normal screen: scroll the view through scrollback.
    let idx = app.wm.active_tab().active;
    let term = &mut app.wm.active_pane_mut().terminal;
    if up {
        term.scroll_view_up(n);
    } else {
        term.scroll_view_down(n);
    }
    app.mui.scrollbar.note_activity(idx, Instant::now());
    app.request_redraw();
}

// ── Scrollbar ───────────────────────────────────────────────────────────

/// (pane index, pane rect, rows, scrollback len, offset) of panes that
/// currently have a scrollbar.
fn scrollable_panes(app: &App) -> Vec<(usize, PaneRect, usize, usize, usize)> {
    let area = app.content_area();
    app.wm
        .pane_layouts(area)
        .into_iter()
        .filter_map(|(idx, rect, _)| {
            let t = &app.wm.active_tab().pane(idx)?.terminal;
            (!t.is_alt_screen() && t.scrollback_len() > 0)
                .then(|| (idx, rect, t.rows, t.scrollback_len(), t.scroll_offset))
        })
        .collect()
}

fn scrollbar_press(app: &mut App) -> bool {
    let (x, y) = (app.cursor_x, app.cursor_y);
    let cw = app.renderer.cell_width();
    let hit = scrollable_panes(app).into_iter().find(|(_, rect, ..)| {
        scrollbar::contains(scrollbar::hit_rect(*rect, cw), x, y)
    });
    let Some((idx, rect, rows, sb, off)) = hit else { return false };

    if idx != app.wm.active_tab().active {
        panes::focus_pane_idx(app, idx);
    }
    let now = Instant::now();
    match scrollbar::hit_track(rect.height, rows, sb, off, y - rect.y) {
        Some(TrackHit::Thumb { grab }) => {
            app.mui.scrollbar.drag = Some(Drag { pane: idx, grab });
        }
        Some(TrackHit::PageUp) => {
            if let Some(p) = app.wm.active_tab_mut().pane_mut(idx) {
                p.terminal.scroll_view_up(rows.saturating_sub(1).max(1));
            }
        }
        Some(TrackHit::PageDown) => {
            if let Some(p) = app.wm.active_tab_mut().pane_mut(idx) {
                p.terminal.scroll_view_down(rows.saturating_sub(1).max(1));
            }
        }
        None => return false,
    }
    app.mui.scrollbar.note_activity(idx, now);
    app.request_redraw();
    true
}

fn scrollbar_drag(app: &mut App) {
    let Some(d) = app.mui.scrollbar.drag else { return };
    let area = app.content_area();
    let Some((_, rect, _)) = app.wm.pane_layouts(area).into_iter().find(|(i, _, _)| *i == d.pane) else {
        app.mui.scrollbar.drag = None;
        return;
    };
    let y = app.cursor_y;
    if let Some(p) = app.wm.active_tab_mut().pane_mut(d.pane) {
        let t = &mut p.terminal;
        let top = y.saturating_sub(rect.y + d.grab);
        t.scroll_offset = scrollbar::offset_for_thumb_top(rect.height, t.rows, t.scrollback_len(), top);
    }
    app.mui.scrollbar.note_activity(d.pane, Instant::now());
    app.request_redraw();
}

fn scrollbar_hover(app: &mut App) {
    let (x, y) = (app.cursor_x, app.cursor_y);
    let cw = app.renderer.cell_width();
    let hover = if y < app.tab_bar_height() {
        None
    } else {
        scrollable_panes(app)
            .into_iter()
            .find(|(_, rect, ..)| scrollbar::contains(scrollbar::hit_rect(*rect, cw), x, y))
            .map(|(idx, ..)| idx)
    };
    if hover != app.mui.scrollbar.hover {
        app.mui.scrollbar.hover = hover;
        app.request_redraw();
    }
}

// ── Selection ───────────────────────────────────────────────────────────

/// Left press in terminal content that is not forwarded to the app.
pub fn begin_selection(app: &mut App) {
    let (x, y) = (app.cursor_x, app.cursor_y);
    let (row, col) = pixel_to_abs(app, x, y);
    let shift = app.modifiers.shift_key();
    let alt = app.modifiers.alt_key();

    // Shift+click extends the existing selection (or the last click point).
    if shift && (app.selection.active || app.mui.last_click.is_some()) {
        app.selection.extend_from_click(row, col);
        app.request_redraw();
        return;
    }

    let now = Instant::now();
    let max_dist = app.renderer.cell_width().max(4);
    let count = match app.mui.last_click {
        Some((t, lx, ly, c)) => next_click_count(
            c,
            now.duration_since(t).as_millis(),
            x.abs_diff(lx).max(y.abs_diff(ly)),
            max_dist,
        ),
        None => 1,
    };
    app.mui.last_click = Some((now, x, y, count));

    match count {
        2 => {
            let range = selection::word_at(&app.wm.active_pane().terminal, row, col);
            app.selection.start_range(SelMode::Word, range);
        }
        3 => {
            let range = selection::line_at(&app.wm.active_pane().terminal, row);
            app.selection.start_range(SelMode::Line, range);
        }
        _ => {
            app.selection.clear();
            if alt {
                app.selection.start_block(row, col);
            } else {
                app.selection.start_at(row, col);
            }
        }
    }
    app.request_redraw();
}

/// Extend the selection to the pointer (all modes).
pub fn drag_selection(app: &mut App) {
    let (row, col) = pixel_to_abs(app, app.cursor_x, app.cursor_y);
    match app.selection.mode {
        SelMode::Char | SelMode::Block => app.selection.extend_to(row, col),
        SelMode::Word => {
            let r = selection::word_at(&app.wm.active_pane().terminal, row, col);
            app.selection.extend_range(r);
        }
        SelMode::Line => {
            let r = selection::line_at(&app.wm.active_pane().terminal, row);
            app.selection.extend_range(r);
        }
    }
    app.request_redraw();
}

/// Mouse released after a selection drag: finalize and copy.
pub fn finish_selection(app: &mut App) {
    app.selection.finish();
    copy_selection(app);
    app.request_redraw();
}

/// While drag-selecting above / below the pane, scroll the view and keep
/// extending. Returns when to call again.
fn autoscroll(app: &mut App, now: Instant) -> Option<Instant> {
    if !(app.mouse_pressed && app.selection.dragging) || app.dragging_border.is_some() {
        app.mui.last_autoscroll = None;
        return None;
    }
    let rect = active_rect(app);
    let ch = app.renderer.cell_height().max(1);
    let y = app.cursor_y;
    let (up, dist) = if y < rect.y {
        (true, rect.y - y)
    } else if y >= rect.y + rect.height {
        (false, y - (rect.y + rect.height) + 1)
    } else {
        app.mui.last_autoscroll = None;
        return None;
    };
    let due = app.mui.last_autoscroll.map_or(true, |t| now.duration_since(t) >= AUTOSCROLL);
    if due {
        let lines = (1 + dist / (ch * 2)).min(8);
        let t = &mut app.wm.active_pane_mut().terminal;
        if up {
            t.scroll_view_up(lines);
        } else {
            t.scroll_view_down(lines);
        }
        app.mui.last_autoscroll = Some(now);
        drag_selection(app);
    }
    Some(app.mui.last_autoscroll.unwrap_or(now) + AUTOSCROLL)
}

// ── Context menu ────────────────────────────────────────────────────────

pub fn open_context_menu(app: &mut App) {
    let (x, y) = (app.cursor_x, app.cursor_y);
    if y < app.tab_bar_height() {
        return;
    }
    let area = app.content_area();
    if let Some(idx) = app.wm.active_tab().pane_at(area, x, y) {
        if idx != app.wm.active_tab().active {
            panes::focus_pane_idx(app, idx);
        }
    }
    let link = url_at_cursor(app);
    let items = context_menu::build_items(
        app.selection.active,
        link.as_deref(),
        app.wm.active_tab().is_zoomed(),
    );
    app.mui.menu.open(x, y, items);
    app.request_redraw();
}

/// Close the menu on a non-left press; true when it was open (click eaten).
pub fn dismiss_menu(app: &mut App) -> bool {
    if app.mui.menu.visible {
        app.mui.menu.close();
        app.request_redraw();
        true
    } else {
        false
    }
}

fn menu_press(app: &mut App, event_loop: &ActiveEventLoop) {
    let (cw, ch) = (app.renderer.cell_width(), app.renderer.cell_height());
    let (ww, wh) = win_size(app);
    let (x, y) = (app.cursor_x, app.cursor_y);
    let menu = &app.mui.menu;
    match menu.item_at(x, y, cw, ch, ww, wh) {
        Some(i) => {
            // Disabled rows swallow the click and keep the menu open.
            if let Some(cmd) = menu.command(i) {
                app.mui.menu.close();
                run_menu_cmd(app, cmd, event_loop);
            }
        }
        None => {
            if !menu.contains(x, y, cw, ch, ww, wh) {
                app.mui.menu.close();
            }
        }
    }
    app.request_redraw();
}

pub fn run_menu_cmd(app: &mut App, cmd: MenuCmd, event_loop: &ActiveEventLoop) {
    match cmd {
        MenuCmd::Copy => {
            copy_selection(app);
        }
        MenuCmd::Paste => paste_clipboard(app),
        MenuCmd::SelectAll => select_all(app),
        MenuCmd::ClearBuffer => clear_buffer(app),
        MenuCmd::SplitRight => panes::run_pane_cmd(app, PaneCmd::SplitRight, event_loop),
        MenuCmd::SplitDown => panes::run_pane_cmd(app, PaneCmd::SplitDown, event_loop),
        MenuCmd::ZoomPane => panes::run_pane_cmd(app, PaneCmd::Zoom, event_loop),
        MenuCmd::ClosePane => panes::run_pane_cmd(app, PaneCmd::ClosePane, event_loop),
        MenuCmd::OpenLink(url) => {
            // Same routing as Cmd+Click: Rift's browser when available.
            if cfg!(feature = "webview") {
                crate::network::browser::open(app, &url);
            } else {
                crate::tools::url_detect::open_url(&url);
            }
        }
        MenuCmd::CopyLink(url) => selection::copy_to_clipboard(&url),
        MenuCmd::SearchWeb => {
            let text = selection_text(app);
            let q: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let q: String = q.chars().take(MAX_QUERY_CHARS).collect();
            if !q.is_empty() {
                let url = format!("{WEB_SEARCH_URL}{}", crate::tools::codec::url_encode(&q));
                crate::tools::url_detect::open_url(&url);
            }
        }
        MenuCmd::AskAi => {
            let text = selection_text(app);
            if !text.is_empty() {
                ask_ai_about(app, &text);
            }
        }
    }
    app.request_redraw();
}

/// Key handling while the context menu is open (always consumes the key).
pub fn handle_menu_key(app: &mut App, event: &KeyEvent, event_loop: &ActiveEventLoop) -> bool {
    match &event.logical_key {
        Key::Named(NamedKey::Escape) => app.mui.menu.close(),
        Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::Tab) => app.mui.menu.move_sel(1),
        Key::Named(NamedKey::ArrowUp) => app.mui.menu.move_sel(-1),
        Key::Named(NamedKey::Home) => app.mui.menu.select_edge(false),
        Key::Named(NamedKey::End) => app.mui.menu.select_edge(true),
        Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => {
            if let Some(cmd) = app.mui.menu.sel.and_then(|i| app.mui.menu.command(i)) {
                app.mui.menu.close();
                run_menu_cmd(app, cmd, event_loop);
            }
        }
        _ => {}
    }
    app.request_redraw();
    true
}

// ── Window-event entry points ───────────────────────────────────────────

/// CursorMoved hook. Returns true when the motion was consumed.
pub fn on_cursor_moved(app: &mut App) -> bool {
    if app.mui.menu.visible {
        let (cw, ch) = (app.renderer.cell_width(), app.renderer.cell_height());
        let (ww, wh) = win_size(app);
        let item = app.mui.menu.item_at(app.cursor_x, app.cursor_y, cw, ch, ww, wh);
        if app.mui.menu.hover(item) {
            app.request_redraw();
        }
        return true;
    }
    if app.mui.scrollbar.drag.is_some() {
        scrollbar_drag(app);
        return true;
    }
    if tabs::on_cursor_moved(app) {
        return true;
    }
    scrollbar_hover(app);
    false
}

/// Left press hook (after the browser hook). Returns true when consumed.
pub fn on_left_press(app: &mut App, event_loop: &ActiveEventLoop) -> bool {
    if app.mui.menu.visible {
        menu_press(app, event_loop);
        return true;
    }
    // A click anywhere else commits an in-progress tab rename.
    tabs::press_outside_editor(app);
    if app.cursor_y >= app.tab_bar_height() && scrollbar_press(app) {
        return true;
    }
    false
}

/// Left release hook. Returns true when it ended a scrollbar / tab drag.
pub fn on_left_release(app: &mut App) -> bool {
    let mut consumed = false;
    if app.mui.scrollbar.drag.take().is_some() {
        scrollbar_hover(app);
        consumed = true;
    }
    if tabs::end_drag(app) {
        consumed = true;
    }
    if consumed {
        app.request_redraw();
    }
    consumed
}

/// Periodic work from `about_to_wait`. Returns the earliest time it needs
/// to run again.
pub fn tick(app: &mut App) -> Option<Instant> {
    let now = Instant::now();
    let mut next: Option<Instant> = None;
    let mut merge = |t: Option<Instant>| {
        if let Some(t) = t {
            next = Some(next.map_or(t, |n| n.min(t)));
        }
    };

    tabs::sync_tab_events(app);

    let idx = app.wm.active_tab().active;
    let off = app.wm.active_pane().terminal.scroll_offset;
    if app.mui.scrollbar.observe(idx, off, now) {
        app.request_redraw();
    }
    let t = app.mui.scrollbar.tick(now);
    if t.redraw {
        app.request_redraw();
    }
    merge(t.next);
    merge(autoscroll(app, now));
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_counts_cycle_and_reset() {
        assert_eq!(next_click_count(0, 0, 0, 8), 1);
        assert_eq!(next_click_count(1, 100, 2, 8), 2);
        assert_eq!(next_click_count(2, 100, 2, 8), 3);
        assert_eq!(next_click_count(3, 100, 2, 8), 1, "fourth click starts over");
        assert_eq!(next_click_count(1, MULTI_CLICK_MS + 1, 0, 8), 1, "too slow");
        assert_eq!(next_click_count(1, 50, 30, 8), 1, "moved too far");
    }

    #[test]
    fn notch_scrolls_three_lines() {
        assert_eq!(notch_lines(1.0), 3);
        assert_eq!(notch_lines(-1.0), -3);
        assert_eq!(notch_lines(2.0), 6);
        // Tiny fractional notches still move one line.
        assert_eq!(notch_lines(0.1), 1);
        assert_eq!(notch_lines(-0.1), -1);
        assert_eq!(notch_lines(0.0), 0);
    }

    #[test]
    fn trackpad_accumulates_sub_line_pixels() {
        let mut acc = 0.0;
        let ch = 20.0;
        // Five 6px swipes = 30px: one line after the 4th, remainder carried.
        let lines: Vec<i32> = (0..5).map(|_| pixel_lines(&mut acc, 6.0, ch)).collect();
        assert_eq!(lines, vec![0, 0, 0, 1, 0]);
        assert!((acc - 10.0).abs() < 1e-9);
        // Big flick scrolls multiple lines at once.
        assert_eq!(pixel_lines(&mut 0.0, 65.0, ch), 3);
        assert_eq!(pixel_lines(&mut 0.0, -65.0, ch), -3);
    }

    #[test]
    fn trackpad_direction_change_drops_remainder() {
        let mut acc = 0.0;
        assert_eq!(pixel_lines(&mut acc, 15.0, 20.0), 0);
        // Reversing must not be cancelled by the stale +15 remainder.
        assert_eq!(pixel_lines(&mut acc, -15.0, 20.0), 0);
        assert!((acc + 15.0).abs() < 1e-9);
        assert_eq!(pixel_lines(&mut acc, -10.0, 20.0), -1);
    }

    #[test]
    fn wheel_reports_use_the_negotiated_encoding() {
        assert_eq!(wheel_report(MouseEncoding::Sgr, true, 4, 9), b"\x1b[<64;10;5M".to_vec());
        assert_eq!(wheel_report(MouseEncoding::Sgr, false, 0, 0), b"\x1b[<65;1;1M".to_vec());
        assert_eq!(
            wheel_report(MouseEncoding::Default, false, 4, 9),
            vec![0x1b, b'[', b'M', 32 + 65, 32 + 10, 32 + 5]
        );
        // X10 coordinates saturate instead of overflowing.
        let v = wheel_report(MouseEncoding::Default, true, 1000, 1000);
        assert_eq!((v[4], v[5]), (255, 255));
    }
}
