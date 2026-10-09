//! Mission Control UI state and layout: the left-docked console, tab badges
//! and the amber pane border for agents waiting on you.
//!
//! * This file: [`AgentsUi`] (selection, scroll, density, hit map), dock
//!   geometry (width, resizable edge), the pure card layout (`rows`, `plan`),
//!   presentation helpers and the tab-bar / pane-border overlays.
//! * `dock.rs`: drawing the dock through the UI kit (`Tokens` + `Ctx`).
//! * `control.rs`: the keyboard state machine; `prompt.rs` / `metrics.rs`: what
//!   is read off the agents' screens.

use std::time::{Duration, Instant};

use super::control::{Control, Env, InfoMap, PaneInfo, Risk};
use super::{AgentRegistry, AgentSession, AgentState};
use crate::config::{Rgb, Theme};
use crate::renderer::font::FontManager;
use crate::ui::kit::{mix, Ctx, Rect, Tokens, Tone};

// ───────────────────────────── state ─────────────────────────────

/// Something clickable, recorded while drawing (the last frame's map is what
/// the mouse is tested against, so layout is never computed twice).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hit {
    Card(usize),
    Answer { uid: usize, option: usize },
    Interrupt(usize),
    Reply(usize),
    Review(usize),
    Restart(usize),
    Close(usize),
    More(usize),
    /// The workflow block of a card: opens its task queue.
    Queue(usize),
    Mark(usize),
    Turn { uid: usize, id: u64 },
    Menu { uid: usize, item: super::control::MenuItem },
    /// Click outside the open menu.
    MenuDismiss,
    Confirm(bool),
    SendReply,
    CancelReply,
    NewAgent,
    Hooks,
    Density,
    /// The draggable right edge.
    Edge,
}

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
    /// One-line cards (expanded shows metrics, files, git).
    pub compact: bool,
    /// Keyboard state machine (mode, marks, composer).
    pub ctl: Control,
    /// Per-agent prompt / metrics / timeline, refreshed by the runtime.
    pub info: InfoMap,
    /// Clickable areas of the last frame, bottom to top.
    pub hits: Vec<(Rect, Hit)>,
    /// What the pointer is over (button hover highlight).
    pub hover: Option<Hit>,
    /// Bring the selected card into view on the next frame.
    pub follow: bool,
    /// Dragging the right edge.
    pub resizing: bool,
    /// Caret rectangle of the composer in the last frame (IME candidate window).
    pub ime_rect: Option<Rect>,
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
        if !self.visible {
            self.resizing = false;
        }
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.focused = false;
        self.hover = None;
        self.resizing = false;
    }

    /// Selection resolved against the live list (falls back to the first).
    pub fn selected_index(&self, uids: &[usize]) -> Option<usize> {
        if uids.is_empty() {
            return None;
        }
        Some(self.selected.and_then(|s| uids.iter().position(|u| *u == s)).unwrap_or(0))
    }

    /// The selected uid, resolved against `uids`.
    pub fn selected_uid(&self, uids: &[usize]) -> Option<usize> {
        self.selected_index(uids).map(|i| uids[i])
    }

    /// Topmost clickable area under (x, y).
    pub fn hit_at(&self, x: usize, y: usize) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.clone())
    }

    pub fn composing(&self) -> bool {
        self.visible && self.focused && matches!(self.ctl.mode, super::control::Mode::Compose { .. })
    }
}

/// The key handler's view of the world, built from the registry and the
/// per-agent info (see [`Env`]).
pub struct DockEnv<'a> {
    pub uids: Vec<usize>,
    pub reg: &'a AgentRegistry,
    pub info: &'a InfoMap,
    pub now: Instant,
}

impl<'a> DockEnv<'a> {
    pub fn new(reg: &'a AgentRegistry, info: &'a InfoMap, now: Instant) -> Self {
        Self { uids: reg.sessions().iter().map(|s| s.pane_uid).collect(), reg, info, now }
    }
}

