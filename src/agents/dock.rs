//! Drawing the Mission Control dock through the UI kit. Layout decisions
//! (which rows a card has, how tall it is) live in `ui.rs` and are pure; this
//! file paints them and records every clickable area into [`Frame::hits`].

use std::time::Instant;

use super::autopilot::{self, AutoView};
use super::control::{Composer, MenuItem, Mode, PaneInfo, Risk};
use super::metrics;
use super::prompt::{self, ApprovalPrompt, PromptKind, Role};
use super::ui::{self, AgentsUi, CardFlags, Detail, Hit, Row};
use super::{AgentKind, AgentRegistry, AgentSession, AgentState};
use crate::config::{Rgb, Theme};
use crate::renderer::font::FontManager;
use crate::review::TurnDigest;
use crate::ui::kit::{ellipsize, mix, ButtonKind, ButtonState, Ctx, Rect, Tokens, Tone};

// ───────────────────────────── frame state ─────────────────────────────

/// Per-frame drawing state shared by the helpers.
struct Frame<'a> {
    hits: Vec<(Rect, Hit)>,
    hover: Option<Hit>,
    now: Instant,
    t: f32,
    focused: bool,
    preedit: &'a str,
    ime: Option<Rect>,
}

impl Frame<'_> {
    fn add(&mut self, r: Rect, h: Hit) {
        self.hits.push((r, h));
    }

    fn hot(&self, h: &Hit) -> bool {
        self.hover.as_ref() == Some(h)
    }
}

// ───────────────────────────── pure content helpers ─────────────────────────────

/// How a line of the prompt box is styled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineStyle {
    Head,
    Command,
    Path,
    Text,
    Faint,
    Danger,
}

/// Text lines of the prompt box (first line = heading), `cols` characters wide.
pub fn prompt_body(p: &ApprovalPrompt, risk: &Risk, cols: usize, compact: bool) -> Vec<(String, LineStyle)> {
    let cols = cols.max(8);
    let mut v: Vec<(String, LineStyle)> = Vec::new();
    let head = if p.title.is_empty() { p.kind.label().to_string() } else { p.title.clone() };
    v.push((head, LineStyle::Head));
    match p.kind {
        PromptKind::Command => {
            // The whole command matters before approving it: wrap, never cut mid-line.
            let lines = p.command.as_deref().map(|c| c.lines().map(str::to_string).collect::<Vec<_>>()).unwrap_or_else(|| p.subject.clone());
            let visual: Vec<String> = lines.iter().flat_map(|l| chunk(l, cols.saturating_sub(2))).collect();
            let shown = visual.len().min(if compact { MAX_COMMAND_LINES_COMPACT } else { MAX_COMMAND_LINES });
            for (i, l) in visual.iter().take(shown).enumerate() {
                let prefix = if i == 0 { "$ " } else { "  " };
                let more = i + 1 == shown && visual.len() > shown;
                let text = format!("{prefix}{l}{}", if more { " \u{2026}" } else { "" });
                v.push((ellipsize(&text, cols), LineStyle::Command));
            }
        }
        PromptKind::Edit | PromptKind::Create => {
            if let Some(f) = &p.file {
                v.push((fit_tail(f, cols), LineStyle::Path));
            } else {
                for l in ui::wrap(&p.question, cols).into_iter().take(2) {
                    v.push((l, LineStyle::Text));
                }
            }
        }
        _ => {
            for l in ui::wrap(&p.question, cols).into_iter().take(2) {
                v.push((l, LineStyle::Text));
            }
            for l in p.subject.iter().take(1) {
                v.push((ellipsize(l, cols), LineStyle::Faint));
            }
        }
    }
    if !compact {
        if let Some(r) = &p.reason {
            v.push((ellipsize(&format!("why: {r}"), cols), LineStyle::Faint));
        }
        if let Some(i) = p.index_of(Role::Always) {
            let label = &p.options[i].label;
            v.push((ellipsize(&format!("{} = {}", p.key_hint(i), label), cols), LineStyle::Faint));
        }
    }
    if let Some(first) = risk.impacts().first() {
        let wrapped = ui::wrap(&format!("! {first}"), cols);
        let n = wrapped.len();
        let keep = if compact { 1 } else { 2 };
        for (i, l) in wrapped.into_iter().take(keep).enumerate() {
            let l = if i + 1 == keep && n > keep { format!("{l}\u{2026}") } else { l };
            v.push((ellipsize(&l, cols), LineStyle::Danger));
        }
    }
    v
}

/// Command lines in a compact prompt box.
pub const MAX_COMMAND_LINES_COMPACT: usize = 2;

/// Lines of a command shown in the prompt box.
pub const MAX_COMMAND_LINES: usize = 4;

/// Wrap at `cols` characters, preferably after a space; a word longer than a
/// line is cut. Nothing is dropped but the spaces at a break (commands must
/// stay readable in full before they are approved).
pub fn chunk(s: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars.len() - i <= cols {
            out.push(chars[i..].iter().collect());
            break;
        }
        // Last space within the next `cols + 1` chars (a space right after the line ends it cleanly).
        let window = &chars[i..=i + cols];
        match window.iter().rposition(|c| *c == ' ').filter(|p| *p > 0) {
            Some(p) => {
                out.push(chars[i..i + p].iter().collect());
                i += p;
                while i < chars.len() && chars[i] == ' ' {
                    i += 1;
                }
            }
            None => {
                out.push(chars[i..i + cols].iter().collect());
                i += cols;
            }
        }
    }
    out
}

/// Keep the end of a path: `src/agents/ui.rs` -> `…/agents/ui.rs`.
pub fn fit_tail(s: &str, cols: usize) -> String {
    let n = s.chars().count();
    if n <= cols || cols < 2 {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - (cols - 1)).collect();
    format!("\u{2026}{tail}")
}

/// The decision buttons of a prompt: (option index, short label, long label), in
/// approve / always / deny order (one per role).
pub fn answer_buttons(p: &ApprovalPrompt) -> Vec<(usize, &'static str, &'static str)> {
    let mut v = Vec::new();
    for (role, long, short) in [(Role::Approve, "Approve", "Yes"), (Role::Always, "Always", "Always"), (Role::Deny, "Deny", "No")] {
        if let Some(i) = p.index_of(role) {
            v.push((i, short, long));
        }
    }
    v
}

/// Metric chips of a card: (text, tone).
pub fn metric_items(s: &AgentSession, info: Option<&PaneInfo>) -> Vec<(String, Tone)> {
    let _ = s;
    let Some(info) = info else { return Vec::new() };
    let m = &info.metrics;
    let mut v: Vec<(String, Tone)> = Vec::new();
    if let Some(model) = &m.model {
        v.push((model.clone(), Tone::Accent));
    }
    if let Some(t) = m.tokens {
        v.push((format!("{} tok", metrics::fmt_tokens(t)), Tone::Neutral));
    }
    if let Some(c) = m.cost {
        v.push((metrics::fmt_cost(c), Tone::Neutral));
    }
    if let Some(c) = m.context_used {
        let tone = if c >= 85 { Tone::Danger } else if c >= 70 { Tone::Warning } else { Tone::Neutral };
        v.push((format!("ctx {c}%"), tone));
    }
    if let Some(r) = &m.reset_in {
        v.push((format!("reset {r}"), Tone::Neutral));
    }
    match info.files {
        (Some(n), total) if total > n => v.push((format!("{n} files ({total})"), Tone::Success)),
        (Some(n), _) => v.push((format!("{n} file{}", if n == 1 { "" } else { "s" }), Tone::Success)),
        (None, total) if total > 0 => v.push((format!("{total} files"), Tone::Neutral)),
        _ => {}
    }
    v
}

/// "tab 2", or "win 2 · tab 1" for agents of the second and later windows.
fn tab_label(s: &AgentSession) -> String {
    match crate::app::windows::pane_window(s.pane_uid) {
        0 => format!("tab {}", s.tab_index + 1),
        w => format!("win {} \u{b7} tab {}", w + 1, s.tab_index + 1),
    }
}

/// "Codex · tab 2" for the collision chip.
fn other_label(o: &AgentSession) -> String {
    format!("{} \u{b7} {}", o.kind.name(), tab_label(o))
}

