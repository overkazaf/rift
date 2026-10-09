use winit::event::KeyEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};

use crate::tools::cicd::CicdKey;
use crate::tools::command_palette::{
    parse_ssh_spec, shell_quote_path, PaletteAction, PaletteContext, PaletteKey, Preview, SshHostInfo,
};
use crate::tools::compare::CompareKey;
use crate::tools::docker_panel::{DockerKey, DockerAction};
use crate::tools::exec_preview::{ExecPreviewAction, ExecPreviewKey};
use crate::tools::file_manager::FileManagerKey;
use crate::tools::git_panel::GitPanelKey;
use crate::tools::heatmap::HeatmapKey;
use crate::tools::history::{HistoryAction, HistoryKey};
use crate::tools::network_monitor::NetMonKey;
use crate::tools::port_dashboard::{PortDashboardKey, PortAction};
use crate::tools::process_tree::ProcTreeKey;
use crate::tools::regex_playground::RegexKey;
use crate::tools::search::{SearchKey, SearchAction};
use crate::tools::system_info::SysInfoKey;
use crate::ui::{PrefsAction, PrefsKey};
use crate::network::{SshDialogKey, WvDialogKey};
use crate::ui::WelcomeKey;

use super::App;

/// Route key events to the topmost visible overlay.
/// Returns true if the event was consumed.
pub fn try_intercept(app: &mut App, event: &KeyEvent, event_loop: &ActiveEventLoop) -> bool {
    // Consent / paste / host-key confirmations beat everything.
    if app.confirm.visible() {
        return handle_confirm(app, event);
    }
    // Preview-Then-Accept danger confirmation — absolute highest priority.
    // Nothing should be able to bypass an unconfirmed dangerous command.
    if app.exec_preview.visible {
        return handle_exec_preview(app, event);
    }
    // TimeWarp browser has highest priority
    if app.timewarp_browser.active {
        return handle_timewarp(app, event);
    }
    // Right-click context menu and inline tab rename are modal for the keyboard.
    if app.mui.menu.visible {
        return super::mouse::handle_menu_key(app, event, event_loop);
    }
    if app.mui.tabs.editor.is_some() {
        return super::tabs::handle_key(app, event);
    }
    // WebView address bar editing
    if app.browser.editing {
        return crate::network::browser::handle_key(app, event);
    }
    // UI Gallery (visual QA page)
    if crate::ui::kit::gallery::visible() {
        if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
            crate::ui::kit::gallery::set_visible(false);
            app.request_redraw();
        }
        return true;
    }
    // Observer summary dismissal
    if app.observer_summary.is_some() {
        if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
            app.observer_summary = None;
            app.request_redraw();
        }
        return true; // consume all keys while summary visible
    }
    if app.welcome.visible {
        return handle_welcome(app, event);
    }
    if app.prefs.visible {
        return handle_prefs(app, event);
    }
    if app.ssh_dialog.visible {
        return handle_ssh(app, event);
    }
    if app.webview_dialog.visible {
        return handle_webview_dialog(app, event);
    }
    if app.compare_view.visible {
        return handle_compare(app, event);
    }
    // Command Palette (Cmd+P) — takes priority over the scrollback search overlay
    if app.command_palette.visible {
        return handle_command_palette(app, event, event_loop);
    }
    if app.search.visible {
        return handle_search(app, event);
    }
    if app.file_manager.visible {
        return handle_file_manager(app, event);
    }
    if app.git_panel.visible {
        return handle_git_panel(app, event);
    }
    if app.cicd.visible {
        return handle_cicd(app, event);
    }
    if app.heatmap.visible {
        return handle_heatmap(app, event);
    }
    if app.docker.visible {
        return handle_docker(app, event);
    }
    if app.network_monitor.visible {
        return handle_netmon(app, event);
    }
    if app.process_tree.visible {
        return handle_proctree(app, event);
    }
    if app.mcp.overlay.visible {
        return handle_mcp_activity(app, event);
    }
    if crate::review::visible(app) {
        return handle_review(app, event);
    }
    if app.system_info.visible {
        return handle_sysinfo(app, event);
    }
    if app.port_dashboard.visible {
        return handle_port_dashboard(app, event);
    }
    if app.regex_playground.visible {
        return handle_regex_playground(app, event);
    }
    // Docked AI chat: keys go to its composer while it has focus; chords it
    // doesn't use (Cmd+T, Cmd+P, ...) fall through to the global shortcuts.
    if app.agents_ui.focused && crate::agents::runtime::handle_key(app, event) {
        return true;
    }
    if app.chat.focused && crate::ai::chat::handle_key(app, event) {
        return true;
    }
    if app.history.visible {
        return handle_history(app, event);
    }
    if app.autocomplete.visible {
        return handle_autocomplete(app, event);
    }
    false
}

