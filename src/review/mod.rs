//! Change Review: see exactly what a coding agent changed, per turn, and
//! accept or revert it.
//!
//! * **Checkpoints** are git *tree objects* of the whole working tree (tracked
//!   and untracked files, `.gitignore` respected), written through a temporary
//!   index. They never touch the user's index, branches, stash or working tree.
//!   Nothing references them, so they are dangling objects that `git gc`
//!   removes once its prune expiry (default 2 weeks) passes. A checkpoint is
//!   therefore only a few KB of tree objects plus blobs of files that changed.
//! * **Turns**: [`TurnEvent::Started`] snapshots the repo, [`TurnEvent::Finished`]
//!   snapshots again and diffs the two (summary + chip). The palette's
//!   "Review: Mark Checkpoint" adds a manual checkpoint for any pane.
//! * **Overlay** ([`ui`]): turns list + files list + diff view, with revert
//!   (confirmed, repo-confined), accept, copy patch and "ask AI".
//! * **Safety**: reverts only ever write or delete paths inside the repository
//!   root (see [`git::safe_target`]), ask for confirmation first, and save a
//!   "Before revert" checkpoint of the working tree so they can be undone.
//!
//! All git work runs on one background worker thread with per-command
//! timeouts; the UI thread only exchanges messages ([`Review::poll`]).
//!
//! # Integration
//! [`on_turn_event`] is the single entry point for agent turn boundaries.
//! `on_agent_event` (bottom of this file) maps the agent registry's events
//! onto it; wire the registry's `drain_events()` into it from `about_to_wait`.

pub mod diff;
pub mod git;
mod ui;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::app::App;
use crate::ui::kit::{Rect, Tone};
use diff::ParsedDiff;
use git::RevertOp;

/// Rows of a file's diff shown before "show more".
pub const ROW_LIMIT: usize = 1500;
/// Rows added per "show more".
pub const ROW_STEP: usize = 4000;
/// Turns remembered per pane.
const MAX_TURNS: usize = 200;
const TOAST_FOR: Duration = Duration::from_secs(4);

// ───────────────────────── model ─────────────────────────

/// A snapshot of the repo's working tree.
#[derive(Clone, Debug)]
#[allow(dead_code)] // identity / provenance fields, kept for the revert log and tests
pub struct Checkpoint {
    pub id: u64,
    pub pane_uid: usize,
    pub repo_root: PathBuf,
    /// Git tree object name.
    pub tree: String,
    pub at: Instant,
    /// Agent turn number (0 for manual checkpoints).
    pub turn: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TurnKind {
    Agent,
    Manual,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Summary {
    pub files: usize,
    pub added: usize,
    pub removed: usize,
}

impl Summary {
    pub fn of(d: &ParsedDiff) -> Summary {
        let (files, added, removed) = d.totals();
        Summary { files, added, removed }
    }

    pub fn short(&self) -> String {
        format!("+{} -{}", self.added, self.removed)
    }
}

/// One entry of a pane's timeline: a checkpoint plus (for finished agent
/// turns) the tree at the end of the turn and a change summary.
#[derive(Clone, Debug)]
pub struct Turn {
    pub id: u64,
    pub kind: TurnKind,
    pub label: String,
    /// `None` while the snapshot is still being taken (or failed: see `error`).
    pub start: Option<Checkpoint>,
    pub end_tree: Option<String>,
    pub finished: bool,
    pub summary: Option<Summary>,
    pub error: Option<String>,
    pub at: Instant,
}

#[derive(Default, Debug)]
pub struct PaneLog {
    pub turns: Vec<Turn>,
    agent_turns: u32,
}

impl PaneLog {
    fn push(&mut self, t: Turn) {
        self.turns.push(t);
        if self.turns.len() > MAX_TURNS {
            self.turns.remove(0);
        }
    }

    fn turn_mut(&mut self, id: u64) -> Option<&mut Turn> {
        self.turns.iter_mut().find(|t| t.id == id)
    }
}

/// "3 files changed" chip on a pane.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Chip {
    pub summary: Summary,
    pub label: String,
}

/// What a turn event reports about where the agent works.
#[derive(Clone, Debug)]
pub enum TurnEvent {
    Started { pane: usize, cwd: Option<String>, git_root: Option<String> },
    Finished { pane: usize, cwd: Option<String>, git_root: Option<String> },
    Exited { pane: usize },
}

// ───────────────────────── view state ─────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Focus {
    #[default]
    Files,
    Turns,
}

/// The range being shown: `base` tree to `target` tree (`None` = working tree now).
#[derive(Clone, Debug)]
pub struct Range {
    pub repo: PathBuf,
    pub base: String,
    pub target: Option<String>,
    pub label: String,
}

#[derive(Default)]
pub enum ViewState {
    #[default]
    Idle,
    Message(String, String),
    Loading,
    Error(String),
    Ready(Box<ParsedDiff>),
}

