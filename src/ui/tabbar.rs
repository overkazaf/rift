//! Tab bar: layout / hit-testing / index remapping (pure, unit-tested), width
//! aware title drawing and the inline rename field.

use unicode_width::UnicodeWidthChar;

use crate::config::Rgb;
use crate::renderer::font::{self, FontManager};

// ── Layout ──────────────────────────────────────────────────────────────

/// Horizontal extents (`x0..x1`) of every tab and of the "+" button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabRects {
    pub tabs: Vec<(usize, usize)>,
    pub plus: (usize, usize),
}

/// Equal-width tabs filling the bar, with `plus_w` reserved on the right for
/// the "+" button. The last tab absorbs the rounding remainder.
pub fn layout(total_w: usize, n: usize, plus_w: usize) -> TabRects {
    let n = n.max(1);
    let plus_w = plus_w.min(total_w / 4);
    let avail = total_w - plus_w;
    let w = avail / n;
    let tabs = (0..n)
        .map(|i| (i * w, if i + 1 == n { avail } else { (i + 1) * w }))
        .collect();
    TabRects { tabs, plus: (avail, total_w) }
}

/// Result of hit-testing the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabHit {
    Tab(usize),
    Close(usize),
    Plus,
}

impl TabHit {
    /// Tab index this hit belongs to, if any.
    pub fn tab(self) -> Option<usize> {
        match self {
            TabHit::Tab(i) | TabHit::Close(i) => Some(i),
            TabHit::Plus => None,
        }
    }
}

/// Close-button zone (`x0..x1`) of a tab.
pub fn close_zone(tab: (usize, usize), close_w: usize) -> (usize, usize) {
    (tab.1.saturating_sub(close_w), tab.1)
}

/// Hit-test x. `show_close` is false when the tab count is 1 (no "×").
pub fn hit_test(l: &TabRects, x: usize, close_w: usize, show_close: bool) -> Option<TabHit> {
    if x >= l.plus.0 && x < l.plus.1 {
        return Some(TabHit::Plus);
    }
    let i = l.tabs.iter().position(|&(a, b)| x >= a && x < b)?;
    let (c0, c1) = close_zone(l.tabs[i], close_w);
    if show_close && x >= c0 && x < c1 {
        Some(TabHit::Close(i))
    } else {
        Some(TabHit::Tab(i))
    }
}

/// Tab whose slot is under x, clamped to the first/last tab (drag targets).
pub fn index_at(l: &TabRects, x: usize) -> usize {
    l.tabs
        .iter()
        .position(|&(a, b)| x >= a && x < b)
        .unwrap_or(if x < l.tabs[0].0 { 0 } else { l.tabs.len() - 1 })
}

/// New position of the item at `idx` after `Vec::remove(from); insert(to)`.
pub fn remap_after_move(idx: usize, from: usize, to: usize) -> usize {
    if idx == from {
        to
    } else if from < to && idx > from && idx <= to {
        idx - 1
    } else if to < from && idx >= to && idx < from {
        idx + 1
    } else {
        idx
    }
}

/// New position of the item at `idx` after removing `closed`; `None` when the
/// item itself was removed.
pub fn remap_after_close(idx: usize, closed: usize) -> Option<usize> {
    use std::cmp::Ordering::*;
    match idx.cmp(&closed) {
        Less => Some(idx),
        Equal => None,
        Greater => Some(idx - 1),
    }
}

/// Fit `title` into `max_cols` terminal cells (wide chars count 2), cutting on
/// a character boundary. Returns the text and its width in cells.
pub fn fit_title(title: &str, max_cols: usize) -> (String, usize) {
    let mut out = String::new();
    let mut cols = 0;
    for c in title.chars() {
        let w = c.width().unwrap_or(0);
        if w == 0 {
            // Control / combining marks: keep combining marks attached, skip controls.
            if !c.is_control() && !out.is_empty() {
                out.push(c);
            }
            continue;
        }
        if cols + w > max_cols {
            break;
        }
        out.push(c);
        cols += w;
    }
    (out, cols)
}

/// Display width in cells.
pub fn text_cols(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

// ── Per-frame chrome state ──────────────────────────────────────────────

/// Mouse-driven chrome state the renderer needs, set by the app each frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChromeUi {
    pub tab_hover: Option<TabHit>,
    pub dragging_tab: Option<usize>,
    pub scrollbar: Option<crate::ui::scrollbar::BarDraw>,
    /// MCP clients currently connected (draws "MCP · n client(s)" in the tab bar).
    pub mcp_clients: u16,
}

