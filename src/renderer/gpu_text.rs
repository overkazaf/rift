//! GPU text renderer, CPU half: instance generation (`--features gpu`).
//!
//! The terminal grid is drawn by the GPU as instanced quads (cell
//! backgrounds, glyphs sampled from an atlas, decorations, cursor,
//! selection / search highlights). This module turns terminal rows into
//! those instances and keeps them between frames:
//!
//! * every visible row of every pane owns a fixed-capacity **slot** in one
//!   big instance buffer; only rows whose content hash changed (the same
//!   damage tracking the CPU renderer uses) are rebuilt and re-uploaded,
//! * glyph bitmaps come from the existing `FontManager` (so glyph shapes are
//!   identical to the CPU path), are cropped to their ink box and packed into
//!   the [`Atlas`]; only new glyphs are uploaded,
//! * ligature / contextual-alternate glyphs come from the [`Shaper`] and are
//!   rasterized by glyph id.
//!
//! Everything that is not terminal cells (tab bar, pane borders, overlays,
//! images) stays in the CPU "UI layer" buffer. Terminal pixels in that buffer
//! hold [`KEY`] in the top byte, meaning "transparent: the GPU layer shows
//! through"; anything the CPU draws on top becomes opaque (the drawing code
//! writes plain `0x00RRGGBB`).
//!
//! Instance order inside a row is the paint order: row background, cell
//! backgrounds + cursor shapes, glyphs, decorations, highlights.

use std::collections::HashMap;
use std::sync::Arc;

use super::atlas::{Atlas, AtlasEntry, Bitmap, GlyphKey};
use super::font::{self, Deco, FontManager, ShapeFace};
use super::shape::ShapedRun;
use super::*;
use crate::terminal::CursorStyle;

/// Top-byte marker of a transparent UI-layer pixel.
pub const KEY: u32 = 0xFF00_0000;

pub const KIND_SOLID: u32 = 0;
pub const KIND_MASK: u32 = 1;
/// Straight-alpha RGBA atlas glyph (reserved for color glyphs).
#[allow(dead_code)]
pub const KIND_COLOR: u32 = 2;

/// One instanced quad (28 bytes). Matches the vertex layout in `gpu.rs`.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Inst {
    /// Top-left corner in window pixels.
    pub pos: [f32; 2],
    pub size: [f32; 2],
    /// Atlas texel of the top-left corner: `x | y << 16`.
    pub uv: u32,
    /// `r | g << 8 | b << 16 | a << 24`.
    pub color: u32,
    /// `KIND_*` in the low byte, atlas page in the next.
    pub kind: u32,
}

#[inline]
fn rgba(c: Rgb, a: u8) -> u32 {
    c.0 as u32 | (c.1 as u32) << 8 | (c.2 as u32) << 16 | (a as u32) << 24
}

impl Inst {
    #[inline]
    pub fn solid(x: f32, y: f32, w: f32, h: f32, color: u32) -> Self {
        Self { pos: [x, y], size: [w, h], uv: 0, color, kind: KIND_SOLID }
    }

    #[inline]
    fn solid_rgb(x: usize, y: usize, w: usize, h: usize, c: Rgb) -> Self {
        Self::solid(x as f32, y as f32, w as f32, h as f32, rgba(c, 255))
    }

    #[inline]
    fn glyph(x: i32, y: i32, e: &AtlasEntry, color: Rgb, kind: u32) -> Self {
        Self {
            pos: [x as f32, y as f32],
            size: [e.w as f32, e.h as f32],
            uv: e.x as u32 | (e.y as u32) << 16,
            color: rgba(color, 255),
            kind: kind | (e.page as u32) << 8,
        }
    }
}

pub use super::cells::HlRect;

/// Instances of one terminal row plus upload state.
#[derive(Default)]
pub struct RowSlot {
    pub inst: Vec<Inst>,
    /// Instances currently valid on the GPU for this slot (may be longer
    /// than `inst`: the stale tail is zeroed on the next upload).
    pub gpu_len: u32,
    pub dirty: bool,
}

