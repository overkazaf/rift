use winit::event::KeyEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::input;
use crate::tools::exec_preview::ExecPreview;
use crate::ui::MenuAction;
use crate::effects::{CrtParams, GlitchParams, MatrixParams, NeonParams, ShaderEffect,
                     AmberParams, HologramParams, PixelateParams, ThermalParams,
                     RaindropParams, VhsParams, GridParams, FilmGrainParams, InvertParams, DesaturateParams,
                     ChromaticParams, PulseParams, SnowParams, UnderwaterParams, NeonOutlineParams, ScanlineRgbParams};
use crate::network::{AuthMethod, SshConfig, SshPty, SshConnectRequest};

use crate::window::tab::PaneCmd;

use super::{App, SshConnecting};
use super::overlays;

/// On macOS, terminal shortcuts use Cmd (super); on other platforms, Ctrl.
fn cmd_or_ctrl(modifiers: &ModifiersState) -> bool {
    #[cfg(target_os = "macos")]
    { modifiers.super_key() }
    #[cfg(not(target_os = "macos"))]
    { modifiers.control_key() }
}

// ── Keyboard ──

pub fn handle_key(app: &mut App, event: &KeyEvent, event_loop: &ActiveEventLoop) {
    // While an IME composition is in progress, text-producing keys belong to the
    // IME (they arrive via Ime::Commit); never also encode them to the PTY/overlays.
    if !app.ime_preedit.is_empty()
        && !app.modifiers.super_key()
        && !app.modifiers.control_key()
        && matches!(&event.logical_key, Key::Character(_) | Key::Named(NamedKey::Space))
    {
        return;
    }

    // 1. Overlay interception (priority order)
    if overlays::try_intercept(app, event, event_loop) {
        return;
    }

    // 1b. Command blocks: Cmd+Shift+Up/Down/C, Cmd+C on a selected block
    if crate::blocks_ui::on_key(app, event) {
        return;
    }

    // 2. Cmd+C / Cmd+V (copy/paste) — macOS super key
    if app.modifiers.super_key() {
        let is_copy = matches!(&event.logical_key, Key::Character(s) if s.eq_ignore_ascii_case("c"));
        let is_paste = matches!(&event.logical_key, Key::Character(s) if s.eq_ignore_ascii_case("v"));
        let is_select_all = matches!(&event.logical_key, Key::Character(s) if s.eq_ignore_ascii_case("a"));

        if is_copy {
            if app.selection.active {
                if super::mouse::copy_selection(app) {
                    log::info!("Copied selection");
                }
                app.selection.clear();
                app.request_redraw();
            }
            return;
        }
        if is_paste {
            super::mouse::paste_clipboard(app);
            return;
        }
        if is_select_all {
            super::mouse::select_all(app);
            return;
        }
    }

    // Clear selection on any typing
    if app.selection.active && !app.modifiers.shift_key() && !app.modifiers.super_key() {
        if matches!(event.logical_key, Key::Character(_)) {
            app.selection.clear();
            app.request_redraw();
        }
    }

    // 2a2. Pane management (focus / resize / swap / zoom / equalize / close)
    if super::panes::try_pane_shortcut(app, event, event_loop) {
        return;
    }

    // 2b. Font zoom: Cmd+= (zoom in), Ctrl+- (zoom out, Cmd+- eaten by macOS), Cmd+0 (reset)
    if app.modifiers.super_key() && !app.modifiers.shift_key() {
        if let Key::Character(ref s) = event.logical_key {
            match s.as_str() {
                "=" | "+" => { zoom_font(app, 1.0); return; }
                "0" => { reset_font(app); return; }
                _ => {}
            }
        }
    }
    // Ctrl+- / Ctrl+= for zoom (bypass macOS menu interception)
    if app.modifiers.control_key() && !app.modifiers.shift_key() && !app.modifiers.super_key() {
        if let Key::Character(ref s) = event.logical_key {
            match s.as_str() {
                "-" | "\u{2013}" | "\u{2014}" | "\u{2212}" | "\u{1f}" => { zoom_font(app, -1.0); return; }
                "=" | "+" => { zoom_font(app, 1.0); return; }
                "0" => { reset_font(app); return; }
                _ => {}
            }
        }
    }
    // Ctrl+Shift shortcuts (avoid macOS interception)
    if app.modifiers.control_key() && app.modifiers.shift_key() {
        if let Key::Character(ref s) = event.logical_key {
            match s.to_lowercase().as_str() {
                "z" if !event.repeat => {
                    if app.timewarp_browser.active { app.timewarp_browser.exit(); }
                    else { app.timewarp_browser.enter(); }
                    app.request_redraw();
                    return;
                }
                "v" if !event.repeat => {
                    if !app.observer.enabled { app.observer.toggle(); }
                    app.observer_summary = Some(app.observer.generate_summary());
                    app.request_redraw();
                    return;
                }
                _ => {}
            }
        }
    }

    // 2b2. Cmd+D (no shift) = vertical split, Cmd+F = search
    if app.modifiers.super_key() && !app.modifiers.shift_key() {
        if let Key::Character(ref s) = event.logical_key {
            if s.eq_ignore_ascii_case("d") && !event.repeat {
                super::panes::run_pane_cmd(app, PaneCmd::SplitRight, event_loop);
                return;
            }
            if s.eq_ignore_ascii_case("f") && !event.repeat {
                app.search.toggle();
                if app.search.visible {
                    let pane = app.wm.active_pane();
                    app.search.search(&pane.terminal.scrollback, &pane.terminal.grid);
                }
                app.request_redraw();
                return;
            }
            if s.eq_ignore_ascii_case("p") && !event.repeat {
                super::overlays::open_command_palette(app);
                app.request_redraw();
                return;
            }
        }
    }

    // 2c. Ctrl+Space → autocomplete (always Ctrl, not Cmd)
    if app.modifiers.control_key() && !app.modifiers.shift_key() {
        if let Key::Named(NamedKey::Space) = event.logical_key {
            trigger_autocomplete(app);
            return;
        }
    }

    // 2c2. Ctrl+R → Smart History Search (fzf/atuin-style reverse search)
    if app.modifiers.control_key() && !app.modifiers.shift_key() {
        if let Key::Character(ref s) = event.logical_key {
            if s.eq_ignore_ascii_case("r") && !event.repeat {
                app.history.toggle();
                app.request_redraw();
                return;
            }
        }
    }

    // 2c. Ctrl+Shift+number — visual effects (NOT Cmd, to avoid macOS screenshot conflict)
    if app.modifiers.control_key() && app.modifiers.shift_key() {
        if let Key::Character(ref s) = event.logical_key {
            let effect_handled = match s.as_str() {
                "1" | "!" => { app.renderer.shader.set_effect(Some(ShaderEffect::Crt(CrtParams::default()))); true }
                "2" | "@" => { app.renderer.shader.set_effect(Some(ShaderEffect::Glitch(GlitchParams::default()))); true }
                "3" | "#" => { app.renderer.shader.set_effect(Some(ShaderEffect::NeonGlow(NeonParams::default()))); true }
                "4" | "$" => { app.renderer.shader.set_effect(Some(ShaderEffect::MatrixRain(MatrixParams::default()))); true }
                "5" | "%" => { app.renderer.shader.set_effect(Some(ShaderEffect::Amber(AmberParams::default()))); true }
                "6" | "^" => { app.renderer.shader.set_effect(Some(ShaderEffect::Hologram(HologramParams::default()))); true }
                "7" | "&" => { app.renderer.shader.set_effect(Some(ShaderEffect::Pixelate(PixelateParams::default()))); true }
                "8" | "*" => { app.renderer.shader.set_effect(Some(ShaderEffect::Thermal(ThermalParams::default()))); true }
                "0" | ")" => { app.renderer.shader.set_effect(None); true }
                _ => false,
            };
            if effect_handled && !event.repeat {
                app.request_redraw();
                return;
            }
        }
    }

    // 3. Cmd+Shift (macOS) / Ctrl+Shift (others) hotkeys
    if cmd_or_ctrl(&app.modifiers) && app.modifiers.shift_key() {
        if let Key::Character(ref s) = event.logical_key {
            if !event.repeat && handle_mod_shift(app, s, event_loop) {
                app.request_redraw();
                return;
            }
        }
    }

    // 4. Ctrl+Tab — switch tabs (always Ctrl, Cmd+Tab is macOS App Switcher)
    if app.modifiers.control_key() {
        if let Key::Named(NamedKey::Tab) = event.logical_key {
            if !event.repeat {
                if app.modifiers.shift_key() {
                    app.wm.prev_tab();
                } else {
                    app.wm.next_tab();
                }
                sync_webview_for_tab(app);
                app.update_title();
                app.request_redraw();
            }
            return;
        }
    }

    // 5. Shift+PageUp/PageDown — scrollback navigation
    if app.modifiers.shift_key() && !app.modifiers.control_key() {
        match event.logical_key {
            Key::Named(NamedKey::PageUp) => {
                let rows = app.wm.active_pane().terminal.rows;
                app.wm.active_pane_mut().terminal.scroll_view_up(rows / 2);
                app.request_redraw();
                return;
            }
            Key::Named(NamedKey::PageDown) => {
                let rows = app.wm.active_pane().terminal.rows;
                app.wm.active_pane_mut().terminal.scroll_view_down(rows / 2);
                app.request_redraw();
                return;
            }
            Key::Named(NamedKey::Home) => {
                let total = app.wm.active_pane().terminal.scrollback_len();
                app.wm.active_pane_mut().terminal.scroll_view_up(total);
                app.request_redraw();
                return;
            }
            Key::Named(NamedKey::End) => {
                app.wm.active_pane_mut().terminal.scroll_to_bottom();
                app.request_redraw();
                return;
            }
            _ => {}
        }
    }

    // 6. Alt+Arrow pane focus is handled by panes::try_pane_shortcut (step 2a2).

    // 7. Normal input → active pane (or broadcast to all panes)
    let app_cursor = app.wm.active_pane().terminal.app_cursor_keys;
    if let Some(bytes) = input::encode_key(event, app.modifiers, app_cursor) {
        // Enter key: feed observer + block tracking
        if bytes == b"\r" {
            let (cmd_opt, scrollback_line) = {
                let term = &app.wm.active_pane().terminal;
                let row = term.cursor_row.min(term.grid.len().saturating_sub(1));
                let line: String = term.grid[row].iter().map(|c| c.c).collect();
                let trimmed = line.trim();
                let cmd = if !trimmed.is_empty() {
                    let c = if let Some(pos) = trimmed.rfind(|c| c == '$' || c == '%') {
                        trimmed[pos + 1..].trim()
                    } else {
                        trimmed
                    };
                    if c.is_empty() { None } else { Some(c.to_string()) }
                } else {
                    None
                };
                (cmd, term.scrollback.len() + row)
            };

            if let Some(cmd) = cmd_opt {
                // Preview-Then-Accept: dangerous commands are intercepted
                // here, before their trailing Enter ever reaches the PTY.
                // The command's characters were already streamed to the PTY
                // as the user typed them (this terminal has no local-echo
                // buffer of its own), so they're already sitting uncommitted
                // in the shell's line editor — confirming later just needs
                // to submit that buffer (see overlays::handle_exec_preview).
                if let Some(preview) = ExecPreview::check_command_in(&cmd, app.wm.active_pane().terminal.cwd.as_deref()) {
                    app.exec_preview = preview;
                    app.exec_preview.visible = true;
                    app.request_redraw();
                    return;
                }

                if app.observer.enabled {
                    app.observer.on_command(&cmd);
                }
                // Exact OSC 133 blocks live on the terminal; heuristic is fallback only.
                if !app.wm.active_pane().terminal.blocks.osc_seen() {
                    app.blocks.on_input(&cmd, scrollback_line);
                }
                if app.audit.enabled {
                    app.audit.log_command(&cmd, 0);
                }
                // Teaching mode: ask LLM to explain the command
                if app.teaching.enabled {
                    let prompt = crate::tools::teaching::TeachingMode::explain_prompt(&cmd);
                    let config = app.llm.config.clone();
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let result = crate::ai::backend::complete_simple(&config, &prompt);
                        let _ = tx.send(result);
                    });
                    app.teaching.set_receiver(rx);
                }
            }
        }

        if let Some(rec) = &mut app.recorder {
            rec.record_input(&bytes);
        }
        if app.broadcast {
            let tab = app.wm.active_tab_mut();
            for pane in tab.panes_mut() {
                pane.write(&bytes);
            }
        } else {
            app.wm.active_pane_mut().write(&bytes);
        }

        // Immediately poll PTY for echo — eliminates 1-2 frame latency
        if app.wm.process_all_output() {
            app.wm.flush_all_responses();
        }
        app.request_redraw();
    }
}