fn handle_confirm(app: &mut App, event: &KeyEvent) -> bool {
    use crate::ui::confirm::ConfirmKey;
    let k = match event.logical_key {
        Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::ArrowUp) => Some(ConfirmKey::Left),
        Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::Tab) => Some(ConfirmKey::Right),
        Key::Named(NamedKey::Enter) => Some(ConfirmKey::Enter),
        Key::Named(NamedKey::Escape) => Some(ConfirmKey::Escape),
        Key::Character(ref s) => s.chars().next().and_then(|c| c.to_digit(10)).map(|d| ConfirmKey::Digit(d as usize)),
        _ => None,
    };
    if let Some(k) = k {
        if let Some((req, choice)) = app.confirm.handle_key(k) {
            crate::ui::confirm::resolve(app, req, choice);
        }
        app.request_redraw();
    }
    true
}

/// Preview-Then-Accept modal: Enter/Y confirms (Critical commands additionally
/// require the user to have typed "yes" first — see `ExecPreview::handle_key`),
/// Esc/N cancels. Confirming sends only `\r` — the command's own characters
/// were already streamed to the PTY as the user typed them, so the shell's
/// line editor already has them buffered; resending the text would duplicate it.
fn handle_exec_preview(app: &mut App, event: &KeyEvent) -> bool {
    let ek = match event.logical_key {
        Key::Named(NamedKey::Enter) => Some(ExecPreviewKey::Enter),
        Key::Named(NamedKey::Escape) => Some(ExecPreviewKey::Escape),
        Key::Named(NamedKey::Backspace) => Some(ExecPreviewKey::Backspace),
        Key::Character(ref s) => s.chars().next().map(ExecPreviewKey::Char),
        _ => None,
    };
    if let Some(k) = ek {
        if let Some(action) = app.exec_preview.handle_key(k) {
            match action {
                ExecPreviewAction::Execute => {
                    app.wm.active_pane_mut().write(b"\r");
                    if app.wm.process_all_output() {
                        app.wm.flush_all_responses();
                    }
                }
                ExecPreviewAction::Cancel => {}
            }
        }
        app.request_redraw();
    }
    true
}

fn handle_welcome(app: &mut App, event: &KeyEvent) -> bool {
    let wk = match event.logical_key {
        Key::Named(NamedKey::ArrowLeft) => WelcomeKey::Left,
        Key::Named(NamedKey::ArrowRight) => WelcomeKey::Right,
        Key::Named(NamedKey::Enter) => WelcomeKey::Enter,
        Key::Named(NamedKey::Space) => WelcomeKey::Space,
        Key::Named(NamedKey::Escape) => WelcomeKey::Escape,
        Key::Character(ref s) if s.eq_ignore_ascii_case("q") => WelcomeKey::Q,
        _ => WelcomeKey::Right,
    };
    app.welcome.handle_key(wk);
    app.request_redraw();
    true
}

fn handle_prefs(app: &mut App, event: &KeyEvent) -> bool {
    let pk = match event.logical_key {
        Key::Named(NamedKey::ArrowUp) => Some(PrefsKey::Up),
        Key::Named(NamedKey::ArrowDown) => Some(PrefsKey::Down),
        Key::Named(NamedKey::ArrowLeft) => Some(PrefsKey::Left),
        Key::Named(NamedKey::ArrowRight) => Some(PrefsKey::Right),
        Key::Named(NamedKey::Enter) => Some(PrefsKey::Enter),
        Key::Named(NamedKey::Escape) => Some(PrefsKey::Escape),
        Key::Named(NamedKey::Tab) => Some(PrefsKey::Tab),
        Key::Character(ref s) if s.eq_ignore_ascii_case("s") => Some(PrefsKey::Save),
        _ => None,
    };

    if let Some(pk) = pk {
        if let Some(action) = app.prefs.handle_key(pk) {
            match action {
                PrefsAction::ThemeChanged(ref name) => {
                    if let Some(theme) = crate::config::Config::theme_by_name(name) {
                        app.config.theme_name = name.clone();
                        app.renderer.set_theme(theme);
                    }
                }
                PrefsAction::FontSizeChanged(size) => {
                    app.config.font_size = size;
                    if let Some(window) = &app.window {
                        let scale = window.scale_factor();
                        let physical = size * scale as f32;
                        let font_path = crate::config::resolve_font_path(&app.config);
                        app.renderer.reinit_font(&font_path, physical);
                        let ws = window.inner_size();
                        super::lifecycle::handle_resize(app, ws.width, ws.height);
                    }
                }
                PrefsAction::OpacityChanged(v) => {
                    app.renderer.opacity = v;
                    app.config.opacity = v;
                    #[cfg(target_os = "macos")]
                    if let Some(w) = &app.window {
                        crate::platform::macos::apply_transparency(w, v as f64);
                    }
                }
                PrefsAction::SaveConfig => {
                    crate::config::toml::save_config(&app.config);
                }
            }
        }
        app.request_redraw();
        return true;
    }

    // Ctrl+Shift+, can close prefs
    if app.modifiers.control_key() && app.modifiers.shift_key() {
        if let Key::Character(ref s) = event.logical_key {
            if s == "," || s == "<" {
                app.prefs.toggle();
                return true;
            }
        }
    }
    true
}

