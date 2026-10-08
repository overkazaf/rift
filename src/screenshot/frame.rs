//! Marketing frame around a rendered app buffer: gradient backdrop, soft
//! shadow, rounded macOS-style window with traffic lights and a title.
//! This is screenshot-only chrome; the app itself never draws any of it.

use crate::config::{Rgb, Theme};
use crate::ui::kit::mix;

/// Geometry of the framed window inside the output image (all physical px).
#[derive(Clone, Copy, Debug)]
pub struct FrameGeom {
    pub img_w: usize,
    pub img_h: usize,
    pub win_x: usize,
    pub win_y: usize,
    pub win_w: usize,
    pub win_h: usize,
    pub title_h: usize,
    pub radius: usize,
}

impl FrameGeom {
    pub fn new(img_w: usize, img_h: usize) -> Self {
        // Image-relative margins so the window always sits nicely, with a
        // little more room below for the shadow.
        let mx = (img_w as f32 * 0.03).round() as usize;
        let top = (img_h as f32 * 0.04).round() as usize;
        let bottom = (img_h as f32 * 0.056).round() as usize;
        let win_w = img_w - 2 * mx;
        let win_h = img_h - top - bottom;
        let title_h = ((img_h as f32) * 0.056).round() as usize;
        let radius = ((img_h as f32) * 0.02).round() as usize;
        Self { img_w, img_h, win_x: mx, win_y: top, win_w, win_h, title_h, radius }
    }

    /// Size of the app buffer that fills the window below the title bar.
    pub fn app_size(&self) -> (usize, usize) {
        (self.win_w, self.win_h - self.title_h)
    }
}

/// A proportional UI font (system sans) for the title bar and web-page
/// placeholders. `None` when no suitable system font exists.
pub struct PropFont {
    regular: fontdue::Font,
    bold: Option<fontdue::Font>,
}

impl PropFont {
    pub fn load() -> Option<Self> {
        let candidates: [(&str, u32, u32); 3] = [
            ("/System/Library/Fonts/HelveticaNeue.ttc", 0, 1),
            ("/System/Library/Fonts/Helvetica.ttc", 0, 1),
            ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 0, 0),
        ];
        for (path, reg, bold) in candidates {
            let Ok(data) = std::fs::read(path) else { continue };
            let load = |idx: u32| {
                fontdue::Font::from_bytes(
                    data.as_slice(),
                    fontdue::FontSettings { collection_index: idx, ..Default::default() },
                )
                .ok()
            };
            if let Some(regular) = load(reg) {
                let bold = if bold != reg { load(bold) } else { None };
                return Some(Self { regular, bold });
            }
        }
        None
    }

    fn face(&self, bold: bool) -> &fontdue::Font {
        if bold { self.bold.as_ref().unwrap_or(&self.regular) } else { &self.regular }
    }

    /// Advance width of `text` at `size` px.
    pub fn width(&self, text: &str, size: f32, bold: bool) -> f32 {
        let f = self.face(bold);
        text.chars().map(|c| f.metrics(c, size).advance_width).sum()
    }

    /// Ascent / line height helpers: (ascent, descent) in px at `size`.
    pub fn ascent(&self, size: f32, bold: bool) -> f32 {
        self.face(bold).horizontal_line_metrics(size).map_or(size * 0.8, |m| m.ascent)
    }

    /// Draw `text` with its top (ascent line) at `y`, left at `x`. Returns
    /// the advance. Clipped to the buffer and to `clip_x1`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        buf: &mut [u32],
        bw: usize,
        x: f32,
        y: f32,
        text: &str,
        size: f32,
        bold: bool,
        color: Rgb,
        clip_x1: usize,
    ) -> f32 {
        let f = self.face(bold);
        let bh = buf.len() / bw.max(1);
        let base = y + self.ascent(size, bold);
        let mut pen = x;
        for c in text.chars() {
            let (m, bmp) = f.rasterize(c, size);
            let gx = (pen + m.xmin as f32).round() as isize;
            let gy = (base - m.height as f32 - m.ymin as f32).round() as isize;
            for row in 0..m.height {
                let py = gy + row as isize;
                if py < 0 || py as usize >= bh {
                    continue;
                }
                for col in 0..m.width {
                    let px = gx + col as isize;
                    if px < 0 || px as usize >= bw || px as usize >= clip_x1 {
                        continue;
                    }
                    let cov = bmp[row * m.width + col] as u32;
                    if cov == 0 {
                        continue;
                    }
                    let i = py as usize * bw + px as usize;
                    buf[i] = blend_px(buf[i], color, cov);
                }
            }
            pen += m.advance_width;
        }
        pen - x
    }
}

