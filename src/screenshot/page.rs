//! Stand-in for the page area of the built-in browser. The real page is a
//! native webview (wry) which cannot be captured offscreen, so the
//! screenshot draws a locally generated docs page for the demo project
//! ("Aurora") with the UI kit and a system sans font. It is a placeholder,
//! not a render of any real website.

use super::frame::PropFont;
use crate::config::{Rgb, Theme};
use crate::network::browser::chrome::BrowserLayout;
use crate::renderer::font::FontManager;
use crate::ui::kit::{Ctx, Rect, Tokens};

const PAPER: Rgb = (249, 250, 252);
const WHITE: Rgb = (255, 255, 255);
const RULE: Rgb = (226, 229, 237);
const INK: Rgb = (28, 33, 48);
const BODY: Rgb = (58, 66, 86);
const MUTED: Rgb = (110, 118, 138);
const CODE_BG: Rgb = (21, 25, 40);
const CODE_FG: Rgb = (214, 222, 245);

/// Greedy word wrap by measured width.
fn wrap(p: &PropFont, text: &str, size: f32, max_w: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if p.width(&cand, size, false) > max_w && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        } else {
            cur = cand;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

pub fn draw_docs_page(buf: &mut [u32], w: usize, h: usize, layout: &BrowserLayout, font: &mut FontManager, theme: &Theme) {
    let r = layout.web;
    if r.w < 200 || r.h < 200 {
        return;
    }
    let tk = Tokens::new(theme, font.cell_width, font.cell_height);
    let accent: Rgb = (46, 98, 235);
    let prop = PropFont::load();
    let mut cx = Ctx::new(buf, w, h, font, &tk);
    let s = (r.w as f32 / 750.0).clamp(0.7, 1.6); // layout scale relative to a 750px page

    cx.fill(Rect::new(r.x, r.y, r.w, r.h), PAPER);

    // ── Header ──
    let hdr_h = (58.0 * s) as usize;
    cx.fill(Rect::new(r.x, r.y, r.w, hdr_h), WHITE);
    cx.hline(r.x, r.y + hdr_h - 1, r.w, RULE);
    let logo = (26.0 * s) as usize;
    let lx = r.x + (22.0 * s) as usize;
    let ly = r.y + (hdr_h - logo) / 2;
    cx.fill_rrect(Rect::new(lx, ly, logo, logo), (7.0 * s) as usize, accent);
    cx.fill_rrect(Rect::new(lx + logo / 4, ly + logo / 4, logo / 2, logo / 2), (4.0 * s) as usize, (150, 190, 255));

    let Some(p) = prop else {
        // No system sans: keep the page honest and minimal.
        cx.text(lx + logo + 12, ly, "Aurora docs", INK);
        return;
    };
    let draw = |cx: &mut Ctx, x: f32, y: f32, t: &str, size: f32, bold: bool, c: Rgb| -> f32 {
        let clip = r.x + r.w;
        p.draw(&mut *cx.buf, w, x, y, t, size, bold, c, clip)
    };
    let tsz = 22.0 * s;
    draw(&mut cx, (lx + logo) as f32 + 12.0 * s, r.y as f32 + (hdr_h as f32 - tsz) / 2.0 - 2.0, "Aurora", tsz, true, INK);
    // Nav links, right aligned.
    let mut nx = (r.x + r.w) as f32 - 22.0 * s;
    for item in ["Changelog", "API", "Guide"] {
        let nsz = 17.0 * s;
        let wd = p.width(item, nsz, false);
        nx -= wd;
        draw(&mut cx, nx, r.y as f32 + (hdr_h as f32 - nsz) / 2.0 - 1.0, item, nsz, false, MUTED);
        nx -= 24.0 * s;
    }

    // ── Sidebar ──
    let side_w = (r.w as f32 * 0.27).max(150.0 * s) as usize;
    let body_y = r.y + hdr_h;
    cx.fill(Rect::new(r.x, body_y, side_w, r.h - hdr_h), (244, 246, 250));
    cx.vline(r.x + side_w - 1, body_y, r.h - hdr_h, RULE);
    let mut sy = body_y as f32 + 22.0 * s;
    let groups: [(&str, &[&str]); 3] = [
        ("Getting started", &["Installation", "Project layout"]),
        ("Middleware", &["Authentication", "Rate limiting", "CORS", "Tracing"]),
        ("Deployment", &["Docker", "Health checks"]),
    ];
    for (g, items) in groups {
        draw(&mut cx, r.x as f32 + 22.0 * s, sy, g, 15.0 * s, true, INK);
        sy += 28.0 * s;
        for it in items {
            let active = *it == "Rate limiting";
            if active {
                let row = Rect::new(r.x + (10.0 * s) as usize, sy as usize - (4.0 * s) as usize, side_w - (22.0 * s) as usize, (28.0 * s) as usize);
                cx.fill_rrect(row, (6.0 * s) as usize, (226, 235, 254));
                cx.fill_rrect(Rect::new(row.x, row.y + row.h / 4, (3.0 * s).max(2.0) as usize, row.h / 2), 2, accent);
            }
            draw(&mut cx, r.x as f32 + 28.0 * s, sy, it, 16.0 * s, active, if active { accent } else { BODY });
            sy += 28.0 * s;
        }
        sy += 12.0 * s;
    }

    // ── Article ──
    let ax = (r.x + side_w) as f32 + 34.0 * s;
    let aw = (r.x + r.w) as f32 - ax - 28.0 * s;
    let bottom = (r.y + r.h) as f32 - 8.0;
    let mut y = body_y as f32 + 26.0 * s;
    draw(&mut cx, ax, y, "Middleware  /  Rate limiting", 14.0 * s, false, MUTED);
    y += 30.0 * s;
    draw(&mut cx, ax, y, "Rate limiting", 40.0 * s, true, INK);
    y += 62.0 * s;

    let para = |cx: &mut Ctx, y: &mut f32, text: &str| {
        let sz = 18.5 * s;
        for l in wrap(&p, text, sz, aw) {
            if *y + sz > bottom {
                return;
            }
            draw(cx, ax, *y, &l, sz, false, BODY);
            *y += sz * 1.55;
        }
        *y += 10.0 * s;
    };
    para(&mut cx, &mut y,
        "Aurora ships a token-bucket limiter as a tower layer. It keeps one bucket per client IP and answers 429 Too Many Requests once a bucket is empty.");

    draw(&mut cx, ax, y, "Install", 26.0 * s, true, INK);
    y += 42.0 * s;
    para(&mut cx, &mut y, "The limiter is built on the governor crate. Add it to your project:");

    // Code block 1 (shell).
    let cw = cx.tk.cw as f32;
    let line_h = cx.tk.ch as f32;
    let block = |cx: &mut Ctx, y: &mut f32, lines: &[(&str, Rgb)]| {
        let pad = 14.0 * s;
        let bh = lines.len() as f32 * line_h + 2.0 * pad;
        if *y + bh > bottom {
            return;
        }
        let rect = Rect::new(ax as usize, *y as usize, aw as usize, bh as usize);
        cx.fill_rrect(rect, (8.0 * s) as usize, CODE_BG);
        let max_cols = ((aw - 2.0 * pad) / cw) as usize;
        for (i, (t, c)) in lines.iter().enumerate() {
            let shown: String = t.chars().take(max_cols).collect();
            cx.text((ax + pad) as usize, (*y + pad + i as f32 * line_h) as usize, &shown, *c);
        }
        *y += bh + 16.0 * s;
    };
    block(&mut cx, &mut y, &[("$ cargo add governor", CODE_FG)]);

    draw(&mut cx, ax, y, "Usage", 26.0 * s, true, INK);
    y += 42.0 * s;
    para(&mut cx, &mut y, "Wrap any route group with the layer. Limits are expressed per second:");
    let kw: Rgb = (255, 125, 175);
    let ty: Rgb = (120, 200, 255);
    let st: Rgb = (140, 230, 170);
    block(&mut cx, &mut y, &[
        ("let app = Router::new()", kw),
        ("    .route(\"/v1/orders\", get(orders))", CODE_FG),
        ("    .layer(rate_limit(50));", ty),
    ]);
    let _ = st;

    // Note callout.
    let note_h = 64.0 * s;
    if y + note_h < bottom {
        let rect = Rect::new(ax as usize, y as usize, aw as usize, note_h as usize);
        cx.fill_rrect(rect, (8.0 * s) as usize, (255, 247, 224));
        cx.fill(Rect::new(rect.x, rect.y + 6, (4.0 * s).max(3.0) as usize, rect.h - 12), (240, 180, 40));
        let nsz = 16.5 * s;
        draw(&mut cx, ax + 20.0 * s, y + 12.0 * s, "Note", nsz, true, (130, 90, 10));
        draw(&mut cx, ax + 20.0 * s, y + 12.0 * s + nsz * 1.45, "Limits are per process. Share a store across replicas.", nsz, false, (110, 84, 20));
    }
}