impl ChromeUi {
    /// Hash of everything that changes the cached tab bar pixels.
    pub fn tab_sig(&self) -> u64 {
        let hover = match self.tab_hover {
            None => 0,
            Some(TabHit::Tab(i)) => 1 + 3 * (i as u64 + 1),
            Some(TabHit::Close(i)) => 2 + 3 * (i as u64 + 1),
            Some(TabHit::Plus) => 3,
        };
        let drag = self.dragging_tab.map_or(0, |i| i as u64 + 1);
        hover.wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ drag.wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            ^ (self.mcp_clients as u64).wrapping_mul(0x1656_67B1_9E37_79F9)
    }
}

// ── Drawing ─────────────────────────────────────────────────────────────

#[inline]
fn blend(color: Rgb, base: u32, cov: u32) -> u32 {
    let inv = 255 - cov;
    let r = (color.0 as u32 * cov + ((base >> 16) & 0xff) * inv) / 255;
    let g = (color.1 as u32 * cov + ((base >> 8) & 0xff) * inv) / 255;
    let b = (color.2 as u32 * cov + (base & 0xff) * inv) / 255;
    (r << 16) | (g << 8) | b
}

/// Draw `text` with one or two cells per char (CJK / emoji aware) at (x, y),
/// clipped to `clip_x1` and the buffer. Returns the x after the last glyph.
pub fn draw_text(
    buffer: &mut [u32],
    buf_w: usize,
    buf_h: usize,
    font: &mut FontManager,
    text: &str,
    x: usize,
    y: usize,
    clip_x1: usize,
    color: Rgb,
) -> usize {
    let cw = font.cell_width;
    let ch = font.cell_height;
    let clip = clip_x1.min(buf_w);
    let mut gx = x;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if w == 0 {
            continue;
        }
        let gw = cw * w;
        if gx + gw > clip {
            break;
        }
        if c != ' ' {
            let wide = font::is_wide(c);
            let bmp = if wide { font.rasterize_wide(c) } else { font.rasterize(c) };
            let stride = if wide { cw * 2 } else { cw };
            for cy in 0..ch {
                let py = y + cy;
                if py >= buf_h {
                    break;
                }
                for cx in 0..stride {
                    let cov = bmp[cy * stride + cx] as u32;
                    if cov == 0 {
                        continue;
                    }
                    let i = py * buf_w + gx + cx;
                    if i < buffer.len() {
                        buffer[i] = blend(color, buffer[i], cov);
                    }
                }
            }
        }
        gx += gw;
    }
    gx
}

// ── Inline rename field ─────────────────────────────────────────────────

/// Editing state for the tab being renamed.
#[derive(Clone, Debug)]
pub struct TabEditor {
    pub idx: usize,
    pub text: String,
    /// Caret position in chars.
    pub caret: usize,
    /// Whole text selected (as on entry): typing replaces it.
    pub all: bool,
}

impl TabEditor {
    pub fn new(idx: usize, text: &str) -> Self {
        Self { idx, text: text.to_string(), caret: text.chars().count(), all: !text.is_empty() }
    }

    /// Drop the selected text (if all is selected). True when it did.
    fn take_selection(&mut self) -> bool {
        if !std::mem::take(&mut self.all) {
            return false;
        }
        self.text.clear();
        self.caret = 0;
        true
    }

    pub fn select_all(&mut self) {
        self.all = !self.text.is_empty();
        self.caret = self.text.chars().count();
    }

    fn byte_at(&self, chars: usize) -> usize {
        self.text.char_indices().nth(chars).map_or(self.text.len(), |(b, _)| b)
    }

    pub fn insert_str(&mut self, s: &str) {
        let s: String = s.chars().filter(|c| !c.is_control()).collect();
        if s.is_empty() {
            return;
        }
        self.take_selection();
        let at = self.byte_at(self.caret);
        self.text.insert_str(at, &s);
        self.caret += s.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.take_selection() {
            return;
        }
        if self.caret > 0 {
            let a = self.byte_at(self.caret - 1);
            let b = self.byte_at(self.caret);
            self.text.replace_range(a..b, "");
            self.caret -= 1;
        }
    }

    pub fn delete(&mut self) {
        if self.take_selection() {
            return;
        }
        if self.caret < self.text.chars().count() {
            let a = self.byte_at(self.caret);
            let b = self.byte_at(self.caret + 1);
            self.text.replace_range(a..b, "");
        }
    }

    /// Left collapses a full selection to its start (macOS text fields).
    pub fn left(&mut self) {
        if std::mem::take(&mut self.all) {
            self.caret = 0;
        } else {
            self.caret = self.caret.saturating_sub(1);
        }
    }

