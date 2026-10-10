//! Pixels for the tutorial list and the player (UI kit throughout).
//!
//! The demo stage is composed by the same code paths as a real frame
//! (`Renderer::render_tabbed_with_cmd`, `blocks_ui::draw`, `ai::inline::draw`,
//! the real command palette and Preview-Then-Accept modal) into an offscreen
//! buffer sized for the demo, then framed with a caption card, key overlay
//! and transport bar.

use super::cast::{Panel, Place};
use super::keys::{self, Seg};
use super::player::Player;
use super::DemoInfo;
use crate::app::keymap::Keymap;
use crate::config::{Rgb, Theme};
use crate::renderer::font::FontManager;
use crate::renderer::{Renderer, SplitUiState};
use crate::ui::kit::{Ctx, ListItem, PanelSpec, Rect, Tokens, Tone};

pub fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

// ───────────────────────────── list ─────────────────────────────

/// Hints of the tutorial list footer.
pub const PICKER_HINTS: &[(&str, &str)] = &[("Up/Down", "select"), ("Enter", "play"), ("Esc", "close")];

pub fn draw_picker(selected: usize, demos: &[(&DemoInfo, f64)], buf: &mut [u32], w: usize, h: usize, font: &mut FontManager, theme: &Theme) {
    let tk = Tokens::new(theme, font.cell_width, font.cell_height);
    let mut cx = Ctx::new(buf, w, h, font, &tk);
    cx.backdrop(0.6);
    let want = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + (demos.len() + 3) * tk.row_h + tk.sp.md;
    let rect = cx.centered_cols(70, want);
    if rect.w < 120 || rect.h < 80 {
        return;
    }
    let spec = PanelSpec::new("Tutorials").sub("watch a feature, then try it").badge("SAMPLE SESSIONS", Tone::Neutral).hints(PICKER_HINTS);
    let body = cx.panel(rect, &spec);
    let metas: Vec<String> = demos.iter().map(|(_, d)| fmt_time(*d)).collect();
    let labels: Vec<String> = demos.iter().enumerate().map(|(i, (d, _))| format!("{}  {}", i + 1, d.title)).collect();
    let items: Vec<ListItem> = labels.iter().zip(&metas).map(|(l, m)| ListItem::new(l).meta(m)).collect();
    let list_h = body.h.saturating_sub(3 * tk.row_h);
    let list_r = Rect::new(body.x, body.y, body.w, list_h);
    let vis = cx.rows_fit(list_h).max(1);
    let scroll = crate::ui::kit::scroll_into_view(selected, 0, vis);
    cx.list(list_r, &items, Some(selected), scroll, None);
    let y = body.y + list_h + tk.sp.xs;
    if let Some((d, _)) = demos.get(selected) {
        cx.line_fit(body.x, y, body.w, d.summary, tk.text);
    }
    cx.line_fit(body.x, y + tk.row_h, body.w, "Playback never touches your shell. Esc returns to your terminal.", tk.text_faint);
}

// ───────────────────────────── player ─────────────────────────────

/// Footer hints while playing / when finished.
pub const PLAY_HINTS: &[(&str, &str)] = &[
    ("Space", "pause"),
    ("Left/Right", "step"),
    ("Shift+Left/Right", "5s"),
    ("1/2/4", "speed"),
    ("R", "restart"),
    ("L", "list"),
    ("Esc", "exit"),
];
pub const DONE_HINTS: &[(&str, &str)] = &[("R", "replay"), ("N", "next demo"), ("Left", "back a step"), ("L", "list"), ("Esc", "exit")];