fn handle_ssh(app: &mut App, event: &KeyEvent) -> bool {
    let sdk = match event.logical_key {
        Key::Named(NamedKey::Escape) => Some(SshDialogKey::Escape),
        Key::Named(NamedKey::Tab) => Some(SshDialogKey::Tab),
        Key::Named(NamedKey::ArrowUp) => Some(SshDialogKey::Up),
        Key::Named(NamedKey::ArrowDown) => Some(SshDialogKey::Down),
        Key::Named(NamedKey::Backspace) => Some(SshDialogKey::Backspace),
        Key::Named(NamedKey::Delete) => Some(SshDialogKey::Delete),
        Key::Named(NamedKey::Enter) => Some(SshDialogKey::Enter),
        Key::Named(NamedKey::Space) => Some(SshDialogKey::Char(' ')),
        Key::Character(ref s) => s.chars().next().map(SshDialogKey::Char),
        _ => None,
    };
    if let Some(k) = sdk {
        if let Some(req) = app.ssh_dialog.handle_key(k) {
            log::info!("SSH dialog: connecting to {}@{}:{} (alias: {})", req.user, req.host, req.port, req.alias);
            super::shortcuts::do_ssh_connect(app, req);
        }
        app.request_redraw();
    }
    true
}

fn handle_webview_dialog(app: &mut App, event: &KeyEvent) -> bool {
    // Cmd+V paste, Cmd+C copy, Cmd+X cut, Cmd+A select all
    if app.modifiers.super_key() {
        if let Some(ref s) = addr_bar_key_char(event) {
            match s.as_str() {
                "v" => {
                    if let Some(text) = crate::window::selection::paste_from_clipboard() {
                        let clean = crate::window::selection::sanitize_paste(text.lines().next().unwrap_or(""), false);
                        for c in clean.chars() {
                            app.webview_dialog.handle_key(WvDialogKey::Char(c));
                        }
                    }
                }
                "c" => {
                    crate::window::selection::copy_to_clipboard(&app.webview_dialog.url);
                }
                "x" => {
                    crate::window::selection::copy_to_clipboard(&app.webview_dialog.url);
                    app.webview_dialog.url.clear();
                }
                "a" => {}
                _ => {}
            }
            app.request_redraw();
            return true;
        }
    }
    let wk = match event.logical_key {
        Key::Named(NamedKey::Escape) => Some(WvDialogKey::Escape),
        Key::Named(NamedKey::Backspace) => Some(WvDialogKey::Backspace),
        Key::Named(NamedKey::Enter) => Some(WvDialogKey::Enter),
        Key::Named(NamedKey::Space) => Some(WvDialogKey::Char(' ')),
        Key::Character(ref s) => s.chars().next().map(WvDialogKey::Char),
        _ => None,
    };
    if let Some(k) = wk {
        if let Some(url) = app.webview_dialog.handle_key(k) {
            super::shortcuts::open_webview(app, &url);
        }
        app.request_redraw();
    }
    true
}

fn handle_timewarp(app: &mut App, event: &KeyEvent) -> bool {
    let max = app.timewarp.snapshot_count();
    match event.logical_key {
        Key::Named(NamedKey::ArrowLeft) => {
            if app.modifiers.shift_key() {
                app.timewarp_browser.jump_back(10, max);
            } else {
                app.timewarp_browser.step_back(max);
            }
        }
        Key::Named(NamedKey::ArrowRight) => {
            if app.modifiers.shift_key() {
                app.timewarp_browser.jump_forward(10);
            } else {
                app.timewarp_browser.step_forward();
            }
        }
        Key::Named(NamedKey::Escape) => {
            app.timewarp_browser.exit();
        }
        // Ctrl+Shift+Z also exits
        _ => {
            if app.modifiers.control_key() && app.modifiers.shift_key() {
                if let Key::Character(ref s) = event.logical_key {
                    if s.eq_ignore_ascii_case("z") {
                        app.timewarp_browser.exit();
                    }
                }
            }
        }
    }
    app.request_redraw();
    true
}

