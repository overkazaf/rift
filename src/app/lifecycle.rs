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
                log::info!("wgpu GPU pipeline initialized");
            }
            Err(e) => {
                log::warn!("wgpu init failed: {e} — using softbuffer fallback");
            }
        }
    }

    app.window = Some(window);
    app.context = Some(context);
    app.surface = Some(surface);
}

// ── Redraw ──

pub fn redraw(app: &mut App) {
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

    // Startup animation (first 2.5 seconds)
    let startup_elapsed = app.startup_time.elapsed().as_secs_f32();
    if startup_elapsed < 2.5 {
        let w = width as usize;
        let h = height as usize;
        let bg = app.renderer.theme.bg;
        let accent = app.renderer.theme.cursor;

        let bg_px = crate::ui::pack(bg.0, bg.1, bg.2);
        buffer.fill(bg_px);

        // Fade: 0→1 in 1.0s, hold 1.0s, 1→0 in last 0.5s
        let alpha = if startup_elapsed < 1.0 {
            startup_elapsed / 1.0
        } else if startup_elapsed > 2.0 {
            1.0 - (startup_elapsed - 2.0) / 0.5
        } else {
            1.0
        }.clamp(0.0, 1.0);

        let ch = app.renderer.cell_height();
        let cw = app.renderer.cell_width();

        let logo = "R I F T";
        let subtitle = "dimension rift terminal";
        let version = format!("v{}", crate::config::VERSION);

        let logo_x = w.saturating_sub(logo.len() * cw) / 2;
        let logo_y = h / 2 - ch * 2;
        let sub_x = w.saturating_sub(subtitle.len() * cw) / 2;
        let sub_y = logo_y + ch * 2;
        let ver_x = w.saturating_sub(version.len() * cw) / 2;
        let ver_y = sub_y + ch + ch / 2;

        let logo_color = crate::ui::dim(accent, alpha);
        let sub_color = crate::ui::dim(app.renderer.theme.fg, alpha * 0.5);
        let ver_color = crate::ui::dim(app.renderer.theme.fg, alpha * 0.3);

        crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, logo, logo_x, logo_y, logo_color);
        crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, subtitle, sub_x, sub_y, sub_color);
        crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, &version, ver_x, ver_y, ver_color);

        // Accent line under logo
        let line_w = logo.len() * cw + 40;
        let line_x = w.saturating_sub(line_w) / 2;
        let line_y = logo_y + ch + 4;
        let line_px = crate::ui::pack_rgb(crate::ui::dim(accent, alpha * 0.6));
        for x in line_x..(line_x + line_w).min(w) {
            crate::ui::set_px(&mut buffer, w, line_y, x, line_px);
        }

        #[cfg(feature = "gpu")]
        if let Some(ref mut gpu) = app.gpu_pipeline {
            let effect = app.renderer.shader.active_effect();
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
    let content_area = PaneRect {
        x: 0,
        y: tbh,
        width: width as usize,
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
            let effect = app.renderer.shader.active_effect();
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
    app.renderer.render_tabbed_with_cmd(&app.wm, content_area, &mut buffer, width, height, cmd_held);

    // Selection highlight
    if app.selection.active {
        app.renderer.render_selection(
            &app.selection, &mut buffer,
            width as usize, height as usize, tbh,
        );
    }

    // Search match highlights
    if app.search.visible && !app.search.matches.is_empty() {
        let cw = app.renderer.cell_width();
        let ch = app.renderer.cell_height();
        let w = width as usize;
        let terminal = &app.wm.active_pane().terminal;
        let sb_len = terminal.scrollback.len();
        let offset = terminal.scroll_offset;
        let accent = app.renderer.theme.cursor;

        for (i, m) in app.search.matches.iter().enumerate() {
            // Convert scrollback-absolute row to visible row
            let visible_start = sb_len.saturating_sub(offset);
            let visible_end = visible_start + terminal.rows;
            if m.row < visible_start || m.row >= visible_end { continue; }

            let screen_row = m.row - visible_start;
            let y0 = tbh + screen_row * ch;
            let is_current = i == app.search.current_match;

            for col in m.col_start..m.col_end {
                let x0 = col * cw;
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

    // HUD — cyberpunk dashboard at bottom
    if app.hud_visible {
        let cw = app.renderer.cell_width();
        let ch = app.renderer.cell_height();
        let w = width as usize;
        let h = height as usize;
        let bar_h = ch * 3 + 20; // three rows + padding
        if h > bar_h + 4 {
            let y_start = h - bar_h;
            let accent = app.renderer.theme.cursor;

            // 1. Gradient background (darker at bottom)
            for y in y_start..h {
                let progress = (y - y_start) as f32 / bar_h as f32;
                let dim_factor = 0.12 + progress * 0.08;
                for x in 0..w {
                    let idx = y * w + x;
                    if idx < buffer.len() {
                        let px = buffer[idx];
                        let r = (((px >> 16) & 0xff) as f32 * dim_factor) as u32;
                        let g = (((px >> 8) & 0xff) as f32 * dim_factor) as u32;
                        let b = ((px & 0xff) as f32 * dim_factor) as u32;
                        buffer[idx] = (r << 16) | (g << 8) | b;
                    }
                }
            }

            // 2. Glowing top border (accent + glow)
            let accent_px = crate::ui::pack(accent.0, accent.1, accent.2);
            let glow = crate::ui::dim(accent, 0.3);
            let glow_px = crate::ui::pack(glow.0, glow.1, glow.2);
            for x in 0..w {
                let i1 = y_start * w + x;
                let i2 = (y_start + 1) * w + x;
                if i1 < buffer.len() { buffer[i1] = accent_px; }
                if i2 < buffer.len() { buffer[i2] = glow_px; }
            }

            let data = app.hud.data();
            let blue: (u8,u8,u8) = (137, 180, 250);
            let yellow: (u8,u8,u8) = (249, 226, 175);
            let cyan: (u8,u8,u8) = (148, 226, 213);
            let pink: (u8,u8,u8) = (245, 194, 231);
            let dim_c: (u8,u8,u8) = (108, 112, 134);
            let accent_rgb: (u8,u8,u8) = (accent.0, accent.1, accent.2);

            // === Row 1: brand + system info ===
            let r1y = y_start + 5;
            let mut tx = 12;

            // ◆ RIFT
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, "\u{25C6} RIFT", tx, r1y, accent_rgb);
            tx += 7 * cw;

            // Separator
            hud_separator(&mut buffer, w, tx, y_start + 4, y_start + 4 + ch, dim_c);
            tx += cw;

            // user@host
            let ident = format!("{}@{}", data.user, data.host);
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, &ident, tx, r1y, cyan);
            tx += (ident.len() + 1) * cw;

            hud_separator(&mut buffer, w, tx, y_start + 4, y_start + 4 + ch, dim_c);
            tx += cw;

            // shell
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, &data.shell, tx, r1y, pink);
            tx += (data.shell.len() + 1) * cw;

            hud_separator(&mut buffer, w, tx, y_start + 4, y_start + 4 + ch, dim_c);
            tx += cw;

            // uptime
            let up_label = format!("up {}", data.uptime);
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, &up_label, tx, r1y, dim_c);

            // Time (right-aligned, row 1)
            let time_x = w.saturating_sub((data.time.len() + 2) * cw);
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, &data.time, time_x, r1y, blue);

            // === Row 2: metrics with pixel progress bars ===
            let r2y = y_start + 5 + ch + 4;
            let bar_px_w = 12 * cw; // progress bar width in pixels
            let bar_px_h = (ch / 2).max(6); // half cell height
            let bar_y_center = r2y + (ch - bar_px_h) / 2; // vertically centered
            let mut tx = 12;

            // MEM label
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, "MEM", tx, r2y, blue);
            tx += 4 * cw;

            // MEM progress bar (pixel-level gradient)
            let mem_frac = (data.mem_pct / 100.0).clamp(0.0, 1.0);
            let mem_low: (u8,u8,u8) = (93, 228, 167);   // green
            let mem_high: (u8,u8,u8) = (243, 139, 168);  // red
            let bar_bg: (u8,u8,u8) = (30, 32, 48);
            hud_progress_bar(&mut buffer, w, tx, bar_y_center, bar_px_w, bar_px_h, mem_frac, mem_low, mem_high, bar_bg);
            tx += bar_px_w + cw;

            // MEM value
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, &data.mem_label, tx, r2y, yellow);
            tx += (data.mem_label.len() + 2) * cw;

            hud_separator(&mut buffer, w, tx, r2y, r2y + ch, dim_c);
            tx += cw + cw / 2;

            // CPU label
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, "CPU", tx, r2y, blue);
            tx += 4 * cw;

            // CPU progress bar
            let cpu_frac = (data.cpu_pct / 100.0).clamp(0.0, 1.0);
            hud_progress_bar(&mut buffer, w, tx, bar_y_center, bar_px_w, bar_px_h, cpu_frac, mem_low, mem_high, bar_bg);
            tx += bar_px_w + cw;

            // CPU value
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, &data.cpu_label, tx, r2y, yellow);

            // === Row 3: extra system info ===
            let r3y = r2y + ch + 4;
            let mut tx = 12;
            let very_dim: (u8,u8,u8) = (80, 84, 108);

            // OS + arch
            let os_info = format!("{}/{}", data.os, data.arch);
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, &os_info, tx, r3y, dim_c);
            tx += (os_info.len() + 1) * cw;

            hud_separator(&mut buffer, w, tx, r3y, r3y + ch, very_dim);
            tx += cw;

            // Git branch
            let git_label = format!("\u{2387} {}", data.git_branch); // ⎇
            let git_color = if data.git_branch == "-" { dim_c } else { (166, 227, 161) }; // green if on branch
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, &git_label, tx, r3y, git_color);
            tx += (git_label.chars().count() + 1) * cw;

            hud_separator(&mut buffer, w, tx, r3y, r3y + ch, very_dim);
            tx += cw;

            // CWD
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, &data.cwd_short, tx, r3y, cyan);
            tx += (data.cwd_short.chars().count() + 1) * cw;

            hud_separator(&mut buffer, w, tx, r3y, r3y + ch, very_dim);
            tx += cw;

            // Disk
            let disk_frac = (data.disk_pct / 100.0).clamp(0.0, 1.0);
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, "DSK", tx, r3y, blue);
            tx += 4 * cw;
            let disk_bar_w = 8 * cw;
            hud_progress_bar(&mut buffer, w, tx, r3y + (ch - bar_px_h) / 2, disk_bar_w, bar_px_h, disk_frac, mem_low, mem_high, bar_bg);
            tx += disk_bar_w + cw;
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, &data.disk_label, tx, r3y, yellow);
            tx += (data.disk_label.len() + 1) * cw;

            hud_separator(&mut buffer, w, tx, r3y, r3y + ch, very_dim);
            tx += cw;

            // Load average
            let load_label = format!("LOAD {}", data.load_avg);
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, &load_label, tx, r3y, dim_c);

            // PID + version (right-aligned, row 3)
            let right_label = format!("PID {} | {}", data.pid, data.rust_version);
            let right_x = w.saturating_sub((right_label.len() + 2) * cw);
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, &right_label, right_x, r3y, very_dim);
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

    // AI panel
    if app.ai_panel.visible {
        app.ai_panel.render(
            &mut buffer, width as usize, height as usize,
            &mut app.renderer.font, &app.renderer.theme,
        );
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
    if app.webview_dialog.visible {
        app.webview_dialog.render(
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
        let w = width as usize;
        let h = height as usize;
        let cw = app.renderer.cell_width();
        let ch = app.renderer.cell_height();
        let theme = &app.renderer.theme;

        // Dim background
        for px in buffer.iter_mut() {
            let r = ((*px >> 16) & 0xff) / 3;
            let g = ((*px >> 8) & 0xff) / 3;
            let b = (*px & 0xff) / 3;
            *px = (r << 16) | (g << 8) | b;
        }

        let lines: Vec<&str> = summary.lines().collect();
        let panel_w = (50 * cw).min(w - 40);
        let panel_h = ((lines.len() + 4) * (ch + 3) + 40).min(h - 40);
        let px0 = (w - panel_w) / 2;
        let py0 = (h - panel_h) / 2;

        let bg = crate::ui::lighten(theme.bg, 6);
        crate::ui::fill_rect(&mut buffer, w, px0, py0, panel_w, panel_h, crate::ui::pack_rgb(bg));
        crate::ui::draw_border(&mut buffer, w, px0, py0, panel_w, panel_h,
            crate::ui::pack_rgb(crate::ui::dim(theme.cursor, 0.4)));

        let mut ty = py0 + 12;
        let enabled_text = if app.observer.enabled { "ON" } else { "OFF" };
        let title = format!("AI Observer [{}]", enabled_text);
        crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, &title, px0 + 16, ty, theme.cursor);
        ty += ch + 8;

        let max_chars = (panel_w - 32) / cw;
        for line in &lines {
            if ty + ch >= py0 + panel_h - ch - 10 { break; }
            let color = if line.starts_with("##") { theme.cursor }
                else if line.starts_with("  ") { crate::ui::dim(theme.fg, 0.7) }
                else { theme.fg };
            crate::ui::render_text(&mut buffer, w, &mut app.renderer.font,
                crate::ui::trunc(line, max_chars), px0 + 16, ty, color);
            ty += ch + 3;
        }

        let help = "Esc: close  V: toggle observer";
        crate::ui::render_text(&mut buffer, w, &mut app.renderer.font, help,
            px0 + 16, py0 + panel_h - ch - 10, crate::ui::dim(theme.fg, 0.3));
    }

    // Loading spinners for async operations
    {
        let w = width as usize;
        let h = height as usize;
        let ch = app.renderer.cell_height();
        let cw = app.renderer.cell_width();
        let elapsed = app.renderer.start_time.elapsed().as_secs_f32();
        let accent = app.renderer.theme.cursor;
        let fg = app.renderer.theme.fg;

        // SSH connecting spinner (bottom center)
        if app.ssh_connecting.is_some() {
            let msg = if let Some(ref c) = app.ssh_connecting {
                format!("Connecting to {}@{}:{}", c.req.user, c.req.host, c.req.port)
            } else { String::new() };

            let bar_w = (msg.len() + 6) * cw;
            let bar_x = (w.saturating_sub(bar_w)) / 2;
            let bar_y = h.saturating_sub(ch * 2 + 20);

            // Dark backdrop bar
            let bar_bg = crate::ui::pack(20, 22, 35);
            crate::ui::fill_rect(&mut buffer, w, bar_x.saturating_sub(8), bar_y.saturating_sub(4), bar_w + 16, ch + 8, bar_bg);
            let border = crate::ui::dim(accent, 0.4);
            crate::ui::draw_border(&mut buffer, w, bar_x.saturating_sub(8), bar_y.saturating_sub(4), bar_w + 16, ch + 8, crate::ui::pack_rgb(border));

            crate::ui::render_spinner(&mut buffer, w, &mut app.renderer.font, bar_x, bar_y, &msg, accent, fg, elapsed);
        }

        // AI thinking spinner (in AI panel area)
        if app.ai_panel.visible && app.ai_panel.loading {
            let panel_y = h.saturating_sub(h / 3);
            let spinner_y = panel_y + ch * 3 + 16;
            if spinner_y + ch < h {
                crate::ui::render_spinner(&mut buffer, w, &mut app.renderer.font, 24, spinner_y, "Thinking", accent, crate::ui::dim(fg, 0.6), elapsed);
                crate::ui::render_dots(&mut buffer, w, &mut app.renderer.font, 24 + 12 * cw, spinner_y, accent, elapsed);
            }
        }
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

    // GPU rendering path: upload pixel buffer to GPU, present via wgpu
    #[cfg(feature = "gpu")]
    if let Some(ref mut gpu) = app.gpu_pipeline {
        let effect = app.renderer.shader.active_effect();
        let time = app.renderer.start_time.elapsed().as_secs_f32();
        gpu.render_frame(&buffer, width, height, effect, time);
        drop(buffer);
        return;
    }

    // Softbuffer fallback
    if let Err(e) = buffer.present() {
        log::warn!("softbuffer present failed: {e}");
    }
}

// ── HUD pixel helpers ──

fn hud_separator(buffer: &mut [u32], buf_w: usize, x: usize, y_top: usize, y_bot: usize, color: (u8,u8,u8)) {
    let px = crate::ui::pack(color.0, color.1, color.2);
    for y in y_top..y_bot {
        let idx = y * buf_w + x;
        if idx < buffer.len() { buffer[idx] = px; }
    }
}

fn hud_progress_bar(
    buffer: &mut [u32], buf_w: usize,
    x: usize, y: usize, w: usize, h: usize,
    pct: f32,
    color_low: (u8,u8,u8), color_high: (u8,u8,u8), bg: (u8,u8,u8),
) {
    let filled = (w as f32 * pct) as usize;
    let bg_px = crate::ui::pack(bg.0, bg.1, bg.2);
    for row in y..y + h {
        for col in x..x + w {
            let idx = row * buf_w + col;
            if idx >= buffer.len() { continue; }
            if col - x < filled {
                let t = (col - x) as f32 / w as f32;
                let r = (color_low.0 as f32 * (1.0 - t) + color_high.0 as f32 * t) as u8;
                let g = (color_low.1 as f32 * (1.0 - t) + color_high.1 as f32 * t) as u8;
                let b = (color_low.2 as f32 * (1.0 - t) + color_high.2 as f32 * t) as u8;
                buffer[idx] = crate::ui::pack(r, g, b);
            } else {
                buffer[idx] = bg_px;
            }
        }
    }
    // Subtle border
    let border = crate::ui::dim(color_low, 0.3);
    let bp = crate::ui::pack(border.0, border.1, border.2);
    for col in x..x + w {
        let top = y * buf_w + col;
        let bot = (y + h.saturating_sub(1)) * buf_w + col;
        if top < buffer.len() { buffer[top] = bp; }
        if bot < buffer.len() { buffer[bot] = bp; }
    }
    for row in y..y + h {
        let left = row * buf_w + x;
        let right = row * buf_w + x + w.saturating_sub(1);
        if left < buffer.len() { buffer[left] = bp; }
        if right < buffer.len() { buffer[right] = bp; }
    }
}

// ── Resize ──

pub fn handle_resize(app: &mut App, width: u32, height: u32) {
    if width == 0 || height == 0 { return; }
    let cw = app.renderer.cell_width();
    let ch = app.renderer.cell_height();
    let hud_h = if app.hud_visible { ch * 3 + 20 } else { 0 };
    let effective_height = (height as usize).saturating_sub(hud_h) as u32;
    app.wm.resize_all(cw, ch, width, effective_height, app.tab_bar_height());

    if let (Some(wv), Some(window)) = (&app.webview, &app.window) {
        if wv.visible {
            let scale = window.scale_factor();
            let lw = width as f64 / scale / 2.0;
            let lh = height as f64 / scale;
            wv.set_bounds(lw as i32, 0, lw as u32, lh as u32);
        }
    }
}

// ── Event loop idle ──

pub fn about_to_wait(app: &mut App, event_loop: &ActiveEventLoop) {
    let has_shader = app.renderer.shader.has_effect();
    let in_startup = app.startup_time.elapsed().as_secs_f32() < 2.5;

    if has_shader || in_startup {
        // Cap at ~60fps to avoid CPU spin (Poll = 100% CPU)
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            std::time::Instant::now() + std::time::Duration::from_millis(16)
        ));
    } else {
        event_loop.set_control_flow(ControlFlow::Wait);
    }

    // During startup animation, just keep redrawing
    if in_startup {
        if let Some(w) = &app.window { w.request_redraw(); }
        return;
    }

    // Poll menu events
    while let Some(action) = app.menubar.poll_event() {
        shortcuts::handle_menu_action(app, action, event_loop);
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
                    app.ssh_dialog.error_msg = Some(format!("Failed: {e}"));
                    app.ssh_dialog.visible = true;
                }
            }
            app.update_title();
        }
    }

    // Poll AI response
    app.ai_panel.poll();

    // Process PTY output from all panes
    let pty_changed = app.wm.process_all_output();
    if pty_changed {
        app.wm.flush_all_responses();
        app.update_title();

        // Feed Observer with the current terminal line (output tracking)
        if app.observer.enabled {
            let term = &app.wm.active_pane().terminal;
            let row = term.cursor_row.min(term.grid.len().saturating_sub(1));
            let line: String = term.grid[row].iter().map(|c| c.c).collect();
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                app.observer.on_output(trimmed);
            }
        }

        // Handle OSC 52 clipboard requests + bell for all panes
        for tab in &mut app.wm.tabs {
            for pane in &mut tab.panes {
                // OSC 52 clipboard
                if let Some(req) = pane.terminal.clipboard_request.take() {
                    match req {
                        crate::terminal::ClipboardRequest::Set(b64_data) => {
                            if let Ok(bytes) = crate::tools::codec::base64_decode(&b64_data) {
                                if let Ok(text) = String::from_utf8(bytes) {
                                    crate::window::selection::copy_to_clipboard(&text);
                                }
                            }
                        }
                        crate::terminal::ClipboardRequest::Query => {
                            if let Some(text) = crate::window::selection::paste_from_clipboard() {
                                let encoded = crate::tools::codec::base64_encode(text.as_bytes());
                                let response = format!("\x1b]52;c;{}\x07", encoded);
                                pane.terminal.response_queue.push(response.into_bytes());
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

        // Capture snapshot for TimeWarp (throttled internally to 100ms)
        let pane = app.wm.active_pane();
        app.timewarp.capture(&pane.terminal.grid, pane.terminal.cursor_row, pane.terminal.cursor_col);
    }

    // Update HUD system info periodically
    if app.hud_visible && app.hud.needs_update() {
        app.hud.update();
    }

    // Error notification auto-dismiss
    app.error_notif.tick();

    // Teaching mode poll LLM response
    app.teaching.poll();

    // Notifier: desktop notification when long command finishes
    if app.notifier.check(app.window_focused) {
        crate::tools::notify::Notifier::send("rift", "Command completed");
    }

    // Request redraw if anything needs it
    let ssh_pending = app.ssh_connecting.is_some();
    let ai_waiting = app.ai_panel.is_waiting();
    let any_overlay = app.prefs.visible
        || app.welcome.visible
        || app.ssh_dialog.visible
        || app.autocomplete.visible
        || app.ai_panel.visible
        || app.compare_view.visible
        || app.search.visible
        || app.file_manager.visible
        || app.git_panel.visible
        || app.cicd.visible
        || app.heatmap.visible
        || app.docker.visible
        || app.error_notif.visible
        || app.teaching.enabled;
    let timewarp_active = app.timewarp_browser.active;

    if pty_changed || has_shader || ssh_pending || ai_waiting || any_overlay || timewarp_active || app.hud_visible {
        app.request_redraw();
    }
}