/// Compose the whole player frame into `buf` (`w * h`).
pub fn draw_player(p: &mut Player, buf: &mut [u32], w: usize, h: usize, renderer: &mut Renderer, km: &Keymap) {
    let (cw, ch) = (renderer.cell_width(), renderer.cell_height());
    p.set_cell((cw, ch));
    let theme = renderer.theme.clone();
    let tk = Tokens::new(&theme, cw, ch);
    let bg = crate::ui::pack(theme.bg.0, theme.bg.1, theme.bg.2);
    buf.fill(bg);

    // ---- layout
    let header_h = tk.row_h + 2 * tk.sp.sm;
    let transport_h = 2 * tk.row_h + 2 * tk.sp.sm;
    let caption_h = 2 * tk.row_h + 2 * tk.sp.md;
    let gap = tk.sp.md;
    let (sw, sh) = p.world().stage_size();
    let top = header_h + gap;
    let bottom_reserved = transport_h + caption_h + 2 * gap;
    let avail_h = h.saturating_sub(top + bottom_reserved);
    let sx = w.saturating_sub(sw) / 2;
    let sy = top + avail_h.saturating_sub(sh) / 2;

    // ---- stage (offscreen, real renderer)
    let mut stage = vec![bg; sw * sh];
    render_stage(p, &mut stage, sw, sh, renderer, km);
    // Shadow + frame, then blit (clipped to the window).
    {
        let mut cx = Ctx::new(buf, w, h, &mut renderer.font, &tk);
        let frame = Rect::new(sx.saturating_sub(1), sy.saturating_sub(1), (sw + 2).min(w), sh + 2);
        cx.shadow(frame, tk.radius_sm);
        cx.fill_rrect(frame, tk.radius_sm, tk.border_strong);
    }
    let vis_w = sw.min(w.saturating_sub(sx));
    let vis_h = sh.min(h.saturating_sub(sy));
    for row in 0..vis_h {
        let src = &stage[row * sw..row * sw + vis_w];
        let o = (sy + row) * w + sx;
        if o + vis_w <= buf.len() {
            buf[o..o + vis_w].copy_from_slice(src);
        }
    }

    let mut cx = Ctx::new(buf, w, h, &mut renderer.font, &tk);
    let stage_r = Rect::new(sx, sy, vis_w, vis_h);

    // ---- header
    let hy = tk.sp.sm;
    let mut x = tk.sp.lg;
    x += cx.badge(x, hy, "TUTORIAL", Tone::Accent, tk.row_h) + tk.sp.md;
    let (step, steps) = p.step_index();
    let right_txt = if steps > 0 { format!("Step {}/{}", step.max(1), steps) } else { String::new() };
    let sample = "SAMPLE \u{00b7} nothing runs";
    let bw = cx.badge_w(sample);
    let rw = cx.tw(&right_txt);
    let right = w.saturating_sub(tk.sp.lg);
    cx.badge(right.saturating_sub(bw), hy, sample, Tone::Warning, tk.row_h);
    let ty = cx.text_y(hy, tk.row_h);
    cx.text_right(right.saturating_sub(bw + tk.sp.md), ty, &right_txt, tk.text_muted);
    let title_max = right.saturating_sub(bw + rw + 2 * tk.sp.md + x);
    cx.text_fit(x, ty, title_max, &p.title, tk.text);

    // ---- key overlay (bottom-right of the stage)
    if let Some((k, label)) = p.key_overlay() {
        let key = keys::display(k, km);
        let label = keys::expand(label, km);
        let ow = cx.kbd_hint_w(&key, &label) + 2 * tk.sp.md;
        let oh = tk.row_h + 2 * tk.sp.sm;
        let ox = stage_r.right().saturating_sub(ow + tk.sp.lg).max(stage_r.x);
        let oy = stage_r.bottom().saturating_sub(oh + tk.sp.lg).max(stage_r.y);
        let inner = cx.float(Rect::new(ox, oy, ow, oh));
        cx.kbd_hint(inner.x + tk.sp.sm, inner.y + inner.h.saturating_sub(tk.row_h) / 2, &key, &label);
    }

    // ---- caption card
    let cap_y = (sy + vis_h + gap).min(h.saturating_sub(transport_h + caption_h + gap));
    let cap_w = sw.max(64 * cw).min(w.saturating_sub(2 * tk.sp.lg));
    let cap_x = w.saturating_sub(cap_w) / 2;
    let cap_r = Rect::new(cap_x, cap_y, cap_w, caption_h);
    let caption = p.caption().to_string();
    if !caption.is_empty() {
        cx.shadow(cap_r, tk.radius);
        cx.fill_rrect(cap_r, tk.radius, tk.border);
        cx.fill_rrect(cap_r.inset(1, 1), tk.radius.saturating_sub(1), tk.elevated);
        cx.fill(Rect::new(cap_r.x + 1, cap_r.y + tk.radius, 3 * tk.scale.max(1), cap_r.h.saturating_sub(2 * tk.radius)), tk.accent);
        let text_r = cap_r.inset(tk.sp.lg, tk.sp.md);
        draw_rich(&mut cx, text_r, &keys::segments(&caption, km));
    }

    // ---- transport
    let tr = Rect::new(tk.sp.lg, h.saturating_sub(transport_h), w.saturating_sub(2 * tk.sp.lg), transport_h);
    cx.hline(0, tr.y, w, tk.border);
    let row1 = tr.y + tk.sp.sm;
    let (state, tone) = if p.finished() {
        ("FINISHED", Tone::Success)
    } else if p.playing {
        ("PLAYING", Tone::Accent)
    } else {
        ("PAUSED", Tone::Warning)
    };
    let mut x = tr.x;
    x += cx.badge(x, row1, state, tone, tk.row_h) + tk.sp.md;
    let speed = format!("{}\u{00d7}", p.speed);
    x += cx.badge(x, row1, &speed, Tone::Neutral, tk.row_h) + tk.sp.md;
    let time = format!("{} / {}", fmt_time(p.pos()), fmt_time(p.duration()));
    let tw = cx.tw(&time);
    cx.text_right(tr.right(), cx.text_y(row1, tk.row_h), &time, tk.text_muted);
    let bar = Rect::new(x, row1, tr.right().saturating_sub(x + tw + tk.sp.md), tk.row_h);
    let dur = p.duration().max(0.001);
    cx.progress(bar, (p.pos() / dur) as f32, Tone::Accent);
    // Step ticks on the bar.
    for s in p.steps() {
        let tx = bar.x + ((s / dur) * bar.w as f64) as usize;
        cx.vline(tx.min(bar.right().saturating_sub(1)), bar.y + bar.h / 2 - (4 * tk.scale).min(bar.h / 2), 8 * tk.scale, tk.text_faint);
    }
    let hints = if p.finished() { DONE_HINTS } else { PLAY_HINTS };
    cx.hint_row(tr.x, row1 + tk.row_h, tr.w, tk.row_h, hints);
}

