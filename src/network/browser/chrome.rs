//! Browser chrome: geometry (single source of truth, physical pixels), hit
//! testing, and softbuffer rendering of the toolbar / divider.

use crate::config::{Rgb, Theme};
use crate::renderer::font::FontManager;
use crate::ui::{fill_rect, lighten, pack_rgb, render_text};

use super::BrowserUi;
use crate::network::WebViewPane;

pub const MIN_RATIO: f32 = 0.2;
pub const MAX_RATIO: f32 = 0.8;
pub const DEFAULT_RATIO: f32 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub fn contains(&self, px: usize, py: usize) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Back,
    Forward,
    Reload,
    Field,
    Maximize,
    Close,
    Divider,
    /// Toolbar background / webview area (inside the browser but not a control).
    Toolbar,
}

/// All browser geometry in physical window pixels.
#[derive(Clone, Copy, Debug)]
pub struct BrowserLayout {
    /// Width left to the terminal (0 when maximized).
    pub terminal_w: usize,
    /// Gutter between terminal and browser (holds the 1px separator + grab zone).
    pub gutter: Rect,
    pub toolbar: Rect,
    /// Native webview area.
    pub web: Rect,
    pub back: Rect,
    pub forward: Rect,
    pub reload: Rect,
    pub field: Rect,
    pub maximize: Rect,
    pub close: Rect,
    pub scale: f64,
}

impl BrowserLayout {
    pub fn toolbar_height(cell_h: usize) -> usize {
        cell_h + 12
    }

    #[allow(clippy::too_many_arguments)]
    pub fn compute(
        win_w: usize,
        win_h: usize,
        tab_bar_h: usize,
        cell_w: usize,
        cell_h: usize,
        scale: f64,
        ratio: f32,
        maximized: bool,
    ) -> Self {
        let th = Self::toolbar_height(cell_h);
        let gutter_w = if maximized { 0 } else { ((4.0 * scale).round() as usize).max(3) };
        let ratio = ratio.clamp(MIN_RATIO, MAX_RATIO);
        let browser_w = if maximized { win_w } else { ((win_w as f32 * ratio).round() as usize).min(win_w) };
        let split_x = win_w - browser_w;
        let x0 = split_x + gutter_w;
        let inner_w = win_w.saturating_sub(x0);
        let y = tab_bar_h;

        let toolbar = Rect { x: x0, y, w: inner_w, h: th };
        let web = Rect { x: x0, y: y + th, w: inner_w, h: win_h.saturating_sub(y + th) };
        let gutter = Rect { x: split_x, y, w: gutter_w, h: win_h.saturating_sub(y) };

        let pad = 4;
        let btn = th.max(cell_w * 2 + 8);
        let mut cx = x0 + pad;
        let mut take = |w: usize| {
            let r = Rect { x: cx, y, w, h: th };
            cx += w;
            r
        };
        let back = take(btn);
        let forward = take(btn);
        let reload = take(btn);
        let field_x = cx + 2;
        let right_btns = btn * 2 + pad;
        let field_end = (x0 + inner_w).saturating_sub(right_btns + 2);
        let field = Rect {
            x: field_x,
            y: y + 3,
            w: field_end.saturating_sub(field_x),
            h: th.saturating_sub(6),
        };
        let maximize = Rect { x: field_end + 2, y, w: btn, h: th };
        let close = Rect { x: field_end + 2 + btn, y, w: btn, h: th };

        BrowserLayout {
            terminal_w: split_x,
            gutter,
            toolbar,
            web,
            back,
            forward,
            reload,
            field,
            maximize,
            close,
            scale,
        }
    }

    /// Native webview bounds in logical units (wry takes logical, we draw in physical).
    pub fn web_logical(&self) -> (f64, f64, f64, f64) {
        let s = self.scale.max(0.1);
        (
            self.web.x as f64 / s,
            self.web.y as f64 / s,
            self.web.w as f64 / s,
            self.web.h as f64 / s,
        )
    }

