//! Low-level pixel drawing: alpha blending, anti-aliased rounded rectangles,
//! soft shadows and truncating text helpers.

use super::{Ctx, Rect};
use crate::config::Rgb;

/// Rounded-corner mask bits for [`Ctx::fill_rrect_ex`].
pub const TL: u8 = 1;
pub const TR: u8 = 2;
pub const BL: u8 = 4;
pub const BR: u8 = 8;
pub const ALL: u8 = TL | TR | BL | BR;
pub const TOP: u8 = TL | TR;
pub const BOTTOM: u8 = BL | BR;

#[inline]
pub fn blend_px(dst: u32, c: Rgb, a: u32) -> u32 {
    if a == 0 {
        return dst;
    }
    if a >= 255 {
        return ((c.0 as u32) << 16) | ((c.1 as u32) << 8) | c.2 as u32;
    }
    let inv = 255 - a;
    let r = (c.0 as u32 * a + ((dst >> 16) & 0xff) * inv + 127) / 255;
    let g = (c.1 as u32 * a + ((dst >> 8) & 0xff) * inv + 127) / 255;
    let b = (c.2 as u32 * a + (dst & 0xff) * inv + 127) / 255;
    (r << 16) | (g << 8) | b
}

/// Signed distance from pixel centre `(px,py)` to a rounded rect (negative inside).
#[inline]
fn rr_sdf(px: f32, py: f32, r: &Rect, rad: f32) -> f32 {
    let cx = r.x as f32 + r.w as f32 / 2.0;
    let cy = r.y as f32 + r.h as f32 / 2.0;
    let hx = r.w as f32 / 2.0 - rad;
    let hy = r.h as f32 / 2.0 - rad;
    let qx = (px - cx).abs() - hx;
    let qy = (py - cy).abs() - hy;
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - rad
}

/// Ellipsize `s` to at most `max_cols` characters, ending in "…" when cut.
pub fn ellipsize(s: &str, max_cols: usize) -> String {
    let n = s.chars().count();
    if n <= max_cols {
        return s.to_string();
    }
    if max_cols == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max_cols - 1).collect();
    out.push('…');
    out
}

/// Column heights (0..=h) for a sparkline of `data` resampled to `w` columns
/// and scaled so `max` fills `h`.
pub fn spark_heights(data: &[f32], w: usize, h: usize, max: f32) -> Vec<usize> {
    if data.is_empty() || w == 0 || max <= 0.0 {
        return vec![0; w];
    }
    (0..w)
        .map(|x| {
            let pos = if w > 1 { x as f32 * (data.len() - 1) as f32 / (w - 1) as f32 } else { 0.0 };
            let (i0, f) = (pos.floor() as usize, pos.fract());
            let i1 = (i0 + 1).min(data.len() - 1);
            let v = data[i0] * (1.0 - f) + data[i1] * f;
            ((v / max).clamp(0.0, 1.0) * h as f32).round() as usize
        })
        .collect()
}

impl<'a> Ctx<'a> {
    #[inline]
    pub fn put(&mut self, x: usize, y: usize, c: Rgb, a: u32) {
        if x < self.w && y < self.h {
            let i = y * self.w + x;
            if i < self.buf.len() {
                self.buf[i] = blend_px(self.buf[i], c, a);
            }
        }
    }

    /// Alpha-blended rectangle (square corners).
    pub fn fill_a(&mut self, r: Rect, c: Rgb, alpha: u8) {
        let x1 = (r.x + r.w).min(self.w);
        let y1 = (r.y + r.h).min(self.h);
        for y in r.y..y1 {
            for x in r.x..x1 {
                let i = y * self.w + x;
                if i < self.buf.len() {
                    self.buf[i] = blend_px(self.buf[i], c, alpha as u32);
                }
            }
        }
    }

    pub fn fill(&mut self, r: Rect, c: Rgb) {
        self.fill_a(r, c, 255);
    }

    pub fn hline(&mut self, x: usize, y: usize, w: usize, c: Rgb) {
        self.fill(Rect::new(x, y, w, 1), c);
    }

    pub fn vline(&mut self, x: usize, y: usize, h: usize, c: Rgb) {
        self.fill(Rect::new(x, y, 1, h), c);
    }

