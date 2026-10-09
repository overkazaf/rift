//! Mission Control UI: the left-docked agent list, tab badges and the amber
//! pane border for agents waiting on you. All drawing goes through the UI kit
//! (`Tokens` + `Ctx`); geometry and navigation are pure and tested.

use std::time::{Duration, Instant};

use super::{AgentKind, AgentSession, AgentState, AgentRegistry};
use crate::config::{Rgb, Theme};
use crate::renderer::font::FontManager;
use crate::ui::kit::{mix, Ctx, Rect, Tokens, Tone};

// ───────────────────────────── state ─────────────────────────────

#[derive(Default)]
pub struct AgentsUi {
    /// Dock shown.
    pub visible: bool,
    /// The dock owns the keyboard.
    pub focused: bool,
    /// Selected session (pane uid).
    pub selected: Option<usize>,
    /// First visible card.
    pub scroll: usize,
    /// Card under the pointer (index into the session list).
    pub hover: Option<usize>,
}

/// What a key press in the dock asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiAction {
    /// Consumed, nothing else to do (selection moved).
    None,
    /// Jump to this pane.
    Jump(usize),
    /// Cycle to the next agent that needs the user.
    NextAttention,
    /// Give the keyboard back to the terminal (dock stays open).
    Blur,
    /// Not a dock key: unfocus and let the terminal have it.
    PassThrough,
}

/// Keys the dock understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockKey {
    Up,
    Down,
    Home,
    End,
    Enter,
    Escape,
    Char(char),
    /// A modifier chord (Cmd/Ctrl): never ours.
    Chord,
}

impl AgentsUi {
    pub fn new() -> Self {
        Self::default()
    }

    /// Show / hide. Showing focuses the dock so the keyboard works at once.
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        self.focused = self.visible;
        self.hover = None;
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.focused = false;
        self.hover = None;
    }

    /// Selection resolved against the live list (falls back to the first).
    pub fn selected_index(&self, uids: &[usize]) -> Option<usize> {
        if uids.is_empty() {
            return None;
        }
        Some(self.selected.and_then(|s| uids.iter().position(|u| *u == s)).unwrap_or(0))
    }

    /// Move the selection and handle keys. `uids` is the session list order.
    pub fn on_key(&mut self, key: DockKey, uids: &[usize]) -> UiAction {
        let n = uids.len();
        let cur = self.selected_index(uids);
        let select = |ui: &mut AgentsUi, i: usize| ui.selected = uids.get(i).copied();
        match key {
            DockKey::Chord => UiAction::PassThrough,
            DockKey::Escape => UiAction::Blur,
            DockKey::Up | DockKey::Char('k') => {
                if let Some(i) = cur {
                    select(self, if i == 0 { n - 1 } else { i - 1 });
                }
                UiAction::None
            }
            DockKey::Down | DockKey::Char('j') => {
                if let Some(i) = cur {
                    select(self, (i + 1) % n);
                }
                UiAction::None
            }
            DockKey::Home => {
                select(self, 0);
                UiAction::None
            }
            DockKey::End => {
                if n > 0 {
                    select(self, n - 1);
                }
                UiAction::None
            }
            DockKey::Enter => cur.map_or(UiAction::None, |i| UiAction::Jump(uids[i])),
            DockKey::Char('n') => UiAction::NextAttention,
            DockKey::Char(c) if c.is_ascii_digit() && c != '0' => {
                let i = c as usize - '1' as usize;
                if i < n {
                    UiAction::Jump(uids[i])
                } else {
                    UiAction::None
                }
            }
            DockKey::Char(_) => UiAction::PassThrough,
        }
    }
}

// ───────────────────────────── geometry ─────────────────────────────

/// Dock width for a window `win_w` wide with `cw`-pixel cells; 0 = too narrow.
pub fn dock_width(win_w: usize, cw: usize) -> usize {
    let cw = cw.max(1);
    // Keep at least ~56 columns for the terminal itself.
    let room = win_w.saturating_sub(56 * cw);
    let w = (34 * cw).min(room);
    if w < 24 * cw {
        0
    } else {
        w
    }
}

