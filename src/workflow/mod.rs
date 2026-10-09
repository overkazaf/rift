//! Workflows on top of Mission Control: several agents on one task, a writer
//! and a reviewer, a fix-the-tests loop and per-agent task queues.
//!
//! * `template.rs`: built-in and user templates (`~/.config/rift/workflows.toml`),
//!   `{task}` / `{branch}` / `{cwd}` expansion.
//! * State machines (pure, tested with synthetic events): `bestof.rs` (N
//!   candidates in worktrees, collect, compare), `review_loop.rs` (writer ->
//!   diff -> reviewer -> feedback), `fixtests.rs` (test, send failures, retest),
//!   `queue.rs` (task queue and its 3 second countdown).
//! * `gitops.rs`: blocking git/shell work (merge, discard, diffs, tests). Runs
//!   on background threads; destructive steps are confirmed through
//!   `ui/confirm.rs` first.
//! * `deliver.rs`: how a prompt reaches an agent (CLI argument verified against
//!   the CLI's own `--help`, or typed once the agent is ready).
//! * `ui.rs`: the start wizard, the queue editor and the compare view.
//! * This file: the glue to `App` ([`Workflows`] lives on `app.workflows`).
//!   `start.rs` launches runs, `actions.rs` carries out the compare view's
//!   actions.

pub mod bestof;
pub mod deliver;
pub mod fixtests;
pub mod gitops;
pub mod queue;
pub mod review_loop;
pub mod sample;
pub mod template;
pub mod ui;

mod actions;
mod start;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::agents::{metrics, AgentEvent, AgentKind, AgentState, AgentRegistry};
use crate::app::App;
use crate::review::diff::ParsedDiff;
use crate::ui::kit::Tone;
use bestof::{BestOfN, DiffStat, TestStatus};
use deliver::{Delivery, PendingSend, Ready, SendTag};
use fixtests::FixTests;
use queue::{Countdown, TaskQueue};
use review_loop::WriteReview;
use ui::{WKey, WorkflowUi};

pub use actions::resolve_confirm;
pub use actions::WfConfirm;
pub use start::{open_best_of_n, open_template};

// ───────────────────────────── runs ─────────────────────────────

/// A best-of-N run plus what the compare view needs.
pub struct BestRun {
    pub sm: BestOfN,
    /// Parsed diff of each candidate against the base.
    pub parsed: Vec<Option<ParsedDiff>>,
    /// A merge / discard is in progress ("Merging").
    pub busy: Option<String>,
    /// Last result, shown under the candidates.
    pub message: Option<(Tone, String)>,
}

pub struct ReviewRun {
    pub sm: WriteReview,
    /// Writer finished a turn; waiting for review to produce the turn's trees.
    pub waiting_range: Option<Instant>,
}

pub struct FixRun {
    pub sm: FixTests,
    pub dir: PathBuf,
}

pub enum Run {
    Best(BestRun),
    Review(ReviewRun),
    Fix(FixRun),
}

impl Run {
    pub fn id(&self) -> u64 {
        match self {
            Run::Best(b) => b.sm.id,
            Run::Review(r) => r.sm.id,
            Run::Fix(f) => f.sm.id,
        }
    }

    pub fn panes(&self) -> Vec<usize> {
        match self {
            Run::Best(b) => b.sm.panes(),
            Run::Review(r) => r.sm.panes().to_vec(),
            Run::Fix(f) => vec![f.sm.pane],
        }
    }

    pub fn is_finished(&self) -> bool {
        match self {
            Run::Best(b) => !b.sm.is_active(),
            Run::Review(r) => r.sm.is_done(),
            Run::Fix(f) => f.sm.is_done(),
        }
    }
}

// ───────────────────────────── dock card data ─────────────────────────────

/// What the dock shows on an agent's card for workflows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardInfo {
    /// "Write & Review \u{b7} round 1/3 \u{b7} reviewer working".
    pub label: Option<String>,
    pub queue: usize,
    pub paused: bool,
    /// Preview of the next queued task.
    pub next: Option<String>,
    /// Seconds until the next queued task is sent.
    pub countdown: Option<u64>,
    pub note: Option<Note>,
}

/// A block of text on a card: the reviewer's feedback, a failing test tail.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub title: String,
    pub text: String,
    pub tone: Tone,
    /// `f` forwards this to the writer.
    pub forward: bool,
}