fn handle_compare(app: &mut App, event: &KeyEvent) -> bool {
    let ck = match event.logical_key {
        Key::Named(NamedKey::Escape) => CompareKey::Escape,
        Key::Named(NamedKey::ArrowUp) => CompareKey::Up,
        Key::Named(NamedKey::ArrowDown) => CompareKey::Down,
        _ => return true, // only Esc closes; other keys are swallowed
    };
    app.compare_view.handle_key(ck);
    app.request_redraw();
    true
}

fn handle_search(app: &mut App, event: &KeyEvent) -> bool {
    let sk = match event.logical_key {
        Key::Named(NamedKey::Escape) => Some(SearchKey::Escape),
        Key::Named(NamedKey::Backspace) => Some(SearchKey::Backspace),
        Key::Named(NamedKey::Enter) => Some(SearchKey::Enter),
        Key::Named(NamedKey::Space) => Some(SearchKey::Space),
        Key::Character(ref s) => s.chars().next().map(SearchKey::Char),
        _ => None,
    };
    if let Some(k) = sk {
        if let Some(action) = app.search.handle_key(k) {
            match action {
                SearchAction::UpdateSearch => {
                    let pane = app.wm.active_pane();
                    app.search.search(&pane.terminal.scrollback, &pane.terminal.grid);
                }
                SearchAction::JumpToMatch(idx) => {
                    if let Some(m) = app.search.matches.get(idx) {
                        let row = m.row;
                        let _ = (m.col_start, m.col_end);
                        let pane = app.wm.active_pane_mut();
                        let sb_len = pane.terminal.scrollback_len();
                        if row < sb_len {
                            let offset = sb_len - row;
                            pane.terminal.scroll_offset = offset;
                        } else {
                            pane.terminal.scroll_to_bottom();
                        }
                    }
                }
            }
        }
        app.request_redraw();
    }
    true
}

fn handle_autocomplete(app: &mut App, event: &KeyEvent) -> bool {
    match event.logical_key {
        Key::Named(NamedKey::Tab) | Key::Named(NamedKey::ArrowDown) => {
            app.autocomplete.select_next();
            app.request_redraw();
            return true;
        }
        Key::Named(NamedKey::ArrowUp) => {
            app.autocomplete.select_prev();
            app.request_redraw();
            return true;
        }
        Key::Named(NamedKey::Enter) => {
            if let Some(text) = app.autocomplete.accept() {
                let terminal = &app.wm.active_pane().terminal;
                let row = terminal.cursor_row;
                let col = terminal.cursor_col;
                let line: String = crate::terminal::grid::cells_text(&terminal.grid[row][..col.min(terminal.grid[row].len())]);
                let current_word = line.split_whitespace().last().unwrap_or("");
                let mut out = vec![0x7fu8; current_word.len()];
                out.extend_from_slice(text.as_bytes());
                app.wm.active_pane_mut().write(&out);
            }
            app.request_redraw();
            return true;
        }
        Key::Named(NamedKey::Escape) => {
            app.autocomplete.dismiss();
            app.request_redraw();
            return true;
        }
        _ => {
            app.autocomplete.dismiss();
            return false; // fall through to normal handling
        }
    }
}

fn handle_history(app: &mut App, event: &KeyEvent) -> bool {
    let hk = match event.logical_key {
        Key::Named(NamedKey::Escape) => Some(HistoryKey::Escape),
        Key::Named(NamedKey::Backspace) => Some(HistoryKey::Backspace),
        Key::Named(NamedKey::Enter) => Some(HistoryKey::Enter),
        Key::Named(NamedKey::Tab) => Some(HistoryKey::Tab),
        Key::Named(NamedKey::ArrowUp) => Some(HistoryKey::Up),
        Key::Named(NamedKey::ArrowDown) => Some(HistoryKey::Down),
        Key::Named(NamedKey::Space) => Some(HistoryKey::Char(' ')),
        Key::Character(ref s) => s.chars().next().map(HistoryKey::Char),
        _ => None,
    };
    if let Some(k) = hk {
        if let Some(action) = app.history.handle_key(k) {
            match action {
                HistoryAction::Execute(cmd) => {
                    let cmd = crate::window::selection::sanitize_paste(&cmd, false);
                    app.wm.active_pane_mut().write(format!("{cmd}\n").as_bytes());
                }
                HistoryAction::Insert(cmd) => {
                    let cmd = crate::window::selection::sanitize_paste(&cmd, false);
                    app.wm.active_pane_mut().write(cmd.as_bytes());
                }
            }
        }
        app.request_redraw();
    }
    true
}

