use winit::event::KeyEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};

use crate::input;
use crate::tools::exec_preview::ExecPreview;
use crate::ui::MenuAction;
use crate::effects::EffectKind;
use crate::network::{AuthMethod, SshConfig, SshPty, SshConnectRequest};

use crate::window::tab::PaneCmd;

use super::keymap::Action;
use super::{App, SshConnecting};
use super::overlays;

// ── Effects ──

/// Select (or clear) the visual effect, persist it to config.toml and repaint.
/// Effects the current renderer cannot draw (GPU-only without a GPU) are
/// refused with a log message instead of silently doing nothing.
pub fn set_effect(app: &mut App, kind: Option<EffectKind>) {
    if let Some(k) = kind {
        if !app.renderer.shader.supports(k) {
            log::warn!("Effect '{}' needs the GPU renderer (cargo build --features gpu); not applied", k.name());
            return;
        }
    }
    app.renderer.shader.set_effect(kind);
    app.config.effect = kind;
    crate::config::toml::save_config(&app.config);
    app.request_redraw();
}

/// Ctrl+Shift+= / Ctrl+Shift+-: change `effect_intensity` by `delta` and persist.
pub fn adjust_effect_intensity(app: &mut App, delta: f32) {
    let v = ((app.config.effect_intensity + delta) * 10.0).round() / 10.0;
    let v = v.clamp(0.0, 1.0);
    app.config.effect_intensity = v;
    app.renderer.shader.set_intensity(v);
    log::info!("Effect intensity: {v:.1}");
    crate::config::toml::save_config(&app.config);
    app.request_redraw();
}

// ── Key routing helpers ──

/// True while the foreground program owns the keyboard: alternate screen
/// (vim, tmux, less...), kitty keyboard protocol enabled, or mouse reporting on.
pub fn app_owns_keys(app: &App) -> bool {
    let t = &app.wm.active_pane().terminal;
    t.is_alt_screen() || t.kitty_keyboard_flags() != 0 || t.mouse_mode != crate::terminal::MouseMode::None
}

fn encode_opts(app: &App) -> input::EncodeOpts {
    let t = &app.wm.active_pane().terminal;
    input::EncodeOpts {
        app_cursor: t.app_cursor_keys,
        app_keypad: t.app_keypad(),
        kitty_flags: t.kitty_keyboard_flags(),
        shift_enter: app.config.input.shift_enter,
        option_as_meta: app.config.input.option_as_meta,
        mac: cfg!(target_os = "macos"),
    }
}

/// Kitty protocol event-type reporting: forward the release of a key whose
/// press went to the PTY.
pub fn handle_key_release(app: &mut App, event: &KeyEvent) {
    if !input::take_forwarded(event.physical_key) {
        return;
    }
    let opts = encode_opts(app);
    if opts.kitty_flags & 2 == 0 {
        return;
    }
    if let Some(bytes) = input::encode_key(event, app.modifiers, &opts) {
        if let Some(rec) = &mut app.recorder {
            rec.record_input(&bytes);
        }
        if app.broadcast {
            for pane in app.wm.active_tab_mut().panes_mut() {
                pane.write(&bytes);
            }
        } else {
            app.wm.active_pane_mut().write(&bytes);
        }
    }
}