/// Card lines for the dock: (text, tone). `cols` is the card's inner width.
pub fn card_lines(ci: &CardInfo, cols: usize, max_note_lines: usize) -> Vec<(String, Tone)> {
    let mut v = Vec::new();
    let mut head = ci.label.clone();
    if ci.queue > 0 || ci.paused {
        let q = if ci.paused { format!("queue {} paused", ci.queue) } else { format!("queue {}", ci.queue) };
        head = Some(match head {
            Some(h) => format!("{h}  \u{b7}  {q}"),
            None => q,
        });
    }
    if let Some(h) = head {
        v.push((crate::ui::kit::ellipsize(&h, cols), if ci.paused { Tone::Warning } else { Tone::Accent }));
    }
    if let Some(s) = ci.countdown {
        let next = ci.next.clone().unwrap_or_default();
        v.push((crate::ui::kit::ellipsize(&format!("next in {s}s: {next}  (Esc pauses)"), cols), Tone::Warning));
    }
    if let Some(n) = &ci.note {
        // The action comes first: compact cards only have room for two lines.
        if n.forward {
            v.push(("f  forward this feedback to the writer".to_string(), Tone::Success));
        }
        v.push((crate::ui::kit::ellipsize(&n.title, cols), n.tone));
        for l in review_loop::note_lines(&n.text, max_note_lines, cols) {
            v.push((l, Tone::Neutral));
        }
    }
    v
}

// ───────────────────────────── background jobs ─────────────────────────────

/// What `start::prepare` hands back from its worker thread.
pub struct Prep {
    pub root: Option<PathBuf>,
    pub base_branch: Option<String>,
    pub base_commit: String,
    pub slug: String,
    pub plans: Vec<crate::agents::launch::WorktreePlan>,
    pub deliveries: HashMap<AgentKind, Delivery>,
}

pub(crate) enum JobMsg {
    Prepared { id: u64, result: Result<Prep, String> },
    ReviewDiff { run: u64, result: Result<String, String> },
    CandDiff { run: u64, idx: usize, result: Result<(DiffStat, ParsedDiff), String> },
    CandTests { run: u64, idx: usize, status: TestStatus },
    FixTests { run: u64, result: Result<(), String> },
    Preflight { run: u64, idx: usize, plan: gitops::MergePlan, result: Result<gitops::Preflight, String> },
    Merged { run: u64, idx: usize, result: Result<gitops::MergeDone, String> },
    Discarded { run: u64, results: Vec<(usize, String, Result<(), String>)> },
}

// ───────────────────────────── state on App ─────────────────────────────

pub struct Workflows {
    pub ui: WorkflowUi,
    /// Task queues by pane uid.
    pub queues: HashMap<usize, TaskQueue>,
    pub countdown: Option<Countdown>,
    pub(crate) runs: Vec<Run>,
    /// Agents that finished a turn and may get their next queued task.
    armed: HashSet<usize>,
    pub(crate) starting: HashMap<u64, start::StartCtx>,
    pub(crate) pending: Vec<PendingSend>,
    pub(crate) delivery: HashMap<AgentKind, Delivery>,
    pub(crate) next_id: u64,
    pub(crate) tx: Sender<JobMsg>,
    rx: Receiver<JobMsg>,
    /// Compare view to open once no modal is in the way.
    pub(crate) open_compare: Option<u64>,
    last_gone_check: Option<Instant>,
    /// Problems in workflows.toml already shown to the user.
    pub(crate) warned: Vec<String>,
}

impl Default for Workflows {
    fn default() -> Self {
        Self::new()
    }
}

/// How often runs check whether their panes still exist.
const GONE_CHECK: Duration = Duration::from_millis(1000);
/// Waiting for Change Review to produce a turn's trees.
const RANGE_WAIT: Duration = Duration::from_secs(20);

impl Workflows {
    pub fn new() -> Workflows {
        let (tx, rx) = channel();
        Workflows {
            ui: WorkflowUi::default(),
            queues: HashMap::new(),
            countdown: None,
            runs: Vec::new(),
            armed: HashSet::new(),
            starting: HashMap::new(),
            pending: Vec::new(),
            delivery: HashMap::new(),
            next_id: 1,
            tx,
            rx,
            open_compare: None,
            last_gone_check: None,
            warned: Vec::new(),
        }
    }

    pub(crate) fn fresh_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// An overlay owns the keyboard.
    pub fn overlay_visible(&self) -> bool {
        self.ui.visible()
    }

    /// Label for the spinner toast while worktrees are being created.
    pub fn progress_label(&self) -> Option<&str> {
        self.starting.values().next().map(|s| s.label.as_str())
    }

    pub(crate) fn run_mut(&mut self, id: u64) -> Option<&mut Run> {
        self.runs.iter_mut().find(|r| r.id() == id)
    }

    pub fn best_run(&self, id: u64) -> Option<&BestRun> {
        self.runs.iter().find_map(|r| match r {
            Run::Best(b) if b.sm.id == id => Some(b),
            _ => None,
        })
    }