/// Open the command palette with fresh dynamic entries (tabs, saved SSH hosts).
pub fn open_command_palette(app: &mut App) {
    let ctx = PaletteContext {
        tabs: app.wm.tabs.iter().map(|t| t.display_title().to_string()).collect(),
        active_tab: app.wm.active_tab,
        ssh_hosts: app
            .ssh_dialog
            .saved_hosts()
            .into_iter()
            .map(|h| SshHostInfo { detail: format!("{}@{}:{}", h.user, h.host, h.port), alias: h.alias })
            .collect(),
        font_size: app.config.font_size,
        models: crate::ai::local::picker::current_options(app),
        agents: crate::agents::runtime::installed(app),
        in_git_repo: {
            let p = app.wm.active_pane();
            p.terminal.cwd.as_deref().is_some_and(|c| crate::agents::git::repo_info(std::path::Path::new(c)).is_some())
        },
    };
    app.command_palette.open(ctx);
}

/// Apply a pending theme live-preview (or rollback) requested by the palette.
/// The committed theme stays in `app.config`, so a rollback just re-applies it.
fn sync_palette_preview(app: &mut App) {
    match app.command_palette.take_preview() {
        Some(Preview::Theme(name)) => {
            if let Some(theme) = crate::config::Config::theme_by_name(&name) {
                app.renderer.set_theme(theme);
            }
        }
        Some(Preview::Restore) => app.renderer.set_theme(app.config.theme.clone()),
        None => {}
    }
}

fn handle_command_palette(app: &mut App, event: &KeyEvent, event_loop: &ActiveEventLoop) -> bool {
    let cmd = app.modifiers.super_key();
    let ctrl = app.modifiers.control_key();
    let alt = app.modifiers.alt_key();
    let mut paste: Option<String> = None;
    let pk = match event.logical_key {
        Key::Named(NamedKey::Escape) => Some(PaletteKey::Escape),
        Key::Named(NamedKey::Backspace) if cmd => Some(PaletteKey::DeleteToStart),
        Key::Named(NamedKey::Backspace) if alt => Some(PaletteKey::DeleteWord),
        Key::Named(NamedKey::Backspace) => Some(PaletteKey::Backspace),
        Key::Named(NamedKey::Delete) => Some(PaletteKey::Delete),
        Key::Named(NamedKey::Enter) if cmd => Some(PaletteKey::EnterKeepOpen),
        Key::Named(NamedKey::Enter) => Some(PaletteKey::Enter),
        Key::Named(NamedKey::Tab) => Some(PaletteKey::Tab),
        Key::Named(NamedKey::ArrowUp) => Some(PaletteKey::Up),
        Key::Named(NamedKey::ArrowDown) => Some(PaletteKey::Down),
        Key::Named(NamedKey::ArrowLeft) if cmd => Some(PaletteKey::Home),
        Key::Named(NamedKey::ArrowLeft) if alt => Some(PaletteKey::WordLeft),
        Key::Named(NamedKey::ArrowLeft) => Some(PaletteKey::Left),
        Key::Named(NamedKey::ArrowRight) if cmd => Some(PaletteKey::End),
        Key::Named(NamedKey::ArrowRight) if alt => Some(PaletteKey::WordRight),
        Key::Named(NamedKey::ArrowRight) => Some(PaletteKey::Right),
        Key::Named(NamedKey::Home) => Some(PaletteKey::Home),
        Key::Named(NamedKey::End) => Some(PaletteKey::End),
        Key::Named(NamedKey::PageUp) => Some(PaletteKey::PageUp),
        Key::Named(NamedKey::PageDown) => Some(PaletteKey::PageDown),
        Key::Named(NamedKey::Space) => Some(PaletteKey::Char(' ')),
        Key::Character(ref s) if cmd => match s.to_ascii_lowercase().as_str() {
            "p" => Some(PaletteKey::Escape), // Cmd+P again closes
            "a" => Some(PaletteKey::Home),
            "v" => {
                paste = crate::window::selection::paste_from_clipboard();
                None
            }
            _ => None,
        },
        Key::Character(ref s) if ctrl => match s.to_ascii_lowercase().as_str() {
            "n" | "j" => Some(PaletteKey::Down),
            "p" | "k" => Some(PaletteKey::Up),
            "a" => Some(PaletteKey::Home),
            "e" => Some(PaletteKey::End),
            "b" => Some(PaletteKey::Left),
            "f" => Some(PaletteKey::Right),
            "u" => Some(PaletteKey::DeleteToStart),
            "w" => Some(PaletteKey::DeleteWord),
            _ => None,
        },
        Key::Character(ref s) => s.chars().next().map(PaletteKey::Char),
        _ => None,
    };
    if let Some(text) = paste {
        // Single-line input: take the first line only, minus control chars.
        let first = crate::window::selection::sanitize_paste(text.lines().next().unwrap_or(""), false);
        for c in first.chars() {
            let _ = app.command_palette.handle_key(PaletteKey::Char(c));
        }
    }
    if let Some(k) = pk {
        if let Some(action) = app.command_palette.handle_key(k) {
            dispatch_palette_action(app, action, event_loop);
        }
    }
    sync_palette_preview(app);
    app.request_redraw();
    true
}