impl Env for DockEnv<'_> {
    fn uids(&self) -> &[usize] {
        &self.uids
    }
    fn live(&self, uid: usize) -> bool {
        self.reg.session(uid).is_some_and(|s| s.state.is_live())
    }
    fn can_interrupt(&self, uid: usize) -> bool {
        self.busy(uid)
    }
    fn answerable(&self, uid: usize) -> bool {
        self.reg.session(uid).is_some_and(|s| s.state == AgentState::WaitingForUser) && self.info.get(&uid).is_some_and(PaneInfo::answerable)
    }
    fn option_for_digit(&self, uid: usize, digit: u8) -> Option<usize> {
        self.info.get(&uid)?.prompt.as_ref()?.option_for_digit(digit)
    }
    fn grants(&self, uid: usize, option: usize) -> bool {
        self.info.get(&uid).and_then(|i| i.prompt.as_ref()).is_some_and(|p| p.grants(option))
    }
    fn risk(&self, uid: usize) -> Risk {
        self.info.get(&uid).map(|i| i.risk.clone()).unwrap_or_default()
    }
    fn busy(&self, uid: usize) -> bool {
        self.reg.session(uid).is_some_and(|s| matches!(s.state, AgentState::Working | AgentState::WaitingForUser | AgentState::Starting))
    }
    fn now(&self) -> Instant {
        self.now
    }
}

// ───────────────────────────── geometry ─────────────────────────────

/// Default dock width in character cells.
pub const DEFAULT_COLS: usize = 34;
/// Resizable range.
pub const MIN_COLS: usize = 28;
pub const MAX_COLS: usize = 80;
/// Columns that always stay for the terminal itself.
const TERMINAL_COLS: usize = 56;
/// Half-width of the draggable edge zone, in px.
pub const GRAB: usize = 4;

/// Dock width in px for a window `win_w` wide with `cw`-pixel cells and a
/// preferred width of `pref_cols` columns (0 = default); 0 = too narrow.
pub fn dock_width_pref(win_w: usize, cw: usize, pref_cols: usize) -> usize {
    let cw = cw.max(1);
    let cols = if pref_cols == 0 { DEFAULT_COLS } else { pref_cols.clamp(MIN_COLS, MAX_COLS) };
    // Keep at least ~56 columns for the terminal itself.
    let room = win_w.saturating_sub(TERMINAL_COLS * cw);
    let w = (cols * cw).min(room);
    if w < 24 * cw {
        0
    } else {
        w
    }
}

/// Dock width for a window `win_w` wide with `cw`-pixel cells; 0 = too narrow.
pub fn dock_width(win_w: usize, cw: usize) -> usize {
    dock_width_pref(win_w, cw, 0)
}

/// Dock rectangle for a window (below the tab bar, above the HUD strip);
/// `None` when the window is too narrow for it.
pub fn dock_rect(win_w: usize, win_h: usize, tab_bar_h: usize, hud_h: usize, cw: usize) -> Option<Rect> {
    dock_rect_pref(win_w, win_h, tab_bar_h, hud_h, cw, 0)
}

/// Like [`dock_rect`] with the user's preferred width (`[agents] dock_cols`).
pub fn dock_rect_pref(win_w: usize, win_h: usize, tab_bar_h: usize, hud_h: usize, cw: usize, pref_cols: usize) -> Option<Rect> {
    let dw = dock_width_pref(win_w, cw, pref_cols);
    (dw > 0).then(|| Rect::new(0, tab_bar_h, dw, win_h.saturating_sub(tab_bar_h + hud_h)))
}

/// Columns for a drag that ends at pointer x: the edge follows the pointer.
pub fn cols_for_drag(x: usize, dock_x: usize, cw: usize) -> usize {
    (x.saturating_sub(dock_x) / cw.max(1)).clamp(MIN_COLS, MAX_COLS)
}

/// The draggable strip along the dock's right edge.
pub fn edge_zone(dock: Rect) -> Rect {
    Rect::new(dock.right().saturating_sub(GRAB), dock.y, 2 * GRAB, dock.h)
}

pub struct Layout {
    pub header: Rect,
    pub list: Rect,
    pub footer: Rect,
    pub gap: usize,
}