/// The demo itself, drawn like a real Rift window into `stage`.
fn render_stage(p: &mut Player, stage: &mut [u32], sw: usize, sh: usize, renderer: &mut Renderer, km: &Keymap) {
    let world = p.world_mut();
    let area = world.content_area();
    let split_ui = SplitUiState { zoomed: world.wm.active_tab().is_zoomed(), ..Default::default() };
    // The demo has its own panes: never reuse the live window's damage state,
    // and draw its text on the CPU (the stage is blitted, not GPU-composited).
    #[cfg(feature = "gpu")]
    let gpu_text = renderer.gpu_text_active();
    #[cfg(feature = "gpu")]
    if gpu_text {
        renderer.set_gpu_suspended(true);
    }
    renderer.invalidate_all();
    renderer.render_tabbed_with_cmd(&world.wm, area, stage, sw as u32, sh as u32, false, &world.blocks, split_ui);
    crate::blocks_ui::draw::draw(&world.wm, renderer, &world.blocks_ui, stage, sw as u32, sh as u32, area);
    crate::ai::inline::draw(&world.wm, renderer, &mut world.inline_ai, stage, sw, sh, area, "");
    if let Some(pal) = &world.palette {
        pal.render(stage, sw, sh, &mut renderer.font, &renderer.theme);
    }
    if let Some(pv) = &world.preview {
        pv.render(stage, sw, sh, &mut renderer.font, &renderer.theme);
    }
    if let Some(panel) = world.panel.clone() {
        let tk = Tokens::new(&renderer.theme, renderer.font.cell_width, renderer.font.cell_height);
        let mut cx = Ctx::new(stage, sw, sh, &mut renderer.font, &tk);
        draw_panel(&mut cx, area.y, world.dock, &panel, km);
    }
    #[cfg(feature = "gpu")]
    if gpu_text {
        renderer.set_gpu_suspended(false);
    }
    renderer.invalidate_all();
}

