//! Pixels for the inline AI: ask popover, suggestion / hint bars, toast.
//! Painted on the output buffer after the damage-tracked pane render and
//! recomputed every frame (nothing is cached in the renderer's back buffer).

use super::{Hits, InlineAi, NlState, PopoverMode};
use crate::renderer::Renderer;
use crate::ui::kit::{ellipsize, Ctx, Rect, Tokens, Tone};
use crate::window::{PaneRect, WindowManager};

#[cfg(target_os = "macos")]
const ASK_KEY: &str = "Cmd+K";
#[cfg(not(target_os = "macos"))]
const ASK_KEY: &str = "Super+K";

/// Top y of a bar of height `bar_h` for a prompt on screen row `cur_row`:
/// in the (blank) row below the prompt when there is room, otherwise in the
/// row above it. `None` when neither exists.
///
/// A bar taller than a text row (HiDPI: kit paddings scale with the font, so
/// `bar_h` can be ~2 rows) is aligned to the *far* edge of its row instead of
/// centred on it, so it never covers the prompt line itself: below the prompt
/// it starts at the top of the row under it, above the prompt it ends at the
/// prompt's top edge. A bar that is not taller than a row stays centred.
pub(crate) fn bar_top(rect_y: usize, rect_h: usize, ch: usize, bar_h: usize, cur_row: usize, below_blank: bool) -> Option<usize> {
    let slack = ch.saturating_sub(bar_h) / 2; // > 0 only when the bar is shorter than a row
    if below_blank {
        let top = rect_y + (cur_row + 1) * ch + slack;
        if top + bar_h <= rect_y + rect_h {
            return Some(top);
        }
    }
    if cur_row >= 1 {
        let prompt_top = rect_y + cur_row * ch;
        let top = if bar_h > ch { prompt_top.saturating_sub(bar_h) } else { prompt_top - ch + slack };
        return Some(top.max(rect_y));
    }
    None
}

/// Top y of the popover: directly under `anchor_row` when it fits, else
/// directly above it. The popover is a whole number of rows tall (see `draw`)
/// and starts/ends on a row boundary, so it never cuts a text row in half.
pub(crate) fn popover_top(rect_y: usize, rect_h: usize, ch: usize, pop_h: usize, anchor_row: usize) -> usize {
    let below = rect_y + (anchor_row + 1) * ch;
    if below + pop_h <= rect_y + rect_h {
        return below;
    }
    let above = (rect_y + anchor_row * ch).saturating_sub(pop_h);
    above.max(rect_y)
}

enum Bar {
    Fix { cmd: String, expl: String, dangerous: bool, can_accept: bool },
    Wait { query: String, secs: f32 },
    Ghost { query: String, dangerous: bool },
}