pub fn layout(dock: Rect, tk: &Tokens) -> Layout {
    let header_h = tk.row_h + tk.sp.md;
    let footer_h = 2 * tk.row_h + tk.sp.sm;
    let (header, rest) = dock.split_top(header_h);
    let (list, footer) = rest.split_bottom(footer_h);
    Layout { header, list, footer, gap: tk.sp.sm }
}

/// Cards that fully fit from `scroll` on: `(index, rect)`. The first card is
/// always returned (callers downgrade its detail when it is taller than the list).
pub fn plan(list: Rect, gap: usize, heights: &[usize], scroll: usize) -> Vec<(usize, Rect)> {
    let mut out = Vec::new();
    let mut y = list.y;
    for (i, h) in heights.iter().enumerate().skip(scroll) {
        if y + h > list.bottom() && !out.is_empty() {
            break;
        }
        out.push((i, Rect::new(list.x, y, list.w, *h)));
        y += h + gap;
    }
    out
}

/// Largest useful scroll offset: the first card from which everything below fits.
pub fn max_scroll(list_h: usize, gap: usize, heights: &[usize]) -> usize {
    let mut used = 0;
    let mut first = heights.len();
    for (i, h) in heights.iter().enumerate().rev() {
        let need = used + h + if used > 0 { gap } else { 0 };
        if need > list_h && first < heights.len() {
            break;
        }
        used = need;
        first = i;
    }
    if heights.is_empty() { 0 } else { first }
}

/// Smallest scroll offset >= `scroll` (or the card itself) that shows card `idx` fully.
pub fn scroll_to(list: Rect, gap: usize, heights: &[usize], idx: usize, scroll: usize) -> usize {
    if idx < scroll {
        return idx;
    }
    let mut s = scroll;
    while s < idx && !plan(list, gap, heights, s).iter().any(|(i, r)| *i == idx && r.bottom() <= list.bottom()) {
        s += 1;
    }
    s
}

// ───────────────────────────── card layout (pure) ─────────────────────────────

/// How much of a card is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detail {
    /// Name and state only (tiny windows).
    Min,
    /// Compact density.
    Compact,
    /// Expanded density: place, metrics, collision hint.
    Full,
}

/// One band of a card, top to bottom. Counts are text lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Header,
    Place,
    Status,
    Metrics(usize),
    /// Same-worktree chip; `hint` adds the "use a worktree" line.
    Chips { hint: bool },
    /// The requested action box with `lines` text lines.
    Prompt(usize),
    Buttons,
    /// The prompt could not be parsed: raw last lines.
    Raw(usize),
    Confirm,
    Composer,
    Actions,
    /// Header + `n` turns.
    Timeline(usize),
    /// Workflow lines: label, queue, countdown, reviewer note.
    Workflow(usize),
}

/// What decides a card's rows.
#[derive(Clone, Copy, Debug, Default)]
pub struct CardFlags {
    pub selected: bool,
    pub waiting: bool,
    /// A parsed prompt box has this many text lines (0 = none).
    pub prompt_lines: usize,
    /// Lines of the trimmed box used by compact cards.
    pub prompt_lines_compact: usize,
    /// Unparsed prompt: raw lines to show.
    pub raw_lines: usize,
    pub metrics_lines: usize,
    pub collision: bool,
    pub confirm: bool,
    pub composer: bool,
    pub turns: usize,
    /// Text lines of the workflow block (0 = none).
    pub workflow_lines: usize,
}

