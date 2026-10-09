use std::num::NonZeroU32;
use std::sync::Arc;

use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::WindowAttributes;

use crate::window::{Pane, PaneRect};

use super::App;
use super::shortcuts;

// ── Window creation ──

pub fn on_resumed(app: &mut App, event_loop: &ActiveEventLoop) {
    if app.window.is_some() { return; }

    let attrs = WindowAttributes::default()
        .with_title("rift")
        .with_transparent(true)
        .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));

    let window = Arc::new(event_loop.create_window(attrs).unwrap());
    // Enable IME (Pinyin, Kana, Hangul ...) so composed text arrives as Ime events.
    window.set_ime_allowed(true);

    // Set window icon (embedded at compile time)
    {
        let icon_bytes = include_bytes!("../../assets/icon.png");
        if let Some(icon) = load_icon_from_png(icon_bytes) {
            window.set_window_icon(Some(icon));
        }
    }

    // Native macOS window transparency
    if app.config.opacity < 1.0 {
        log::info!("Window opacity: {:.0}%", app.config.opacity * 100.0);
        #[cfg(target_os = "macos")]
        crate::platform::macos::apply_transparency(&window, app.config.opacity as f64);
    }
    let scale_factor = window.scale_factor();
    log::info!("Display scale_factor: {scale_factor}");

    let physical_font_size = app.config.font_size * scale_factor as f32;
    let font_path = crate::config::resolve_font_path(&app.config);
    app.renderer.reinit_font(&font_path, physical_font_size);

    let cw = app.renderer.cell_width() as f64 / scale_factor;
    let ch = app.renderer.cell_height() as f64 / scale_factor;
    let tab_bar_logical = app.tab_bar_height() as f64 / scale_factor;
    let pw = app.config.cols as f64 * cw;
    let ph = app.config.rows as f64 * ch + tab_bar_logical;

    let _ = window.request_inner_size(winit::dpi::LogicalSize::new(pw, ph));

    let context = softbuffer::Context::new(window.clone()).unwrap();
    let surface = softbuffer::Surface::new(&context, window.clone()).unwrap();

    app.menubar.init_for_nsapp();

    // Initialize wgpu GPU pipeline (if feature enabled)
    #[cfg(feature = "gpu")]
    {
        match crate::renderer::gpu::GpuPipeline::new(window.clone()) {
            Ok(pipeline) => {
                app.gpu_pipeline = Some(pipeline);
                app.renderer.shader.set_gpu_active(true);
                log::info!("wgpu GPU pipeline initialized");
            }
            Err(e) => {
                log::warn!("wgpu init failed: {e} — using softbuffer fallback");
            }
        }
    }

    app.menubar.set_gpu_effects(app.renderer.shader.gpu_active());

    app.window = Some(window);
    app.context = Some(context);
    app.surface = Some(surface);
}

// ── Redraw ──

/// True while the startup splash should be on screen.
pub fn startup_active(app: &App) -> bool {
    app.config.startup_animation
        && !app.startup_skipped
        && app.startup_time.elapsed().as_secs_f32() < crate::ui::splash::SPLASH_SECS
}

