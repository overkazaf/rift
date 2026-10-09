//! Warp-style command blocks UI: gutter bars, separators, header chips,
//! failed-block tint, hover toolbar, collapsing, block selection and
//! keyboard navigation.
//!
//! # Design
//!
//! * Block data comes from `terminal.blocks` (OSC 133, per pane).
//! * **Painting** is a pure overlay pass on the output buffer
//!   ([`draw::draw`], called after `render_tabbed_with_cmd`). Nothing block
//!   related is stored in the renderer's persistent back buffer or row
//!   hashes, so damage tracking stays exact: every frame the overlay is
//!   recomputed from the current view + block state.
//! * **Collapsing** changes *which lines* a pane shows, so it lives in the
//!   view model ([`view::build_view`]): the renderer asks
//!   [`view::folded_view`] and, when a collapsed block is on screen, renders
//!   those rows (real lines + one placeholder row per fold) instead of
//!   `Terminal::visible_rows()`. Row hashes are content based, so damage
//!   tracking needs no change.
//! * Hit-testing and painting share the same geometry ([`view::pane_view`],
//!   [`view::spans_of`], [`view::toolbar_layout`]).
//! * Hooks into the app: [`on_mouse_move`], [`on_click`], [`on_key`],
//!   [`tick`] and [`draw::draw`].

pub mod draw;
pub mod view;

use std::time::{Duration, Instant};

use winit::event::KeyEvent;
use winit::keyboard::{Key, NamedKey};

use crate::app::App;
use crate::terminal::Terminal;
use crate::tools::blocks::CommandBlock;
use view::{Button, Span, ViewRow};

/// How long a toast stays on screen.
const TOAST_TTL: Duration = Duration::from_millis(1800);
/// Redraw cadence while a block is running (pulsing bar, live duration).
const ANIM_INTERVAL: Duration = Duration::from_millis(125);
/// Lines of output sent to the AI.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hover {
    pub tab: usize,
    pub pane: usize,
    pub block: usize,
    pub button: Option<Button>,
}

pub struct BlocksUi {
    pub hover: Option<Hover>,
    /// Selected block: (tab, pane, block index).
    pub selected: Option<(usize, usize, usize)>,
    toast: Option<(String, Instant)>,
    last_anim: Instant,
}

impl BlocksUi {
    pub fn new() -> Self {
        Self { hover: None, selected: None, toast: None, last_anim: Instant::now() }
    }

    pub fn show_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    pub fn active_toast(&self) -> Option<&str> {
        match &self.toast {
            Some((m, t)) if t.elapsed() < TOAST_TTL => Some(m),
            _ => None,
        }
    }
}

/// Block geometry of one pane for the current frame.
pub struct PaneGeom {
    pub view: Vec<ViewRow>,
    pub spans: Vec<Span>,
}

/// Geometry for a pane, or `None` if it has no OSC 133 blocks / is on the
/// alternate screen.
pub fn pane_geom(t: &Terminal) -> Option<PaneGeom> {
    if t.is_alt_screen() || !t.blocks.osc_seen() || t.blocks.block_count() == 0 {
        return None;
    }
    let view = view::pane_view(t);
    let lookup = |l: usize| t.blocks.block_at_line(l).map(|(i, _)| i);
    let spans = view::spans_of(&view, &lookup);
    Some(PaneGeom { view, spans })
}

fn modal_open(app: &App) -> bool {
    app.win.prefs.visible
        || app.win.welcome.visible
        || app.win.ssh_dialog.visible
        || app.win.compare_view.visible
        || app.win.command_palette.visible
        || app.win.search.visible
        || app.win.file_manager.visible
        || app.win.git_panel.visible
        || app.win.cicd.visible
        || app.win.heatmap.visible
        || app.win.docker.visible
        || app.win.network_monitor.visible
        || app.win.process_tree.visible
        || app.mcp.overlay.visible
        || app.win.agents_ui.policy_log.visible
        || app.win.system_info.visible
        || app.win.port_dashboard.visible
        || app.win.regex_playground.visible
        || app.win.history.visible
        || app.win.timewarp_browser.active
}

/// What is under the mouse.
struct Hit {
    tab: usize,
    pane: usize,
    span: Span,
    in_view_folded: bool,
    button: Option<Button>,
    in_gutter: bool,
}

