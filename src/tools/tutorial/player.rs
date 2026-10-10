//! Tutorial playback: a private "world" (scripted panes, tabs, block UI,
//! inline-AI state, palette / preview / panels) rebuilt from a [`Cast`], and
//! the [`Player`] state machine (play / pause / speed / step / scrub).
//!
//! Like Time Warp, playback shows terminal *frames* instead of the live
//! panes. Seeking is exact: the world is rebuilt from t=0 and the events up
//! to the target are replayed through the real VT parser (demos are a few
//! KB, so this takes well under a millisecond).
//!
//! **Nothing here can reach a shell.** Every pane in the world is
//! [`Pane::scripted`] (`PtyKind::Inert`: reads yield nothing, writes are
//! dropped), terminal replies to queries are discarded, and the player has
//! no reference to the user's panes.

use std::collections::HashMap;

use super::cast::{AiOp, BlocksOp, Cast, Event, Kind, Mark, PaneOp, Panel, TabOp, UiOp};
use crate::ai::inline::context::{BlockSnap, BlockSource, Resolved};
use crate::ai::inline::fix::FixSuggestion;
use crate::ai::inline::line_edit::LineEdit;
use crate::ai::inline::{ActiveFix, BlockSig, InlineAi, NlGhost, NlState, Popover, PopoverMode};
use crate::blocks_ui::view::Button;
use crate::blocks_ui::{BlocksUi, Hover};
use crate::tools::blocks::BlockManager;
use crate::tools::command_palette::{CommandPalette, History, PaletteContext, PaletteKey};
use crate::tools::exec_preview::ExecPreview;
use crate::window::tab::{SplitDir, Tab};
use crate::window::{Pane, PaneRect, WindowManager};

/// How long a key-press overlay stays up (demo seconds).
pub const KEY_TTL: f64 = 1.8;
/// Allowed playback speeds.
pub const SPEEDS: [u32; 3] = [1, 2, 4];

// ───────────────────────────── world ─────────────────────────────

/// Everything a demo frame shows. Mirrors the parts of `App` that
/// `lifecycle::redraw` reads (same types), but owns only scripted panes.
pub struct World {
    pub wm: WindowManager,
    /// The legacy heuristic block manager the renderer signature wants.
    pub blocks: BlockManager,
    pub blocks_ui: BlocksUi,
    pub inline_ai: InlineAi,
    pub palette: Option<CommandPalette>,
    pub preview: Option<ExecPreview>,
    pub panel: Option<Panel>,
    pub caption: String,
    /// A left-placed panel docks like Mission Control: it takes this strip
    /// of the stage and the panes shrink beside it (same geometry as the app).
    pub dock: Option<crate::ui::kit::Rect>,
    /// Key overlay: (reference, label, demo time it was pressed).
    pub key: Option<(String, String, f64)>,
    cols: usize,
    rows: usize,
    cell: (usize, usize),
    /// Pane id -> demo time its running block started (OSC 133;C).
    block_start: HashMap<usize, f64>,
}

impl World {
    pub fn new(cols: usize, rows: usize, cell: (usize, usize)) -> Self {
        let mut wm = WindowManager::headless(cols, rows);
        wm.tabs[0].title = "zsh".into();
        wm.tabs[0].custom_title = true;
        let mut w = Self {
            wm,
            blocks: BlockManager::new(),
            blocks_ui: BlocksUi::new(),
            inline_ai: InlineAi::new(),
            palette: None,
            preview: None,
            panel: None,
            caption: String::new(),
            dock: None,
            key: None,
            cols,
            rows,
            cell: (cell.0.max(1), cell.1.max(1)),
            block_start: HashMap::new(),
        };
        w.layout();
        w
    }

    pub fn tab_bar_h(&self) -> usize {
        self.cell.1 + 16
    }

    /// Pane area inside the stage (below the demo's tab bar, beside the dock).
    pub fn content_area(&self) -> PaneRect {
        let dock = self.dock.map_or(0, |r| r.w);
        PaneRect { x: dock, y: self.tab_bar_h(), width: (self.cols * self.cell.0).saturating_sub(dock), height: self.rows * self.cell.1 }
    }

    /// Stage size in pixels (tab bar + panes + dock).
    pub fn stage_size(&self) -> (usize, usize) {
        (self.cols * self.cell.0, self.tab_bar_h() + self.rows * self.cell.1)
    }

    fn set_dock(&mut self, on: bool) {
        let (sw, sh) = self.stage_size();
        let dock = if on { crate::agents::ui::dock_rect_pref(sw, sh, self.tab_bar_h(), 0, self.cell.0, 0) } else { None };
        if dock != self.dock {
            self.dock = dock;
            self.layout();
        }
    }