pub fn redraw(app: &mut App) {
    // DEC 2026 synchronized output: hold the frame while the app is mid-update
    // (the scheduler re-requests a redraw once the 150 ms safety timeout hits).
    if app.wm.active_pane().terminal.sync_pending() {
        return;
    }
    let splash = startup_active(app);
    let Some(window) = &app.window else { return };
    let Some(surface) = &mut app.surface else { return };

    let size = window.inner_size();
    let (width, height) = (size.width, size.height);
    if width == 0 || height == 0 { return; }

    let tbh = app.renderer.cell_height() + 16;

    let (Some(nz_w), Some(nz_h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) else {
        return;
    };
    if let Err(e) = surface.resize(nz_w, nz_h) {
        log::warn!("Surface resize failed: {e}");
        return;
    }

    let Ok(mut buffer) = surface.buffer_mut() else {
        log::warn!("Surface buffer_mut failed, skipping frame");
        return;
    };

    // Startup splash (<= 1.2s, skippable, `startup_animation = false` disables)
    if splash {
        let t = app.startup_time.elapsed().as_secs_f32();
        let (cw, ch) = (app.renderer.cell_width(), app.renderer.cell_height());
        let tk = crate::ui::kit::Tokens::new(&app.renderer.theme, cw, ch);
        let secondary = app.renderer.theme.palette[6];
        let footer = format!(
            "v{}  |  by {}  |  ko-fi.com/john5555555555",
            crate::config::VERSION, crate::config::AUTHOR
        );
        {
            let mut cx = crate::ui::kit::Ctx::new(&mut buffer, width as usize, height as usize, &mut app.renderer.font, &tk);
            crate::ui::splash::draw(&mut cx, t, secondary, &footer);
        }

        #[cfg(feature = "gpu")]
        if let Some(ref mut gpu) = app.gpu_pipeline {
            let effect = app.renderer.active_effect();
            let time = app.renderer.start_time.elapsed().as_secs_f32();
            gpu.render_frame(&buffer, width, height, effect, time);
            drop(buffer);
            if let Some(win) = &app.window { win.request_redraw(); }
            return;
        }
        if let Err(e) = buffer.present() { log::warn!("present: {e}"); }
        if let Some(win) = &app.window { win.request_redraw(); }
        return;
    }

    // Reserve space for HUD at bottom when visible
    let ch = app.renderer.cell_height();
    let hud_h = if app.hud_visible { ch * 3 + 20 } else { 0 };
    // Chat dock takes the far right edge; browser + terminal share the rest
    // (same rule as `App::terminal_width` / `App::browser_geometry`).
    let chat_w = app.chat.dock_w(width as usize);
    let avail_w = width as usize - chat_w;
    let blayout = app.webview.as_ref().filter(|wv| wv.visible).map(|_| {
        crate::network::browser::chrome::BrowserLayout::compute(
            avail_w, height as usize, tbh,
            app.renderer.cell_width(), app.renderer.cell_height(),
            window.scale_factor(), app.browser.ratio, app.webview_maximized,
        )
    });
    let content_w = blayout.map_or(avail_w, |l| l.terminal_w.min(avail_w));
    let content_area = PaneRect {
        x: 0,
        y: tbh,
        width: content_w,
        height: (height as usize).saturating_sub(tbh + hud_h),
    };

    // TimeWarp mode: render snapshot instead of live terminal
    if app.timewarp_browser.active {
        if let Some((grid, crow, ccol, age)) = app.timewarp.get(app.timewarp_browser.position) {
            // Fill background
            let bg = crate::ui::pack(app.renderer.theme.bg.0, app.renderer.theme.bg.1, app.renderer.theme.bg.2);
            buffer.fill(bg);

            // Render snapshot grid
            app.renderer.render_snapshot(grid, crow, ccol, &mut buffer, width as usize, height as usize, tbh);

            // Status bar at bottom
            let ch = app.renderer.cell_height();
            let pos = app.timewarp_browser.position;
            let total = app.timewarp.snapshot_count();
            let age_secs = age.as_secs_f32();
            let status = format!(
                " [TIME WARP]  Frame {}/{} | {:.1}s ago | \u{2190}\u{2192} navigate | Shift+Arrow x10 | Esc exit ",
                total - pos, total, age_secs
            );
            let bar_y = (height as usize).saturating_sub(ch + 8);
            let bar_bg = crate::ui::pack(40, 20, 60);
            for y in bar_y..height as usize {
                let off = y * width as usize;
                let end = (off + width as usize).min(buffer.len());
                if off < buffer.len() { buffer[off..end].fill(bar_bg); }
            }
            crate::ui::render_text(
                &mut buffer, width as usize, &mut app.renderer.font,
                &status, 8, bar_y + 4, (200, 160, 255),
            );
        }

        #[cfg(feature = "gpu")]
        if let Some(ref mut gpu) = app.gpu_pipeline {
            let effect = app.renderer.active_effect();
            let time = app.renderer.start_time.elapsed().as_secs_f32();
            gpu.render_frame(&buffer, width, height, effect, time);
            drop(buffer);
            return;
        }
        if let Err(e) = buffer.present() { log::warn!("present: {e}"); }
        return;
    }

    // Normal mode: Terminal content + tab bar
    let cmd_held = app.modifiers.super_key();
    let split_ui = crate::renderer::SplitUiState {
        hover_pane: app.hover_pane,
        hover_border: app.hover_border,
        dragging_border: app.dragging_border,
        zoomed: app.wm.active_tab().is_zoomed(),
    };
    app.renderer.chrome = app.mui.chrome(std::time::Instant::now());
    app.renderer.render_tabbed_with_cmd(&app.wm, content_area, &mut buffer, width, height, cmd_held, &app.blocks, split_ui);
    // Command-block chrome (gutter bars, chips, toolbar) on the output buffer
    crate::blocks_ui::draw::draw(&app.wm, &mut app.renderer, &app.blocks_ui, &mut buffer, width, height, content_area);
    let t_overlays = std::time::Instant::now();

    // Selection highlight
    if app.selection.active {
        let sel_rect = app.wm.pane_layouts(content_area)
            .into_iter()
            .find(|(_, _, active)| *active)
            .map(|(_, r, _)| r)
            .unwrap_or(content_area);
        let row_abs = crate::blocks_ui::view::view_abs_rows(&app.wm.active_pane().terminal);
        app.renderer.render_selection(
            &app.selection, &row_abs, &mut buffer,
            width as usize, height as usize, sel_rect,
        );
    }

    // IME: position the OS candidate window and draw inline preedit text
    // (While renaming a tab the preedit belongs to the rename field instead.)
    // (While the Cmd+K popover is open the preedit belongs to its input.)
    if app.mui.tabs.editor.is_none() && app.inline_ai.popover.is_none() && !app.chat.focused {
        super::ime::update_cursor_area(&app.wm, &app.renderer, window, &mut app.ime_area, content_area);
        super::ime::render_preedit(&app.wm, &mut app.renderer, &app.ime_preedit, &mut buffer, width as usize, content_area);
    }

    // Inline AI: Cmd+K popover, suggestion / hint bars, toast
    crate::ai::inline::draw(&app.wm, &mut app.renderer, &mut app.inline_ai, &mut buffer, width as usize, height as usize, content_area, &app.ime_preedit);
    if let Some(r) = app.inline_ai.ime_hint() {
        super::ime::set_cursor_area(window, &mut app.ime_area, (r.x, r.y, r.w, r.h));
    }

    // Search match highlights
    if app.search.visible && !app.search.matches.is_empty() {
        let cw = app.renderer.cell_width();
        let ch = app.renderer.cell_height();
        let w = width as usize;
        let terminal = &app.wm.active_pane().terminal;
        let accent = app.renderer.theme.cursor;
        // Map absolute line -> on-screen row within the active pane (fold-aware)
        let row_abs = crate::blocks_ui::view::view_abs_rows(terminal);
        let pane_rect = app.wm.pane_layouts(content_area)
            .into_iter()
            .find(|(_, _, active)| *active)
            .map(|(_, r, _)| r)
            .unwrap_or(content_area);
        let max_col = pane_rect.width / cw.max(1);

        for (i, m) in app.search.matches.iter().enumerate() {
            let Some(screen_row) = row_abs.iter().position(|r| *r == Some(m.row)) else { continue };
            let y0 = pane_rect.y + screen_row * ch;
            if y0 + ch > pane_rect.y + pane_rect.height { continue; }
            let is_current = i == app.search.current_match;

            for col in m.col_start..m.col_end.min(max_col) {
                let x0 = pane_rect.x + col * cw;
                // Highlight: current match = accent bg, others = dim accent bg
                let (hr, hg, hb) = if is_current { accent } else { crate::ui::dim(accent, 0.3) };
                let alpha: u32 = if is_current { 120 } else { 60 };
                let inv = 255 - alpha;
                for cy in 0..ch {
                    for cx in 0..cw {
                        let idx = (y0 + cy) * w + x0 + cx;
                        if idx < buffer.len() {
                            let px = buffer[idx];
                            let r = ((px >> 16) & 0xff) * inv / 255 + hr as u32 * alpha / 255;
                            let g = ((px >> 8) & 0xff) * inv / 255 + hg as u32 * alpha / 255;
                            let b = (px & 0xff) * inv / 255 + hb as u32 * alpha / 255;
                            buffer[idx] = (r.min(255) << 16) | (g.min(255) << 8) | b.min(255);
                        }
                    }
                }
            }
        }
    }

    // HUD — cyberpunk dashboard at bottom (UI kit: tokens + Ctx)
    if app.hud_visible {
        let ch = app.renderer.cell_height();
        let bar_h = ch * 3 + 20; // three rows + padding
        let h = height as usize;
        if h > bar_h + 4 {
            let tk = crate::ui::kit::Tokens::new(&app.renderer.theme, app.renderer.cell_width(), ch);
            let mut cx = crate::ui::kit::Ctx::new(&mut buffer, width as usize, h, &mut app.renderer.font, &tk);
            crate::tools::hud::draw(&mut cx, &app.hud, h - bar_h, bar_h);
        }
    }

    // Autocomplete popup
    if app.autocomplete.visible {
        let cw = app.renderer.cell_width();
        let ch = app.renderer.cell_height();
        let pane = app.wm.active_pane();
        let layouts = app.wm.pane_layouts(content_area);
        let active_idx = app.wm.active_tab().active;
        let pane_rect = layouts.iter()
            .find(|(idx, _, _)| *idx == active_idx)
            .map(|(_, r, _)| *r)
            .unwrap_or(content_area);
        let cx = pane_rect.x + pane.terminal.cursor_col * cw;
        let cy = pane_rect.y + pane.terminal.cursor_row * ch;
        app.autocomplete.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme, cx, cy,
        );
    }

    // Docked AI chat sidebar (right edge, below the tab bar / above the HUD).
    if let Some(area) = app.chat.dock_rect(width as usize, height as usize, tbh, hud_h) {
        app.chat.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
            area, &app.advisor, &app.ime_preedit,
        );
        // Keep the OS candidate window next to the composer caret.
        if app.chat.focused {
            if let Some((x, y, w, h)) = app.chat.take_ime_area() {
                window.set_ime_cursor_area(
                    winit::dpi::PhysicalPosition::new(x, y),
                    winit::dpi::PhysicalSize::new(w.max(1), h.max(1)),
                );
            }
        }
    }

    // Search overlay
    if app.search.visible {
        app.search.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }

    // Compare view overlay
    if app.compare_view.visible {
        app.compare_view.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }

    // Tool panels
    if app.file_manager.visible {
        app.file_manager.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }
    if app.git_panel.visible {
        app.git_panel.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }
    if app.cicd.visible {
        app.cicd.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }
    if app.heatmap.visible {
        app.heatmap.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }
    if app.docker.visible {
        app.docker.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }
    if app.network_monitor.visible {
        app.network_monitor.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }
    if app.process_tree.visible {
        app.process_tree.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }
    if app.system_info.visible {
        app.system_info.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }
    if app.port_dashboard.visible {
        app.port_dashboard.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }
    if app.regex_playground.visible {
        app.regex_playground.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }
    if app.error_notif.visible {
        app.error_notif.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }
    if app.teaching.enabled {
        let cw = app.renderer.cell_width();
        let ch = app.renderer.cell_height();
        let pane = app.wm.active_pane();
        let cx = pane.terminal.cursor_col * cw;
        let cy = tbh + pane.terminal.cursor_row * ch;
        app.teaching.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme, cx, cy,
        );
    }

    // Overlay panels (rendered back to front: prefs, ssh, webview, welcome on top)
    app.renderer.render_overlay(&app.prefs, &mut buffer, width, height);
    if app.ssh_dialog.visible {
        app.ssh_dialog.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }
    // Browser chrome (toolbar, divider) drawn above the native WebView's area
    if let (Some(layout), Some(wv)) = (&blayout, &app.webview) {
        crate::network::browser::render(
            &mut buffer, width as usize, height as usize, layout, &mut app.browser, wv,
            app.webview_maximized, &mut app.renderer.font, &app.renderer.theme,
        );
    }
    if app.webview_dialog.visible {
        app.webview_dialog.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }
    // Command Palette — Cmd+P. Rendered above the other tool overlays but
    // below the Welcome guide.
    if app.command_palette.visible {
        app.command_palette.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }
    if app.welcome.visible {
        app.welcome.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }

    // Observer summary overlay
    if let Some(ref summary) = app.observer_summary {
        crate::ui::observer_summary::render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme, summary, app.observer.enabled,
        );
    }

    // UI Gallery (Help > UI Gallery, or RIFT_UI_GALLERY=1)
    if crate::ui::kit::gallery::visible() {
        crate::ui::kit::gallery::render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }

    // Smart History Search — full-width bottom panel, drawn after every other overlay.
    if app.history.visible {
        app.history.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }

    // Loading spinner for async operations (the AI panel animates its own).
    if let Some(ref c) = app.ssh_connecting {
        let msg = format!("Connecting to {}@{}:{}", c.req.user, c.req.host, c.req.port);
        let elapsed = app.renderer.start_time.elapsed().as_secs_f32();
        let tk = crate::ui::kit::Tokens::new(&app.renderer.theme, app.renderer.font.cell_width, app.renderer.font.cell_height);
        let mut cx = crate::ui::kit::Ctx::new(&mut buffer, width as usize, height as usize, &mut app.renderer.font, &tk);
        cx.spinner_toast(&msg, elapsed);
    }

    // Inline tab rename field and the right-click menu sit above the overlays.
    if let Some(ed) = &app.mui.tabs.editor {
        let slot = crate::ui::tabbar::layout(width as usize, app.wm.tab_count(), tbh)
            .tabs
            .get(ed.idx)
            .copied();
        if let Some(slot) = slot {
            crate::ui::tabbar::render_editor(
                &mut buffer, width as usize, height as usize,
                &mut app.renderer.font, &app.renderer.theme,
                ed, &app.ime_preedit, slot, tbh,
            );
        }
    }
    if app.mui.menu.visible {
        app.mui.menu.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }

    // Preview-Then-Accept danger confirmation — drawn last, on top of every
    // other overlay and HUD element, so it can never be obscured.
    if app.exec_preview.visible {
        app.exec_preview.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }

    // Consent / paste / SSH host-key confirmations: topmost of all.
    if app.confirm.visible() {
        app.confirm.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
    }

    // Pixel-level opacity fallback (non-macOS only; macOS uses native NSWindow alpha)
    #[cfg(not(target_os = "macos"))]
    if app.renderer.opacity < 0.99 {
        let alpha = (app.renderer.opacity * 255.0) as u32;
        for px in buffer.iter_mut() {
            let r = ((*px >> 16) & 0xff) * alpha / 255;
            let g = ((*px >> 8) & 0xff) * alpha / 255;
            let b = (*px & 0xff) * alpha / 255;
            *px = (r << 16) | (g << 8) | b;
        }
    }

    let t_present = std::time::Instant::now();
    app.renderer.prof_add(crate::renderer::Phase::Overlays, t_present - t_overlays);

    // GPU rendering path: upload pixel buffer to GPU, present via wgpu
    #[cfg(feature = "gpu")]
    if let Some(ref mut gpu) = app.gpu_pipeline {
        let effect = app.renderer.active_effect();
        let time = app.renderer.start_time.elapsed().as_secs_f32();
        gpu.render_frame(&buffer, width, height, effect, time);
        drop(buffer);
        finish_frame(app, t_present);
        return;
    }

    // Softbuffer fallback
    if let Err(e) = buffer.present() {
        log::warn!("softbuffer present failed: {e}");
    }
    finish_frame(app, t_present);
}

