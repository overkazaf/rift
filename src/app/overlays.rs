use winit::event::KeyEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};

use crate::ai::{AiAction, AiPanelKey};
use crate::tools::cicd::CicdKey;
use crate::tools::command_palette::{PaletteAction, PaletteEffect, PaletteKey};
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
use crate::ui::{MenuAction, PrefsAction, PrefsKey};
use crate::network::{SshDialogKey, WvDialogKey};
use crate::ui::WelcomeKey;

use super::App;

/// Route key events to the topmost visible overlay.
/// Returns true if the event was consumed.
pub fn try_intercept(app: &mut App, event: &KeyEvent, event_loop: &ActiveEventLoop) -> bool {
    // Preview-Then-Accept danger confirmation — absolute highest priority.
    // Nothing should be able to bypass an unconfirmed dangerous command.
    if app.exec_preview.visible {
        return handle_exec_preview(app, event);
    }
    // TimeWarp browser has highest priority
    if app.timewarp_browser.active {
        return handle_timewarp(app, event);
    }
    // WebView address bar editing
    if app.addr_bar_editing {
        return handle_addr_bar(app, event);
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
    if app.system_info.visible {
        return handle_sysinfo(app, event);
    }
    if app.port_dashboard.visible {
        return handle_port_dashboard(app, event);
    }
    if app.regex_playground.visible {
        return handle_regex_playground(app, event);
    }
    if app.ai_panel.visible {
        return handle_ai(app, event);
    }
    if app.history.visible {
        return handle_history(app, event);
    }
    if app.autocomplete.visible {
        return handle_autocomplete(app, event);
    }
    false
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
                        let clean = text.lines().next().unwrap_or("").to_string();
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

fn handle_ai(app: &mut App, event: &KeyEvent) -> bool {
    let ak = match event.logical_key {
        Key::Named(NamedKey::Escape) => Some(AiPanelKey::Escape),
        Key::Named(NamedKey::Backspace) => Some(AiPanelKey::Backspace),
        Key::Named(NamedKey::Enter) => {
            if app.modifiers.shift_key() {
                Some(AiPanelKey::ShiftEnter)
            } else {
                Some(AiPanelKey::Enter)
            }
        }
        Key::Named(NamedKey::Tab) => Some(AiPanelKey::Tab),
        Key::Named(NamedKey::Space) => Some(AiPanelKey::Char(' ')),
        Key::Character(ref s) => s.chars().next().map(AiPanelKey::Char),
        _ => None,
    };
    if let Some(k) = ak {
        if let Some(action) = app.ai_panel.handle_key(k) {
            match action {
                AiAction::Ask(question) => {
                    let ctx = crate::ai::context::TermContext::collect();
                    let rx = app.llm.ask(&question, ctx);
                    app.ai_panel.set_receiver(rx);
                    // A new question invalidates any review of the previous
                    // suggested command — drop it so the badge doesn't show
                    // stale advice next to an unrelated response.
                    app.advisor.clear();
                }
                AiAction::Execute(cmd) => {
                    app.wm.active_pane_mut().write(format!("{cmd}\n").as_bytes());
                }
                AiAction::CopyToTerminal(cmd) => {
                    app.wm.active_pane_mut().write(cmd.as_bytes());
                }
            }
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
        _ => CompareKey::Escape,
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
                let line: String = terminal.grid[row].iter().take(col).map(|c| c.c).collect();
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
                    app.wm.active_pane_mut().write(format!("{cmd}\n").as_bytes());
                }
                HistoryAction::Insert(cmd) => {
                    app.wm.active_pane_mut().write(cmd.as_bytes());
                }
            }
        }
        app.request_redraw();
    }
    true
}

fn handle_command_palette(app: &mut App, event: &KeyEvent, event_loop: &ActiveEventLoop) -> bool {
    let pk = match event.logical_key {
        Key::Named(NamedKey::Escape) => Some(PaletteKey::Escape),
        Key::Named(NamedKey::Backspace) => Some(PaletteKey::Backspace),
        Key::Named(NamedKey::Enter) => Some(PaletteKey::Enter),
        Key::Named(NamedKey::ArrowUp) => Some(PaletteKey::Up),
        Key::Named(NamedKey::ArrowDown) => Some(PaletteKey::Down),
        Key::Named(NamedKey::Space) => Some(PaletteKey::Char(' ')),
        Key::Character(ref s) => s.chars().next().map(PaletteKey::Char),
        _ => None,
    };
    if let Some(k) = pk {
        if let Some(action) = app.command_palette.handle_key(k) {
            dispatch_palette_action(app, action, event_loop);
        }
        app.request_redraw();
    }
    true
}