    /// The newest best-of-N run whose results are ready.
    pub fn latest_ready_best(&self) -> Option<u64> {
        self.runs.iter().rev().find_map(|r| match r {
            Run::Best(b) if b.sm.phase == bestof::Phase::Ready => Some(b.sm.id),
            _ => None,
        })
    }

    /// Is `uid` driven by a run (so its queue must not interfere)?
    fn managed(&self, uid: usize) -> bool {
        self.runs.iter().any(|r| !r.is_finished() && r.panes().contains(&uid))
    }

    // ── dock card ──

    pub fn card_info(&self, uid: usize, now: Instant) -> CardInfo {
        let mut ci = CardInfo::default();
        if let Some(q) = self.queues.get(&uid) {
            ci.queue = q.len();
            ci.paused = q.paused;
            ci.next = q.tasks.first().map(|t| queue::preview(t, 40));
        }
        ci.countdown = self.countdown.as_ref().filter(|c| c.uid == uid).map(|c| c.secs_left(now));
        for run in &self.runs {
            if !run.panes().contains(&uid) {
                continue;
            }
            match run {
                Run::Best(b) => {
                    let n = b.sm.cands.len();
                    ci.label = Some(match &b.sm.phase {
                        bestof::Phase::Creating => format!("Best of {n} \u{b7} starting"),
                        bestof::Phase::Running => format!("Best of {n} \u{b7} {}/{n} finished", b.sm.finished_count()),
                        bestof::Phase::Collecting => format!("Best of {n} \u{b7} collecting results"),
                        bestof::Phase::Ready => format!("Best of {n} \u{b7} ready to compare"),
                        bestof::Phase::Aborted(_) => format!("Best of {n} \u{b7} aborted"),
                    });
                    if b.sm.phase == bestof::Phase::Ready {
                        ci.note = Some(Note { title: "Compare is ready".into(), text: "Palette > Workflow: Compare Candidates".into(), tone: Tone::Success, forward: false });
                    }
                }
                Run::Review(r) => {
                    let s = &r.sm;
                    let phase = match s.phase {
                        review_loop::Phase::Writing => "writing",
                        review_loop::Phase::AwaitDiff => "collecting the diff",
                        review_loop::Phase::Reviewing => "reviewer working",
                        review_loop::Phase::Feedback => "feedback ready",
                        review_loop::Phase::Done => "",
                    };
                    let role = if s.reviewer == uid { "Reviewer" } else { "Write & Review" };
                    ci.label = Some(match &s.outcome {
                        Some(o) => format!("{role} \u{b7} {}", o.label()),
                        None => format!("{role} \u{b7} round {}/{} \u{b7} {phase}", s.round, s.max_rounds),
                    });
                    if let Some(fb) = &s.feedback {
                        let verdict = match fb.verdict {
                            review_loop::Verdict::Approved => "approved",
                            review_loop::Verdict::ChangesRequested => "changes requested",
                            review_loop::Verdict::Unclear => "no verdict",
                        };
                        ci.note = Some(Note {
                            title: format!("Reviewer \u{b7} round {} \u{b7} {verdict}", fb.round),
                            text: fb.text.clone(),
                            tone: if fb.verdict == review_loop::Verdict::Approved { Tone::Success } else { Tone::Warning },
                            forward: s.phase == review_loop::Phase::Feedback && s.writer == uid,
                        });
                    }
                }
                Run::Fix(f) => {
                    let s = &f.sm;
                    let phase = match s.phase {
                        fixtests::Phase::Testing => "running tests",
                        fixtests::Phase::Fixing => "agent fixing",
                        fixtests::Phase::Done => "",
                    };
                    ci.label = Some(match &s.outcome {
                        Some(o) => format!("Fix tests \u{b7} {}", o.label()),
                        None => format!("Fix tests \u{b7} attempt {}/{} \u{b7} {phase}", s.attempts.max(1), s.max_attempts),
                    });
                    if let Some(out) = &s.last_failure {
                        ci.note = Some(Note { title: "Last failing output".into(), text: out.clone(), tone: Tone::Danger, forward: false });
                    }
                }
            }
        }
        ci
    }

    // ── queues ──

    /// Queue edit from the editor: keeps the map free of empty queues.
    fn queue_mut(&mut self, uid: usize) -> &mut TaskQueue {
        self.queues.entry(uid).or_default()
    }

    fn drop_empty_queue(&mut self, uid: usize) {
        if self.queues.get(&uid).is_some_and(|q| q.is_empty() && !q.paused) {
            self.queues.remove(&uid);
        }
    }

    /// Queues for the session file: per tab, (in-order pane index, queue).
    pub fn queues_for_session(&self, wm: &crate::window::WindowManager) -> Vec<queue::LeafQueues> {
        wm.tabs
            .iter()
            .map(|t| {
                t.panes()
                    .iter()
                    .enumerate()
                    .filter_map(|(i, p)| self.queues.get(&p.id).filter(|q| !q.is_empty()).map(|q| (i, q.clone())))
                    .collect()
            })
            .collect()
    }
}

