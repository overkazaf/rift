pub mod font;
#[cfg(feature = "gpu")]
pub mod gpu;

use std::time::Instant;

use crate::config::{Rgb, Theme};
use crate::effects::ShaderPipeline;
use crate::window::PaneRect;
use crate::terminal::{Color, Terminal};
use crate::window::WindowManager;
use font::FontManager;

pub struct Renderer {
    pub font: FontManager,
    pub theme: Theme,
    pub shader: ShaderPipeline,
    pub opacity: f32,
    pub start_time: Instant,
}

impl Renderer {
    pub fn new(font_path: &str, font_size: f32, theme: Theme) -> Self {
        log::info!("Renderer: font_size={font_size}px, font_path={font_path}");
        Self {
            font: FontManager::new(font_path, font_size),
            theme,
            shader: ShaderPipeline::new(),
            opacity: 1.0,
            start_time: Instant::now(),
        }
    }

    pub fn reinit_font(&mut self, font_path: &str, font_size: f32) {
        log::info!("Reinit font: {font_size}px (scaled for display)");
        self.font = FontManager::new(font_path, font_size);
    }

    pub fn cell_width(&self) -> usize { self.font.cell_width }
    pub fn cell_height(&self) -> usize { self.font.cell_height }

    #[allow(dead_code)]
    pub fn render_tabbed(
        &mut self,
        wm: &WindowManager,
        content_area: PaneRect,
        buffer: &mut [u32],
        width: u32,
        height: u32,
    ) {
        self.render_tabbed_with_cmd(wm, content_area, buffer, width, height, false);
    }

    pub fn render_tabbed_with_cmd(
        &mut self,
        wm: &WindowManager,
        content_area: PaneRect,
        buffer: &mut [u32],
        width: u32,
        height: u32,
        cmd_held: bool,
    ) {
        let w = width as usize;
        let h = height as usize;

        let bg = pack(self.theme.bg.0, self.theme.bg.1, self.theme.bg.2);
        buffer.fill(bg);

        let tab_bar_h = content_area.y;
        self.render_tab_bar(wm, buffer, w, tab_bar_h);

        let layouts = wm.pane_layouts(content_area);
        let active_tab = wm.active_tab();
        for (idx, rect, is_active) in &layouts {
            if *idx < active_tab.panes.len() {
                let terminal = &active_tab.panes[*idx].terminal;
                self.render_pane_inner(terminal, buffer, w, h, *rect, *is_active, cmd_held);
            }
        }

        if layouts.len() > 1 {
            self.render_pane_borders(&layouts, buffer, w, h);
        }

        let elapsed = self.start_time.elapsed().as_secs_f32();
        self.shader.apply(buffer, width, height, elapsed);
    }