fn hit_test(app: &App) -> Option<Hit> {
    if modal_open(app) {
        return None;
    }
    let (px, py) = (app.win.cursor_x, app.win.cursor_y);
    let area = app.content_area();
    let tab = app.win.wm.active_tab().pane_at(area, px, py)?;
    let rect = app.win.wm.pane_layouts(area).into_iter().find(|(i, _, _)| *i == tab).map(|(_, r, _)| r)?;
    if px < rect.x || px >= rect.x + rect.width {
        return None;
    }
    let pane = app.win.wm.active_tab().pane(tab)?;
    let t = &pane.terminal;
    if t.mouse_mode != crate::terminal::MouseMode::None && !app.win.modifiers.shift_key() {
        return None;
    }
    let geom = pane_geom(t)?;
    let cw = app.win.renderer.cell_width().max(1);
    let ch = app.win.renderer.cell_height().max(1);
    let (row, span) = view::span_at_y(&geom.spans, py, rect.y, ch, geom.view.len())?;
    let block = t.blocks.get(span.block)?;
    let collapsed = block.collapsed;
    let tb = view::toolbar_layout(rect.x, rect.width, rect.y + span.first * ch, cw, ch, block.running, collapsed);
    let button = tb.button_at(px, py);
    let gutter_w = (cw / 2).max(8);
    Some(Hit {
        tab: app.win.wm.active_tab,
        pane: tab,
        span,
        in_view_folded: matches!(geom.view[row], ViewRow::Folded { .. }),
        button,
        in_gutter: px < rect.x + gutter_w,
    })
}

/// Mouse moved. Updates hover state and the cursor icon. Returns true when the
/// pointer is over a block control (toolbar button / gutter / fold row).
pub fn on_mouse_move(app: &mut App) -> bool {
    let hit = hit_test(app);
    let new = hit.as_ref().map(|h| Hover { tab: h.tab, pane: h.pane, block: h.span.block, button: h.button });
    if new != app.win.blocks_ui.hover {
        app.win.blocks_ui.hover = new;
        app.request_redraw();
    }
    let interactive = hit.as_ref().map_or(false, |h| h.button.is_some() || h.in_gutter || h.in_view_folded);
    if interactive {
        if let Some(w) = &app.win.window {
            w.set_cursor(winit::window::CursorIcon::Pointer);
        }
    }
    interactive
}

/// Left mouse press. Returns true if the click was consumed.
pub fn on_click(app: &mut App) -> bool {
    let Some(hit) = hit_test(app) else {
        if app.win.blocks_ui.selected.take().is_some() {
            app.request_redraw();
        }
        return false;
    };
    let (tab, pane, block) = (hit.tab, hit.pane, hit.span.block);
    if let Some(btn) = hit.button {
        run_button(app, pane, block, btn);
        app.request_redraw();
        return true;
    }
    if hit.in_gutter {
        let key = (tab, pane, block);
        app.win.blocks_ui.selected = if app.win.blocks_ui.selected == Some(key) { None } else { Some(key) };
        app.win.selection.clear();
        app.request_redraw();
        return true;
    }
    if hit.in_view_folded {
        run_button(app, pane, block, Button::Collapse);
        app.request_redraw();
        return true;
    }
    if app.win.blocks_ui.selected.take().is_some() {
        app.request_redraw();
    }
    false
}