    fn layout(&mut self) {
        let area = self.content_area();
        self.wm.resize_to(self.cell.0, self.cell.1, area);
    }

    fn pane_ids(&self) -> (usize, usize) {
        (self.wm.active_tab, self.wm.active_tab().active)
    }

    pub fn apply(&mut self, e: &Event) {
        match &e.kind {
            Kind::Output(bytes) => self.feed(e.t, bytes),
            Kind::Mark(m) => self.mark(e.t, m),
        }
    }

    fn feed(&mut self, t: f64, bytes: &[u8]) {
        let pane = self.wm.active_pane_mut();
        let before = pane.terminal.blocks.running_osc_elapsed_ms().is_some();
        pane.feed(bytes);
        // Replies to terminal queries (DA, DSR, ...) have nowhere to go.
        pane.terminal.response_queue.clear();
        let after = pane.terminal.blocks.running_osc_elapsed_ms().is_some();
        let id = pane.id;
        if !before && after {
            self.block_start.insert(id, t);
        }
        if before && !after {
            // Durations come from the demo clock, not from how fast we parse.
            let start = self.block_start.remove(&id).unwrap_or(t);
            if let Some(b) = pane.terminal.blocks.blocks_mut().last_mut() {
                if !b.running {
                    b.duration_ms = ((t - start).max(0.0) * 1000.0).round() as u64;
                }
            }
        }
    }

    fn new_pane(&mut self) -> Pane {
        let id = self.wm.alloc_pane_id();
        Pane::scripted(id, self.cols, self.rows)
    }

    fn last_block_snap(&self) -> Option<BlockSnap> {
        let t = &self.wm.active_pane().terminal;
        let b = t.blocks.blocks().last()?;
        Some(BlockSnap { command: b.command.clone(), exit_code: b.exit_code, output: String::new(), cwd: t.cwd.clone(), running: b.running, line: b.command_line })
    }

    fn mark(&mut self, t: f64, m: &Mark) {
        match m {
            Mark::Caption(c) => self.caption = c.clone(),
            Mark::Key(k, l) => self.key = Some((k.clone(), l.clone(), t)),
            Mark::Pane(op) => {
                match op {
                    PaneOp::SplitRight | PaneOp::SplitDown => {
                        let dir = if matches!(op, PaneOp::SplitRight) { SplitDir::Horizontal } else { SplitDir::Vertical };
                        let p = self.new_pane();
                        self.wm.active_tab_mut().split(dir, p);
                    }
                    PaneOp::Focus(i) => {
                        let tab = self.wm.active_tab_mut();
                        if *i < tab.pane_count() {
                            tab.focus_pane(*i);
                        }
                    }
                    PaneOp::Zoom => {
                        self.wm.active_tab_mut().toggle_zoom();
                    }
                }
                self.layout();
            }
            Mark::Tab(op) => {
                match op {
                    TabOp::New(title) => {
                        let p = self.new_pane();
                        let mut tab = Tab::new(p);
                        tab.title = title.clone();
                        tab.custom_title = true;
                        self.wm.tabs.push(tab);
                        self.wm.active_tab = self.wm.tabs.len() - 1;
                    }
                    TabOp::Select(i) => {
                        if *i < self.wm.tabs.len() {
                            self.wm.active_tab = *i;
                        }
                    }
                    TabOp::Title(title) => {
                        let tab = self.wm.active_tab_mut();
                        tab.title = title.clone();
                        tab.custom_title = true;
                    }
                }
                self.layout();
            }
            Mark::Blocks(op) => self.blocks_op(op),
            Mark::Ai(op) => self.ai_op(op),
            Mark::Ui(op) => match op {
                UiOp::Clear => {
                    self.palette = None;
                    self.preview = None;
                    self.panel = None;
                    self.set_dock(false);
                }
                UiOp::Panel(p) => {
                    self.set_dock(p.place == super::cast::Place::Left);
                    self.panel = Some(p.clone());
                }
                UiOp::Palette(q) => {
                    // No history file: playback never reads or writes the user's frecency.
                    let mut pal = CommandPalette::with_parts(History::default(), None);
                    pal.open(PaletteContext {
                        tabs: self.wm.tabs.iter().map(|t| t.title.clone()).collect(),
                        active_tab: self.wm.active_tab,
                        font_size: 14.0,
                        ..Default::default()
                    });
                    for c in q.chars() {
                        pal.handle_key(PaletteKey::Char(c));
                    }
                    self.palette = Some(pal);
                }
                UiOp::Preview(cmd, typed) => {
                    self.preview = ExecPreview::check_static(cmd).map(|mut p| {
                        p.visible = true;
                        p.confirm_input = typed.clone();
                        p
                    });
                }
            },
        }
    }