/// Bookkeeping after a normal-mode frame was presented: profiling sample and
/// the timestamp the PTY-burst coalescing in `about_to_wait` paces against.
fn finish_frame(app: &mut App, t_present: std::time::Instant) {
    app.renderer.prof_add(crate::renderer::Phase::Present, t_present.elapsed());
    app.renderer.prof_frame_end();
    SCHED.with(|s| {
        let mut s = s.borrow_mut();
        s.last_redraw = Some(std::time::Instant::now());
        s.redraw_pending = false;
    });
}

// ── Resize ──

pub fn handle_resize(app: &mut App, width: u32, height: u32) {
    if width == 0 || height == 0 { return; }
    let cw = app.renderer.cell_width();
    let ch = app.renderer.cell_height();
    let hud_h = if app.hud_visible { ch * 3 + 20 } else { 0 };
    let effective_height = (height as usize).saturating_sub(hud_h) as u32;
    let effective_width = app.terminal_width(width as usize) as u32;
    if effective_width > 0 {
        app.wm.resize_all(cw, ch, effective_width, effective_height, app.tab_bar_height());
    }

    crate::network::browser::apply_bounds(app);
}

// ── Event loop idle ──

/// Target minimum spacing between redraws while PTY output is streaming
/// (~vsync at 120Hz); keystroke-driven redraws are not throttled.
const PTY_FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(8);
/// Safety-net poll while idle. Every producer (PTY/SSH reader, menu callback, AI
/// and HUD workers) now wakes the loop through the proxy, so this only bounds
/// the latency of anything that forgot to; idle wakeups stay at <= 1/s.
const IDLE_POLL_MS: u64 = 1000;