// ── Mouse ──

pub fn handle_click(app: &mut App, event_loop: &ActiveEventLoop) {
    let y = app.cursor_y;
    let tbh = app.tab_bar_height();

    if y < tbh {
        super::tabs::press(app, event_loop);
    }
    app.request_redraw();
}

// ── Menu bar ──

pub fn handle_menu_action(app: &mut App, action: MenuAction, event_loop: &ActiveEventLoop) {
    match action {
        MenuAction::NewTab => {
            let (c, r) = pane_size(app);
            app.wm.new_tab(c, r);
            sync_webview_for_tab(app);
            app.update_title();
        }
        MenuAction::CloseTab => {
            if app.wm.close_current() { event_loop.exit(); }
            app.update_title();
        }
        MenuAction::SshConnect => app.ssh_dialog.toggle(),
        MenuAction::ToggleFullScreen => {
            if let Some(w) = &app.window {
                let fs = w.fullscreen().is_some();
                w.set_fullscreen(if fs { None } else {
                    Some(winit::window::Fullscreen::Borderless(None))
                });
            }
        }
        MenuAction::ZoomIn => zoom_font(app, 1.0),
        MenuAction::ZoomOut => zoom_font(app, -1.0),
        MenuAction::ZoomReset => reset_font(app),
        MenuAction::SplitH => super::panes::run_pane_cmd(app, PaneCmd::SplitRight, event_loop),
        MenuAction::SplitV => super::panes::run_pane_cmd(app, PaneCmd::SplitDown, event_loop),
        MenuAction::Pane(cmd) => super::panes::run_pane_cmd(app, cmd, event_loop),
        MenuAction::Recording => app.toggle_recording(),
        // Effects
        MenuAction::CrtEffect => app.renderer.shader.set_effect(Some(ShaderEffect::Crt(CrtParams::default()))),
        MenuAction::GlitchEffect => app.renderer.shader.set_effect(Some(ShaderEffect::Glitch(GlitchParams::default()))),
        MenuAction::NeonEffect => app.renderer.shader.set_effect(Some(ShaderEffect::NeonGlow(NeonParams::default()))),
        MenuAction::MatrixEffect => app.renderer.shader.set_effect(Some(ShaderEffect::MatrixRain(MatrixParams::default()))),
        MenuAction::AmberEffect => app.renderer.shader.set_effect(Some(ShaderEffect::Amber(AmberParams::default()))),
        MenuAction::HologramEffect => app.renderer.shader.set_effect(Some(ShaderEffect::Hologram(HologramParams::default()))),
        MenuAction::PixelateEffect => app.renderer.shader.set_effect(Some(ShaderEffect::Pixelate(PixelateParams::default()))),
        MenuAction::ThermalEffect => app.renderer.shader.set_effect(Some(ShaderEffect::Thermal(ThermalParams::default()))),
        MenuAction::RaindropEffect => app.renderer.shader.set_effect(Some(ShaderEffect::Raindrop(RaindropParams::default()))),
        MenuAction::VhsEffect => app.renderer.shader.set_effect(Some(ShaderEffect::Vhs(VhsParams::default()))),
        MenuAction::GridEffect => app.renderer.shader.set_effect(Some(ShaderEffect::CyberpunkGrid(GridParams::default()))),
        MenuAction::FilmGrainEffect => app.renderer.shader.set_effect(Some(ShaderEffect::FilmGrain(FilmGrainParams::default()))),
        MenuAction::InvertEffect => app.renderer.shader.set_effect(Some(ShaderEffect::Invert(InvertParams::default()))),
        MenuAction::DesaturateEffect => app.renderer.shader.set_effect(Some(ShaderEffect::Desaturate(DesaturateParams::default()))),
        MenuAction::ChromaticEffect => app.renderer.shader.set_effect(Some(ShaderEffect::Chromatic(ChromaticParams::default()))),
        MenuAction::PulseEffect => app.renderer.shader.set_effect(Some(ShaderEffect::Pulse(PulseParams::default()))),
        MenuAction::SnowEffect => app.renderer.shader.set_effect(Some(ShaderEffect::Snow(SnowParams::default()))),
        MenuAction::UnderwaterEffect => app.renderer.shader.set_effect(Some(ShaderEffect::Underwater(UnderwaterParams::default()))),
        MenuAction::NeonOutlineEffect => app.renderer.shader.set_effect(Some(ShaderEffect::NeonOutline(NeonOutlineParams::default()))),
        MenuAction::ScanlineRgbEffect => app.renderer.shader.set_effect(Some(ShaderEffect::ScanlineRgb(ScanlineRgbParams::default()))),
        MenuAction::NoEffect => app.renderer.shader.set_effect(None),
        // UI panels
        MenuAction::Preferences => app.prefs.toggle(),
        MenuAction::Welcome => app.welcome.toggle(),
        MenuAction::UiGallery => crate::ui::kit::gallery::toggle(),
        MenuAction::WebView => crate::network::browser::toggle(app),
        MenuAction::Browser(cmd) => crate::network::browser::run_command(app, cmd),
        MenuAction::ClearBuffer => super::mouse::clear_buffer(app),
        MenuAction::Find => {
            app.search.toggle();
            if app.search.visible {
                let pane = app.wm.active_pane();
                app.search.search(&pane.terminal.scrollback, &pane.terminal.grid);
            }
        }
        // Tools
        MenuAction::FileManager => app.file_manager.toggle(),
        MenuAction::GitPanel => app.git_panel.toggle(),
        MenuAction::DockerPanel => app.docker.toggle(),
        MenuAction::CicdPanel => app.cicd.toggle(),
        MenuAction::NetworkMonitor => app.network_monitor.toggle(),
        MenuAction::ProcessTree => app.process_tree.toggle(),
        MenuAction::SystemInfo => app.system_info.toggle(),
        MenuAction::PortDashboard => app.port_dashboard.toggle(),
        MenuAction::RegexPlayground => app.regex_playground.toggle(),
        MenuAction::Heatmap => app.heatmap.toggle(),
        MenuAction::SecretMask => app.secret_mask.toggle(),
        MenuAction::AuditLog => app.audit.toggle(),
        MenuAction::TeachingMode => app.teaching.toggle(),
        // AI
        MenuAction::AiAssistant => app.ai_panel.toggle(),
        MenuAction::ObserverMode => {
            app.observer.toggle();
            if app.observer.enabled {
                log::info!("Observer: enabled");
            } else {
                log::info!("Observer: disabled");
            }
        }
        MenuAction::AdvisorMode => {
            app.advisor.toggle();
            log::info!("Advisor: {}", if app.advisor.enabled { "enabled" } else { "disabled" });
        }
        // Terminal
        MenuAction::HudToggle => {
            app.hud_visible = !app.hud_visible;
            if app.hud_visible { app.hud.update(); }
            resize_from_window(app);
        }
        MenuAction::TimeWarp => {
            if app.timewarp_browser.active {
                app.timewarp_browser.exit();
            } else {
                app.timewarp_browser.enter();
            }
        }
        MenuAction::BroadcastToggle => {
            app.broadcast = !app.broadcast;
            app.update_title();
        }
        MenuAction::CompareOutput => {
            let panes = app.wm.active_tab().panes();
            app.compare_view.collect_and_compare(&panes);
        }
    }
    app.request_redraw();
}

