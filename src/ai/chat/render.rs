//! Drawing of the chat sidebar. Pure software rendering through the UI kit;
//! hit regions are recorded into [`Frame`] for the mouse handlers.

use super::composer::{locate, visual_lines};
use super::markdown::{self, str_cells, Block, Line, Style};
use super::session::{self, Message, Role};
use super::{ChatUi, Frame, Target, EXAMPLES};
use crate::ai::advisor::{Advisor, RiskLevel};
use crate::config::{Rgb, Theme};
use crate::renderer::font::FontManager;
use crate::ui::kit::draw::{ALL, BL, BR, TL, TOP, TR};
use crate::ui::kit::{ellipsize, mix, ButtonKind, ButtonState, Ctx, Rect, Tokens, Tone};
use crate::ui::tabbar::draw_text;

const MAX_COMPOSER_ROWS: usize = 6;

/// Pixel metrics derived from the tokens.
struct Ms {
    cw: usize,
    ch: usize,
    lh: usize,
    xs: usize,
    sm: usize,
    md: usize,
    btn_h: usize,
    code_hdr: usize,
    chip_h: usize,
}

impl Ms {
    fn new(tk: &Tokens) -> Self {
        let btn_h = tk.ch + tk.sp.sm;
        Self {
            cw: tk.cw.max(1),
            ch: tk.ch,
            lh: tk.ch + 3 * tk.scale,
            xs: tk.sp.xs,
            sm: tk.sp.sm,
            md: tk.sp.md,
            btn_h,
            code_hdr: btn_h + tk.sp.xs + tk.scale,
            chip_h: tk.ch + tk.sp.xs + tk.scale,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Adv {
    None,
    Reviewing,
    Done(RiskLevel),
}

enum Item {
    Gap(usize),
    Chips(Vec<Vec<String>>),
    Bubble(Vec<String>),
    Text { lines: Vec<Line>, indent: usize, marker: Option<String>, heading: bool, cursor: bool },
    Code {
        msg: usize,
        block: usize,
        lang: String,
        lines: Vec<String>,
        closed: bool,
        runnable: bool,
        cursor: bool,
        adv: Adv,
        footer: Vec<(String, Rgb)>,
    },
    /// Lines, index where the muted hint lines start.
    Error(Vec<String>, usize),
    Thinking,
}

fn item_h(it: &Item, ms: &Ms) -> usize {
    match it {
        Item::Gap(n) => *n,
        Item::Chips(rows) => rows.len() * (ms.chip_h + ms.xs),
        Item::Bubble(lines) => lines.len() * ms.lh + 2 * ms.sm,
        Item::Text { lines, .. } => lines.len() * ms.lh,
        Item::Code { lines, footer, .. } => ms.code_hdr + ms.xs + lines.len() * ms.lh + ms.xs + footer.len() * ms.lh + if footer.is_empty() { 0 } else { ms.xs },
        Item::Error(lines, _) => lines.len() * ms.lh + 2 * ms.sm,
        Item::Thinking => ms.lh,
    }
}

/// Greedy row packing of `widths` into rows of at most `max_w` (gap between).
fn pack_rows(widths: &[usize], max_w: usize, gap: usize) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
    let mut used = 0;
    for (i, w) in widths.iter().enumerate() {
        let need = if rows.last().unwrap().is_empty() { *w } else { used + gap + *w };
        if need > max_w && !rows.last().unwrap().is_empty() {
            rows.push(Vec::new());
            used = *w;
        } else {
            used = need;
        }
        rows.last_mut().unwrap().push(i);
    }
    rows
}

/// Fill `[x, x+w) x [y, y+h)` (y may be negative) clipped to `clip`; corners
/// that were cut off by the clip are squared so the shape looks continuous.
fn fill_clipped(cx: &mut Ctx, clip: Rect, x: usize, y: isize, w: usize, h: usize, rad: usize, c: Rgb, mask: u8) {
    let top = y.max(clip.y as isize);
    let bot = (y + h as isize).min(clip.bottom() as isize);
    if bot <= top || w == 0 {
        return;
    }
    let mut m = mask;
    if y < clip.y as isize {
        m &= !(TL | TR);
    }
    if y + h as isize > clip.bottom() as isize {
        m &= !(BL | BR);
    }
    cx.fill_rrect_ex(Rect::new(x, top as usize, w, (bot - top) as usize), rad, c, 255, m);
}

fn visible(y: isize, h: usize, clip: Rect) -> bool {
    y + h as isize > clip.y as isize && y < clip.bottom() as isize
}

/// Text at window-space `y` (must be >= 0 here), clipped to `clip_x1`.
fn put(cx: &mut Ctx, text: &str, x: usize, y: isize, clip_x1: usize, color: Rgb) -> usize {
    if y < 0 {
        return x;
    }
    let h = cx.h;
    draw_text(cx.buf, cx.w, h, cx.font, text, x, y as usize, clip_x1, color)
}

impl ChatUi {
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        buf: &mut [u32],
        w: usize,
        h: usize,
        font: &mut FontManager,
        theme: &Theme,
        area: Rect,
        advisor: &Advisor,
        preedit: &str,
    ) {
        if area.w < 8 * font.cell_width || area.h < 6 * font.cell_height || area.bottom() > h {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let ms = Ms::new(&tk);
        let elapsed = self.started.elapsed().as_secs_f32();
        let streaming = self.stream.is_some();
        let mut cx = Ctx::new(buf, w, area.bottom(), font, &tk);
        let mut hits: Vec<(Rect, Target)> = Vec::new();
        let mut list_hits: Vec<(Rect, Target)> = Vec::new();
        let hover = self.hover;

        let pad = ms.md;
        let x0 = area.x + pad;
        let full_w = area.w.saturating_sub(2 * pad);
        let inner_w = full_w.saturating_sub(ms.sm + 3 * tk.scale); // room for the scrollbar

        cx.fill(area, tk.surface);

        // ── Composer geometry (needed first: it sizes the message area) ──
        let box_inner_cols = (full_w.saturating_sub(2 * ms.sm)) / ms.cw;
        let box_inner_cols = box_inner_cols.max(1);
        self.composer.cols = box_inner_cols;
        let ctext = self.composer.chars();
        let cur = self.composer.cursor();
        let pre: Vec<char> = preedit.chars().filter(|c| !c.is_control()).collect();
        let mut disp: Vec<char> = ctext[..cur].to_vec();
        disp.extend_from_slice(&pre);
        disp.extend_from_slice(&ctext[cur..]);
        let caret_pos = cur + pre.len();
        let vlines = visual_lines(&disp, box_inner_cols);
        let (caret_row, caret_col) = locate(&disp, &vlines, caret_pos);
        let rows_shown = vlines.len().clamp(1, MAX_COMPOSER_ROWS);
        if caret_row < self.comp_top {
            self.comp_top = caret_row;
        }
        if caret_row >= self.comp_top + rows_shown {
            self.comp_top = caret_row + 1 - rows_shown;
        }
        self.comp_top = self.comp_top.min(vlines.len().saturating_sub(rows_shown));
        let box_h = rows_shown * ms.lh + 2 * ms.sm;

        // Pending-context chips above the input.
        let chip_labels: Vec<String> = self
            .pending_ctx
            .iter()
            .map(|c| ellipsize(&session::badge_for(c), (full_w / ms.cw).saturating_sub(6).max(4)))
            .collect();
        let chip_ws: Vec<usize> = chip_labels.iter().map(|l| ms.sm + ms.sm + ms.xs + l.chars().count() * ms.cw + ms.xs + ms.cw + ms.xs).collect();
        let chip_rows = if chip_ws.is_empty() { Vec::new() } else { pack_rows(&chip_ws, full_w, ms.xs) };
        let chips_h = chip_rows.len() * (ms.chip_h + ms.xs);

        let footer_h = ms.btn_h;
        let comp_h = ms.sm + chips_h + box_h + ms.xs + footer_h + ms.sm;
        let hdr_h = tk.row_h + ms.sm;
        let comp = Rect::new(area.x, area.bottom().saturating_sub(comp_h), area.w, comp_h);
        let msg_area = Rect::new(area.x, area.y + hdr_h, area.w, comp.y.saturating_sub(area.y + hdr_h));

        // ── Messages ──
        let items = if self.session.is_empty() { Vec::new() } else { self.build_items(&ms, &tk, inner_w, advisor, streaming) };
        let content_h: usize = items.iter().map(|i| item_h(i, &ms)).sum::<usize>() + ms.md;
        let view_h = msg_area.h;
        let max_scroll = content_h.saturating_sub(view_h);
        if self.stick {
            self.scroll = max_scroll;
        } else {
            self.scroll = self.scroll.min(max_scroll);
            if self.scroll >= max_scroll {
                self.stick = true;
            }
        }

        if self.session.is_empty() {
            self.draw_empty(&mut cx, &ms, msg_area, x0, inner_w, &mut list_hits, hover);
        } else {
            let mut top = msg_area.y as isize + ms.sm as isize - self.scroll as isize;
            for it in &items {
                let ih = item_h(it, &ms);
                if visible(top, ih, msg_area) {
                    self.draw_item(&mut cx, &ms, it, top, msg_area, x0, inner_w, &mut list_hits, hover, elapsed);
                }
                top += ih as isize;
            }
        }

        // ── Header (repaints any overflow from the list) ──
        let hdr = Rect::new(area.x, area.y, area.w, hdr_h);
        cx.fill(hdr, tk.surface);
        cx.hline(area.x, hdr.bottom() - 1, area.w, tk.border);
        let ty = cx.text_y(hdr.y, hdr_h);
        let tw = put(&mut cx, "Rift AI", x0, ty as isize, area.right(), tk.text);
        put(&mut cx, "Rift AI", x0 + 1, ty as isize, area.right(), tk.text); // faux bold
        let sub = if streaming { "answering...".to_string() } else { ellipsize(&self.model, 24) };
        let sub_col = if streaming { tk.accent } else { tk.text_faint };
        let btn_w = |label: &str| label.chars().count() * ms.cw + 3 * ms.sm;
        let by = hdr.y + (hdr_h - ms.btn_h) / 2;
        let close_r = Rect::new(area.right() - pad - btn_w("x"), by, btn_w("x"), ms.btn_h);
        let new_r = Rect::new(close_r.x - ms.xs - btn_w("New"), by, btn_w("New"), ms.btn_h);
        let sub_end = put(&mut cx, &sub, tw + ms.md, ty as isize, new_r.x.saturating_sub(ms.sm), sub_col);
        // LOCAL (green) / CLOUD (amber): where the answer is computed. Click: Privacy Report.
        if let Some(local) = self.local {
            let (label, tone) = if local { ("LOCAL", Tone::Success) } else { ("CLOUD", Tone::Warning) };
            let bx = sub_end + ms.sm;
            let bw = cx.badge_w(label);
            if bx + bw <= new_r.x.saturating_sub(ms.xs) {
                cx.badge(bx, hdr.y, label, tone, hdr_h);
                hits.push((Rect::new(bx, hdr.y, bw, hdr_h), Target::Privacy));
            }
        }
        for (r, label, t) in [(new_r, "New", Target::NewChat), (close_r, "x", Target::Close)] {
            let hv = hover == Some(t);
            cx.button(r, label, if hv { ButtonKind::Primary } else { ButtonKind::Secondary }, ButtonState::Normal);
            hits.push((r, t));
        }

        // ── Scrollbar / jump-to-latest / toast ──
        let sb_rect = Rect::new(msg_area.x, msg_area.y, msg_area.w.saturating_sub(ms.xs), msg_area.h);
        cx.scrollbar(sb_rect, content_h, view_h, self.scroll);
        if !self.stick && max_scroll > 0 {
            let label = "\u{2193} Latest";
            let bw = btn_w(label);
            let r = Rect::new(msg_area.x + (msg_area.w.saturating_sub(bw)) / 2, msg_area.bottom().saturating_sub(ms.btn_h + ms.sm), bw, ms.btn_h);
            let hv = hover == Some(Target::ScrollBottom);
            cx.button(r, label, if hv { ButtonKind::Primary } else { ButtonKind::Secondary }, ButtonState::Normal);
            hits.push((r, Target::ScrollBottom));
        }
        // Badge tooltip: the day's outbound total (loopback traffic is not counted).
        if hover == Some(Target::Privacy) {
            let msg = format!(
                "{} - click for the Privacy Report",
                crate::ai::local::usage::footer_text(crate::ai::local::usage::today())
            );
            let tw_ = (cx.tw(&msg) + 2 * tk.sp.lg + 3 * tk.sp.sm).min(area.w.saturating_sub(2 * pad));
            cx.toast_at(area.x + pad, hdr.bottom() + ms.xs, tw_, Tone::Neutral, &msg);
        }
        if let Some((msg, _)) = &self.toast {
            let th = tk.row_h + tk.sp.md;
            let tw_ = (cx.tw(msg) + 2 * tk.sp.lg + 3 * tk.sp.sm).min(msg_area.w.saturating_sub(2 * pad));
            let ty = msg_area.bottom().saturating_sub(th + ms.sm + ms.btn_h + ms.sm);
            if ty > msg_area.y {
                cx.toast_at(msg_area.x + (msg_area.w - tw_) / 2, ty, tw_, Tone::Neutral, msg);
            }
        }

        // ── Composer ──
        cx.fill(comp, tk.surface);
        cx.hline(comp.x, comp.y, comp.w, tk.border);
        let mut y = comp.y + ms.sm;
        for row in &chip_rows {
            let mut x = x0;
            for &i in row {
                let r = Rect::new(x, y, chip_ws[i], ms.chip_h);
                cx.fill_rrect(r, ms.chip_h / 2, tk.border_strong);
                cx.fill_rrect(r.inset(1, 1), ms.chip_h / 2, tk.surface_alt);
                let dot = Rect::new(r.x + ms.sm, r.y + ms.chip_h / 2 - ms.xs / 2, ms.xs, ms.xs);
                cx.fill_rrect(dot, ms.xs / 2, tk.accent);
                let tx = dot.right() + ms.xs;
                let tyy = cx.text_y(r.y, ms.chip_h);
                cx.text(tx, tyy, &chip_labels[i], tk.text);
                let xr = Rect::new(r.right() - ms.cw - ms.xs - ms.xs, r.y, ms.cw + 2 * ms.xs, ms.chip_h);
                let hv = hover == Some(Target::ChipX(i));
                cx.text(xr.x + ms.xs, tyy, "x", if hv { tk.danger } else { tk.text_muted });
                hits.push((xr, Target::ChipX(i)));
                x += chip_ws[i] + ms.xs;
            }
            y += ms.chip_h + ms.xs;
        }
        let box_r = Rect::new(x0, y, full_w, box_h);
        let edge = if self.focused { tk.accent } else { tk.border_strong };
        cx.fill_rrect(box_r, tk.radius_sm, edge);
        cx.fill_rrect(box_r.inset(1, 1), tk.radius_sm.saturating_sub(1), tk.field);
        hits.push((box_r, Target::Composer));
        let (tx0, ty0) = (box_r.x + ms.sm, box_r.y + ms.sm);
        if disp.is_empty() {
            let ph = "Ask anything, or describe a command...";
            put(&mut cx, ph, tx0, ty0 as isize + ((ms.lh - ms.ch) / 2) as isize, box_r.right() - ms.sm, tk.text_faint);
        }
        for (ri, vl) in vlines.iter().enumerate().skip(self.comp_top).take(rows_shown) {
            let ly = ty0 + (ri - self.comp_top) * ms.lh;
            let text: String = disp[vl.start..vl.end].iter().collect();
            put(&mut cx, &text, tx0, (ly + (ms.lh - ms.ch) / 2) as isize, box_r.right() - ms.sm, tk.text);
            // Underline the IME composition.
            if !pre.is_empty() {
                let (ps, pe) = (cur.max(vl.start), (cur + pre.len()).min(vl.end));
                if ps < pe {
                    let sx = tx0 + disp[vl.start..ps].iter().map(|c| markdown::cells(*c)).sum::<usize>() * ms.cw;
                    let wx = disp[ps..pe].iter().map(|c| markdown::cells(*c)).sum::<usize>() * ms.cw;
                    cx.fill(Rect::new(sx, ly + ms.lh - tk.scale.max(1) - 1, wx, tk.scale.max(1)), tk.accent);
                }
            }
        }
        let caret_x = tx0 + caret_col * ms.cw;
        let caret_y = ty0 + caret_row.saturating_sub(self.comp_top) * ms.lh;
        if self.focused {
            cx.fill(Rect::new(caret_x, caret_y + (ms.lh - ms.ch) / 2, 2 * tk.scale.max(1), ms.ch), tk.accent);
        }
        let ime = Some((caret_x as i32, caret_y as i32, ms.cw as u32, ms.lh as u32));
        y += box_h + ms.xs;

        // Footer: key hints on the left, Send / Stop on the right.
        let (bl, bt, kind) = if streaming { ("Stop", Target::Stop, ButtonKind::Danger) } else { ("Send", Target::Send, ButtonKind::Primary) };
        let bw = btn_w(bl);
        let br = Rect::new(box_r.right() - bw, y, bw, footer_h);
        let hints: &[(&str, &str)] = if streaming {
            &[("Esc", "stop")]
        } else if self.focused {
            &[("Enter", "send"), ("Shift+Enter", "newline"), ("Cmd+Enter", "run")]
        } else {
            &[("Cmd+Shift+A", "focus chat")]
        };
        cx.hint_row(x0, y, br.x.saturating_sub(x0 + ms.sm), footer_h, hints);
        cx.button(br, bl, kind, ButtonState::Normal);
        hits.push((br, bt));

        // ── Left edge / divider ──
        let div_col = if self.divider_drag || self.divider_hover { tk.accent } else { tk.border_strong };
        let div_w = if self.divider_drag || self.divider_hover { 2 * tk.scale.max(1) } else { 1 };
        cx.fill(Rect::new(area.x, area.y, div_w, area.h), div_col);

        // Chrome hits come last so they win over anything scrolled beneath.
        let mut all_hits = clip_list_hits(list_hits, msg_area);
        all_hits.extend(hits);
        self.frame = Frame {
            rect: area,
            hits: all_hits,
            msg_area,
            comp_origin: (tx0, ty0),
            comp_rows: rows_shown,
            comp_cols: box_inner_cols,
            max_scroll,
            line_h: ms.lh,
            ime,
        };
    }
}

/// Clip message-list hits to the list area (buttons scrolled under the
/// header or composer must not stay clickable).
fn clip_list_hits(hits: Vec<(Rect, Target)>, msg_area: Rect) -> Vec<(Rect, Target)> {
    hits.into_iter()
        .filter_map(|(r, t)| {
            let top = r.y.max(msg_area.y);
            let bot = r.bottom().min(msg_area.bottom());
            (bot > top).then(|| (Rect::new(r.x, top, r.w, bot - top), t))
        })
        .collect()
}

impl ChatUi {
    fn build_items(&self, ms: &Ms, tk: &Tokens, inner_w: usize, advisor: &Advisor, streaming: bool) -> Vec<Item> {
        let mut out: Vec<Item> = Vec::new();
        let msgs = &self.session.messages;
        let cols = (inner_w / ms.cw).max(4);
        let code_cols = (inner_w.saturating_sub(2 * ms.sm) / ms.cw).max(4);
        let last = msgs.len().saturating_sub(1);
        let last_answer = msgs.iter().rposition(|m| m.role == Role::Assistant);

        for (mi, m) in msgs.iter().enumerate() {
            match m.role {
                Role::User => {
                    self.build_user(&mut out, ms, m, inner_w);
                    if let Some(why) = &m.injection {
                        let mut lines = vec!["Possible prompt injection".to_string()];
                        lines.extend(markdown::wrap_text(&format!("The terminal text sent with this question {why}. Run buttons in the answer ask for confirmation."), cols.saturating_sub(2)));
                        let hint_at = lines.len();
                        out.push(Item::Error(lines, hint_at));
                        out.push(Item::Gap(ms.xs));
                    }
                }
                Role::Assistant => {
                    let is_streaming = streaming && mi == last;
                    let start = out.len();
                    if m.error {
                        let mut lines = vec!["Request failed".to_string()];
                        lines.extend(markdown::wrap_text(&m.content, cols.saturating_sub(2)));
                        let hint_at = lines.len();
                        lines.extend(markdown::wrap_text("Check the [llm] provider, model and api_url in your config.", cols.saturating_sub(2)));
                        out.push(Item::Error(lines, hint_at));
                    } else {
                        let mut block_idx = 0;
                        let mut first_runnable_seen = false;
                        for b in markdown::parse_blocks(&m.content) {
                            match b {
                                Block::Blank => out.push(Item::Gap(ms.lh / 2)),
                                Block::Heading { spans, .. } => {
                                    if out.len() > start {
                                        out.push(Item::Gap(ms.xs));
                                    }
                                    out.push(Item::Text { lines: markdown::wrap_spans(&spans, cols), indent: 0, marker: None, heading: true, cursor: false });
                                }
                                Block::Paragraph(spans) => {
                                    out.push(Item::Text { lines: markdown::wrap_spans(&spans, cols), indent: 0, marker: None, heading: false, cursor: false });
                                }
                                Block::ListItem { marker, indent, spans } => {
                                    let ind = 1 + indent * 2;
                                    let mw = str_cells(&marker) + 1;
                                    let tc = cols.saturating_sub(ind + mw).max(4);
                                    out.push(Item::Text { lines: markdown::wrap_spans(&spans, tc), indent: ind + mw, marker: Some(format!("{}{}", " ".repeat(ind), marker)), heading: false, cursor: false });
                                }
                                Block::Code { lang, code, closed } => {
                                    let runnable = closed && session::is_shell_lang(&lang) && !session::block_command(&code).is_empty();
                                    let is_adv_block = runnable
                                        && !first_runnable_seen
                                        && Some(mi) == last_answer
                                        && advisor.enabled
                                        && self.advised.is_some();
                                    if runnable {
                                        first_runnable_seen = true;
                                    }
                                    let mut lines = Vec::new();
                                    for l in code.split('\n') {
                                        lines.extend(markdown::wrap_code_line(l.trim_end_matches('\r'), code_cols));
                                    }
                                    let (adv, footer) = if is_adv_block { advisor_state(advisor, tk, code_cols) } else { (Adv::None, Vec::new()) };
                                    out.push(Item::Code { msg: mi, block: block_idx, lang, lines, closed, runnable, cursor: false, adv, footer });
                                    block_idx += 1;
                                }
                            }
                        }
                        // Don't leave a trailing gap inside the message.
                        while matches!(out.last(), Some(Item::Gap(_))) && out.len() > start {
                            out.pop();
                        }
                    }
                    if let (Some(why), false) = (&m.incomplete, m.error) {
                        out.push(Item::Gap(ms.xs));
                        let mut lines = vec!["Answer incomplete".to_string()];
                        lines.extend(markdown::wrap_text(why, cols.saturating_sub(2)));
                        let hint_at = lines.len();
                        out.push(Item::Error(lines, hint_at));
                    }
                    if is_streaming {
                        if out.len() == start {
                            out.push(Item::Thinking);
                        } else if let Some(it) = out[start..].iter_mut().rev().find(|i| matches!(i, Item::Text { .. } | Item::Code { .. })) {
                            match it {
                                Item::Text { cursor, .. } | Item::Code { cursor, .. } => *cursor = true,
                                _ => {}
                            }
                        }
                    }
                    out.push(Item::Gap(ms.md));
                }
            }
        }
        out
    }

    fn build_user(&self, out: &mut Vec<Item>, ms: &Ms, m: &Message, inner_w: usize) {
        if !m.context_badges.is_empty() || m.redacted > 0 {
            let max_label = (inner_w / ms.cw).saturating_sub(6).max(4);
            let mut labels: Vec<String> = m.context_badges.iter().map(|b| ellipsize(b, max_label)).collect();
            if m.redacted > 0 {
                labels.push(session::redaction_label(m.redacted));
            }
            let ws: Vec<usize> = labels.iter().map(|l| 2 * ms.sm + ms.xs + l.chars().count() * ms.cw).collect();
            let rows = pack_rows(&ws, inner_w, ms.xs).into_iter().map(|r| r.into_iter().map(|i| labels[i].clone()).collect()).collect();
            out.push(Item::Chips(rows));
        }
        if !m.content.trim().is_empty() {
            let max_bubble = inner_w * 85 / 100;
            let cols = (max_bubble.saturating_sub(2 * ms.sm) / ms.cw).max(4);
            out.push(Item::Bubble(markdown::wrap_text(m.content.trim_end(), cols)));
        }
        out.push(Item::Gap(ms.md));
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_item(
        &self,
        cx: &mut Ctx,
        ms: &Ms,
        it: &Item,
        top: isize,
        clip: Rect,
        x0: usize,
        inner_w: usize,
        hits: &mut Vec<(Rect, Target)>,
        hover: Option<Target>,
        elapsed: f32,
    ) {
        let tk = cx.tk;
        let right = x0 + inner_w;
        let text_dy = ((ms.lh - ms.ch) / 2) as isize;
        // Blinking caret shared by text and code items.
        let blink_on = (elapsed * 2.0) as u32 % 2 == 0;
        match it {
            Item::Gap(_) => {}
            Item::Thinking => {
                if top >= 0 {
                    let ty = top as usize + (ms.lh - ms.ch) / 2;
                    crate::ui::render_spinner(cx.buf, cx.w, cx.font, x0, ty, "Thinking", tk.accent, tk.text_muted, elapsed);
                    crate::ui::render_dots(cx.buf, cx.w, cx.font, x0 + 11 * ms.cw, ty, tk.accent, elapsed);
                }
            }
            Item::Chips(rows) => {
                let mut y = top;
                for row in rows {
                    let widths: Vec<usize> = row.iter().map(|l| 2 * ms.sm + ms.xs + l.chars().count() * ms.cw).collect();
                    let total: usize = widths.iter().sum::<usize>() + ms.xs * row.len().saturating_sub(1);
                    let mut x = right.saturating_sub(total);
                    for (l, w) in row.iter().zip(&widths) {
                        if visible(y, ms.chip_h, clip) {
                            fill_clipped(cx, clip, x, y, *w, ms.chip_h, ms.chip_h / 2, tk.border_strong, ALL);
                            fill_clipped(cx, clip, x + 1, y + 1, w - 2, ms.chip_h - 2, ms.chip_h / 2, tk.surface_alt, ALL);
                            let dot = ms.xs;
                            fill_clipped(cx, clip, x + ms.sm, y + (ms.chip_h / 2) as isize - (dot / 2) as isize, dot, dot, dot / 2, tk.accent, ALL);
                            put(cx, l, x + ms.sm + dot + ms.xs, y + ((ms.chip_h - ms.ch) / 2) as isize, right, tk.text_muted);
                        }
                        x += w + ms.xs;
                    }
                    y += (ms.chip_h + ms.xs) as isize;
                }
            }
            Item::Bubble(lines) => {
                let widest = lines.iter().map(|l| str_cells(l)).max().unwrap_or(0) * ms.cw;
                let bw = (widest + 2 * ms.sm).min(inner_w);
                let bh = lines.len() * ms.lh + 2 * ms.sm;
                let bx = right - bw;
                fill_clipped(cx, clip, bx, top, bw, bh, tk.radius, tk.elevated, ALL);
                for (i, l) in lines.iter().enumerate() {
                    let ly = top + (ms.sm + i * ms.lh) as isize;
                    if visible(ly, ms.lh, clip) {
                        put(cx, l, bx + ms.sm, ly + text_dy, right, tk.text);
                    }
                }
            }
            Item::Error(lines, hint_at) => {
                let bh = lines.len() * ms.lh + 2 * ms.sm;
                fill_clipped(cx, clip, x0, top, inner_w, bh, tk.radius_sm, tk.danger, ALL);
                fill_clipped(cx, clip, x0 + 1, top + 1, inner_w - 2, bh - 2, tk.radius_sm.saturating_sub(1), mix(tk.surface, tk.danger, 0.14), ALL);
                for (i, l) in lines.iter().enumerate() {
                    let ly = top + (ms.sm + i * ms.lh) as isize;
                    if !visible(ly, ms.lh, clip) {
                        continue;
                    }
                    let col = if i == 0 {
                        tk.danger
                    } else if i >= *hint_at {
                        tk.text_muted
                    } else {
                        tk.text
                    };
                    put(cx, l, x0 + ms.sm, ly + text_dy, right - ms.xs, col);
                }
            }
            Item::Text { lines, indent, marker, heading, cursor } => {
                if let Some(mk) = marker {
                    if visible(top, ms.lh, clip) {
                        put(cx, mk, x0, top + text_dy, right, tk.text_muted);
                    }
                }
                let base = x0 + indent * ms.cw;
                for (i, line) in lines.iter().enumerate() {
                    let ly = top + (i * ms.lh) as isize;
                    if !visible(ly, ms.lh, clip) {
                        continue;
                    }
                    let mut x = base;
                    for sp in line {
                        let wpx = str_cells(&sp.text) * ms.cw;
                        let col = match (sp.style, *heading) {
                            (_, true) => tk.accent,
                            (Style::Bold, _) => tk.accent,
                            (Style::Code, _) => tk.text,
                            _ => tk.text,
                        };
                        if sp.style == Style::Code {
                            fill_clipped(cx, clip, x.saturating_sub(1), ly + 1, wpx + 2, ms.lh - 2, tk.radius_sm.min(4), tk.surface_alt, ALL);
                        }
                        put(cx, &sp.text, x, ly + text_dy, right, col);
                        if *heading {
                            put(cx, &sp.text, x + 1, ly + text_dy, right, col);
                        }
                        x += wpx;
                    }
                    if *cursor && i + 1 == lines.len() && blink_on {
                        fill_clipped(cx, clip, x + 1, ly + text_dy, (ms.cw / 2).max(2), ms.ch, 0, tk.accent, 0);
                    }
                }
            }
            Item::Code { msg, block, lang, lines, closed, runnable, cursor, adv, footer } => {
                let h = item_h(it, ms);
                // Panel: border, body, header strip.
                fill_clipped(cx, clip, x0, top, inner_w, h, tk.radius_sm, tk.border_strong, ALL);
                fill_clipped(cx, clip, x0 + 1, top + 1, inner_w - 2, h - 2, tk.radius_sm.saturating_sub(1), tk.field, ALL);
                fill_clipped(cx, clip, x0 + 1, top + 1, inner_w - 2, ms.code_hdr - 1, tk.radius_sm.saturating_sub(1), tk.surface_alt, TOP);
                let hdr_y = top;
                if visible(hdr_y, ms.code_hdr, clip) {
                    let label = if lang.is_empty() { "shell" } else { lang.as_str() };
                    let ty = hdr_y + ((ms.code_hdr - ms.ch) / 2) as isize;
                    let lw = put(cx, label, x0 + ms.sm, ty, right, tk.text_muted);
                    let armed = self.confirm_run == Some((*msg, *block));
                    let run_label = if armed { "Confirm" } else { "Run" };
                    // Space the buttons will take on the right; the advisor
                    // badge must not run underneath them in a narrow sidebar.
                    let btn_labels: &[&str] = match (*closed, *runnable) {
                        (false, _) => &[],
                        (true, false) => &["Copy"],
                        (true, true) => &["Copy", "Insert", run_label],
                    };
                    let btns_w: usize = btn_labels.iter().map(|l| l.chars().count() * ms.cw + 3 * ms.sm + ms.xs).sum();
                    let badge_limit = right.saturating_sub(ms.sm + btns_w);
                    // Advisor verdict next to the language label.
                    let mut bx = lw + ms.sm;
                    match adv {
                        Adv::Reviewing => {
                            put(cx, "advisor reviewing...", bx, ty, right, tk.text_faint);
                        }
                        Adv::Done(risk) => {
                            let tone = match risk {
                                RiskLevel::Safe => Tone::Success,
                                RiskLevel::Caution => Tone::Warning,
                                RiskLevel::Danger => Tone::Danger,
                            };
                            if hdr_y >= 0 && bx + cx.badge_w(risk.label()) <= badge_limit {
                                bx += cx.badge(bx, hdr_y as usize, risk.label(), tone, ms.code_hdr);
                            }
                        }
                        Adv::None => {}
                    }
                    let _ = bx;
                    // Buttons, right to left: Copy, Insert, Run.
                    if *closed && hdr_y >= 0 {
                        let mut names: Vec<(&str, Target)> = vec![("Copy", Target::Copy(*msg, *block))];
                        if *runnable {
                            names.push(("Insert", Target::Insert(*msg, *block)));
                            names.push((run_label, Target::Run(*msg, *block)));
                        }
                        let mut x = right - ms.sm;
                        for (label, t) in names {
                            let bw = label.chars().count() * ms.cw + 3 * ms.sm;
                            x -= bw;
                            let r = Rect::new(x, hdr_y as usize + (ms.code_hdr - ms.btn_h) / 2, bw, ms.btn_h);
                            let is_run = matches!(t, Target::Run(..));
                            let hv = hover == Some(t);
                            let kind = if is_run && matches!(adv, Adv::Done(RiskLevel::Danger)) {
                                ButtonKind::Danger
                            } else if hv || (is_run && armed) {
                                ButtonKind::Primary
                            } else {
                                ButtonKind::Secondary
                            };
                            cx.button(r, label, kind, ButtonState::Normal);
                            hits.push((r, t));
                            x -= ms.xs;
                        }
                    }
                }
                let mut y = top + (ms.code_hdr + ms.xs) as isize;
                for (i, l) in lines.iter().enumerate() {
                    let ly = y + (i * ms.lh) as isize;
                    if !visible(ly, ms.lh, clip) {
                        continue;
                    }
                    let comment = l.trim_start().starts_with('#') && crate::ai::chat::session::is_shell_lang(lang);
                    let col = if comment { tk.text_faint } else { tk.text };
                    let ex = put(cx, l, x0 + ms.sm, ly + text_dy, right - ms.xs, col);
                    if *cursor && i + 1 == lines.len() && blink_on {
                        fill_clipped(cx, clip, ex + 1, ly + text_dy, (ms.cw / 2).max(2), ms.ch, 0, tk.accent, 0);
                    }
                }
                y += (lines.len() * ms.lh) as isize + ms.xs as isize;
                for (txt, col) in footer {
                    if visible(y, ms.lh, clip) {
                        put(cx, txt, x0 + ms.sm, y + text_dy, right - ms.xs, *col);
                    }
                    y += ms.lh as isize;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_empty(&self, cx: &mut Ctx, ms: &Ms, area: Rect, x0: usize, inner_w: usize, hits: &mut Vec<(Rect, Target)>, hover: Option<Target>) {
        let tk = cx.tk;
        let mut y = area.y + ms.md * 2;
        let title = "Ask Rift AI";
        let tx = x0 + inner_w.saturating_sub(title.len() * ms.cw) / 2;
        put(cx, title, tx, y as isize, x0 + inner_w, tk.accent);
        put(cx, title, tx + 1, y as isize, x0 + inner_w, tk.accent);
        y += ms.lh + ms.xs;
        let cols = (inner_w / ms.cw).max(8);
        for l in markdown::wrap_text("Explain errors, fix failed commands, or turn plain words into shell commands.", cols) {
            let lx = x0 + inner_w.saturating_sub(str_cells(&l) * ms.cw) / 2;
            put(cx, &l, lx, y as isize, x0 + inner_w, tk.text_muted);
            y += ms.lh;
        }
        y += ms.md;
        cx.section(x0, y, inner_w, "Try asking");
        y += tk.row_h + ms.xs;
        for (i, (text, _)) in EXAMPLES.iter().enumerate() {
            let r = Rect::new(x0, y, inner_w, tk.row_h);
            if r.bottom() > area.bottom() {
                break;
            }
            let hv = hover == Some(Target::Example(i));
            cx.fill_rrect(r, tk.radius_sm, tk.border);
            cx.fill_rrect(r.inset(1, 1), tk.radius_sm.saturating_sub(1), if hv { tk.accent_soft } else { tk.surface_alt });
            let ty = cx.text_y(r.y, r.h);
            cx.text(r.x + ms.sm, ty, "\u{203a}", tk.accent);
            let avail = inner_w.saturating_sub(2 * ms.sm + 2 * ms.cw);
            cx.text_fit(r.x + ms.sm + 2 * ms.cw, ty, avail, text, tk.text);
            hits.push((r, Target::Example(i)));
            y += tk.row_h + ms.xs;
        }
        y += ms.md;
        for l in markdown::wrap_text("Tip: Cmd+Enter runs the first command of an answer, Cmd+Shift+Enter inserts it.", cols) {
            if y + ms.lh > area.bottom() {
                break;
            }
            put(cx, &l, x0, y as isize, x0 + inner_w, tk.text_faint);
            y += ms.lh;
        }
        // Privacy footer: what has left the machine today.
        if y + ms.lh <= area.bottom() {
            let note = crate::ai::local::usage::footer_text(crate::ai::local::usage::today());
            put(cx, &note, x0, (area.bottom() - ms.lh) as isize, x0 + inner_w, tk.text_faint);
        }
    }
}

fn advisor_state(advisor: &Advisor, tk: &Tokens, cols: usize) -> (Adv, Vec<(String, Rgb)>) {
    if advisor.is_loading() {
        return (Adv::Reviewing, Vec::new());
    }
    let Some(r) = &advisor.review else { return (Adv::None, Vec::new()) };
    let mut footer = Vec::new();
    if let Some(s) = &r.suggestion {
        for (i, l) in markdown::wrap_text(&format!("Advisor suggests: {s}"), cols).into_iter().enumerate() {
            footer.push((if i == 0 { l } else { format!("  {l}") }, tk.accent));
        }
    }
    for n in r.notes.iter().take(2) {
        for (i, l) in markdown::wrap_text(&format!("- {n}"), cols).into_iter().enumerate() {
            footer.push((if i == 0 { l } else { format!("  {l}") }, tk.text_muted));
        }
    }
    footer.truncate(5);
    (Adv::Done(r.risk_level), footer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_rows_wraps_greedily() {
        assert_eq!(pack_rows(&[40, 40, 40], 100, 4), vec![vec![0, 1], vec![2]]);
        assert_eq!(pack_rows(&[200], 100, 4), vec![vec![0]]);
        assert_eq!(pack_rows(&[], 100, 4), vec![Vec::<usize>::new()]);
    }

    #[test]
    fn list_hits_are_clipped_to_the_message_area() {
        let msg = Rect::new(0, 20, 100, 160);
        let hits = vec![
            (Rect::new(0, 10, 10, 20), Target::Copy(0, 0)),  // half under the header
            (Rect::new(0, 100, 10, 10), Target::Run(0, 0)),  // fully visible
            (Rect::new(0, 0, 10, 15), Target::Insert(0, 0)), // fully hidden
            (Rect::new(0, 170, 10, 30), Target::Send),       // half under the composer
        ];
        let out = clip_list_hits(hits, msg);
        assert_eq!(out.len(), 3);
        let copy = out.iter().find(|(_, t)| matches!(t, Target::Copy(..))).unwrap().0;
        assert_eq!((copy.y, copy.h), (20, 10));
        let last = out.iter().find(|(_, t)| *t == Target::Send).unwrap().0;
        assert_eq!((last.y, last.h), (170, 10));
        assert!(!out.iter().any(|(_, t)| matches!(t, Target::Insert(..))));
    }

    fn sample_chat() -> ChatUi {
        use crate::ai::hub::{AskRequest, ContextItem, Intent};
        let mut c = ChatUi::new();
        c.visible = true;
        c.focused = true;
        c.model = "llama3.2".into();
        let req = AskRequest::new("", Intent::Fix).with(ContextItem::Block {
            command: "cargo build".into(),
            exit_code: Some(101),
            output: "error[E0425]".into(),
            cwd: None,
            running: false,
        });
        c.session.push_user(&req);
        c.session.push_assistant_placeholder();
        let a = c.session.last_assistant_mut().unwrap();
        a.content = "# Fix\nThe **build** failed because `foo` is undefined \u{2014} \u{627f}\u{8fd0}\u{884c}\u{4e2d}\u{6587}\u{6d4b}\u{8bd5}\u{6587}\u{5b57}.\n\n- first item\n- second item\n\n```sh\ncargo clean\ncargo build\n```\nDone.".into();
        a.refresh_actions();
        c
    }

    fn render_chat(c: &mut ChatUi, b: &mut [u32], w: usize, h: usize, f: &mut FontManager, t: &Theme, adv: &Advisor) {
        let cw = f.cell_width;
        let dock = crate::ai::chat::dock_width(0.38, w).max(40 * cw).min(w);
        let area = Rect::new(w - dock, 20, dock, h - 20);
        c.render(b, w, h, f, t, area, adv, "");
    }

    #[test]
    fn chat_renders_in_every_theme_and_records_hits() {
        let adv = Advisor::new();
        let mut c = sample_chat();
        crate::ui::kit::gallery::qa::each_theme("chat-answer", |b, w, h, f, t| render_chat(&mut c, b, w, h, f, t, &adv));
        let t: Vec<Target> = c.frame.hits.iter().map(|(_, t)| *t).collect();
        for want in [Target::Close, Target::NewChat, Target::Send, Target::Composer, Target::Run(1, 0), Target::Insert(1, 0), Target::Copy(1, 0)] {
            assert!(t.contains(&want), "missing {want:?} in {t:?}");
        }
        // Streaming: Stop replaces Send; an empty answer shows the spinner.
        let (_tx, h) = crate::ai::chat::stream::StreamHandle::manual();
        c.stream = Some(h);
        c.session.messages.last_mut().unwrap().content.clear();
        crate::ui::kit::gallery::qa::each_theme("chat-streaming", |b, w, h, f, t| render_chat(&mut c, b, w, h, f, t, &adv));
        assert!(c.frame.hits.iter().any(|(_, t)| *t == Target::Stop));
        // Error + empty states.
        c.stream = None;
        let m = c.session.messages.last_mut().unwrap();
        m.content = "HTTP 401: bad key".into();
        m.error = true;
        crate::ui::kit::gallery::qa::each_theme("chat-error", |b, w, h, f, t| render_chat(&mut c, b, w, h, f, t, &adv));
        let mut empty = ChatUi::new();
        empty.visible = true;
        crate::ui::kit::gallery::qa::each_theme("chat-empty", |b, w, h, f, t| render_chat(&mut empty, b, w, h, f, t, &adv));
        assert!(empty.frame.hits.iter().any(|(_, t)| matches!(t, Target::Example(0))));
    }

    #[test]
    fn long_conversation_scrolls_and_sticks_to_bottom() {
        let adv = Advisor::new();
        let mut c = sample_chat();
        for i in 0..30 {
            let mut m = Message::user(format!("question {i}"));
            m.prompt = None;
            c.session.messages.push(m);
            c.session.messages.push(Message::assistant(format!("answer {i}\n\n```sh\necho {i}\n```")));
        }
        crate::ui::kit::gallery::qa::each_theme("chat-long", |b, w, h, f, t| render_chat(&mut c, b, w, h, f, t, &adv));
        assert!(c.frame.max_scroll > 0);
        assert!(c.stick && c.scroll == c.frame.max_scroll);
        // Only on-screen buttons are clickable.
        let ma = c.frame.msg_area;
        assert!(c.frame.hits.iter().filter(|(_, t)| matches!(t, Target::Run(..))).all(|(r, _)| r.y >= ma.y && r.bottom() <= ma.bottom()));
        c.scroll_by(-100_000);
        assert_eq!(c.scroll, 0);
        assert!(!c.stick);
        crate::ui::kit::gallery::qa::each_theme("chat-top", |b, w, h, f, t| render_chat(&mut c, b, w, h, f, t, &adv));
        assert_eq!(c.scroll, 0);
    }

    #[test]
    fn composer_grows_and_scrolls_with_long_input() {
        let adv = Advisor::new();
        let mut c = ChatUi::new();
        c.visible = true;
        c.focused = true;
        c.composer.set_text(&"line of input\n".repeat(12));
        crate::ui::kit::gallery::qa::each_theme("chat-composer", |b, w, h, f, t| render_chat(&mut c, b, w, h, f, t, &adv));
        assert_eq!(c.frame.comp_rows, MAX_COMPOSER_ROWS);
        assert!(c.comp_top > 0, "caret at the end must be visible");
        assert!(c.frame.ime.is_some());
    }
}