/// Map a palette selection onto the existing feature it represents. Most
/// actions have a 1:1 `MenuAction` equivalent, so we reuse
/// `shortcuts::handle_menu_action` instead of duplicating its logic
/// (pane sizing, resize-on-split, exit-on-last-tab-close, etc).
fn dispatch_palette_action(app: &mut App, action: PaletteAction, event_loop: &ActiveEventLoop) {
    let menu_action = match action {
        PaletteAction::NewTab => Some(MenuAction::NewTab),
        PaletteAction::CloseTab => Some(MenuAction::CloseTab),
        PaletteAction::SplitH => Some(MenuAction::SplitH),
        PaletteAction::SplitV => Some(MenuAction::SplitV),
        PaletteAction::Search => Some(MenuAction::Find),
        PaletteAction::Ssh => Some(MenuAction::SshConnect),
        PaletteAction::Ai => Some(MenuAction::AiAssistant),
        PaletteAction::Hud => Some(MenuAction::HudToggle),
        PaletteAction::TimeWarp => Some(MenuAction::TimeWarp),
        PaletteAction::Recording => Some(MenuAction::Recording),
        PaletteAction::FileManager => Some(MenuAction::FileManager),
        PaletteAction::GitPanel => Some(MenuAction::GitPanel),
        PaletteAction::Docker => Some(MenuAction::DockerPanel),
        PaletteAction::Cicd => Some(MenuAction::CicdPanel),
        PaletteAction::Heatmap => Some(MenuAction::Heatmap),
        PaletteAction::SecretMask => Some(MenuAction::SecretMask),
        PaletteAction::AuditLog => Some(MenuAction::AuditLog),
        PaletteAction::Teaching => Some(MenuAction::TeachingMode),
        PaletteAction::Observer => Some(MenuAction::ObserverMode),
        PaletteAction::Welcome => Some(MenuAction::Welcome),
        PaletteAction::Prefs => Some(MenuAction::Preferences),
        PaletteAction::NetworkMonitor => Some(MenuAction::NetworkMonitor),
        PaletteAction::ProcessTree => Some(MenuAction::ProcessTree),
        PaletteAction::SystemInfo => Some(MenuAction::SystemInfo),
        PaletteAction::WebView => Some(MenuAction::WebView),
        PaletteAction::Effect(e) => Some(match e {
            PaletteEffect::Crt => MenuAction::CrtEffect,
            PaletteEffect::Glitch => MenuAction::GlitchEffect,
            PaletteEffect::Neon => MenuAction::NeonEffect,
            PaletteEffect::Matrix => MenuAction::MatrixEffect,
            PaletteEffect::Amber => MenuAction::AmberEffect,
            PaletteEffect::Hologram => MenuAction::HologramEffect,
            PaletteEffect::Pixelate => MenuAction::PixelateEffect,
            PaletteEffect::Thermal => MenuAction::ThermalEffect,
            PaletteEffect::Off => MenuAction::NoEffect,
        }),
        PaletteAction::Theme(name) => {
            if let Some(theme) = crate::config::Config::theme_by_name(&name) {
                app.renderer.set_theme(theme);
                app.config.theme_name = name;
            }
            None
        }
    };
    if let Some(ma) = menu_action {
        super::shortcuts::handle_menu_action(app, ma, event_loop);
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
        _ => HeatmapKey::Escape,
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

fn handle_addr_bar(app: &mut App, event: &KeyEvent) -> bool {
    // Cmd+C/V/A/X in address bar
    if app.modifiers.super_key() {
        if let Some(ref s) = addr_bar_key_char(event) {
            match s.as_str() {
                "c" => {
                    crate::window::selection::copy_to_clipboard(&app.addr_bar_text);
                    log::info!("Address bar: copied URL ({} chars)", app.addr_bar_text.len());
                    app.request_redraw();
                    return true;
                }
                "v" => {
                    if let Some(text) = crate::window::selection::paste_from_clipboard() {
                        let clean = text.lines().next().unwrap_or("").to_string();
                        app.addr_bar_text.push_str(&clean);
                        log::info!("Address bar: pasted {} chars", clean.len());
                    }
                    app.request_redraw();
                    return true;
                }
                "a" => {
                    app.request_redraw();
                    return true;
                }
                "x" => {
                    crate::window::selection::copy_to_clipboard(&app.addr_bar_text);
                    app.addr_bar_text.clear();
                    app.request_redraw();
                    return true;
                }
                _ => {}
            }
        }
        // Consume Cmd+<anything> to prevent it leaking to terminal
        app.request_redraw();
        return true;
    }
    match &event.logical_key {
        Key::Named(NamedKey::Escape) => {
            app.addr_bar_editing = false;
        }
        Key::Named(NamedKey::Backspace) => {
            app.addr_bar_text.pop();
        }
        Key::Named(NamedKey::Enter) => {
            if app.modifiers.shift_key() {
                super::shortcuts::toggle_webview_maximize(app);
            } else {
                let url = app.addr_bar_text.trim().to_string();
                app.addr_bar_editing = false;
                if !url.is_empty() {
                    if let Some(wv) = &mut app.webview {
                        let nav_url = if url.starts_with("http://") || url.starts_with("https://") {
                            url.clone()
                        } else {
                            format!("https://{}", url)
                        };
                        wv.navigate(&nav_url);
                        wv.url = nav_url;
                    }
                }
            }
        }
        Key::Character(ref s) => {
            app.addr_bar_text.push_str(s);
        }
        Key::Named(NamedKey::Space) => {
            app.addr_bar_text.push(' ');
        }
        _ => {}
    }
    app.request_redraw();
    true
}