/// Mouse hover over the palette: highlight the row under the cursor.
pub fn palette_cursor_moved(app: &mut App) {
    let (x, y) = (app.cursor_x, app.cursor_y);
    if app.command_palette.mouse_move(x, y) {
        sync_palette_preview(app);
        app.request_redraw();
    }
}

/// Left click: run the row under the cursor (Cmd+click keeps the palette
/// open where sensible); clicking outside the panel closes it.
pub fn palette_click(app: &mut App, event_loop: &ActiveEventLoop) {
    let (x, y) = (app.cursor_x, app.cursor_y);
    let keep = app.modifiers.super_key();
    if let Some(action) = app.command_palette.mouse_click(x, y, keep) {
        dispatch_palette_action(app, action, event_loop);
    }
    sync_palette_preview(app);
    app.request_redraw();
}

/// Wheel over the palette scrolls its list; positive `lines` scrolls up.
pub fn palette_wheel(app: &mut App, lines: i32) {
    app.command_palette.scroll_lines(lines);
    app.request_redraw();
}

/// Commit a theme: update renderer and the persisted config together.
fn apply_theme(app: &mut App, name: &str) {
    if let Some(theme) = crate::config::Config::theme_by_name(name) {
        app.config.theme = theme.clone();
        app.config.theme_name = name.to_string();
        app.renderer.set_theme(theme);
    }
}

/// Map a palette selection onto the existing feature it represents. Menu
/// entries go through `shortcuts::handle_menu_action` instead of duplicating
/// its logic (pane sizing, resize-on-split, exit-on-last-tab-close, etc).
fn dispatch_palette_action(app: &mut App, action: PaletteAction, event_loop: &ActiveEventLoop) {
    match action {
        PaletteAction::Menu(m) => super::shortcuts::handle_menu_action(app, m, event_loop),
        PaletteAction::Theme(name) => apply_theme(app, &name),
        PaletteAction::FontSize(size) => {
            app.config.font_size = size.clamp(8.0, 32.0);
            super::shortcuts::reinit_font_from_config(app);
        }
        PaletteAction::OpenUrl(input) => crate::network::browser::open(app, &input),
        PaletteAction::Ssh(target) => {
            let saved = app.ssh_dialog.saved_hosts().into_iter().find(|h| h.alias == target);
            let req = saved.or_else(|| {
                let (user, host, port) = parse_ssh_spec(&target)?;
                let user = user
                    .or_else(|| std::env::var("USER").ok())
                    .unwrap_or_else(|| "root".to_string());
                Some(crate::network::SshConnectRequest { alias: String::new(), host, port, user })
            });
            match req {
                Some(req) => super::shortcuts::do_ssh_connect(app, req),
                None => log::warn!("palette: unknown ssh target '{target}'"),
            }
        }
        PaletteAction::Cd(path) => {
            let line = format!("cd {}\n", shell_quote_path(&path));
            app.wm.active_pane_mut().write(line.as_bytes());
        }
        PaletteAction::Shell(cmd) => {
            let cmd = crate::window::selection::sanitize_paste(&cmd, false);
            app.wm.active_pane_mut().write(format!("{cmd}\n").as_bytes());
        }
        PaletteAction::AskAi(question) => {
            // Open the chat sidebar and send the question right away.
            let req = crate::ai::hub::AskRequest::new(question, crate::ai::hub::Intent::Explain);
            crate::ai::hub::ask(app, req);
        }
        PaletteAction::SwitchTab(i) => {
            if i < app.wm.tab_count() {
                app.wm.switch_tab(i);
                super::shortcuts::sync_webview_for_tab(app);
                app.update_title();
            }
        }
        PaletteAction::NextTab => {
            app.wm.next_tab();
            super::shortcuts::sync_webview_for_tab(app);
            app.update_title();
        }
        PaletteAction::PrevTab => {
            app.wm.prev_tab();
            super::shortcuts::sync_webview_for_tab(app);
            app.update_title();
        }
        PaletteAction::SelectModel(choice) => crate::ai::local::picker::select(app, &choice),
        PaletteAction::PrivacyReport => crate::ui::confirm::show_privacy_report(app),
        PaletteAction::McpActivity => app.mcp.overlay.toggle(),
        PaletteAction::Agent(l) => crate::agents::runtime::launch(app, l),
        PaletteAction::Template(_) => {}
    }
}

// ── New tool overlay handlers ──