/// One timeline row: (glyph tone, "#3", duration, summary text, summary tone).
pub fn turn_row(d: &TurnDigest, n: usize) -> (Tone, String, String, String, Tone) {
    let dur = d.duration.map(ui::format_turn).unwrap_or_else(|| "running".into());
    if d.running {
        return (Tone::Accent, format!("#{n}"), dur, "in progress".into(), Tone::Accent);
    }
    if d.failed {
        return (Tone::Danger, format!("#{n}"), dur, "snapshot failed".into(), Tone::Danger);
    }
    match d.summary {
        Some(s) if s.files > 0 => (
            Tone::Success,
            format!("#{n}"),
            dur,
            format!("{} file{} {}", s.files, if s.files == 1 { "" } else { "s" }, s.short()),
            Tone::Success,
        ),
        Some(_) => (Tone::Neutral, format!("#{n}"), dur, "no changes".into(), Tone::Neutral),
        None => (Tone::Neutral, format!("#{n}"), dur, "\u{2026}".into(), Tone::Neutral),
    }
}

// ───────────────────────────── small widgets ─────────────────────────────

fn pill(cx: &mut Ctx, x: usize, y: usize, h: usize, s: &AgentSession, t: f32) -> usize {
    let tk = cx.tk;
    let tone = ui::state_tone(s.state);
    let fg = tk.tone(tone);
    let label = ui::state_label(s.state);
    let glyph_w = match s.state {
        AgentState::Done { .. } | AgentState::Error => tk.ch * 7 / 10 + tk.sp.xs,
        _ => 0,
    };
    let w = cx.tw(label) + glyph_w + 2 * tk.sp.sm;
    let r = Rect::new(x, y, w, h);
    let fill_alpha = match s.state {
        AgentState::Working => 40 + (ui::pulse(t, 1.2) * 60.0) as u8,
        AgentState::WaitingForUser => 60 + (ui::pulse(t, 0.9) * 130.0) as u8,
        _ => 46,
    };
    cx.fill_rrect_ex(r, h / 2, fg, fill_alpha, crate::ui::kit::draw::ALL);
    let ty = cx.text_y(y, h);
    let mut tx = x + tk.sp.sm;
    match s.state {
        AgentState::Done { .. } => {
            let sz = tk.ch * 7 / 10;
            draw_check(cx, tx, y + (h.saturating_sub(sz)) / 2, sz, fg);
            tx += glyph_w;
        }
        AgentState::Error => {
            let sz = tk.ch * 7 / 10;
            draw_cross(cx, tx, y + (h.saturating_sub(sz)) / 2, sz, fg);
            tx += glyph_w;
        }
        _ => {}
    }
    cx.text(tx, ty, label, fg);
    w
}

fn draw_check(cx: &mut Ctx, x: usize, y: usize, s: usize, c: Rgb) {
    // Two strokes: short down-right, long up-right.
    let t = (s / 8).max(1);
    let mid = (x + s * 2 / 5, y + s * 4 / 5);
    for k in 0..=s * 2 / 5 {
        let (px, py) = (x + k, y + s / 2 + k * 3 / 4);
        cx.fill(Rect::new(px, py, t + 1, t + 1), c);
    }
    for k in 0..=s * 3 / 5 {
        let (px, py) = (mid.0 + k, mid.1.saturating_sub(k * 4 / 3));
        cx.fill(Rect::new(px, py, t + 1, t + 1), c);
    }
}

fn draw_cross(cx: &mut Ctx, x: usize, y: usize, s: usize, c: Rgb) {
    let t = (s / 8).max(1);
    for k in 0..s {
        cx.fill(Rect::new(x + k, y + k, t + 1, t + 1), c);
        cx.fill(Rect::new(x + s - 1 - k, y + k, t + 1, t + 1), c);
    }
}

pub(crate) fn kind_chip(cx: &mut Ctx, x: usize, y: usize, size: usize, kind: AgentKind) {
    let c = kind.color();
    cx.fill_rrect(Rect::new(x, y, size, size), cx.tk.radius_sm, c);
    // Dark or light glyph, whichever reads better on the brand colour.
    let ink = if crate::ui::kit::luminance(c) > 0.35 { (20, 20, 24) } else { (250, 250, 252) };
    let gx = x + size.saturating_sub(cx.tk.cw) / 2;
    let gy = y + size.saturating_sub(cx.tk.ch) / 2;
    cx.text(gx, gy, &kind.glyph().to_string(), ink);
}

/// Small tinted button with an optional key hint. Returns its width.
fn btn(cx: &mut Ctx, fr: &mut Frame, x: usize, y: usize, h: usize, key: &str, label: &str, tone: Tone, enabled: bool, hit: Hit) -> usize {
    let tk = cx.tk;
    let kw = if key.is_empty() { 0 } else { cx.tw(key) + tk.sp.xs + tk.scale };
    let w = kw + cx.tw(label) + 2 * tk.sp.sm;
    let r = Rect::new(x, y, w, h);
    let hot = enabled && fr.hot(&hit);
    let fg = if enabled { tk.tone(tone) } else { tk.text_faint };
    let fill = mix(tk.surface, fg, if hot { 0.34 } else { 0.15 });
    cx.fill_rrect(r, tk.radius_sm, fill);
    cx.stroke_rrect(r, tk.radius_sm, tk.scale.max(1), mix(tk.surface, fg, if hot { 0.8 } else { 0.45 }));
    let ty = cx.text_y(y, h);
    let mut tx = x + tk.sp.sm;
    if !key.is_empty() {
        cx.text(tx, ty, key, mix(fill, fg, 0.65));
        tx += kw;
    }
    cx.text(tx, ty, label, fg);
    fr.add(r, hit);
    w
}

fn btn_w(cx: &Ctx, key: &str, label: &str) -> usize {
    let tk = cx.tk;
    (if key.is_empty() { 0 } else { cx.tw(key) + tk.sp.xs + tk.scale }) + cx.tw(label) + 2 * tk.sp.sm
}

// ───────────────────────────── cards ─────────────────────────────

struct CardData<'a> {
    s: &'a AgentSession,
    info: Option<&'a PaneInfo>,
    selected: bool,
    marked: bool,
    any_marked: bool,
    hovered: bool,
    others: Vec<String>,
    items: Vec<(String, Tone)>,
    prompt: Option<&'a ApprovalPrompt>,
    mode: &'a Mode,
    composer: &'a Composer,
    marks: usize,
    auto: AutoView,
}

impl CardData<'_> {
    fn waiting(&self) -> bool {
        self.s.state == AgentState::WaitingForUser
    }

    fn risk(&self) -> Risk {
        self.info.map(|i| i.risk.clone()).unwrap_or_default()
    }

    /// Does the current modal state belong to this card?
    fn confirm_text(&self) -> Option<String> {
        match self.mode {
            Mode::ConfirmClose(u) if *u == self.s.pane_uid => Some("Close this pane?".into()),
            Mode::ConfirmRestart(u) if *u == self.s.pane_uid => Some(format!("Restart {}?", self.s.kind.slug())),
            Mode::ConfirmRisk { uid, .. } if *uid == self.s.pane_uid => Some("Critical command: confirm".into()),
            Mode::ConfirmSend { targets } if self.selected => Some(format!("Send to {} agents?", targets.len())),
            _ => None,
        }
    }

    fn composing(&self) -> bool {
        self.selected && matches!(self.mode, Mode::Compose { .. } | Mode::ConfirmSend { .. })
    }
}

/// Everything about a card that the layout needs, computed once per frame.
struct Plan {
    flags: CardFlags,
    /// Metric items grouped into lines.
    lines: Vec<Vec<usize>>,
    /// Prompt box text, full and trimmed (compact cards).
    body: Vec<(String, LineStyle)>,
    body_compact: Vec<(String, LineStyle)>,
    /// Workflow block (label, queue, countdown, reviewer note).
    wf: Vec<(String, Tone)>,
    /// Autopilot block: countdown, switch state and counters.
    auto: Vec<(String, Tone)>,
}