/// CPU-side state of the GPU text renderer.
pub struct GpuText {
    pub atlas: Atlas,
    /// Reserved for color glyphs; nothing fills it with the current font stack.
    pub color_atlas: Atlas,
    pub slots: Vec<RowSlot>,
    /// Instances per slot.
    pub cap: usize,
    /// Per-frame quads drawn after all rows (pane dimming, margin strips).
    pub tail: Vec<Inst>,
    /// Bumped whenever the slot layout changes; the pipeline reallocates.
    pub layout_gen: u64,
    /// pane index -> (first slot, row count)
    panes: HashMap<usize, (usize, usize)>,
    /// Rows rebuilt since the last submit (profiling).
    pub rows_built: u64,
    /// Slots whose instance list was truncated to `cap`.
    pub overflowed: u64,
    /// Instances per column reserved in each row slot (grows on overflow).
    cap_per_col: usize,
    grow_cap: bool,
    /// A glyph could not be placed because every atlas page was in use this
    /// frame; rows are rebuilt next frame.
    pub starved: bool,
    /// Start the next full rebuild from an empty atlas (pages could not be
    /// recycled one by one because the frame's working set was spread over all).
    reset_atlas: bool,
}

/// Room for per-frame quads after the row slots.
pub const TAIL_CAP: usize = 256;

impl GpuText {
    pub fn new() -> Self {
        Self::with_atlas(2048, 4)
    }

    pub fn with_atlas(page: u32, pages: usize) -> Self {
        Self {
            atlas: Atlas::new(page, page, pages, 1),
            color_atlas: Atlas::new(1024, 1024, 1, 4),
            slots: Vec::new(),
            cap: 0,
            tail: Vec::new(),
            layout_gen: 0,
            panes: HashMap::new(),
            rows_built: 0,
            overflowed: 0,
            cap_per_col: 3,
            grow_cap: false,
            starved: false,
            reset_atlas: false,
        }
    }

    /// Assign slots for the pane layout (`(pane index, rect)`) and mark
    /// everything dirty.
    pub fn layout(&mut self, panes: &[(usize, PaneRect)], cw: usize, ch: usize) {
        self.panes.clear();
        let mut total = 0;
        let mut max_cols = 1;
        for (idx, r) in panes {
            let rows = r.height / ch.max(1);
            self.panes.insert(*idx, (total, rows));
            total += rows;
            max_cols = max_cols.max(r.width / cw.max(1) + 1);
        }
        self.cap = max_cols * self.cap_per_col + 16;
        self.slots.clear();
        let cap = self.cap as u32;
        self.slots.resize_with(total, || RowSlot { dirty: true, gpu_len: cap, ..Default::default() });
        self.layout_gen += 1;
    }

    #[inline]
    pub fn slot_index(&self, pane: usize, row: usize) -> Option<usize> {
        let (base, rows) = *self.panes.get(&pane)?;
        (row < rows).then_some(base + row)
    }

    /// Slot range of a pane.
    pub fn pane_rows(&self, pane: usize) -> Option<(usize, usize)> {
        self.panes.get(&pane).copied()
    }

    fn set_slot(&mut self, slot: usize, mut inst: Vec<Inst>) {
        if inst.len() > self.cap {
            inst.truncate(self.cap);
            self.overflowed += 1;
            self.grow_cap = true;
        }
        let s = &mut self.slots[slot];
        s.inst = inst;
        s.dirty = true;
        self.rows_built += 1;
    }

    fn clear_slot(&mut self, slot: usize) {
        let s = &mut self.slots[slot];
        if !s.inst.is_empty() || s.gpu_len > 0 {
            s.inst.clear();
            s.dirty = true;
        }
    }

    /// Total instances the GPU buffer must hold (rows + tail).
    pub fn buffer_instances(&self) -> usize {
        self.slots.len() * self.cap + TAIL_CAP
    }

    /// Mark every slot dirty (device lost, buffer recreated).
    pub fn mark_all_dirty(&mut self) {
        // After a relayout the buffer still holds instances of the old layout
        // at these offsets: treat the whole slot as stale so it is zero-padded.
        let cap = self.cap as u32;
        for s in &mut self.slots {
            s.dirty = true;
            s.gpu_len = cap;
        }
    }