pub struct Layout {
    pub header: Rect,
    pub list: Rect,
    pub footer: Rect,
    pub card_h: usize,
    pub gap: usize,
}

pub fn layout(dock: Rect, tk: &Tokens) -> Layout {
    let header_h = tk.row_h + tk.sp.md;
    let footer_h = tk.row_h + tk.sp.sm;
    let (header, rest) = dock.split_top(header_h);
    let (list, footer) = rest.split_bottom(footer_h);
    Layout { header, list, footer, card_h: 3 * tk.ch + 3 * tk.sp.sm, gap: tk.sp.sm }
}

impl Layout {
    fn stride(&self) -> usize {
        self.card_h + self.gap
    }

    /// Whole cards that fit in the list.
    pub fn visible(&self) -> usize {
        ((self.list.h + self.gap) / self.stride().max(1)).max(1)
    }

    /// Largest useful scroll offset (in cards).
    pub fn max_scroll(&self, n: usize) -> usize {
        n.saturating_sub(self.visible())
    }

    /// Card rectangle, or `None` when scrolled out of view.
    pub fn card(&self, idx: usize, scroll: usize) -> Option<Rect> {
        let rel = idx.checked_sub(scroll)?;
        (rel < self.visible()).then(|| Rect::new(self.list.x, self.list.y + rel * self.stride(), self.list.w, self.card_h))
    }

    /// Card at (x, y) given the scroll offset (in cards).
    pub fn hit(&self, n: usize, scroll: usize, x: usize, y: usize) -> Option<usize> {
        if !self.list.contains(x, y) {
            return None;
        }
        let rel = y - self.list.y;
        let (i, off) = (rel / self.stride(), rel % self.stride());
        let idx = scroll + i;
        (idx < n && i < self.visible() && off < self.card_h).then_some(idx)
    }

    /// Scroll offset that keeps card `idx` in view.
    pub fn scroll_to(&self, idx: usize, scroll: usize) -> usize {
        crate::ui::kit::scroll_into_view(idx, scroll, self.visible())
    }
}

// ───────────────────────────── presentation helpers ─────────────────────────────

/// Compact duration: 42s, 5m, 1h02m.
pub fn format_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// Elapsed time worth showing for a session.
pub fn elapsed_for(s: &AgentSession, now: Instant) -> Duration {
    let from = match s.state {
        AgentState::Working => s.last_turn_started_at.unwrap_or(s.state_since),
        _ => s.state_since,
    };
    now.saturating_duration_since(from)
}

pub fn state_label(st: AgentState) -> &'static str {
    match st {
        AgentState::Starting => "starting",
        AgentState::Working => "working",
        AgentState::WaitingForUser => "needs you",
        AgentState::Idle => "idle",
        AgentState::Done { .. } => "done",
        AgentState::Error => "error",
    }
}

pub fn state_tone(st: AgentState) -> Tone {
    match st {
        AgentState::Working => Tone::Accent,
        AgentState::WaitingForUser => Tone::Warning,
        AgentState::Done { .. } => Tone::Success,
        AgentState::Error => Tone::Danger,
        AgentState::Starting | AgentState::Idle => Tone::Neutral,
    }
}

/// Agents per tab: (live count, any waiting). Index = tab index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TabBadge {
    pub count: usize,
    pub attention: bool,
}

pub fn tab_badges(reg: &AgentRegistry, tab_count: usize) -> Vec<TabBadge> {
    let mut v = vec![TabBadge::default(); tab_count];
    for s in reg.sessions().iter().filter(|s| s.state.is_live()) {
        if let Some(b) = v.get_mut(s.tab_index) {
            b.count += 1;
            b.attention |= s.needs_attention;
        }
    }
    v
}