fn plan_card(d: &CardData, inner_cols: usize) -> Plan {
    let widths: Vec<usize> = d.items.iter().map(|(t, _)| t.chars().count()).collect();
    let lines = ui::flow(&widths, inner_cols, 3);
    let (body, body_compact) = match (d.waiting(), d.prompt) {
        (true, Some(p)) => {
            let cols = inner_cols.saturating_sub(2);
            (prompt_body(p, &d.risk(), cols, false), prompt_body(p, &d.risk(), cols, true))
        }
        _ => (Vec::new(), Vec::new()),
    };
    let wf = d.info.map(|i| crate::workflow::card_lines(&i.wf, inner_cols, if d.selected { 3 } else { 2 })).unwrap_or_default();
    let auto = autopilot::auto_lines(&d.auto, inner_cols, d.selected);
    let raw = if d.waiting() && body.is_empty() { d.info.map_or(0, |i| i.raw_tail.len().min(3)) } else { 0 };
    let flags = CardFlags {
        selected: d.selected,
        waiting: d.waiting(),
        prompt_lines: body.len(),
        prompt_lines_compact: body_compact.len(),
        raw_lines: raw,
        metrics_lines: lines.len(),
        collision: !d.others.is_empty(),
        confirm: d.confirm_text().is_some(),
        composer: d.composing(),
        turns: d.info.map_or(0, |i| i.turns.len()),
        workflow_lines: wf.len(),
        auto_lines: auto.len(),
    };
    Plan { flags, lines, body, body_compact, wf, auto }
}

fn draw_card(cx: &mut Ctx, fr: &mut Frame, r: Rect, d: &CardData, detail: Detail, row_list: &[Row], plan: &Plan) {
    let lines = &plan.lines;
    let body: &[(String, LineStyle)] = if detail == Detail::Full { &plan.body } else { &plan.body_compact };
    let tk = cx.tk;
    let s = d.s;
    let uid = s.pane_uid;
    let fg = tk.tone(ui::state_tone(s.state));
    let bg = ui::card_bg(tk, d.selected, d.hovered);
    cx.fill_rrect(r, tk.radius_sm, bg);
    if s.state == AgentState::WaitingForUser {
        // Breathing amber edge: this is the card that needs a human.
        let a = 90 + (ui::pulse(fr.t, 0.9) * 150.0) as u32;
        cx.stroke_rrect(r, tk.radius_sm, tk.scale.max(1), mix(bg, tk.warning, a as f32 / 255.0));
    } else if d.selected && fr.focused {
        cx.stroke_rrect(r, tk.radius_sm, tk.scale.max(1), tk.accent);
    } else if d.marked {
        cx.stroke_rrect(r, tk.radius_sm, tk.scale.max(1), mix(bg, tk.accent, 0.7));
    }
    // State stripe.
    let stripe = Rect::new(r.x + tk.scale, r.y + tk.sp.xs, 3 * tk.scale, r.h.saturating_sub(2 * tk.sp.xs));
    cx.fill_rrect(stripe, tk.scale + 1, fg);
    fr.add(r, Hit::Card(uid));

    let left = r.x + tk.sp.md + 3 * tk.scale;
    let right = r.right().saturating_sub(tk.sp.sm);
    let inner_w = right.saturating_sub(left);
    let lp = ui::line_pitch(tk);
    let mut y = r.y + ui::card_pad(tk);
    for row in row_list {
        let h = ui::row_h(*row, tk);
        match *row {
            Row::Header => draw_header(cx, fr, d, left, right, y, h),
            Row::Place => draw_place(cx, d, left, right, y),
            Row::Status => draw_status(cx, fr, d, detail, left, right, y, h),
            Row::Metrics(n) => {
                for (li, line) in lines.iter().take(n).enumerate() {
                    let mut x = left;
                    for (k, idx) in line.iter().enumerate() {
                        if k > 0 {
                            cx.text(x + cx.tk.cw, y + li * lp, "\u{b7}", tk.text_faint);
                            x += 3 * tk.cw;
                        }
                        let (text, tone) = &d.items[*idx];
                        let colour = if *tone == Tone::Neutral { tk.text_muted } else { tk.tone(*tone) };
                        x += cx.text_fit(x, y + li * lp, right.saturating_sub(x), text, colour);
                    }
                }
            }
            Row::Chips { hint } => draw_chips(cx, d, left, right, y, hint),
            Row::Prompt(_) => draw_prompt(cx, fr, d, Rect::new(left, y, inner_w, h), body),
            Row::Buttons => draw_answers(cx, fr, d, left, right, y, h),
            Row::Raw(n) => {
                if let Some(info) = d.info {
                    let from = info.raw_tail.len().saturating_sub(n);
                    for (i, l) in info.raw_tail[from..].iter().enumerate() {
                        cx.text_fit(left, y + i * lp, inner_w, l, tk.text_muted);
                    }
                }
            }
            Row::Confirm => draw_confirm(cx, fr, d, left, right, y, h),
            Row::Composer => draw_composer(cx, fr, d, left, right, y, h),
            Row::Actions => draw_actions(cx, fr, d, left, right, y, h),
            Row::Timeline(n) => draw_timeline(cx, fr, d, left, right, y, n),
            Row::Auto(n) => {
                let band = Rect::new(left.saturating_sub(tk.sp.xs), y, inner_w + tk.sp.xs, n * lp + tk.sp.xs);
                if fr.hot(&Hit::Autopilot(uid)) {
                    cx.fill_rrect(band, tk.radius_sm, tk.surface_alt);
                }
                for (i, (text, tone)) in plan.auto.iter().take(n).enumerate() {
                    let colour = if *tone == Tone::Neutral { tk.text_faint } else { tk.tone(*tone) };
                    cx.text_fit(left, y + i * lp + lp.saturating_sub(tk.ch) / 2 + tk.sp.xs / 2, inner_w, text, colour);
                }
                fr.add(band, Hit::Autopilot(uid));
            }
            Row::Workflow(n) => {
                let band = Rect::new(left.saturating_sub(tk.sp.xs), y, inner_w + tk.sp.xs, n * lp);
                for (i, (text, tone)) in plan.wf.iter().take(n).enumerate() {
                    let colour = if *tone == Tone::Neutral { tk.text_muted } else { tk.tone(*tone) };
                    cx.text_fit(left, y + i * lp + lp.saturating_sub(tk.ch) / 2, inner_w, text, colour);
                }
                fr.add(band, Hit::Queue(uid));
            }
        }
        y += h + ui::rows_gap(tk);
    }
}

fn draw_header(cx: &mut Ctx, fr: &mut Frame, d: &CardData, left: usize, right: usize, y: usize, h: usize) {
    let tk = cx.tk;
    let s = d.s;
    let chip = tk.ch + tk.sp.xs;
    let cy = y + h.saturating_sub(chip) / 2;
    kind_chip(cx, left, cy, chip, s.kind);
    let el = ui::format_elapsed(ui::elapsed_for(s, fr.now));
    let el_w = cx.tw(&el);
    let ty = cx.text_y(y, h);
    cx.text(right.saturating_sub(el_w), ty, &el, tk.text_muted);
    // Broadcast mark: a check box that shows on hover / selection / while marking.
    let mut limit = right.saturating_sub(el_w + tk.sp.sm);
    if d.marked || d.any_marked || d.hovered || d.selected {
        let bs = (tk.ch * 3 / 4).max(8);
        let bx = limit.saturating_sub(bs);
        let by = y + h.saturating_sub(bs) / 2;
        let r = Rect::new(bx, by, bs, bs);
        if d.marked {
            cx.fill_rrect(r, tk.scale + 1, tk.accent);
            let ink = if crate::ui::kit::luminance(tk.accent) > 0.35 { (20, 20, 24) } else { (250, 250, 252) };
            draw_check(cx, bx + bs / 6, by + bs / 6, bs * 2 / 3, ink);
        } else {
            cx.stroke_rrect(r, tk.scale + 1, tk.scale.max(1), tk.border_strong);
        }
        fr.add(Rect::new(bx.saturating_sub(tk.sp.xs), y, bs + 2 * tk.sp.xs, h), Hit::Mark(s.pane_uid));
        limit = bx.saturating_sub(tk.sp.sm);
    }
    let name_x = left + chip + tk.sp.sm;
    cx.text_fit(name_x, ty, limit.saturating_sub(name_x), &s.title, tk.text);
}

fn draw_place(cx: &mut Ctx, d: &CardData, left: usize, right: usize, y: usize) {
    let tk = cx.tk;
    let s = d.s;
    let tab = tab_label(s);
    let tab_w = cx.tw(&tab);
    cx.text(right.saturating_sub(tab_w), y, &tab, tk.text_faint);
    let mut place = s.place();
    if let Some(dir) = ui::worktree_dir(s) {
        place = format!("{place}  wt {dir}");
    }
    cx.text_fit(left, y, right.saturating_sub(left + tab_w + tk.sp.sm), &place, tk.text_muted);
}