    fn render_tab_bar(
        &mut self,
        wm: &WindowManager,
        buffer: &mut [u32],
        buf_width: usize,
        bar_height: usize,
    ) {
        if bar_height == 0 { return; }

        let bar_bg = darken(self.theme.bg, 24);
        let accent = self.theme.cursor;

        // Background with subtle horizontal gradient (slightly brighter toward right)
        for y in 0..bar_height {
            for x in 0..buf_width {
                let grad = (x as f32 / buf_width as f32) * 3.0; // 0 to 3 brightness boost
                let r = (bar_bg.0 as f32 + grad).min(255.0) as u8;
                let g = (bar_bg.1 as f32 + grad).min(255.0) as u8;
                let b = (bar_bg.2 as f32 + grad).min(255.0) as u8;
                let idx = y * buf_width + x;
                if idx < buffer.len() { buffer[idx] = pack(r, g, b); }
            }
        }

        // Bottom accent line (1px accent, 1px glow)
        let accent_px = pack(accent.0, accent.1, accent.2);
        let glow = dim(accent, 0.25);
        let glow_px = pack(glow.0, glow.1, glow.2);
        {
            let y = bar_height.saturating_sub(1);
            for x in 0..buf_width {
                let idx = y * buf_width + x;
                if idx < buffer.len() { buffer[idx] = accent_px; }
            }
            if bar_height >= 2 {
                let y2 = bar_height.saturating_sub(2);
                for x in 0..buf_width {
                    let idx = y2 * buf_width + x;
                    if idx < buffer.len() { buffer[idx] = glow_px; }
                }
            }
        }

        let tabs = wm.tab_bar_info();
        let cw = self.font.cell_width;
        let ch = self.font.cell_height;
        let text_y = (bar_height.saturating_sub(ch + 2)) / 2;
        let active_text = self.theme.fg;
        let inactive_text = dim(self.theme.fg, 0.55);  // brighter for readability
        let inactive_bg = lighten(bar_bg, 14);  // more visible background
        let inactive_bg_px = pack(inactive_bg.0, inactive_bg.1, inactive_bg.2);
        let active_bg_px = pack(self.theme.bg.0, self.theme.bg.1, self.theme.bg.2);

        let tab_count = tabs.len().max(1);
        let tab_w = (buf_width / tab_count).min(300).max(60);

        for (i, (title, is_active)) in tabs.iter().enumerate() {
            let x0 = i * tab_w;
            let x1 = ((i + 1) * tab_w).min(buf_width);
            if x0 >= buf_width { break; }

            let margin = 3;
            let top = 4;
            let bot = bar_height.saturating_sub(2); // leave room for accent line

            if *is_active {
                // Active: terminal bg with rounded top corners
                let pl = x0 + margin;
                let pr = x1.saturating_sub(margin);
                let r = 5i32;

                for y in top..bot {
                    let dy = (top as i32 + r) - y as i32;
                    let inset = if dy > 0 && dy <= r {
                        let val = r * r - (r - dy) * (r - dy);
                        r - if val > 0 { integer_sqrt(val as usize) as i32 } else { 0 }
                    } else { 0 };

                    let rl = (pl as i32 + inset).max(0) as usize;
                    let rr = (pr as i32 - inset).max(0) as usize;
                    if rl < rr && rl < buf_width {
                        let off = y * buf_width + rl;
                        let end = (off + rr - rl).min(buffer.len());
                        if off < buffer.len() {
                            buffer[off..end].fill(active_bg_px);
                        }
                    }
                }

                // Active tab: 3px accent bar at bottom (connection to content)
                for dy in 0..3 {
                    let y = bot.saturating_sub(1) + dy;
                    if y < bar_height {
                        let off = y * buf_width + pl;
                        let end = (off + pr.saturating_sub(pl)).min(buffer.len());
                        if off < buffer.len() {
                            let bar_color = if dy == 0 { accent_px } else { glow_px };
                            buffer[off..end].fill(bar_color);
                        }
                    }
                }
            } else {
                // Inactive: subtle background (not transparent — gives tabs "shape")
                let pl = x0 + margin + 1;
                let pr = x1.saturating_sub(margin + 1);
                for y in (top + 2)..bot {
                    if pl < pr {
                        let off = y * buf_width + pl;
                        let end = (off + pr - pl).min(buffer.len());
                        if off < buffer.len() {
                            buffer[off..end].fill(inactive_bg_px);
                        }
                    }
                }

                // Subtle separator between inactive tabs
                if i > 0 && !tabs.get(i - 1).map_or(false, |(_, a)| *a) {
                    let sep_c = lighten(bar_bg, 8);
                    let sp = pack(sep_c.0, sep_c.1, sep_c.2);
                    for y in (top + 4)..bot.saturating_sub(2) {
                        let idx = y * buf_width + x0;
                        if idx < buffer.len() { buffer[idx] = sp; }
                    }
                }
            }

            // Status dot: ● for active (accent), · for inactive (dim)
            let dot_x = x0 + margin + 6;
            let dot_cy = text_y as i32 + ch as i32 / 2;
            if *is_active {
                let dot_r: i32 = 3;
                let dot_px = accent_px;
                for ddy in -dot_r..=dot_r {
                    for ddx in -dot_r..=dot_r {
                        if ddx * ddx + ddy * ddy <= dot_r * dot_r {
                            let px = dot_x as i32 + ddx;
                            let py = dot_cy + ddy;
                            if px >= 0 && py >= 0 && (px as usize) < buf_width && (py as usize) < bar_height {
                                let idx = py as usize * buf_width + px as usize;
                                if idx < buffer.len() { buffer[idx] = dot_px; }
                            }
                        }
                    }
                }
            } else {
                // Small dim dot for inactive
                let dot_px = pack(inactive_text.0, inactive_text.1, inactive_text.2);
                let dot_r: i32 = 2;
                for ddy in -dot_r..=dot_r {
                    for ddx in -dot_r..=dot_r {
                        if ddx * ddx + ddy * ddy <= dot_r * dot_r {
                            let px = dot_x as i32 + ddx;
                            let py = dot_cy + ddy;
                            if px >= 0 && py >= 0 && (px as usize) < buf_width && (py as usize) < bar_height {
                                let idx = py as usize * buf_width + px as usize;
                                if idx < buffer.len() { buffer[idx] = dot_px; }
                            }
                        }
                    }
                }
            }

            // Title text after dot
            let title_x = dot_x as usize + 8;
            let max_chars = (x1.saturating_sub(title_x + cw * 2)) / cw;
            let title_str = truncate_str(title, max_chars);
            let tc = if *is_active { active_text } else { inactive_text };

            for (ci, c) in title_str.chars().enumerate() {
                let gx = title_x + ci * cw;
                if gx + cw > x1 || gx + cw > buf_width { break; }
                if c == ' ' { continue; }
                self.draw_char(buffer, buf_width, bar_height, c, gx, text_y, tc);
            }

            // Close button "x" (active tab slightly brighter)
            if tab_count > 1 && x1 >= cw * 2 + margin + 8 {
                let close_x = x1.saturating_sub(cw + margin + 6);
                let close_color = if *is_active { dim(active_text, 0.35) } else { dim(inactive_text, 0.5) };
                if close_x + cw <= buf_width {
                    self.draw_char(buffer, buf_width, bar_height, 'x', close_x, text_y, close_color);
                }
            }
        }
    }