pub fn rows(f: &CardFlags, detail: Detail) -> Vec<Row> {
    let mut v = vec![Row::Header];
    match detail {
        Detail::Min => v.push(Row::Status),
        Detail::Compact => {
            v.push(Row::Status);
            if f.metrics_lines > 0 {
                v.push(Row::Metrics(1));
            }
            if f.collision {
                v.push(Row::Chips { hint: false });
            }
            if f.workflow_lines > 0 {
                v.push(Row::Workflow(f.workflow_lines.min(COMPACT_WORKFLOW_LINES)));
            }
        }
        Detail::Full => {
            v.push(Row::Place);
            v.push(Row::Status);
            if f.metrics_lines > 0 {
                v.push(Row::Metrics(f.metrics_lines));
            }
            if f.collision {
                // A waiting card keeps its room for the prompt; the other collision card carries the tip.
                v.push(Row::Chips { hint: !f.waiting });
            }
            if f.workflow_lines > 0 {
                v.push(Row::Workflow(f.workflow_lines));
            }
        }
    }
    if detail != Detail::Min {
        let prompt_lines = if detail == Detail::Full { f.prompt_lines } else { f.prompt_lines_compact };
        if f.waiting && prompt_lines > 0 {
            v.push(Row::Prompt(prompt_lines));
            v.push(Row::Buttons);
        } else if f.waiting && f.raw_lines > 0 {
            v.push(Row::Raw(f.raw_lines));
        }
        if f.confirm {
            v.push(Row::Confirm);
        }
        if f.composer {
            v.push(Row::Composer);
        } else if f.selected {
            v.push(Row::Actions);
        }
        if f.selected && detail == Detail::Full && f.turns > 0 {
            v.push(Row::Timeline(f.turns.min(MAX_TURNS_SHOWN)));
        }
    }
    v
}

/// Workflow lines kept on compact cards.
pub const COMPACT_WORKFLOW_LINES: usize = 3;

/// Turns listed in a card's timeline.
pub const MAX_TURNS_SHOWN: usize = 4;

/// Line pitch of text rows.
pub fn line_pitch(tk: &Tokens) -> usize {
    tk.ch + tk.sp.xs
}

/// Height of the small action buttons (Interrupt / Reply / ...).
pub fn action_h(tk: &Tokens) -> usize {
    tk.ch + tk.sp.md
}

pub fn row_h(r: Row, tk: &Tokens) -> usize {
    let lp = line_pitch(tk);
    match r {
        Row::Header => tk.ch + tk.sp.sm,
        Row::Place => lp,
        Row::Status => tk.ch + tk.sp.xs,
        Row::Metrics(n) => n * lp,
        Row::Chips { hint } => tk.ch + tk.sp.xs + if hint { lp } else { 0 },
        Row::Prompt(n) => n * lp + 2 * tk.sp.sm,
        Row::Buttons => action_h(tk) + tk.sp.xs,
        Row::Raw(n) => n * lp,
        Row::Confirm => action_h(tk) + tk.sp.xs,
        Row::Composer => tk.input_h,
        Row::Actions => action_h(tk),
        Row::Timeline(n) => (n + 1) * lp,
        Row::Workflow(n) => n * lp,
    }
}

/// Inner padding of a card.
pub fn card_pad(tk: &Tokens) -> usize {
    tk.sp.sm
}

pub fn rows_gap(tk: &Tokens) -> usize {
    tk.sp.xs
}

pub fn card_height(rows: &[Row], tk: &Tokens) -> usize {
    let body: usize = rows.iter().map(|r| row_h(*r, tk)).sum();
    body + rows.len().saturating_sub(1) * rows_gap(tk) + 2 * card_pad(tk)
}

/// Pick the richest detail level whose card height fits `max_h`.
pub fn fit_detail(f: &CardFlags, want: Detail, max_h: usize, tk: &Tokens) -> (Detail, usize) {
    let order = [Detail::Full, Detail::Compact, Detail::Min];
    let start = order.iter().position(|d| *d == want).unwrap_or(0);
    for d in &order[start..] {
        let h = card_height(&rows(f, *d), tk);
        if h <= max_h || *d == Detail::Min {
            return (*d, h);
        }
    }
    unreachable!()
}

/// A card with the detail level and rows it will be drawn with.
#[derive(Clone, Copy, Debug)]
pub struct Fitted {
    pub detail: Detail,
    pub flags: CardFlags,
    pub height: usize,
}