// ── SSH connect (non-blocking) ──

pub fn do_ssh_connect(app: &mut App, req: SshConnectRequest) {
    if app.ssh_connecting.is_some() {
        log::warn!("SSH: already connecting, ignoring");
        return;
    }

    let cols = app.wm.active_pane().terminal.cols as u16;
    let rows = app.wm.active_pane().terminal.rows as u16;

    let config = SshConfig {
        host: req.host.clone(),
        port: req.port,
        user: req.user.clone(),
        auth: AuthMethod::Agent,
    };

    log::info!("SSH: connecting to {}@{}:{} (background)...", req.user, req.host, req.port);
    if let Some(w) = &app.window {
        w.set_title(&format!("rift — connecting to {}...", req.host));
    }

    let proxy = app.wm.get_proxy();
    let proxy2 = proxy.clone();
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = SshPty::connect(config, cols, rows, proxy2);
        let _ = tx.send(result.map_err(|e| e.to_string()));
        let _ = proxy.send_event(());
    });

    app.ssh_connecting = Some(SshConnecting { rx, req });
}

// ── WebView ──

pub fn open_webview(app: &mut App, url: &str) {
    crate::network::browser::open(app, url);
}

// ── Private helpers ──

fn handle_mod_shift(app: &mut App, key: &str, event_loop: &ActiveEventLoop) -> bool {
    match key {
        "r" | "R" => { app.toggle_recording(); true }
        "t" | "T" => {
            let (c, r) = pane_size(app);
            app.wm.new_tab(c, r);
            sync_webview_for_tab(app);
            app.update_title();
            true
        }
        "w" | "W" => {
            if app.wm.close_current() { event_loop.exit(); }
            app.update_title();
            true
        }
        "d" | "D" => {
            // Cmd+Shift+D = split down (new pane below)
            super::panes::run_pane_cmd(app, PaneCmd::SplitDown, event_loop);
            true
        }
        "-" | "_" => {
            super::panes::run_pane_cmd(app, PaneCmd::SplitDown, event_loop);
            true
        }
        // Effects moved to Ctrl+Shift+1-8 (avoids macOS screenshot conflict)
        "," | "<" => { app.prefs.toggle(); true }
        "[" | "{" => {
            app.wm.prev_tab(); sync_webview_for_tab(app); app.update_title(); true
        }
        "]" | "}" => {
            app.wm.next_tab(); sync_webview_for_tab(app); app.update_title(); true
        }
        "?" | "/" => { app.welcome.toggle(); true }
        "z" | "Z" => {
            if app.timewarp_browser.active {
                app.timewarp_browser.exit();
            } else {
                app.timewarp_browser.enter();
            }
            true
        }
        "h" | "H" => {
            app.hud_visible = !app.hud_visible;
            if app.hud_visible { app.hud.update(); }
            // Resize panes to account for HUD space
            resize_from_window(app);
            true
        }
        "s" | "S" => { app.ssh_dialog.toggle(); true }
        "a" | "A" => { app.ai_panel.toggle(); true }
        "b" | "B" => { crate::network::browser::toggle(app); true }
        "g" | "G" => {
            app.git_panel.toggle();
            true
        }
        "k" | "K" => {
            let panes = app.wm.active_tab().panes();
            app.compare_view.collect_and_compare(&panes);
            true
        }
        "f" | "F" => {
            app.search.toggle();
            if app.search.visible {
                let pane = app.wm.active_pane();
                app.search.search(&pane.terminal.scrollback, &pane.terminal.grid);
            }
            true
        }
        "e" | "E" => { app.file_manager.toggle(); true }
        "i" | "I" => { app.cicd.toggle(); true }
        "l" | "L" => { app.teaching.toggle(); true }
        "y" | "Y" => { app.heatmap.toggle(); true }
        "o" | "O" => { app.docker.toggle(); true }
        "x" | "X" => { app.regex_playground.toggle(); true }
        "m" | "M" => { app.secret_mask.toggle(); true }
        "u" | "U" => { app.audit.toggle(); true }
        // "v" removed — Cmd+Shift+V intercepted by macOS "Paste and Match Style"
        // Observer moved to Ctrl+Shift+V below
        "p" | "P" => {
            app.broadcast = !app.broadcast;
            app.update_title();
            log::info!("Broadcast mode: {}", if app.broadcast { "ON" } else { "OFF" });
            true
        }
        _ => false,
    }
}