// ───────────────────────────── agent events ─────────────────────────────

fn screen_tail(app: &App, uid: usize, n: usize) -> Vec<String> {
    let Some(pane) = app.pane_ref(uid) else { return Vec::new() };
    let t = &pane.terminal;
    let mut rows: Vec<String> = t.grid.iter().map(|r| crate::terminal::grid::cells_text(r)).collect();
    let need = n.saturating_sub(rows.len());
    if need > 0 {
        let skip = t.scrollback.len().saturating_sub(need);
        let mut older: Vec<String> = t.scrollback.iter().skip(skip).map(|r| crate::terminal::grid::cells_text(r)).collect();
        older.append(&mut rows);
        rows = older;
    }
    let from = rows.len().saturating_sub(n);
    rows.split_off(from)
}

fn metrics_of(app: &App, uid: usize, kind: AgentKind) -> (Option<f64>, Option<u64>) {
    let lines = screen_tail(app, uid, 40);
    let fresh = metrics::parse_screen(Some(kind), &lines);
    let m = match app.win.agents_ui.info.get(&uid) {
        Some(i) => metrics::merge(&i.metrics, &fresh),
        None => fresh,
    };
    (m.cost, m.tokens)
}

/// Agent lifecycle events arrive here (from `agents::runtime::poll`).
pub fn on_agent_event(app: &mut App, ev: &AgentEvent) {
    let now = Instant::now();
    match ev {
        AgentEvent::TurnStarted { pane_uid, .. } => {
            app.workflows.armed.remove(pane_uid);
            let ids: Vec<u64> = app.workflows.runs.iter().filter(|r| r.panes().contains(pane_uid)).map(|r| r.id()).collect();
            for id in ids {
                if let Some(Run::Best(b)) = app.workflows.run_mut(id) {
                    b.sm.on_event(bestof::Ev::TurnStarted(*pane_uid, now));
                }
            }
        }
        AgentEvent::TurnFinished { pane_uid, kind, .. } => {
            let uid = *pane_uid;
            let mut managed = false;
            let ids: Vec<u64> = app.workflows.runs.iter().filter(|r| r.panes().contains(&uid)).map(|r| r.id()).collect();
            for id in ids {
                managed = true;
                let is_best = matches!(app.workflows.run_mut(id), Some(Run::Best(_)));
                let is_reviewer = matches!(app.workflows.run_mut(id), Some(Run::Review(r)) if r.sm.reviewer == uid);
                if is_best {
                    let (cost, tokens) = metrics_of(app, uid, *kind);
                    let cmds = match app.workflows.run_mut(id) {
                        Some(Run::Best(b)) => b.sm.on_event(bestof::Ev::TurnFinished { pane: uid, at: now, cost, tokens }),
                        _ => Vec::new(),
                    };
                    actions::run_best_cmds(app, id, cmds);
                } else if is_reviewer {
                    let marker = match app.workflows.run_mut(id) {
                        Some(Run::Review(r)) => review_loop::marker(r.sm.round),
                        _ => String::new(),
                    };
                    let reply = review_loop::extract_reply(&screen_tail(app, uid, 300), &marker);
                    let cmds = match app.workflows.run_mut(id) {
                        Some(Run::Review(r)) => r.sm.on_event(review_loop::Ev::ReviewerFinished(reply)),
                        _ => Vec::new(),
                    };
                    start::run_review_cmds(app, id, cmds);
                } else {
                    match app.workflows.run_mut(id) {
                        Some(Run::Review(r)) => {
                            let cmds = r.sm.on_event(review_loop::Ev::WriterFinished);
                            if cmds.contains(&review_loop::Cmd::ComputeDiff) {
                                r.waiting_range = Some(now);
                            }
                            start::run_review_cmds(app, id, cmds);
                        }
                        Some(Run::Fix(f)) => {
                            let cmds = f.sm.on_event(fixtests::Ev::AgentFinished);
                            start::run_fix_cmds(app, id, cmds);
                        }
                        _ => {}
                    }
                }
            }
            if !managed && app.workflows.queues.get(&uid).is_some_and(|q| !q.is_empty() && !q.paused) {
                app.workflows.armed.insert(uid);
            }
        }
        AgentEvent::Exited { pane_uid, .. } => gone(app, *pane_uid, now),
        AgentEvent::NeedsUser { .. } => {}
    }
    app.request_redraw();
}