/// Keyboard shortcuts. Call before the generic Cmd+C handling.
pub fn on_key(app: &mut App, event: &KeyEvent) -> bool {
    let m = app.win.modifiers;
    if !m.super_key() || m.control_key() || m.alt_key() || modal_open(app) {
        return false;
    }
    match &event.logical_key {
        Key::Named(NamedKey::ArrowUp) if m.shift_key() => jump_block(app, -1),
        Key::Named(NamedKey::ArrowDown) if m.shift_key() => jump_block(app, 1),
        Key::Character(s) if s.eq_ignore_ascii_case("c") => {
            if m.shift_key() {
                match target_block(app) {
                    Some((pane, block)) => {
                        copy_output(app, pane, block);
                        true
                    }
                    None => false,
                }
            } else if let (Some((tab, pane, block)), false) = (app.win.blocks_ui.selected, app.win.selection.active) {
                if tab == app.win.wm.active_tab {
                    copy_output(app, pane, block);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Redraw scheduling: pulsing/live-duration frames while a block runs and
/// toast expiry. Lowers `wake_at` when it needs to be woken.
pub fn tick(app: &mut App, wake_at: &mut Instant) {
    let ui = &mut app.win.blocks_ui;
    if let Some((_, t)) = &ui.toast {
        let end = *t + TOAST_TTL;
        if Instant::now() >= end {
            ui.toast = None;
            app.request_redraw();
        } else {
            *wake_at = (*wake_at).min(end);
        }
    }
    let running = app.win.wm.active_tab().panes().iter().any(|p| {
        !p.terminal.is_alt_screen() && p.terminal.blocks.running_osc_elapsed_ms().is_some()
    });
    if running {
        let next = app.win.blocks_ui.last_anim + ANIM_INTERVAL;
        if Instant::now() >= next {
            app.win.blocks_ui.last_anim = Instant::now();
            app.request_redraw();
        }
        *wake_at = (*wake_at).min(app.win.blocks_ui.last_anim + ANIM_INTERVAL);
    }
}

// ── Navigation ──

fn jump_block(app: &mut App, dir: i32) -> bool {
    let t = &mut app.win.wm.active_pane_mut().terminal;
    if t.is_alt_screen() || !t.blocks.osc_seen() || t.blocks.blocks().is_empty() {
        return false;
    }
    let v = view::pane_view(t);
    let Some(top) = v.first().map(|r| r.abs()) else { return false };
    let blocks = t.blocks.blocks();
    let target = if dir < 0 {
        blocks.iter().rev().find(|b| b.command_line < top)
    } else {
        blocks.iter().find(|b| b.command_line > top)
    };
    match target {
        Some(b) => {
            let folds = view::folds_of(&t.blocks);
            let total = t.scrollback.len() + t.grid.len();
            let off = view::scroll_offset_for_top(total, t.rows.min(t.grid.len()), &folds, b.command_line);
            t.scroll_offset = off.min(t.scrollback.len());
        }
        None if dir > 0 => t.scroll_to_bottom(),
        None => {}
    }
    app.request_redraw();
    true
}

/// Block targeted by block-level shortcuts: selected, else hovered, else the
/// last finished block of the active pane. Returns (pane idx, block idx).
fn target_block(app: &App) -> Option<(usize, usize)> {
    let tab = app.win.wm.active_tab;
    if let Some((t, p, b)) = app.win.blocks_ui.selected {
        if t == tab {
            return Some((p, b));
        }
    }
    if let Some(h) = app.win.blocks_ui.hover {
        if h.tab == tab && app.win.cursor_y >= app.tab_bar_height() {
            return Some((h.pane, h.block));
        }
    }
    let p = app.win.wm.active_tab().active;
    let t = &app.win.wm.active_pane().terminal;
    t.blocks.blocks().len().checked_sub(1).map(|b| (p, b))
}

// ── Actions ──

/// Owned copy of the block fields the actions need.
struct BlockInfo {
    command: String,
    exit_code: Option<i32>,
    output_start: usize,
    output_end: usize,
    running: bool,
}

impl BlockInfo {
    fn of(b: &CommandBlock) -> Self {
        Self {
            command: b.command.clone(),
            exit_code: b.exit_code,
            output_start: b.output_start,
            output_end: b.output_end,
            running: b.running,
        }
    }
}

fn block_info(app: &App, pane: usize, block: usize) -> Option<(BlockInfo, String)> {
    let t = &app.win.wm.active_tab().pane(pane)?.terminal;
    let b = t.blocks.get(block)?;
    let info = BlockInfo::of(b);
    let out = output_text(t, info.output_start, info.output_end);
    Some((info, out))
}

fn run_button(app: &mut App, pane: usize, block: usize, btn: Button) {
    match btn {
        Button::CopyCmd => {
            let cmd = app.win.wm.active_tab().pane(pane)
                .and_then(|p| p.terminal.blocks.get(block).map(|b| b.command.clone()));
            match cmd {
                Some(c) if !c.is_empty() => {
                    crate::window::selection::copy_to_clipboard(&c);
                    app.win.blocks_ui.show_toast("Copied command");
                }
                _ => app.win.blocks_ui.show_toast("No command text"),
            }
        }
        Button::CopyOutput => copy_output(app, pane, block),
        Button::AskAi => ask_ai_about_block(app, pane, block),
        Button::Rerun => {
            let cmd = app.win.wm.active_tab().pane(pane).and_then(|p| {
                p.terminal.blocks.get(block).filter(|b| !b.running).map(|b| b.command.clone())
            });
            if let Some(cmd) = cmd.filter(|c| !c.is_empty()) {
                if let Some(p) = app.win.wm.active_tab_mut().pane_mut(pane) {
                    p.terminal.scroll_to_bottom();
                    p.write(format!("{cmd}\r").as_bytes());
                }
                app.win.blocks_ui.show_toast("Rerunning command");
            }
        }
        Button::Collapse => {
            if let Some(p) = app.win.wm.active_tab_mut().pane_mut(pane) {
                p.terminal.blocks.toggle_collapse(block);
            }
        }
    }
}

fn copy_output(app: &mut App, pane: usize, block: usize) {
    let Some((_, out)) = block_info(app, pane, block) else { return };
    if out.is_empty() {
        app.win.blocks_ui.show_toast("No output to copy");
        return;
    }
    let n = out.lines().count();
    crate::window::selection::copy_to_clipboard(&out);
    app.win.blocks_ui.show_toast(format!("Copied output \u{00B7} {n} line{}", if n == 1 { "" } else { "s" }));
    app.request_redraw();
}

/// Open the AI panel with a prompt about a block: its command, exit code and
/// the last ~80 lines of output, and submit it (same path as
/// `AiAction::Ask` in `app::overlays`). The single entry point P2's docked
/// chat will replace.
pub fn ask_ai_about_block(app: &mut App, pane: usize, block: usize) {
    use crate::ai::hub::{AskRequest, ContextItem, Intent};
    let Some((info, out)) = block_info(app, pane, block) else { return };
    let shown = format!(
        "{}{}",
        crate::ui::trunc(&info.command, 48),
        info.exit_code.map_or(String::new(), |c| format!(" (exit {c})")),
    );
    let failed = info.exit_code.is_some_and(|c| c != 0);
    let cwd = app.win.wm.active_tab().pane(pane).and_then(|p| p.terminal.cwd.clone());
    let req = AskRequest::new("", if failed { Intent::Fix } else { Intent::Explain })
        .with(ContextItem::Block {
            command: info.command.clone(),
            exit_code: info.exit_code,
            output: out,
            cwd,
            running: info.running,
        })
        .display(shown);
    crate::ai::hub::ask(app, req);
}

/// Text of absolute lines `first..=last` (soft-wrapped rows joined, trailing
/// blanks trimmed). Used for "copy output".
pub fn output_text(t: &Terminal, first: usize, last: usize) -> String {
    if last < first {
        return String::new();
    }
    let sb = t.scrollback.len();
    let total = sb + t.grid.len();
    let mut out = String::new();
    for line in first..=last.min(total.saturating_sub(1)) {
        let row = if line < sb { t.scrollback.get(line) } else { t.grid.get(line - sb) };
        let Some(row) = row else { break };
        let seg: String = crate::terminal::grid::cells_text(row);
        let wrapped = row.last().map_or(false, |c| c.wrap()) && line != last;
        if wrapped {
            out.push_str(&seg);
        } else {
            out.push_str(seg.trim_end());
            if line != last {
                out.push('\n');
            }
        }
    }
    out.trim_matches('\n').trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Cell;

    #[test]
    fn output_text_joins_wrapped_rows_and_trims() {
        let mut t = Terminal::new(4, 3);
        let put = |row: &mut Vec<Cell>, s: &str| {
            for (i, c) in s.chars().enumerate() {
                row[i].c = c;
            }
        };
        put(&mut t.grid[0], "abcd"); // full row that soft-wrapped into the next
        t.grid[0][3].set_wrap(true);
        put(&mut t.grid[1], "ef");
        let txt = output_text(&t, 0, 2);
        assert_eq!(txt, "abcdef");
        assert_eq!(output_text(&t, 1, 0), "");
    }
}