/// Redraw scheduling state for `about_to_wait` / `redraw`.
#[derive(Default)]
struct Sched {
    last_redraw: Option<std::time::Instant>,
    /// PTY output was processed but not yet drawn (throttled).
    redraw_pending: bool,
    /// Last cursor blink phase we requested a redraw for.
    last_blink_phase: Option<u32>,
}

thread_local! {
    static SCHED: std::cell::RefCell<Sched> = std::cell::RefCell::new(Sched::default());
}

pub fn about_to_wait(app: &mut App, event_loop: &ActiveEventLoop) {
    about_to_wait_inner(app, event_loop);
    // Parse budget exhausted: output is still queued. Come straight back (after
    // the pending input / redraw events were dispatched) instead of sleeping;
    // rendering is paced separately by PTY_FRAME_INTERVAL.
    if app.wm.has_backlog() {
        event_loop.set_control_flow(ControlFlow::Poll);
    }
}

fn about_to_wait_inner(app: &mut App, event_loop: &ActiveEventLoop) {
    // Animated effects redraw at ~60fps only while they are actually animating.
    let has_shader = app.renderer.shader.animating(app.renderer.start_time.elapsed().as_secs_f32());
    let in_startup = startup_active(app);

    // PTY reader thread calls proxy.send_event(()) which wakes the loop from Wait.
    // Use 16ms for animations, otherwise a short poll for responsiveness.
    let poll_ms = if has_shader || in_startup { 16 } else { IDLE_POLL_MS };
    let mut wake_at = std::time::Instant::now() + std::time::Duration::from_millis(poll_ms);
    event_loop.set_control_flow(ControlFlow::WaitUntil(wake_at));

    // During startup animation, just keep redrawing
    if in_startup {
        if let Some(w) = &app.window { w.request_redraw(); }
        return;
    }

    // Poll menu events
    while let Some(action) = app.menubar.poll_event() {
        shortcuts::handle_menu_action(app, action, event_loop);
    }

    // Browser: drain webview events (title/url/loading/focus)
    crate::network::browser::poll(app);

    // One-time cloud-AI consent prompt (an env API key alone never enables AI).
    crate::ai::consent::maybe_prompt(app);

    // Unknown SSH host keys: ask the user, show the fingerprint.
    if let Some(ref c) = app.ssh_connecting {
        let mut asked = Vec::new();
        while let Ok(p) = c.prompts.try_recv() {
            asked.push(p);
        }
        for p in asked {
            crate::ui::confirm::show_ssh_host_key(app, p);
            app.request_redraw();
        }
    }

    // Poll SSH connection
    if let Some(ref connecting) = app.ssh_connecting {
        if let Ok(result) = connecting.rx.try_recv() {
            let info = app.ssh_connecting.take().unwrap();
            match result {
                Ok(ssh) => {
                    let id = app.wm.alloc_pane_id();
                    let cols = app.wm.active_pane().terminal.cols;
                    let rows = app.wm.active_pane().terminal.rows;
                    let pane = Pane::from_ssh(id, cols, rows, ssh);
                    // Use alias as tab title if set, otherwise user@host:port
                    let title = if !info.req.alias.is_empty() {
                        info.req.alias.clone()
                    } else {
                        format!("{}@{}:{}", info.req.user, info.req.host, info.req.port)
                    };
                    app.wm.add_ssh_tab(pane, &title);
                    app.ssh_dialog.save_host(&info.req);
                    log::info!("SSH: connected as '{}', new tab opened", title);
                }
                Err(e) => {
                    log::error!("SSH: connection failed: {e}");
                    if e.contains("HOST IDENTIFICATION HAS CHANGED") {
                        crate::ui::confirm::show_notice(app, "SSH host key changed", &e);
                    }
                    app.ssh_dialog.error_msg = Some(format!("Failed: {e}"));
                    app.ssh_dialog.visible = true;
                }
            }
            app.update_title();
        }
    }

    // Chat: drain streamed deltas (the stream thread wakes the loop; the
    // chat kicks off the Advisor review itself when an answer completes).
    let chat_changed = crate::ai::chat::poll(app);
    let advisor_busy = app.advisor.is_loading();
    app.advisor.poll();
    let advisor_changed = advisor_busy && !app.advisor.is_loading();

    // Process PTY output from all panes
    let t_pty = std::time::Instant::now();
    let pty_changed = app.wm.process_all_output();
    if pty_changed {
        app.renderer.prof_add(crate::renderer::Phase::Pty, t_pty.elapsed());
    }
    if pty_changed {
        app.wm.sync_theme_colors(&app.renderer.theme);
        app.wm.flush_all_responses();
        app.update_title();

        // Feed tools with current terminal line
        {
            let term = &app.wm.active_pane().terminal;
            let row = term.cursor_row.min(term.grid.len().saturating_sub(1));
            let line: String = crate::terminal::grid::cells_text(&term.grid[row]);
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                // Observer
                if app.observer.enabled {
                    app.observer.on_output(trimmed);
                }
                // Block tracking (Warp-style command blocks)
                let scrollback_line = term.scrollback.len() + row;
                if !term.blocks.osc_seen() {
                    app.blocks.on_output_line(trimmed, scrollback_line);
                }
                // Error detection (cheap pre-check avoids building context every line)
                if app.error_detector.matches_any(trimmed) {
                    let recent: Vec<String> = term.grid.iter()
                        .map(|r| r.iter().map(|c| c.c).collect::<String>())
                        .collect();
                    if let Some(err) = app.error_detector.check_line(trimmed, scrollback_line, &recent) {
                        app.error_notif.show(err, 10);
                    }
                }
            }
        }

        // Handle OSC 52 clipboard requests + bell for all panes
        let mut osc52_toast: Option<&str> = None;
        for tab in &mut app.wm.tabs {
            for pane in tab.panes_mut() {
                // OSC 52 clipboard
                if let Some(req) = pane.terminal.clipboard_request.take() {
                    match req {
                        crate::terminal::ClipboardRequest::Set(b64_data) => {
                            let decoded = crate::tools::codec::base64_decode(&b64_data)
                                .ok()
                                .and_then(|bytes| String::from_utf8(bytes).ok());
                            if let Some(text) = decoded {
                                crate::window::selection::copy_to_clipboard(&text);
                                osc52_toast = Some("Clipboard set by program");
                            }
                        }
                        crate::terminal::ClipboardRequest::Query => {
                            if let Some(text) = crate::window::selection::paste_from_clipboard() {
                                let encoded = crate::tools::codec::base64_encode(text.as_bytes());
                                let response = format!("\x1b]52;c;{}\x07", encoded);
                                pane.terminal.response_queue.push(response.into_bytes());
                            }
                        }
                        crate::terminal::ClipboardRequest::Blocked { query, oversized } => {
                            // Toast once per kind per run; never spam.
                            let (flag, msg) = if query {
                                (&mut app.osc52_warned.0, "A program asked to read your clipboard \u{2014} blocked (set [security] osc52 in config.toml)")
                            } else if oversized {
                                (&mut app.osc52_warned.1, "A program tried to set a very large clipboard \u{2014} blocked")
                            } else {
                                (&mut app.osc52_warned.2, "A program tried to set your clipboard \u{2014} blocked (set [security] osc52 in config.toml)")
                            };
                            if !*flag {
                                *flag = true;
                                osc52_toast = Some(msg);
                            }
                        }
                    }
                }
                // Bell → request attention (dock bounce on macOS)
                if pane.terminal.bell {
                    pane.terminal.bell = false;
                    #[cfg(target_os = "macos")]
                    if let Some(w) = &app.window {
                        if !app.window_focused {
                            let _ = w.request_user_attention(Some(winit::window::UserAttentionType::Informational));
                        }
                    }
                }
            }
        }
        if let Some(msg) = osc52_toast {
            app.blocks_ui.show_toast(msg);
            app.needs_render = true;
        }

        // OSC 9 / OSC 777 desktop notifications (AI agents' "needs you" signal):
        // always drained; posted only when the user can't already see the pane.
        {
            let focused = app.window_focused;
            let active_tab = app.wm.active_tab;
            let mut posted = 0;
            for (ti, tab) in app.wm.tabs.iter_mut().enumerate() {
                let (zoomed, active_leaf) = (tab.is_zoomed(), tab.active);
                for (pi, pane) in tab.panes_mut().into_iter().enumerate() {
                    let notes = pane.terminal.take_notifications();
                    let visible = ti == active_tab && (!zoomed || pi == active_leaf);
                    if notes.is_empty() || (focused && visible) {
                        continue;
                    }
                    for n in notes {
                        if posted >= 3 {
                            break; // per-pass cap: a misbehaving program can't spam the desktop
                        }
                        posted += 1;
                        let title = if n.title.is_empty() { "rift" } else { n.title.as_str() };
                        crate::tools::notify::Notifier::send(title, &n.body);
                    }
                }
            }
        }

        // Capture snapshot for TimeWarp (throttled internally to 100ms)
        let pane = app.wm.active_pane();
        app.timewarp.capture(&pane.terminal.grid, pane.terminal.cursor_row, pane.terminal.cursor_col);
    }

    // Update HUD system info periodically
    app.hud.poll();
    if app.hud_visible && app.hud.needs_update() {
        app.hud.update();
    }

    // Scrollbar auto-hide, drag-select auto-scroll, tab-list bookkeeping.
    if let Some(t) = super::mouse::tick(app) {
        wake_at = wake_at.min(t);
        event_loop.set_control_flow(ControlFlow::WaitUntil(wake_at));
    }

    // Error notification auto-dismiss
    app.error_notif.tick();

    // Teaching mode poll LLM response
    app.teaching.poll();

    // Notifier: desktop notification when long command finishes
    if app.notifier.check(app.window_focused) {
        crate::tools::notify::Notifier::send("rift", "Command completed");
    }

    // Command blocks: running-block animation + toast expiry
    crate::blocks_ui::tick(app, &mut wake_at);

    // Inline AI: background fix / `#` results, failure detection, spinner
    crate::ai::inline::tick(app, &mut wake_at);

    // Request redraw if anything needs it
    let ssh_pending = app.ssh_connecting.is_some();
    // Streaming answers animate (caret, spinner); redraw while they run.
    let ai_waiting = app.chat.animating() || chat_changed || advisor_changed || (advisor_busy && app.chat.visible);
    let any_overlay = app.prefs.visible
        || app.confirm.visible()
        || app.welcome.visible
        || app.ssh_dialog.visible
        || app.autocomplete.visible
        || app.compare_view.visible
        || app.command_palette.visible
        || app.search.visible
        || app.file_manager.visible
        || app.git_panel.visible
        || app.cicd.visible
        || app.heatmap.visible
        || app.docker.visible
        || app.network_monitor.visible
        || app.process_tree.visible
        || app.system_info.visible
        || app.port_dashboard.visible
        || app.regex_playground.visible
        || app.error_notif.visible
        || app.history.visible
        || app.teaching.enabled;
    let timewarp_active = app.timewarp_browser.active;

    if has_shader || ssh_pending || ai_waiting || any_overlay || timewarp_active || app.hud_visible {
        app.request_redraw();
        return;
    }

    // Cursor blink (Bar/Underline, ~2Hz): only redraw on phase flips; the
    // renderer repaints just the cursor row.
    let blink_style = {
        let t = &app.wm.active_pane().terminal;
        t.cursor_visible
            && !t.is_scrolled_back()
            && t.cursor_style != crate::terminal::CursorStyle::Block
    };
    let mut blink_due = false;
    if blink_style {
        let elapsed = app.renderer.start_time.elapsed().as_secs_f32();
        let phase = (elapsed * 2.0) as u32;
        SCHED.with(|s| {
            let mut s = s.borrow_mut();
            if s.last_blink_phase != Some(phase) {
                s.last_blink_phase = Some(phase);
                blink_due = true;
            }
        });
        let next = app.renderer.start_time
            + std::time::Duration::from_secs_f32((phase + 1) as f32 / 2.0);
        wake_at = wake_at.min(next);
    }

    // PTY bursts: everything pending was just parsed above; draw at most once
    // per PTY_FRAME_INTERVAL while output keeps streaming.
    let (pending, last) = SCHED.with(|s| {
        let mut s = s.borrow_mut();
        if pty_changed { s.redraw_pending = true; }
        (s.redraw_pending, s.last_redraw)
    });
    let sync_hold = app.wm.active_pane().terminal.sync_remaining();
    if let Some(rem) = sync_hold.filter(|_| pending) {
        wake_at = wake_at.min(std::time::Instant::now() + rem);
    }
    if pending && sync_hold.is_none() {
        let ready_at = last.map(|t| t + PTY_FRAME_INTERVAL);
        match ready_at {
            Some(t) if t > std::time::Instant::now() => wake_at = wake_at.min(t),
            _ => {
                app.request_redraw();
                SCHED.with(|s| s.borrow_mut().redraw_pending = false);
                blink_due = false;
            }
        }
    }
    if blink_due {
        app.request_redraw();
    }
    event_loop.set_control_flow(ControlFlow::WaitUntil(wake_at));
}