/// Decide how much of every card to show so that, when possible, all cards fit
/// the list at once. Optional content goes first: the turn timeline, then the
/// other cards' extras (they turn compact), then the selected card's own.
/// With too many agents for any of that, cards keep their wanted detail and
/// the list scrolls.
pub fn fit_cards(flags: &[CardFlags], selected: Option<usize>, want: Detail, list_h: usize, gap: usize, tk: &Tokens) -> Vec<Fitted> {
    let build = |sel_detail: Detail, other: Detail, timeline: bool| -> Vec<Fitted> {
        flags
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let detail = if Some(i) == selected { sel_detail } else { other };
                let mut f = *f;
                if !timeline {
                    f.turns = 0;
                }
                Fitted { detail, flags: f, height: card_height(&rows(&f, detail), tk) }
            })
            .collect()
    };
    let total = |v: &[Fitted]| v.iter().map(|f| f.height).sum::<usize>() + gap * v.len().saturating_sub(1);
    let compact = if want == Detail::Min { Detail::Min } else { Detail::Compact };
    let attempts = [(want, want, true), (want, want, false), (want, compact, false), (compact, compact, false)];
    for (sel, other, timeline) in attempts {
        let v = build(sel, other, timeline);
        if total(&v) <= list_h {
            return v;
        }
    }
    // Scrolling regime: wanted detail, and a card taller than the list is downgraded.
    build(want, want, true)
        .into_iter()
        .map(|f| {
            let (detail, height) = fit_detail(&f.flags, f.detail, list_h, tk);
            Fitted { detail, flags: f.flags, height }
        })
        .collect()
}

/// Greedy line breaking for metric items (`cols` characters per line, items
/// separated by `sep` columns). Items longer than a line get a line of their own.
pub fn flow(widths: &[usize], cols: usize, sep: usize) -> Vec<Vec<usize>> {
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut used = 0;
    for (i, w) in widths.iter().enumerate() {
        match lines.last_mut() {
            Some(line) if used + sep + w <= cols => {
                line.push(i);
                used += sep + w;
            }
            _ => {
                lines.push(vec![i]);
                used = *w;
            }
        }
    }
    lines
}