#[derive(Default)]
pub struct ReviewUi {
    pub visible: bool,
    pub pane: usize,
    /// 0 = "all since start", `i` = turn `i - 1`.
    pub sel_pos: usize,
    pub focus: Focus,
    pub gen: u64,
    pub view: ViewState,
    pub range: Option<Range>,
    pub file: usize,
    pub scroll: usize,
    pub limit: usize,
    pub file_scroll: usize,
    pub turn_scroll: usize,
    /// Diff rows visible at the last render (page size).
    pub page: usize,
}

impl ReviewUi {
    fn ready(&self) -> Option<&ParsedDiff> {
        match &self.view {
            ViewState::Ready(d) => Some(d),
            _ => None,
        }
    }

    fn current_file(&self) -> Option<&diff::FileDiff> {
        self.ready().and_then(|d| d.files.get(self.file))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReviewKey {
    Escape,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Tab,
    Char(char),
}

/// Destructive action awaiting the user's confirmation.
pub struct RevertPlan {
    pub pane: usize,
    pub repo: PathBuf,
    pub base: String,
    pub ops: Vec<RevertOp>,
    pub label: String,
}

// ───────────────────────── worker ─────────────────────────

enum Job {
    Snapshot { req: u64, dir: PathBuf },
    Diff { req: u64, repo: PathBuf, base: String, target: Option<String> },
    Revert { req: u64, repo: PathBuf, base: String, ops: Vec<RevertOp> },
}

struct DiffDone {
    parsed: ParsedDiff,
    #[allow(dead_code)]
    target_tree: String,
}

enum Msg {
    Snapshot { req: u64, result: Result<(PathBuf, String), String> },
    Diff { req: u64, result: Result<DiffDone, String> },
    Reverted { req: u64, result: Result<(usize, Option<String>), String> },
}

struct Worker {
    tx: Sender<Job>,
    rx: Receiver<Msg>,
}

impl Worker {
    fn spawn() -> Worker {
        let (tx, jobs) = channel::<Job>();
        let (out, rx) = channel::<Msg>();
        std::thread::Builder::new()
            .name("rift-review".into())
            .spawn(move || {
                for job in jobs {
                    let msg = run_job(job);
                    if out.send(msg).is_err() {
                        break;
                    }
                    crate::wake::wake();
                }
            })
            .ok();
        Worker { tx, rx }
    }
}

fn run_job(job: Job) -> Msg {
    match job {
        Job::Snapshot { req, dir } => {
            let result = git::repo_root(&dir).and_then(|root| git::snapshot_tree(&root).map(|t| (root, t)));
            Msg::Snapshot { req, result }
        }
        Job::Diff { req, repo, base, target } => {
            let result = (|| {
                let target_tree = match target {
                    Some(t) => t,
                    None => git::snapshot_tree(&repo)?,
                };
                let out = git::diff_trees(&repo, &base, &target_tree)?;
                Ok(DiffDone { parsed: diff::parse_unified(&out.text(), out.truncated), target_tree })
            })();
            Msg::Diff { req, result }
        }
        Job::Revert { req, repo, base, ops } => {
            // Safety net first: a checkpoint of what is about to be overwritten.
            let safety = git::snapshot_tree(&repo).ok();
            let result = git::revert(&repo, &base, &ops).map(|n| (n, safety));
            Msg::Reverted { req, result }
        }
    }
}

enum Pending {
    TurnStart { pane: usize, turn: u64 },
    TurnEnd { pane: usize, turn: u64 },
    TurnSummary { pane: usize, turn: u64 },
    Mark { pane: usize },
    Accept { pane: usize },
    View { gen: u64 },
    Revert { pane: usize },
}

// ───────────────────────── state ─────────────────────────

struct Toast {
    msg: String,
    tone: Tone,
    until: Instant,
}

#[derive(Default)]
pub struct Review {
    pub logs: HashMap<usize, PaneLog>,
    pub chips: HashMap<usize, Chip>,
    pub ui: ReviewUi,
    worker: Option<Worker>,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    toast: Option<Toast>,
    /// Chip rectangles from the last frame (mouse hit-testing).
    chip_rects: Vec<(usize, Rect)>,
    last_prune: Option<Instant>,
}

impl Review {
    fn fresh_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn submit(&mut self, mk: impl FnOnce(u64) -> Job, p: Pending) {
        let req = self.fresh_id();
        let w = self.worker.get_or_insert_with(Worker::spawn);
        self.pending.insert(req, p);
        if w.tx.send(mk(req)).is_err() {
            self.pending.remove(&req);
            self.worker = None;
        }
    }

    pub fn toast(&mut self, tone: Tone, msg: impl Into<String>) {
        self.toast = Some(Toast { msg: msg.into(), tone, until: Instant::now() + TOAST_FOR });
    }

    /// Any git work still in flight?
    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }

    // ── turns ──

    pub fn turn_started(&mut self, pane: usize, dir: PathBuf) {
        self.chips.remove(&pane);
        let id = self.fresh_id();
        let log = self.logs.entry(pane).or_default();
        log.agent_turns += 1;
        let label = format!("Turn {}", log.agent_turns);
        log.push(Turn { id, kind: TurnKind::Agent, label, start: None, end_tree: None, finished: false, summary: None, error: None, at: Instant::now() });
        self.submit(|req| Job::Snapshot { req, dir }, Pending::TurnStart { pane, turn: id });
    }

    pub fn turn_finished(&mut self, pane: usize, dir: PathBuf) {
        let Some(log) = self.logs.get_mut(&pane) else { return };
        let Some(t) = log.turns.iter_mut().rev().find(|t| t.kind == TurnKind::Agent && !t.finished) else { return };
        if t.error.is_some() {
            t.finished = true;
            return;
        }
        t.finished = true;
        let id = t.id;
        let dir = t.start.as_ref().map(|c| c.repo_root.clone()).unwrap_or(dir);
        self.submit(|req| Job::Snapshot { req, dir }, Pending::TurnEnd { pane, turn: id });
    }

    /// Manual checkpoint for `pane` ("Review: Mark Checkpoint").
    pub fn mark(&mut self, pane: usize, dir: PathBuf) {
        self.submit(|req| Job::Snapshot { req, dir }, Pending::Mark { pane });
    }

    fn add_manual(&mut self, pane: usize, root: PathBuf, tree: String, label: &str) -> u64 {
        let id = self.fresh_id();
        let log = self.logs.entry(pane).or_default();
        let label = if label.is_empty() { format!("Checkpoint {}", log.turns.iter().filter(|t| t.kind == TurnKind::Manual).count() + 1) } else { label.to_string() };
        let now = Instant::now();
        log.push(Turn {
            id,
            kind: TurnKind::Manual,
            label,
            start: Some(Checkpoint { id, pane_uid: pane, repo_root: root, tree, at: now, turn: 0 }),
            end_tree: None,
            finished: false,
            summary: None,
            error: None,
            at: now,
        });
        id
    }

    /// Forget a pane that no longer exists.
    pub fn forget_pane(&mut self, pane: usize) {
        self.logs.remove(&pane);
        self.chips.remove(&pane);
        if self.ui.visible && self.ui.pane == pane {
            self.ui.visible = false;
        }
    }

    // ── overlay ──

    /// Open the overlay on `pane`'s newest turn ("changes since checkpoint").
    pub fn open(&mut self, pane: usize) {
        self.chips.remove(&pane);
        let n = self.logs.get(&pane).map_or(0, |l| l.turns.len());
        self.ui = ReviewUi { visible: true, pane, sel_pos: n, gen: self.ui.gen, limit: ROW_LIMIT, ..Default::default() };
        self.load_view();
    }

    pub fn close(&mut self) {
        self.ui.visible = false;
        self.ui.gen += 1; // drop any in-flight result
    }

    fn range_for(&self, pane: usize, pos: usize) -> Result<Range, (String, String)> {
        let none = || ("No checkpoint for this pane".to_string(), "Run \"Review: Mark Checkpoint\" (palette), or start an agent turn".to_string());
        let log = self.logs.get(&pane).ok_or_else(none)?;
        if log.turns.is_empty() {
            return Err(none());
        }
        let pending = |t: &Turn| match &t.error {
            Some(e) => (format!("No checkpoint: {e}"), "Change Review needs the pane's directory to be inside a git repository".to_string()),
            None => ("Taking checkpoint\u{2026}".to_string(), String::new()),
        };
        if pos == 0 {
            let last = log.turns.iter().rev().find_map(|t| t.start.as_ref()).ok_or_else(|| pending(&log.turns[log.turns.len() - 1]))?;
            let first = log.turns.iter().filter_map(|t| t.start.as_ref()).find(|c| c.repo_root == last.repo_root).unwrap_or(last);
            return Ok(Range { repo: last.repo_root.clone(), base: first.tree.clone(), target: None, label: "all changes since start".into() });
        }
        let i = pos - 1;
        let t = log.turns.get(i).ok_or_else(none)?;
        let start = t.start.as_ref().ok_or_else(|| pending(t))?;
        let target = t.end_tree.clone().or_else(|| {
            log.turns.get(i + 1).and_then(|n| n.start.as_ref()).filter(|c| c.repo_root == start.repo_root).map(|c| c.tree.clone())
        });
        let label = match (&target, t.kind) {
            (Some(_), _) => format!("changes in {}", t.label.to_lowercase()),
            (None, TurnKind::Manual) => format!("changes since {}", t.label.to_lowercase()),
            (None, TurnKind::Agent) => format!("changes in {} (running)", t.label.to_lowercase()),
        };
        Ok(Range { repo: start.repo_root.clone(), base: start.tree.clone(), target, label })
    }

    /// (Re)compute the diff for the selected range on the worker.
    pub fn load_view(&mut self) {
        self.ui.gen += 1;
        let gen = self.ui.gen;
        self.ui.file = 0;
        self.ui.scroll = 0;
        self.ui.limit = ROW_LIMIT;
        match self.range_for(self.ui.pane, self.ui.sel_pos) {
            Err((m, h)) => {
                self.ui.range = None;
                self.ui.view = ViewState::Message(m, h);
            }
            Ok(r) => {
                self.ui.range = Some(r.clone());
                self.ui.view = ViewState::Loading;
                self.submit(|req| Job::Diff { req, repo: r.repo, base: r.base, target: r.target }, Pending::View { gen });
            }
        }
    }

    fn items(&self) -> usize {
        self.logs.get(&self.ui.pane).map_or(0, |l| l.turns.len()) + 1
    }

    fn select_pos(&mut self, pos: usize) {
        let pos = pos.min(self.items() - 1);
        if pos != self.ui.sel_pos {
            self.ui.sel_pos = pos;
            self.load_view();
        }
    }

    fn select_file(&mut self, file: usize) {
        let n = self.ui.ready().map_or(0, |d| d.files.len());
        if n == 0 {
            return;
        }
        self.ui.file = file.min(n - 1);
        self.ui.scroll = 0;
        self.ui.limit = ROW_LIMIT;
    }

    fn jump_hunk(&mut self, forward: bool) {
        let Some(f) = self.ui.current_file() else { return };
        let rows = f.hunk_rows();
        let cur = self.ui.scroll;
        let target = if forward { rows.iter().copied().find(|&r| r > cur) } else { rows.iter().copied().rev().find(|&r| r < cur) };
        if let Some(t) = target {
            if t >= self.ui.limit {
                self.ui.limit = t + ROW_STEP / 4;
            }
            self.ui.scroll = t;
        }
    }

    /// Plan a revert of the selected file or of everything in view.
    pub fn revert_plan(&self, all: bool) -> Result<RevertPlan, String> {
        let r = self.ui.range.as_ref().ok_or("nothing to revert: no checkpoint")?;
        let d = self.ui.ready().ok_or("diff is not loaded yet")?;
        let files: Vec<&diff::FileDiff> = if all { d.files.iter().collect() } else { d.current_pick(self.ui.file) };
        if files.is_empty() {
            return Err("no changed files to revert".into());
        }
        let ops: Vec<RevertOp> = files.iter().flat_map(|f| git::ops_for(f)).collect();
        for op in &ops {
            git::safe_target(&r.repo, op.path()).map_err(|e| format!("{}: {e}", op.path()))?;
        }
        Ok(RevertPlan { pane: self.ui.pane, repo: r.repo.clone(), base: r.base.clone(), ops, label: r.label.clone() })
    }

    pub fn start_revert(&mut self, plan: RevertPlan) {
        let pane = plan.pane;
        self.toast(Tone::Neutral, "Reverting\u{2026}");
        self.submit(|req| Job::Revert { req, repo: plan.repo, base: plan.base, ops: plan.ops }, Pending::Revert { pane });
    }

    /// Accept: advance the checkpoint to "now" and dismiss the chip.
    pub fn accept(&mut self) {
        let pane = self.ui.pane;
        let Some(repo) = self.ui.range.as_ref().map(|r| r.repo.clone()) else { return };
        self.chips.remove(&pane);
        self.close();
        self.submit(|req| Job::Snapshot { req, dir: repo }, Pending::Accept { pane });
    }

    // ── results ──

    /// Drain worker results. Returns true when something changed on screen.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            let msg = match self.worker.as_ref().map(|w| w.rx.try_recv()) {
                Some(Ok(m)) => m,
                _ => break,
            };
            self.on_msg(msg);
            changed = true;
        }
        if self.toast.as_ref().is_some_and(|t| Instant::now() >= t.until) {
            self.toast = None;
            changed = true;
        }
        changed
    }

    /// Time of the next toast expiry (for scheduling a redraw).
    pub fn next_deadline(&self) -> Option<Instant> {
        self.toast.as_ref().map(|t| t.until)
    }

    fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Snapshot { req, result } => match self.pending.remove(&req) {
                Some(Pending::TurnStart { pane, turn }) => {
                    if let Some(t) = self.logs.get_mut(&pane).and_then(|l| l.turn_mut(turn)) {
                        match result {
                            Ok((root, tree)) => {
                                let n = t.label.trim_start_matches("Turn ").parse().unwrap_or(0);
                                t.start = Some(Checkpoint { id: turn, pane_uid: pane, repo_root: root, tree, at: t.at, turn: n });
                            }
                            Err(e) => t.error = Some(e),
                        }
                    }
                    self.refresh_if_open(pane);
                }
                Some(Pending::TurnEnd { pane, turn }) => {
                    let Ok((root, tree)) = result else { return };
                    let base = {
                        let Some(t) = self.logs.get_mut(&pane).and_then(|l| l.turn_mut(turn)) else { return };
                        t.end_tree = Some(tree.clone());
                        t.start.as_ref().map(|c| c.tree.clone())
                    };
                    if let Some(base) = base {
                        self.submit(|req| Job::Diff { req, repo: root, base, target: Some(tree) }, Pending::TurnSummary { pane, turn });
                    }
                    self.refresh_if_open(pane);
                }
                Some(Pending::Mark { pane }) => match result {
                    Ok((root, tree)) => {
                        let short = tree.chars().take(7).collect::<String>();
                        self.add_manual(pane, root, tree, "");
                        self.toast(Tone::Success, format!("Review checkpoint saved ({short})"));
                        self.refresh_if_open(pane);
                    }
                    Err(e) => self.toast(Tone::Warning, format!("Review: no checkpoint \u{2014} {e}. Open a pane inside a git repository.")),
                },
                Some(Pending::Accept { pane }) => match result {
                    Ok((root, tree)) => {
                        self.add_manual(pane, root, tree, "Accepted");
                        self.toast(Tone::Success, "Changes accepted; checkpoint advanced");
                    }
                    Err(e) => self.toast(Tone::Warning, format!("Review: accept failed \u{2014} {e}")),
                },
                _ => {}
            },
            Msg::Diff { req, result } => match self.pending.remove(&req) {
                Some(Pending::TurnSummary { pane, turn }) => {
                    let Ok(done) = result else { return };
                    let s = Summary::of(&done.parsed);
                    let mut label = String::new();
                    if let Some(t) = self.logs.get_mut(&pane).and_then(|l| l.turn_mut(turn)) {
                        t.summary = Some(s);
                        label = t.label.clone();
                    }
                    if s.files > 0 {
                        self.chips.insert(pane, Chip { summary: s, label });
                    }
                    self.refresh_if_open(pane);
                }
                Some(Pending::View { gen }) if gen == self.ui.gen => match result {
                    Ok(done) => {
                        self.ui.view = ViewState::Ready(Box::new(done.parsed));
                        self.ui.file = 0;
                        self.ui.scroll = 0;
                    }
                    Err(e) => self.ui.view = ViewState::Error(e),
                },
                _ => {}
            },
            Msg::Reverted { req, result } => {
                let Some(Pending::Revert { pane }) = self.pending.remove(&req) else { return };
                match result {
                    Ok((n, safety)) => {
                        let mut note = String::new();
                        if let (Some(tree), Some(repo)) = (safety, self.ui.range.as_ref().map(|r| r.repo.clone())) {
                            self.add_manual(pane, repo, tree, "Before revert");
                            note = " (undo: 'Before revert' checkpoint)".into();
                        }
                        self.toast(Tone::Success, format!("Reverted {n} path{}{note}", if n == 1 { "" } else { "s" }));
                        if self.ui.visible && self.ui.pane == pane {
                            self.load_view();
                        }
                    }
                    Err(e) => self.toast(Tone::Danger, format!("Revert failed \u{2014} {e}")),
                }
            }
        }
    }

    /// A pane's timeline changed: reload the overlay if it shows that pane's newest range.
    fn refresh_if_open(&mut self, pane: usize) {
        if self.ui.visible && self.ui.pane == pane && matches!(self.ui.view, ViewState::Message(..)) {
            self.load_view();
        }
    }

    /// Drop state of panes that were closed. `alive` lists the open pane ids.
    pub fn prune(&mut self, alive: &[usize]) {
        let gone: Vec<usize> = self.logs.keys().chain(self.chips.keys()).copied().filter(|p| !alive.contains(p)).collect();
        for p in gone {
            self.forget_pane(p);
        }
    }

    /// Mouse: click on a pane's chip.
    pub fn chip_at(&self, x: usize, y: usize) -> Option<usize> {
        self.chip_rects.iter().find(|(_, r)| r.contains(x, y)).map(|(p, _)| *p)
    }
}

