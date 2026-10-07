use winit::event::KeyEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::input;
use crate::ui::MenuAction;
use crate::effects::{CrtParams, GlitchParams, MatrixParams, NeonParams, ShaderEffect,
                     AmberParams, HologramParams, PixelateParams, ThermalParams,
                     RaindropParams, VhsParams, GridParams, FilmGrainParams, InvertParams, DesaturateParams,
                     ChromaticParams, PulseParams, SnowParams, UnderwaterParams, NeonOutlineParams, ScanlineRgbParams};
use crate::network::{AuthMethod, SshConfig, SshPty, SshConnectRequest, WebViewPane};

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
    // 1. Overlay interception (priority order)
    if overlays::try_intercept(app, event) {
        return;
    }

    // 2. Cmd+C / Cmd+V (copy/paste) — macOS super key
    if app.modifiers.super_key() {
        let is_copy = matches!(&event.logical_key, Key::Character(s) if s.eq_ignore_ascii_case("c"));
        let is_paste = matches!(&event.logical_key, Key::Character(s) if s.eq_ignore_ascii_case("v"));
        let is_select_all = matches!(&event.logical_key, Key::Character(s) if s.eq_ignore_ascii_case("a"));

        if is_copy {
            if app.selection.active {
                let text = app.selection.extract_text(&app.wm.active_pane().terminal.grid);
                if !text.is_empty() {
                    crate::window::selection::copy_to_clipboard(&text);
                    log::info!("Copied {} chars", text.len());
                }
                app.selection.clear();
                app.request_redraw();
            }
            return;
        }
        if is_paste {
            if let Some(text) = crate::window::selection::paste_from_clipboard() {
                let pane = app.wm.active_pane_mut();
                if pane.terminal.bracketed_paste {
                    pane.write(b"\x1b[200~");
                    pane.write(text.as_bytes());
                    pane.write(b"\x1b[201~");
                } else {
                    pane.write(text.as_bytes());
                }
            }
            return;
        }
        if is_select_all {
            let terminal = &app.wm.active_pane().terminal;
            app.selection.start_at(0, 0);
            let last_row = terminal.rows.saturating_sub(1);
            let last_col = terminal.cols.saturating_sub(1);
            app.selection.extend_to(last_row, last_col);
            app.selection.finish();
            app.request_redraw();
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
                let (c, r) = pane_size(app);
                app.wm.split_h(c, r);
                resize_from_window(app);
                app.request_redraw();
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
        }
    }

    // 2c. Ctrl+Space → autocomplete (always Ctrl, not Cmd)
    if app.modifiers.control_key() && !app.modifiers.shift_key() {
        if let Key::Named(NamedKey::Space) = event.logical_key {
            trigger_autocomplete(app);
            return;
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

    // 6. Alt+Arrow — switch pane focus
    if app.modifiers.alt_key() {
        match event.logical_key {
            Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::ArrowDown) => {
                if !event.repeat { app.wm.focus_next_pane(); app.request_redraw(); }
                return;
            }
            Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::ArrowUp) => {
                if !event.repeat { app.wm.focus_prev_pane(); app.request_redraw(); }
                return;
            }
            _ => {}
        }
    }

    // 7. Normal input → active pane (or broadcast to all panes)
    let app_cursor = app.wm.active_pane().terminal.app_cursor_keys;
    if let Some(bytes) = input::encode_key(event, app.modifiers, app_cursor) {
        if let Some(rec) = &mut app.recorder {
            rec.record_input(&bytes);
        }
        if app.broadcast {
            let tab = app.wm.active_tab_mut();
            for pane in &mut tab.panes {
                pane.write(&bytes);
            }
        } else {
            app.wm.active_pane_mut().write(&bytes);
        }
    }
}

// ── Mouse ──

pub fn handle_click(app: &mut App, event_loop: &ActiveEventLoop) {
    let x = app.cursor_x;
    let y = app.cursor_y;
    let tbh = app.tab_bar_height();

    if y < tbh {
        handle_tab_bar_click(app, x, y, event_loop);
    }
    app.request_redraw();
}

// ── Menu bar ──