    /// Bitmap for an atlas key (rasterized on a cache miss only).
    fn make_bitmap(font: &mut FontManager, key: GlyphKey) -> Bitmap {
        let (cw, ch) = (font.cell_width, font.cell_height);
        match key {
            GlyphKey::Char { c, style, wide } => {
                if wide {
                    Bitmap::crop_mask(font.rasterize_wide_styled(c, style), cw * 2, ch, 0, 0)
                } else {
                    Bitmap::crop_mask(font.rasterize_styled(c, style), cw, ch, 0, 0)
                }
            }
            GlyphKey::Mark { c, wide } => {
                let w = if wide { cw * 2 } else { cw };
                Bitmap::crop_mask(font.rasterize_mark(c, wide), w, ch, 0, 0)
            }
            GlyphKey::Gid { slot, gid, synth } => {
                let b = font.rasterize_gid(slot as usize, gid, synth & 1 != 0, synth & 2 != 0);
                Bitmap::crop_mask(&b.data, b.w, b.h, b.x, b.y)
            }
            GlyphKey::Deco { bits, w } => {
                let ul = match bits & 7 {
                    1 => UnderlineStyle::Single,
                    2 => UnderlineStyle::Double,
                    3 => UnderlineStyle::Curly,
                    4 => UnderlineStyle::Dotted,
                    5 => UnderlineStyle::Dashed,
                    _ => UnderlineStyle::None,
                };
                let d = Deco { ul, strike: bits & 8 != 0, over: bits & 16 != 0 };
                let w = w as usize;
                let mut px = vec![0u32; w * ch];
                let dm = font.deco;
                font::paint_decor(&mut px, w, 0, 0, w, ch, &d, &dm, 1, 1);
                let mask: Vec<u8> = px.iter().map(|&p| if p != 0 { 255 } else { 0 }).collect();
                Bitmap::crop_mask(&mask, w, ch, 0, 0)
            }
            GlyphKey::LinkDots { w } => {
                let w = w as usize;
                let data: Vec<u8> = (0..w).map(|x| if x % 2 == 0 { 255 } else { 0 }).collect();
                Bitmap::crop_mask(&data, w, 1, 0, ch as i32 - 2)
            }
            GlyphKey::Color { .. } => Bitmap::empty(),
        }
    }

    fn entry(&mut self, font: &mut FontManager, key: GlyphKey) -> Option<AtlasEntry> {
        let e = self.atlas.get_or_insert_with(key, || Self::make_bitmap(font, key));
        if e.is_none() {
            self.starved = true;
        }
        e
    }

    /// Whether this frame lost glyphs or evicted atlas pages, i.e. cached
    /// rows may be wrong and everything must be rebuilt. Clears the flags.
    pub fn take_stale(&mut self) -> bool {
        let evicted = self.atlas.take_evicted();
        self.color_atlas.take_evicted();
        if std::mem::take(&mut self.grow_cap) {
            // A row needed more instances than its slot holds (dense colors +
            // decorations): reserve more per column and rebuild.
            self.cap_per_col = (self.cap_per_col + 2).min(16);
            return true;
        }
        if self.starved {
            self.starved = false;
            self.reset_atlas = true;
            return true;
        }
        evicted
    }

    /// Called at the start of a full rebuild: drop the atlas if the last
    /// frame ran out of room (everything is about to be re-requested anyway).
    pub fn begin_rebuild(&mut self) {
        if std::mem::take(&mut self.reset_atlas) {
            self.atlas.clear();
            self.color_atlas.clear();
        }
    }
}

impl Renderer {
    /// Switch the GPU text path on or off. Off keeps the CPU renderer as is.
    pub fn enable_gpu_text(&mut self, on: bool) {
        if on == self.gpu.is_some() {
            return;
        }
        self.gpu = on.then(|| Box::new(GpuText::new()));
        self.invalidate();
    }

    /// GPU text is enabled and not suspended for this frame.
    #[inline]
    pub fn gpu_text_active(&self) -> bool {
        self.gpu.is_some() && !self.gpu_suspended
    }

    /// True once after the GPU text path needs an immediate extra frame.
    pub fn take_redraw_request(&mut self) -> bool {
        std::mem::take(&mut self.redraw_requested)
    }

    /// Hand terminal text to the CPU (`true`) or back to the GPU for the
    /// next frames (switching repaints everything). Not used by the app, which
    /// handles overlays through translucent UI-layer pixels; kept as an escape
    /// hatch and for tests of the opaque path.
    #[allow(dead_code)]
    pub fn set_gpu_suspended(&mut self, suspended: bool) {
        if self.gpu_suspended != suspended {
            self.gpu_suspended = suspended;
            self.invalidate();
        }
    }

    #[inline]
    pub(super) fn gpu_text_on(&self) -> bool {
        self.gpu_text_active() && self.cur_pane != NO_CACHE
    }