    /// Anti-aliased rounded rectangle with all corners rounded.
    pub fn fill_rrect(&mut self, r: Rect, rad: usize, c: Rgb) {
        self.fill_rrect_ex(r, rad, c, 255, ALL);
    }

    /// Anti-aliased rounded rectangle; `corners` selects which are rounded.
    pub fn fill_rrect_ex(&mut self, r: Rect, rad: usize, c: Rgb, alpha: u8, corners: u8) {
        if r.w == 0 || r.h == 0 {
            return;
        }
        let rad = rad.min(r.w / 2).min(r.h / 2);
        let radf = rad as f32;
        let x1 = (r.x + r.w).min(self.w);
        let y1 = (r.y + r.h).min(self.h);
        for y in r.y..y1 {
            let top = y < r.y + rad;
            let bot = y + rad >= r.y + r.h;
            let corner_row = rad > 0 && (top || bot);
            for x in r.x..x1 {
                let left = x < r.x + rad;
                let right = x + rad >= r.x + r.w;
                let mut a = alpha as u32;
                if corner_row && (left || right) {
                    let mask = match (top, left) {
                        (true, true) => TL,
                        (true, false) => TR,
                        (false, true) => BL,
                        (false, false) => BR,
                    };
                    if corners & mask != 0 {
                        let d = rr_sdf(x as f32 + 0.5, y as f32 + 0.5, &r, radf);
                        let cov = (0.5 - d).clamp(0.0, 1.0);
                        a = (a as f32 * cov).round() as u32;
                    }
                }
                if a > 0 {
                    let i = y * self.w + x;
                    if i < self.buf.len() {
                        self.buf[i] = blend_px(self.buf[i], c, a);
                    }
                }
            }
        }
    }

    /// 1px (or `t`px) anti-aliased outline drawn over whatever is below.
    pub fn stroke_rrect(&mut self, r: Rect, rad: usize, t: usize, c: Rgb) {
        if r.w < 2 * t || r.h < 2 * t {
            return;
        }
        let rad = rad.min(r.w / 2).min(r.h / 2);
        let inner = Rect::new(r.x + t, r.y + t, r.w - 2 * t, r.h - 2 * t);
        let irad = rad.saturating_sub(t);
        let x1 = (r.x + r.w).min(self.w);
        let y1 = (r.y + r.h).min(self.h);
        for y in r.y..y1 {
            let edge_row = y < r.y + t + rad || y + t + rad >= r.y + r.h;
            for x in r.x..x1 {
                let edge_col = x < r.x + t + rad || x + t + rad >= r.x + r.w;
                if !edge_row && !edge_col {
                    // jump straight to the right edge band
                    continue;
                }
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let co = (0.5 - rr_sdf(fx, fy, &r, rad as f32)).clamp(0.0, 1.0);
                let ci = (0.5 - rr_sdf(fx, fy, &inner, irad as f32)).clamp(0.0, 1.0);
                let a = ((co - ci).max(0.0) * 255.0).round() as u32;
                if a > 0 {
                    let i = y * self.w + x;
                    if i < self.buf.len() {
                        self.buf[i] = blend_px(self.buf[i], c, a);
                    }
                }
            }
        }
    }

    /// Multi-step soft drop shadow around a rounded rect (drawn beneath it).
    pub fn shadow(&mut self, r: Rect, rad: usize) {
        let blur = self.tk.shadow_blur;
        let off = self.tk.shadow_offset;
        let max_a = self.tk.shadow_alpha as f32;
        let s = Rect::new(r.x, r.y + off, r.w, r.h);
        let y0 = s.y.saturating_sub(blur);
        let y1 = (s.y + s.h + blur).min(self.h);
        let x0 = s.x.saturating_sub(blur);
        let x1 = (s.x + s.w + blur).min(self.w);
        let core_x0 = r.x + rad;
        let core_x1 = (r.x + r.w).saturating_sub(rad);
        let core_y0 = r.y + rad;
        let core_y1 = (r.y + r.h).saturating_sub(rad);
        let radf = rad as f32;
        for y in y0..y1 {
            let in_core_row = y >= core_y0 && y < core_y1;
            let mut x = x0;
            while x < x1 {
                if in_core_row && x >= core_x0 && x < core_x1 {
                    x = core_x1; // hidden under the panel itself
                    continue;
                }
                let d = rr_sdf(x as f32 + 0.5, y as f32 + 0.5, &s, radf);
                if d < blur as f32 {
                    let k = if d <= 0.0 { 1.0 } else { 1.0 - d / blur as f32 };
                    let a = (max_a * k * k).round() as u32;
                    if a > 0 {
                        let i = y * self.w + x;
                        if i < self.buf.len() {
                            self.buf[i] = blend_px(self.buf[i], (0, 0, 0), a);
                        }
                    }
                }
                x += 1;
            }
        }
    }