pub(super) fn sync_webview_for_tab(app: &mut App) {
    if let Some(wv) = &mut app.webview {
        let should_show = app.webview_tab == Some(app.wm.active_tab);
        if wv.visible != should_show {
            wv.set_visible(should_show);
            if !should_show {
                wv.focus_parent();
                app.browser.editing = false;
                app.browser.focused = false;
            }
            resize_from_window(app);
            crate::network::browser::apply_bounds(app);
            if !should_show {
                if let Some(w) = &app.window { w.focus_window(); }
            }
        }
    }
}

fn trigger_autocomplete(app: &mut App) {
    let terminal = &app.wm.active_pane().terminal;
    let row = terminal.cursor_row;
    let col = terminal.cursor_col;
    let line: String = terminal.grid[row]
        .iter()
        .take(col)
        .map(|c| c.c)
        .collect::<String>()
        .trim_start()
        .to_string();
    if !line.is_empty() {
        app.autocomplete.update(&line);
        app.request_redraw();
    }
}

fn pane_size(app: &App) -> (usize, usize) {
    let p = app.wm.active_pane();
    (p.terminal.cols, p.terminal.rows)
}

pub fn resize_from_window(app: &mut App) {
    if let Some(win) = &app.window {
        let s = win.inner_size();
        super::lifecycle::handle_resize(app, s.width, s.height);
    }
}

fn zoom_font(app: &mut App, delta: f32) {
    app.config.font_size = (app.config.font_size + delta).clamp(8.0, 32.0);
    reinit_font_from_config(app);
}

fn reset_font(app: &mut App) {
    app.config.font_size = 15.0;
    reinit_font_from_config(app);
}

pub(super) fn reinit_font_from_config(app: &mut App) {
    if let Some(window) = &app.window {
        let scale = window.scale_factor();
        let physical = app.config.font_size * scale as f32;
        let font_path = crate::config::resolve_font_path(&app.config);
        app.renderer.reinit_font(&font_path, physical);
        let size = window.inner_size();
        super::lifecycle::handle_resize(app, size.width, size.height);
        app.request_redraw();
        log::info!("Font size: {}px ({}px physical)", app.config.font_size, physical);
    }
}