/// Draw the bar at `y`, right-aligned in `rect`. Returns the bar rectangle
/// and the rectangle of each drawn hint, in order.
#[allow(clippy::too_many_arguments)]
fn draw_bar(
    cx: &mut Ctx,
    rect: PaneRect,
    y: usize,
    badges: &[(&str, Tone)],
    primary: &str,
    secondary: &str,
    spinner: Option<char>,
    hints: &[(&str, &str)],
) -> (Rect, Vec<Rect>) {
    let tk = *cx.tk;
    let bar_h = tk.row_h + tk.sp.xs;
    let pad = tk.sp.md;
    let gap = tk.sp.sm;
    let avail = rect.width.saturating_sub(2 * tk.sp.md);

    let spin_w = if spinner.is_some() { 2 * tk.cw } else { 0 };
    let badges_w: usize = badges.iter().map(|(t, _)| cx.badge_w(t) + gap).sum();
    let fixed = 2 * pad + spin_w + badges_w;
    let min_text = 16 * tk.cw;

    // Longest prefix of the hints that still leaves room for the text.
    let hint_w = |n: usize| -> usize {
        hints[..n].iter().map(|(k, l)| cx.kbd_hint_w(k, l)).sum::<usize>() + n.saturating_sub(1) * tk.sp.lg + if n > 0 { gap } else { 0 }
    };
    let mut n = hints.len();
    while n > 0 && fixed + hint_w(n) + min_text > avail {
        n -= 1;
    }
    let hw = hint_w(n);
    let text_room = avail.saturating_sub(fixed + hw);
    let want_text = cx.tw(primary) + if secondary.is_empty() { 0 } else { 2 * tk.cw + cx.tw(secondary) };
    let text_w = want_text.min(text_room);
    let w = (fixed + hw + text_w).min(avail).max(tk.cw * 8);

    let x = rect.x + rect.width.saturating_sub(w + tk.sp.md);
    let r = Rect::new(x, y, w, bar_h);
    cx.shadow(r, tk.radius);
    cx.fill_rrect(r, tk.radius, tk.border_strong);
    cx.fill_rrect(r.inset(1, 1), tk.radius.saturating_sub(1), tk.elevated);

    let line_y = r.y + (bar_h - tk.row_h) / 2;
    let ty = cx.text_y(r.y, bar_h);
    let mut cxp = r.x + pad;
    if let Some(ch) = spinner {
        cx.text(cxp, ty, &ch.to_string(), tk.accent);
        cxp += spin_w;
    }
    for (t, tone) in badges {
        cxp += cx.badge(cxp, r.y, t, *tone, bar_h) + gap;
    }
    let text_end = r.right().saturating_sub(pad + hw);
    let used = cx.text_fit(cxp, ty, text_end.saturating_sub(cxp), primary, tk.text);
    if !secondary.is_empty() {
        let sx = cxp + used + 2 * tk.cw;
        if sx < text_end {
            cx.text_fit(sx, ty, text_end - sx, secondary, tk.text_muted);
        }
    }

    // Hints, right-aligned.
    let mut hx = r.right().saturating_sub(pad + hw.saturating_sub(gap));
    let mut rects = Vec::new();
    for (k, l) in &hints[..n] {
        let kw = cx.kbd_hint(hx, line_y, k, l);
        rects.push(Rect::new(hx, r.y, kw, bar_h));
        hx += kw + tk.sp.lg;
    }
    (r, rects)
}

