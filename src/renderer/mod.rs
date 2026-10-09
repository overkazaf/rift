pub mod font;
pub mod shape;
#[cfg(any(feature = "gpu", test))]
pub mod atlas;
#[cfg(feature = "gpu")]
pub mod gpu;
#[cfg(feature = "gpu")]
pub mod gpu_text;
#[cfg(all(test, feature = "gpu"))]
mod gpu_tests;

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::time::{Duration, Instant};

use crate::config::{Rgb, Theme};
use crate::effects::ShaderPipeline;
use crate::window::PaneRect;
use crate::window::tab::{SplitBorder, SplitDir, BORDER};
use crate::terminal::{Cell, Color, ImageCell, Terminal, TermImage};
use crate::window::WindowManager;
use font::{Deco, FontManager};
use crate::terminal::grid::UnderlineStyle;

// ── Cheap hashing (FxHash-style) for damage tracking and the tile cache ──

#[derive(Default, Clone, Copy)]
pub struct FxHasher(u64);

const FX_SEED: u64 = 0x517c_c1b7_2722_0a95;

impl FxHasher {
    #[inline]
    fn add(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(FX_SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 { self.0 }
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes { self.add(b as u64); }
    }
    #[inline]
    fn write_u8(&mut self, i: u8) { self.add(i as u64); }
    #[inline]
    fn write_u32(&mut self, i: u32) { self.add(i as u64); }
    #[inline]
    fn write_u64(&mut self, i: u64) { self.add(i); }
    #[inline]
    fn write_usize(&mut self, i: usize) { self.add(i as u64); }
}

type FxBuild = BuildHasherDefault<FxHasher>;

#[inline]
fn mix(h: u64, v: u64) -> u64 {
    (h.rotate_left(5) ^ v).wrapping_mul(FX_SEED)
}

#[inline]
fn color_bits(c: Color) -> u64 {
    match c {
        Color::Default => 0,
        Color::Indexed(i) => (1 << 24) | i as u64,
        Color::Rgb(r, g, b) => (2 << 24) | (r as u64) << 16 | (g as u64) << 8 | b as u64,
    }
}

#[inline]
fn rgb_bits(c: Rgb) -> u64 {
    (c.0 as u64) << 16 | (c.1 as u64) << 8 | c.2 as u64
}

/// Hash of everything that determines a cell's pixels (apart from cursor /
/// selection-like per-frame state, which is mixed in separately).
#[inline]
fn hash_cells(mut h: u64, cells: &[Cell], hover_link: u16) -> u64 {
    for cell in cells {
        h = mix(h, cell.c as u64 | (cell.attr_bits() as u64) << 32);
        h = mix(h, color_bits(cell.fg) << 32 | color_bits(cell.bg));
        // Interned (hyperlink, cluster extras, underline colour) id, and
        // whether this cell's link is the hovered one (draws an underline).
        let x = cell.extra_id();
        if x != 0 {
            let hov = (hover_link != 0 && cell.link() == hover_link) as u64;
            h = mix(h, 1 << 48 | (x as u64) << 24 | hov);
        }
    }
    h
}

// ── Pre-blended glyph tile cache ──

/// (glyph, fg pixel, underlying bg pixel, style/decoration bits) -> cw*ch
/// pre-blended pixels.
type TileKey = (char, u32, u32, u64);
type TileMap = HashMap<TileKey, Box<[u32]>, FxBuild>;

/// Max number of cached tiles (two generations of half this size each).
const TILE_CACHE_CAP: usize = 4096;

/// Two-generation (approximate LRU) tile cache: lookups promote from `old`
/// into `cur`; when `cur` fills up it becomes `old` and the previous `old`
/// (the least recently used half) is dropped.
#[derive(Default)]
struct TileCache {
    cur: TileMap,
    old: TileMap,
}

impl TileCache {
    fn clear(&mut self) {
        self.cur.clear();
        self.old.clear();
    }

    /// Ensure `key` is in `cur`; returns false if it must be built.
    #[inline]
    fn promote(&mut self, key: TileKey) -> bool {
        if self.cur.contains_key(&key) { return true; }
        if let Some(t) = self.old.remove(&key) {
            self.insert(key, t);
            return true;
        }
        false
    }

    fn insert(&mut self, key: TileKey, tile: Box<[u32]>) {
        if self.cur.len() >= TILE_CACHE_CAP / 2 {
            self.old = std::mem::take(&mut self.cur);
        }
        self.cur.insert(key, tile);
    }

    #[inline]
    fn get(&self, key: &TileKey) -> &[u32] {
        &self.cur[key]
    }
}

/// Decoration paint request for [`build_tile`]: shape, geometry, underline pixel.
type TileDeco<'a> = (&'a Deco, &'a font::DecoMetrics, u32);

fn build_tile(
    bitmap: &[u8], cw: usize, ch: usize, fg: Rgb, fg_px: u32, bg_px: u32, deco: Option<TileDeco>,
) -> Box<[u32]> {
    let n = cw * ch;
    let mut tile = vec![bg_px; n];
    for (i, t) in tile.iter_mut().enumerate() {
        let coverage = bitmap.get(i).copied().unwrap_or(0) as u32;
        if coverage == 0 { continue; }
        *t = if coverage >= 250 { fg_px } else { blend(fg, bg_px, coverage) };
    }
    if let Some((d, m, ul_px)) = deco {
        font::paint_decor(&mut tile, cw, 0, 0, cw, ch, d, m, fg_px, ul_px);
    }
    tile.into_boxed_slice()
}

/// Decorations (underline shape, strikethrough, overline) of a cell.
#[inline]
fn cell_deco(cell: &Cell) -> Deco {
    Deco { ul: cell.ul_style(), strike: cell.strikethrough(), over: cell.overline() }
}

#[inline]
fn blit_tile(buffer: &mut [u32], buf_width: usize, x0: usize, y0: usize, cw: usize, ch: usize, tile: &[u32]) {
    for cy in 0..ch {
        let o = (y0 + cy) * buf_width + x0;
        buffer[o..o + cw].copy_from_slice(&tile[cy * cw..cy * cw + cw]);
    }
}

// ── Opt-in frame profiler (RIFT_PROFILE=1) ──

#[derive(Clone, Copy)]
pub enum Phase { Pty = 0, Render = 1, Overlays = 2, Present = 3, GpuInst = 4, GpuUi = 5 }

const PHASE_NAMES: [&str; 6] = ["pty", "render_tabbed", "overlays", "present", "gpu_inst", "gpu_ui"];
const PROFILE_REPORT_FRAMES: u32 = 120;

#[derive(Default)]
pub struct Profiler {
    sum: [Duration; 6],
    max: [Duration; 6],
    cnt: [u32; 6],
    frames: u32,
}

impl Profiler {
    fn add(&mut self, phase: Phase, d: Duration) {
        let i = phase as usize;
        self.sum[i] += d;
        self.max[i] = self.max[i].max(d);
        self.cnt[i] += 1;
    }

    fn frame_end(&mut self) {
        self.frames += 1;
        if self.frames < PROFILE_REPORT_FRAMES { return; }
        let mut line = format!("[rift-profile] {} frames:", self.frames);
        for i in 0..PHASE_NAMES.len() {
            if self.cnt[i] == 0 && i >= 4 { continue; }
            let n = self.cnt[i].max(1) as f64;
            line.push_str(&format!(
                " | {} avg {:.2} max {:.2} ms (n={})",
                PHASE_NAMES[i],
                self.sum[i].as_secs_f64() * 1000.0 / n,
                self.max[i].as_secs_f64() * 1000.0,
                self.cnt[i],
            ));
        }
        eprintln!("{line}");
        *self = Self::default();
    }
}

/// Marker for "no damage cache" (external buffers, tests): every row is dirty.
const NO_CACHE: usize = usize::MAX;

pub struct Renderer {
    pub font: FontManager,
    pub theme: Theme,
    pub shader: ShaderPipeline,
    pub opacity: f32,
    pub start_time: Instant,