/// 0..1 pulse with period `period` seconds.
pub fn pulse(t: f32, period: f32) -> f32 {
    0.5 + 0.5 * (t * std::f32::consts::TAU / period.max(0.1)).sin()
}

// ───────────────────────────── drawing ─────────────────────────────

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

fn dot(cx: &mut Ctx, x: usize, y: usize, d: usize, c: Rgb, alpha: u8) {
    cx.fill_rrect_ex(Rect::new(x, y, d, d), d / 2, c, alpha, crate::ui::kit::draw::ALL);
}

/// Pill with an animated look: returns its width.
fn pill(cx: &mut Ctx, x: usize, y: usize, h: usize, s: &AgentSession, t: f32) -> usize {
    let tk = cx.tk;
    let tone = state_tone(s.state);
    let fg = tk.tone(tone);
    let label = state_label(s.state);
    let glyph_w = match s.state {
        AgentState::Done { .. } | AgentState::Error => tk.ch * 7 / 10 + tk.sp.xs,
        _ => 0,
    };
    let w = cx.tw(label) + glyph_w + 2 * tk.sp.sm;
    let r = Rect::new(x, y, w, h);
    let fill_alpha = match s.state {
        AgentState::Working => 40 + (pulse(t, 1.2) * 60.0) as u8,
        AgentState::WaitingForUser => 60 + (pulse(t, 0.9) * 130.0) as u8,
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

fn kind_chip(cx: &mut Ctx, x: usize, y: usize, size: usize, kind: AgentKind) {
    let c = kind.color();
    cx.fill_rrect(Rect::new(x, y, size, size), cx.tk.radius_sm, c);
    // Dark or light glyph, whichever reads better on the brand colour.
    let ink = if crate::ui::kit::luminance(c) > 0.35 { (20, 20, 24) } else { (250, 250, 252) };
    let gx = x + size.saturating_sub(cx.tk.cw) / 2;
    let gy = y + size.saturating_sub(cx.tk.ch) / 2;
    cx.text(gx, gy, &kind.glyph().to_string(), ink);
}

fn card(cx: &mut Ctx, r: Rect, s: &AgentSession, selected: bool, hover: bool, focused: bool, now: Instant, t: f32) {
    let tk = cx.tk;
    let fg = tk.tone(state_tone(s.state));
    let bg = if selected { tk.selection } else if hover { tk.surface_alt } else { tk.surface };
    cx.fill_rrect(r, tk.radius_sm, bg);
    if s.state == AgentState::WaitingForUser {
        // Breathing amber edge: this is the card that needs a human.
        let a = 90 + (pulse(t, 0.9) * 150.0) as u32;
        cx.stroke_rrect(r, tk.radius_sm, tk.scale.max(1), mix(bg, tk.warning, a as f32 / 255.0));
    } else if selected && focused {
        cx.stroke_rrect(r, tk.radius_sm, tk.scale.max(1), tk.accent);
    }
    // State stripe.
    let stripe = Rect::new(r.x + tk.scale, r.y + tk.sp.xs, 3 * tk.scale, r.h.saturating_sub(2 * tk.sp.xs));
    cx.fill_rrect(stripe, tk.scale + 1, fg);

    let pad = tk.sp.md;
    let left = r.x + pad + 3 * tk.scale;
    let right = r.right().saturating_sub(tk.sp.sm);
    let line_h = tk.ch + tk.sp.sm;
    let y0 = r.y + tk.sp.xs;

    // Line 1: icon, name, elapsed.
    let chip = tk.ch + tk.sp.xs;
    kind_chip(cx, left, y0, chip, s.kind);
    let el = format_elapsed(elapsed_for(s, now));
    let el_w = cx.tw(&el);
    cx.text(right.saturating_sub(el_w), y0 + (chip - tk.ch) / 2, &el, tk.text_muted);
    let name_x = left + chip + tk.sp.sm;
    cx.text_fit(name_x, y0 + (chip - tk.ch) / 2, right.saturating_sub(name_x + el_w + tk.sp.sm), &s.title, tk.text);

    // Line 2: repo/branch (or directory) and tab number.
    let y1 = y0 + line_h + tk.sp.xs;
    let place = s.place();
    let tab = format!("tab {}", s.tab_index + 1);
    let tab_w = cx.tw(&tab);
    cx.text(right.saturating_sub(tab_w), y1, &tab, tk.text_faint);
    cx.text_fit(left, y1, right.saturating_sub(left + tab_w + tk.sp.sm), &place, tk.text_muted);

    // Line 3: state pill + output preview.
    let y2 = y1 + line_h;
    let ph = tk.ch + tk.sp.xs;
    let pw = pill(cx, left, y2, ph, s, t);
    let px = left + pw + tk.sp.sm;
    let preview = match (&s.state, &s.waiting_reason) {
        (AgentState::WaitingForUser, Some(r)) => r.clone(),
        _ => s.preview.clone(),
    };
    cx.text_fit(px, y2 + (ph - tk.ch) / 2, right.saturating_sub(px), &preview, tk.text_faint);
}

/// Draw the dock into `buf`. `dock` is its rectangle (from `Layout` inputs).
pub fn draw_dock(
    buf: &mut [u32],
    w: usize,
    h: usize,
    font: &mut FontManager,
    theme: &Theme,
    dock: Rect,
    reg: &AgentRegistry,
    ui: &mut AgentsUi,
    now: Instant,
    t: f32,
) {
    let (cw, ch) = (font.cell_width, font.cell_height);
    let tk = Tokens::new(theme, cw, ch);
    let mut cx = Ctx::new(buf, w, h, font, &tk);
    let tk = cx.tk;
    // Panel + right edge.
    cx.fill(dock, mix(tk.bg, tk.surface, 0.6));
    cx.vline(dock.right().saturating_sub(1), dock.y, dock.h, tk.border);

    let l = layout(dock, tk);
    let sessions = reg.sessions();
    let uids: Vec<usize> = sessions.iter().map(|s| s.pane_uid).collect();
    let sel = ui.selected_index(&uids);
    ui.scroll = ui.scroll.min(l.max_scroll(sessions.len()));

    // Header: title + counts.
    let hx = dock.x + tk.sp.md;
    let title_y = cx.text_y(l.header.y, l.header.h);
    cx.text(hx, title_y, "AGENTS", tk.accent);
    let attention = reg.attention_count();
    let live = reg.live_count();
    let mut rx = dock.right().saturating_sub(tk.sp.md + 1);
    if attention > 0 {
        let label = format!("{attention} needs you");
        let bw = cx.badge_w(&label);
        cx.badge(rx.saturating_sub(bw), l.header.y, &label, Tone::Warning, l.header.h);
        rx = rx.saturating_sub(bw + tk.sp.sm);
    }
    if live > 0 && rx > hx + 12 * tk.cw {
        let label = format!("{live} live");
        let bw = cx.badge_w(&label);
        if rx.saturating_sub(bw) > hx + 8 * tk.cw {
            cx.badge(rx.saturating_sub(bw), l.header.y, &label, Tone::Neutral, l.header.h);
        }
    }
    cx.hline(dock.x, l.header.bottom().saturating_sub(1), dock.w.saturating_sub(1), tk.border);

    // Cards (clipped to the list by skipping those fully outside).
    if sessions.is_empty() {
        cx.empty_state(l.list, "No agents running", "Run claude, codex, gemini ... in any pane");
    } else {
        let pad_x = tk.sp.sm;
        for (i, s) in sessions.iter().enumerate() {
            let Some(mut r) = l.card(i, ui.scroll) else { continue };
            r.x += pad_x;
            r.w = r.w.saturating_sub(2 * pad_x + 1);
            card(&mut cx, r, s, Some(i) == sel, ui.hover == Some(i), ui.focused, now, t);
        }
        cx.scrollbar(l.list, sessions.len(), l.visible(), ui.scroll);
    }

    // Footer hints.
    cx.hline(dock.x, l.footer.y, dock.w.saturating_sub(1), tk.border);
    cx.hint_row(
        dock.x + tk.sp.md,
        l.footer.y,
        dock.w.saturating_sub(2 * tk.sp.md),
        l.footer.h,
        &[("Enter", "jump"), ("n", "next"), ("Esc", "back")],
    );
}

/// Badges on the tab bar: an accent dot + count, amber and pulsing when any
/// agent in the tab needs you. Drawn into the left padding of each tab.
pub fn draw_tab_badges(buf: &mut [u32], w: usize, h: usize, font: &mut FontManager, theme: &Theme, tab_bar_h: usize, reg: &AgentRegistry, tab_count: usize, t: f32) {
    let badges = tab_badges(reg, tab_count);
    if badges.iter().all(|b| b.count == 0) {
        return;
    }
    let (cw, ch) = (font.cell_width, font.cell_height);
    let tk = Tokens::new(theme, cw, ch);
    let mut cx = Ctx::new(buf, w, h, font, &tk);
    let tk = cx.tk;
    let slots = crate::ui::tabbar::layout(w, tab_count, tab_bar_h).tabs;
    for (i, b) in badges.iter().enumerate() {
        let Some(&(x0, x1)) = slots.get(i) else { break };
        if b.count == 0 || x1.saturating_sub(x0) < 10 * cw {
            continue;
        }
        let colour = if b.attention { tk.warning } else { tk.accent };
        let d = (ch / 2).max(6);
        let y = tab_bar_h.saturating_sub(d) / 2;
        let alpha = if b.attention { 140 + (pulse(t, 0.9) * 115.0) as u8 } else { 255 };
        let x = x0 + cw / 2;
        dot(&mut cx, x, y, d, colour, alpha);
        let label = if b.count > 9 { "9+".to_string() } else { b.count.to_string() };
        cx.text(x + d + cw / 4, tab_bar_h.saturating_sub(ch) / 2, &label, colour);
    }
}

/// Amber frame around panes whose agent waits for you.
pub fn draw_attention_borders(buf: &mut [u32], w: usize, h: usize, rects: &[Rect], theme: &Theme, thickness: usize, t: f32) {
    let c = theme_amber(theme);
    let a = 110 + (pulse(t, 0.9) * 140.0) as u32;
    for r in rects {
        let th = thickness.max(1).min(r.w / 2).min(r.h / 2);
        let bands = [
            Rect::new(r.x, r.y, r.w, th),
            Rect::new(r.x, r.bottom().saturating_sub(th), r.w, th),
            Rect::new(r.x, r.y, th, r.h),
            Rect::new(r.right().saturating_sub(th), r.y, th, r.h),
        ];
        for b in bands {
            for y in b.y..b.bottom().min(h) {
                for x in b.x..b.right().min(w) {
                    let i = y * w + x;
                    if i < buf.len() {
                        buf[i] = crate::ui::kit::draw::blend_px(buf[i], c, a);
                    }
                }
            }
        }
    }
}

fn theme_amber(theme: &Theme) -> Rgb {
    let tk = Tokens::new(theme, 8, 16);
    tk.warning
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::registry::PaneObs;

    fn tk() -> Tokens {
        Tokens::new(&Theme::rift_neon(), 9, 18)
    }

    #[test]
    fn elapsed_formatting() {
        assert_eq!(format_elapsed(Duration::from_secs(0)), "0s");
        assert_eq!(format_elapsed(Duration::from_secs(59)), "59s");
        assert_eq!(format_elapsed(Duration::from_secs(60)), "1m");
        assert_eq!(format_elapsed(Duration::from_secs(3599)), "59m");
        assert_eq!(format_elapsed(Duration::from_secs(3720)), "1h02m");
    }

    #[test]
    fn dock_width_rules() {
        assert_eq!(dock_width(2000, 10), 340);
        assert_eq!(dock_width(1000, 10), 340.min(1000 - 560));
        assert_eq!(dock_width(700, 10), 0, "too narrow: hidden rather than squeezing the terminal");
        assert_eq!(dock_width(0, 10), 0);
    }

    #[test]
    fn navigation_wraps_and_jumps() {
        let uids = [10, 20, 30];
        let mut ui = AgentsUi::new();
        assert_eq!(ui.selected_index(&uids), Some(0));
        assert_eq!(ui.on_key(DockKey::Down, &uids), UiAction::None);
        assert_eq!(ui.selected, Some(20));
        ui.on_key(DockKey::Char('j'), &uids);
        assert_eq!(ui.selected, Some(30));
        ui.on_key(DockKey::Down, &uids);
        assert_eq!(ui.selected, Some(10), "wraps");
        ui.on_key(DockKey::Up, &uids);
        assert_eq!(ui.selected, Some(30), "wraps backwards");
        assert_eq!(ui.on_key(DockKey::Enter, &uids), UiAction::Jump(30));
        ui.on_key(DockKey::Home, &uids);
        assert_eq!(ui.selected, Some(10));
        ui.on_key(DockKey::End, &uids);
        assert_eq!(ui.selected, Some(30));
        assert_eq!(ui.on_key(DockKey::Char('2'), &uids), UiAction::Jump(20));
        assert_eq!(ui.on_key(DockKey::Char('9'), &uids), UiAction::None);
        assert_eq!(ui.on_key(DockKey::Char('n'), &uids), UiAction::NextAttention);
        assert_eq!(ui.on_key(DockKey::Escape, &uids), UiAction::Blur);
        assert_eq!(ui.on_key(DockKey::Char('x'), &uids), UiAction::PassThrough);
        assert_eq!(ui.on_key(DockKey::Chord, &uids), UiAction::PassThrough);
    }

    #[test]
    fn navigation_with_no_sessions_is_safe() {
        let mut ui = AgentsUi::new();
        for k in [DockKey::Up, DockKey::Down, DockKey::Home, DockKey::End, DockKey::Enter] {
            assert_eq!(ui.on_key(k, &[]), UiAction::None);
        }
        assert_eq!(ui.selected_index(&[]), None);
    }

    #[test]
    fn stale_selection_falls_back_to_the_first() {
        let mut ui = AgentsUi::new();
        ui.selected = Some(99);
        assert_eq!(ui.selected_index(&[1, 2]), Some(0));
    }

    #[test]
    fn toggling_focuses_when_shown() {
        let mut ui = AgentsUi::new();
        ui.toggle();
        assert!(ui.visible && ui.focused);
        ui.toggle();
        assert!(!ui.visible && !ui.focused);
    }

    #[test]
    fn layout_hit_testing_and_scrolling() {
        let tk = tk();
        let dock = Rect::new(0, 40, 340, 500);
        let l = layout(dock, &tk);
        assert!(l.list.h > 0 && l.list.y > dock.y);
        assert_eq!(l.list.bottom() + l.footer.h, dock.bottom());
        let vis = l.visible();
        assert!(vis >= 2, "a 500px dock shows several cards");
        let n = vis + 5;
        // First card is hit in its middle, not in the gap below it.
        let c0 = l.card(0, 0).unwrap();
        assert_eq!(l.hit(n, 0, 20, c0.y + c0.h / 2), Some(0));
        assert_eq!(l.hit(n, 0, 20, c0.bottom() + l.gap / 2), None);
        let c1 = l.card(1, 0).unwrap();
        assert_eq!(l.hit(n, 0, 20, c1.y + 1), Some(1));
        // Outside the list.
        assert_eq!(l.hit(n, 0, 20, l.header.y), None);
        assert_eq!(l.hit(n, 0, 400, c0.y + 1), None);
        // Beyond the last card.
        assert_eq!(l.hit(1, 0, 20, c1.y + 1), None);
        // Scrolling moves which card sits at the top.
        assert_eq!(l.hit(n, 2, 20, c0.y + 2), Some(2));
        assert!(l.card(0, 2).is_none(), "scrolled out of view");
        assert!(l.card(vis + 1, 2).is_some());
        assert!(l.card(vis + 2, 2).is_none(), "below the fold");
        assert_eq!(l.max_scroll(n), 5);
        assert_eq!(l.max_scroll(1), 0);
        // scroll_to keeps the selection visible.
        assert_eq!(l.scroll_to(n - 1, 0), n - vis);
        assert_eq!(l.scroll_to(0, 3), 0);
        assert_eq!(l.scroll_to(1, 0), 0, "already visible");
    }

    #[test]
    fn tones_and_labels() {
        assert_eq!(state_tone(AgentState::WaitingForUser), Tone::Warning);
        assert_eq!(state_tone(AgentState::Working), Tone::Accent);
        assert_eq!(state_tone(AgentState::Done { exit: Some(0) }), Tone::Success);
        assert_eq!(state_tone(AgentState::Error), Tone::Danger);
        assert_eq!(state_label(AgentState::WaitingForUser), "needs you");
        assert_eq!(state_label(AgentState::Done { exit: None }), "done");
    }

    fn registry_with(states: &[(usize, usize, &str)]) -> AgentRegistry {
        // (pane uid, tab index, "wait" | "work")
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        for (uid, tab, what) in states {
            let o = PaneObs { uid: *uid, tab_index: *tab, osc_seen: true, block_running: true, running_cmd: Some("claude".into()), ..Default::default() };
            r.observe_pane(&o, &mut || crate::agents::registry::Probe::Unknown, t0);
            if *what == "wait" {
                r.observe_notification(*uid, "", "needs your permission", t0);
            }
        }
        r
    }

    #[test]
    fn tab_badges_count_live_agents_and_attention() {
        let r = registry_with(&[(1, 0, "work"), (2, 0, "wait"), (3, 2, "work")]);
        let b = tab_badges(&r, 4);
        assert_eq!(b[0], TabBadge { count: 2, attention: true });
        assert_eq!(b[1], TabBadge::default());
        assert_eq!(b[2], TabBadge { count: 1, attention: false });
        assert_eq!(b[3], TabBadge::default());
        // A session on a tab that no longer exists is ignored, not a panic.
        let b = tab_badges(&r, 1);
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn pulse_is_bounded() {
        for i in 0..100 {
            let p = pulse(i as f32 * 0.07, 0.9);
            assert!((0.0..=1.0).contains(&p));
        }
    }

    #[test]
    fn dock_tab_badges_and_borders_render_in_every_theme() {
        use crate::ui::kit::gallery::qa::each_theme;
        let r = registry_with(&[(1, 0, "work"), (2, 0, "wait"), (3, 1, "work"), (4, 1, "wait")]);
        let mut ui = AgentsUi::new();
        ui.visible = true;
        ui.selected = Some(2);
        let now = Instant::now() + Duration::from_secs(125);
        each_theme("agents-dock", |b, w, h, f, t| {
            let dock = Rect::new(0, 40, dock_width(w, f.cell_width), h - 40);
            draw_dock(b, w, h, f, t, dock, &r, &mut ui, now, 0.3);
            draw_tab_badges(b, w, h, f, t, 40, &r, 3, 0.3);
            draw_attention_borders(b, w, h, &[Rect::new(dock.right() + 4, 44, 400, 300)], t, 2, 0.3);
        });
        let empty = AgentRegistry::new();
        each_theme("agents-dock-empty", |b, w, h, f, t| {
            let dock = Rect::new(0, 40, dock_width(w, f.cell_width), h - 40);
            draw_dock(b, w, h, f, t, dock, &empty, &mut ui, now, 0.0);
        });
    }

    #[test]
    fn ellipsize_used_for_titles() {
        assert_eq!(crate::ui::kit::ellipsize("Claude Code", 5), "Clau\u{2026}");
    }
}
