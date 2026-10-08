//! Overlay painting for command blocks. Everything is drawn on the *output*
//! buffer after the damage-tracked pane render, recomputed each frame from
//! the current view and block state (never cached in the back buffer).

use super::view::{self, Toolbar, ViewRow};
use super::pane_geom;
use crate::renderer::Renderer;
use crate::window::WindowManager;
use super::BlocksUi;
use crate::config::Rgb;
use crate::ui::{dim, lighten, pack_rgb, render_text};
use crate::window::PaneRect;

const FAIL: Rgb = (230, 70, 70);
const GUTTER_W: usize = 3;

/// Per-span facts gathered before painting (so the wm borrow ends before we
/// need `&mut` access to the font).
struct SpanPaint {
    first: usize,
    last: usize,
    failed: bool,
    running: bool,
    selected: bool,
    hovered: bool,
    /// Last view row that belongs to the prompt/command (brighter bar).
    cmd_last: Option<usize>,
    /// First row is the block's first line (draw a separator above it).
    starts_here: bool,
    /// Row holding the command line + cells' last used column.
    chip_row: Option<(usize, usize)>,
    chip: (String, String, &'static str),
}

fn blend(base: u32, c: Rgb, alpha: u32) -> u32 {
    let inv = 255 - alpha;
    let r = ((base >> 16) & 0xff) * inv / 255 + c.0 as u32 * alpha / 255;
    let g = ((base >> 8) & 0xff) * inv / 255 + c.1 as u32 * alpha / 255;
    let b = (base & 0xff) * inv / 255 + c.2 as u32 * alpha / 255;
    (r.min(255) << 16) | (g.min(255) << 8) | b.min(255)
}

fn tint(buf: &mut [u32], w: usize, x0: usize, x1: usize, y0: usize, y1: usize, c: Rgb, alpha: u32) {
    for y in y0..y1 {
        let o = y * w;
        if o + x1 > buf.len() {
            break;
        }
        for px in &mut buf[o + x0..o + x1] {
            *px = blend(*px, c, alpha);
        }
    }
}

fn fill(buf: &mut [u32], w: usize, x0: usize, x1: usize, y0: usize, y1: usize, px: u32) {
    for y in y0..y1 {
        let o = y * w;
        if o + x1 > buf.len() {
            break;
        }
        buf[o + x0..o + x1].fill(px);
    }
}

pub fn draw(
    wm: &WindowManager,
    renderer: &mut Renderer,
    ui: &BlocksUi,
    buffer: &mut [u32],
    width: u32,
    height: u32,
    content_area: PaneRect,
) {
    let w = width as usize;
    let h = height as usize;
    let cw = renderer.cell_width();
    let ch = renderer.cell_height();
    let theme_fg = renderer.theme.fg;
    let theme_bg = renderer.theme.bg;
    let accent = renderer.theme.cursor;
    let t_secs = renderer.start_time.elapsed().as_secs_f32();
    let active_tab = wm.active_tab;
    let layouts = wm.pane_layouts(content_area);
    let hover = ui.hover.filter(|hv| hv.tab == active_tab);
    let selected = ui.selected.filter(|s| s.0 == active_tab);

    // Gather (pane rect, spans, toolbar) first.
    struct PanePlan {
        rect: PaneRect,
        spans: Vec<SpanPaint>,
        toolbar: Option<(Toolbar, Option<view::Button>, usize)>,
    }
    let mut plans: Vec<PanePlan> = Vec::new();
    for (idx, rect, _active) in &layouts {
        let Some(pane) = wm.active_tab().pane(*idx) else { continue };
        let t = &pane.terminal;
        let Some(geom) = pane_geom(t) else { continue };
        let mut spans = Vec::new();
        let mut toolbar = None;
        for s in &geom.spans {
            let Some(b) = t.blocks.get(s.block) else { continue };
            let failed = !b.running && b.exit_code.map_or(false, |c| c != 0);
            let min_line = b.prompt_line.min(b.output_start);
            let starts_here = matches!(geom.view[s.first], ViewRow::Line(l) if l == min_line);
            let cmd_last = (s.first..=s.last)
                .rev()
                .find(|&r| matches!(geom.view[r], ViewRow::Line(l) if l < b.output_start));
            let chip_row = view::row_of_line(&geom.view, b.command_line)
                .filter(|r| (s.first..=s.last).contains(r) && matches!(geom.view[*r], ViewRow::Line(_)))
                .map(|r| {
                    let line = geom.view[r].abs();
                    let sb = t.scrollback.len();
                    let cells = if line < sb { t.scrollback.get(line) } else { t.grid.get(line - sb) };
                    let used = cells
                        .and_then(|c| c.iter().rposition(|c| c.c != ' ' && c.c != '\0'))
                        .map_or(0, |i| i + 1);
                    (r, used)
                });
            let dur = if b.running { t.blocks.running_osc_elapsed_ms().unwrap_or(0) } else { b.duration_ms };
            let (badge, d) = view::chip_parts(b.running, b.exit_code, dur);
            let sep = view::chip_sep(b.running, b.exit_code);
            let is_hover = hover.map_or(false, |hv| hv.pane == *idx && hv.block == s.block);
            if is_hover {
                let tb = view::toolbar_layout(
                    rect.x, rect.width, rect.y + s.first * ch, cw, ch, b.running, b.collapsed,
                );
                toolbar = Some((tb, hover.and_then(|hv| hv.button), s.first));
            }
            spans.push(SpanPaint {
                first: s.first,
                last: s.last,
                failed,
                running: b.running,
                selected: selected.map_or(false, |sel| sel.1 == *idx && sel.2 == s.block),
                hovered: is_hover,
                cmd_last,
                starts_here,
                chip_row,
                chip: (badge, d, sep),
            });
        }
        plans.push(PanePlan { rect: *rect, spans, toolbar });
    }

    let pulse = 0.5 + 0.5 * (t_secs * std::f32::consts::TAU / 1.4).sin();
    let sep_c = theme_fg;
    let ok_c = accent;
    let chip_bg = lighten(theme_bg, 14);
    let chip_bg_px = pack_rgb(chip_bg);
    let dim_text: Rgb = (140, 144, 160);

    for plan in &plans {
        let r = plan.rect;
        let x_end = (r.x + r.width).min(w);
        let y_max = (r.y + r.height).min(h);
        for sp in &plan.spans {
            let y0 = r.y + sp.first * ch;
            let y1 = (r.y + (sp.last + 1) * ch).min(y_max);
            if y0 >= y1 {
                continue;
            }
            // Failed blocks: faint red tint over the whole region.
            if sp.failed {
                tint(buffer, w, r.x, x_end, y0, y1, FAIL, 16);
            }
            if sp.selected {
                tint(buffer, w, r.x, x_end, y0, y1, accent, 30);
            } else if sp.hovered {
                tint(buffer, w, r.x, x_end, y0, y1, theme_fg, 6);
            }
            // Separator above the block.
            if sp.starts_here && sp.first > 0 {
                tint(buffer, w, r.x, x_end, y0, (y0 + 1).min(y_max), sep_c, 36);
            }
            // Gutter bar: bright on the command rows, dimmer on the output.
            let base = if sp.failed { FAIL } else { ok_c };
            let (bar_hi, bar_lo) = if sp.running {
                let f = 0.35 + 0.65 * pulse;
                (dim(base, f), dim(base, f * 0.6))
            } else {
                (base, dim(base, 0.5))
            };
            let bw = if sp.selected { GUTTER_W + 2 } else { GUTTER_W };
            for row in sp.first..=sp.last {
                let ry0 = r.y + row * ch;
                let ry1 = (ry0 + ch).min(y_max);
                if ry0 >= ry1 {
                    continue;
                }
                let is_cmd = sp.cmd_last.map_or(false, |c| row <= c);
                let px = pack_rgb(if is_cmd { bar_hi } else { bar_lo });
                fill(buffer, w, r.x, (r.x + bw).min(x_end), ry0, ry1, px);
            }
        }
    }

    // Header chips (after tints so they stay crisp), skipped under the toolbar
    // and when the command text would run underneath them.
    for plan in &plans {
        let r = plan.rect;
        for sp in &plan.spans {
            if sp.hovered {
                continue;
            }
            let Some((row, used)) = sp.chip_row else { continue };
            let (badge, dur, sep) = &sp.chip;
            let text_w = (badge.chars().count() + sep.chars().count() + dur.chars().count()) * cw;
            let pill_w = text_w + 12;
            let right = r.x + r.width.saturating_sub(super::view::EDGE_MARGIN);
            if pill_w + 8 * cw > right.saturating_sub(r.x) {
                continue;
            }
            let x = right - pill_w;
            if (used + 2) * cw + r.x > x {
                continue;
            }
            let y = r.y + row * ch;
            if y + ch > h {
                continue;
            }
            fill(buffer, w, x, (x + pill_w).min(w), y + 1, y + ch - 1, chip_bg_px);
            let badge_c = if sp.running {
                dim(ok_c, 0.35 + 0.65 * pulse)
            } else if sp.failed {
                FAIL
            } else {
                ok_c
            };
            render_text(buffer, w, &mut renderer.font, badge, x + 6, y, badge_c);
            let rest = format!("{sep}{dur}");
            render_text(buffer, w, &mut renderer.font, &rest, x + 6 + badge.chars().count() * cw, y, dim_text);
        }
    }

    // Hover toolbar.
    for plan in &plans {
        let Some((tb, hot, _first)) = &plan.toolbar else { continue };
        if tb.y + tb.h > h {
            continue;
        }
        let bar_bg = lighten(theme_bg, 24);
        let border = lighten(theme_bg, 56);
        fill(buffer, w, tb.x, (tb.x + tb.w).min(w), tb.y, tb.y + tb.h, pack_rgb(bar_bg));
        // 1px outline
        let (x1, y1) = ((tb.x + tb.w).min(w), tb.y + tb.h);
        fill(buffer, w, tb.x, x1, tb.y, tb.y + 1, pack_rgb(border));
        fill(buffer, w, tb.x, x1, y1 - 1, y1, pack_rgb(border));
        fill(buffer, w, tb.x, tb.x + 1, tb.y, y1, pack_rgb(border));
        fill(buffer, w, x1.saturating_sub(1), x1, tb.y, y1, pack_rgb(border));
        for (i, b) in tb.buttons.iter().enumerate() {
            let is_hot = Some(b.button) == *hot;
            if is_hot {
                tint(buffer, w, b.x0, b.x1.min(w), tb.y + 1, y1 - 1, accent, 70);
            }
            if i > 0 {
                fill(buffer, w, b.x0, b.x0 + 1, tb.y + 3, y1.saturating_sub(3), pack_rgb(border));
            }
            let col = if is_hot { accent } else { theme_fg };
            render_text(buffer, w, &mut renderer.font, b.label, b.x0 + cw, tb.y, col);
        }
    }

    // Toast, bottom-centre of the active pane.
    if let Some(msg) = ui.active_toast().map(str::to_owned) {
        let rect = layouts.iter().find(|(_, _, a)| *a).map(|(_, r, _)| *r).unwrap_or(content_area);
        draw_toast(renderer, buffer, w, h, rect, &msg, accent);
    }
}

fn draw_toast(renderer: &mut Renderer, buffer: &mut [u32], w: usize, h: usize, rect: PaneRect, msg: &str, accent: Rgb) {
    let cw = renderer.cell_width();
    let ch = renderer.cell_height();
    let text_w = msg.chars().count() * cw;
    let pw = text_w + 2 * cw;
    let ph = ch + 8;
    let x = rect.x + rect.width.saturating_sub(pw) / 2;
    let y = (rect.y + rect.height).saturating_sub(ph + ch * 2);
    if y + ph > h || x + pw > w {
        return;
    }
    let bg = lighten(renderer.theme.bg, 30);
    fill(buffer, w, x, x + pw, y, y + ph, pack_rgb(bg));
    let bpx = pack_rgb(dim(accent, 0.8));
    fill(buffer, w, x, x + pw, y, y + 1, bpx);
    fill(buffer, w, x, x + pw, y + ph - 1, y + ph, bpx);
    fill(buffer, w, x, x + 1, y, y + ph, bpx);
    fill(buffer, w, x + pw - 1, x + pw, y, y + ph, bpx);
    let fg = renderer.theme.fg;
    render_text(buffer, w, &mut renderer.font, msg, x + cw, y + 4, fg);
}
