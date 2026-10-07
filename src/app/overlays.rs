use winit::event::KeyEvent;
use winit::keyboard::{Key, NamedKey};

use crate::ai::{AiAction, AiPanelKey};
use crate::tools::cicd::CicdKey;
use crate::tools::compare::CompareKey;
use crate::tools::docker_panel::{DockerKey, DockerAction};
use crate::tools::file_manager::FileManagerKey;
use crate::tools::git_panel::GitPanelKey;
use crate::tools::heatmap::HeatmapKey;
use crate::tools::search::{SearchKey, SearchAction};
use crate::ui::{PrefsAction, PrefsKey};
use crate::network::{SshDialogKey, WvDialogKey};
use crate::ui::WelcomeKey;

use super::App;

/// Route key events to the topmost visible overlay.
/// Returns true if the event was consumed.
pub fn try_intercept(app: &mut App, event: &KeyEvent) -> bool {
    // TimeWarp browser has highest priority
    if app.timewarp_browser.active {
        return handle_timewarp(app, event);
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
    if app.ai_panel.visible {
        return handle_ai(app, event);
    }
    if app.autocomplete.visible {
        return handle_autocomplete(app, event);
    }
    false
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