/// An agent quit or its pane closed: tell the runs it belongs to.
fn gone(app: &mut App, uid: usize, now: Instant) {
    app.workflows.armed.remove(&uid);
    if app.workflows.countdown.as_ref().is_some_and(|c| c.uid == uid) {
        app.workflows.countdown = None;
    }
    app.workflows.pending.retain(|p| p.uid != uid);
    let ids: Vec<u64> = app.workflows.runs.iter().filter(|r| r.panes().contains(&uid)).map(|r| r.id()).collect();
    for id in ids {
        match app.workflows.run_mut(id) {
            Some(Run::Best(b)) => {
                let cmds = b.sm.on_event(bestof::Ev::Exited(uid, now));
                actions::run_best_cmds(app, id, cmds);
            }
            Some(Run::Review(r)) => {
                let cmds = r.sm.on_event(review_loop::Ev::PaneGone(uid));
                start::run_review_cmds(app, id, cmds);
            }
            Some(Run::Fix(f)) => {
                let cmds = f.sm.on_event(fixtests::Ev::AgentGone);
                start::run_fix_cmds(app, id, cmds);
            }
            None => {}
        }
    }
}

// ───────────────────────────── poll ─────────────────────────────

/// Per-frame work from `about_to_wait`: job results, prompts waiting for an
/// agent, the queue countdown, panes that disappeared.
pub fn poll(app: &mut App, wake_at: &mut Instant) {
    let now = Instant::now();
    let mut changed = false;
    while let Ok(msg) = app.workflows.rx.try_recv() {
        actions::on_job(app, msg);
        changed = true;
    }
    flush_pending(app, now, &mut changed);
    changed |= poll_queues(app, now, wake_at);
    poll_ranges(app, now);
    check_gone(app, now);
    if let Some(id) = app.workflows.open_compare {
        if !app.win.confirm.visible() && !app.win.exec_preview.visible {
            app.workflows.open_compare = None;
            actions::open_compare(app, id);
            changed = true;
        }
    }
    if !app.workflows.pending.is_empty() {
        *wake_at = (*wake_at).min(now + Duration::from_millis(250));
    }
    if !app.workflows.starting.is_empty() {
        // The progress spinner animates.
        *wake_at = (*wake_at).min(now + Duration::from_millis(100));
        changed = true;
    }
    if app.workflows.runs.iter().any(|r| matches!(r, Run::Review(rr) if rr.waiting_range.is_some())) {
        *wake_at = (*wake_at).min(now + Duration::from_millis(250));
    }
    if changed {
        app.request_redraw();
    }
}

/// Type prompts into agents that have become ready.
fn flush_pending(app: &mut App, now: Instant, changed: &mut bool) {
    if app.workflows.pending.is_empty() {
        return;
    }
    let mut keep = Vec::new();
    for p in std::mem::take(&mut app.workflows.pending) {
        if !app.pane_exists(p.uid) {
            continue; // the pane is gone
        }
        let (state, age) = match app.agents.session(p.uid) {
            Some(s) => (Some(s.state), now.saturating_duration_since(s.state_since)),
            None => (None, Duration::ZERO),
        };
        match deliver::readiness(state, age, now.saturating_duration_since(p.since)) {
            Ready::Wait => keep.push(p),
            Ready::GiveUp => {
                app.win.blocks_ui.show_toast("A workflow prompt could not be delivered: the agent never became ready");
                *changed = true;
                on_delivery_failed(app, &p);
            }
            Ready::Send => {
                crate::agents::console::send_text(app, &[p.uid], &p.text);
                on_delivered(app, &p);
                *changed = true;
            }
        }
    }
    keep.extend(std::mem::take(&mut app.workflows.pending));
    app.workflows.pending = keep;
}

fn on_delivered(app: &mut App, p: &PendingSend) {
    match p.tag {
        SendTag::Candidate { run } => {
            if let Some(Run::Best(b)) = app.workflows.run_mut(run) {
                b.sm.on_event(bestof::Ev::Delivered(p.uid));
            }
        }
        SendTag::Writer { run } => {
            if let Some(Run::Review(r)) = app.workflows.run_mut(run) {
                r.sm.on_event(review_loop::Ev::WriterSent);
            }
        }
        SendTag::Reviewer { run } => {
            if let Some(Run::Review(r)) = app.workflows.run_mut(run) {
                r.sm.on_event(review_loop::Ev::ReviewerSent);
            }
        }
        SendTag::Fixer { run } => {
            if let Some(Run::Fix(f)) = app.workflows.run_mut(run) {
                f.sm.on_event(fixtests::Ev::Sent);
            }
        }
        SendTag::Queue => {
            app.workflows.armed.remove(&p.uid);
        }
    }
}

fn on_delivery_failed(app: &mut App, p: &PendingSend) {
    gone(app, p.uid, Instant::now());
}