/// Execute a keymap action. Returns true when the key was consumed.
fn run_action(app: &mut App, action: Action, event: &KeyEvent, event_loop: &ActiveEventLoop) -> bool {
    use super::keymap::Action::*;
    // One-shot actions must not auto-repeat (but the key is still ours).
    if event.repeat && !matches!(action, ZoomIn | ZoomOut | EffectIntensityUp | EffectIntensityDown) {
        return true;
    }
    match action {
        Copy => {
            if app.selection.active {
                if super::mouse::copy_selection(app) {
                    log::info!("Copied selection");
                }
                app.selection.clear();
                app.request_redraw();
            }
        }
        Paste => super::mouse::paste_clipboard(app),
        SelectAll => super::mouse::select_all(app),
        SplitRight => super::panes::run_pane_cmd(app, PaneCmd::SplitRight, event_loop),
        SplitDown => super::panes::run_pane_cmd(app, PaneCmd::SplitDown, event_loop),
        Search => handle_menu_action(app, MenuAction::Find, event_loop),
        CommandPalette => {
            super::overlays::open_command_palette(app);
            app.request_redraw();
        }
        HistorySearch => {
            app.history.toggle();
            app.request_redraw();
        }
        Autocomplete => trigger_autocomplete(app),
        NewTab => handle_menu_action(app, MenuAction::NewTab, event_loop),
        CloseTab => handle_menu_action(app, MenuAction::CloseTab, event_loop),
        PrevTab | NextTab => {
            if action == PrevTab { app.wm.prev_tab() } else { app.wm.next_tab() }
            sync_webview_for_tab(app);
            app.update_title();
            app.request_redraw();
        }
        Preferences => handle_menu_action(app, MenuAction::Preferences, event_loop),
        Welcome => handle_menu_action(app, MenuAction::Welcome, event_loop),
        Recording => handle_menu_action(app, MenuAction::Recording, event_loop),
        TimeWarp => handle_menu_action(app, MenuAction::TimeWarp, event_loop),
        Hud => handle_menu_action(app, MenuAction::HudToggle, event_loop),
        Ssh => handle_menu_action(app, MenuAction::SshConnect, event_loop),
        AiAssistant => handle_menu_action(app, MenuAction::AiAssistant, event_loop),
        Browser => handle_menu_action(app, MenuAction::WebView, event_loop),
        GitPanel => handle_menu_action(app, MenuAction::GitPanel, event_loop),
        CompareOutput => handle_menu_action(app, MenuAction::CompareOutput, event_loop),
        FileManager => handle_menu_action(app, MenuAction::FileManager, event_loop),
        Cicd => handle_menu_action(app, MenuAction::CicdPanel, event_loop),
        Teaching => handle_menu_action(app, MenuAction::TeachingMode, event_loop),
        Heatmap => handle_menu_action(app, MenuAction::Heatmap, event_loop),
        Docker => handle_menu_action(app, MenuAction::DockerPanel, event_loop),
        Regex => handle_menu_action(app, MenuAction::RegexPlayground, event_loop),
        SecretMask => handle_menu_action(app, MenuAction::SecretMask, event_loop),
        AuditLog => handle_menu_action(app, MenuAction::AuditLog, event_loop),
        Broadcast => handle_menu_action(app, MenuAction::BroadcastToggle, event_loop),
        Observer => {
            if !app.observer.enabled {
                app.observer.toggle();
            }
            app.observer_summary = Some(app.observer.generate_summary());
            app.request_redraw();
        }
        ZoomIn => zoom_font(app, 1.0),
        ZoomOut => zoom_font(app, -1.0),
        ZoomReset => reset_font(app),
        EffectCrt => set_effect(app, Some(EffectKind::Crt)),
        EffectGlitch => set_effect(app, Some(EffectKind::Glitch)),
        EffectNeon => set_effect(app, Some(EffectKind::Neon)),
        EffectMatrix => set_effect(app, Some(EffectKind::Matrix)),
        EffectAmber => set_effect(app, Some(EffectKind::Amber)),
        EffectHologram => set_effect(app, Some(EffectKind::Hologram)),
        EffectOff => set_effect(app, None),
        EffectIntensityUp => adjust_effect_intensity(app, 0.1),
        EffectIntensityDown => adjust_effect_intensity(app, -0.1),
    }
    true
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

    // 0. Inline AI popover (Cmd+K) owns the keyboard while open.
    if crate::ai::inline::on_key_modal(app, event) {
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

    // 1c. Inline AI: Cmd+K, Tab accepts a suggested fix, Esc dismisses/clears
    if crate::ai::inline::on_key(app, event) {
        return;
    }

    // 2. Rift keybindings (src/app/keymap.rs). Ctrl/Alt chords fall through to the
    //    program while it owns the keyboard (alt screen / kitty keyboard / mouse
    //    reporting) unless the action is Essential (clipboard, tabs).
    if let Some((action, chord)) = app.keymap.lookup_event(event, app.modifiers) {
        let guard = super::keymap::ACTIONS.iter().find(|d| d.action == action).map(|d| d.guard);
        let blocked = chord.is_program_chord()
            && guard == Some(super::keymap::Guard::Passthrough)
            && app_owns_keys(app);
        if !blocked && run_action(app, action, event, event_loop) {
            return;
        }
    }

    // 2a. Legacy Ctrl+Space autocomplete: only at a shell prompt (OSC 133) and never
    //     while a program owns the keyboard; everywhere else Ctrl+Space is NUL.
    if app.modifiers.control_key()
        && !app.modifiers.shift_key()
        && !app.modifiers.alt_key()
        && !app.modifiers.super_key()
        && matches!(event.logical_key, Key::Named(NamedKey::Space))
        && !app_owns_keys(app)
        && app.wm.active_pane().terminal.at_shell_prompt()
    {
        if !event.repeat {
            trigger_autocomplete(app);
        }
        return;
    }

    // Clear selection on any typing
    if app.selection.active && !app.modifiers.shift_key() && !app.modifiers.super_key() {
        if matches!(event.logical_key, Key::Character(_)) {
            app.selection.clear();
            app.request_redraw();
        }
    }

    // 2b. Pane management (focus / resize / swap / zoom / equalize / close)
    if super::panes::try_pane_shortcut(app, event, event_loop) {
        return;
    }

    // 5. Shift+PageUp/PageDown — scrollback navigation
    if app.modifiers.shift_key() && !app.modifiers.control_key() && !app.wm.active_pane().terminal.is_alt_screen() {
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
    let opts = encode_opts(app);
    if let Some(bytes) = input::encode_key(event, app.modifiers, &opts) {
        input::note_forwarded(event.physical_key, true);
        // The shell behind this pane exited ("[process exited N]" is on screen):
        // Enter closes the pane/tab instead of typing into a dead PTY.
        if (bytes == b"\r" || bytes == b"\x1b[13u") && app.wm.active_pane().exited.is_some() {
            if app.wm.close_current() {
                event_loop.exit();
            }
            app.update_title();
            return;
        }
        // Enter key: feed observer + block tracking (also CSI 13 u when the
        // program asked for every key as an escape code, so safety checks still run)
        if bytes == b"\r" || bytes == b"\x1b[13u" {
            // `# natural language` at the prompt: generate instead of running.
            if crate::ai::inline::on_enter(app) {
                return;
            }
            // The command being submitted: OSC 133 marked input when the shell
            // is integrated (never while a command runs, at a non-prompt, or
            // in the alternate screen), screen scraping only without OSC 133.
            let (cmd_opt, scrollback_line) = {
                let term = &app.wm.active_pane().terminal;
                let row = term.cursor_row.min(term.grid.len().saturating_sub(1));
                (term.pending_command_line(), term.scrollback.len() + row)
            };

            if let Some(cmd) = cmd_opt {
                // Preview-Then-Accept: dangerous commands are intercepted
                // here, before their trailing Enter ever reaches the PTY.
                // The command's characters were already streamed to the PTY
                // as the user typed them (this terminal has no local-echo
                // buffer of its own), so they're already sitting uncommitted
                // in the shell's line editor — confirming later just needs
                // to submit that buffer (see overlays::handle_exec_preview).
                if let Some(preview) = ExecPreview::check_for_enter(&cmd, app.wm.active_pane().terminal.cwd.as_deref()) {
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
                // Teaching mode: explains automatically only when opted in
                // (`teaching.on_submit`); otherwise on demand, see MenuAction::TeachingMode.
                if crate::ai::consent::allowed(app) {
                    let config = app.llm.config.clone();
                    app.teaching.on_submit(&cmd, &config);
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
        MenuAction::CrtEffect => set_effect(app, Some(EffectKind::Crt)),
        MenuAction::GlitchEffect => set_effect(app, Some(EffectKind::Glitch)),
        MenuAction::NeonEffect => set_effect(app, Some(EffectKind::Neon)),
        MenuAction::MatrixEffect => set_effect(app, Some(EffectKind::Matrix)),
        MenuAction::AmberEffect => set_effect(app, Some(EffectKind::Amber)),
        MenuAction::HologramEffect => set_effect(app, Some(EffectKind::Hologram)),
        MenuAction::NoEffect => set_effect(app, None),
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
        MenuAction::TeachingMode => {
            // While on, using the shortcut again with a command typed at the prompt
            // explains that command (before it runs); with an empty prompt it toggles off.
            let typed = if app.teaching.enabled { app.wm.active_pane().terminal.typed_input() } else { None };
            match typed.filter(|t| !t.trim().is_empty()) {
                Some(cmd) if crate::ai::consent::allowed(app) => {
                    let config = app.llm.config.clone();
                    app.teaching.explain_now(&cmd, &config);
                }
                Some(_) => {}
                None => app.teaching.toggle(),
            }
        }
        // AI
        MenuAction::AiAssistant => crate::ai::chat::toggle(app),
        MenuAction::ObserverMode => {
            app.observer.toggle();
            if app.observer.enabled {
                log::info!("Observer: enabled");
            } else {
                log::info!("Observer: disabled");
            }
        }
        MenuAction::AskAboutThis => crate::ai::inline::open(app),
        MenuAction::AutoFixToggle => {
            let on = !app.config.ai_auto_fix;
            crate::ai::inline::set_auto_fix(app, on);
        }
        MenuAction::NaturalLanguageToggle => {
            let on = !app.config.ai_nl_hash;
            crate::ai::inline::set_nl_hash(app, on);
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
    let (prompt_tx, prompt_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = SshPty::connect(config, cols, rows, proxy2, prompt_tx);
        let _ = tx.send(result.map_err(|e| e.to_string()));
        let _ = proxy.send_event(());
    });

    app.ssh_connecting = Some(SshConnecting { rx, req, prompts: prompt_rx });
}

// ── WebView ──

pub fn open_webview(app: &mut App, url: &str) {
    crate::network::browser::open(app, url);
}

// ── Private helpers ──

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