fn draw_status(cx: &mut Ctx, fr: &mut Frame, d: &CardData, detail: Detail, left: usize, right: usize, y: usize, h: usize) {
    let tk = cx.tk;
    let s = d.s;
    let pw = pill(cx, left, y, h, s, fr.t);
    let px = left + pw + tk.sp.sm;
    let text = if detail == Detail::Full {
        match (&s.state, &s.waiting_reason) {
            (AgentState::WaitingForUser, Some(r)) => r.clone(),
            _ => s.preview.clone(),
        }
    } else {
        // Compact: where it runs instead of what it said.
        s.place()
    };
    cx.text_fit(px, y + (h.saturating_sub(tk.ch)) / 2, right.saturating_sub(px), &text, tk.text_faint);
}

fn draw_chips(cx: &mut Ctx, d: &CardData, left: usize, right: usize, y: usize, hint: bool) {
    let tk = cx.tk;
    let h = tk.ch + tk.sp.xs;
    let label = format!("same worktree as {}", d.others.join(", "));
    let max_w = right.saturating_sub(left);
    let text = ellipsize(&label, cx.cols(max_w.saturating_sub(2 * tk.sp.sm)));
    let w = cx.tw(&text) + 2 * tk.sp.sm;
    let r = Rect::new(left, y, w, h);
    cx.fill_rrect_ex(r, h / 2, tk.warning, 52, crate::ui::kit::draw::ALL);
    cx.text(left + tk.sp.sm, y + h.saturating_sub(tk.ch) / 2, &text, tk.warning);
    if hint {
        let lp = ui::line_pitch(tk);
        cx.text_fit(left, y + h + tk.sp.xs / 2 + (lp.saturating_sub(tk.ch)) / 2, max_w, "tip: New Agent > worktree", tk.text_faint);
    }
}

fn draw_prompt(cx: &mut Ctx, fr: &mut Frame, d: &CardData, r: Rect, body: &[(String, LineStyle)]) {
    let tk = cx.tk;
    cx.well(r);
    let lp = ui::line_pitch(tk);
    let x = r.x + tk.sp.sm;
    let w = r.w.saturating_sub(2 * tk.sp.sm);
    let risk = d.risk();
    for (i, (text, style)) in body.iter().enumerate() {
        let y = r.y + tk.sp.sm + i * lp;
        match style {
            LineStyle::Head => {
                cx.text_fit(x, y, w, &text.to_uppercase(), tk.warning);
                // Badge, right aligned on the heading line.
                let (label, tone) = match &risk {
                    Risk::Critical(_) => ("RISKY", Tone::Danger),
                    Risk::Risky(_) => ("RISKY", Tone::Danger),
                    Risk::Pending => ("checking", Tone::Neutral),
                    Risk::Safe => ("", Tone::Neutral),
                };
                if !label.is_empty() {
                    let bw = cx.badge_w(label);
                    let bx = (x + w).saturating_sub(bw);
                    // Make room by blanking the heading's tail under the badge.
                    cx.fill(Rect::new(bx.saturating_sub(tk.sp.xs), y, bw + tk.sp.xs, lp), tk.field);
                    cx.badge(bx, y.saturating_sub(tk.sp.xs / 2), label, tone, lp + tk.sp.xs / 2);
                }
            }
            LineStyle::Command => {
                cx.text_fit(x, y, w, text, tk.text);
            }
            LineStyle::Path => {
                cx.text_fit(x, y, w, text, tk.accent);
            }
            LineStyle::Text => {
                cx.text_fit(x, y, w, text, tk.text);
            }
            LineStyle::Faint => {
                cx.text_fit(x, y, w, text, tk.text_faint);
            }
            LineStyle::Danger => {
                cx.text_fit(x, y, w, text, tk.danger);
            }
        }
    }
    let _ = fr;
}

fn draw_answers(cx: &mut Ctx, fr: &mut Frame, d: &CardData, left: usize, right: usize, y: usize, h: usize) {
    let tk = cx.tk;
    let Some(p) = d.prompt else { return };
    let uid = d.s.pane_uid;
    let list = prompt::plan_answer;
    let buttons = answer_buttons(p);
    let avail = right.saturating_sub(left);
    let gap = tk.sp.xs;
    let risk = d.risk();
    let bh = ui::action_h(tk);
    let by = y + tk.sp.xs / 2;
    let _ = h;
    // Full labels if they fit, else the short ones.
    let width_of = |short: bool, cx: &Ctx| -> usize {
        buttons.iter().map(|(i, s, l)| btn_w(cx, &p.key_hint(*i), if short { s } else { l })).sum::<usize>() + gap * buttons.len().saturating_sub(1)
    };
    let short = width_of(false, cx) > avail;
    let mut x = left;
    for (i, s_label, l_label) in &buttons {
        let role = p.options[*i].role;
        let mut tone = match role {
            Role::Approve => Tone::Success,
            Role::Always => Tone::Accent,
            Role::Deny => Tone::Danger,
            Role::Other => Tone::Neutral,
        };
        if risk.is_flagged() && matches!(role, Role::Approve | Role::Always) {
            tone = Tone::Warning;
        }
        let pending = matches!(risk, Risk::Pending) && p.grants(*i);
        let armed = matches!(d.mode, Mode::ConfirmRisk { uid: u, option } if *u == uid && *option == *i);
        let enabled = list(p, *i).is_some() && !pending;
        let label = if armed { "Confirm" } else if short { s_label } else { l_label };
        x += btn(cx, fr, x, by, bh, &p.key_hint(*i), label, tone, enabled, Hit::Answer { uid, option: *i }) + gap;
    }
}

fn draw_confirm(cx: &mut Ctx, fr: &mut Frame, d: &CardData, left: usize, right: usize, y: usize, h: usize) {
    let tk = cx.tk;
    let Some(text) = d.confirm_text() else { return };
    let bh = ui::action_h(tk);
    let by = y + tk.sp.xs / 2;
    let esc_w = btn_w(cx, "Esc", "Cancel");
    let ok_w = btn_w(cx, "Enter", "Confirm");
    let danger = matches!(d.mode, Mode::ConfirmClose(_) | Mode::ConfirmRisk { .. });
    let bx = right.saturating_sub(esc_w);
    btn(cx, fr, bx, by, bh, "Esc", "Cancel", Tone::Neutral, true, Hit::Confirm(false));
    let ox = bx.saturating_sub(ok_w + tk.sp.xs);
    btn(cx, fr, ox, by, bh, "Enter", "Confirm", if danger { Tone::Danger } else { Tone::Success }, true, Hit::Confirm(true));
    cx.text_fit(left, cx.text_y(y, h), ox.saturating_sub(left + tk.sp.sm), &text, if danger { tk.danger } else { tk.text });
}

fn draw_composer(cx: &mut Ctx, fr: &mut Frame, d: &CardData, left: usize, right: usize, y: usize, h: usize) {
    let tk = cx.tk;
    let uid = d.s.pane_uid;
    let broadcast = matches!(d.mode, Mode::Compose { broadcast: true } | Mode::ConfirmSend { .. });
    let send_w = btn_w(cx, "", "Send");
    let r = Rect::new(left, y, right.saturating_sub(left + send_w + tk.sp.xs), h);
    let (text, col) = d.composer.display();
    // The IME preedit sits at the caret.
    let (shown, caret) = if fr.preedit.is_empty() {
        (text.clone(), col)
    } else {
        let chars: Vec<char> = text.chars().collect();
        let head: String = chars[..col.min(chars.len())].iter().collect();
        let tail: String = chars[col.min(chars.len())..].iter().collect();
        (format!("{head}{}{tail}", fr.preedit), col + fr.preedit.chars().count())
    };
    let placeholder = if broadcast { format!("Broadcast to {} agents\u{2026}", d.marks) } else { format!("Reply to {}\u{2026}", d.s.kind.name()) };
    cx.text_input(r, &shown, caret, None, &placeholder, true);
    // Caret rectangle for the OS candidate window (mirrors `text_input`'s scrolling).
    let pad = tk.sp.md;
    let cols = cx.cols(r.w.saturating_sub(2 * pad + 2 * tk.scale)).max(1);
    let start = if caret + 1 > cols { caret + 1 - cols } else { 0 };
    let cx_px = r.x + pad + (caret - start.min(caret)) * tk.cw;
    fr.ime = Some(Rect::new(cx_px, r.y + r.h.saturating_sub(tk.ch) / 2, tk.cw, tk.ch));
    let bh = h.min(ui::action_h(tk) + tk.sp.xs);
    let by = y + (h.saturating_sub(bh)) / 2;
    btn(cx, fr, r.right() + tk.sp.xs, by, bh, "", "Send", Tone::Success, !d.composer.is_empty(), Hit::SendReply);
    let _ = uid;
}