/// Queued tasks: arm, count down, send.
fn poll_queues(app: &mut App, now: Instant, wake_at: &mut Instant) -> bool {
    let mut changed = false;
    // Only agents that still have something to send stay armed.
    let queues = &app.workflows.queues;
    let agents = &app.agents;
    app.workflows.armed.retain(|uid| queues.get(uid).is_some_and(|q| !q.is_empty() && !q.paused) && agents.session(*uid).is_some_and(|s| s.state.is_live()));
    // Drop queues of panes that no longer exist (never the persisted-but-unattached ones: those wait for their pane).
    // A running countdown ticks once a second.
    if let Some(c) = app.workflows.countdown.clone() {
        let state = app.agents.session(c.uid).map(|s| s.state);
        let still = app.workflows.queues.get(&c.uid).is_some_and(|q| !q.is_empty() && !q.paused);
        if state != Some(AgentState::Idle) || !still {
            // The user got there first (typed, approval prompt, closed): stand down.
            app.workflows.countdown = None;
            app.workflows.armed.remove(&c.uid);
            return true;
        }
        if c.due(now) {
            app.workflows.countdown = None;
            let task = app.workflows.queues.get_mut(&c.uid).and_then(|q| q.pop_front());
            app.workflows.drop_empty_queue(c.uid);
            if let Some(task) = task {
                let left = app.workflows.queues.get(&c.uid).map_or(0, |q| q.len());
                app.workflows.pending.push(PendingSend { uid: c.uid, text: task, since: now, tag: SendTag::Queue });
                app.win.blocks_ui.show_toast(format!("Sent the next queued task ({left} left)"));
            }
            return true;
        }
        *wake_at = (*wake_at).min(now + Duration::from_millis(200));
        changed = true; // repaint the countdown
    } else {
        let candidate = app
            .workflows
            .armed
            .iter()
            .copied()
            .find(|uid| {
                app.workflows.queues.get(uid).is_some_and(|q| !q.is_empty() && !q.paused)
                    && !app.workflows.pending.iter().any(|p| p.uid == *uid)
                    && app.agents.session(*uid).is_some_and(|s| s.state == AgentState::Idle && now.saturating_duration_since(s.state_since) >= Duration::from_millis(400))
            });
        if let Some(uid) = candidate {
            let task = app.workflows.queues[&uid].tasks[0].clone();
            app.workflows.countdown = Some(Countdown::start(uid, &task, now));
            *wake_at = (*wake_at).min(now + Duration::from_millis(200));
            changed = true;
        } else if !app.workflows.armed.is_empty() {
            *wake_at = (*wake_at).min(now + Duration::from_millis(300));
        }
    }
    changed
}

/// Writer turns waiting for Change Review's trees.
fn poll_ranges(app: &mut App, now: Instant) {
    let waiting: Vec<(u64, usize, Instant)> = app
        .workflows
        .runs
        .iter()
        .filter_map(|r| match r {
            Run::Review(rr) => rr.waiting_range.map(|t| (rr.sm.id, rr.sm.writer, t)),
            _ => None,
        })
        .collect();
    for (id, writer, since) in waiting {
        if let Some(range) = app.review.last_turn_range(writer) {
            if let Some(Run::Review(r)) = app.workflows.run_mut(id) {
                r.waiting_range = None;
            }
            let tx = app.workflows.tx.clone();
            std::thread::spawn(move || {
                let result = gitops::tree_diff_text(&range.repo, &range.base, &range.end);
                let _ = tx.send(JobMsg::ReviewDiff { run: id, result });
                crate::wake::wake();
            });
        } else if now.saturating_duration_since(since) > RANGE_WAIT {
            if let Some(Run::Review(r)) = app.workflows.run_mut(id) {
                r.waiting_range = None;
                let cmds = r.sm.on_event(review_loop::Ev::Fail("could not read the turn's changes (is the directory a git repository?)".into()));
                start::run_review_cmds(app, id, cmds);
            }
        }
    }
}

/// Panes that vanished without an Exited event.
fn check_gone(app: &mut App, now: Instant) {
    if app.workflows.last_gone_check.is_some_and(|t| now.saturating_duration_since(t) < GONE_CHECK) {
        return;
    }
    app.workflows.last_gone_check = Some(now);
    let missing: Vec<usize> = app
        .workflows
        .runs
        .iter()
        .filter(|r| !r.is_finished())
        .flat_map(|r| r.panes())
        .filter(|uid| !app.pane_exists(*uid))
        .collect();
    for uid in missing {
        gone(app, uid, now);
    }
    // Queues of closed panes go away; queues loaded from a session wait for a pane with that id.
}

// ───────────────────────────── user actions ─────────────────────────────

/// Esc during the countdown: send nothing and pause the queue.
pub fn cancel_countdown(app: &mut App) -> bool {
    let Some(c) = app.workflows.countdown.take() else { return false };
    if let Some(q) = app.workflows.queues.get_mut(&c.uid) {
        q.paused = true;
    }
    app.workflows.armed.remove(&c.uid);
    app.win.blocks_ui.show_toast("Queue paused: the next task was not sent");
    app.request_redraw();
    true
}

