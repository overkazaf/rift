//! Startup splash: a short RIFT logo reveal in the theme accent with a
//! scanline sweep and a glitch that settles, plus version / author / ko-fi
//! line at the bottom. Pure drawing; timing and skipping live in the app.

use super::kit::{mix, Ctx, Rect};
use crate::config::Rgb;
use crate::effects::hash_u32;

/// Total splash length in seconds (skippable earlier).
pub const SPLASH_SECS: f32 = 1.2;
/// The logo is fully revealed at this time; the rest is hold + fade.
const REVEAL_END: f32 = 0.65;
const FADE_START: f32 = 0.95;

/// 5x7 pixel font for the logo letters.
const GLYPHS: [(&str, [&str; 7]); 4] = [
    ("R", ["####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#"]),
    ("I", ["#####", "..#..", "..#..", "..#..", "..#..", "..#..", "#####"]),
    ("F", ["#####", "#....", "#....", "####.", "#....", "#....", "#...."]),
    ("T", ["#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#.."]),
];

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 0..1 progress of the left-to-right reveal.
pub fn reveal_frac(t: f32) -> f32 {
    smooth(t / REVEAL_END)
}

/// Overall opacity: 1.0 until the fade-out begins, 0.0 at the end.
pub fn opacity(t: f32) -> f32 {
    1.0 - smooth((t - FADE_START) / (SPLASH_SECS - FADE_START))
}

/// Horizontal glitch amplitude (in logo units); decays to zero by the end of the reveal.
pub fn glitch_amount(t: f32) -> f32 {
    (1.0 - t / REVEAL_END).clamp(0.0, 1.0)
}

/// Draw the splash for time `t` (seconds since startup). `secondary` is the
/// cool counter-colour used for the scan edge and chromatic fringe.
pub fn draw(cx: &mut Ctx, t: f32, secondary: Rgb, footer: &str) {
    let tk = *cx.tk;
    let (w, h) = (cx.w, cx.h);
    cx.fill(Rect::new(0, 0, w, h), tk.bg);

    // Logo geometry: letters are 5 units wide with a 1 unit gap.
    let unit = (w / 60).min(h / 16).max(3);
    let logo_w = (GLYPHS.len() * 6 - 1) * unit;
    let logo_h = 7 * unit;
    let x0 = w.saturating_sub(logo_w) / 2;
    let y0 = (h.saturating_sub(logo_h) / 2).saturating_sub(tk.ch);

    let reveal_px = (reveal_frac(t) * (logo_w + unit) as f32) as usize;
    let glitch = glitch_amount(t);
    let tick = (t * 30.0) as u32;

    for (li, (_, rows)) in GLYPHS.iter().enumerate() {
        for (ry, row) in rows.iter().enumerate() {
            // Per-row horizontal tear that settles as the reveal completes.
            let hsh = hash_u32(tick.wrapping_mul(131) ^ (ry as u32 * 7919 + 17));
            let torn = glitch > 0.0 && (hsh % 100) < 35;
            let dx = if torn { ((hsh >> 8) % 7) as i32 - 3 } else { 0 };
            let dx = (dx as f32 * glitch * unit as f32) as i32;
            for (rx, ch) in row.chars().enumerate() {
                if ch != '#' {
                    continue;
                }
                let lx = (li * 6 + rx) * unit;
                if lx >= reveal_px {
                    continue;
                }
                let px = (x0 + lx).saturating_add_signed(dx as isize);
                let py = y0 + ry * unit;
                let cell = Rect::new(px, py, unit - 1, unit - 1);
                if torn {
                    // chromatic fringe on torn rows
                    cx.fill_a(Rect::new(cell.x.saturating_sub(2), cell.y, cell.w, cell.h), secondary, 140);
                }
                // top of the logo glows brighter than the bottom
                let k = 0.75 + 0.25 * (1.0 - ry as f32 / 7.0);
                cx.fill(cell, mix(tk.bg, tk.accent, k));
            }
        }
    }

    // Bright scan edge while revealing.
    if t < REVEAL_END && reveal_px < logo_w + unit {
        let ex = x0 + reveal_px;
        cx.fill_a(Rect::new(ex, y0.saturating_sub(unit), 2, logo_h + 2 * unit), secondary, 230);
        cx.fill_a(Rect::new(ex.saturating_sub(unit), y0, unit, logo_h), secondary, 40);
    }

    // CRT scanlines across the logo block.
    let mut y = y0.saturating_sub(unit);
    while y < y0 + logo_h + unit {
        cx.fill_a(Rect::new(x0.saturating_sub(unit), y, logo_w + 2 * unit, 1), tk.bg, 60);
        y += 2;
    }

    // Thin accent rule under the logo, drawn with the reveal.
    let rule_y = y0 + logo_h + unit * 2;
    cx.hline(x0, rule_y, (logo_w as f32 * reveal_frac(t)) as usize, tk.accent);
    cx.fill_a(Rect::new(x0, rule_y + 1, (logo_w as f32 * reveal_frac(t)) as usize, 1), tk.accent, 60);

    // Footer: fades in with the reveal.
    if t > REVEAL_END * 0.6 {
        let a = smooth((t - REVEAL_END * 0.6) / 0.3);
        let col = mix(tk.bg, tk.text_muted, a);
        let fy = h.saturating_sub(tk.ch * 2);
        cx.text_center(0, fy, w, footer, col);
    }

    // Global fade-out.
    let o = opacity(t);
    if o < 1.0 {
        cx.fill_a(Rect::new(0, 0, w, h), tk.bg, ((1.0 - o) * 255.0) as u8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splash_is_short_and_fully_fades() {
        assert!(SPLASH_SECS <= 1.2);
        assert_eq!(opacity(0.0), 1.0);
        assert_eq!(opacity(SPLASH_SECS), 0.0);
        assert!(reveal_frac(0.0) == 0.0 && reveal_frac(REVEAL_END) == 1.0);
        assert_eq!(glitch_amount(REVEAL_END), 0.0);
        assert!(REVEAL_END < FADE_START && FADE_START < SPLASH_SECS);
    }

    #[test]
    fn glyphs_are_well_formed() {
        let name: String = GLYPHS.iter().map(|(n, _)| *n).collect();
        assert_eq!(name, "RIFT");
        for (n, rows) in GLYPHS {
            for r in rows {
                assert_eq!(r.len(), 5, "glyph {n}");
                assert!(r.chars().all(|c| c == '#' || c == '.'));
            }
        }
    }
}

#[cfg(test)]
mod qa_tests {
    use super::*;
    use crate::ui::kit::gallery::qa::each_theme;
    use crate::ui::kit::Tokens;

    #[test]
    fn splash_renders_in_every_theme_across_the_timeline() {
        for &t in &[SPLASH_SECS, 0.1f32, 0.4, 1.0, 0.8] {
            each_theme("splash", |b, w, h, f, th| {
                let tk = Tokens::new(th, f.cell_width, f.cell_height);
                let mut cx = Ctx::new(b, w, h, f, &tk);
                draw(&mut cx, t, th.palette[6], "v0 · by x · ko-fi.com/y");
            });
        }
    }
}