/// Mock panel (kit style) on the stage.
fn draw_panel(cx: &mut Ctx, top: usize, dock: Option<Rect>, p: &Panel, km: &Keymap) {
    let tk = cx.tk;
    let hints: Vec<(String, String)> = p.hints.iter().map(|(k, l)| (keys::expand(k, km), keys::expand(l, km))).collect();
    let hint_refs: Vec<(&str, &str)> = hints.iter().map(|(k, l)| (k.as_str(), l.as_str())).collect();
    let lines_h = p.lines.len().max(1) * tk.row_h;
    let want = cx.title_h() + if hints.is_empty() { 0 } else { cx.footer_h() } + 2 * tk.sp.md + lines_h;
    let (w, h) = (cx.w, cx.h);
    let m = tk.sp.md;
    let avail_h = h.saturating_sub(top + 2 * m);
    let side_w = (38 * tk.cw + 2 * tk.sp.lg).min(w.saturating_sub(2 * m));
    let rect = match p.place {
        Place::Center => {
            let r = cx.centered_cols(58.min(cx.cols(w.saturating_sub(4 * m))), want.min(avail_h));
            Rect::new(r.x, r.y.max(top + m), r.w, r.h)
        }
        // Docked like Mission Control (the panes were laid out beside it).
        Place::Left => dock.unwrap_or_else(|| Rect::new(m, top + m, side_w, want.min(avail_h))),
        Place::Right => Rect::new(w.saturating_sub(side_w + m), top + m, side_w, want.min(avail_h)),
        Place::Bottom => {
            let hh = want.min(avail_h);
            Rect::new(m, h.saturating_sub(hh + m), w.saturating_sub(2 * m), hh)
        }
    };
    let tone = if p.badge.eq_ignore_ascii_case("sample") { Tone::Warning } else { Tone::Accent };
    let mut spec = PanelSpec::new(&p.title).no_close().hints(&hint_refs);
    if !p.badge.is_empty() {
        spec = spec.badge(&p.badge, tone);
    }
    let body = cx.panel(rect, &spec);
    let mut y = body.y;
    for line in &p.lines {
        if y + tk.row_h > body.bottom() + tk.sp.md {
            break;
        }
        let text = keys::expand(line, km);
        let (t, c): (&str, Rgb) = if let Some(r) = text.strip_prefix("## ") {
            cx.section(body.x, y, body.w, r);
            y += tk.row_h;
            continue;
        } else if let Some(r) = text.strip_prefix("~ ") {
            (r, tk.text_muted)
        } else if let Some(r) = text.strip_prefix("! ") {
            (r, tk.warning)
        } else if let Some(r) = text.strip_prefix("+ ") {
            (r, tk.success)
        } else if let Some(r) = text.strip_prefix("> ") {
            (r, tk.accent)
        } else {
            (text.as_str(), tk.text)
        };
        cx.line_fit(body.x, y, body.w, t, c);
        y += tk.row_h;
    }
}

/// Caption text with inline key chips, word-wrapped into `r`.
fn draw_rich(cx: &mut Ctx, r: Rect, segs: &[Seg]) {
    let tk = cx.tk;
    // Tokens: (text, is_chip, space_before)
    let mut toks: Vec<(String, bool, bool)> = Vec::new();
    let mut pending_space = false;
    for s in segs {
        match s {
            Seg::Key(k) => {
                toks.push((k.clone(), true, pending_space || toks.is_empty()));
                pending_space = false;
            }
            Seg::Text(t) => {
                let starts_space = t.starts_with(' ');
                for (i, word) in t.split(' ').enumerate() {
                    if word.is_empty() {
                        continue;
                    }
                    let sp = if i == 0 { pending_space || starts_space } else { true };
                    toks.push((word.to_string(), false, sp));
                    pending_space = false;
                }
                pending_space = t.ends_with(' ');
            }
        }
    }
    let space = tk.cw;
    let chip_w = |cx: &Ctx, k: &str| cx.kbd_hint_w(k, "").saturating_sub(tk.sp.sm);
    let max_lines = (r.h / tk.row_h).max(1);
    let (mut x, mut line) = (r.x, 0usize);
    for (text, chip, sp) in &toks {
        let tw = if *chip { chip_w(cx, text) } else { cx.tw(text) };
        let lead = if *sp && x > r.x { space } else { 0 };
        if x + lead + tw > r.right() && x > r.x {
            line += 1;
            x = r.x;
            if line >= max_lines {
                break;
            }
        } else {
            x += lead;
        }
        let y = r.y + line * tk.row_h;
        if *chip {
            x += cx.kbd_chip(x, y, tk.row_h, text);
        } else {
            let t = if x + tw > r.right() { crate::ui::kit::ellipsize(text, cx.cols(r.right().saturating_sub(x))) } else { text.clone() };
            cx.line(x, y, &t, tk.text);
            x += cx.tw(&t);
        }
    }
}