fn load_icon_from_png(png_data: &[u8]) -> Option<winit::window::Icon> {
    // Minimal PNG decoder for the embedded icon (RGBA, non-interlaced)
    if png_data.len() < 33 || &png_data[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    // Read IHDR
    let width = u32::from_be_bytes([png_data[16], png_data[17], png_data[18], png_data[19]]);
    let height = u32::from_be_bytes([png_data[20], png_data[21], png_data[22], png_data[23]]);

    // Collect all IDAT chunks
    let mut idat_data = Vec::new();
    let mut pos = 8;
    while pos + 12 <= png_data.len() {
        let chunk_len = u32::from_be_bytes([png_data[pos], png_data[pos+1], png_data[pos+2], png_data[pos+3]]) as usize;
        let chunk_type = &png_data[pos+4..pos+8];
        if chunk_type == b"IDAT" && pos + 8 + chunk_len <= png_data.len() {
            idat_data.extend_from_slice(&png_data[pos+8..pos+8+chunk_len]);
        }
        pos += 12 + chunk_len;
    }

    // Decompress
    use std::io::Read;
    let mut decoder = flate2::read::ZlibDecoder::new(&idat_data[..]);
    let mut raw = Vec::new();
    decoder.read_to_end(&mut raw).ok()?;

    // Unfilter (filter type 0 = None for our generated icon)
    let stride = (width as usize) * 4;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height as usize {
        let offset = row * (stride + 1) + 1; // skip filter byte
        if offset + stride <= raw.len() {
            rgba.extend_from_slice(&raw[offset..offset + stride]);
        }
    }

    winit::window::Icon::from_rgba(rgba, width, height).ok()
}