impl ParsedDiff {
    fn current_pick(&self, i: usize) -> Vec<&diff::FileDiff> {
        self.files.get(i).into_iter().collect()
    }
}

// ───────────────────────── App-level entry points ─────────────────────────

/// Directory to snapshot for `pane`: the event's git root / cwd, else the
/// shell-reported cwd of the pane, else Rift's own working directory.
fn resolve_dir(app: &App, pane: usize, git_root: Option<&str>, cwd: Option<&str>) -> PathBuf {
    let pane_cwd = app.wm.tabs.iter().flat_map(|t| t.panes()).find(|p| p.id == pane).and_then(|p| p.terminal.cwd.clone());
    git_root
        .map(str::to_string)
        .or_else(|| cwd.map(str::to_string))
        .or(pane_cwd)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Agent turn boundaries (from the agent registry) arrive here.
pub fn on_turn_event(app: &mut App, ev: TurnEvent) {
    match ev {
        TurnEvent::Started { pane, cwd, git_root } => {
            let dir = resolve_dir(app, pane, git_root.as_deref(), cwd.as_deref());
            app.review.turn_started(pane, dir);
        }
        TurnEvent::Finished { pane, cwd, git_root } => {
            let dir = resolve_dir(app, pane, git_root.as_deref(), cwd.as_deref());
            app.review.turn_finished(pane, dir);
        }
        TurnEvent::Exited { pane } => {
            app.review.chips.remove(&pane);
        }
    }
    app.request_redraw();
}

// ── Agent registry integration ──────────────────────────────────────────────

/// Map an agent lifecycle event onto a review turn event. Called from the agents
/// runtime for each `registry.drain_events()` item.
pub fn on_agent_event(app: &mut App, ev: &crate::agents::AgentEvent) {
    use crate::agents::AgentEvent as E;
    match ev {
        E::TurnStarted { pane_uid, cwd, git_root, .. } => {
            on_turn_event(app, TurnEvent::Started { pane: *pane_uid, cwd: cwd.clone(), git_root: git_root.clone() })
        }
        E::TurnFinished { pane_uid, cwd, git_root, .. } => {
            on_turn_event(app, TurnEvent::Finished { pane: *pane_uid, cwd: cwd.clone(), git_root: git_root.clone() })
        }
        E::Exited { pane_uid, .. } => on_turn_event(app, TurnEvent::Exited { pane: *pane_uid }),
        E::NeedsUser { .. } => {}
    }
}

/// "Review: Mark Checkpoint" for the active pane.
pub fn mark_checkpoint(app: &mut App) {
    let pane = app.wm.active_pane().id;
    let dir = resolve_dir(app, pane, None, None);
    app.review.mark(pane, dir);
    app.review.toast(Tone::Neutral, "Saving checkpoint\u{2026}");
    app.request_redraw();
}

/// "Review: Changes Since Checkpoint" for the active pane.
pub fn open_changes(app: &mut App) {
    let pane = app.wm.active_pane().id;
    open_pane(app, pane);
}

pub fn open_pane(app: &mut App, pane: usize) {
    app.review.open(pane);
    app.request_redraw();
}

/// Per-frame work from `about_to_wait`.
pub fn poll(app: &mut App) {
    if app.review.poll() {
        app.request_redraw();
    }
    let due = app.review.last_prune.map_or(true, |t| t.elapsed() > Duration::from_secs(5));
    if due && !(app.review.logs.is_empty() && app.review.chips.is_empty()) {
        app.review.last_prune = Some(Instant::now());
        let alive: Vec<usize> = app.wm.tabs.iter().flat_map(|t| t.panes()).map(|p| p.id).collect();
        app.review.prune(&alive);
    }
}

/// Left click: chip on a pane opens its review. True when consumed.
pub fn on_click(app: &mut App, x: usize, y: usize) -> bool {
    match app.review.chip_at(x, y) {
        Some(pane) => {
            open_pane(app, pane);
            true
        }
        None => false,
    }
}

/// True while the overlay owns the keyboard.
pub fn visible(app: &App) -> bool {
    app.review.ui.visible
}

pub fn handle_key(app: &mut App, key: ReviewKey) {
    use ReviewKey::*;
    let rv = &mut app.review;
    let files_focus = rv.ui.focus == Focus::Files;
    let page = rv.ui.page.max(2) - 1;
    match key {
        Escape => rv.close(),
        Tab => rv.ui.focus = if files_focus { Focus::Turns } else { Focus::Files },
        Char('j') if files_focus => rv.select_file(rv.ui.file + 1),
        Char('k') if files_focus => rv.select_file(rv.ui.file.saturating_sub(1)),
        Char('j') | Down if !files_focus => rv.select_pos(rv.ui.sel_pos + 1),
        Char('k') | Up if !files_focus => rv.select_pos(rv.ui.sel_pos.saturating_sub(1)),
        Down => rv.ui.scroll += 1,
        Up => rv.ui.scroll = rv.ui.scroll.saturating_sub(1),
        PageDown | Char(' ') | Char('d') => rv.ui.scroll += page,
        PageUp | Char('b') | Char('u') => rv.ui.scroll = rv.ui.scroll.saturating_sub(page),
        Home | Char('g') => rv.ui.scroll = 0,
        End | Char('G') => rv.ui.scroll = usize::MAX / 2,
        Char('n') => rv.jump_hunk(true),
        Char('p') => rv.jump_hunk(false),
        Char(']') => rv.select_pos(rv.ui.sel_pos + 1),
        Char('[') => rv.select_pos(rv.ui.sel_pos.saturating_sub(1)),
        Char('m') => rv.ui.limit += ROW_STEP,
        Char('r') | Char('R') => {
            let all = key == Char('R');
            match rv.revert_plan(all) {
                Ok(plan) => confirm_revert(app, plan, all),
                Err(e) => rv.toast(Tone::Warning, format!("Revert: {e}")),
            }
        }
        Char('a') => rv.accept(),
        Char('c') | Char('C') => {
            let text = match (&rv.ui.view, key) {
                (ViewState::Ready(d), Char('C')) => d.full_patch(),
                _ => rv.ui.current_file().map(|f| f.raw.clone()).unwrap_or_default(),
            };
            if text.is_empty() {
                rv.toast(Tone::Warning, "Nothing to copy");
            } else {
                let n = text.lines().count();
                crate::window::selection::copy_to_clipboard(&text);
                rv.toast(Tone::Success, format!("Patch copied ({n} lines)"));
            }
        }
        Char('i') => ask_ai(app),
        _ => {}
    }
    app.request_redraw();
}

fn confirm_revert(app: &mut App, plan: RevertPlan, all: bool) {
    use crate::ui::confirm::{ConfirmAction, ConfirmRequest};
    let mut lines = vec![format!(
        "{} will be restored to its state at \"{}\". Your current edits to {} are discarded; files the agent created are deleted.",
        if all { format!("{} path{}", plan.ops.len(), if plan.ops.len() == 1 { "" } else { "s" }) } else { "This file".to_string() },
        plan.label,
        if all { "them" } else { "it" },
    )];
    for op in plan.ops.iter().take(6) {
        lines.push(match op {
            RevertOp::Restore(p) => format!("  restore  {p}"),
            RevertOp::Delete(p) => format!("  delete   {p}"),
        });
    }
    if plan.ops.len() > 6 {
        lines.push(format!("  \u{2026} and {} more", plan.ops.len() - 6));
    }
    lines.push("Only files inside the repository are touched. A \"Before revert\" checkpoint is saved so you can undo this.".into());
    app.confirm.push(ConfirmRequest {
        title: if all { "Revert all changes?".into() } else { "Revert this file?".into() },
        badge: Some(("DESTRUCTIVE".into(), Tone::Danger)),
        lines,
        buttons: vec!["Revert".into(), "Cancel".into()],
        default_sel: 1,
        esc_choice: Some(1),
        tone: Tone::Danger,
        action: ConfirmAction::ReviewRevert(Box::new(plan)),
    });
}

/// Confirmation answered: run the revert on the worker.
pub fn finish_revert(app: &mut App, plan: RevertPlan, choice: Option<usize>) {
    if choice == Some(0) {
        app.review.start_revert(plan);
    }
}

fn ask_ai(app: &mut App) {
    use crate::ai::hub::{ask, AskRequest, ContextItem, Intent};
    let Some(f) = app.review.ui.current_file() else {
        app.review.toast(Tone::Warning, "No diff selected");
        return;
    };
    let path = f.display_path();
    let patch = f.raw.clone();
    let q = format!(
        "This diff of `{path}` was produced by an AI coding agent. Summarise what it changes, point out bugs, risky edits or missing tests, and tell me whether to accept or revert it."
    );
    let req = AskRequest::new(q, Intent::Explain).with(ContextItem::Selection(patch)).display(format!("Review diff: {path}"));
    app.review.close();
    ask(app, req);
}

#[cfg(test)]
mod tests {
    use super::git::testutil::TempRepo;
    use super::*;

    fn wait(rv: &mut Review, what: &str, mut done: impl FnMut(&Review) -> bool) {
        let t = Instant::now();
        while !done(rv) {
            rv.poll();
            assert!(t.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn idle(rv: &mut Review) {
        wait(rv, "worker idle", |r| !r.busy());
    }

    #[test]
    fn turn_flow_summary_chip_view_revert_and_accept() {
        let r = TempRepo::new("flow");
        r.write("a.txt", "one\ntwo\nthree\n");
        r.write("b.txt", "bee\n");
        r.commit_all("init");
        let mut rv = Review::default();

        rv.turn_started(7, r.dir.clone());
        idle(&mut rv);
        let t = &rv.logs[&7].turns[0];
        assert_eq!((t.label.as_str(), t.kind), ("Turn 1", TurnKind::Agent));
        let cp = t.start.as_ref().expect("start checkpoint");
        assert_eq!((cp.pane_uid, cp.turn, &cp.repo_root), (7, 1, &r.dir));

        // The "agent" edits files.
        r.write("a.txt", "one\nTWO\nthree\nfour\n");
        std::fs::remove_file(r.dir.join("b.txt")).unwrap();
        r.write("c.txt", "new\n");
        rv.turn_finished(7, r.dir.clone());
        wait(&mut rv, "chip", |r| r.chips.contains_key(&7));
        let s = rv.chips[&7].summary;
        assert_eq!((s.files, s.added, s.removed), (3, 3, 2));
        assert_eq!(rv.logs[&7].turns[0].summary, Some(s));

        // Overlay opens on the newest turn and loads its diff.
        rv.open(7);
        assert!(!rv.chips.contains_key(&7), "opening clears the chip");
        wait(&mut rv, "view", |r| matches!(r.ui.view, ViewState::Ready(_)));
        let names: Vec<String> = rv.ui.ready().unwrap().files.iter().map(|f| f.new_path.clone()).collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "c.txt"]);
        assert!(rv.ui.range.as_ref().unwrap().target.is_some(), "finished turn has a fixed end");

        // Revert one file (a.txt) -> others untouched.
        let plan = rv.revert_plan(false).unwrap();
        assert_eq!(plan.ops, vec![RevertOp::Restore("a.txt".into())]);
        rv.start_revert(plan);
        idle(&mut rv);
        assert_eq!(r.read("a.txt"), "one\ntwo\nthree\n");
        assert!(r.exists("c.txt") && !r.exists("b.txt"));
        assert_eq!(rv.logs[&7].turns.last().unwrap().label, "Before revert");

        // Revert all.
        rv.select_pos(1);
        wait(&mut rv, "view 2", |r| matches!(r.ui.view, ViewState::Ready(_)));
        let plan = rv.revert_plan(true).unwrap();
        assert!(plan.ops.contains(&RevertOp::Delete("c.txt".into())));
        rv.start_revert(plan);
        idle(&mut rv);
        assert!(!r.exists("c.txt") && r.exists("b.txt"));
        assert_eq!(r.read("b.txt"), "bee\n");

        // The pre-revert checkpoint can bring the agent's work back.
        let before = rv.logs[&7].turns.iter().rposition(|t| t.label == "Before revert").unwrap();
        rv.select_pos(before + 1);
        wait(&mut rv, "view 3", |r| matches!(r.ui.view, ViewState::Ready(_)));
        assert!(rv.ui.ready().unwrap().files.iter().any(|f| f.new_path == "c.txt"));

        // Accept advances the checkpoint and closes.
        rv.accept();
        assert!(!rv.ui.visible);
        idle(&mut rv);
        assert_eq!(rv.logs[&7].turns.last().unwrap().label, "Accepted");
    }

    #[test]
    fn manual_checkpoint_all_range_and_clean_repo() {
        let r = TempRepo::new("manual");
        r.write("f.txt", "1\n");
        r.commit_all("init");
        let mut rv = Review::default();
        rv.mark(3, r.dir.clone());
        idle(&mut rv);
        assert_eq!(rv.logs[&3].turns[0].label, "Checkpoint 1");

        rv.open(3);
        wait(&mut rv, "clean view", |r| matches!(r.ui.view, ViewState::Ready(_)));
        assert!(rv.ui.ready().unwrap().files.is_empty(), "nothing changed yet");

        r.write("f.txt", "1\n2\n");
        rv.mark(3, r.dir.clone());
        idle(&mut rv);
        r.write("g.txt", "g\n");
        rv.select_pos(0); // all since start
        wait(&mut rv, "all view", |r| matches!(r.ui.view, ViewState::Ready(_)));
        let d = rv.ui.ready().unwrap();
        assert_eq!(d.files.len(), 2, "f.txt edit + g.txt, measured from the first checkpoint");
        rv.select_pos(1); // changes since checkpoint 1 (ends at checkpoint 2)
        wait(&mut rv, "turn view", |r| matches!(r.ui.view, ViewState::Ready(_)));
        let d = rv.ui.ready().unwrap();
        assert_eq!(d.files.len(), 1);
        assert_eq!(d.files[0].new_path, "f.txt");
    }

    #[test]
    fn non_git_directory_degrades_to_a_hint() {
        let dir = std::env::temp_dir().join(format!("rift-review-nogit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        if git::repo_root(&dir).is_ok() {
            let _ = std::fs::remove_dir_all(&dir);
            return; // temp dir lives inside a repository on this machine
        }
        let mut rv = Review::default();
        rv.mark(1, dir.clone());
        idle(&mut rv);
        assert!(rv.logs.get(&1).map_or(true, |l| l.turns.is_empty()));
        assert!(rv.toast.as_ref().unwrap().msg.contains("git"));
        rv.turn_started(1, dir.clone());
        idle(&mut rv);
        assert!(rv.logs[&1].turns[0].error.is_some());
        rv.open(1);
        assert!(matches!(rv.ui.view, ViewState::Message(..)), "hint instead of a diff");
        rv.turn_finished(1, dir.clone()); // must not panic or queue work
        assert!(!rv.busy());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn revert_plan_requires_a_loaded_view_and_prune_forgets_closed_panes() {
        let mut rv = Review::default();
        assert!(rv.revert_plan(true).is_err());
        rv.logs.insert(5, PaneLog::default());
        rv.logs.insert(6, PaneLog::default());
        rv.prune(&[6]);
        assert!(!rv.logs.contains_key(&5) && rv.logs.contains_key(&6));
    }

    #[test]
    fn navigation_clamps_and_hunk_jumps() {
        let mut rv = Review::default();
        let text = (0..40).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
        let mut old = String::new();
        let mut new = String::new();
        for i in 0..40 {
            old.push_str(&format!("l{i}\n"));
            new.push_str(&if i % 15 == 7 { format!("changed{i}\n") } else { format!("l{i}\n") });
        }
        let _ = text;
        let r = TempRepo::new("nav");
        r.write("f.txt", &old);
        r.commit_all("i");
        let a = git::snapshot_tree(&r.dir).unwrap();
        r.write("f.txt", &new);
        let b = git::snapshot_tree(&r.dir).unwrap();
        let o = git::diff_trees(&r.dir, &a, &b).unwrap();
        let d = diff::parse_unified(&o.text(), false);
        assert_eq!(d.files[0].hunks.len(), 3);
        rv.ui.view = ViewState::Ready(Box::new(d));
        rv.ui.limit = ROW_LIMIT;
        rv.jump_hunk(true);
        let first = rv.ui.scroll;
        assert!(first > 0);
        rv.jump_hunk(true);
        assert!(rv.ui.scroll > first);
        rv.jump_hunk(false);
        assert_eq!(rv.ui.scroll, first);
        rv.select_file(99);
        assert_eq!(rv.ui.file, 0, "single file: clamped");
    }
}