    // ── Damage tracking state ──
    /// Persistent back buffer; the softbuffer buffer is not preserved between
    /// frames, so each frame we memcpy this into it and draw overlays on top.
    back: Vec<u32>,
    /// Signature of everything that forces a full redraw (size, theme, font
    /// metrics, layout, ...). A change invalidates all row hashes.
    frame_sig: u64,
    force_full: bool,
    /// True while the current frame is a full redraw (all rows dirty).
    full_frame: bool,
    tab_sig: u64,
    /// Per pane (index within the active tab) -> per visible row hash.
    pane_rows: HashMap<usize, Vec<u64>>,
    cur_pane: usize,
    /// View row holding the live cursor in the pane being rendered (differs
    /// from `cursor_row` when collapsed blocks shift rows).
    view_cursor_row: usize,
    /// Output of the last `render_pane_inner`: which rows were repainted.
    dirty_rows: Vec<bool>,
    tiles: TileCache,
    /// Draw bold text with the bright palette variant (config `bold_is_bright`).
    bold_is_bright: bool,
    /// DECSCNM state of the pane being rendered (swaps default fg/bg).
    scr_reverse: bool,
    /// Link id hovered in the pane being rendered (0 = none).
    cur_hover_link: u16,
    /// OSC 4 palette overrides of the pane being rendered (empty = none).
    pal_over: Vec<Option<Rgb>>,
    /// Hovered cell for OSC 8 link underlines: (pane index, view row, col).
    pub link_hover: Option<(usize, usize, usize)>,
    /// Hover / drag / scrollbar state for the software-drawn chrome; set by
    /// the app right before each frame.
    pub chrome: crate::ui::tabbar::ChromeUi,
    pub prof: Option<Profiler>,
    /// Shaping engine and cache for programming ligatures.
    shaper: shape::Shaper,
    /// Shape runs of cells for ligatures / contextual alternates.
    ligatures: bool,
    /// GPU text renderer state (None = CPU rendering).
    #[cfg(feature = "gpu")]
    pub gpu: Option<Box<gpu_text::GpuText>>,
    /// GPU text temporarily off: the CPU renders terminal text this frame
    /// (a full-window dim backdrop must also dim the text underneath).
    #[cfg(feature = "gpu")]
    gpu_suspended: bool,
    /// Another frame is needed right away (GPU atlas had to be recycled).
    #[cfg(feature = "gpu")]
    redraw_requested: bool,
    /// Highlighted ranges (selection, search) for the GPU path.
    #[cfg(feature = "gpu")]
    hl_pane: usize,
    #[cfg(feature = "gpu")]
    hl_rows: HashMap<usize, Vec<gpu_text::HlRect>>,
    #[cfg(feature = "gpu")]
    hl_hash: HashMap<usize, u64>,
}

/// How far unfocused panes are blended toward the background (0.0..1.0).
const UNFOCUSED_DIM: f32 = 0.28;
/// Divider base color: theme.bg lightened by this much.
const DIVIDER_LIGHTEN: u8 = 18;

/// Mouse / zoom state the split chrome needs, passed in from the app.
#[derive(Clone, Copy, Debug, Default)]
pub struct SplitUiState {
    pub hover_pane: Option<usize>,
    pub hover_border: Option<usize>,
    pub dragging_border: Option<usize>,
    pub zoomed: bool,
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
            back: Vec::new(),
            frame_sig: 0,
            force_full: true,
            full_frame: true,
            tab_sig: 0,
            pane_rows: HashMap::new(),
            cur_pane: NO_CACHE,
            view_cursor_row: 0,
            dirty_rows: Vec::new(),
            tiles: TileCache::default(),
            bold_is_bright: false,
            scr_reverse: false,
            cur_hover_link: 0,
            pal_over: Vec::new(),
            link_hover: None,
            chrome: Default::default(),
            prof: if std::env::var("RIFT_PROFILE").map_or(false, |v| v == "1") {
                Some(Profiler::default())
            } else {
                None
            },
            shaper: shape::Shaper::new(),
            ligatures: false,
            #[cfg(feature = "gpu")]
            gpu: None,
            #[cfg(feature = "gpu")]
            gpu_suspended: false,
            #[cfg(feature = "gpu")]
            redraw_requested: false,
            #[cfg(feature = "gpu")]
            hl_pane: usize::MAX,
            #[cfg(feature = "gpu")]
            hl_rows: HashMap::new(),
            #[cfg(feature = "gpu")]
            hl_hash: HashMap::new(),
        }
    }

    pub fn reinit_font(&mut self, font_path: &str, font_size: f32) {
        log::info!("Reinit font: {font_size}px (scaled for display)");
        self.font = FontManager::new(font_path, font_size);
        self.shaper.clear();
        #[cfg(feature = "gpu")]
        if let Some(g) = &mut self.gpu {
            g.atlas.clear();
            g.color_atlas.clear();
        }
        self.invalidate();
    }

    /// Enable / disable ligature shaping (config `font_ligatures`).
    pub fn set_ligatures(&mut self, on: bool) {
        if self.ligatures != on {
            self.ligatures = on;
            self.invalidate();
        }
    }

    pub fn ligatures(&self) -> bool { self.ligatures }

    /// Drop all cached pixels/hashes; the next frame is a full redraw.
    pub fn invalidate(&mut self) {
        self.force_full = true;
        self.tiles.clear();
    }

    /// Record a profiling sample (no-op unless RIFT_PROFILE=1).
    #[inline]
    pub fn prof_add(&mut self, phase: Phase, d: Duration) {
        if let Some(p) = &mut self.prof { p.add(phase, d); }
    }

    /// Mark the end of a presented frame (prints a report every 120 frames).
    pub fn prof_frame_end(&mut self) {
        if let Some(p) = &mut self.prof { p.frame_end(); }
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
        self.render_tabbed_with_cmd(wm, content_area, buffer, width, height, false, blocks, SplitUiState::default());
    }

    pub fn render_tabbed_with_cmd(
        &mut self,
        wm: &WindowManager,
        content_area: PaneRect,
        buffer: &mut [u32],
        width: u32,
        height: u32,
        cmd_held: bool,
        _blocks: &crate::tools::blocks::BlockManager, // block chrome is an overlay: see blocks_ui::draw
        split_ui: SplitUiState,
    ) {
        let t_start = Instant::now();
        let w = width as usize;
        let h = height as usize;
        let n = buffer.len();

        let bg = pack(self.theme.bg.0, self.theme.bg.1, self.theme.bg.2);
        let tab_bar_h = content_area.y;

        let layouts = wm.pane_layouts(content_area);
        let active_tab = wm.active_tab();
        let tabs = wm.tab_bar_info();

        // Everything that forces a full repaint is folded into one signature.
        let cw = self.font.cell_width;
        let ch = self.font.cell_height;
        let mut sig = mix(0, w as u64);
        sig = mix(sig, h as u64);
        sig = mix(sig, n as u64);
        sig = mix(sig, cw as u64);
        sig = mix(sig, ch as u64);
        sig = mix(sig, cmd_held as u64);
        sig = mix(sig, rgb_bits(self.theme.fg));
        sig = mix(sig, rgb_bits(self.theme.bg));
        sig = mix(sig, rgb_bits(self.theme.cursor));
        for c in self.theme.palette { sig = mix(sig, rgb_bits(c)); }
        for v in [content_area.x, content_area.y, content_area.width, content_area.height] {
            sig = mix(sig, v as u64);
        }
        for (idx, rect, is_active) in &layouts {
            for v in [*idx, rect.x, rect.y, rect.width, rect.height, *is_active as usize] {
                sig = mix(sig, v as u64);
            }
        }
        let mut tsig = mix(sig, tabs.len() as u64);
        for (title, active) in &tabs {
            for b in title.bytes() { tsig = mix(tsig, b as u64); }
            tsig = mix(tsig, *active as u64 + 0x100);
        }
        tsig = mix(tsig, self.chrome.tab_sig());

        // Effects (GPU shader, or the cheap CPU fallback) post-process the
        // output copy only; the back buffer stays unshaded, so damage
        // tracking keeps working with an effect active.
        let full = self.force_full
            || self.back.len() != n
            || sig != self.frame_sig;
        self.force_full = false;
        self.frame_sig = sig;
        self.full_frame = full;

        #[cfg(feature = "gpu")]
        let gpu_on = self.gpu_text_active();
        #[cfg(not(feature = "gpu"))]
        let gpu_on = false;

        let mut back = std::mem::take(&mut self.back);
        if full {
            back.clear();
            back.resize(n, bg);
            self.pane_rows.clear();
            #[cfg(feature = "gpu")]
            if let (true, Some(g)) = (gpu_on, &mut self.gpu) {
                // GPU text: pane areas are transparent in the UI layer and
                // every pane row gets an instance slot.
                let key = gpu_text::KEY | bg;
                for (_, rect, _) in layouts.iter() {
                    let x1 = rect.right().min(w);
                    for y in rect.y..rect.bottom().min(h) {
                        if x1 > rect.x { back[y * w + rect.x..y * w + x1].fill(key); }
                    }
                }
                let panes: Vec<(usize, PaneRect)> = layouts.iter().map(|(i, r, _)| (*i, *r)).collect();
                g.begin_rebuild();
                g.layout(&panes, cw, ch);
            }
        }
        #[cfg(feature = "gpu")]
        if let (true, Some(g)) = (gpu_on, &mut self.gpu) {
            g.atlas.begin_frame();
            g.color_atlas.begin_frame();
        }

        for (idx, rect, is_active) in layouts.iter() {
            if let Some(pane) = active_tab.pane(*idx) {
                let terminal = &pane.terminal;
                self.cur_pane = *idx;
                self.render_pane_inner(terminal, &mut back, w, h, *rect, *is_active, cmd_held);
            }
        }
        self.cur_pane = NO_CACHE;

        // Tab bar rendered LAST so it's always on top of pane content
        if full || tsig != self.tab_sig {
            self.render_tab_bar(wm, &mut back, w, tab_bar_h);
            self.tab_sig = tsig;
        }

        // The softbuffer buffer is not preserved between frames: copy our
        // persistent frame into it (cheap memcpy), overlays go on top of that.
        buffer.copy_from_slice(&back);
        self.back = back;

        #[cfg(feature = "gpu")]
        if gpu_on {
            self.gpu_frame_tail(&layouts, wm, split_ui.hover_pane);
            if self.gpu.as_mut().map_or(false, |g| g.take_stale()) {
                // Atlas pages were recycled (or full) mid-frame: rows cached
                // earlier may point at replaced glyphs. Rebuild everything on
                // the very next frame.
                self.force_full = true;
                self.redraw_requested = true;
            }
        }

        // Split chrome is composited onto the output buffer (never the
        // persistent back buffer) so dimming cannot compound across frames.
        if layouts.len() > 1 {
            let borders = active_tab.split_borders(content_area);
            if !gpu_on {
                self.dim_unfocused_panes(&layouts, buffer, w, h, split_ui.hover_pane);
            }
            self.render_pane_borders(&layouts, &borders, buffer, w, h, &split_ui);
        } else if split_ui.zoomed {
            if let Some((_, rect, _)) = layouts.first() {
                self.draw_zoom_badge(buffer, w, h, *rect);
            }
        }

        // Overlay scrollbar: output buffer only, so the damage-tracked back
        // buffer never sees it.
        if let Some(bar) = self.chrome.scrollbar {
            if let (Some((_, rect, _)), Some(pane)) =
                (layouts.iter().find(|(i, _, _)| *i == bar.pane), active_tab.pane(bar.pane))
            {
                let t = &pane.terminal;
                if !t.is_alt_screen() {
                    if let Some(th) = crate::ui::scrollbar::thumb(rect.height, t.rows, t.scrollback_len(), t.scroll_offset) {
                        crate::ui::scrollbar::draw(buffer, w, *rect, cw, th, bar.alpha, bar.emphasized, self.theme.fg);
                    }
                }
            }
        }

        let elapsed = self.start_time.elapsed().as_secs_f32();
        self.shader.apply(buffer, width, height, elapsed);
        self.prof_add(Phase::Render, t_start.elapsed());
    }

    /// Effect to hand to the GPU pipeline this frame (None = passthrough).
    #[cfg(feature = "gpu")]
    pub fn active_effect(&self) -> Option<crate::effects::ActiveEffect> {
        self.shader.kind().map(|kind| crate::effects::ActiveEffect {
            kind,
            intensity: self.shader.intensity(),
            bg: self.theme.bg,
            accent: self.theme.accent(),
        })
    }

    fn render_tab_bar(
        &mut self,
        wm: &WindowManager,
        buffer: &mut [u32],
        buf_width: usize,
        bar_height: usize,
    ) {
        use crate::ui::tabbar::{self, TabHit};
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
        let buf_height = buffer.len() / buf_width.max(1);

        let active_text = self.theme.fg;
        let inactive_text = dim(self.theme.fg, 0.45);

        // Active tab = terminal bg (content flows into the tab)
        let active_bg_px = pack_rgb(self.theme.bg);
        let hover_px = pack_rgb(lighten(bar_bg, 12));
        let drag_px = pack_rgb(lighten(bar_bg, 22));

        let tab_count = tabs.len().max(1);
        let layout = tabbar::layout(buf_width, tab_count, bar_height);
        let show_close = tab_count > 1;
        let close_w = cw * 2;
        let hover = self.chrome.tab_hover;

        // Find active tab index for separator logic
        let active_idx = tabs.iter().position(|(_, a)| *a);

        for (i, (title, is_active)) in tabs.iter().enumerate() {
            let (x0, x1) = layout.tabs[i];
            if x0 >= buf_width { break; }
            let hovered = hover.and_then(|h| h.tab()) == Some(i);
            let dragging = self.chrome.dragging_tab == Some(i);

            if *is_active {
                fill_tab_shape(buffer, buf_width, x0, x1, bar_height, active_bg_px);
            } else if dragging {
                fill_tab_shape(buffer, buf_width, x0, x1, bar_height, drag_px);
            } else if hovered {
                fill_tab_shape(buffer, buf_width, x0, x1, bar_height, hover_px);
            }

            // Title centered, measured in terminal cells (CJK = 2 cells).
            let text_color = if *is_active || hovered { active_text } else { inactive_text };
            let reserve = if show_close { 4 } else { 2 };
            let max_cols = ((x1 - x0) / cw.max(1)).saturating_sub(reserve);
            let (title_str, cols) = tabbar::fit_title(title, max_cols);
            let text_x = x0 + ((x1 - x0).saturating_sub(cols * cw)) / 2;
            tabbar::draw_text(buffer, buf_width, buf_height, &mut self.font, &title_str, text_x, text_y, x1, text_color);

            // Close button, only while the tab is hovered.
            if show_close && hovered {
                let (c0, c1) = tabbar::close_zone((x0, x1), close_w);
                let close_hot = hover == Some(TabHit::Close(i));
                if close_hot {
                    let s = (ch + 2).min(c1 - c0).min(bar_height);
                    let bx = c0 + (c1 - c0).saturating_sub(s) / 2;
                    let by = bar_height.saturating_sub(s) / 2;
                    crate::ui::fill_rect(buffer, buf_width, bx, by, s, s, pack_rgb(lighten(bar_bg, 40)));
                }
                let gx = c0 + (c1 - c0).saturating_sub(cw) / 2;
                let color = if close_hot { active_text } else { dim(self.theme.fg, 0.6) };
                tabbar::draw_text(buffer, buf_width, buf_height, &mut self.font, "\u{00d7}", gx, text_y, c1, color);
            }
        }

        // "+" new-tab button
        {
            let (p0, p1) = layout.plus;
            let hot = hover == Some(TabHit::Plus);
            let s = (ch + 2).min(p1 - p0).min(bar_height);
            let bx = p0 + (p1 - p0).saturating_sub(s) / 2;
            let by = bar_height.saturating_sub(s) / 2;
            if hot {
                crate::ui::fill_rect(buffer, buf_width, bx, by, s, s, hover_px);
            }
            let gx = p0 + (p1 - p0).saturating_sub(cw) / 2;
            let color = if hot { active_text } else { inactive_text };
            tabbar::draw_text(buffer, buf_width, buf_height, &mut self.font, "+", gx, text_y, p1, color);
        }

        // MCP indicator: only while an agent is connected, and only if it fits
        // to the right of the "+" button (never covers a tab).
        if self.chrome.mcp_clients > 0 {
            let label = crate::mcp::overlay::indicator(self.chrome.mcp_clients as usize);
            let w = (tabbar::text_cols(&label) + 2) * cw;
            let x0 = buf_width.saturating_sub(w + cw / 2);
            if x0 > layout.plus.1 + cw {
                let h = (ch + 2).min(bar_height);
                let y0 = bar_height.saturating_sub(h) / 2;
                crate::ui::fill_rect(buffer, buf_width, x0, y0, w, h, pack_rgb(lighten(bar_bg, 14)));
                tabbar::draw_text(buffer, buf_width, buf_height, &mut self.font, &label, x0 + cw, text_y, x0 + w, self.theme.accent());
            }
        }

        // Bottom border: 1px line under inactive regions only
        let sep_px = pack_rgb(darken(self.theme.bg, 10));
        let sep_y = bar_height.saturating_sub(1);
        let (ax0, ax1) = match active_idx {
            Some(ai) => {
                let m = 4;
                (layout.tabs[ai].0 + m, layout.tabs[ai].1.saturating_sub(m))
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

    /// Render one pane into `buffer`, repainting only rows whose content hash
    /// changed since the previous frame (`self.cur_pane` selects the hash
    /// cache slot; `NO_CACHE` or a full frame repaints every row). The
    /// repainted rows are reported in `self.dirty_rows`.
    ///
    /// The caller guarantees that for non-dirty rows `buffer` still holds the
    /// pixels produced for the identical hash last time (the persistent back
    /// buffer), and that anything affecting all rows (theme, font metrics,
    /// layout, Cmd state) is covered by the frame signature.
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
        self.set_screen_state(terminal);

        // Collapsed command blocks change which lines are shown (see
        // blocks_ui::view); otherwise this is the plain screen / scrollback view.
        let folded = crate::blocks_ui::view::folded_view(terminal);
        let visible = match &folded {
            Some(f) => f.cell_rows(terminal),
            None => terminal.visible_rows(),
        };
        self.view_cursor_row = match &folded {
            Some(f) => crate::blocks_ui::view::row_of_line(&f.rows, terminal.abs_cursor_line())
                .unwrap_or(usize::MAX),
            None => terminal.cursor_row,
        };
        let view_cursor_row = self.view_cursor_row;
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
        let images_visible = !terminal.is_scrolled_back() && folded.is_none();
        let has_images = images_visible && terminal.image_store.has_placements();

        // Scrollback indicator text (drawn over the first rows).
        let indicator = if terminal.is_scrolled_back() {
            Some(format!("[{}/{}]", terminal.scroll_offset, terminal.scrollback_len()))
        } else {
            None
        };
        let ind_rows = ((ch + 8).div_ceil(ch.max(1))).min(visible.len());
        let ind_hash = indicator.as_ref().map_or(0, |t| {
            t.bytes().fold(0x9e37u64, |h, b| mix(h, b as u64))
        });

        // OSC 8 link under the pointer (underlined while hovered).
        let hover_link: u16 = match self.link_hover {
            Some((pane, r, c)) if pane == self.cur_pane => visible.get(r).map_or(0, |cells| {
                match cells.get(c) {
                    Some(cell) if cell.c == '\0' && c > 0 => cells[c - 1].link(),
                    Some(cell) => cell.link(),
                    None => 0,
                }
            }),
            _ => 0,
        };
        self.cur_hover_link = hover_link;
        // Screen-wide state that recolors every cell: DECSCNM + palette overrides.
        let screen_sig = terminal.reverse_screen as u64 | (terminal.palette_gen as u64) << 1;

        // ── Damage detection: hash every visible row ──
        let key = self.cur_pane;
        let use_cache = key != NO_CACHE && !self.full_frame;
        let old_hashes = if use_cache { self.pane_rows.remove(&key).unwrap_or_default() } else { Vec::new() };
        let nrows = visible.len();
        let mut new_hashes: Vec<u64> = Vec::with_capacity(nrows);
        let mut dirty = std::mem::take(&mut self.dirty_rows);
        dirty.clear();
        for (row, cells) in visible.iter().enumerate() {
            let mut h = hash_cells(mix(mix(0, cells.len() as u64), screen_sig), cells, hover_link);
            if show_cursor && row == view_cursor_row {
                h = mix(h, 1 << 40 | (terminal.cursor_col as u64) << 8 | terminal.cursor_style as u64);
            }
            if has_images {
                for col in 0..cells.len() {
                    if let Some(ic) = terminal.image_store.get_cell(row, col) {
                        h = mix(h, ic.image_id as u64 | (ic.offset_x as u64) << 32);
                        h = mix(h, ic.offset_y as u64 | (col as u64) << 32);
                        if let Some(img) = terminal.image_store.get_image(ic.image_id) {
                            h = mix(h, img.data.as_ptr() as u64);
                            h = mix(h, (img.width as u64) << 32 | img.height as u64);
                            h = mix(h, img.data.len() as u64);
                            if let Some(p) = &img.placement {
                                h = mix(h, (p.cell_rows as u64) << 32 | p.cell_cols as u64);
                            }
                        }
                    }
                }
            }
            #[cfg(feature = "gpu")]
            {
                let hl = self.hl_row_hash(row);
                if hl != 0 { h = mix(h, hl); }
            }
            if row < ind_rows { h = mix(h, ind_hash); }
            // Edge rows may carry pane-border pixels that interior rows don't.
            if row == 0 || row + 1 == nrows { h = mix(h, 0xed9e + row.min(1) as u64); }
            dirty.push(old_hashes.get(row) != Some(&h));
            new_hashes.push(h);
        }

        #[cfg(feature = "gpu")]
        let gpu_paint = self.gpu_text_on();
        #[cfg(not(feature = "gpu"))]
        let gpu_paint = false;
        #[cfg(feature = "gpu")]
        if gpu_paint {
            self.gpu_paint_pane(
                terminal, &visible, &dirty, old_hashes.len(), buffer, buf_width, buf_height,
                rect, show_cursor, images_visible, cmd_held,
            );
        }

        for (row, cells) in visible.iter().enumerate() {
            if gpu_paint || !dirty[row] { continue; }
            let url_ranges: Vec<(usize, usize)> = if cmd_held {
                let line: String = cells.iter().map(|c| c.c).collect();
                crate::tools::url_detect::detect_urls(&line)
                    .into_iter()
                    .map(|(start, end, _)| (start, end))
                    .collect()
            } else {
                Vec::new()
            };
            self.render_row(
                terminal, row, cells, buffer, buf_width, buf_height, rect,
                show_cursor, images_visible, &url_ranges, cmd_held,
            );
        }

        // Rows that existed last frame but are gone now (short scrollback view):
        // clear their bands, as the old whole-frame fill did.
        if old_hashes.len() > nrows && !gpu_paint {
            let dbg = self.def_bg();
            let bg_px = pack(dbg.0, dbg.1, dbg.2);
            let x1 = (rect.x + rect.width).min(buf_width);
            for row in nrows..old_hashes.len() {
                let y0 = rect.y + row * ch;
                if y0 + ch > rect.y + rect.height || y0 + ch > buf_height || x1 <= rect.x { continue; }
                for cy in 0..ch {
                    let o = (y0 + cy) * buf_width;
                    buffer[o + rect.x..o + x1].fill(bg_px);
                }
            }
        }

        // Scrollback indicator: repaint whenever any row under it was repainted
        if let Some(indicator) = indicator {
            if dirty.iter().take(ind_rows).any(|d| *d) {
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

        if key != NO_CACHE {
            self.pane_rows.insert(key, new_hashes);
        }
        self.dirty_rows = dirty;
        self.clear_screen_state();
    }

    /// Repaint a single terminal row: clear its band to the theme bg, then
    /// draw every cell (glyphs through the pre-blended tile cache).
    fn render_row(
        &mut self,
        terminal: &Terminal,
        row: usize,
        cells: &[Cell],
        buffer: &mut [u32],
        buf_width: usize,
        buf_height: usize,
        rect: PaneRect,
        show_cursor: bool,
        images_visible: bool,
        url_ranges: &[(usize, usize)],
        url_cmd: bool,
    ) {
        let cw = self.font.cell_width;
        let ch = self.font.cell_height;
        let y0 = rect.y + row * ch;
        if y0 + ch > rect.y + rect.height || y0 + ch > buf_height { return; }

        // Row band background (replaces the old whole-frame buffer.fill).
        let dbg = self.def_bg();
        let theme_bg_px = pack(dbg.0, dbg.1, dbg.2);
        // Under DECSCNM the whole pane is inverted, margins included.
        let band_end = if self.scr_reverse { rect.x + rect.width } else { rect.x + cells.len().max(terminal.cols) * cw };
        let bx1 = band_end.min(rect.x + rect.width).min(buf_width);
        if bx1 > rect.x {
            for cy in 0..ch {
                let o = (y0 + cy) * buf_width;
                buffer[o + rect.x..o + bx1].fill(theme_bg_px);
            }
        }

        for (col, cell) in cells.iter().enumerate() {
            let x0 = rect.x + col * cw;
            if x0 + cw > rect.x + rect.width { continue; }
            if x0 + cw > buf_width { continue; }

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
                self.resolve_fg(cell),
                self.resolve(cell.bg, false),
            );
            if cell.reverse() { std::mem::swap(&mut fg, &mut bg); }
            if cell.dim() {
                fg = (fg.0 / 2, fg.1 / 2, fg.2 / 2);
            }

            // Skip wide-char continuation placeholder
            if cell.c == '\0' { continue; }

            let style = font::style_bits(cell.bold(), cell.italic());
            let deco = if cell.hidden() { Deco::default() } else { cell_deco(cell) };
            let ul_rgb = match cell.underline_color() {
                Some(c) if deco.ul != UnderlineStyle::None => self.resolve(c, true),
                _ => fg,
            };

            let is_cursor = show_cursor
                && row == self.view_cursor_row
                && col == terminal.cursor_col;

            let has_glyph = cell.c != ' ' && !cell.hidden();
            let wide = has_glyph && font::is_wide(cell.c);

            if (has_glyph || deco.any()) && !wide && !is_cursor {
                // Fast path: one pre-blended (glyph, fg, bg) tile, copied row-wise.
                let under = if cell.bg != Color::Default || cell.reverse() { bg } else { dbg };
                let fg_px = pack(fg.0, fg.1, fg.2);
                let bg_px = pack(under.0, under.1, under.2);
                let ul_px = pack(ul_rgb.0, ul_rgb.1, ul_rgb.2);
                let dbits = if deco.any() {
                    deco.bits() | (if deco.ul != UnderlineStyle::None { (ul_px as u64) << 8 } else { 0 })
                } else { 0 };
                let tkey = (if has_glyph { cell.c } else { ' ' }, fg_px, bg_px, dbits | (style as u64) << 40);
                if !self.tiles.promote(tkey) {
                    let dm = self.font.deco;
                    let bmp: &[u8] = if has_glyph { self.font.rasterize_styled(cell.c, style) } else { &[] };
                    let tile = build_tile(bmp, cw, ch, fg, fg_px, bg_px, deco.any().then_some((&deco, &dm, ul_px)));
                    self.tiles.insert(tkey, tile);
                }
                blit_tile(buffer, buf_width, x0, y0, cw, ch, self.tiles.get(&tkey));
            } else {
                self.draw_cell_slow(
                    cell, fg, bg, is_cursor, wide, has_glyph, terminal.cursor_style,
                    style, deco, ul_rgb, buffer, buf_width, x0, y0,
                );
            }

            // Combining marks from the cluster extras, overlaid on the base glyph.
            if cell.extra_id() != 0 && !cell.hidden() {
                let extra = cell.extra();
                let on_block = is_cursor && terminal.cursor_style == crate::terminal::CursorStyle::Block;
                let color = if on_block { self.theme.bg } else { fg };
                self.overlay_marks(&extra, color, font::is_wide(cell.c), buffer, buf_width, x0, y0);
            }

            // OSC 8 hyperlinks: dotted accent underline while Cmd is held
            // (all links) or when the pointer hovers the link.
            let link = cell.link();
            if link != 0 && (url_cmd || link == self.cur_hover_link) {
                self.draw_link_underline(buffer, buf_width, buf_height, x0, y0, if font::is_wide(cell.c) { cw * 2 } else { cw });
            }

            // URL: underline + accent color (only when Cmd held)
            if !url_ranges.is_empty() {
                let is_url = url_ranges.iter().any(|&(s, e)| col >= s && col < e);
                if is_url {
                    // Recolor text to accent/cursor color
                    let accent = self.theme.cursor;
                    if cell.c != ' ' && !cell.hidden() {
                        let wide = font::is_wide(cell.c);
                        let gw = if wide { cw * 2 } else { cw };
                        let bitmap = if wide { self.font.rasterize_wide_styled(cell.c, style) } else { self.font.rasterize_styled(cell.c, style) };
                        for cy in 0..ch {
                            for cx in 0..gw.min(buf_width - x0) {
                                let coverage = bitmap[cy * gw + cx] as u32;
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

    /// Rasterize each combining mark in `extra` and blend it over the base
    /// glyph's cell box at (`x0`, `y0`).
    fn overlay_marks(
        &mut self, extra: &str, color: Rgb, wide: bool,
        buffer: &mut [u32], buf_width: usize, x0: usize, y0: usize,
    ) {
        let cw = self.font.cell_width;
        let ch = self.font.cell_height;
        let w = if wide { cw * 2 } else { cw };
        for m in extra.chars().filter(|&c| font::is_overlay_mark(c)) {
            let bmp = self.font.rasterize_mark(m, wide);
            for cy in 0..ch {
                for cx in 0..w.min(buf_width.saturating_sub(x0)) {
                    let cov = bmp[cy * w + cx] as u32;
                    if cov == 0 { continue; }
                    let idx = (y0 + cy) * buf_width + x0 + cx;
                    if idx < buffer.len() {
                        buffer[idx] = blend(color, buffer[idx], cov);
                    }
                }
            }
        }
    }

    /// Dotted underline in the accent color (hyperlink affordance).
    fn draw_link_underline(
        &self, buffer: &mut [u32], buf_width: usize, buf_height: usize,
        x0: usize, y0: usize, w: usize,
    ) {
        let uy = y0 + self.font.cell_height.saturating_sub(2);
        if uy >= buf_height { return; }
        let a = self.theme.cursor;
        let px = pack(a.0, a.1, a.2);
        for cx in (0..w).filter(|cx| cx % 2 == 0) {
            let idx = uy * buf_width + x0 + cx;
            if x0 + cx < buf_width && idx < buffer.len() { buffer[idx] = px; }
        }
    }

    /// Per-pixel blending path: cursor cells, wide glyphs, and cells without
    /// a glyph (background only). Mirrors the original renderer exactly.
    fn draw_cell_slow(
        &mut self,
        cell: &Cell,
        fg: Rgb,
        bg: Rgb,
        is_cursor: bool,
        wide: bool,
        has_glyph: bool,
        cursor_style: crate::terminal::CursorStyle,
        style: u8,
        deco: Deco,
        ul_rgb: Rgb,
        buffer: &mut [u32],
        buf_width: usize,
        x0: usize,
        y0: usize,
    ) {
        let cw = self.font.cell_width;
        let ch = self.font.cell_height;

        // Determine cursor width (2 cells for wide chars)
        let cursor_w = if is_cursor {
            use unicode_width::UnicodeWidthChar;
            cell.c.width().unwrap_or(1).max(1) * cw
        } else { cw };

        // Background fill
        if cell.bg != Color::Default || cell.reverse() || (is_cursor && cursor_style == crate::terminal::CursorStyle::Block) {
            let fill = if is_cursor && cursor_style == crate::terminal::CursorStyle::Block {
                self.theme.cursor
            } else { bg };
            let px = pack(fill.0, fill.1, fill.2);
            let fill_w = if is_cursor { cursor_w } else if wide { cw * 2 } else { cw };
            for cy in 0..ch {
                let offset = (y0 + cy) * buf_width + x0;
                if offset + fill_w <= buffer.len() {
                    buffer[offset..offset + fill_w].fill(px);
                }
            }
        }

        // Bar cursor (left 2px)
        if is_cursor && cursor_style == crate::terminal::CursorStyle::Bar {
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
        if is_cursor && cursor_style == crate::terminal::CursorStyle::Underline {
            let ul_px = pack(self.theme.cursor.0, self.theme.cursor.1, self.theme.cursor.2);
            for dy in 0..2usize {
                let uy = y0 + ch.saturating_sub(1 + dy);
                for cx in 0..cursor_w {
                    let idx = uy * buf_width + x0 + cx;
                    if idx < buffer.len() { buffer[idx] = ul_px; }
                }
            }
        }

        if has_glyph {
            let text_color = if is_cursor && cursor_style == crate::terminal::CursorStyle::Block {
                self.theme.bg
            } else { fg };
            let fg_px = pack(text_color.0, text_color.1, text_color.2);
            let gw = if wide { cw * 2 } else { cw };
            let bitmap = if wide { self.font.rasterize_wide_styled(cell.c, style) } else { self.font.rasterize_styled(cell.c, style) };
            for cy in 0..ch {
                let row_off = (y0 + cy) * buf_width + x0;
                let bmp_off = cy * gw;
                for cx in 0..gw.min(buf_width - x0) {
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

        if deco.any() {
            let text_color = if is_cursor && cursor_style == crate::terminal::CursorStyle::Block {
                self.theme.bg
            } else { fg };
            let ul_c = if cell.underline_color().is_some() && !(is_cursor && cursor_style == crate::terminal::CursorStyle::Block) { ul_rgb } else { text_color };
            let w = (if wide { cw * 2 } else { cursor_w.max(cw) }).min(buf_width.saturating_sub(x0));
            let dm = self.font.deco;
            font::paint_decor(
                buffer, buf_width, x0, y0, w, ch, &deco, &dm,
                pack(text_color.0, text_color.1, text_color.2), pack(ul_c.0, ul_c.1, ul_c.2),
            );
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

    /// Blend every unfocused pane toward the theme background so the active
    /// pane stands out. The hovered pane is dimmed half as much.
    fn dim_unfocused_panes(
        &self,
        layouts: &[(usize, PaneRect, bool)],
        buffer: &mut [u32],
        buf_width: usize,
        buf_height: usize,
        hover_pane: Option<usize>,
    ) {
        let bg = [self.theme.bg.0 as i32, self.theme.bg.1 as i32, self.theme.bg.2 as i32];
        for (idx, rect, is_active) in layouts {
            if *is_active { continue; }
            let k = if hover_pane == Some(*idx) { UNFOCUSED_DIM * 0.5 } else { UNFOCUSED_DIM };
            let k = (k.clamp(0.0, 1.0) * 256.0) as i32;
            let x0 = rect.x.min(buf_width);
            let x1 = rect.right().min(buf_width);
            for y in rect.y..rect.bottom().min(buf_height) {
                let row = &mut buffer[y * buf_width + x0..y * buf_width + x1];
                for px in row.iter_mut() {
                    let p = *px;
                    let r = ((p >> 16) & 0xff) as i32;
                    let g = ((p >> 8) & 0xff) as i32;
                    let b = (p & 0xff) as i32;
                    let r = r + (((bg[0] - r) * k) >> 8);
                    let g = g + (((bg[1] - g) * k) >> 8);
                    let b = b + (((bg[2] - b) * k) >> 8);
                    *px = (r as u32) << 16 | (g as u32) << 8 | b as u32;
                }
            }
        }
    }

    /// Ghostty-style 1px dividers: muted by default, tinted with the cursor
    /// color where they touch the active pane, and a 3px highlight while
    /// hovered or dragged.
    fn render_pane_borders(
        &self,
        layouts: &[(usize, PaneRect, bool)],
        borders: &[SplitBorder],
        buffer: &mut [u32],
        buf_width: usize,
        buf_height: usize,
        ui: &SplitUiState,
    ) {
        let muted = lighten(self.theme.bg, DIVIDER_LIGHTEN);
        let muted_px = pack(muted.0, muted.1, muted.2);
        let tint_px = blend(self.theme.cursor, muted_px, 153); // cursor @ 60%
        let hot = self.theme.cursor;
        let hot_px = pack(hot.0, hot.1, hot.2);
        let active = layouts.iter().find(|l| l.2).map(|l| l.1);

        for (i, b) in borders.iter().enumerate() {
            let hot_border = match ui.dragging_border {
                Some(d) => d == i,
                None => ui.hover_border == Some(i),
            };
            let horizontal = b.dir == SplitDir::Horizontal;
            let (span0, span1) = if horizontal {
                (b.area.y, b.area.bottom())
            } else {
                (b.area.x, b.area.right())
            };
            // Part of the divider that touches the active pane.
            let tinted = active.and_then(|a| {
                let (touches, a0, a1) = if horizontal {
                    (a.right() == b.pos || a.x == b.pos + BORDER, a.y, a.bottom())
                } else {
                    (a.bottom() == b.pos || a.y == b.pos + BORDER, a.x, a.right())
                };
                let (s, e) = (a0.max(span0), a1.min(span1));
                (touches && s < e).then_some((s, e))
            });
            // Pixel at (position along the divider, offset across it).
            let at = |t: usize, off: isize| -> Option<usize> {
                let across = b.pos.checked_add_signed(off)?;
                let (x, y) = if horizontal { (across, t) } else { (t, across) };
                (x < buf_width && y < buf_height).then(|| y * buf_width + x)
            };
            for t in span0..span1 {
                let px = match tinted {
                    Some((s, e)) if t >= s && t < e => tint_px,
                    _ => muted_px,
                };
                if let Some(i) = at(t, 0) { buffer[i] = px; }
                if hot_border {
                    if let Some(i) = at(t, 0) { buffer[i] = hot_px; }
                    for off in [-1isize, 1] {
                        if let Some(i) = at(t, off) {
                            buffer[i] = blend(hot, buffer[i], 120);
                        }
                    }
                }
            }
        }
    }

    /// Small "ZOOM" tag in the bottom-right corner of a zoomed pane.
    fn draw_zoom_badge(&mut self, buffer: &mut [u32], buf_width: usize, buf_height: usize, rect: PaneRect) {
        let cw = self.font.cell_width;
        let ch = self.font.cell_height;
        let label = "ZOOM";
        let bw = label.len() * cw + 12;
        let bh = ch + 4;
        if rect.width < bw + 8 || rect.height < bh + 8 { return; }
        let x = rect.right() - bw - 6;
        let y = rect.bottom() - bh - 6;
        let c = self.theme.cursor;
        let badge_px = blend(c, pack(self.theme.bg.0, self.theme.bg.1, self.theme.bg.2), 200);
        for yy in y..(y + bh).min(buf_height) {
            let o = yy * buf_width;
            if o + x + bw <= buffer.len() { buffer[o + x..o + x + bw].fill(badge_px); }
        }
        self.draw_char_seq(buffer, buf_width, buf_height, label, x + 6, y + 2, self.theme.bg);
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
                let (mut fg, mut bg) = (self.resolve_fg(cell), self.resolve(cell.bg, false));
                if cell.reverse() { std::mem::swap(&mut fg, &mut bg); }
                if cell.dim() { fg = (fg.0 / 2, fg.1 / 2, fg.2 / 2); }

                let is_cursor = row == cursor_row && col == cursor_col;

                let fill = if is_cursor { self.theme.cursor } else { bg };
                if is_cursor || cell.bg != Color::Default || cell.reverse() {
                    let px = pack(fill.0, fill.1, fill.2);
                    for cy in 0..ch {
                        let offset = (y0 + cy) * buf_width + x0;
                        if offset + cw <= buffer.len() {
                            buffer[offset..offset + cw].fill(px);
                        }
                    }
                }

                if cell.c != ' ' && !cell.hidden() {
                    let text_color = if is_cursor { self.theme.bg } else { fg };
                    let style = font::style_bits(cell.bold(), cell.italic());
                    let bitmap = self.font.rasterize_styled(cell.c, style);
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
                if !cell.hidden() {
                    let deco = cell_deco(cell);
                    if deco.any() {
                        let ul_c = cell.underline_color().map_or(if is_cursor { self.theme.bg } else { fg }, |c| self.resolve(c, true));
                        let tc = if is_cursor { self.theme.bg } else { fg };
                        let dm = self.font.deco;
                        font::paint_decor(buffer, buf_width, x0, y0, cw, ch, &deco, &dm,
                            pack(tc.0, tc.1, tc.2), pack(ul_c.0, ul_c.1, ul_c.2));
                    }
                }
            }
        }
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.invalidate();
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
        row_abs: &[Option<usize>],
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

        for row in 0..max_row.min(row_abs.len()) {
            let Some(abs) = row_abs[row] else { continue };
            for col in 0..max_col {
                if sel.contains(abs, col) {
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

    /// Foreground of `cell`, honoring "bold is bright" for palette 0-7.
    #[inline]
    fn resolve_fg(&self, cell: &Cell) -> Rgb {
        if self.bold_is_bright && cell.bold() {
            if let Color::Indexed(i @ 0..=7) = cell.fg {
                return self.indexed(i + 8);
            }
        }
        self.resolve(cell.fg, true)
    }

    /// Enable/disable drawing bold text with bright palette colors (0-7 -> 8-15).
    pub fn set_bold_is_bright(&mut self, on: bool) {
        if self.bold_is_bright != on {
            self.bold_is_bright = on;
            self.invalidate();
        }
    }

    /// Default foreground, honoring DECSCNM.
    #[inline]
    fn def_fg(&self) -> Rgb { if self.scr_reverse { self.theme.bg } else { self.theme.fg } }

    /// Default background, honoring DECSCNM.
    #[inline]
    fn def_bg(&self) -> Rgb { if self.scr_reverse { self.theme.fg } else { self.theme.bg } }

    /// Indexed color: OSC 4 override of the current pane, else the theme.
    #[inline]
    fn indexed(&self, idx: u8) -> Rgb {
        match self.pal_over.get(idx as usize) {
            Some(Some(c)) => *c,
            _ => self.theme.resolve_indexed(idx),
        }
    }

    /// Latch per-pane screen state (DECSCNM, OSC 4 overrides) for `resolve`.
    fn set_screen_state(&mut self, terminal: &Terminal) {
        self.scr_reverse = terminal.reverse_screen;
        self.pal_over.clear();
        if terminal.palette_gen != 0 {
            self.pal_over.extend((0..=255u8).map(|i| terminal.palette_override(i)));
        }
    }

    fn clear_screen_state(&mut self) {
        self.scr_reverse = false;
        self.pal_over.clear();
    }

    fn resolve(&self, color: Color, is_fg: bool) -> Rgb {
        match color {
            Color::Default => if is_fg { self.def_fg() } else { self.def_bg() },
            Color::Indexed(idx) => self.indexed(idx),
            Color::Rgb(r, g, b) => (r, g, b),
        }
    }
}

use crate::ui::{pack, pack_rgb, darken, dim, lighten};

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

/// Ghostty-style tab shape: top-rounded rect, bottom flush, side margins for
/// the gap between neighbours.
fn fill_tab_shape(buffer: &mut [u32], buf_width: usize, x0: usize, x1: usize, bar_height: usize, px: u32) {
    let margin = 4;
    let top = 3;
    let pl = x0 + margin;
    let pr = x1.saturating_sub(margin);
    let r = 8i32;
    for y in top..bar_height {
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
                buffer[off..end].fill(px);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{Attrs, CursorStyle};

    const COLS: usize = 200;
    const ROWS: usize = 60;

    fn test_renderer() -> Renderer {
        let path = crate::config::find_font_path();
        Renderer::new(&path, 28.0, Theme::catppuccin_mocha())
    }

    /// Deterministic pseudo-random "program output" for row `seed`.
    fn fill_row(cells: &mut [Cell], seed: usize) {
        let mut x = (seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        for (i, cell) in cells.iter_mut().enumerate() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let r = (x >> 8) as u32;
            cell.c = if i % 9 == 0 { ' ' } else { (b'!' + (r % 90) as u8) as char };
            // Realistic palette use: a handful of distinct colors, not noise.
            cell.fg = match r % 6 {
                0 | 1 => Color::Default,
                2 => Color::Indexed(((r >> 4) % 8) as u8 + 1),
                3 => Color::Rgb(200, 120 + ((r >> 8) % 3) as u8 * 40, 90),
                _ => Color::Indexed(2),
            };
            cell.bg = if r % 23 == 0 { Color::Indexed(4) } else { Color::Default };
            cell.set_attrs(&Attrs { bold: r % 7 == 0, reverse: r % 31 == 0, ..Attrs::default() });
        }
    }

    fn make_terminal() -> Terminal {
        let mut t = Terminal::new(COLS, ROWS);
        for (r, row) in t.grid.iter_mut().enumerate() {
            fill_row(row, r);
        }
        t.cursor_row = ROWS - 1;
        t.cursor_col = 5;
        t
    }

    fn rect(r: &Renderer) -> (PaneRect, usize, usize) {
        let w = COLS * r.cell_width();
        let h = ROWS * r.cell_height();
        (PaneRect { x: 0, y: 0, width: w, height: h }, w, h)
    }

    /// Render `t` the way a fresh renderer would (every row, empty cache).
    fn render_fresh(r: &mut Renderer, t: &Terminal, cmd_held: bool, start: Instant) -> Vec<u32> {
        r.start_time = start;
        let (rc, w, h) = rect(&r);
        let bg = pack(r.theme.bg.0, r.theme.bg.1, r.theme.bg.2);
        let mut buf = vec![bg; w * h];
        r.cur_pane = NO_CACHE;
        r.full_frame = true;
        r.render_pane_inner(t, &mut buf, w, h, rc, true, cmd_held);
        buf
    }

    #[test]
    fn tile_cache_is_bounded_and_promotes() {
        let mut c = TileCache::default();
        for i in 0..10_000u32 {
            c.insert(('a', i, 0, 0), vec![i; 4].into_boxed_slice());
            assert!(c.cur.len() + c.old.len() <= TILE_CACHE_CAP);
        }
        // Recent entries survive; a hit in the old generation is promoted.
        assert!(c.promote(('a', 9_999, 0, 0)));
        assert!(!c.promote(('a', 0, 0, 0)));
        let old_key = *c.old.keys().next().unwrap();
        assert!(c.promote(old_key));
        assert_eq!(c.get(&old_key).len(), 4);
    }

    /// Damage-tracked rendering must be pixel-identical to a full repaint,
    /// across edits, cursor movement/styles, scrolling and scrollback view.
    #[test]
    fn damage_tracking_matches_full_render() {
        let mut r = test_renderer();
        let (rc, w, h) = rect(&r);
        let bg = pack(r.theme.bg.0, r.theme.bg.1, r.theme.bg.2);
        let mut back = vec![bg; w * h];
        let mut t = make_terminal();

        let mut counter = 0usize;
        let n = &mut counter;
        let mut fr = test_renderer();
        let fr = &mut fr;
        let mut step = |r: &mut Renderer, back: &mut Vec<u32>, t: &Terminal, first: bool, cmd: bool| {
            *n += 1;
            r.cur_pane = 0;
            r.full_frame = first;
            if first { r.pane_rows.clear(); }
            r.render_pane_inner(t, back, w, h, rc, true, cmd);
            let fresh = render_fresh(fr, t, cmd, r.start_time);
            let bad = back.iter().zip(&fresh).position(|(a, b)| a != b);
            if let Some(b) = bad { eprintln!("ch={} cw={} row={} a={:08x} b={:08x}", r.cell_height(), r.cell_width(), b / w / r.cell_height(), back[b], fresh[b]); }
            assert!(bad.is_none(), "diverged at pixel {:?} (x={}, y={}) in step {}", bad, bad.unwrap() % w, bad.unwrap() / w, *n);
        };

        step(&mut r, &mut back, &t, true, false);
        // idle frame: nothing changes, nothing repainted
        step(&mut r, &mut back, &t, false, false);
        assert!(r.dirty_rows.iter().all(|d| !d));
        // edit one row
        fill_row(&mut t.grid[10], 999);
        step(&mut r, &mut back, &t, false, false);
        assert_eq!(r.dirty_rows.iter().filter(|d| **d).count(), 1);
        // cursor moves: only old and new cursor rows repaint
        t.cursor_row = 20;
        t.cursor_col = 7;
        step(&mut r, &mut back, &t, false, false);
        assert_eq!(r.dirty_rows.iter().filter(|d| **d).count(), 2);
        // cursor styles
        for style in [CursorStyle::Bar, CursorStyle::Underline, CursorStyle::Block] {
            t.cursor_style = style;
            t.cursor_col = 8;
            step(&mut r, &mut back, &t, false, false);
        }
        // wide char under the cursor
        t.grid[20][8].c = '\u{4e2d}';
        t.grid[20][9].c = '\0';
        step(&mut r, &mut back, &t, false, false);
        // scroll by one line (new text at bottom)
        let first = t.grid.remove(0);
        t.scrollback.push_back(first);
        let mut last = vec![Cell::default(); COLS];
        fill_row(&mut last, 12345);
        t.grid.push(last);
        step(&mut r, &mut back, &t, false, false);
        // Cmd held (URL underline) then released
        t.grid[3][0].c = 'h';
        for (i, c) in "http://example.com/x".chars().enumerate() { t.grid[3][i].c = c; }
        step(&mut r, &mut back, &t, true, true);
        step(&mut r, &mut back, &t, true, false);
        // scrollback view + indicator
        t.scroll_offset = 4;
        step(&mut r, &mut back, &t, false, false);
        t.scroll_offset = 9;
        step(&mut r, &mut back, &t, false, false);
        t.scroll_offset = 0;
        step(&mut r, &mut back, &t, false, false);
    }

    /// Scrollback rows are stored without their trailing default blanks; a view
    /// that includes them must be pixel-identical to one whose rows are padded.
    #[test]
    fn trimmed_scrollback_rows_render_like_padded_rows() {
        let build = || {
            let mut t = Terminal::new(COLS, ROWS);
            let mut p = vte::Parser::new();
            let mut h = crate::terminal::AnsiHandler::new(&mut t);
            for i in 0..(ROWS * 2) {
                // Short lines, some with colored/reversed text and a colored blank tail.
                let line = match i % 4 {
                    0 => format!("\x1b[3{}mline {i}\x1b[0m\r\n", 1 + i % 6),
                    1 => format!("plain text {i}\r\n"),
                    2 => format!("\x1b[44m   bg {i}   \x1b[0m\r\n"),
                    _ => "\r\n".to_string(),
                };
                for b in line.bytes() {
                    p.advance(&mut h, b);
                }
            }
            t
        };
        let t = build();
        let mut padded = build();
        assert!(t.scrollback.iter().any(|r| r.len() < COLS / 2), "scrollback rows should be stored trimmed");
        for row in padded.scrollback.iter_mut() {
            row.resize(COLS, Cell::default());
        }
        let start = Instant::now();
        let mut ra = test_renderer();
        let mut rb = test_renderer();
        for offset in [0, 3, 40, t.scrollback.len()] {
            let (mut a, mut b) = (build(), build());
            for row in b.scrollback.iter_mut() {
                row.resize(COLS, Cell::default());
            }
            a.scroll_offset = offset;
            b.scroll_offset = offset;
            let fa = render_fresh(&mut ra, &a, false, start);
            let fb = render_fresh(&mut rb, &b, false, start);
            assert!(fa == fb, "trimmed vs padded scrollback differ at offset {offset}");
        }
        // Incremental (damage-tracked) rendering while scrolling through trimmed rows.
        let (rc, w, h) = rect(&ra);
        let bg = pack(ra.theme.bg.0, ra.theme.bg.1, ra.theme.bg.2);
        let mut back = vec![bg; w * h];
        let mut fr = test_renderer();
        let mut tt = build();
        for (i, offset) in [0usize, 5, 6, 30, 2, 0].into_iter().enumerate() {
            tt.scroll_offset = offset;
            ra.cur_pane = 0;
            ra.full_frame = i == 0;
            if i == 0 { ra.pane_rows.clear(); }
            ra.render_pane_inner(&tt, &mut back, w, h, rc, true, false);
            let fresh = render_fresh(&mut fr, &padded_copy(&tt), false, ra.start_time);
            assert!(back == fresh, "damage-tracked render of trimmed rows diverged at step {i}");
        }
    }

    /// Clone of the visible state with scrollback rows padded to full width.
    fn padded_copy(t: &Terminal) -> Terminal {
        let mut c = Terminal::new(t.cols, t.rows);
        c.scrollback = t.scrollback.iter().map(|r| {
            let mut v = r.clone();
            v.resize(COLS, Cell::default());
            v
        }).collect();
        for (dst, src) in c.grid.iter_mut().zip(t.grid.iter()) {
            dst.clone_from(src);
        }
        c.cursor_row = t.cursor_row;
        c.cursor_col = t.cursor_col;
        c.scroll_offset = t.scroll_offset;
        c
    }

    #[test]
    fn collapsed_block_view_matches_full_render() {
        let mut r = test_renderer();
        let (rc, w, h) = rect(&r);
        let bg = pack(r.theme.bg.0, r.theme.bg.1, r.theme.bg.2);
        let mut back = vec![bg; w * h];
        let mut fr = test_renderer();
        let mut t = make_terminal();
        // Move 8 rows into scrollback so folds can pull older lines in.
        for _ in 0..8 {
            let first = t.grid.remove(0);
            t.scrollback.push_back(first);
            let mut last = vec![Cell::default(); COLS];
            fill_row(&mut last, 777 + t.scrollback.len());
            t.grid.push(last);
        }
        let base = t.scrollback.len();
        t.blocks.on_prompt_start(base + 3);
        t.blocks.on_command_start(base + 3, 0);
        t.blocks.on_command_output(base + 4, "ls".into());
        t.blocks.on_command_finished(base + 9, Some(0));

        let mut first = true;
        let mut step = |r: &mut Renderer, back: &mut Vec<u32>, t: &Terminal, label: &str| {
            r.cur_pane = 0;
            r.full_frame = first;
            if first { r.pane_rows.clear(); }
            first = false;
            r.render_pane_inner(t, back, w, h, rc, true, false);
            let fresh = render_fresh(&mut fr, t, false, r.start_time);
            assert!(back.iter().zip(&fresh).all(|(a, b)| a == b), "diverged: {label}");
        };
        step(&mut r, &mut back, &t, "expanded");
        t.blocks.toggle_collapse(0);
        step(&mut r, &mut back, &t, "collapsed");
        assert!(crate::blocks_ui::view::folded_view(&t).is_some());
        t.cursor_col = 3;
        step(&mut r, &mut back, &t, "cursor move while collapsed");
        t.scroll_offset = 5;
        step(&mut r, &mut back, &t, "scrolled while collapsed");
        t.scroll_offset = 0;
        t.blocks.toggle_collapse(0);
        step(&mut r, &mut back, &t, "re-expanded");
    }

    /// Reference implementation of the pre-optimisation renderer for the
    /// common (non-cursor, narrow glyph) cells: per-pixel alpha blending.
    fn legacy_render(r: &mut Renderer, t: &Terminal, buffer: &mut [u32], w: usize) {
        let (cw, ch) = (r.font.cell_width, r.font.cell_height);
        let bg = pack(r.theme.bg.0, r.theme.bg.1, r.theme.bg.2);
        buffer.fill(bg);
        for (row, cells) in t.grid.iter().enumerate() {
            for (col, cell) in cells.iter().enumerate() {
                let (x0, y0) = (col * cw, row * ch);
                let (mut fg, mut cbg) = (r.resolve(cell.fg, true), r.resolve(cell.bg, false));
                if cell.reverse() { std::mem::swap(&mut fg, &mut cbg); }
                if cell.bg != Color::Default || cell.reverse() {
                    let px = pack(cbg.0, cbg.1, cbg.2);
                    for cy in 0..ch {
                        let o = (y0 + cy) * w + x0;
                        buffer[o..o + cw].fill(px);
                    }
                }
                if cell.c != ' ' {
                    let fg_px = pack(fg.0, fg.1, fg.2);
                    let bitmap = r.font.rasterize(cell.c);
                    for cy in 0..ch {
                        for cx in 0..cw {
                            let cov = bitmap[cy * cw + cx] as u32;
                            if cov == 0 { continue; }
                            let idx = (y0 + cy) * w + x0 + cx;
                            buffer[idx] = if cov >= 250 { fg_px } else { blend(fg, buffer[idx], cov) };
                        }
                    }
                }
            }
        }
    }

    fn time_ms(frames: usize, mut f: impl FnMut(usize)) -> f64 {
        let t0 = Instant::now();
        for i in 0..frames { f(i); }
        t0.elapsed().as_secs_f64() * 1000.0 / frames as f64
    }

    /// `cargo test --release --bin rift -- --ignored --nocapture bench_render`
    #[test]
    #[ignore]
    fn bench_render() {
        const N: usize = 200;
        let mut r = test_renderer();
        let (rc, w, h) = rect(&r);
        let bg = pack(r.theme.bg.0, r.theme.bg.1, r.theme.bg.2);
        let mut t = make_terminal();
        let mut out = vec![bg; w * h]; // stands in for the softbuffer buffer
        let mut back = vec![bg; w * h];
        println!("grid {COLS}x{ROWS}, cell {}x{}, buffer {w}x{h}", r.cell_width(), r.cell_height());

        let legacy = time_ms(N, |_| legacy_render(&mut r, &t, &mut out, w));

        // Full repaint every frame (resize / theme change / worst case).
        let full = time_ms(N, |_| {
            r.cur_pane = 0;
            r.full_frame = true;
            r.pane_rows.clear();
            r.render_pane_inner(&t, &mut back, w, h, rc, true, false);
            out.copy_from_slice(&back);
        });

        // Nothing changed (e.g. overlay-only frame).
        r.full_frame = false;
        let idle = time_ms(N, |_| {
            r.cur_pane = 0;
            r.render_pane_inner(&t, &mut back, w, h, rc, true, false);
            out.copy_from_slice(&back);
        });

        // Typing: one row edited + cursor moves along it.
        let typing = time_ms(N, |i| {
            t.grid[ROWS - 1][5 + i % 100].c = (b'a' + (i % 26) as u8) as char;
            t.cursor_col = 6 + i % 100;
            r.cur_pane = 0;
            r.render_pane_inner(&t, &mut back, w, h, rc, true, false);
            out.copy_from_slice(&back);
        });

        // `cat bigfile`: every frame scrolls ~3 lines of new text in.
        let mut seed = 1000;
        let cat = time_ms(N, |_| {
            for _ in 0..3 {
                let mut line = t.grid.remove(0);
                fill_row(&mut line, seed);
                seed += 1;
                t.grid.push(line);
            }
            r.cur_pane = 0;
            r.render_pane_inner(&t, &mut back, w, h, rc, true, false);
            out.copy_from_slice(&back);
        });

        let copy = time_ms(N, |_| out.copy_from_slice(&back));
        println!("legacy full frame (fill + per-pixel blend) : {legacy:8.3} ms");
        println!("new    full repaint (tile cache) + memcpy  : {full:8.3} ms");
        println!("new    idle (hash only) + memcpy           : {idle:8.3} ms");
        println!("new    keystroke (1 row + cursor) + memcpy : {typing:8.3} ms");
        println!("new    cat flood (3 lines/frame) + memcpy  : {cat:8.3} ms");
        println!("       (memcpy of back buffer alone        : {copy:8.3} ms)");
    }

    /// Every decoration / style variant must change pixels vs plain text,
    /// and decorated cells must match between tile path and slow path
    /// (cursor on the cell uses the slow path). Also dumps a gallery PNG.
    #[test]
    fn decorations_and_styles_render() {
        use crate::terminal::grid::UnderlineStyle as U;
        let path = crate::config::find_font_path();
        if !std::path::Path::new(&path).exists() { return; }
        let mut r = Renderer::new(&path, 20.0, Theme::catppuccin_mocha());
        let (cw, ch) = (r.cell_width(), r.cell_height());
        let variants: Vec<(&str, Box<dyn Fn(&mut crate::terminal::Attrs)>)> = vec![
            ("plain", Box::new(|_| {})),
            ("bold", Box::new(|a| a.bold = true)),
            ("italic", Box::new(|a| a.italic = true)),
            ("bolditalic", Box::new(|a| { a.bold = true; a.italic = true; })),
            ("single", Box::new(|a| a.underline_style = U::Single)),
            ("legacy_ul", Box::new(|a| a.underline = true)),
            ("double", Box::new(|a| a.underline_style = U::Double)),
            ("curly", Box::new(|a| a.underline_style = U::Curly)),
            ("dotted", Box::new(|a| a.underline_style = U::Dotted)),
            ("dashed", Box::new(|a| a.underline_style = U::Dashed)),
            ("strike", Box::new(|a| a.strikethrough = true)),
            ("overline", Box::new(|a| a.overline = true)),
            ("ul_color", Box::new(|a| { a.underline_style = U::Curly; a.underline_color = Some(Color::Rgb(255, 0, 0)); })),
        ];
        let cols = 12;
        let rows = variants.len();
        let (w, h) = (cols * cw, rows * ch);
        let render = |r: &mut Renderer, t: &Terminal| {
            let mut buf = vec![0u32; w * h];
            r.cur_pane = NO_CACHE;
            r.full_frame = true;
            r.render_pane_inner(t, &mut buf, w, h, PaneRect { x: 0, y: 0, width: w, height: h }, false, false);
            buf
        };
        let mut t = Terminal::new(cols, rows);
        for (ri, (_, f)) in variants.iter().enumerate() {
            for (ci, c) in "Hello gjpq 中".chars().enumerate().take(cols) {
                t.grid[ri][ci].c = c;
                t.grid[ri][ci].update_attrs(|a| f(a));
            }
        }
        let buf = render(&mut r, &t);
        crate::audit::write_png(&crate::audit::scratch_dir("png").join("styles_gallery.png"), w, h, &buf);
        let band = |buf: &[u32], row: usize| buf[row * ch * w..(row + 1) * ch * w].to_vec();
        let plain = band(&buf, 0);
        for (ri, (name, _)) in variants.iter().enumerate().skip(1) {
            if *name == "legacy_ul" { assert_eq!(band(&buf, ri), band(&buf, 4), "legacy underline == Single"); continue; }
            assert_ne!(band(&buf, ri), plain, "{name} must change pixels");
        }
        // A blank cell with an underline still draws it; hidden suppresses it.
        let mut t2 = Terminal::new(4, 1);
        t2.grid[0][1].update_attrs(|a| a.underline_style = U::Double);
        let b = render(&mut r, &t2);
        let refpx = b[3 * cw];
        let cell1 = |b: &[u32]| -> Vec<u32> { (0..ch).flat_map(|y| b[y * w + cw..y * w + 2 * cw].to_vec()).collect() };
        assert!(cell1(&b).iter().any(|p| *p != refpx), "underlined space");
        t2.grid[0][1].update_attrs(|a| a.hidden = true);
        let b = render(&mut r, &t2);
        assert!(cell1(&b).iter().all(|p| *p == refpx), "hidden hides decorations");
    }

    #[test]
    fn synthetic_styles_work_without_faces() {
        let mut bmp = vec![0u8; 8 * 10];
        for y in 2..8 { bmp[y * 8 + 3] = 255; }
        let orig = bmp.clone();
        font::embolden_for_test(&mut bmp, 8, 10);
        assert!(bmp.iter().filter(|v| **v > 0).count() > orig.iter().filter(|v| **v > 0).count());
        let mut sh = orig.clone();
        font::shear_for_test(&mut sh, 8, 10, 8);
        assert_ne!(sh, orig);
        // top of the stem leans right of the bottom
        let cx = |row: usize| (0..8).max_by_key(|x| sh[row * 8 + x]).unwrap();
        assert!(cx(2) > cx(7));
    }

    #[test]
    fn bold_is_bright_remaps_palette() {
        let path = crate::config::find_font_path();
        if !std::path::Path::new(&path).exists() { return; }
        let mut r = Renderer::new(&path, 16.0, Theme::rift_neon());
        let mut c = Cell::default();
        c.fg = Color::Indexed(1);
        c.update_attrs(|a| a.bold = true);
        let plain = r.resolve_fg(&c);
        r.set_bold_is_bright(true);
        assert_eq!(r.resolve_fg(&c), r.theme.resolve_indexed(9));
        assert_ne!(plain, r.resolve_fg(&c));
        c.fg = Color::Indexed(9);
        assert_eq!(r.resolve_fg(&c), r.theme.resolve_indexed(9));
    }

    /// SGR 7 on a default-background cell must paint the swapped background;
    /// otherwise the glyph is drawn in the bg color on a bg-colored cell and
    /// vanishes (zsh highlights pasted text with reverse video).
    #[test]
    fn reverse_video_on_default_bg_is_visible() {
        let path = crate::config::resolve_font_path(&crate::config::Config::default());
        if !std::path::Path::new(&path).exists() {
            return;
        }
        let mut r = Renderer::new(&path, 16.0, crate::config::Theme::catppuccin_mocha());
        let (cw, ch) = (r.cell_width(), r.cell_height());
        let mut t = Terminal::new(4, 1);
        t.grid[0][0].c = 'X';
        t.grid[0][0].update_attrs(|a| a.reverse = true);
        t.grid[0][1].update_attrs(|a| a.reverse = true); // reversed blank
        let (w, h) = (cw * 4, ch);
        let mut buf = vec![0u32; w * h];
        let rect = PaneRect { x: 0, y: 0, width: w, height: h };
        r.render_pane_inner(&t, &mut buf, w, h, rect, false, false);
        let fg = r.theme.fg;
        let fg_px = pack(fg.0, fg.1, fg.2);
        // The reversed blank cell is filled with the old foreground color.
        assert_eq!(buf[(ch / 2) * w + cw + cw / 2], fg_px);
        // The reversed 'X' cell has a foreground-colored background somewhere.
        assert!((0..ch).any(|y| (0..cw).any(|x| buf[y * w + x] == fg_px)));
    }


    // ── terminal-core follow-ups: hash inputs, DECSCNM, OSC 4, marks, links ──

    fn feed_bytes(t: &mut Terminal, bytes: &[u8]) {
        let mut p = vte::Parser::new();
        let mut h = crate::terminal::AnsiHandler::new(t);
        for &b in bytes { p.advance(&mut h, b); }
    }

    /// Renders a `cols x 1` terminal; None when no font is installed.
    fn render_small(r: &mut Renderer, t: &Terminal, cols: usize, cmd_held: bool) -> Vec<u32> {
        let (w, h) = (cols * r.cell_width(), r.cell_height());
        let mut buf = vec![0u32; w * h];
        r.cur_pane = NO_CACHE;
        r.full_frame = true;
        r.render_pane_inner(t, &mut buf, w, h, PaneRect { x: 0, y: 0, width: w, height: h }, false, cmd_held);
        buf
    }

    fn small_renderer() -> Option<Renderer> {
        let path = crate::config::resolve_font_path(&crate::config::Config::default());
        if !std::path::Path::new(&path).exists() { return None; }
        Some(Renderer::new(&path, 16.0, crate::config::Theme::catppuccin_mocha()))
    }

    #[test]
    fn row_hash_covers_cluster_and_link() {
        let mut a = vec![Cell::default(); 3];
        a[1].c = 'e';
        let base = hash_cells(0, &a, 0);
        let mut b = a.clone();
        b[1].set_cluster("\u{301}");
        assert_ne!(hash_cells(0, &b, 0), base, "cluster extras change the hash");
        let mut c = a.clone();
        c[1].set_link(2);
        assert_ne!(hash_cells(0, &c, 0), base, "link id changes the hash");
        assert_ne!(hash_cells(0, &c, 2), hash_cells(0, &c, 0), "hover on the link changes the hash");
        assert_eq!(hash_cells(0, &a, 2), base, "hover elsewhere does not");
    }

    #[test]
    fn decscnm_swaps_default_colors() {
        let Some(mut r) = small_renderer() else { return };
        let mut t = Terminal::new(4, 1);
        let normal = render_small(&mut r, &t, 4, false);
        let bg = pack(r.theme.bg.0, r.theme.bg.1, r.theme.bg.2);
        let fg = pack(r.theme.fg.0, r.theme.fg.1, r.theme.fg.2);
        assert_eq!(normal[1], bg);
        feed_bytes(&mut t, b"\x1b[?5h");
        let rev = render_small(&mut r, &t, 4, false);
        assert_eq!(rev[1], fg, "screen background becomes the theme foreground");
        // Hash differs so cached rows repaint.
        let screen_sig = |t: &Terminal| t.reverse_screen as u64 | (t.palette_gen as u64) << 1;
        assert_ne!(screen_sig(&t), 0);
        // State does not leak into later non-pane drawing.
        assert_eq!(r.def_bg(), r.theme.bg);
    }

    #[test]
    fn osc4_override_recolors_indexed_cells() {
        let Some(mut r) = small_renderer() else { return };
        let mut t = Terminal::new(3, 1);
        t.grid[0][0].bg = Color::Indexed(1);
        let before = render_small(&mut r, &t, 3, false);
        feed_bytes(&mut t, b"\x1b]4;1;rgb:12/34/56\x07");
        let after = render_small(&mut r, &t, 3, false);
        let want = pack(0x12, 0x34, 0x56);
        let mid = r.cell_height() / 2 * (3 * r.cell_width()) + 2;
        assert_ne!(before[mid], want);
        assert_eq!(after[mid], want);
        feed_bytes(&mut t, b"\x1b]104;1\x07");
        assert_eq!(render_small(&mut r, &t, 3, false), before);
    }

    #[test]
    fn combining_mark_is_overlaid_on_base() {
        let Some(mut r) = small_renderer() else { return };
        let mut t = Terminal::new(3, 1);
        t.grid[0][0].c = 'e';
        let plain = render_small(&mut r, &t, 3, false);
        feed_bytes(&mut t, b"\x1b[H");
        let mut t2 = Terminal::new(3, 1);
        feed_bytes(&mut t2, "e\u{301}".as_bytes());
        assert!(t2.grid[0][0].has_cluster(), "terminal stores the combining mark");
        let marked = render_small(&mut r, &t2, 3, false);
        assert_ne!(plain, marked, "mark adds ink");
        // Ink stays inside the base cell.
        let cw = r.cell_width();
        let w = 3 * cw;
        let ch = r.cell_height();
        for y in 0..ch { for x in cw..w { assert_eq!(plain[y * w + x], marked[y * w + x]); } }
    }

    #[test]
    fn osc8_link_underlines_on_cmd_and_hover() {
        let Some(mut r) = small_renderer() else { return };
        let mut t = Terminal::new(4, 1);
        feed_bytes(&mut t, b"\x1b]8;;http://x.test\x07ab\x1b]8;;\x07cd");
        assert!(t.grid[0][0].link() != 0);
        let idle = render_small(&mut r, &t, 4, false);
        r.cur_pane = NO_CACHE;
        let cmd = render_small(&mut r, &t, 4, true);
        assert_ne!(idle, cmd, "Cmd-held underlines links");
        // Hover (no Cmd): only the linked cells change.
        r.link_hover = Some((NO_CACHE, 0, 1));
        let hov = render_small(&mut r, &t, 4, false);
        assert_ne!(hov, idle);
        let (cw, ch) = (r.cell_width(), r.cell_height());
        let w = 4 * cw;
        for y in 0..ch { for x in 2 * cw..w { assert_eq!(hov[y * w + x], idle[y * w + x]); } }
    }
}