/// Dock `t`: open the queue editor for an agent.
pub fn open_queue(app: &mut App, uid: usize) {
    let Some(s) = app.agents.session(uid) else {
        app.win.blocks_ui.show_toast("That agent's pane is gone");
        return;
    };
    let title = format!("{} \u{b7} {}", s.title, s.place());
    app.workflows.ui.close_all();
    let mut ed = ui::QueueEditor::new(uid, title);
    if app.workflows.queues.get(&uid).is_none_or(|q| q.is_empty()) {
        ed.begin_add();
    }
    app.workflows.ui.queue = Some(ed);
    app.request_redraw();
}

/// Dock `f`: send the reviewer's feedback to the writer.
pub fn forward_feedback(app: &mut App, uid: usize) {
    let ids: Vec<u64> = app.workflows.runs.iter().filter(|r| matches!(r, Run::Review(rr) if rr.sm.panes().contains(&uid))).map(|r| r.id()).collect();
    let Some(&id) = ids.last() else {
        app.win.blocks_ui.show_toast("No reviewer feedback to forward");
        return;
    };
    let cmds = match app.workflows.run_mut(id) {
        Some(Run::Review(r)) if r.sm.phase == review_loop::Phase::Feedback => r.sm.on_event(review_loop::Ev::Forward),
        _ => {
            app.win.blocks_ui.show_toast("No reviewer feedback to forward");
            return;
        }
    };
    start::run_review_cmds(app, id, cmds);
    app.request_redraw();
}

/// Stop the workflow `uid` belongs to (or all running ones when `None`).
pub fn stop(app: &mut App, uid: Option<usize>) {
    let ids: Vec<u64> = app
        .workflows
        .runs
        .iter()
        .filter(|r| !r.is_finished() && uid.is_none_or(|u| r.panes().contains(&u)))
        .filter(|r| matches!(r, Run::Review(_) | Run::Fix(_)))
        .map(|r| r.id())
        .collect();
    if ids.is_empty() {
        app.win.blocks_ui.show_toast("No loop is running");
        return;
    }
    for id in &ids {
        match app.workflows.run_mut(*id) {
            Some(Run::Review(r)) => {
                r.sm.on_event(review_loop::Ev::Stop);
            }
            Some(Run::Fix(f)) => {
                f.sm.on_event(fixtests::Ev::Stop);
            }
            _ => {}
        }
        let panes: Vec<usize> = app.workflows.runs.iter().find(|r| r.id() == *id).map(|r| r.panes()).unwrap_or_default();
        app.workflows.pending.retain(|p| !panes.contains(&p.uid));
    }
    app.win.blocks_ui.show_toast(format!("Stopped {} workflow{}", ids.len(), if ids.len() == 1 { "" } else { "s" }));
    app.request_redraw();
}

/// Palette commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowCmd {
    BestOfN,
    /// A built-in or user template by name.
    Template(String),
    Queue,
    Compare,
    Stop,
}

pub fn run_command(app: &mut App, cmd: WorkflowCmd) {
    match cmd {
        WorkflowCmd::BestOfN => open_best_of_n(app),
        WorkflowCmd::Template(name) => open_template(app, &name),
        WorkflowCmd::Queue => {
            let uid = app.win.agents_ui.selected.filter(|u| app.agents.session(*u).is_some()).or_else(|| {
                let active = app.win.wm.active_pane().id;
                app.agents.session(active).map(|_| active)
            });
            match uid {
                Some(uid) => open_queue(app, uid),
                None => {
                    app.win.blocks_ui.show_toast("Select an agent first: focus its pane or pick its card in Mission Control");
                    app.request_redraw();
                }
            }
        }
        WorkflowCmd::Compare => match app.workflows.latest_ready_best() {
            Some(id) => actions::open_compare(app, id),
            None => {
                app.win.blocks_ui.show_toast("No finished best-of-N run to compare yet");
                app.request_redraw();
            }
        },
        WorkflowCmd::Stop => stop(app, None),
    }
}

/// Palette rows for the user's own templates: (name shown after "Workflow: ", detail).
/// The built-ins have fixed palette entries.
pub fn palette_user_templates(app: &mut App) -> Vec<(String, String)> {
    let all = start::templates(app);
    all.iter().filter(|t| !t.builtin).map(|t| (t.name.clone(), format!("{} \u{b7} workflows.toml", t.strategy.label()))).collect()
}

// ───────────────────────────── keys, clicks, drawing ─────────────────────────────