fn draw_actions(cx: &mut Ctx, fr: &mut Frame, d: &CardData, left: usize, right: usize, y: usize, h: usize) {
    let tk = cx.tk;
    let uid = d.s.pane_uid;
    let live = d.s.state.is_live();
    let gap = tk.sp.xs;
    let more_w = btn_w(cx, "", "\u{b7}\u{b7}\u{b7}");
    let items: Vec<(&str, &str, Tone, Hit)> = if live {
        vec![("Esc", "Stop", Tone::Neutral, Hit::Interrupt(uid)), ("r", "Reply", Tone::Neutral, Hit::Reply(uid)), ("v", "Review", Tone::Neutral, Hit::Review(uid))]
    } else {
        vec![("R", "Restart", Tone::Neutral, Hit::Restart(uid)), ("v", "Review", Tone::Neutral, Hit::Review(uid)), ("x", "Close", Tone::Neutral, Hit::Close(uid))]
    };
    let mut x = left;
    for (key, label, tone, hit) in items {
        let w = btn_w(cx, key, label);
        if x + w + gap + more_w > right {
            break;
        }
        x += btn(cx, fr, x, y, h, key, label, tone, true, hit) + gap;
    }
    btn(cx, fr, x, y, h, "", "\u{b7}\u{b7}\u{b7}", Tone::Neutral, true, Hit::More(uid));
}

fn draw_timeline(cx: &mut Ctx, fr: &mut Frame, d: &CardData, left: usize, right: usize, y: usize, n: usize) {
    let tk = cx.tk;
    let Some(info) = d.info else { return };
    let lp = ui::line_pitch(tk);
    cx.text(left, y, "TURNS", tk.text_faint);
    let total = info.turns.len();
    cx.text_right(right, y, &format!("{total} total"), tk.text_faint);
    for (k, t) in info.turns.iter().rev().take(n).enumerate() {
        let number = total - k;
        let (gt, num, dur, summary, st) = turn_row(t, number);
        let ry = y + (k + 1) * lp;
        let row = Rect::new(left.saturating_sub(tk.sp.xs), ry, right.saturating_sub(left) + tk.sp.xs, lp);
        let hit = Hit::Turn { uid: d.s.pane_uid, id: t.id };
        if fr.hot(&hit) {
            cx.fill_rrect(row, tk.radius_sm, tk.surface_alt);
        }
        let dsz = (tk.ch / 3).max(5);
        let gy = ry + lp.saturating_sub(dsz) / 2;
        cx.fill_rrect_ex(Rect::new(left, gy, dsz, dsz), dsz / 2, tk.tone(gt), 255, crate::ui::kit::draw::ALL);
        let mut x = left + dsz + tk.sp.sm;
        cx.text(x, ry + lp.saturating_sub(tk.ch) / 2, &num, tk.text_muted);
        x += (num.chars().count() + 1) * tk.cw;
        cx.text(x, ry + lp.saturating_sub(tk.ch) / 2, &dur, tk.text_faint);
        x += (dur.chars().count().max(6) + 1) * tk.cw;
        let colour = if st == Tone::Neutral { tk.text_faint } else { tk.tone(st) };
        cx.text_fit(x, ry + lp.saturating_sub(tk.ch) / 2, right.saturating_sub(x), &summary, colour);
        fr.add(row, hit);
    }
}

// ───────────────────────────── dock ─────────────────────────────

fn draw_density_icon(cx: &mut Ctx, fr: &mut Frame, r: Rect, compact: bool) {
    let tk = cx.tk;
    let hit = Hit::Density;
    let hot = fr.hot(&hit);
    let colour = if hot { tk.accent } else { tk.text_muted };
    let bars = if compact { 4 } else { 3 };
    let bh = tk.scale.max(1) * 2;
    let total = r.h * 2 / 3;
    let gap = (total.saturating_sub(bars * bh)) / (bars - 1).max(1);
    let x = r.x + r.w / 6;
    let w = r.w * 2 / 3;
    let y0 = r.y + r.h.saturating_sub(bars * bh + gap * (bars - 1)) / 2;
    for i in 0..bars {
        // Expanded: the first bar of each pair is long; compact: all short.
        let bw = if compact { w } else if i % 2 == 0 { w } else { w * 2 / 3 };
        cx.fill(Rect::new(x, y0 + i * (bh + gap), bw, bh), colour);
    }
    fr.add(r, hit);
}

/// Footer totals across agents: (text, tone) segments, most important first.
pub fn totals_segments(reg: &AgentRegistry, info: &super::control::InfoMap) -> Vec<(String, Tone)> {
    let live = reg.sessions().iter().filter(|s| s.state.is_live()).count();
    let waiting = reg.attention_count();
    let (tokens, cost) = metrics::totals(reg.sessions().iter().filter_map(|s| info.get(&s.pane_uid)).map(|i| &i.metrics));
    let mut v = vec![(format!("{live} agent{}", if live == 1 { "" } else { "s" }), Tone::Neutral)];
    if waiting > 0 {
        v.push((format!("{waiting} waiting"), Tone::Warning));
    }
    if cost > 0.0 {
        v.push((metrics::fmt_cost(cost), Tone::Neutral));
    }
    if tokens > 0 {
        v.push((format!("{} tok", metrics::fmt_tokens(tokens)), Tone::Neutral));
    }
    v
}

