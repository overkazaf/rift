pub mod font;
#[cfg(feature = "gpu")]
pub mod gpu;

use std::time::Instant;

use crate::config::{Rgb, Theme};
use crate::effects::ShaderPipeline;
use crate::window::PaneRect;
use crate::terminal::{Color, ImageCell, Terminal, TermImage};
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
        blocks: &crate::tools::blocks::BlockManager,
    ) {
        self.render_tabbed_with_cmd(wm, content_area, buffer, width, height, false, blocks, None);
    }

    pub fn render_tabbed_with_cmd(
        &mut self,
        wm: &WindowManager,
        content_area: PaneRect,
        buffer: &mut [u32],
        width: u32,
        height: u32,
        cmd_held: bool,
        blocks: &crate::tools::blocks::BlockManager,
        hover_pane: Option<usize>,
    ) {
        let w = width as usize;
        let h = height as usize;

        let bg = pack(self.theme.bg.0, self.theme.bg.1, self.theme.bg.2);
        buffer.fill(bg);

        let tab_bar_h = content_area.y;

        let layouts = wm.pane_layouts(content_area);
        let active_tab = wm.active_tab();
        for (idx, rect, is_active) in &layouts {
            if let Some(pane) = active_tab.pane(*idx) {
                let terminal = &pane.terminal;
                self.render_pane_inner(terminal, buffer, w, h, *rect, *is_active, cmd_held);
            }
        }

        if layouts.len() > 1 {
            self.render_pane_borders(&layouts, buffer, w, h, hover_pane);
        }

        // Warp-style command block indicators (left-side bar)
        if blocks.block_count() > 0 {
            if let Some((_, rect, _)) = layouts.first() {
                let terminal = &active_tab.active_pane().terminal;
                self.render_block_indicators(terminal, blocks, buffer, w, *rect);
            }
        }

        // Tab bar rendered LAST so it's always on top of pane content
        self.render_tab_bar(wm, buffer, w, tab_bar_h);

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

        // Ghostty: dark titlebar bg, noticeably darker than terminal content
        let bar_bg = darken(self.theme.bg, 30);
        let bar_bg_px = pack(bar_bg.0, bar_bg.1, bar_bg.2);

        for y in 0..bar_height {
            let off = y * buf_width;
            let end = (off + buf_width).min(buffer.len());
            buffer[off..end].fill(bar_bg_px);
        }

        let tabs = wm.tab_bar_info();
        let cw = self.font.cell_width;
        let ch = self.font.cell_height;
        let text_y = (bar_height.saturating_sub(ch)) / 2;

        let active_text = self.theme.fg;
        let inactive_text = dim(self.theme.fg, 0.45);

        // Active tab = terminal bg (content flows into the tab)
        let active_bg = self.theme.bg;
        let active_bg_px = pack(active_bg.0, active_bg.1, active_bg.2);

        let tab_count = tabs.len().max(1);
        let tab_w = buf_width / tab_count;

        // Find active tab index for separator logic
        let active_idx = tabs.iter().position(|(_, a)| *a);

        for (i, (title, is_active)) in tabs.iter().enumerate() {
            let x0 = i * tab_w;
            let x1 = if i == tab_count - 1 { buf_width } else { (i + 1) * tab_w };
            if x0 >= buf_width { break; }

            if *is_active {
                // Ghostty: top-rounded rect, bottom flush, side margins for the gap
                let margin = 4;
                let top = 3;
                let bot = bar_height;
                let pl = x0 + margin;
                let pr = x1.saturating_sub(margin);
                let r = 8i32;

                for y in top..bot {
                    let dy = (top as i32 + r) - y as i32;
                    let inset = if dy > 0 && dy <= r {
                        // Anti-aliased rounded corner
                        let exact = r as f32 - ((r * r - dy * dy) as f32).sqrt();
                        exact.round() as i32
                    } else {
                        0
                    };
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
            }

            // Title centered
            let text_color = if *is_active { active_text } else { inactive_text };
            let max_chars = ((x1 - x0) / cw).saturating_sub(2);
            let title_str = truncate_str(title, max_chars);
            let title_len = title_str.chars().count();
            let text_total_w = title_len * cw;
            let text_x = x0 + ((x1 - x0).saturating_sub(text_total_w)) / 2;

            for (ci, c) in title_str.chars().enumerate() {
                let gx = text_x + ci * cw;
                if gx + cw > x1 || gx + cw > buf_width { break; }
                if c == ' ' { continue; }
                self.draw_char(buffer, buf_width, bar_height, c, gx, text_y, text_color);
            }
        }

        // Bottom border: 1px line under inactive regions only
        let sep_px = pack_rgb(darken(self.theme.bg, 10));
        let sep_y = bar_height.saturating_sub(1);
        let (ax0, ax1) = match active_idx {
            Some(ai) => {
                let m = 4;
                (ai * tab_w + m, (if ai == tab_count - 1 { buf_width } else { (ai + 1) * tab_w }).saturating_sub(m))
            }
            None => (0, 0),
        };
        if ax0 > 0 {
            let off = sep_y * buf_width;
            let end = (off + ax0).min(buffer.len());
            if off < buffer.len() { buffer[off..end].fill(sep_px); }
        }
        if ax1 < buf_width {
            let off = sep_y * buf_width + ax1;
            let end = (sep_y * buf_width + buf_width).min(buffer.len());
            if off < end { buffer[off..end].fill(sep_px); }
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
        // Kitty graphics placements are absolute grid coordinates that don't
        // track scrollback (see terminal::images module docs), so only blit
        // them while looking at the live screen.
        let images_visible = !terminal.is_scrolled_back();

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

                // Kitty graphics: if this cell is covered by a placed image,
                // blit the corresponding slice of pixels and skip normal
                // glyph/background rendering for it entirely — mirrors how
                // real terminals leave blanks in the text grid under an
                // image placement.
                if images_visible {
                    if let Some(ic) = terminal.image_store.get_cell(row, col) {
                        if let Some(img) = terminal.image_store.get_image(ic.image_id) {
                            blit_image_cell(buffer, buf_width, buf_height, img, ic, x0, y0, cw, ch);
                        }
                        continue;
                    }
                }

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
                    let fg_px = pack(text_color.0, text_color.1, text_color.2);
                    let bitmap = self.font.rasterize(cell.c);
                    for cy in 0..ch {
                        let row_off = (y0 + cy) * buf_width + x0;
                        let bmp_off = cy * cw;
                        for cx in 0..cw {
                            let coverage = bitmap[bmp_off + cx] as u32;
                            if coverage == 0 { continue; }
                            let idx = row_off + cx;
                            if idx < buffer.len() {
                                buffer[idx] = if coverage >= 250 { fg_px } else {
                                    blend(text_color, buffer[idx], coverage)
                                };
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
        hover_pane: Option<usize>,
    ) {
        let split_color = darken(self.theme.bg, 6);
        let split_px = pack(split_color.0, split_color.1, split_color.2);
        let active_border = self.theme.cursor;
        let active_px = pack(active_border.0, active_border.1, active_border.2);
        let hover_border = dim(self.theme.cursor, 0.4);
        let hover_px = pack(hover_border.0, hover_border.1, hover_border.2);

        for (idx, rect, is_active) in layouts {
            let is_hover = hover_pane == Some(*idx) && !*is_active;
            if *is_active {
                draw_rect_border(buffer, buf_width, buf_height, rect, active_px);
            } else if is_hover {
                draw_rect_border(buffer, buf_width, buf_height, rect, hover_px);
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

    fn render_block_indicators(
        &self,
        terminal: &Terminal,
        blocks: &crate::tools::blocks::BlockManager,
        buffer: &mut [u32],
        buf_width: usize,
        rect: PaneRect,
    ) {
        let ch = self.font.cell_height;
        let visible = terminal.visible_rows();
        let scroll_top = terminal.scrollback.len().saturating_sub(terminal.scroll_offset);

        let bar_w = 3;
        let bar_x = rect.x + 2;
        let accent = self.theme.cursor;
        let accent_px = pack(accent.0, accent.1, accent.2);
        let dim_accent = dim(accent, 0.3);
        let dim_px = pack(dim_accent.0, dim_accent.1, dim_accent.2);

        for (row, _) in visible.iter().enumerate() {
            let abs_line = scroll_top + row;
            if let Some((_, blk)) = blocks.block_at_line(abs_line) {
                let is_cmd_line = abs_line == blk.output_start.saturating_sub(1);
                let px = if is_cmd_line { accent_px } else { dim_px };
                let y0 = rect.y + row * ch;
                for y in y0..(y0 + ch).min(rect.y + rect.height) {
                    for dx in 0..bar_w {
                        let idx = y * buf_width + bar_x + dx;
                        if idx < buffer.len() { buffer[idx] = px; }
                    }
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
        rect: PaneRect,
    ) {
        if !sel.active { return; }
        let cw = self.font.cell_width;
        let ch = self.font.cell_height;
        let offset_y = rect.y;
        let offset_x = rect.x;
        let stride = buf_width;
        let clip_right = (rect.x + rect.width).min(buf_width);
        let buf_height = (rect.y + rect.height).min(buf_height);
        let buf_width = clip_right;

        let max_row = buf_height.saturating_sub(offset_y) / ch.max(1);
        let max_col = buf_width.saturating_sub(offset_x) / cw.max(1);

        for row in 0..max_row {
            for col in 0..max_col {
                if sel.contains(row, col) {
                    let x0 = offset_x + col * cw;
                    let y0 = offset_y + row * ch;
                    for cy in 0..ch {
                        let py = y0 + cy;
                        if py >= buf_height { break; }
                        for cx in 0..cw {
                            let px = x0 + cx;
                            if px >= buf_width { break; }
                            let idx = py * stride + px;
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

use crate::ui::{pack, pack_rgb, darken, dim};

/// Blit the slice of `img` that belongs in one terminal cell (as identified
/// by `ic.offset_x`/`offset_y`, the cell's position within the image's
/// placement span) into the pixel rectangle `[x0, x0+cw) x [y0, y0+ch)`.
/// Scales via nearest-neighbor sampling, so the image is stretched to
/// exactly fill however many cells its placement spans.
fn blit_image_cell(
    buffer: &mut [u32],
    buf_width: usize,
    buf_height: usize,
    img: &TermImage,
    ic: ImageCell,
    x0: usize,
    y0: usize,
    cw: usize,
    ch: usize,
) {
    if img.width == 0 || img.height == 0 {
        return;
    }
    let expected = img.width as usize * img.height as usize * 4;
    if img.data.len() < expected {
        return;
    }

    let (span_rows, span_cols) = img
        .placement
        .as_ref()
        .map(|p| (p.cell_rows.max(1), p.cell_cols.max(1)))
        .unwrap_or((1, 1));

    // The full placement spans (span_cols*cw, span_rows*ch) pixels; this one
    // cell is a (cw, ch) slice of that, offset by (offset_x*cw, offset_y*ch).
    let full_w = (span_cols * cw).max(1);
    let full_h = (span_rows * ch).max(1);

    for dy in 0..ch {
        let py = y0 + dy;
        if py >= buf_height {
            break;
        }
        let out_y = ic.offset_y as usize * ch + dy;
        let src_y = (out_y * img.height as usize / full_h).min(img.height as usize - 1);
        for dx in 0..cw {
            let px = x0 + dx;
            if px >= buf_width {
                break;
            }
            let out_x = ic.offset_x as usize * cw + dx;
            let src_x = (out_x * img.width as usize / full_w).min(img.width as usize - 1);
            let si = (src_y * img.width as usize + src_x) * 4;
            let a = img.data[si + 3] as u32;
            if a == 0 {
                continue;
            }
            let idx = py * buf_width + px;
            if idx >= buffer.len() {
                continue;
            }
            let src_rgb = (img.data[si], img.data[si + 1], img.data[si + 2]);
            buffer[idx] = if a >= 250 {
                pack(src_rgb.0, src_rgb.1, src_rgb.2)
            } else {
                blend(src_rgb, buffer[idx], a)
            };
        }
    }
}

#[inline]
fn blend(fg: Rgb, base: u32, alpha: u32) -> u32 {
    if alpha >= 250 {
        return pack(fg.0, fg.1, fg.2);
    }
    let inv = 255 - alpha;
    let br = (base >> 16) & 0xff;
    let bg = (base >> 8) & 0xff;
    let bb = base & 0xff;
    let r = (fg.0 as u32 * alpha + br * inv) / 255;
    let g = (fg.1 as u32 * alpha + bg * inv) / 255;
    let b = (fg.2 as u32 * alpha + bb * inv) / 255;
    pack(r as u8, g as u8, b as u8)
}

fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len { return s; }
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) { end -= 1; }
    &s[..end]
}