    /// Divider grab zone: the gutter plus a couple of pixels on the terminal side.
    pub fn divider_zone(&self) -> Option<Rect> {
        if self.gutter.w == 0 {
            return None;
        }
        let pad = 3;
        Some(Rect {
            x: self.gutter.x.saturating_sub(pad),
            y: self.gutter.y,
            w: self.gutter.w + pad,
            h: self.gutter.h,
        })
    }

    pub fn hit(&self, px: usize, py: usize) -> Option<Hit> {
        if let Some(d) = self.divider_zone() {
            if d.contains(px, py) {
                return Some(Hit::Divider);
            }
        }
        if self.toolbar.contains(px, py) {
            let order = [
                (self.back, Hit::Back),
                (self.forward, Hit::Forward),
                (self.reload, Hit::Reload),
                (self.maximize, Hit::Maximize),
                (self.close, Hit::Close),
            ];
            for (r, h) in order {
                if r.contains(px, py) {
                    return Some(h);
                }
            }
            if self.field.contains(px, py) {
                return Some(Hit::Field);
            }
            return Some(Hit::Toolbar);
        }
        if self.web.contains(px, py) {
            return Some(Hit::Toolbar);
        }
        None
    }

    /// Where the field's text starts, and how many columns fit.
    pub fn field_text_origin(&self, cell_w: usize) -> (usize, usize) {
        let x = self.field.x + 6;
        let cols = self.field.w.saturating_sub(12) / cell_w.max(1);
        (x, cols)
    }
}

// ── Drawing helpers ──

fn plot(buf: &mut [u32], bw: usize, x: i64, y: i64, px: u32) {
    if x < 0 || y < 0 || x as usize >= bw {
        return;
    }
    let idx = y as usize * bw + x as usize;
    if idx < buf.len() {
        buf[idx] = px;
    }
}

fn blob(buf: &mut [u32], bw: usize, x: i64, y: i64, t: i64, px: u32) {
    let lo = -(t / 2);
    for dy in lo..lo + t {
        for dx in lo..lo + t {
            plot(buf, bw, x + dx, y + dy, px);
        }
    }
}