#[inline]
pub fn blend_px(dst: u32, c: Rgb, a: u32) -> u32 {
    let inv = 255 - a;
    let r = ((dst >> 16) & 0xff) * inv / 255 + c.0 as u32 * a / 255;
    let g = ((dst >> 8) & 0xff) * inv / 255 + c.1 as u32 * a / 255;
    let b = (dst & 0xff) * inv / 255 + c.2 as u32 * a / 255;
    (r.min(255) << 16) | (g.min(255) << 8) | b.min(255)
}

fn pack(c: Rgb) -> u32 {
    ((c.0 as u32) << 16) | ((c.1 as u32) << 8) | c.2 as u32
}

/// Signed distance from (px, py) to a rounded rect (negative inside).
fn sdf_rrect(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = (px - cx).abs() - (hw - r);
    let qy = (py - cy).abs() - (hh - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

fn hash01(x: usize, y: usize) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA6B);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h & 0xffff) as f32 / 65535.0
}

/// Gradient backdrop tinted by the theme's accent colours.
fn backdrop(theme: &Theme, w: usize, h: usize) -> Vec<[f32; 3]> {
    let base: Rgb = (6, 7, 14);
    let a = mix(base, theme.accent(), 0.30);
    let b = mix(base, theme.palette[6], 0.26);
    let c = mix(base, theme.palette[4], 0.20);
    let f = |c: Rgb| [c.0 as f32, c.1 as f32, c.2 as f32];
    let (a, b, c) = (f(a), f(b), f(c));
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let u = x as f32 / w as f32;
            let v = y as f32 / h as f32;
            // Two soft colour pools (top-left accent, bottom-right cyan) over a dark base.
            let d1 = ((u - 0.05).powi(2) * 1.4 + (v + 0.05).powi(2)).sqrt();
            let d2 = ((u - 0.98).powi(2) * 1.4 + (v - 1.05).powi(2)).sqrt();
            let d3 = ((u - 0.6).powi(2) * 1.4 + (v - 0.0).powi(2)).sqrt();
            let w1 = (1.0 - d1 / 0.95).clamp(0.0, 1.0).powi(2);
            let w2 = (1.0 - d2 / 0.95).clamp(0.0, 1.0).powi(2);
            let w3 = (1.0 - d3 / 0.7).clamp(0.0, 1.0).powi(2) * 0.6;
            let n = (hash01(x, y) - 0.5) * 1.6; // dither against banding
            let mut px = [base.0 as f32, base.1 as f32, base.2 as f32];
            for k in 0..3 {
                px[k] = (px[k] * (1.0 - (w1 + w2 + w3).min(1.0)) + a[k] * w1 + b[k] * w2 + c[k] * w3 + n).clamp(0.0, 255.0);
            }
            out.push(px);
        }
    }
    out
}