    fn draw_char(
        &mut self,
        buffer: &mut [u32],
        buf_width: usize,
        buf_height: usize,
        c: char,
        x: usize,
        y: usize,
        color: Rgb,
    ) {
        let cw = self.font.cell_width;
        let ch = self.font.cell_height;
        let bitmap = self.font.rasterize(c);
        for cy in 0..ch {
            let py = y + cy;
            if py >= buf_height { break; }
            for cx in 0..cw {
                let px = x + cx;
                if px >= buf_width { break; }
                let coverage = bitmap[cy * cw + cx] as u32;
                if coverage == 0 { continue; }
                let idx = py * buf_width + px;
                if idx < buffer.len() {
                    buffer[idx] = blend(color, buffer[idx], coverage);
                }
            }
        }
    }

    #[allow(dead_code)]
    fn render_pane(
        &mut self,
        terminal: &Terminal,
        buffer: &mut [u32],
        buf_width: usize,
        buf_height: usize,
        rect: PaneRect,
        is_active: bool,
    ) {
        self.render_pane_inner(terminal, buffer, buf_width, buf_height, rect, is_active, false)
    }

    #[allow(dead_code)]
    pub fn render_pane_with_cmd(
        &mut self,
        terminal: &Terminal,
        buffer: &mut [u32],
        buf_width: usize,
        buf_height: usize,
        rect: PaneRect,
        is_active: bool,
        cmd_held: bool,
    ) {
        self.render_pane_inner(terminal, buffer, buf_width, buf_height, rect, is_active, cmd_held)
    }