    /// Right collapses a full selection to its end.
    pub fn right(&mut self) {
        if !std::mem::take(&mut self.all) {
            self.caret = (self.caret + 1).min(self.text.chars().count());
        }
    }

    pub fn home(&mut self) {
        self.all = false;
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.all = false;
        self.caret = self.text.chars().count();
    }
}

/// Pixel rect (x, y, w, h) of the rename field inside a tab slot.
pub fn editor_rect(tab: (usize, usize), bar_h: usize) -> (usize, usize, usize, usize) {
    let m = 6;
    let x = tab.0 + m;
    let w = tab.1.saturating_sub(tab.0 + 2 * m);
    let y = 4;
    (x, y, w, bar_h.saturating_sub(y + 4))
}

/// Draw the rename field (with caret and IME preedit) over the tab bar.
pub fn render_editor(
    buffer: &mut [u32],
    buf_w: usize,
    buf_h: usize,
    font: &mut FontManager,
    theme: &crate::config::Theme,
    ed: &TabEditor,
    preedit: &str,
    tab: (usize, usize),
    bar_h: usize,
) {
    let (x, y, w, h) = editor_rect(tab, bar_h);
    if w < 8 || h < 6 || y + h > buf_h {
        return;
    }
    let cw = font.cell_width;
    let ch = font.cell_height;
    let bg = crate::ui::pack_rgb(crate::ui::lighten(theme.bg, 14));
    let border = crate::ui::pack_rgb(theme.cursor);
    crate::ui::fill_rect(buffer, buf_w, x, y, w, h, bg);
    crate::ui::draw_border(buffer, buf_w, x, y, w, h, border);

    // Show the tail of the text when it does not fit.
    let pad = 6;
    let inner = w.saturating_sub(2 * pad);
    let max_cols = (inner / cw.max(1)).max(1);
    let before: String = ed.text.chars().take(ed.caret).collect();
    let after: String = ed.text.chars().skip(ed.caret).collect();
    let mut shown = format!("{before}{preedit}");
    // Drop leading chars until caret + preedit fit (leave one cell for caret).
    while text_cols(&shown) + 1 > max_cols && !shown.is_empty() {
        shown.remove(0);
    }
    let ty = y + h.saturating_sub(ch) / 2;
    let tx = x + pad;
    let clip = x + w - 2;
    let fg = theme.fg;
    if ed.all && preedit.is_empty() {
        // Selected text (typing replaces it).
        let sw = (text_cols(&shown) * cw).min(clip.saturating_sub(tx));
        let sel = crate::ui::pack_rgb(crate::ui::lighten(theme.bg, 45));
        crate::ui::fill_rect(buffer, buf_w, tx, ty, sw, ch, sel);
    }
    let end = draw_text(buffer, buf_w, buf_h, font, &shown, tx, ty, clip, fg);
    if !preedit.is_empty() {
        // Underline the composition.
        let pw = text_cols(preedit) * cw;
        let ux = end.saturating_sub(pw);
        crate::ui::fill_rect(buffer, buf_w, ux, (ty + ch).saturating_sub(2), end - ux, 1, border);
    }
    // Caret.
    crate::ui::fill_rect(buffer, buf_w, end.min(clip.saturating_sub(1)), ty, 2, ch, border);
    let room = clip.saturating_sub(end + 2);
    let (tail, _) = fit_title(&after, room / cw.max(1));
    draw_text(buffer, buf_w, buf_h, font, &tail, end + 2, ty, clip, fg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_starts_fully_selected_and_typing_replaces() {
        // Regression: the field opened with the caret after the old title, so
        // typing "build" produced "Tab 1build".
        let mut e = TabEditor::new(0, "Tab 1");
        assert!(e.all);
        e.insert_str("b");
        e.insert_str("uild");
        assert_eq!((e.text.as_str(), e.caret, e.all), ("build", 5, false));
        // IME commits go through insert_str too.
        let mut e = TabEditor::new(0, "Tab 1");
        e.insert_str("构建");
        assert_eq!(e.text, "构建");
        // Control chars / empty commits leave the selection alone.
        let mut e = TabEditor::new(0, "Tab 1");
        e.insert_str("\r");
        assert_eq!((e.text.as_str(), e.all), ("Tab 1", true));
    }

    #[test]
    fn editor_selection_collapses_like_a_text_field() {
        let mut e = TabEditor::new(0, "abc");
        e.right();
        assert_eq!((e.caret, e.all), (3, false));
        e.insert_str("d");
        assert_eq!(e.text, "abcd");
        let mut e = TabEditor::new(0, "abc");
        e.left();
        assert_eq!((e.caret, e.all), (0, false));
        e.insert_str("x");
        assert_eq!(e.text, "xabc");
        let mut e = TabEditor::new(0, "abc");
        e.backspace();
        assert_eq!((e.text.as_str(), e.caret), ("", 0));
        let mut e = TabEditor::new(0, "abc");
        e.delete();
        assert_eq!(e.text, "");
        let mut e = TabEditor::new(0, "abc");
        e.home();
        e.delete();
        assert_eq!(e.text, "bc");
        e.select_all();
        assert!(e.all);
        e.insert_str("z");
        assert_eq!(e.text, "z");
        // Empty title: nothing to select.
        assert!(!TabEditor::new(0, "").all);
    }

    #[test]
    fn layout_reserves_plus_and_fills() {
        let l = layout(1000, 4, 40);
        assert_eq!(l.plus, (960, 1000));
        assert_eq!(l.tabs.len(), 4);
        assert_eq!(l.tabs[0].0, 0);
        assert_eq!(l.tabs[3].1, 960);
        for w in l.tabs.windows(2) {
            assert_eq!(w[0].1, w[1].0, "tabs are contiguous");
        }
    }

    #[test]
    fn layout_single_and_tiny() {
        let l = layout(300, 1, 30);
        assert_eq!(l.tabs, vec![(0, 270)]);
        // Plus never takes more than a quarter of the bar.
        let l = layout(100, 2, 90);
        assert_eq!(l.plus, (75, 100));
    }

    #[test]
    fn hit_testing() {
        let l = layout(1000, 4, 40); // tabs of 240
        assert_eq!(hit_test(&l, 5, 20, true), Some(TabHit::Tab(0)));
        assert_eq!(hit_test(&l, 235, 20, true), Some(TabHit::Close(0)));
        assert_eq!(hit_test(&l, 235, 20, false), Some(TabHit::Tab(0)));
        assert_eq!(hit_test(&l, 250, 20, true), Some(TabHit::Tab(1)));
        assert_eq!(hit_test(&l, 970, 20, true), Some(TabHit::Plus));
        assert_eq!(hit_test(&l, 1000, 20, true), None);
    }

    #[test]
    fn drag_target_clamps() {
        let l = layout(1000, 4, 40);
        assert_eq!(index_at(&l, 0), 0);
        assert_eq!(index_at(&l, 500), 2);
        assert_eq!(index_at(&l, 990), 3);
        assert_eq!(index_at(&l, 5000), 3);
    }

    /// Reference implementation: perform the move on a Vec of labels.
    fn moved(n: usize, from: usize, to: usize) -> Vec<usize> {
        let mut v: Vec<usize> = (0..n).collect();
        let x = v.remove(from);
        v.insert(to, x);
        v
    }

    #[test]
    fn move_remap_matches_vec_semantics() {
        for n in 1..6 {
            for from in 0..n {
                for to in 0..n {
                    let v = moved(n, from, to);
                    for idx in 0..n {
                        let new = remap_after_move(idx, from, to);
                        assert_eq!(v[new], idx, "n={n} from={from} to={to} idx={idx}");
                    }
                }
            }
        }
    }

    #[test]
    fn close_remap() {
        assert_eq!(remap_after_close(0, 2), Some(0));
        assert_eq!(remap_after_close(2, 2), None);
        assert_eq!(remap_after_close(3, 2), Some(2));
    }

    #[test]
    fn title_fit_is_width_aware() {
        assert_eq!(fit_title("hello", 3), ("hel".to_string(), 3));
        assert_eq!(fit_title("hi", 10), ("hi".to_string(), 2));
        // Wide chars take two cells and are never split.
        assert_eq!(fit_title("终端标签", 5), ("终端".to_string(), 4));
        assert_eq!(fit_title("终端标签", 8), ("终端标签".to_string(), 8));
        assert_eq!(fit_title("a终", 2), ("a".to_string(), 1));
        assert_eq!(text_cols("a终b"), 4);
    }

    #[test]
    fn editor_edits_by_char() {
        let mut e = TabEditor::new(0, "终端");
        assert_eq!(e.caret, 2);
        e.end(); // collapse the initial select-all, caret after the text
        e.insert_str("ab\n");
        assert_eq!(e.text, "终端ab");
        e.left();
        e.backspace();
        assert_eq!(e.text, "终端b");
        e.home();
        e.delete();
        assert_eq!(e.text, "端b");
        e.end();
        assert_eq!(e.caret, 2);
        e.insert_str("中");
        assert_eq!(e.text, "端b中");
        e.home();
        e.backspace(); // no-op at start
        assert_eq!(e.text, "端b中");
    }
}