    fn blocks_op(&mut self, op: &BlocksOp) {
        let (tab, pane) = self.pane_ids();
        match op {
            BlocksOp::Fold(i) => self.wm.active_pane_mut().terminal.blocks.toggle_collapse(*i),
            BlocksOp::Hover(i, b) => {
                let button = b.as_deref().and_then(|b| match b {
                    "copy-cmd" => Some(Button::CopyCmd),
                    "copy-output" => Some(Button::CopyOutput),
                    "ask" => Some(Button::AskAi),
                    "rerun" => Some(Button::Rerun),
                    "collapse" => Some(Button::Collapse),
                    _ => None,
                });
                self.blocks_ui.hover = Some(Hover { tab, pane, block: *i, button });
            }
            BlocksOp::Select(i) => self.blocks_ui.selected = Some((tab, pane, *i)),
            BlocksOp::Jump(i) => {
                // Same arithmetic as `blocks_ui::jump_block`.
                let t = &mut self.wm.active_pane_mut().terminal;
                if let Some(line) = t.blocks.blocks().get(*i).map(|b| b.command_line) {
                    let folds = crate::blocks_ui::view::folds_of(&t.blocks);
                    let total = t.scrollback.len() + t.grid.len();
                    let off = crate::blocks_ui::view::scroll_offset_for_top(total, t.rows.min(t.grid.len()), &folds, line);
                    t.scroll_offset = off.min(t.scrollback.len());
                }
            }
            BlocksOp::Bottom => self.wm.active_pane_mut().terminal.scroll_to_bottom(),
            BlocksOp::Clear => {
                self.blocks_ui.hover = None;
                self.blocks_ui.selected = None;
            }
        }
    }

    fn ai_op(&mut self, op: &AiOp) {
        let pane_id = self.wm.active_pane().id;
        match op {
            AiOp::Clear => {
                self.inline_ai.fix.current = None;
                self.inline_ai.nl = NlState::Idle;
                self.inline_ai.popover = None;
            }
            AiOp::Fix(cmd, expl) => {
                if let Some(snap) = self.last_block_snap() {
                    let sig = BlockSig::of(pane_id, &self.wm.active_pane().terminal);
                    self.inline_ai.fix.current = Some(ActiveFix {
                        suggestion: FixSuggestion { command: cmd.clone(), explanation: expl.clone() },
                        dangerous: ExecPreview::check_static(cmd).is_some_and(|p| p.severity != crate::tools::exec_preview::Severity::Info),
                        sig,
                        snap,
                    });
                }
            }
            AiOp::Nl(query, cmd) => {
                self.inline_ai.nl = NlState::Ghost(NlGhost { query: query.clone(), command: cmd.clone(), dangerous: false, pane_id });
            }
            AiOp::Ask(q) => {
                if let Some(snap) = self.last_block_snap() {
                    self.inline_ai.popover = Some(Popover { edit: LineEdit::with_text(q), resolved: Resolved::Block { snap, source: BlockSource::Last }, mode: PopoverMode::Ask });
                }
            }
        }
    }
}

// ───────────────────────────── player ─────────────────────────────

/// Keys the player understands (decoded from winit by `app::overlays`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TKey {
    Space,
    Left,
    Right,
    ShiftLeft,
    ShiftRight,
    Up,
    Down,
    Enter,
    Esc,
    Home,
    End,
    Char(char),
}

/// What the caller should do after a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    None,
    /// Close the player; the user's terminal comes back as it was.
    Exit,
    /// Play the next bundled demo.
    Next,
    /// Back to the tutorial list.
    List,
}

pub struct Player {
    pub cast: Cast,
    pub title: String,
    /// Bundled demo id (None for a user recording).
    pub id: Option<&'static str>,
    world: World,
    applied: usize,
    pos: f64,
    pub playing: bool,
    pub speed: u32,
    cell: (usize, usize),
    steps: Vec<f64>,
}