fn handle_file_manager(app: &mut App, event: &KeyEvent) -> bool {
    // Only intercept navigation keys — let other keys pass through to terminal
    let fk = match event.logical_key {
        Key::Named(NamedKey::Escape) => Some(FileManagerKey::Escape),
        Key::Named(NamedKey::ArrowUp) => Some(FileManagerKey::Up),
        Key::Named(NamedKey::ArrowDown) => Some(FileManagerKey::Down),
        Key::Named(NamedKey::Enter) => Some(FileManagerKey::Enter),
        Key::Character(ref s) if s == "h" || s == "H" => Some(FileManagerKey::Char('h')),
        _ => None,
    };
    if let Some(k) = fk {
        if let Some(action) = app.file_manager.handle_key(k) {
            match action {
                crate::tools::file_manager::FileManagerAction::OpenFile(path) => {
                    let cmd = format!("cat {}\n", path);
                    app.wm.active_pane_mut().write(cmd.as_bytes());
                    app.file_manager.toggle();
                }
            }
        }
        app.request_redraw();
        return true;
    }
    false // Don't block terminal input
}

fn handle_git_panel(app: &mut App, event: &KeyEvent) -> bool {
    let gk = match event.logical_key {
        Key::Named(NamedKey::Escape) => GitPanelKey::Escape,
        Key::Named(NamedKey::ArrowUp) => GitPanelKey::Up,
        Key::Named(NamedKey::ArrowDown) => GitPanelKey::Down,
        Key::Named(NamedKey::Tab) => GitPanelKey::Tab,
        Key::Named(NamedKey::Enter) => GitPanelKey::Enter,
        Key::Character(ref s) => {
            if let Some(c) = s.chars().next() { GitPanelKey::Char(c) }
            else { return true; }
        }
        _ => return true,
    };
    app.git_panel.handle_key(gk);
    app.request_redraw();
    true
}

fn handle_cicd(app: &mut App, event: &KeyEvent) -> bool {
    let ck = match event.logical_key {
        Key::Named(NamedKey::Escape) => CicdKey::Escape,
        Key::Named(NamedKey::ArrowUp) => CicdKey::Up,
        Key::Named(NamedKey::ArrowDown) => CicdKey::Down,
        Key::Character(ref s) => {
            if let Some(c) = s.chars().next() { CicdKey::Char(c) }
            else { return true; }
        }
        _ => return true,
    };
    app.cicd.handle_key(ck);
    app.request_redraw();
    true
}

fn handle_heatmap(app: &mut App, event: &KeyEvent) -> bool {
    let hk = match event.logical_key {
        Key::Named(NamedKey::Escape) => HeatmapKey::Escape,
        _ => return true, // only Esc closes; other keys are swallowed
    };
    app.heatmap.handle_key(hk);
    app.request_redraw();
    true
}

fn handle_docker(app: &mut App, event: &KeyEvent) -> bool {
    let dk = match event.logical_key {
        Key::Named(NamedKey::Escape) => DockerKey::Escape,
        Key::Named(NamedKey::ArrowUp) => DockerKey::Up,
        Key::Named(NamedKey::ArrowDown) => DockerKey::Down,
        Key::Named(NamedKey::Tab) => DockerKey::Tab,
        Key::Character(ref s) => {
            if let Some(c) = s.chars().next() { DockerKey::Char(c) }
            else { return true; }
        }
        _ => return true,
    };
    if let Some(action) = app.docker.handle_key(dk) {
        match action {
            DockerAction::RunCommand(cmd) => {
                let cmd = crate::window::selection::sanitize_paste(&cmd, false);
                app.wm.active_pane_mut().write(format!("{cmd}\n").as_bytes());
                app.docker.toggle();
            }
        }
    }
    app.request_redraw();
    true
}

fn handle_netmon(app: &mut App, event: &KeyEvent) -> bool {
    let key = match event.logical_key {
        Key::Named(NamedKey::Escape) => NetMonKey::Escape,
        Key::Character(ref s) => {
            if let Some(c) = s.chars().next() { NetMonKey::Char(c) } else { return true; }
        }
        _ => return true,
    };
    app.network_monitor.handle_key(key);
    app.request_redraw();
    true
}

/// Change Review overlay: modal, consumes every key.
fn handle_review(app: &mut App, event: &KeyEvent) -> bool {
    use crate::review::ReviewKey;
    if app.modifiers.super_key() || app.modifiers.control_key() {
        return true;
    }
    let key = match &event.logical_key {
        Key::Named(NamedKey::Escape) => ReviewKey::Escape,
        Key::Named(NamedKey::ArrowUp) => ReviewKey::Up,
        Key::Named(NamedKey::ArrowDown) => ReviewKey::Down,
        Key::Named(NamedKey::PageUp) => ReviewKey::PageUp,
        Key::Named(NamedKey::PageDown) => ReviewKey::PageDown,
        Key::Named(NamedKey::Home) => ReviewKey::Home,
        Key::Named(NamedKey::End) => ReviewKey::End,
        Key::Named(NamedKey::Tab) => ReviewKey::Tab,
        Key::Named(NamedKey::Space) => ReviewKey::Char(' '),
        Key::Character(s) => match s.chars().next() {
            Some(c) => ReviewKey::Char(c),
            None => return true,
        },
        _ => return true,
    };
    if event.repeat && matches!(key, ReviewKey::Char('r' | 'R' | 'a' | 'i' | 'c' | 'C')) {
        return true;
    }
    crate::review::handle_key(app, key);
    true
}

