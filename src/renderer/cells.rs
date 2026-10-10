//! Per-row cell resolution shared by the CPU and GPU text paths: effective
//! colors, cursor shape, decorations, and the grouping of cells into
//! ligature runs.

use std::sync::Arc;

use super::font::{self, Deco, FontManager, ShapeFace};
use super::shape::ShapedRun;
use super::*;
use crate::terminal::CursorStyle;

/// One highlighted run of cells in a view row (search match; the selection
/// is a cell background, see `Renderer::set_selection`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HlRect {
    pub row: usize,
    /// Columns `c0..c1`.
    pub c0: usize,
    pub c1: usize,
    /// Straight-alpha color blended over the finished cells.
    pub rgba: [u8; 4],
}

/// Per-cell facts resolved once per row build.
#[derive(Clone, Copy)]
pub(super) struct Cd {
    pub x0: usize,
    /// Fill color when `fill` is set.
    pub bg: Rgb,
    pub fill: bool,
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub fill_w: usize,
    /// Width of the cursor shape on this cell (1 or 2 cells).
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub cursor_w: usize,
    /// Color of the glyph and its decorations (cursor / URL aware).
    pub text: Rgb,
    pub ul: Rgb,
    pub style: u8,
    pub deco: Deco,
    pub cursor: bool,
    pub glyph: bool,
    pub wide: bool,
    /// Nothing to draw for this cell (continuation, image, off the edge).
    pub skip: bool,
    pub url: bool,
}

impl Cd {
    pub fn skipped(x0: usize) -> Self {
        Self {
            x0,
            bg: (0, 0, 0),
            fill: false,
            fill_w: 0,
            cursor_w: 0,
            text: (0, 0, 0),
            ul: (0, 0, 0),
            style: 0,
            deco: Deco::default(),
            cursor: false,
            glyph: false,
            wide: false,
            skip: true,
            url: false,
        }
    }
}

type RunKey = (u8, Rgb, Rgb, bool, u64, Rgb, bool);