impl Player {
    pub fn new(cast: Cast, title: String, id: Option<&'static str>, cell: (usize, usize)) -> Self {
        let world = World::new(cast.cols, cast.rows, cell);
        let steps = cast.steps();
        let mut p = Self { cast, title, id, world, applied: 0, pos: 0.0, playing: true, speed: 1, cell, steps };
        p.apply_until(0.0);
        p
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    pub fn pos(&self) -> f64 {
        self.pos
    }

    pub fn duration(&self) -> f64 {
        self.cast.duration()
    }

    pub fn finished(&self) -> bool {
        self.pos >= self.duration() - 1e-9
    }

    pub fn steps(&self) -> &[f64] {
        &self.steps
    }

    /// (current step, 1-based; 0 before the first caption), total steps.
    pub fn step_index(&self) -> (usize, usize) {
        (self.steps.iter().filter(|s| **s <= self.pos + 1e-9).count(), self.steps.len())
    }

    pub fn caption(&self) -> &str {
        &self.world.caption
    }

    /// Key overlay currently shown: (reference, label).
    pub fn key_overlay(&self) -> Option<(&str, &str)> {
        self.world
            .key
            .as_ref()
            .filter(|(_, _, t)| self.pos - t < KEY_TTL)
            .map(|(k, l, _)| (k.as_str(), l.as_str()))
    }

    /// Events applied so far (for tests).
    pub fn applied(&self) -> usize {
        self.applied
    }

    fn apply_until(&mut self, t: f64) {
        while let Some(e) = self.cast.events.get(self.applied) {
            if e.t > t + 1e-9 {
                break;
            }
            self.world.apply(e);
            self.applied += 1;
        }
    }

    fn rebuild(&mut self) {
        self.world = World::new(self.cast.cols, self.cast.rows, self.cell);
        self.applied = 0;
        self.pos = 0.0;
    }

    /// Wall-clock time passed (seconds); scaled by the speed.
    pub fn advance(&mut self, dt: f64) {
        if !self.playing || dt <= 0.0 {
            return;
        }
        let end = self.duration();
        self.pos = (self.pos + dt * self.speed as f64).min(end);
        let p = self.pos;
        self.apply_until(p);
        if self.pos >= end {
            self.playing = false;
        }
    }

    /// Jump to `t` (clamped). Going backwards rebuilds the world.
    pub fn seek(&mut self, t: f64) {
        let t = t.clamp(0.0, self.duration());
        let behind = self.applied > 0 && self.cast.events[self.applied - 1].t > t + 1e-9;
        if t < self.pos - 1e-9 || behind {
            self.rebuild();
        }
        self.apply_until(t);
        self.pos = t;
    }

    pub fn restart(&mut self) {
        self.rebuild();
        self.apply_until(0.0);
        self.playing = true;
    }

    pub fn toggle_pause(&mut self) {
        if self.finished() {
            self.restart();
        } else {
            self.playing = !self.playing;
        }
    }

    pub fn set_speed(&mut self, s: u32) {
        if SPEEDS.contains(&s) {
            self.speed = s;
        }
    }

    pub fn cycle_speed(&mut self) {
        let i = SPEEDS.iter().position(|s| *s == self.speed).unwrap_or(0);
        self.speed = SPEEDS[(i + 1) % SPEEDS.len()];
    }

    /// Next caption (or the end).
    pub fn step_next(&mut self) {
        let next = self.steps.iter().copied().find(|s| *s > self.pos + 0.05).unwrap_or(self.duration());
        self.seek(next);
    }

    /// Start of the current caption; if it just started, the previous one.
    pub fn step_prev(&mut self) {
        let done: Vec<f64> = self.steps.iter().copied().filter(|s| *s <= self.pos + 1e-9).collect();
        let target = match done.as_slice() {
            [] => 0.0,
            [.., prev, cur] if self.pos - cur < 1.0 => *prev,
            [cur] if self.pos - cur < 1.0 => 0.0,
            [.., cur] => *cur,
        };
        self.seek(target);
    }

    pub fn scrub(&mut self, delta: f64) {
        self.seek(self.pos + delta);
    }

    /// The font changed: lay the world out again at the new cell size.
    pub fn set_cell(&mut self, cell: (usize, usize)) {
        if cell != self.cell && cell.0 > 0 && cell.1 > 0 {
            self.cell = cell;
            let pos = self.pos;
            self.rebuild();
            self.apply_until(pos);
            self.pos = pos;
        }
    }

    pub fn handle(&mut self, key: TKey) -> Outcome {
        match key {
            TKey::Esc | TKey::Char('q') => return Outcome::Exit,
            TKey::Char('n') => return Outcome::Next,
            TKey::Char('l') => return Outcome::List,
            TKey::Space | TKey::Enter if !matches!(key, TKey::Enter) || self.finished() => self.toggle_pause(),
            TKey::Left | TKey::Up => self.step_prev(),
            TKey::Right | TKey::Down => self.step_next(),
            TKey::ShiftLeft => self.scrub(-5.0),
            TKey::ShiftRight => self.scrub(5.0),
            TKey::Char('1') => self.set_speed(1),
            TKey::Char('2') => self.set_speed(2),
            TKey::Char('4') => self.set_speed(4),
            TKey::Char('s') => self.cycle_speed(),
            TKey::Char('r') | TKey::Home => self.restart(),
            TKey::End => self.seek(self.duration()),
            _ => {}
        }
        Outcome::None
    }
}