    fn render_pane_inner(
        &mut self,
        terminal: &Terminal,
        buffer: &mut [u32],
        buf_width: usize,
        buf_height: usize,
        rect: PaneRect,
        is_active: bool,
        cmd_held: bool,
    ) {
        let cw = self.font.cell_width;
        let ch = self.font.cell_height;

        let visible = terminal.visible_rows();
        // Cursor blink: Bar and Underline blink at ~2Hz, Block stays solid
        let elapsed = self.start_time.elapsed().as_secs_f32();
        let cursor_blink_on = match terminal.cursor_style {
            crate::terminal::CursorStyle::Block => true,
            _ => (elapsed * 2.0) as u32 % 2 == 0,
        };
        let show_cursor = is_active && terminal.cursor_visible && !terminal.is_scrolled_back() && cursor_blink_on;

        // Pre-detect URLs (only when Cmd is held)
        let url_ranges: Vec<Vec<(usize, usize)>> = if cmd_held {
            visible.iter().map(|cells| {
                let line: String = cells.iter().map(|c| c.c).collect();
                crate::tools::url_detect::detect_urls(&line)
                    .into_iter()
                    .map(|(start, end, _)| (start, end))
                    .collect()
            }).collect()
        } else {
            Vec::new()
        };

        for (row, cells) in visible.iter().enumerate() {
            for (col, cell) in cells.iter().enumerate() {
                let x0 = rect.x + col * cw;
                let y0 = rect.y + row * ch;
                if x0 + cw > rect.x + rect.width || y0 + ch > rect.y + rect.height { continue; }
                if x0 + cw > buf_width || y0 + ch > buf_height { continue; }

                let (mut fg, mut bg) = (
                    self.resolve(cell.fg, true),
                    self.resolve(cell.bg, false),
                );
                if cell.attrs.reverse { std::mem::swap(&mut fg, &mut bg); }
                if cell.attrs.dim {
                    fg = (fg.0 / 2, fg.1 / 2, fg.2 / 2);
                }

                // Skip wide-char continuation placeholder
                if cell.c == '\0' { continue; }

                let is_cursor = show_cursor
                    && row == terminal.cursor_row
                    && col == terminal.cursor_col;

                // Determine cursor width (2 cells for wide chars)
                let cursor_w = if is_cursor {
                    use unicode_width::UnicodeWidthChar;
                    cell.c.width().unwrap_or(1).max(1) * cw
                } else { cw };

                // Background fill
                if cell.bg != Color::Default || (is_cursor && terminal.cursor_style == crate::terminal::CursorStyle::Block) {
                    let fill = if is_cursor && terminal.cursor_style == crate::terminal::CursorStyle::Block {
                        self.theme.cursor
                    } else { bg };
                    let px = pack(fill.0, fill.1, fill.2);
                    let fill_w = if is_cursor { cursor_w } else { cw };
                    for cy in 0..ch {
                        let offset = (y0 + cy) * buf_width + x0;
                        if offset + fill_w <= buffer.len() {
                            buffer[offset..offset + fill_w].fill(px);
                        }
                    }
                }

                // Bar cursor (left 2px)
                if is_cursor && terminal.cursor_style == crate::terminal::CursorStyle::Bar {
                    let bar_px = pack(self.theme.cursor.0, self.theme.cursor.1, self.theme.cursor.2);
                    for cy in 0..ch {
                        let offset = (y0 + cy) * buf_width + x0;
                        if offset + 1 < buffer.len() {
                            buffer[offset] = bar_px;
                            buffer[offset + 1] = bar_px;
                        }
                    }
                }

                // Underline cursor (bottom 2px, width matches char)
                if is_cursor && terminal.cursor_style == crate::terminal::CursorStyle::Underline {
                    let ul_px = pack(self.theme.cursor.0, self.theme.cursor.1, self.theme.cursor.2);
                    for dy in 0..2usize {
                        let uy = y0 + ch.saturating_sub(1 + dy);
                        for cx in 0..cursor_w {
                            let idx = uy * buf_width + x0 + cx;
                            if idx < buffer.len() { buffer[idx] = ul_px; }
                        }
                    }
                }

                if cell.c != ' ' && !cell.attrs.hidden {
                    let text_color = if is_cursor && terminal.cursor_style == crate::terminal::CursorStyle::Block {
                        self.theme.bg
                    } else { fg };
                    let bitmap = self.font.rasterize(cell.c);
                    for cy in 0..ch {
                        for cx in 0..cw {
                            let coverage = bitmap[cy * cw + cx] as u32;
                            if coverage == 0 { continue; }
                            let idx = (y0 + cy) * buf_width + x0 + cx;
                            if idx < buffer.len() {
                                buffer[idx] = blend(text_color, buffer[idx], coverage);
                            }
                        }
                    }
                }

                // URL: underline + accent color (only when Cmd held)
                if !url_ranges.is_empty() && row < url_ranges.len() {
                    let is_url = url_ranges[row].iter().any(|&(s, e)| col >= s && col < e);
                    if is_url {
                        // Recolor text to accent/cursor color
                        let accent = self.theme.cursor;
                        if cell.c != ' ' && !cell.attrs.hidden {
                            let bitmap = self.font.rasterize(cell.c);
                            for cy in 0..ch {
                                for cx in 0..cw {
                                    let coverage = bitmap[cy * cw + cx] as u32;
                                    if coverage > 128 {
                                        let idx = (y0 + cy) * buf_width + x0 + cx;
                                        if idx < buffer.len() {
                                            buffer[idx] = blend(accent, buffer[idx], coverage);
                                        }
                                    }
                                }
                            }
                        }
                        // Underline
                        let uy = y0 + ch - 2;
                        if uy < buf_height {
                            let ul_px = pack(accent.0, accent.1, accent.2);
                            for cx in 0..cw {
                                let idx = uy * buf_width + x0 + cx;
                                if idx < buffer.len() { buffer[idx] = ul_px; }
                            }
                        }
                    }
                }
            }
        }

        // Scrollback indicator
        if terminal.is_scrolled_back() {
            let indicator = format!("[{}/{}]", terminal.scroll_offset, terminal.scrollback_len());
            let ix = rect.x + rect.width.saturating_sub(indicator.len() * cw + 8);
            let iy = rect.y + 4;
            let ind_bg = pack(40, 40, 60);
            for y in iy..iy + ch + 4 {
                for x in ix.saturating_sub(4)..ix + indicator.len() * cw + 4 {
                    if x < buf_width && y < buf_height {
                        let idx = y * buf_width + x;
                        if idx < buffer.len() { buffer[idx] = ind_bg; }
                    }
                }
            }
            self.draw_char_seq(buffer, buf_width, buf_height, &indicator, ix, iy + 2, (200, 200, 255));
        }
    }