    /// Replace the highlight set (selection, search matches) of `pane`.
    /// Rows are repainted only if their highlights changed.
    pub fn set_highlights(&mut self, pane: usize, rects: Vec<HlRect>) {
        self.hl_pane = pane;
        self.hl_rows.clear();
        self.hl_hash.clear();
        for r in rects {
            let h = self.hl_hash.entry(r.row).or_insert(0x4c1);
            *h = mix(*h, (r.c0 as u64) << 32 | r.c1 as u64);
            *h = mix(*h, u32::from_le_bytes(r.rgba) as u64);
            self.hl_rows.entry(r.row).or_default().push(r);
        }
    }

    /// Hash of the highlights on `row` of the pane being rendered (0 = none).
    #[inline]
    pub(super) fn hl_row_hash(&self, row: usize) -> u64 {
        if !self.gpu_text_active() || self.cur_pane != self.hl_pane {
            return 0;
        }
        self.hl_hash.get(&row).copied().unwrap_or(0)
    }

    /// Per-frame quads that follow the rows: margin strips below the last
    /// row of each pane and the dimming of unfocused panes.
    pub(super) fn gpu_frame_tail(
        &mut self,
        layouts: &[(usize, PaneRect, bool)],
        wm: &WindowManager,
        hover_pane: Option<usize>,
    ) {
        let ch = self.font.cell_height.max(1);
        let Some(mut g) = self.gpu.take() else { return };
        g.tail.clear();
        let active_tab = wm.active_tab();
        for (idx, rect, _) in layouts {
            let used = rect.height / ch * ch;
            if used < rect.height {
                let reverse = active_tab.pane(*idx).map_or(false, |p| p.terminal.reverse_screen);
                let c = if reverse { self.theme.fg } else { self.theme.bg };
                g.tail.push(Inst::solid_rgb(rect.x, rect.y + used, rect.width, rect.height - used, c));
            }
        }
        if layouts.len() > 1 {
            for (idx, rect, active) in layouts {
                if *active {
                    continue;
                }
                let k = if hover_pane == Some(*idx) { UNFOCUSED_DIM * 0.5 } else { UNFOCUSED_DIM };
                // Same weight as the CPU blend: k256 / 256 toward the theme bg.
                let k256 = (k.clamp(0.0, 1.0) * 256.0) as i32;
                let a = ((k256 as f32 / 256.0) * 255.0).round() as u8;
                g.tail.push(Inst::solid(
                    rect.x as f32, rect.y as f32, rect.width as f32, rect.height as f32,
                    rgba(self.theme.bg, a),
                ));
            }
        }
        g.tail.truncate(TAIL_CAP);
        self.gpu = Some(g);
    }