/// Draw the dock into `buf`. `dock` is its rectangle. Records the clickable
/// areas in `ui.hits` and the composer caret in `ui.ime_rect`.
pub fn draw_dock(
    buf: &mut [u32],
    w: usize,
    h: usize,
    font: &mut FontManager,
    theme: &Theme,
    dock: Rect,
    reg: &AgentRegistry,
    ui_state: &mut AgentsUi,
    now: Instant,
    t: f32,
    preedit: &str,
) {
    let (cw, ch) = (font.cell_width, font.cell_height);
    let tk = Tokens::new(theme, cw, ch);
    let mut cx = Ctx::new(buf, w, h, font, &tk);
    let tk = cx.tk;
    let mut fr = Frame { hits: Vec::new(), hover: ui_state.hover.clone(), now, t, focused: ui_state.focused, preedit, ime: None };

    // Panel + right edge (a brighter handle while hovered / dragged).
    cx.fill(dock, mix(tk.bg, tk.surface, 0.6));
    let edge_hot = ui_state.resizing || fr.hot(&Hit::Edge);
    if edge_hot {
        let tw = 2 * tk.scale.max(1);
        cx.fill(Rect::new(dock.right().saturating_sub(tw), dock.y, tw, dock.h), tk.accent);
    } else {
        cx.vline(dock.right().saturating_sub(1), dock.y, dock.h, tk.border);
    }

    let l = ui::layout(dock, tk);
    let sessions = reg.sessions();
    let uids: Vec<usize> = sessions.iter().map(|s| s.pane_uid).collect();
    let sel = ui_state.selected_index(&uids);
    let selected_uid = sel.map(|i| uids[i]);

    // ── Header: title, counts, density toggle.
    let hx = dock.x + tk.sp.md;
    let title_y = cx.text_y(l.header.y, l.header.h);
    cx.text(hx, title_y, "AGENTS", tk.accent);
    let icon = Rect::new(dock.right().saturating_sub(tk.sp.md + tk.row_h), l.header.y, tk.row_h, l.header.h);
    if !sessions.is_empty() {
        draw_density_icon(&mut cx, &mut fr, Rect::new(icon.x, icon.y + (icon.h.saturating_sub(tk.row_h)) / 2, icon.w, tk.row_h), ui_state.compact);
    }
    // Autopilot switch, right of the title.
    let (auto_label, auto_tone) = autopilot::header_label(&ui_state.auto);
    let auto_w = cx.badge_w(&auto_label);
    let auto_x = hx + 8 * tk.cw;
    let auto_shown = auto_x + auto_w + tk.sp.sm < icon.x;
    let auto_end = if auto_shown { auto_x + auto_w } else { hx + 6 * tk.cw };
    if auto_shown {
        cx.badge(auto_x, l.header.y, &auto_label, auto_tone, l.header.h);
        fr.add(Rect::new(auto_x, l.header.y, auto_w, l.header.h), Hit::AutopilotAll);
    }
    let attention = reg.attention_count();
    let live = reg.live_count();
    let mut rx = icon.x.saturating_sub(tk.sp.sm);
    if attention > 0 {
        let label = format!("{attention} needs you");
        let bw = cx.badge_w(&label);
        if rx.saturating_sub(bw) > auto_end + tk.sp.sm {
            cx.badge(rx.saturating_sub(bw), l.header.y, &label, Tone::Warning, l.header.h);
        }
        rx = rx.saturating_sub(bw + tk.sp.sm);
    }
    if live > 0 && rx > auto_end + 4 * tk.cw {
        let label = format!("{live} live");
        let bw = cx.badge_w(&label);
        if rx.saturating_sub(bw) > auto_end + tk.sp.sm {
            cx.badge(rx.saturating_sub(bw), l.header.y, &label, Tone::Neutral, l.header.h);
        }
    }
    cx.hline(dock.x, l.header.bottom().saturating_sub(1), dock.w.saturating_sub(1), tk.border);

    // ── Cards.
    let pad_x = tk.sp.sm;
    let list = Rect::new(l.list.x + pad_x, l.list.y, l.list.w.saturating_sub(2 * pad_x + 1), l.list.h);
    let mut menu_anchor: Option<Rect> = None;
    if sessions.is_empty() {
        draw_empty(&mut cx, &mut fr, l.list);
    } else {
        let collisions = metrics::collisions(sessions);
        let empty_info = PaneInfo::default();
        let _ = &empty_info;
        let mode = ui_state.ctl.mode.clone();
        let composer = ui_state.ctl.composer.clone();
        let marks = ui_state.ctl.marked.len();
        let inner_cols = cx.cols(list.w.saturating_sub(tk.sp.md + tk.sp.sm + 3 * tk.scale));
        let datas: Vec<CardData> = sessions
            .iter()
            .map(|s| {
                let info = ui_state.info.get(&s.pane_uid);
                let waiting = s.state == AgentState::WaitingForUser;
                CardData {
                    s,
                    info,
                    selected: selected_uid == Some(s.pane_uid),
                    marked: ui_state.ctl.is_marked(s.pane_uid),
                    any_marked: marks > 0,
                    hovered: matches!(ui_state.hover, Some(Hit::Card(u)) if u == s.pane_uid),
                    others: collisions.get(&s.pane_uid).map(|v| v.iter().filter_map(|o| reg.session(*o)).map(other_label).collect()).unwrap_or_default(),
                    items: metric_items(s, info),
                    prompt: info.and_then(|i| i.prompt.as_ref()).filter(|_| waiting),
                    mode: &mode,
                    composer: &composer,
                    marks,
                    auto: ui_state.auto.view(s.pane_uid, now),
                }
            })
            .collect();
        let want = if ui_state.compact { Detail::Compact } else { Detail::Full };
        // Row plans and heights: optional content gives way before agents scroll out of view.
        let plans: Vec<Plan> = datas.iter().map(|d| plan_card(d, inner_cols)).collect();
        let flag_list: Vec<CardFlags> = plans.iter().map(|p| p.flags).collect();
        let fitted = ui::fit_cards(&flag_list, sel, want, list.h, l.gap, tk);
        let heights: Vec<usize> = fitted.iter().map(|f| f.height).collect();

        let max = ui::max_scroll(list.h, l.gap, &heights);
        ui_state.scroll = ui_state.scroll.min(max);
        if ui_state.follow {
            if let Some(i) = sel {
                ui_state.scroll = ui::scroll_to(list, l.gap, &heights, i, ui_state.scroll).min(max.max(i.min(heights.len().saturating_sub(1))));
            }
            ui_state.follow = false;
        }
        let shown = ui::plan(list, l.gap, &heights, ui_state.scroll);
        for (i, r) in &shown {
            let detail = fitted[*i].detail;
            let row_list = ui::rows(&fitted[*i].flags, detail);
            draw_card(&mut cx, &mut fr, *r, &datas[*i], detail, &row_list, &plans[*i]);
            if let Mode::Menu { uid, .. } = &ui_state.ctl.mode {
                if *uid == datas[*i].s.pane_uid {
                    menu_anchor = Some(*r);
                }
            }
        }
        cx.scrollbar(l.list, sessions.len(), shown.len().max(1), ui_state.scroll);
    }

    // The list may have overdrawn into the header / footer when a lone card was taller than the list.
    cx.fill(Rect::new(dock.x, l.footer.y, dock.w.saturating_sub(1), dock.bottom().saturating_sub(l.footer.y)), mix(tk.bg, tk.surface, 0.6));

    // ── Footer: totals, then hints for the current mode.
    cx.hline(dock.x, l.footer.y, dock.w.saturating_sub(1), tk.border);
    let fy = l.footer.y + tk.sp.xs;
    let tyy = cx.text_y(fy, tk.row_h);
    if !sessions.is_empty() {
        let limit = dock.right().saturating_sub(tk.sp.md + 1);
        let mut x = hx;
        for (i, (text, tone)) in totals_segments(reg, &ui_state.info).iter().enumerate() {
            let sep = if i > 0 { 3 * tk.cw } else { 0 };
            if x + sep + cx.tw(text) > limit {
                break;
            }
            if i > 0 {
                cx.text(x + tk.cw, tyy, "\u{b7}", tk.text_faint);
                x += sep;
            }
            let colour = if *tone == Tone::Neutral { tk.text_muted } else { tk.tone(*tone) };
            cx.text(x, tyy, text, colour);
            x += cx.tw(text);
        }
    }
    let waiting_selected = selected_uid.and_then(|u| reg.session(u)).is_some_and(|s| s.state == AgentState::WaitingForUser);
    let hints = if sessions.is_empty() { vec![("Esc", "back")] } else { ui_state.ctl.hints(waiting_selected) };
    cx.hint_row(hx, l.footer.y + tk.row_h + tk.sp.xs, dock.w.saturating_sub(2 * tk.sp.md), tk.row_h, &hints);

    // ── Context menu on top.
    if let Mode::Menu { uid, sel } = ui_state.ctl.mode.clone() {
        draw_menu(&mut cx, &mut fr, dock, l.list, menu_anchor, uid, sel, reg.session(uid).is_some_and(|s| s.state.is_live()));
    }

    // The draggable edge wins over whatever is under it.
    fr.add(ui::edge_zone(dock), Hit::Edge);
    ui_state.hits = fr.hits;
    ui_state.ime_rect = fr.ime;
}

fn draw_menu(cx: &mut Ctx, fr: &mut Frame, dock: Rect, list: Rect, anchor: Option<Rect>, uid: usize, sel: usize, live: bool) {
    let tk = cx.tk;
    // Clicking anywhere else closes it.
    fr.add(dock, Hit::MenuDismiss);
    let item_h = tk.row_h;
    let labels: Vec<(MenuItem, &str, &str)> = MenuItem::ALL.iter().map(|m| (*m, m.label(), m.key())).collect();
    let w = labels.iter().map(|(_, l, k)| cx.tw(l) + cx.tw(k) + 4 * tk.sp.md).max().unwrap_or(0).min(dock.w.saturating_sub(2 * tk.sp.sm));
    let h = labels.len() * item_h + 2 * tk.sp.sm;
    let ax = dock.right().saturating_sub(w + tk.sp.md);
    let mut ay = anchor.map_or(list.y + tk.sp.sm, |a| a.y + tk.row_h);
    if ay + h > dock.bottom() {
        ay = dock.bottom().saturating_sub(h + tk.sp.sm).max(dock.y);
    }
    let r = Rect::new(ax, ay, w, h);
    let inner = cx.float(r);
    for (i, (item, label, key)) in labels.iter().enumerate() {
        let row = Rect::new(inner.x, inner.y + i * item_h, inner.w, item_h);
        let hit = Hit::Menu { uid, item: *item };
        let hot = i == sel || fr.hot(&hit);
        cx.row_bg(row, hot, false);
        let enabled = live || !matches!(item, MenuItem::Interrupt | MenuItem::CtrlC | MenuItem::Reply);
        let colour = if !enabled { tk.text_faint } else if matches!(item, MenuItem::Close) { tk.danger } else { tk.text };
        let ty = cx.text_y(row.y, row.h);
        cx.text_fit(row.x + tk.sp.md, ty, row.w.saturating_sub(2 * tk.sp.md + cx.tw(key)), label, colour);
        cx.text_right(row.right().saturating_sub(tk.sp.sm), ty, key, tk.text_faint);
        fr.add(row, hit);
    }
}