/// Thick line (Bresenham with a square brush).
fn line(buf: &mut [u32], bw: usize, x0: i64, y0: i64, x1: i64, y1: i64, t: i64, px: u32) {
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let (mut x, mut y, mut err) = (x0, y0, dx + dy);
    loop {
        blob(buf, bw, x, y, t, px);
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Back,
    Forward,
    Reload,
    Stop,
    Maximize,
    Restore,
    Close,
}

fn draw_icon(buf: &mut [u32], bw: usize, r: Rect, icon: Icon, color: Rgb, thick: i64) {
    let px = pack_rgb(color);
    let cx = (r.x + r.w / 2) as i64;
    let cy = (r.y + r.h / 2) as i64;
    let s = (r.h.min(r.w) as i64 / 4).max(3); // half-extent of the glyph
    match icon {
        Icon::Back => {
            line(buf, bw, cx + s / 2, cy - s, cx - s / 2, cy, thick, px);
            line(buf, bw, cx - s / 2, cy, cx + s / 2, cy + s, thick, px);
        }
        Icon::Forward => {
            line(buf, bw, cx - s / 2, cy - s, cx + s / 2, cy, thick, px);
            line(buf, bw, cx + s / 2, cy, cx - s / 2, cy + s, thick, px);
        }
        Icon::Close | Icon::Stop => {
            let d = s - s / 4;
            line(buf, bw, cx - d, cy - d, cx + d, cy + d, thick, px);
            line(buf, bw, cx - d, cy + d, cx + d, cy - d, thick, px);
        }
        Icon::Maximize => {
            let d = s - s / 4;
            rect_outline(buf, bw, cx - d, cy - d, cx + d, cy + d, thick, px);
        }
        Icon::Restore => {
            let d = s - s / 4;
            let o = (d / 2).max(2);
            rect_outline(buf, bw, cx - d, cy - d + o, cx + d - o, cy + d, thick, px);
            // back square: only the two visible edges
            line(buf, bw, cx - d + o, cy - d + o, cx - d + o, cy - d, thick, px);
            line(buf, bw, cx - d + o, cy - d, cx + d, cy - d, thick, px);
            line(buf, bw, cx + d, cy - d, cx + d, cy + d - o, thick, px);
            line(buf, bw, cx + d, cy + d - o, cx + d - o, cy + d - o, thick, px);
        }
        Icon::Reload => {
            // ~300 degree ring, gap at the upper right, with an arrowhead.
            let rad = s as f64;
            let start = (-20.0f64).to_radians(); // arrow tip side
            let sweep = 300.0f64.to_radians();
            let steps = (rad * 8.0) as usize + 8;
            let mut last = None;
            for i in 0..=steps {
                let a = start - sweep * (i as f64 / steps as f64);
                let (x, y) = (cx as f64 + rad * a.cos(), cy as f64 + rad * a.sin());
                let p = (x.round() as i64, y.round() as i64);
                if let Some((lx, ly)) = last {
                    line(buf, bw, lx, ly, p.0, p.1, thick, px);
                }
                last = Some(p);
            }
            // arrowhead at the start point, pointing clockwise (towards a increasing)
            let tip = (cx as f64 + rad * start.cos(), cy as f64 + rad * start.sin());
            let h = (rad * 0.9).max(3.0);
            line(buf, bw, tip.0.round() as i64, tip.1.round() as i64, (tip.0 + h).round() as i64, tip.1.round() as i64, thick, px);
            line(buf, bw, tip.0.round() as i64, tip.1.round() as i64, tip.0.round() as i64, (tip.1 - h).round() as i64, thick, px);
        }
    }
}

fn rect_outline(buf: &mut [u32], bw: usize, x0: i64, y0: i64, x1: i64, y1: i64, t: i64, px: u32) {
    line(buf, bw, x0, y0, x1, y0, t, px);
    line(buf, bw, x1, y0, x1, y1, t, px);
    line(buf, bw, x1, y1, x0, y1, t, px);
    line(buf, bw, x0, y1, x0, y0, t, px);
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t) as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

fn fit(chars: &[char], cols: usize) -> String {
    if chars.len() <= cols {
        return chars.iter().collect();
    }
    if cols <= 2 {
        return chars.iter().take(cols).collect();
    }
    let mut s: String = chars.iter().take(cols - 2).collect();
    s.push_str("..");
    s
}

/// What the toolbar shows about the page (a `WebViewPane` snapshot, which
/// the headless screenshot renderer can build without a native webview).
pub struct PageInfo<'a> {
    pub url: &'a str,
    pub title: &'a str,
    pub loading: bool,
    pub can_back: bool,
    pub can_forward: bool,
}

/// Draw the gutter, toolbar, address field and progress bar.
#[allow(clippy::too_many_arguments)]
pub fn render(
    buf: &mut [u32],
    bw: usize,
    bh: usize,
    layout: &BrowserLayout,
    ui: &mut BrowserUi,
    pane: &WebViewPane,
    maximized: bool,
    font: &mut FontManager,
    theme: &Theme,
) {
    let info = PageInfo {
        url: &pane.url,
        title: &pane.title,
        loading: pane.loading,
        can_back: pane.can_back,
        can_forward: pane.can_forward,
    };
    render_page(buf, bw, bh, layout, ui, &info, maximized, font, theme);
}