    fn draw_char_seq(
        &mut self, buffer: &mut [u32], buf_width: usize, buf_height: usize,
        text: &str, x: usize, y: usize, color: Rgb,
    ) {
        let cw = self.font.cell_width;
        for (i, c) in text.chars().enumerate() {
            if c == ' ' { continue; }
            self.draw_char(buffer, buf_width, buf_height, c, x + i * cw, y, color);
        }
    }

    fn render_pane_borders(
        &self,
        layouts: &[(usize, PaneRect, bool)],
        buffer: &mut [u32],
        buf_width: usize,
        buf_height: usize,
    ) {
        let split_color = darken(self.theme.bg, 6);
        let split_px = pack(split_color.0, split_color.1, split_color.2);
        let active_border = self.theme.cursor;
        let active_px = pack(active_border.0, active_border.1, active_border.2);

        for (_, rect, is_active) in layouts {
            if *is_active {
                draw_rect_border(buffer, buf_width, buf_height, rect, active_px);
            } else {
                if rect.x > 0 {
                    let bx = rect.x - 1;
                    for y in rect.y..((rect.y + rect.height).min(buf_height)) {
                        let idx = y * buf_width + bx;
                        if idx < buffer.len() { buffer[idx] = split_px; }
                    }
                }
                if rect.y > 0 {
                    let by = rect.y - 1;
                    let off = by * buf_width + rect.x;
                    let end = (off + rect.width).min(buffer.len());
                    if off < buffer.len() { buffer[off..end].fill(split_px); }
                }
            }
        }
    }