/// Wrap `text` to `cols` columns on spaces (long words are cut).
pub fn wrap(text: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        while word.chars().count() > cols {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            out.push(word.chars().take(cols).collect());
            word = word.chars().skip(cols).collect();
        }
        if cur.is_empty() {
            cur = word;
        } else if cur.chars().count() + 1 + word.chars().count() <= cols {
            cur.push(' ');
            cur.push_str(&word);
        } else {
            out.push(std::mem::replace(&mut cur, word));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
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

/// Precise duration for turn rows: 42s, 2m 41s, 1h 02m.
pub fn format_turn(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
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

/// Directory name of the work tree an agent runs in, when it is a linked
/// worktree (differs from the repository's name).
pub fn worktree_dir(s: &AgentSession) -> Option<String> {
    let root = s.git_root.as_deref()?;
    let dir = std::path::Path::new(root).file_name()?.to_string_lossy().into_owned();
    match &s.repo {
        Some(repo) if *repo == dir => None,
        _ => Some(dir),
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

// ───────────────────────────── tab badges & pane borders ─────────────────────────────

fn dot(cx: &mut Ctx, x: usize, y: usize, d: usize, c: Rgb, alpha: u8) {
    cx.fill_rrect_ex(Rect::new(x, y, d, d), d / 2, c, alpha, crate::ui::kit::draw::ALL);
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

/// Selection / hover fill helper shared with the dock drawing.
pub fn card_bg(tk: &Tokens, selected: bool, hover: bool) -> Rgb {
    if selected {
        tk.selection
    } else if hover {
        tk.surface_alt
    } else {
        mix(tk.surface, tk.surface_alt, 0.35)
    }
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
pub(crate) mod tests {
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
        assert_eq!(format_turn(Duration::from_secs(161)), "2m 41s");
        assert_eq!(format_turn(Duration::from_secs(7)), "7s");
        assert_eq!(format_turn(Duration::from_secs(3720)), "1h 02m");
    }

    #[test]
    fn dock_width_rules() {
        assert_eq!(dock_width(2000, 10), 340);
        assert_eq!(dock_width(1000, 10), 340.min(1000 - 560));
        assert_eq!(dock_width(700, 10), 0, "too narrow: hidden rather than squeezing the terminal");
        assert_eq!(dock_width(0, 10), 0);
    }

    #[test]
    fn preferred_width_is_clamped_and_never_starves_the_terminal() {
        assert_eq!(dock_width_pref(2000, 10, 0), 340, "0 = default");
        assert_eq!(dock_width_pref(2000, 10, 50), 500);
        assert_eq!(dock_width_pref(2000, 10, 10), 280, "min width");
        assert_eq!(dock_width_pref(2000, 10, 500), 800, "max width");
        assert_eq!(dock_width_pref(1000, 10, 60), 440, "56 columns stay for the terminal");
        assert_eq!(dock_width_pref(700, 10, 60), 0);
        assert_eq!(dock_rect_pref(2000, 1000, 40, 0, 10, 50).unwrap(), Rect::new(0, 40, 500, 960));
        assert_eq!(dock_rect(2000, 1000, 40, 60, 10).unwrap(), Rect::new(0, 40, 340, 900));
    }

    #[test]
    fn edge_drag_maps_pointer_to_columns() {
        assert_eq!(cols_for_drag(450, 0, 10), 45);
        assert_eq!(cols_for_drag(50, 0, 10), MIN_COLS);
        assert_eq!(cols_for_drag(5000, 0, 10), MAX_COLS);
        let z = edge_zone(Rect::new(0, 40, 340, 500));
        assert!(z.contains(339, 100) && z.contains(343, 100) && !z.contains(330, 100));
    }

    #[test]
    fn toggling_focuses_when_shown() {
        let mut ui = AgentsUi::new();
        ui.toggle();
        assert!(ui.visible && ui.focused);
        ui.resizing = true;
        ui.toggle();
        assert!(!ui.visible && !ui.focused && !ui.resizing);
    }

    #[test]
    fn stale_selection_falls_back_to_the_first() {
        let mut ui = AgentsUi::new();
        ui.selected = Some(99);
        assert_eq!(ui.selected_index(&[1, 2]), Some(0));
        assert_eq!(ui.selected_uid(&[7, 2]), Some(7));
        assert_eq!(ui.selected_index(&[]), None);
    }

    #[test]
    fn hits_resolve_topmost_first() {
        let mut ui = AgentsUi::new();
        ui.hits.push((Rect::new(0, 0, 100, 100), Hit::Card(1)));
        ui.hits.push((Rect::new(10, 10, 20, 20), Hit::Reply(1)));
        assert_eq!(ui.hit_at(15, 15), Some(Hit::Reply(1)));
        assert_eq!(ui.hit_at(60, 60), Some(Hit::Card(1)));
        assert_eq!(ui.hit_at(200, 200), None);
    }

    fn flags() -> CardFlags {
        CardFlags { metrics_lines: 2, ..Default::default() }
    }

    #[test]
    fn rows_grow_with_content_and_never_lose_the_header() {
        let tk = tk();
        let base = rows(&flags(), Detail::Full);
        assert_eq!(base, vec![Row::Header, Row::Place, Row::Status, Row::Metrics(2)]);
        let compact = rows(&flags(), Detail::Compact);
        assert_eq!(compact, vec![Row::Header, Row::Status, Row::Metrics(1)], "metrics survive compaction, on one line");
        assert!(card_height(&compact, &tk) < card_height(&base, &tk));

        let waiting = CardFlags { waiting: true, prompt_lines: 6, prompt_lines_compact: 4, selected: true, collision: true, turns: 6, ..flags() };
        let full = rows(&waiting, Detail::Full);
        assert_eq!(
            full,
            vec![Row::Header, Row::Place, Row::Status, Row::Metrics(2), Row::Chips { hint: false }, Row::Prompt(6), Row::Buttons, Row::Actions, Row::Timeline(MAX_TURNS_SHOWN)]
        );
        // A collision card that is not waiting carries the worktree tip.
        let idle = CardFlags { collision: true, ..flags() };
        assert!(rows(&idle, Detail::Full).contains(&Row::Chips { hint: true }));
        // Compact keeps a trimmed prompt (it must stay answerable) but drops the timeline.
        let c = rows(&waiting, Detail::Compact);
        assert!(c.contains(&Row::Prompt(4)) && c.contains(&Row::Buttons) && !c.iter().any(|r| matches!(r, Row::Timeline(_))));
        // Min is just header + status.
        assert_eq!(rows(&waiting, Detail::Min), vec![Row::Header, Row::Status]);
        // Composer replaces the action row; confirm sits above it.
        let comp = CardFlags { selected: true, composer: true, confirm: true, ..flags() };
        let r = rows(&comp, Detail::Compact);
        assert_eq!(r, vec![Row::Header, Row::Status, Row::Metrics(1), Row::Confirm, Row::Composer]);
    }

    #[test]
    fn heights_are_the_sum_of_rows_plus_gaps_and_padding() {
        let tk = tk();
        let r = vec![Row::Header, Row::Status];
        let want = row_h(Row::Header, &tk) + row_h(Row::Status, &tk) + rows_gap(&tk) + 2 * card_pad(&tk);
        assert_eq!(card_height(&r, &tk), want);
        assert_eq!(card_height(&[], &tk), 2 * card_pad(&tk));
    }

    #[test]
    fn fit_detail_downgrades_instead_of_overflowing() {
        let tk = tk();
        let f = CardFlags { waiting: true, prompt_lines: 5, prompt_lines_compact: 3, selected: true, turns: 4, collision: true, ..flags() };
        let (d, h) = fit_detail(&f, Detail::Full, 10_000, &tk);
        assert_eq!(d, Detail::Full);
        let (d2, h2) = fit_detail(&f, Detail::Full, h - 1, &tk);
        assert_eq!(d2, Detail::Compact);
        assert!(h2 < h);
        let (d3, h3) = fit_detail(&f, Detail::Full, 10, &tk);
        assert_eq!(d3, Detail::Min, "never below Min, even if it still does not fit");
        assert!(h3 > 10);
        assert_eq!(fit_detail(&f, Detail::Compact, 10_000, &tk).0, Detail::Compact);
    }

    #[test]
    fn fit_cards_drops_optional_content_before_hiding_agents() {
        let tk = tk();
        let sel = CardFlags { selected: true, waiting: true, prompt_lines: 6, prompt_lines_compact: 4, turns: 5, collision: true, metrics_lines: 2, ..Default::default() };
        let other = CardFlags { metrics_lines: 2, ..Default::default() };
        let flags = [sel, other, other];
        let full = fit_cards(&flags, Some(0), Detail::Full, 100_000, 8, &tk);
        assert!(full.iter().all(|f| f.detail == Detail::Full));
        assert_eq!(full[0].flags.turns, 5, "plenty of room: the timeline stays");
        let total = |v: &[Fitted]| v.iter().map(|f| f.height).sum::<usize>() + 8 * (v.len() - 1);

        // Slightly too tall: the timeline goes first.
        let tight = fit_cards(&flags, Some(0), Detail::Full, total(&full) - 1, 8, &tk);
        assert_eq!(tight[0].flags.turns, 0);
        assert!(tight.iter().all(|f| f.detail == Detail::Full));
        assert!(total(&tight) <= total(&full) - 1);

        // Tighter: the other cards turn compact, the selected one keeps its prompt.
        let tighter = fit_cards(&flags, Some(0), Detail::Full, total(&tight) - 1, 8, &tk);
        assert_eq!(tighter[0].detail, Detail::Full);
        assert_eq!((tighter[1].detail, tighter[2].detail), (Detail::Compact, Detail::Compact));
        assert!(rows(&tighter[0].flags, tighter[0].detail).contains(&Row::Buttons));

        // Tighter still: everything compact, prompt still answerable.
        let all_c = fit_cards(&flags, Some(0), Detail::Full, total(&tighter) - 1, 8, &tk);
        assert!(all_c.iter().all(|f| f.detail == Detail::Compact));
        assert!(rows(&all_c[0].flags, Detail::Compact).contains(&Row::Buttons));

        // Far too many agents: keep the wanted detail and scroll; a lone tall card is downgraded.
        let many = vec![other; 20];
        let s = fit_cards(&many, Some(0), Detail::Full, 300, 8, &tk);
        assert!(s.iter().all(|f| f.detail == Detail::Full));
        let big = fit_cards(&[sel], Some(0), Detail::Full, 60, 8, &tk);
        assert_eq!(big[0].detail, Detail::Min);
        assert!(fit_cards(&[], None, Detail::Full, 100, 8, &tk).is_empty());
    }

    #[test]
    fn plan_scroll_and_follow() {
        let list = Rect::new(0, 100, 300, 300);
        let heights = [100, 100, 100, 100, 100];
        let p = plan(list, 10, &heights, 0);
        assert_eq!(p.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![0, 1], "100+10+100 fits, a third does not");
        assert_eq!(p[1].1.y, 210);
        assert_eq!(plan(list, 10, &heights, 3).len(), 2);
        // A card taller than the list is still returned (and downgraded by the caller).
        assert_eq!(plan(Rect::new(0, 0, 10, 50), 5, &[500], 0).len(), 1);
        assert_eq!(max_scroll(300, 10, &heights), 3);
        assert_eq!(max_scroll(300, 10, &[100]), 0);
        assert_eq!(max_scroll(300, 10, &[]), 0);
        // Following the selection moves the window only as far as needed.
        assert_eq!(scroll_to(list, 10, &heights, 1, 0), 0);
        assert_eq!(scroll_to(list, 10, &heights, 2, 0), 1);
        assert_eq!(scroll_to(list, 10, &heights, 4, 0), 3);
        assert_eq!(scroll_to(list, 10, &heights, 0, 3), 0);
        // Variable heights.
        let mixed = [250, 60, 60, 60];
        assert_eq!(scroll_to(list, 10, &mixed, 3, 0), 1, "250 + 60 + 60 overflow with gaps; cards 1..3 fit");
    }

    #[test]
    fn flow_breaks_lines_greedily() {
        assert_eq!(flow(&[8, 9, 6], 30, 3), vec![vec![0, 1, 2]]);
        assert_eq!(flow(&[8, 9, 12], 30, 3), vec![vec![0, 1], vec![2]]);
        assert_eq!(flow(&[40], 30, 3), vec![vec![0]], "overlong item keeps its own line");
        assert!(flow(&[], 30, 3).is_empty());
    }

    #[test]
    fn wrap_breaks_on_spaces_and_cuts_long_words() {
        assert_eq!(wrap("add the file to the chat please", 12), vec!["add the file", "to the chat", "please"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert!(wrap("   ", 10).is_empty());
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

    pub(crate) fn registry_with(states: &[(usize, usize, &str)]) -> AgentRegistry {
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
    fn dock_env_reports_answerable_only_for_waiting_agents_with_a_prompt() {
        use crate::agents::prompt;
        let r = registry_with(&[(1, 0, "work"), (2, 0, "wait")]);
        let mut info = InfoMap::new();
        let screen: Vec<String> = prompt::fixtures::CLAUDE_BASH.lines().map(str::to_string).collect();
        let p = prompt::parse(None, &screen).unwrap();
        for uid in [1, 2] {
            info.insert(uid, PaneInfo { prompt: Some(p.clone()), ..Default::default() });
        }
        let env = DockEnv::new(&r, &info, Instant::now());
        assert!(!env.answerable(1), "working agent: a stale prompt is not answerable");
        assert!(env.answerable(2));
        assert_eq!(env.option_for_digit(2, 2), Some(1));
        assert!(env.grants(2, 1) && !env.grants(2, 2));
        assert!(env.busy(1) && env.live(2) && !env.live(99));
    }

    #[test]
    fn pulse_is_bounded() {
        for i in 0..100 {
            let p = pulse(i as f32 * 0.07, 0.9);
            assert!((0.0..=1.0).contains(&p));
        }
    }

    #[test]
    fn worktree_dir_only_for_linked_worktrees() {
        let r = registry_with(&[(1, 0, "work")]);
        let mut s = r.sessions()[0].clone();
        s.repo = Some("aurora".into());
        s.git_root = Some("/Users/maya/dev/aurora".into());
        assert_eq!(worktree_dir(&s), None);
        s.git_root = Some("/Users/maya/dev/aurora-claude-1".into());
        assert_eq!(worktree_dir(&s).as_deref(), Some("aurora-claude-1"));
        s.git_root = None;
        assert_eq!(worktree_dir(&s), None);
    }

    #[test]
    fn ellipsize_used_for_titles() {
        assert_eq!(crate::ui::kit::ellipsize("Claude Code", 5), "Clau\u{2026}");
    }
}