/// [`render`] with the page state passed explicitly.
#[allow(clippy::too_many_arguments)]
pub fn render_page(
    buf: &mut [u32],
    bw: usize,
    bh: usize,
    layout: &BrowserLayout,
    ui: &mut BrowserUi,
    pane: &PageInfo,
    maximized: bool,
    font: &mut FontManager,
    theme: &Theme,
) {
    if bw == 0 || bh == 0 {
        return;
    }
    let cw = font.cell_width;
    let ch = font.cell_height;
    let fg = theme.fg;
    let accent = theme.cursor;
    let bar_bg = crate::ui::darken(theme.bg, 10);
    let bar_px = pack_rgb(bar_bg);
    let hover_bg = pack_rgb(lighten(bar_bg, 18));
    let thick = ((layout.scale.round() as i64).max(1)).min(2);

    // Gutter + separator line (accent while hovered / dragged).
    let g = layout.gutter;
    if g.w > 0 {
        fill_rect(buf, bw, g.x, g.y, g.w, g.h.min(bh.saturating_sub(g.y)), bar_px);
        let active = ui.divider_hover || ui.divider_drag;
        let line_c = if active { accent } else { lighten(bar_bg, 22) };
        let lw = if active { 2 } else { 1 };
        fill_rect(buf, bw, g.x, g.y, lw, g.h.min(bh.saturating_sub(g.y)), pack_rgb(line_c));
    }

    // Toolbar background.
    let t = layout.toolbar;
    fill_rect(buf, bw, t.x, t.y, t.w, t.h.min(bh.saturating_sub(t.y)), bar_px);

    // Buttons.
    let enabled = fg;
    let disabled = crate::ui::dim(fg, 0.35);
    let normal = crate::ui::dim(fg, 0.8);
    let btns = [
        (layout.back, super::chrome::Hit::Back, Icon::Back, pane.can_back),
        (layout.forward, Hit::Forward, Icon::Forward, pane.can_forward),
        (layout.reload, Hit::Reload, if pane.loading { Icon::Stop } else { Icon::Reload }, true),
        (layout.maximize, Hit::Maximize, if maximized { Icon::Restore } else { Icon::Maximize }, true),
        (layout.close, Hit::Close, Icon::Close, true),
    ];
    for (r, hit, icon, on) in btns {
        let hovered = ui.hover == Some(hit) && on;
        if hovered {
            let inset = 3;
            fill_rect(buf, bw, r.x + 1, r.y + inset, r.w.saturating_sub(2), r.h.saturating_sub(inset * 2), hover_bg);
        }
        let c = if !on { disabled } else if hovered { enabled } else { normal };
        draw_icon(buf, bw, r, icon, c, thick);
    }

    // Address field.
    let f = layout.field;
    if f.w > 8 {
        let editing = ui.editing;
        let field_bg = if editing { lighten(bar_bg, 22) } else { lighten(bar_bg, 12) };
        fill_rect(buf, bw, f.x, f.y, f.w, f.h, pack_rgb(field_bg));
        if editing {
            let bp = pack_rgb(accent);
            fill_rect(buf, bw, f.x, f.y, f.w, 1, bp);
            fill_rect(buf, bw, f.x, f.y + f.h - 1, f.w, 1, bp);
            fill_rect(buf, bw, f.x, f.y, 1, f.h, bp);
            fill_rect(buf, bw, f.x + f.w - 1, f.y, 1, f.h, bp);
        }
        let (tx, cols) = layout.field_text_origin(cw);
        let ty = f.y + f.h.saturating_sub(ch) / 2;
        if editing {
            let (s, e) = ui.field.visible_range(cols);
            if let Some((a, b)) = ui.field.selection() {
                let (a, b) = (a.max(s), b.min(e));
                if a < b {
                    let sel = pack_rgb(mix(field_bg, accent, 0.45));
                    fill_rect(buf, bw, tx + (a - s) * cw, ty, (b - a) * cw, ch, sel);
                }
            }
            let shown: String = ui.field.chars()[s..e].iter().collect();
            render_text(buf, bw, font, &shown, tx, ty, fg);
            // Cursor bar.
            let col = ui.field.cursor() - s;
            let cx = tx + col * cw;
            if cx < f.x + f.w {
                fill_rect(buf, bw, cx, ty, 2, ch, pack_rgb(accent));
            }
        } else if cols > 0 {
            // Title (bright) when known, URL otherwise; URL dimmed after the title if it fits.
            let url: Vec<char> = pane.url.chars().collect();
            let title: Vec<char> = pane.title.chars().collect();
            if title.is_empty() {
                render_text(buf, bw, font, &fit(&url, cols), tx, ty, crate::ui::dim(fg, 0.85));
            } else {
                let title_s = fit(&title, cols);
                let used = title_s.chars().count();
                render_text(buf, bw, font, &title_s, tx, ty, fg);
                let rest = cols.saturating_sub(used + 3);
                if rest >= 8 {
                    render_text(buf, bw, font, &fit(&url, rest), tx + (used + 3) * cw, ty, crate::ui::dim(fg, 0.4));
                }
            }
        }
    }

    // Bottom separator.
    let sep_y = t.y + t.h.saturating_sub(1);
    if sep_y < bh {
        fill_rect(buf, bw, t.x, sep_y, t.w, 1, pack_rgb(lighten(bar_bg, 8)));
    }

    // Loading progress sweep under the toolbar.
    if pane.loading {
        let bar_h = ((2.0 * layout.scale).round() as usize).max(2);
        let y = (t.y + t.h).saturating_sub(bar_h);
        let ms = ui.anim_ms();
        let period = 1100u128;
        let seg = (t.w / 3).max(20);
        let frac = (ms % period) as f64 / period as f64;
        let start = (frac * (t.w + seg) as f64) as isize - seg as isize;
        let x0 = start.max(0) as usize;
        let x1 = ((start + seg as isize).max(0) as usize).min(t.w);
        if x1 > x0 && y + bar_h <= bh {
            fill_rect(buf, bw, t.x + x0, y, x1 - x0, bar_h, pack_rgb(accent));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(maximized: bool) -> BrowserLayout {
        BrowserLayout::compute(2000, 1200, 40, 16, 32, 2.0, 0.5, maximized)
    }

    #[test]
    fn split_geometry_is_consistent() {
        let l = layout(false);
        assert_eq!(l.terminal_w, 1000);
        assert_eq!(l.gutter.x, 1000);
        assert_eq!(l.toolbar.x, l.gutter.x + l.gutter.w);
        assert_eq!(l.web.x + l.web.w, 2000);
        assert_eq!(l.web.y, l.toolbar.y + l.toolbar.h);
        assert_eq!(l.web.y + l.web.h, 1200);
    }

    #[test]
    fn maximized_covers_window() {
        let l = layout(true);
        assert_eq!(l.terminal_w, 0);
        assert_eq!(l.web.x, 0);
        assert_eq!(l.web.w, 2000);
        assert!(l.divider_zone().is_none());
    }

    #[test]
    fn ratio_is_clamped() {
        let l = BrowserLayout::compute(1000, 600, 40, 8, 16, 1.0, 0.99, false);
        assert_eq!(l.terminal_w, 200);
        let l = BrowserLayout::compute(1000, 600, 40, 8, 16, 1.0, 0.0, false);
        assert_eq!(l.terminal_w, 800);
    }

    #[test]
    fn logical_bounds_divide_by_scale() {
        let l = layout(false);
        let (x, y, w, h) = l.web_logical();
        assert_eq!(x * 2.0, l.web.x as f64);
        assert_eq!(y * 2.0, l.web.y as f64);
        assert_eq!(w * 2.0, l.web.w as f64);
        assert_eq!(h * 2.0, l.web.h as f64);
    }

    #[test]
    fn hit_testing() {
        let l = layout(false);
        let c = |r: Rect| (r.x + r.w / 2, r.y + r.h / 2);
        for (r, h) in [
            (l.back, Hit::Back),
            (l.forward, Hit::Forward),
            (l.reload, Hit::Reload),
            (l.field, Hit::Field),
            (l.maximize, Hit::Maximize),
            (l.close, Hit::Close),
        ] {
            let (x, y) = c(r);
            assert_eq!(l.hit(x, y), Some(h));
        }
        assert_eq!(l.hit(l.gutter.x + 1, 600), Some(Hit::Divider));
        assert_eq!(l.hit(l.gutter.x - 2, 600), Some(Hit::Divider));
        assert_eq!(l.hit(100, 600), None);
        assert_eq!(l.hit(1500, 600), Some(Hit::Toolbar));
    }

    #[test]
    fn narrow_window_does_not_underflow() {
        let l = BrowserLayout::compute(120, 80, 20, 8, 16, 1.0, 0.5, false);
        assert!(l.field.w < 120);
        let _ = l.hit(10, 30);
    }

    #[test]
    fn fit_truncates_with_dots() {
        let c: Vec<char> = "abcdefghij".chars().collect();
        assert_eq!(fit(&c, 20), "abcdefghij");
        assert_eq!(fit(&c, 6), "abcd..");
    }
}