    /// Render a snapshot grid (used by TimeWarp playback).
    pub fn render_snapshot(
        &mut self,
        grid: &[Vec<crate::terminal::Cell>],
        cursor_row: usize,
        cursor_col: usize,
        buffer: &mut [u32],
        buf_width: usize,
        buf_height: usize,
        offset_y: usize,
    ) {
        let cw = self.font.cell_width;
        let ch = self.font.cell_height;
        let rows = grid.len();

        for row in 0..rows {
            let cols = grid[row].len();
            for col in 0..cols {
                let x0 = col * cw;
                let y0 = offset_y + row * ch;
                if x0 + cw > buf_width || y0 + ch > buf_height { continue; }

                let cell = &grid[row][col];
                let (mut fg, mut bg) = (self.resolve(cell.fg, true), self.resolve(cell.bg, false));
                if cell.attrs.reverse { std::mem::swap(&mut fg, &mut bg); }
                if cell.attrs.dim { fg = (fg.0 / 2, fg.1 / 2, fg.2 / 2); }

                let is_cursor = row == cursor_row && col == cursor_col;

                let fill = if is_cursor { self.theme.cursor } else { bg };
                if is_cursor || cell.bg != Color::Default {
                    let px = pack(fill.0, fill.1, fill.2);
                    for cy in 0..ch {
                        let offset = (y0 + cy) * buf_width + x0;
                        if offset + cw <= buffer.len() {
                            buffer[offset..offset + cw].fill(px);
                        }
                    }
                }

                if cell.c != ' ' && !cell.attrs.hidden {
                    let text_color = if is_cursor { self.theme.bg } else { fg };
                    let bitmap = self.font.rasterize(cell.c);
                    for cy in 0..ch {
                        for cx in 0..cw {
                            let coverage = bitmap[cy * cw + cx] as u32;
                            if coverage == 0 { continue; }
                            let idx = (y0 + cy) * buf_width + x0 + cx;
                            if idx < buffer.len() {
                                buffer[idx] = blend(text_color, buffer[idx], coverage);
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    pub fn render_overlay(
        &mut self,
        prefs: &crate::ui::Preferences,
        buffer: &mut [u32],
        width: u32,
        height: u32,
    ) {
        prefs.render(buffer, width as usize, height as usize, &mut self.font, &self.theme);
    }

    pub fn render_selection(
        &self,
        sel: &crate::window::Selection,
        buffer: &mut [u32],
        buf_width: usize,
        buf_height: usize,
        offset_y: usize,
    ) {
        if !sel.active { return; }
        let cw = self.font.cell_width;
        let ch = self.font.cell_height;

        let max_row = buf_height.saturating_sub(offset_y) / ch.max(1);
        let max_col = buf_width / cw.max(1);

        for row in 0..max_row {
            for col in 0..max_col {
                if sel.contains(row, col) {
                    let x0 = col * cw;
                    let y0 = offset_y + row * ch;
                    for cy in 0..ch {
                        let py = y0 + cy;
                        if py >= buf_height { break; }
                        for cx in 0..cw {
                            let px = x0 + cx;
                            if px >= buf_width { break; }
                            let idx = py * buf_width + px;
                            if idx < buffer.len() {
                                let base = buffer[idx];
                                let br = (base >> 16) & 0xff;
                                let bg = (base >> 8) & 0xff;
                                let bb = base & 0xff;
                                let r = (br * 150 + 80 * 105) / 255;
                                let g = (bg * 150 + 120 * 105) / 255;
                                let b = (bb * 150 + 200 * 105) / 255;
                                buffer[idx] = (r << 16) | (g << 8) | b;
                            }
                        }
                    }
                }
            }
        }
    }

    fn resolve(&self, color: Color, is_fg: bool) -> Rgb {
        match color {
            Color::Default => if is_fg { self.theme.fg } else { self.theme.bg },
            Color::Indexed(idx) => self.theme.resolve_indexed(idx),
            Color::Rgb(r, g, b) => (r, g, b),
        }
    }
}

fn draw_rect_border(
    buffer: &mut [u32],
    buf_width: usize,
    buf_height: usize,
    rect: &PaneRect,
    px: u32,
) {
    // Draw border INSIDE the pane rect (so it's never clipped)
    let x1 = rect.x;
    let y1 = rect.y;
    let x2 = (rect.x + rect.width).min(buf_width).saturating_sub(1);
    let y2 = (rect.y + rect.height).min(buf_height).saturating_sub(1);

    // Top edge
    for x in x1..=x2.min(buf_width.saturating_sub(1)) {
        let idx = y1 * buf_width + x;
        if idx < buffer.len() { buffer[idx] = px; }
    }
    // Bottom edge
    if y2 < buf_height {
        for x in x1..=x2.min(buf_width.saturating_sub(1)) {
            let idx = y2 * buf_width + x;
            if idx < buffer.len() { buffer[idx] = px; }
        }
    }
    // Left + Right edges
    for y in y1..=y2.min(buf_height.saturating_sub(1)) {
        if x1 < buf_width {
            let idx = y * buf_width + x1;
            if idx < buffer.len() { buffer[idx] = px; }
        }
        if x2 < buf_width {
            let idx = y * buf_width + x2;
            if idx < buffer.len() { buffer[idx] = px; }
        }
    }
}

use crate::ui::{pack, darken, dim, lighten};

#[inline]
fn blend(fg: Rgb, base: u32, alpha: u32) -> u32 {
    let inv = 255 - alpha;
    let br = (base >> 16) & 0xff;
    let bg = (base >> 8) & 0xff;
    let bb = base & 0xff;
    let r = (fg.0 as u32 * alpha + br * inv) / 255;
    let g = (fg.1 as u32 * alpha + bg * inv) / 255;
    let b = (fg.2 as u32 * alpha + bb * inv) / 255;
    pack(r as u8, g as u8, b as u8)
}

fn integer_sqrt(n: usize) -> usize {
    if n == 0 { return 0; }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len { return s; }
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) { end -= 1; }
    &s[..end]
}