fn handle_mcp_activity(app: &mut App, event: &KeyEvent) -> bool {
    use crate::mcp::overlay::OverlayKey;
    let key = match event.logical_key {
        Key::Named(NamedKey::Escape) => OverlayKey::Escape,
        Key::Named(NamedKey::ArrowUp) => OverlayKey::Up,
        Key::Named(NamedKey::ArrowDown) => OverlayKey::Down,
        Key::Named(NamedKey::PageUp) => OverlayKey::PageUp,
        Key::Named(NamedKey::PageDown) => OverlayKey::PageDown,
        _ => return true,
    };
    let total = app.mcp.shared.as_ref().map_or(0, |s| s.activity_len());
    app.mcp.overlay.handle_key(key, total);
    app.request_redraw();
    true
}

fn handle_proctree(app: &mut App, event: &KeyEvent) -> bool {
    let key = match event.logical_key {
        Key::Named(NamedKey::Escape) => ProcTreeKey::Escape,
        Key::Character(ref s) => {
            if let Some(c) = s.chars().next() { ProcTreeKey::Char(c) } else { return true; }
        }
        _ => return true,
    };
    app.process_tree.handle_key(key);
    app.request_redraw();
    true
}

fn handle_sysinfo(app: &mut App, event: &KeyEvent) -> bool {
    let key = match event.logical_key {
        Key::Named(NamedKey::Escape) => SysInfoKey::Escape,
        _ => return true,
    };
    app.system_info.handle_key(key);
    app.request_redraw();
    true
}

fn handle_port_dashboard(app: &mut App, event: &KeyEvent) -> bool {
    let key = match event.logical_key {
        Key::Named(NamedKey::Escape) => PortDashboardKey::Escape,
        Key::Named(NamedKey::ArrowUp) => PortDashboardKey::Up,
        Key::Named(NamedKey::ArrowDown) => PortDashboardKey::Down,
        Key::Named(NamedKey::Enter) => PortDashboardKey::Enter,
        Key::Character(ref s) => {
            if let Some(c) = s.chars().next() { PortDashboardKey::Char(c) } else { return true; }
        }
        _ => return true,
    };
    if let Some(action) = app.port_dashboard.handle_key(key) {
        match action {
            PortAction::Kill(pid) => {
                log::info!("Port Dashboard: killing pid {pid} (SIGTERM)");
                crate::tools::port_dashboard::kill_pid(pid);
                app.port_dashboard.refresh();
            }
        }
    }
    app.request_redraw();
    true
}

fn handle_regex_playground(app: &mut App, event: &KeyEvent) -> bool {
    let ctrl = app.modifiers.control_key();
    let rk = match &event.logical_key {
        Key::Named(NamedKey::Escape) => Some(RegexKey::Escape),
        Key::Named(NamedKey::Backspace) => Some(RegexKey::Backspace),
        Key::Named(NamedKey::Enter) => Some(RegexKey::Enter),
        Key::Named(NamedKey::Tab) => Some(RegexKey::Tab),
        Key::Named(NamedKey::Space) => Some(RegexKey::Char(' ')),
        Key::Character(s) if ctrl && s.eq_ignore_ascii_case("i") => Some(RegexKey::CtrlI),
        Key::Character(s) if ctrl && s.eq_ignore_ascii_case("m") => Some(RegexKey::CtrlM),
        Key::Character(s) if ctrl && s.eq_ignore_ascii_case("s") => Some(RegexKey::CtrlS),
        Key::Character(s) if !ctrl => s.chars().next().map(RegexKey::Char),
        _ => None,
    };
    if let Some(k) = rk {
        app.regex_playground.handle_key(k);
    }
    app.request_redraw();
    true
}

fn addr_bar_key_char(event: &KeyEvent) -> Option<String> {
    use winit::keyboard::PhysicalKey;
    match &event.logical_key {
        Key::Character(s) => return Some(s.to_lowercase()),
        _ => {}
    }
    // Fallback: decode from physical key (Cmd changes logical_key on some macOS configs)
    if let PhysicalKey::Code(code) = event.physical_key {
        let c = match code {
            winit::keyboard::KeyCode::KeyA => "a",
            winit::keyboard::KeyCode::KeyC => "c",
            winit::keyboard::KeyCode::KeyV => "v",
            winit::keyboard::KeyCode::KeyX => "x",
            _ => return None,
        };
        return Some(c.to_string());
    }
    None
}