fn draw_empty(cx: &mut Ctx, fr: &mut Frame, area: Rect) {
    let tk = cx.tk;
    let lp = ui::line_pitch(tk);
    let cols = cx.cols(area.w.saturating_sub(2 * tk.sp.lg));
    let hook_lines = ui::wrap("Hooks make state exact: call rift agent-event from Claude Code's hooks.", cols);
    let intro = ui::wrap("Run claude, codex or gemini in any pane", cols);
    let total = tk.row_h + lp * intro.len() + tk.sp.md + tk.button_h + tk.sp.xl + lp * (hook_lines.len() + 1);
    let mut y = area.y + area.h.saturating_sub(total) / 2;
    cx.text_center(area.x, cx.text_y(y, tk.row_h), area.w, "No agents running", tk.text_muted);
    y += tk.row_h;
    for l in &intro {
        cx.text_center(area.x, y, area.w, l, tk.text_faint);
        y += lp;
    }
    y += tk.sp.md;
    let label = "New Agent\u{2026}";
    let bw = cx.button_w(label);
    let br = Rect::new(area.x + area.w.saturating_sub(bw) / 2, y, bw, tk.button_h);
    let hit = Hit::NewAgent;
    cx.button(br, label, ButtonKind::Primary, if fr.hot(&hit) { ButtonState::Focused } else { ButtonState::Normal });
    fr.add(br, hit);
    y += tk.button_h + tk.sp.xl;
    for l in &hook_lines {
        cx.text_center(area.x, y, area.w, l, tk.text_faint);
        y += lp;
    }
    let link = "Setup: README > Claude Code hooks";
    let lw = cx.tw(link);
    let lx = area.x + area.w.saturating_sub(lw) / 2;
    let hit = Hit::Hooks;
    let colour = if fr.hot(&hit) { tk.text } else { tk.accent };
    cx.text(lx, y, link, colour);
    cx.fill(Rect::new(lx, y + tk.ch, lw, tk.scale.max(1)), colour);
    fr.add(Rect::new(lx, y, lw, tk.ch + tk.sp.xs), hit);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::control::Risk;
    use crate::agents::prompt::{fixtures, parse};
    use crate::agents::ui::tests::registry_with;
    use crate::review::TurnDigest;
    use std::time::Duration;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(str::to_string).collect()
    }

    #[test]
    fn prompt_body_for_a_command() {
        let p = parse(None, &lines(fixtures::CLAUDE_BASH)).unwrap();
        let b = prompt_body(&p, &Risk::Safe, 30, false);
        assert_eq!(b[0], ("Bash command".to_string(), LineStyle::Head));
        assert_eq!(b[1], ("$ cargo test --bin rift".to_string(), LineStyle::Command));
        let always = b.iter().find(|(t, _)| t.starts_with("2 = ")).expect("scope of the always option is visible");
        assert!(always.0.chars().count() <= 30, "{always:?}");
        assert!(!b.iter().any(|(_, s)| *s == LineStyle::Danger));
    }

    #[test]
    fn prompt_body_shows_the_first_impact_when_risky() {
        let p = parse(None, &lines(fixtures::CLAUDE_RM)).unwrap();
        let risk = Risk::Critical(vec!["Deletes ~/projects/build recursively".into(), "second".into()]);
        let b = prompt_body(&p, &risk, 40, false);
        let last = b.last().unwrap();
        assert_eq!(last.1, LineStyle::Danger);
        assert!(last.0.starts_with("! Deletes"));
        assert_eq!(b.iter().filter(|(_, s)| *s == LineStyle::Danger).count(), 1);
    }

    #[test]
    fn prompt_body_for_edits_and_long_commands() {
        let p = parse(None, &lines(fixtures::CLAUDE_EDIT)).unwrap();
        let b = prompt_body(&p, &Risk::Safe, 12, false);
        assert_eq!(b[1].1, LineStyle::Path);
        assert_eq!(b[1].0, "\u{2026}gents/ui.rs", "keeps the end of the path");
        let mut p = parse(None, &lines(fixtures::CODEX_CMD)).unwrap();
        p.command = Some("a\nb\nc\nd\ne".into());
        let b = prompt_body(&p, &Risk::Safe, 30, false);
        let cmds: Vec<_> = b.iter().filter(|(_, s)| *s == LineStyle::Command).collect();
        assert_eq!(cmds.len(), MAX_COMMAND_LINES);
        assert!(cmds[MAX_COMMAND_LINES - 1].0.ends_with('\u{2026}'), "more lines than shown");
        // Codex puts its reason under the command.
        assert!(b.iter().any(|(t, _)| t.starts_with("why: ")));
    }

    #[test]
    fn answer_buttons_follow_the_agents_options() {
        let p = parse(None, &lines(fixtures::CLAUDE_BASH)).unwrap();
        assert_eq!(answer_buttons(&p), vec![(0, "Yes", "Approve"), (1, "Always", "Always"), (2, "No", "Deny")]);
        let g = parse(None, &lines(fixtures::GEMINI_SHELL)).unwrap();
        let b = answer_buttons(&g);
        assert_eq!(b.iter().map(|x| x.0).collect::<Vec<_>>(), vec![0, 1, 3], "the editor option has no button");
        let o = parse(None, &lines(fixtures::AIDER_YN)).unwrap();
        assert_eq!(answer_buttons(&o).len(), 3);
    }

    #[test]
    fn metric_items_format() {
        use crate::agents::metrics::Metrics;
        let r = registry_with(&[(1, 0, "work")]);
        let s = &r.sessions()[0];
        assert!(metric_items(s, None).is_empty());
        let mut info = PaneInfo::default();
        info.metrics = Metrics { model: Some("Opus 5.5".into()), tokens: Some(57_929), cost: Some(1.234), context_used: Some(88), reset_in: Some("4h 37m".into()) };
        info.files = (Some(3), 11);
        let items = metric_items(s, Some(&info));
        let texts: Vec<&str> = items.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(texts, vec!["Opus 5.5", "57.9k tok", "$1.23", "ctx 88%", "reset 4h 37m", "3 files (11)"]);
        assert_eq!(items[3].1, Tone::Danger, "context nearly full");
        info.files = (Some(1), 1);
        assert_eq!(metric_items(s, Some(&info)).last().unwrap().0, "1 file");
        info.files = (None, 4);
        assert_eq!(metric_items(s, Some(&info)).last().unwrap().0, "4 files");
    }

    #[test]
    fn timeline_rows() {
        use crate::review::Summary;
        let d = |running, failed, files: Option<usize>, secs| TurnDigest {
            id: 1,
            label: "Turn 1".into(),
            running,
            duration: (!running).then(|| Duration::from_secs(secs)),
            summary: files.map(|f| Summary { files: f, added: 45, removed: 12 }),
            failed,
        };
        let (tone, n, dur, text, _) = turn_row(&d(false, false, Some(3), 161), 3);
        assert_eq!((tone, n.as_str(), dur.as_str(), text.as_str()), (Tone::Success, "#3", "2m 41s", "3 files +45 -12"));
        assert_eq!(turn_row(&d(false, false, Some(0), 5), 1).3, "no changes");
        assert_eq!(turn_row(&d(false, true, None, 5), 1).0, Tone::Danger);
        let r = turn_row(&d(true, false, None, 0), 2);
        assert_eq!((r.0, r.2.as_str(), r.3.as_str()), (Tone::Accent, "running", "in progress"));
        assert_eq!(turn_row(&d(false, false, Some(1), 5), 1).3, "1 file +45 -12");
    }

    #[test]
    fn long_commands_wrap_instead_of_being_cut() {
        let mut p = parse(None, &lines(fixtures::CODEX_CMD)).unwrap();
        p.command = Some("git push --force-with-lease origin feat/rate-limit".into());
        let b = prompt_body(&p, &Risk::Safe, 30, false);
        let cmd: Vec<&str> = b.iter().filter(|(_, s)| *s == LineStyle::Command).map(|(t, _)| t.as_str()).collect();
        assert_eq!(cmd, vec!["$ git push --force-with-lease", "  origin feat/rate-limit"]);
        assert_eq!(chunk("abcdef", 4), vec!["abcd", "ef"], "long words are cut");
        assert_eq!(chunk("ab cd ef", 5), vec!["ab cd", "ef"]);
        assert_eq!(chunk("ab cdefgh", 5), vec!["ab", "cdefg", "h"]);
        assert_eq!(chunk("", 4), vec![String::new()]);
        // Impact text wraps to at most two lines.
        let r = Risk::Risky(vec!["Force-push rewrites history on origin/feat/rate-limit and more words here to overflow".into()]);
        let danger: Vec<_> = prompt_body(&p, &r, 30, false).into_iter().filter(|(_, s)| *s == LineStyle::Danger).collect();
        assert_eq!(danger.len(), 2);
        assert!(danger[1].0.ends_with('\u{2026}'));
    }

    #[test]
    fn fit_tail_keeps_the_end() {
        assert_eq!(fit_tail("src/a.rs", 20), "src/a.rs");
        assert_eq!(fit_tail("src/agents/ui.rs", 8), "\u{2026}s/ui.rs");
    }

    fn scenario() -> (AgentRegistry, AgentsUi) {
        use crate::agents::metrics::Metrics;
        let r = registry_with(&[(1, 0, "work"), (2, 0, "wait"), (3, 1, "work"), (4, 1, "wait")]);
        let mut ui = AgentsUi::new();
        ui.visible = true;
        ui.focused = true;
        ui.selected = Some(2);
        let p = parse(None, &lines(fixtures::CLAUDE_BASH)).unwrap();
        let mut waiting = PaneInfo { prompt: Some(p), ..Default::default() };
        waiting.metrics = Metrics { model: Some("Opus 5.5".into()), tokens: Some(57_929), cost: Some(0.42), context_used: Some(41), reset_in: None };
        waiting.files = (Some(2), 5);
        waiting.risk = Risk::Risky(vec!["Writes outside the project".into()]);
        waiting.turns = vec![
            TurnDigest { id: 1, label: "Turn 1".into(), running: false, duration: Some(Duration::from_secs(95)), summary: Some(crate::review::Summary { files: 3, added: 40, removed: 8 }), failed: false },
            TurnDigest { id: 2, label: "Turn 2".into(), running: true, duration: None, summary: None, failed: false },
        ];
        ui.info.insert(2, waiting);
        let raw = PaneInfo { raw_tail: vec!["Please confirm the deploy".into(), "Waiting...".into()], ..Default::default() };
        ui.info.insert(4, raw);
        (r, ui)
    }

    #[test]
    fn dock_renders_in_every_theme_and_every_mode() {
        use crate::ui::kit::gallery::qa::each_theme;
        let (r, mut ui) = scenario();
        let now = Instant::now() + Duration::from_secs(125);
        each_theme("agents-dock", |b, w, h, f, t| {
            let dock = Rect::new(0, 40, ui::dock_width(w, f.cell_width), h - 40);
            draw_dock(b, w, h, f, t, dock, &r, &mut ui, now, 0.3, "");
            ui::draw_tab_badges(b, w, h, f, t, 40, &r, 0, 3, 0.3);
            ui::draw_attention_borders(b, w, h, &[Rect::new(dock.right() + 4, 44, 400, 300)], t, 2, 0.3);
            assert!(ui.hits.iter().any(|(_, h)| matches!(h, Hit::Answer { uid: 2, option: 0 })), "approve button is clickable");
            assert!(ui.hits.iter().any(|(_, h)| *h == Hit::Edge));
        });
        // Compact density, composing with an IME preedit, every modal state, marks.
        ui.compact = true;
        ui.ctl.marked = vec![1, 2];
        for mode in [
            Mode::Browse,
            Mode::Compose { broadcast: false },
            Mode::Compose { broadcast: true },
            Mode::ConfirmSend { targets: vec![1, 3] },
            Mode::ConfirmClose(2),
            Mode::ConfirmRestart(1),
            Mode::ConfirmRisk { uid: 2, option: 0 },
            Mode::Menu { uid: 2, sel: 3 },
        ] {
            ui.ctl.mode = mode.clone();
            ui.ctl.composer.insert_str("fix the failing test");
            each_theme("agents-dock-modes", |b, w, h, f, t| {
                let dock = Rect::new(0, 40, ui::dock_width_pref(w, f.cell_width, 44), h - 40);
                draw_dock(b, w, h, f, t, dock, &r, &mut ui, now, 0.6, "\u{4f60}");
                if matches!(mode, Mode::Compose { .. }) {
                    assert!(ui.ime_rect.is_some(), "composer reports the caret for the IME");
                }
            });
        }
        let empty = AgentRegistry::new();
        let mut ui2 = AgentsUi::new();
        each_theme("agents-dock-empty", |b, w, h, f, t| {
            let dock = Rect::new(0, 40, ui::dock_width(w, f.cell_width), h - 40);
            draw_dock(b, w, h, f, t, dock, &empty, &mut ui2, now, 0.0, "");
            assert!(ui2.hits.iter().any(|(_, h)| *h == Hit::NewAgent) && ui2.hits.iter().any(|(_, h)| *h == Hit::Hooks));
        });
    }

    #[test]
    fn autopilot_countdown_and_header_switch_are_drawn_and_clickable() {
        use crate::agents::policy::{Outcome, Tool, Verdict};
        use crate::ui::kit::gallery::qa::each_theme;
        let (r, mut ui) = scenario();
        let now = Instant::now();
        ui.auto.set_global(true);
        let o = Outcome { verdict: Verdict::Approve, option: Some(0), rule: "default:cargo-checks".into(), reason: "cargo check/test".into(), subject: "cargo test --bin rift".into(), tool: Some(Tool::Bash) };
        ui.auto.consider(2, "sig", &o, now);
        ui.auto.record(1, Verdict::Approve);
        each_theme("agents-dock-autopilot", |b, w, h, f, t| {
            let dock = Rect::new(0, 40, ui::dock_width_pref(w, f.cell_width, 42), h - 40);
            draw_dock(b, w, h, f, t, dock, &r, &mut ui, now, 0.3, "");
            assert!(ui.hits.iter().any(|(_, h)| *h == Hit::AutopilotAll), "header switch");
            assert!(ui.hits.iter().any(|(_, h)| *h == Hit::Autopilot(2)), "the countdown line is clickable (stops it)");
        });
        // Switched off: a quiet card shows nothing, the selected one a hint, the header says off.
        ui.auto.set_global(false);
        each_theme("agents-dock-autopilot-off", |b, w, h, f, t| {
            let dock = Rect::new(0, 40, ui::dock_width_pref(w, f.cell_width, 34), h - 40);
            draw_dock(b, w, h, f, t, dock, &r, &mut ui, now, 0.3, "");
            assert!(ui.hits.iter().any(|(_, h)| *h == Hit::AutopilotAll));
            assert!(ui.hits.iter().any(|(_, h)| *h == Hit::Autopilot(2)), "selected card offers the switch");
            assert!(!ui.hits.iter().any(|(_, h)| *h == Hit::Autopilot(3)), "unselected quiet cards stay quiet");
        });
    }

    #[test]
    fn tiny_windows_do_not_panic() {
        let Some(mut font) = crate::ui::kit::gallery::qa::font() else { return };
        let (r, mut ui) = scenario();
        let theme = Theme::rift_neon();
        for (w, h) in [(420usize, 130usize), (300, 300), (500, 700)] {
            let mut buf = vec![0u32; w * h];
            let dock = Rect::new(0, 0, w.min(340), h);
            ui.follow = true;
            draw_dock(&mut buf, w, h, &mut font, &theme, dock, &r, &mut ui, Instant::now(), 0.0, "");
        }
    }
}