/// Overlay keys (from `app/overlays.rs`).
pub fn handle_key(app: &mut App, key: WKey) {
    if let Some(mut w) = app.workflows.ui.wizard.take() {
        match w.handle_key(key) {
            ui::WizardOutcome::Cancel => {}
            ui::WizardOutcome::None => app.workflows.ui.wizard = Some(w),
            ui::WizardOutcome::Start => match w.spec() {
                Ok(spec) => start::start(app, spec),
                Err(e) => {
                    w.error = Some(e);
                    app.workflows.ui.wizard = Some(w);
                }
            },
        }
    } else if let Some(mut ed) = app.workflows.ui.queue.take() {
        let uid = ed.uid;
        let snapshot = app.workflows.queues.get(&uid).cloned().unwrap_or_default();
        let act = ed.handle_key(key, &snapshot);
        let keep = act != ui::QueueAct::Close;
        apply_queue_act(app, uid, act);
        if keep {
            app.workflows.ui.queue = Some(ed);
        }
    } else if let Some(mut st) = app.workflows.ui.compare.take() {
        let act = match app.workflows.best_run(st.run) {
            Some(run) => st.handle_key(key, run),
            None => ui::CompareAct::Close,
        };
        let id = st.run;
        let keep = act != ui::CompareAct::Close;
        if keep {
            app.workflows.ui.compare = Some(st);
        }
        actions::compare_act(app, id, act);
    }
    app.request_redraw();
}

fn apply_queue_act(app: &mut App, uid: usize, act: ui::QueueAct) {
    use ui::QueueAct as A;
    let wf = &mut app.workflows;
    match act {
        A::None | A::Close => {}
        A::Add(text) => {
            if !wf.queue_mut(uid).push(&text) {
                app.win.blocks_ui.show_toast("Queue is full or the task is empty");
            } else if app.agents.session(uid).is_some_and(|s| s.state == AgentState::Idle) && !app.workflows.managed(uid) {
                // An idle agent takes it right away (after the countdown).
                app.workflows.armed.insert(uid);
            }
        }
        A::Replace(i, text) => {
            wf.queue_mut(uid).set(i, &text);
        }
        A::Remove(i) => {
            wf.queue_mut(uid).remove(i);
        }
        A::MoveUp(i) => {
            wf.queue_mut(uid).move_up(i);
        }
        A::MoveDown(i) => {
            wf.queue_mut(uid).move_down(i);
        }
        A::Clear => wf.queue_mut(uid).clear(),
        A::TogglePause => {
            let q = wf.queue_mut(uid);
            q.paused = !q.paused;
            if !q.paused && !q.is_empty() && app.agents.session(uid).is_some_and(|s| s.state == AgentState::Idle) {
                app.workflows.armed.insert(uid);
            }
        }
    }
    app.workflows.drop_empty_queue(uid);
}

/// Mouse press while an overlay is open. True when consumed.
pub fn on_click(app: &mut App) -> bool {
    if !app.workflows.ui.visible() {
        return false;
    }
    let (x, y) = (app.win.cursor_x, app.win.cursor_y);
    if let Some(mut st) = app.workflows.ui.compare.take() {
        let act = st.click(x, y);
        let id = st.run;
        app.workflows.ui.compare = Some(st);
        actions::compare_act(app, id, act);
    }
    app.request_redraw();
    true
}

/// Draw the open overlay and the countdown toast.
pub fn render(wf: &mut Workflows, agents: &AgentRegistry, renderer: &mut crate::renderer::Renderer, buf: &mut [u32], w: usize, h: usize) {
    let font = &mut renderer.font;
    let theme = &renderer.theme;
    if let Some(wz) = wf.ui.wizard.as_mut() {
        wz.render(buf, w, h, font, theme);
    } else if let Some(ed) = wf.ui.queue.as_mut() {
        let q = wf.queues.get(&ed.uid).cloned().unwrap_or_default();
        ed.render(buf, w, h, font, theme, &q);
    } else if let Some(st) = wf.ui.compare.as_mut() {
        let id = st.run;
        if let Some(Run::Best(b)) = wf.runs.iter().find(|r| r.id() == id) {
            st.render(buf, w, h, font, theme, b);
        }
    } else if let Some(c) = &wf.countdown {
        let title = agents.session(c.uid).map(|s| s.title.clone()).unwrap_or_default();
        let msg = format!("Next task for {title} in {}s: {}   Esc pauses the queue", c.secs_left(Instant::now()), queue::preview(&c.task, 48));
        let tk = crate::ui::kit::Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = crate::ui::kit::Ctx::new(buf, w, h, font, &tk);
        cx.toast(Tone::Warning, &msg);
    }
}

/// Is the countdown (and so the Esc key) active?
pub fn countdown_active(app: &App) -> bool {
    app.workflows.countdown.is_some()
}

#[cfg(test)]
mod tests;