#[allow(clippy::too_many_arguments)]
pub fn draw(
    wm: &WindowManager,
    renderer: &mut Renderer,
    ai: &mut InlineAi,
    buffer: &mut [u32],
    w: usize,
    h: usize,
    area: PaneRect,
    preedit: &str,
) {
    ai.hits = Hits::default();
    ai.ime_hint = None;

    let pane = wm.active_pane();
    let t = &pane.terminal;
    let toast = ai.active_toast().map(str::to_owned);

    // What bar (if any) to show for the active pane.
    let can_accept = t.typed_input().is_some_and(|s| s.is_empty());
    let bar = if ai.popover.is_some() || t.is_alt_screen() || t.is_scrolled_back() {
        None
    } else if let NlState::Pending(p) = &ai.nl {
        (p.pane_id == pane.id).then(|| Bar::Wait { query: p.query.clone(), secs: p.started.elapsed().as_secs_f32() })
    } else if let NlState::Ghost(g) = &ai.nl {
        (g.pane_id == pane.id).then(|| Bar::Ghost { query: g.query.clone(), dangerous: g.dangerous })
    } else {
        ai.fix.current.as_ref().filter(|f| f.sig.pane_id == pane.id).map(|f| Bar::Fix {
            cmd: f.suggestion.command.clone(),
            expl: f.suggestion.explanation.clone(),
            dangerous: f.dangerous,
            can_accept,
        })
    };

    if ai.popover.is_none() && bar.is_none() && toast.is_none() {
        return;
    }

    let rect = wm
        .pane_layouts(area)
        .into_iter()
        .find(|(_, _, active)| *active)
        .map(|(_, r, _)| r)
        .unwrap_or(area);
    let ch = renderer.cell_height().max(1);
    let theme = renderer.theme.clone();
    let tk = Tokens::new(&theme, renderer.font.cell_width, renderer.font.cell_height);
    let abs_rows = crate::blocks_ui::view::view_abs_rows(t);
    let cursor_abs = t.scrollback.len() + t.cursor_row;
    let cur_row = abs_rows.iter().position(|r| *r == Some(cursor_abs));
    let below_blank = t
        .grid
        .get(t.cursor_row + 1)
        .is_some_and(|row| row.iter().all(|c| c.c == ' ' || c.c == '\0'))
        && cur_row.is_some_and(|r| r + 1 < abs_rows.len());

    let mut cx = Ctx::new(buffer, w, h, &mut renderer.font, &tk);

    if let (Some(bar), Some(cur_row)) = (&bar, cur_row) {
        let bar_h = tk.row_h + tk.sp.xs;
        if let Some(y) = bar_top(rect.y, rect.height, ch, bar_h, cur_row, below_blank) {
            let k = ASK_KEY;
            match bar {
                Bar::Fix { cmd, expl, dangerous, can_accept } => {
                    let mut badges = vec![("Fix", Tone::Accent)];
                    if *dangerous {
                        badges.push(("RISKY", Tone::Danger));
                    }
                    let mut hints: Vec<(&str, &str)> = Vec::new();
                    if *can_accept {
                        hints.push(("Tab", "accept"));
                    }
                    hints.push(("Esc", "dismiss"));
                    hints.push((k, "ask more"));
                    let secondary = if expl.is_empty() { String::new() } else { format!("\u{2014} {expl}") };
                    let (r, rects) = draw_bar(&mut cx, rect, y, &badges, cmd, &secondary, None, &hints);
                    let mut hit = Hits { bar: Some(r), ..Default::default() };
                    // Rects line up with the (possibly truncated) hint list.
                    for (rc, (key, _)) in rects.iter().zip(hints.iter()) {
                        match *key {
                            "Tab" => hit.accept = Some(*rc),
                            "Esc" => hit.dismiss = Some(*rc),
                            _ => hit.ask_more = Some(*rc),
                        }
                    }
                    ai.hits = hit;
                }
                Bar::Wait { query, secs } => {
                    let msg = format!("Generating command: {}", ellipsize(query, 40));
                    let (r, _) = draw_bar(&mut cx, rect, y, &[("AI", Tone::Accent)], &msg, "", Some(crate::ui::spinner_char(*secs)), &[("Esc", "cancel")]);
                    ai.hits = Hits { bar: Some(r), ..Default::default() };
                }
                Bar::Ghost { query, dangerous } => {
                    let mut badges = vec![("AI", Tone::Accent)];
                    if *dangerous {
                        badges.push(("RISKY", Tone::Danger));
                    }
                    let from = format!("from: {}", ellipsize(query, 48));
                    let (r, _) = draw_bar(&mut cx, rect, y, &badges, &from, "", None, &[("Enter", "run"), ("Esc", "clear"), (k, "refine")]);
                    ai.hits = Hits { bar: Some(r), ..Default::default() };
                }
            }
        }
    }

    if let Some(pop) = &ai.popover {
        let pad = tk.sp.sm;
        let pw = (64 * tk.cw).min(rect.width.saturating_sub(2 * tk.sp.md)).max(24 * tk.cw.min(rect.width / 24 + 1));
        // Round up to whole terminal rows so the edges fall between text rows;
        // the spare pixels are split between top and bottom padding.
        let raw_ph = 2 * pad + tk.row_h + tk.input_h + tk.row_h;
        let ph = raw_ph.div_ceil(ch) * ch;
        let anchor_row = pop
            .resolved
            .anchor_line()
            .and_then(|l| abs_rows.iter().position(|r| *r == Some(l)))
            .or(cur_row)
            .unwrap_or(0);
        let py = popover_top(rect.y, rect.height, ch, ph, anchor_row);
        let px = (rect.x + 2 * tk.cw).min((rect.x + rect.width).saturating_sub(pw + tk.sp.md)).max(rect.x);
        let r = Rect::new(px, py, pw, ph);
        let float_inner = cx.float(r);
        let inner = Rect::new(float_inner.x, float_inner.y + (ph - raw_ph) / 2, float_inner.w, float_inner.h);

        // Header: badge + what "this" is.
        let (title, tone) = match &pop.mode {
            PopoverMode::Ask => ("Ask AI", Tone::Accent),
            PopoverMode::RefineNl { .. } => ("Refine", Tone::Accent),
        };
        let bw = cx.badge_line(inner.x, inner.y, title, tone);
        let label = match &pop.mode {
            PopoverMode::Ask => pop.resolved.label(),
            PopoverMode::RefineNl { previous } => format!("Current: {previous}"),
        };
        cx.line_fit(inner.x + bw + tk.sp.sm, inner.y, inner.w.saturating_sub(bw + tk.sp.sm), &label, tk.text_muted);

        // Input (with the IME preedit spliced in at the cursor).
        let ir = Rect::new(inner.x, inner.y + tk.row_h, inner.w, tk.input_h);
        let chars: Vec<char> = pop.edit.text().chars().collect();
        let cur = pop.edit.cursor().min(chars.len());
        let pre: Vec<char> = preedit.chars().collect();
        let mut shown: String = chars[..cur].iter().collect();
        shown.extend(pre.iter());
        shown.extend(chars[cur..].iter());
        let shown_cursor = cur + pre.len();
        let placeholder = match (&pop.mode, pop.resolved.is_failure()) {
            (PopoverMode::RefineNl { .. }, _) => "How should the command change?",
            (_, true) => "Enter to get a fix, or ask a question",
            (_, false) if pop.resolved.has_context() => "Enter to explain, or ask a question",
            _ => "Ask anything",
        };
        cx.text_input(ir, &shown, shown_cursor, None, placeholder, true);

        // IME candidate window anchor: the caret inside the input.
        let cols = cx.cols(ir.w.saturating_sub(2 * tk.sp.md + 2 * tk.scale)).max(1);
        let vis_cursor = shown_cursor.min(cols);
        ai.ime_hint = Some(Rect::new(ir.x + tk.sp.md + vis_cursor * tk.cw, ir.y, tk.cw, ir.h));

        let enter_label = match &pop.mode {
            PopoverMode::Ask => "ask",
            PopoverMode::RefineNl { .. } => "regenerate",
        };
        cx.hint_row(inner.x, inner.y + tk.row_h + tk.input_h, inner.w, tk.row_h, &[("Enter", enter_label), ("Esc", "cancel")]);
        ai.hits.popover = Some(r);
    }

    if let Some(msg) = toast {
        cx.toast(Tone::Warning, &msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_prefers_blank_row_below_prompt() {
        // ch=20, bar_h=28 (taller than a row), prompt on row 3, room below:
        // the bar starts at the top of row 4, never covering the prompt row.
        let y = bar_top(40, 400, 20, 28, 3, true).unwrap();
        assert_eq!(y, 40 + 4 * 20);
        // No blank row below: it ends at the prompt's top edge.
        let y = bar_top(40, 400, 20, 28, 3, false).unwrap();
        assert_eq!(y + 28, 40 + 3 * 20);
        // A bar shorter than a row is centred in the row it uses.
        let y = bar_top(40, 400, 20, 12, 3, true).unwrap();
        assert_eq!(y, 40 + 4 * 20 + 4);
        let y = bar_top(40, 400, 20, 12, 3, false).unwrap();
        assert_eq!(y, 40 + 2 * 20 + 4);
        // Prompt on the first row with nothing below: nowhere to go.
        assert_eq!(bar_top(40, 400, 20, 28, 0, false), None);
        // Never above the pane top.
        assert_eq!(bar_top(40, 400, 20, 60, 1, false), Some(40));
        // Prompt on the last row: below does not fit, so it goes above.
        assert!(bar_top(0, 100, 20, 28, 4, true).unwrap() + 28 <= 4 * 20);
        // A tall bar needs its whole height inside the pane to go below.
        assert!(bar_top(0, 100, 20, 28, 3, true).unwrap() + 28 <= 3 * 20);
    }

    #[test]
    fn popover_flips_above_near_the_bottom() {
        // Plenty of room: directly under the anchor row (on a row boundary).
        assert_eq!(popover_top(40, 400, 20, 100, 2), 40 + 3 * 20);
        // Anchor near the bottom: sits above it.
        let y = popover_top(40, 400, 20, 100, 17);
        assert!(y + 100 <= 40 + 17 * 20, "{y}");
        // Tiny pane: clamps to the pane top.
        assert_eq!(popover_top(40, 60, 20, 100, 1), 40);
    }
}