pub fn handle_menu_action(app: &mut App, action: MenuAction, event_loop: &ActiveEventLoop) {
    match action {
        MenuAction::NewTab => {
            let (c, r) = pane_size(app);
            app.wm.new_tab(c, r);
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
        MenuAction::SplitH => {
            let (c, r) = pane_size(app);
            app.wm.split_h(c, r);
            resize_from_window(app);
        }
        MenuAction::SplitV => {
            let (c, r) = pane_size(app);
            app.wm.split_v(c, r);
            resize_from_window(app);
        }
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
        MenuAction::WebView => app.webview_dialog.toggle(),
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
        MenuAction::NetworkMonitor => {} // TODO: integrate
        MenuAction::ProcessTree => {}    // TODO: integrate
        MenuAction::SystemInfo => {}     // TODO: integrate
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
            let panes = &app.wm.active_tab().panes;
            app.compare_view.collect_and_compare(panes);
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
    if let Some(window) = &app.window {
        let size = window.inner_size();
        let scale = window.scale_factor();
        let w = size.width as f64 / scale / 2.0;
        let h = (size.height as f64 / scale).max(1.0);

        if let Some(wv) = &mut app.webview {
            wv.navigate(url);
            wv.set_visible(true);
        } else {
            match WebViewPane::new(window, url, w as i32, 0, w as u32, h as u32) {
                Ok(wv) => {
                    log::info!("WebView opened: {url}");
                    app.webview = Some(wv);
                }
                Err(e) => log::error!("WebView failed: {e}"),
            }
        }
    }
}

// ── Private helpers ──

fn handle_mod_shift(app: &mut App, key: &str, event_loop: &ActiveEventLoop) -> bool {
    match key {
        "r" | "R" => { app.toggle_recording(); true }
        "t" | "T" => {
            let (c, r) = pane_size(app);
            app.wm.new_tab(c, r);
            app.update_title();
            true
        }
        "w" | "W" => {
            if app.wm.close_current() { event_loop.exit(); }
            app.update_title();
            true
        }
        "d" | "D" => {
            let (c, r) = pane_size(app);
            app.wm.split_v(c, r);  // Cmd+Shift+D = horizontal split (up/down)
            resize_from_window(app);
            true
        }
        "-" | "_" => {
            let (c, r) = pane_size(app);
            app.wm.split_v(c, r);
            resize_from_window(app);
            true
        }
        // Effects moved to Ctrl+Shift+1-8 (avoids macOS screenshot conflict)
        "," | "<" => { app.prefs.toggle(); true }
        "[" | "{" => { app.wm.prev_tab(); app.update_title(); true }
        "]" | "}" => { app.wm.next_tab(); app.update_title(); true }
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
        "b" | "B" => {
            if let Some(wv) = &mut app.webview {
                let new_vis = !wv.visible;
                wv.set_visible(new_vis);
            } else {
                app.webview_dialog.toggle();
            }
            true
        }
        "g" | "G" => {
            app.git_panel.toggle();
            true
        }
        "k" | "K" => {
            let panes = &app.wm.active_tab().panes;
            app.compare_view.collect_and_compare(panes);
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

fn handle_tab_bar_click(app: &mut App, x: usize, _y: usize, event_loop: &ActiveEventLoop) {
    let Some(window) = &app.window else { return };
    let buf_width = window.inner_size().width as usize;
    let cw = app.renderer.cell_width();
    let tab_count = app.wm.tab_count().max(1);
    let tab_w = (buf_width / tab_count).min(300).max(60);

    let tabs_end = tab_count * tab_w;
    if x >= tabs_end {
        let (c, r) = pane_size(app);
        app.wm.new_tab(c, r);
        app.update_title();
        return;
    }

    let tab_idx = x / tab_w;
    if tab_idx >= tab_count { return; }

    let tab_right = (tab_idx + 1) * tab_w;
    let close_zone = tab_right.saturating_sub(cw * 2 + 4);
    if tab_count > 1 && x >= close_zone {
        if app.wm.close_tab_at(tab_idx) {
            event_loop.exit();
        }
        app.update_title();
        return;
    }

    app.wm.switch_tab(tab_idx);
    app.update_title();
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

fn resize_from_window(app: &mut App) {
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

fn reinit_font_from_config(app: &mut App) {
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