    /// Build the instances for every dirty row of one pane and keep the CPU
    /// UI layer (`buffer`) transparent under them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn gpu_paint_pane(
        &mut self,
        terminal: &Terminal,
        visible: &[&Vec<Cell>],
        dirty: &[bool],
        old_rows: usize,
        buffer: &mut [u32],
        buf_width: usize,
        buf_height: usize,
        rect: PaneRect,
        show_cursor: bool,
        images_visible: bool,
        cmd_held: bool,
    ) {
        let Some(mut g) = self.gpu.take() else { return };
        let ch = self.font.cell_height;
        let pane = self.cur_pane;
        let key_px = KEY | pack_rgb(self.def_bg());
        let x1 = rect.right().min(buf_width);
        let key_band = |buffer: &mut [u32], y0: usize| {
            if y0 + ch > rect.bottom() || y0 + ch > buf_height || x1 <= rect.x {
                return;
            }
            for cy in 0..ch {
                let o = (y0 + cy) * buf_width;
                buffer[o + rect.x..o + x1].fill(key_px);
            }
        };

        for (row, cells) in visible.iter().enumerate() {
            if !dirty[row] {
                continue;
            }
            let Some(slot) = g.slot_index(pane, row) else { continue };
            let y0 = rect.y + row * ch;
            // Kitty image slices from a previous frame live in the UI layer.
            key_band(buffer, y0);
            let url_ranges: Vec<(usize, usize)> = if cmd_held {
                let line: String = cells.iter().map(|c| c.c).collect();
                crate::tools::url_detect::detect_urls(&line)
                    .into_iter()
                    .map(|(start, end, _)| (start, end))
                    .collect()
            } else {
                Vec::new()
            };
            let mut out = std::mem::take(&mut g.slots[slot].inst);
            out.clear();
            self.gpu_build_row(
                &mut g, &mut out, terminal, row, cells, buffer, buf_width, buf_height, rect,
                show_cursor, images_visible, &url_ranges, cmd_held,
            );
            g.set_slot(slot, out);
        }
        // Rows that existed last frame but are gone now.
        for row in visible.len()..old_rows {
            if let Some(slot) = g.slot_index(pane, row) {
                g.clear_slot(slot);
            }
        }
        // Rows can also vanish from the layout without a hash entry; make
        // sure unused slots of this pane are empty.
        if let Some((base, rows)) = g.pane_rows(pane) {
            for row in visible.len()..rows {
                g.clear_slot(base + row);
            }
        }
        self.gpu = Some(g);
    }

    #[allow(clippy::too_many_arguments)]
    fn gpu_build_row(
        &mut self,
        g: &mut GpuText,
        out: &mut Vec<Inst>,
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
        let (cw, ch) = (self.font.cell_width, self.font.cell_height);
        let y0 = rect.y + row * ch;
        if y0 + ch > rect.y + rect.height || y0 + ch > buf_height {
            return;
        }
        let dbg = self.def_bg();
        let theme_cursor = self.theme.cursor;
        let cursor_style = terminal.cursor_style;
        let accent = self.theme.cursor;
        out.push(Inst::solid_rgb(rect.x, y0, rect.width, ch, dbg));

        // ── Resolve every cell once ──
        let cds = self.resolve_row_cells(
            terminal, row, cells, rect, buf_width, show_cursor, images_visible, url_ranges,
            Some((&mut *buffer, buf_height)),
        );

        // ── Pass 1: cell backgrounds (merged runs) and cursor shapes ──
        let mut cursor_quads: Vec<Inst> = Vec::new();
        let mut run: Option<(usize, usize, Rgb)> = None; // x start, x end, color
        for cd in &cds {
            if cd.skip {
                continue;
            }
            if cd.fill {
                match &mut run {
                    Some((_, end, c)) if *end == cd.x0 && *c == cd.bg => *end = cd.x0 + cd.fill_w,
                    _ => {
                        if let Some((s, e, c)) = run.take() {
                            out.push(Inst::solid_rgb(s, y0, e - s, ch, c));
                        }
                        run = Some((cd.x0, cd.x0 + cd.fill_w, cd.bg));
                    }
                }
            }
            if cd.cursor {
                match cursor_style {
                    CursorStyle::Bar => cursor_quads.push(Inst::solid_rgb(cd.x0, y0, 2, ch, theme_cursor)),
                    CursorStyle::Underline => {
                        let h = 2.min(ch);
                        cursor_quads.push(Inst::solid_rgb(cd.x0, y0 + ch - h, cd.cursor_w, h, theme_cursor));
                    }
                    _ => {}
                }
            }
        }
        if let Some((s, e, c)) = run.take() {
            out.push(Inst::solid_rgb(s, y0, e - s, ch, c));
        }
        out.append(&mut cursor_quads);

        // ── Ligature runs ──
        let mut covered = vec![false; cds.len()];
        let mut shaped: Vec<(usize, ShapeFace, Arc<ShapedRun>)> = Vec::new();
        if self.ligatures {
            self.find_ligatures(cells, &cds, row, &mut covered, &mut shaped);
        }

        // ── Pass 2: glyphs ──
        for (col, cd) in cds.iter().enumerate() {
            if cd.skip || !cd.glyph || covered[col] {
                continue;
            }
            let c = cells[col].c;
            let key = GlyphKey::Char { c, style: cd.style, wide: cd.wide };
            if let Some(e) = g.entry(&mut self.font, key) {
                if !e.is_empty() {
                    out.push(Inst::glyph(cd.x0 as i32 + e.off_x as i32, y0 as i32 + e.off_y as i32, &e, cd.text, KIND_MASK));
                }
            }
        }
        for (start, face, run) in &shaped {
            let synth = face.synth_bold as u8 | (face.synth_italic as u8) << 1;
            for sg in &run.glyphs {
                let col = start + sg.cell as usize;
                let Some(cd) = cds.get(col) else { continue };
                let key = GlyphKey::Gid { slot: face.slot as u8, gid: sg.gid, synth };
                if let Some(e) = g.entry(&mut self.font, key) {
                    if !e.is_empty() {
                        out.push(Inst::glyph(
                            cd.x0 as i32 + sg.x_off as i32 + e.off_x as i32,
                            y0 as i32 + sg.y_off as i32 + e.off_y as i32,
                            &e, cd.text, KIND_MASK,
                        ));
                    }
                }
            }
        }
        // Combining marks overlaid on the base glyph.
        for (col, cd) in cds.iter().enumerate() {
            let cell = &cells[col];
            if cd.skip || cell.extra_id() == 0 || cell.hidden() {
                continue;
            }
            let wide = font::is_wide(cell.c);
            let extra = cell.extra();
            for m in extra.chars().filter(|&c| font::is_overlay_mark(c)) {
                if let Some(e) = g.entry(&mut self.font, GlyphKey::Mark { c: m, wide }) {
                    if !e.is_empty() {
                        out.push(Inst::glyph(cd.x0 as i32 + e.off_x as i32, y0 as i32 + e.off_y as i32, &e, cd.text, KIND_MASK));
                    }
                }
            }
        }

        // ── Pass 3: decorations, link underlines ──
        let dm = self.font.deco;
        for (col, cd) in cds.iter().enumerate() {
            if cd.skip {
                continue;
            }
            let cell = &cells[col];
            if cd.deco.any() {
                let w = if cd.wide { cw * 2 } else if cd.cursor { cd.fill_w.max(cw) } else { cw };
                let x0 = cd.x0;
                let t = dm.ul_thick.max(1);
                let rect_q = |top: usize, h: usize, c: Rgb| -> Option<Inst> {
                    let h = h.min(ch.saturating_sub(top));
                    (h > 0).then(|| Inst::solid_rgb(x0, y0 + top, w, h, c))
                };
                match cd.deco.ul {
                    UnderlineStyle::None => {}
                    UnderlineStyle::Single => out.extend(rect_q(dm.ul_top, t, cd.ul)),
                    UnderlineStyle::Double => {
                        let gap = t.max(1);
                        let total = 2 * t + gap;
                        let top = dm.ul_top.min(ch.saturating_sub(total));
                        out.extend(rect_q(top, t, cd.ul));
                        out.extend(rect_q(top + t + gap, t, cd.ul));
                    }
                    s @ (UnderlineStyle::Dotted | UnderlineStyle::Dashed | UnderlineStyle::Curly) => {
                        let key = GlyphKey::Deco { bits: s as u64, w: w as u16 };
                        if let Some(e) = g.entry(&mut self.font, key) {
                            if !e.is_empty() {
                                out.push(Inst::glyph(x0 as i32 + e.off_x as i32, y0 as i32 + e.off_y as i32, &e, cd.ul, KIND_MASK));
                            }
                        }
                    }
                }
                if cd.deco.strike {
                    out.extend(rect_q(dm.strike_top, dm.strike_thick.max(1), cd.text));
                }
                if cd.deco.over {
                    out.extend(rect_q(0, t, cd.text));
                }
            }
            // OSC 8 hyperlinks: dotted accent underline (Cmd held / hovered).
            let link = cell.link();
            if link != 0 && (url_cmd || link == self.cur_hover_link) {
                let w = if font::is_wide(cell.c) { cw * 2 } else { cw };
                if let Some(e) = g.entry(&mut self.font, GlyphKey::LinkDots { w: w as u16 }) {
                    if !e.is_empty() {
                        out.push(Inst::glyph(cd.x0 as i32 + e.off_x as i32, y0 as i32 + e.off_y as i32, &e, accent, KIND_MASK));
                    }
                }
            }
            if cd.url && ch >= 2 {
                out.push(Inst::solid_rgb(cd.x0, y0 + ch - 2, cw, 1, accent));
            }
        }

        // ── Pass 4: highlights (selection, search) over everything ──
        if self.cur_pane == self.hl_pane {
            if let Some(rects) = self.hl_rows.get(&row) {
                let max_col = rect.width / cw.max(1);
                for r in rects {
                    let (c0, c1) = (r.c0.min(max_col), r.c1.min(max_col));
                    if c1 > c0 {
                        out.push(Inst::solid(
                            (rect.x + c0 * cw) as f32, y0 as f32, ((c1 - c0) * cw) as f32, ch as f32,
                            u32::from_le_bytes(r.rgba),
                        ));
                    }
                }
            }
        }
    }
}