    /// Darken the whole buffer; `amount` is the darkening fraction (0..=1).
    pub fn backdrop(&mut self, amount: f32) {
        let keep = ((1.0 - amount.clamp(0.0, 1.0)) * 256.0) as u32;
        for px in self.buf.iter_mut() {
            let r = ((*px >> 16) & 0xff) * keep >> 8;
            let g = ((*px >> 8) & 0xff) * keep >> 8;
            let b = (*px & 0xff) * keep >> 8;
            *px = (r << 16) | (g << 8) | b;
        }
    }

    // ---- text -------------------------------------------------------------

    /// Text width in pixels.
    pub fn tw(&self, s: &str) -> usize {
        s.chars().count() * self.tk.cw
    }

    /// How many characters fit in `px` pixels.
    pub fn cols(&self, px: usize) -> usize {
        px / self.tk.cw.max(1)
    }

    pub fn text(&mut self, x: usize, y: usize, s: &str, c: Rgb) {
        crate::ui::render_text(self.buf, self.w, self.font, s, x, y, c);
    }

    /// Text truncated with "…" to fit `max_w` pixels. Returns pixels drawn.
    pub fn text_fit(&mut self, x: usize, y: usize, max_w: usize, s: &str, c: Rgb) -> usize {
        let t = ellipsize(s, self.cols(max_w));
        self.text(x, y, &t, c);
        self.tw(&t)
    }

    /// Right-aligned text ending at `right` (exclusive).
    pub fn text_right(&mut self, right: usize, y: usize, s: &str, c: Rgb) {
        let w = self.tw(s);
        self.text(right.saturating_sub(w), y, s, c);
    }

    /// Y that vertically centres a text line inside `[y, y+h)`.
    pub fn text_y(&self, y: usize, h: usize) -> usize {
        y + h.saturating_sub(self.tk.ch) / 2
    }

    /// Text centred horizontally in `[x, x+w)`, truncated to fit.
    pub fn text_center(&mut self, x: usize, y: usize, w: usize, s: &str, c: Rgb) {
        let t = ellipsize(s, self.cols(w));
        let tw = self.tw(&t);
        self.text(x + w.saturating_sub(tw) / 2, y, &t, c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ellipsize_basic() {
        assert_eq!(ellipsize("hello", 10), "hello");
        assert_eq!(ellipsize("hello world", 8), "hello w…");
        assert_eq!(ellipsize("hello", 1), "…");
        assert_eq!(ellipsize("hello", 0), "");
        assert_eq!(ellipsize("héllo wörld", 5).chars().count(), 5);
    }

    #[test]
    fn spark_heights_resample_and_clamp() {
        assert_eq!(spark_heights(&[], 4, 8, 100.0), vec![0; 4]);
        assert_eq!(spark_heights(&[0.0, 100.0], 3, 8, 100.0), vec![0, 4, 8]);
        assert_eq!(spark_heights(&[250.0], 2, 8, 100.0), vec![8, 8]);
        assert_eq!(spark_heights(&[50.0], 0, 8, 100.0), Vec::<usize>::new());
    }

    #[test]
    fn blend_extremes() {
        assert_eq!(blend_px(0x102030, (255, 0, 0), 0), 0x102030);
        assert_eq!(blend_px(0x102030, (255, 0, 0), 255), 0xff0000);
        let m = blend_px(0x000000, (200, 100, 50), 128);
        assert!(((m >> 16) & 0xff) > 90 && ((m >> 16) & 0xff) < 110);
    }

    #[test]
    fn sdf_inside_outside() {
        let r = Rect::new(0, 0, 40, 40);
        assert!(rr_sdf(20.0, 20.0, &r, 8.0) < 0.0);
        assert!(rr_sdf(0.5, 0.5, &r, 8.0) > 0.0); // clipped corner pixel
        assert!(rr_sdf(20.0, 0.5, &r, 8.0) < 0.0); // straight edge pixel
    }
}