/// Compose the final image. `app` is the `app_size()` buffer (0x00RRGGBB);
/// returns tightly packed RGBA8.
pub fn compose(
    geom: &FrameGeom,
    theme: &Theme,
    app: &[u32],
    title: &str,
    prop: Option<&PropFont>,
    mono_title: &mut dyn FnMut(&mut [u32], usize, &str, usize, usize, Rgb),
) -> Vec<u8> {
    let (iw, ih) = (geom.img_w, geom.img_h);
    let (aw, ah) = geom.app_size();
    assert_eq!(app.len(), aw * ah, "app buffer size mismatch");

    let mut bg = backdrop(theme, iw, ih);
    let (wx, wy, ww, wh) = (geom.win_x as f32, geom.win_y as f32, geom.win_w as f32, geom.win_h as f32);
    let (cx, cy, hw, hh) = (wx + ww / 2.0, wy + wh / 2.0, ww / 2.0, wh / 2.0);
    let r = geom.radius as f32;
    let scale = ih as f32 / 1000.0;

    // Shadows: wide soft + tight contact.
    let layers = [(60.0 * scale, 34.0 * scale, 0.62f32), (14.0 * scale, 8.0 * scale, 0.45f32)];
    for y in 0..ih {
        for x in 0..iw {
            let mut keep = 1.0f32;
            for (sigma, off, strength) in layers {
                let d = sdf_rrect(x as f32 + 0.5, y as f32 + 0.5 - off, cx, cy, hw, hh, r);
                let a = if d <= 0.0 { strength } else { strength * (-(d * d) / (2.0 * sigma * sigma)).exp() };
                keep *= 1.0 - a;
            }
            let px = &mut bg[y * iw + x];
            for k in 0..3 {
                px[k] *= keep;
            }
        }
    }

    // Window surface (title bar + app) as an opaque buffer of the window size.
    let (win_w, win_h, th) = (geom.win_w, geom.win_h, geom.title_h);
    let title_bg = crate::ui::darken(theme.bg, 34);
    let mut win = vec![pack(title_bg); win_w * win_h];
    for y in 0..ah {
        let dst = (th + y) * win_w;
        win[dst..dst + aw].copy_from_slice(&app[y * aw..(y + 1) * aw]);
    }
    // Hairline under the title bar.
    let line = pack(crate::ui::lighten(title_bg, 10));
    win[(th - 1) * win_w..th * win_w].fill(line);

    // Traffic lights.
    let dia = (th as f32 * 0.43).round();
    let lights: [(Rgb, Rgb); 3] = [
        ((255, 95, 87), (224, 68, 62)),
        ((254, 188, 46), (222, 161, 35)),
        ((40, 200, 64), (26, 171, 41)),
    ];
    for (i, (fill, ring)) in lights.iter().enumerate() {
        let lx = th as f32 * 0.62 + i as f32 * dia * 1.62;
        let ly = (th as f32 - 1.0) / 2.0;
        let rad = dia / 2.0;
        let (x0, x1) = ((lx - rad - 2.0).floor().max(0.0) as usize, (lx + rad + 2.0).ceil() as usize);
        let (y0, y1) = ((ly - rad - 2.0).floor().max(0.0) as usize, (ly + rad + 2.0).ceil() as usize);
        for y in y0..y1.min(th) {
            for x in x0..x1.min(win_w) {
                let d = ((x as f32 + 0.5 - lx).powi(2) + (y as f32 + 0.5 - ly).powi(2)).sqrt() - rad;
                let cov = (0.5 - d).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                // Darker rim, brighter body (top-lit).
                let t = ((y as f32 - (ly - rad)) / (2.0 * rad)).clamp(0.0, 1.0);
                let body = mix(*fill, (255, 255, 255), 0.12 * (1.0 - t));
                let c = if d > -(dia * 0.07).max(1.0) { *ring } else { body };
                let i2 = y * win_w + x;
                win[i2] = blend_px(win[i2], c, (cov * 255.0) as u32);
            }
        }
    }

    // Title text, centred in the bar.
    let fg = crate::ui::dim(theme.fg, 0.62);
    match prop {
        Some(p) => {
            let size = th as f32 * 0.42;
            let tw = p.width(title, size, false);
            let x = (win_w as f32 - tw) / 2.0;
            let y = (th as f32 - size) / 2.0 - size * 0.06;
            p.draw(&mut win, win_w, x, y, title, size, true, fg, win_w);
        }
        None => mono_title(&mut win, win_w, title, th, win_w, fg),
    }

    // Composite window over the shadowed backdrop with an antialiased
    // rounded mask and a faint light rim.
    let mut out = vec![0u8; iw * ih * 4];
    for y in 0..ih {
        for x in 0..iw {
            let bgp = bg[y * iw + x];
            let mut rgb = bgp;
            let d = sdf_rrect(x as f32 + 0.5, y as f32 + 0.5, cx, cy, hw, hh, r);
            if d < 0.5 {
                let cov = (0.5 - d).clamp(0.0, 1.0);
                let lx = x.saturating_sub(geom.win_x).min(win_w - 1);
                let ly = y.saturating_sub(geom.win_y).min(win_h - 1);
                let wp = win[ly * win_w + lx];
                let mut src = [((wp >> 16) & 0xff) as f32, ((wp >> 8) & 0xff) as f32, (wp & 0xff) as f32];
                // 1px rim: brighter along the top edge.
                let rim = (1.0 - (-d / 1.5)).clamp(0.0, 1.0) * (d > -1.5) as u8 as f32;
                if rim > 0.0 {
                    let top_bias = 1.0 - ((y as f32 - wy) / wh).clamp(0.0, 1.0);
                    let a = rim * (0.10 + 0.14 * top_bias);
                    for s in src.iter_mut() {
                        *s = *s * (1.0 - a) + 255.0 * a;
                    }
                }
                for k in 0..3 {
                    rgb[k] = bgp[k] * (1.0 - cov) + src[k] * cov;
                }
            }
            let o = (y * iw + x) * 4;
            out[o] = rgb[0].round().clamp(0.0, 255.0) as u8;
            out[o + 1] = rgb[1].round().clamp(0.0, 255.0) as u8;
            out[o + 2] = rgb[2].round().clamp(0.0, 255.0) as u8;
            out[o + 3] = 255;
        }
    }
    out
}