impl Renderer {
    /// Resolve every cell of a row. Cells under a Kitty image are skipped;
    /// with `blit` (UI-layer buffer + its height) their image slice is drawn
    /// into the buffer as well.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn resolve_row_cells(
        &mut self,
        terminal: &Terminal,
        row: usize,
        cells: &[Cell],
        rect: PaneRect,
        buf_width: usize,
        show_cursor: bool,
        images_visible: bool,
        url_ranges: &[(usize, usize)],
        mut blit: Option<(&mut [u32], usize)>,
    ) -> Vec<Cd> {
        let (cw, ch) = (self.font.cell_width, self.font.cell_height);
        let y0 = rect.y + row * ch;
        let theme_bg = self.theme.bg;
        let theme_cursor = self.theme.cursor;
        let cursor_style = terminal.cursor_style;
        let accent = self.theme.cursor;
        let sel_span = self.sel_span(row);
        let sel_bg = self.sel_bg;
        let mut cds: Vec<Cd> = Vec::with_capacity(cells.len());
        for (col, cell) in cells.iter().enumerate() {
            let x0 = rect.x + col * cw;
            let mut cd = Cd::skipped(x0);
            if x0 + cw > rect.x + rect.width || x0 + cw > buf_width {
                cds.push(cd);
                continue;
            }
            if images_visible {
                if let Some(ic) = terminal.image_store.get_cell(row, col) {
                    if let Some(img) = terminal.image_store.get_image(ic.image_id) {
                        if let Some((buffer, buf_h)) = blit.as_mut() {
                            blit_image_cell(buffer, buf_width, *buf_h, img, ic, x0, y0, cw, ch);
                        }
                    }
                    cds.push(cd);
                    continue;
                }
            }
            let (mut fg, mut bg) = (self.resolve_fg(cell), self.resolve(cell.bg, false));
            if cell.reverse() {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.dim() {
                fg = (fg.0 / 2, fg.1 / 2, fg.2 / 2);
            }
            // Selection: background behind the glyph, text kept readable.
            let selected = sel_span.map_or(false, |(a, b)| col >= a && col < b);
            if selected {
                bg = sel_bg;
                fg = self.theme.selection_text(fg, sel_bg);
            }
            if cell.c == '\0' {
                cds.push(cd);
                continue;
            }
            let is_cursor = show_cursor && row == self.view_cursor_row && col == terminal.cursor_col;
            let block = is_cursor && cursor_style == CursorStyle::Block;
            let glyph = cell.c != ' ' && !cell.hidden();
            let wide = glyph && font::is_wide(cell.c);
            let deco = if cell.hidden() { Deco::default() } else { cell_deco(cell) };
            let cursor_w = if is_cursor {
                use unicode_width::UnicodeWidthChar;
                cell.c.width().unwrap_or(1).max(1) * cw
            } else {
                cw
            };
            let is_url = url_ranges.iter().any(|&(s, e)| col >= s && col < e);
            let text = if block { theme_bg } else if is_url && glyph { accent } else { fg };
            let ul_rgb = match cell.underline_color() {
                Some(c) if deco.ul != UnderlineStyle::None && !block => self.resolve(c, true),
                _ => text,
            };
            cd = Cd {
                x0,
                bg: if block { theme_cursor } else { bg },
                fill: cell.bg != Color::Default || cell.reverse() || block || selected,
                fill_w: if is_cursor { cursor_w } else if wide { cw * 2 } else { cw },
                cursor_w,
                text,
                ul: ul_rgb,
                style: font::style_bits(cell.bold(), cell.italic()),
                deco,
                cursor: is_cursor,
                glyph,
                wide,
                skip: false,
                url: is_url,
            };
            cds.push(cd);
        }

        cds
    }

    /// Group eligible cells into runs and shape them. Fills `covered` for
    /// cells drawn by a shaped glyph and `shaped` with `(first col, face, run)`.
    pub(super) fn find_ligatures(
        &mut self,
        cells: &[Cell],
        cds: &[Cd],
        row: usize,
        covered: &mut [bool],
        shaped: &mut Vec<(usize, ShapeFace, Arc<ShapedRun>)>,
    ) {
        // Fonts without ligature features (Menlo, DejaVu Sans Mono, ...) never change shape.
        match self.font.shaping_face(0) {
            Some(f) if self.shaper.supports(&f) => {}
            _ => return,
        }
        let mut faces: [Option<Option<ShapeFace>>; 4] = [None; 4];
        let mut brk = vec![false; cds.len() + 1];
        if self.cur_pane == self.hl_pane {
            if let Some(rects) = self.hl_rows.get(&row) {
                for r in rects {
                    if r.c0 < brk.len() {
                        brk[r.c0] = true;
                    }
                    if r.c1 < brk.len() {
                        brk[r.c1] = true;
                    }
                }
            }
        }
        let cw = self.font.cell_width;
        let size = self.font.font_size();
        let eligible = |font: &mut FontManager, faces: &mut [Option<Option<ShapeFace>>; 4], col: usize| -> Option<ShapeFace> {
            let (cd, cell) = (&cds[col], &cells[col]);
            if cd.skip || !cd.glyph || cd.wide || cd.cursor || cell.extra_id() != 0 {
                return None;
            }
            let c = cell.c;
            if (c as u32) < 0x20 || c == '\u{7f}' || c == ' ' {
                return None;
            }
            let f = (*faces[cd.style as usize].get_or_insert_with(|| font.shaping_face(cd.style)))?;
            font.slot_has_glyph(f.slot, c).then_some(f)
        };
        let key_of = |col: usize| -> RunKey {
            let cd = &cds[col];
            (cd.style, cd.text, cd.bg, cd.fill, cd.deco.bits(), cd.ul, cd.url)
        };
        let mut col = 0;
        let mut text: Vec<char> = Vec::new();
        while col < cds.len() {
            let Some(face) = eligible(&mut self.font, &mut faces, col) else {
                col += 1;
                continue;
            };
            let start = col;
            let key = key_of(col);
            text.clear();
            text.push(cells[col].c);
            while col + 1 < cds.len()
                && !brk[col + 1]
                && key_of(col + 1) == key
                && eligible(&mut self.font, &mut faces, col + 1).is_some()
            {
                col += 1;
                text.push(cells[col].c);
            }
            col += 1;
            if text.len() < 2 {
                continue;
            }
            if let Some(run) = self.shaper.shape(&face, size, cw, &text) {
                for (i, c) in run.covered.iter().enumerate() {
                    if *c {
                        covered[start + i] = true;
                    }
                }
                shaped.push((start, face, run));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::WindowManager;

    fn ligature_font() -> Option<String> {
        let home = std::env::var("HOME").ok()?;
        [
            format!("{home}/Library/Fonts/JetBrainsMonoNerdFontMono-Regular.ttf"),
            format!("{home}/Library/Fonts/JetBrainsMonoNerdFont-Regular.ttf"),
            "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf".to_string(),
        ]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
    }

    fn frame(r: &mut Renderer, wm: &WindowManager, w: usize, h: usize) -> Vec<u32> {
        let mut buf = vec![0u32; w * h];
        r.invalidate();
        let blocks = crate::tools::blocks::BlockManager::new();
        r.render_tabbed(wm, PaneRect { x: 0, y: 0, width: w, height: h }, &mut buf, w as u32, h as u32, &blocks);
        buf
    }

    #[test]
    fn cpu_ligatures_change_operators_only() {
        let Some(font) = ligature_font() else { return };
        let mut r = Renderer::new(&font, 15.0, crate::config::Config::default().theme);
        let (cw, ch) = (r.cell_width(), r.cell_height());
        let (w, h) = (40 * cw, 3 * ch);
        let mut wm = WindowManager::headless(40, 3);
        wm.active_pane_mut().feed(b"a -> b != c\r\nplain words only (here)\r\n");
        wm.active_pane_mut().feed(b"\x1b[3;1H"); // park the cursor away from row 0/1
        let off = frame(&mut r, &wm, w, h);
        r.set_ligatures(true);
        assert!(r.ligatures());
        let on = frame(&mut r, &wm, w, h);
        // Row 0 holds ligatures: pixels differ. Row 1 has none: identical.
        assert_ne!(off[..w * ch], on[..w * ch], "-> and != must change with ligatures");
        assert_eq!(off[w * ch..2 * w * ch], on[w * ch..2 * w * ch], "plain text is untouched");
        // Toggling back restores the plain rendering exactly.
        r.set_ligatures(false);
        assert_eq!(frame(&mut r, &wm, w, h), off);
    }

    #[test]
    fn color_changes_split_runs() {
        let Some(font) = ligature_font() else { return };
        let mut r = Renderer::new(&font, 15.0, crate::config::Config::default().theme);
        let (cw, ch) = (r.cell_width(), r.cell_height());
        let (w, h) = (20 * cw, 3 * ch);
        let mut wm = WindowManager::headless(20, 3);
        // Row 0: "-" and ">" in different colors; row 1: same color.
        wm.active_pane_mut().feed(b"\x1b[31m-\x1b[32m>\x1b[0m\r\n\x1b[31m->\x1b[0m\x1b[3;1H");
        let off = frame(&mut r, &wm, w, h);
        r.set_ligatures(true);
        let on = frame(&mut r, &wm, w, h);
        assert_eq!(off[..w * ch], on[..w * ch], "differently colored cells never form a ligature");
        assert_ne!(off[w * ch..2 * w * ch], on[w * ch..2 * w * ch], "same-colored -> does");
    }
}
