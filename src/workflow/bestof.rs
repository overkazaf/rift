//! Best-of-N orchestration as a pure state machine: worktrees are created,
//! panes opened, the same task delivered to every candidate, and once all of
//! them finished a turn the results (diff, tests) are collected and the
//! compare view can open. `Ev` goes in, `Cmd` comes out; the glue in
//! `workflow/mod.rs` turns commands into git/test jobs and UI.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::agents::AgentKind;

// ───────────────────────────── naming ─────────────────────────────

/// Lowercase ASCII slug of a task for branch names: "Add rate limiting!" -> "add-rate-limiting".
pub fn slugify(task: &str, max: usize) -> String {
    let mut out = String::new();
    let mut dash = true; // swallow leading separators
    for c in task.chars() {
        if out.chars().count() >= max {
            break;
        }
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "task".to_string()
    } else {
        out
    }
}

/// Branch of candidate `i` (0-based): `agent/bestof-<slug>-<i+1>`.
pub fn branch_name(slug: &str, i: usize) -> String {
    format!("agent/bestof-{slug}-{}", i + 1)
}

/// Only branches this module creates may be deleted by "Discard".
pub fn is_candidate_branch(b: &str) -> bool {
    b.strip_prefix("agent/bestof-").is_some_and(|rest| !rest.is_empty() && !rest.contains("..") && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

/// Grid for `n` candidates (columns, rows).
pub fn grid_for(n: usize) -> (usize, usize) {
    match n {
        0 | 1 => (1, 1),
        2 => (2, 1),
        3 => (3, 1),
        _ => (2, 2),
    }
}

/// "Claude Code #1" when a kind appears more than once, else the plain name.
pub fn labels(kinds: &[AgentKind]) -> Vec<String> {
    kinds
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let same = kinds.iter().filter(|o| *o == k).count();
            if same > 1 {
                let nth = kinds[..=i].iter().filter(|o| *o == k).count();
                format!("{} #{nth}", k.name())
            } else {
                k.name().to_string()
            }
        })
        .collect()
}

// ───────────────────────────── model ─────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CandState {
    /// Worktree being created / pane being opened.
    Preparing,
    /// Agent started; the task may not have reached it yet.
    Launched,
    Working,
    Finished,
    /// The agent quit before finishing a turn.
    Exited,
}

impl CandState {
    pub fn is_done(&self) -> bool {
        matches!(self, CandState::Finished | CandState::Exited)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestStatus {
    /// No test command configured.
    None,
    Pending,
    Running,
    Passed,
    /// Failed (or could not run): the last lines of output.
    Failed(String),
}

impl TestStatus {
    pub fn is_settled(&self) -> bool {
        !matches!(self, TestStatus::Pending | TestStatus::Running)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffStat {
    pub files: usize,
    pub added: usize,
    pub removed: usize,
}

#[derive(Clone, Debug)]
pub struct Cand {
    pub kind: AgentKind,
    pub label: String,
    pub branch: String,
    pub dir: PathBuf,
    pub pane: Option<usize>,
    pub state: CandState,
    /// The task has been handed to the agent (as argument or typed).
    pub delivered: bool,
    pub started: Option<Instant>,
    pub finished: Option<Instant>,
    pub cost: Option<f64>,
    pub tokens: Option<u64>,
    pub diff: Option<DiffStat>,
    pub diff_err: Option<String>,
    pub tests: TestStatus,
    pub merged: bool,
    /// Worktree and branch were removed.
    pub removed: bool,
}

impl Cand {
    pub fn duration(&self) -> Option<Duration> {
        Some(self.finished?.saturating_duration_since(self.started?))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Creating,
    Running,
    Collecting,
    Ready,
    Aborted(String),
}

#[derive(Clone, Debug)]
pub struct BestOfN {
    pub id: u64,
    pub task: String,
    pub slug: String,
    pub repo: PathBuf,
    pub base_branch: Option<String>,
    /// Commit the worktrees were created from (what the diffs compare against).
    pub base_commit: String,
    pub test_cmd: String,
    pub cands: Vec<Cand>,
    pub phase: Phase,
}

pub enum Ev {
    /// Worktree creation finished: (dir, branch) per candidate, in order.
    Created(Result<Vec<(PathBuf, String)>, String>),
    /// Panes opened, in candidate order.
    Opened(Vec<usize>),
    /// The task reached the agent in this pane.
    Delivered(usize),
    TurnStarted(usize, Instant),
    TurnFinished { pane: usize, at: Instant, cost: Option<f64>, tokens: Option<u64> },
    Exited(usize, Instant),
    DiffDone(usize, Result<DiffStat, String>),
    TestsDone(usize, TestStatus),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// Compute candidate `i`'s diff against the base.
    Diff(usize),
    /// Run the test command in candidate `i`'s worktree.
    Tests(usize),
    /// Everything is collected: open the compare view.
    CompareReady,
    Abort(String),
}

impl BestOfN {
    pub fn new(id: u64, task: &str, repo: PathBuf, base_branch: Option<String>, base_commit: String, kinds: &[AgentKind], test_cmd: &str) -> BestOfN {
        let slug = slugify(task, 24);
        let names = labels(kinds);
        let cands = kinds
            .iter()
            .enumerate()
            .map(|(i, k)| Cand {
                kind: *k,
                label: names[i].clone(),
                branch: branch_name(&slug, i),
                dir: PathBuf::new(),
                pane: None,
                state: CandState::Preparing,
                delivered: false,
                started: None,
                finished: None,
                cost: None,
                tokens: None,
                diff: None,
                diff_err: None,
                tests: if test_cmd.trim().is_empty() { TestStatus::None } else { TestStatus::Pending },
                merged: false,
                removed: false,
            })
            .collect();
        BestOfN { id, task: task.to_string(), slug, repo, base_branch, base_commit, test_cmd: test_cmd.trim().to_string(), cands, phase: Phase::Creating }
    }

    pub fn index_of(&self, pane: usize) -> Option<usize> {
        self.cands.iter().position(|c| c.pane == Some(pane))
    }

    pub fn panes(&self) -> Vec<usize> {
        self.cands.iter().filter_map(|c| c.pane).collect()
    }

    pub fn finished_count(&self) -> usize {
        self.cands.iter().filter(|c| c.state.is_done()).count()
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.phase, Phase::Aborted(_))
    }

    pub fn on_event(&mut self, ev: Ev) -> Vec<Cmd> {
        match ev {
            Ev::Created(Err(e)) => {
                self.phase = Phase::Aborted(e.clone());
                vec![Cmd::Abort(e)]
            }
            Ev::Created(Ok(list)) => {
                for (c, (dir, branch)) in self.cands.iter_mut().zip(list) {
                    c.dir = dir;
                    c.branch = branch;
                }
                Vec::new()
            }
            Ev::Opened(uids) => {
                for (c, uid) in self.cands.iter_mut().zip(uids) {
                    c.pane = Some(uid);
                    c.state = CandState::Launched;
                }
                self.phase = Phase::Running;
                Vec::new()
            }
            Ev::Delivered(pane) => {
                if let Some(i) = self.index_of(pane) {
                    self.cands[i].delivered = true;
                }
                Vec::new()
            }
            Ev::TurnStarted(pane, at) => {
                let Some(i) = self.index_of(pane) else { return Vec::new() };
                let c = &mut self.cands[i];
                // The agent's own start-up output can look like a turn: only count
                // turns that began after the task was handed over.
                if !c.delivered || c.removed {
                    return Vec::new();
                }
                c.state = CandState::Working;
                c.started.get_or_insert(at);
                if matches!(self.phase, Phase::Collecting | Phase::Ready) {
                    // Someone kept going in a candidate's pane: results are stale.
                    c.finished = None;
                    c.diff = None;
                    c.diff_err = None;
                    self.phase = Phase::Running;
                }
                Vec::new()
            }
            Ev::TurnFinished { pane, at, cost, tokens } => {
                let Some(i) = self.index_of(pane) else { return Vec::new() };
                let c = &mut self.cands[i];
                if !c.delivered || !matches!(c.state, CandState::Launched | CandState::Working) {
                    return Vec::new();
                }
                c.state = CandState::Finished;
                c.finished = Some(at);
                c.cost = cost.or(c.cost);
                c.tokens = tokens.or(c.tokens);
                self.maybe_collect()
            }
            Ev::Exited(pane, at) => {
                let Some(i) = self.index_of(pane) else { return Vec::new() };
                let c = &mut self.cands[i];
                if !c.state.is_done() {
                    c.state = CandState::Exited;
                    c.finished = Some(at);
                    return self.maybe_collect();
                }
                Vec::new()
            }
            Ev::DiffDone(i, res) => {
                if let Some(c) = self.cands.get_mut(i) {
                    match res {
                        Ok(d) => {
                            c.diff = Some(d);
                            c.diff_err = None;
                        }
                        Err(e) => {
                            c.diff = None;
                            c.diff_err = Some(e);
                        }
                    }
                }
                self.maybe_ready()
            }
            Ev::TestsDone(i, status) => {
                if let Some(c) = self.cands.get_mut(i) {
                    c.tests = status;
                }
                self.maybe_ready()
            }
        }
    }

    /// All candidates done: start collecting diffs and test results.
    fn maybe_collect(&mut self) -> Vec<Cmd> {
        if self.phase != Phase::Running || !self.cands.iter().all(|c| c.state.is_done()) {
            return Vec::new();
        }
        let usable: Vec<usize> = self.cands.iter().enumerate().filter(|(_, c)| !c.removed).collect::<Vec<_>>().into_iter().map(|(i, _)| i).collect();
        if usable.is_empty() {
            let msg = "No candidate produced a result".to_string();
            self.phase = Phase::Aborted(msg.clone());
            return vec![Cmd::Abort(msg)];
        }
        self.phase = Phase::Collecting;
        let mut cmds = Vec::new();
        for i in usable {
            self.cands[i].diff = None;
            self.cands[i].diff_err = None;
            cmds.push(Cmd::Diff(i));
            if !self.test_cmd.is_empty() {
                self.cands[i].tests = TestStatus::Running;
                cmds.push(Cmd::Tests(i));
            }
        }
        cmds
    }

    fn maybe_ready(&mut self) -> Vec<Cmd> {
        if self.phase != Phase::Collecting {
            return Vec::new();
        }
        let ok = self.cands.iter().filter(|c| !c.removed).all(|c| (c.diff.is_some() || c.diff_err.is_some()) && c.tests.is_settled());
        if ok {
            self.phase = Phase::Ready;
            vec![Cmd::CompareReady]
        } else {
            Vec::new()
        }
    }

    /// Index of the cheapest-looking pick for the compare view's initial selection:
    /// the first candidate with passing tests, else the first with a diff.
    pub fn suggested(&self) -> usize {
        self.cands
            .iter()
            .position(|c| c.tests == TestStatus::Passed && c.diff.is_some_and(|d| d.files > 0))
            .or_else(|| self.cands.iter().position(|c| c.diff.is_some_and(|d| d.files > 0)))
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(n: usize, test: &str) -> BestOfN {
        let kinds = [AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Gemini, AgentKind::ClaudeCode];
        BestOfN::new(1, "Add rate limiting to /v1/orders", PathBuf::from("/w/app"), Some("main".into()), "abc123".into(), &kinds[..n], test)
    }

    fn open(r: &mut BestOfN) {
        let n = r.cands.len();
        let list = (0..n).map(|i| (PathBuf::from(format!("/w/app-bestof-{i}")), r.cands[i].branch.clone())).collect();
        assert!(r.on_event(Ev::Created(Ok(list))).is_empty());
        assert!(r.on_event(Ev::Opened((0..n).map(|i| 10 + i).collect())).is_empty());
        for i in 0..n {
            r.on_event(Ev::Delivered(10 + i));
        }
    }

    fn fin(pane: usize, at: Instant) -> Ev {
        Ev::TurnFinished { pane, at, cost: Some(0.5), tokens: Some(1000) }
    }

    #[test]
    fn slugs_and_branches() {
        assert_eq!(slugify("Add rate limiting to /v1/orders", 24), "add-rate-limiting-to-v1");
        assert_eq!(slugify("  --Hello,   World!!  ", 40), "hello-world");
        assert_eq!(slugify("\u{4fee}\u{590d}", 20), "task");
        assert_eq!(slugify("", 20), "task");
        assert_eq!(slugify("a b", 1), "a");
        assert_eq!(branch_name("fix-login", 0), "agent/bestof-fix-login-1");
        assert_eq!(branch_name("fix-login", 2), "agent/bestof-fix-login-3");
        assert!(is_candidate_branch("agent/bestof-fix-login-1"));
        assert!(!is_candidate_branch("main"));
        assert!(!is_candidate_branch("agent/claude-1"));
        assert!(!is_candidate_branch("agent/bestof-"));
        assert!(!is_candidate_branch("agent/bestof-x/../main"));
        assert!(!is_candidate_branch("agent/bestof-a b"));
    }

    #[test]
    fn labels_number_duplicates_only() {
        let l = labels(&[AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Gemini]);
        assert_eq!(l, ["Claude Code", "Codex", "Gemini CLI"]);
        let l = labels(&[AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::ClaudeCode]);
        assert_eq!(l, ["Claude Code #1", "Codex", "Claude Code #2"]);
        assert_eq!(grid_for(2), (2, 1));
        assert_eq!(grid_for(3), (3, 1));
        assert_eq!(grid_for(4), (2, 2));
    }

    #[test]
    fn happy_path_with_tests() {
        let t0 = Instant::now();
        let mut r = run(3, "cargo test");
        assert_eq!(r.phase, Phase::Creating);
        open(&mut r);
        assert_eq!(r.phase, Phase::Running);
        for i in 0..3 {
            assert!(r.on_event(Ev::TurnStarted(10 + i, t0)).is_empty());
            assert_eq!(r.cands[i].state, CandState::Working);
        }
        assert!(r.on_event(fin(10, t0 + Duration::from_secs(60))).is_empty());
        assert!(r.on_event(fin(11, t0 + Duration::from_secs(90))).is_empty());
        assert_eq!(r.finished_count(), 2);
        let cmds = r.on_event(fin(12, t0 + Duration::from_secs(120)));
        assert_eq!(cmds, vec![Cmd::Diff(0), Cmd::Tests(0), Cmd::Diff(1), Cmd::Tests(1), Cmd::Diff(2), Cmd::Tests(2)]);
        assert_eq!(r.phase, Phase::Collecting);
        assert_eq!(r.cands[0].duration(), Some(Duration::from_secs(60)));
        assert_eq!(r.cands[0].cost, Some(0.5));
        // Results arrive in any order; Ready only when all are in.
        assert!(r.on_event(Ev::DiffDone(2, Ok(DiffStat { files: 3, added: 40, removed: 2 }))).is_empty());
        assert!(r.on_event(Ev::TestsDone(2, TestStatus::Passed)).is_empty());
        assert!(r.on_event(Ev::DiffDone(0, Ok(DiffStat { files: 1, added: 5, removed: 0 }))).is_empty());
        assert!(r.on_event(Ev::DiffDone(1, Err("git failed".into()))).is_empty());
        assert!(r.on_event(Ev::TestsDone(0, TestStatus::Failed("boom".into()))).is_empty());
        let cmds = r.on_event(Ev::TestsDone(1, TestStatus::Passed));
        assert_eq!(cmds, vec![Cmd::CompareReady]);
        assert_eq!(r.phase, Phase::Ready);
        assert_eq!(r.suggested(), 2, "first candidate that passes and changed something");
        // Extra events after Ready change nothing.
        assert!(r.on_event(Ev::TestsDone(1, TestStatus::Passed)).is_empty());
    }

    #[test]
    fn without_a_test_command_ready_follows_the_diffs() {
        let t0 = Instant::now();
        let mut r = run(2, "");
        open(&mut r);
        assert!(r.cands.iter().all(|c| c.tests == TestStatus::None));
        r.on_event(Ev::TurnStarted(10, t0));
        r.on_event(Ev::TurnStarted(11, t0));
        r.on_event(fin(10, t0));
        let cmds = r.on_event(fin(11, t0));
        assert_eq!(cmds, vec![Cmd::Diff(0), Cmd::Diff(1)]);
        r.on_event(Ev::DiffDone(0, Ok(DiffStat::default())));
        assert_eq!(r.on_event(Ev::DiffDone(1, Ok(DiffStat { files: 1, added: 1, removed: 0 }))), vec![Cmd::CompareReady]);
        assert_eq!(r.suggested(), 1);
    }

    #[test]
    fn startup_output_before_delivery_is_not_a_turn() {
        let t0 = Instant::now();
        let mut r = run(2, "");
        let list = (0..2).map(|i| (PathBuf::from(format!("/w/{i}")), r.cands[i].branch.clone())).collect();
        r.on_event(Ev::Created(Ok(list)));
        r.on_event(Ev::Opened(vec![10, 11]));
        // The agents paint their UI: a start-up "turn" while the task is not delivered yet.
        r.on_event(Ev::TurnStarted(10, t0));
        assert!(r.on_event(fin(10, t0)).is_empty());
        assert_eq!(r.cands[0].state, CandState::Launched);
        r.on_event(Ev::Delivered(10));
        r.on_event(Ev::Delivered(11));
        r.on_event(Ev::TurnStarted(10, t0));
        r.on_event(Ev::TurnStarted(11, t0));
        assert!(r.on_event(fin(10, t0)).is_empty());
        assert!(!r.on_event(fin(11, t0)).is_empty());
    }

    #[test]
    fn an_exited_candidate_does_not_block_the_others() {
        let t0 = Instant::now();
        let mut r = run(3, "");
        open(&mut r);
        for i in 0..3 {
            r.on_event(Ev::TurnStarted(10 + i, t0));
        }
        assert!(r.on_event(Ev::Exited(11, t0)).is_empty());
        assert_eq!(r.cands[1].state, CandState::Exited);
        r.on_event(fin(10, t0));
        let cmds = r.on_event(fin(12, t0));
        assert_eq!(cmds.len(), 3, "an exited agent may still have left changes: {cmds:?}");
        // A late Exited for a finished candidate is ignored.
        assert!(r.on_event(Ev::Exited(10, t0)).is_empty());
        assert_eq!(r.cands[0].state, CandState::Finished);
    }

    #[test]
    fn nothing_left_to_collect_aborts() {
        let mut r = run(2, "");
        open(&mut r);
        r.cands[0].state = CandState::Exited;
        r.cands[1].state = CandState::Exited;
        r.cands[0].removed = true;
        r.cands[1].removed = true;
        r.phase = Phase::Running;
        let cmds = r.maybe_collect();
        assert!(matches!(cmds.as_slice(), [Cmd::Abort(_)]));
        assert!(!r.is_active());
    }

    #[test]
    fn worktree_failure_aborts() {
        let mut r = run(2, "");
        let cmds = r.on_event(Ev::Created(Err("git worktree add failed".into())));
        assert_eq!(cmds, vec![Cmd::Abort("git worktree add failed".into())]);
        assert!(!r.is_active());
        // Nothing else moves an aborted run.
        assert!(r.on_event(Ev::Opened(vec![1, 2])).is_empty() || r.phase != Phase::Creating);
    }

    #[test]
    fn continuing_a_candidate_after_ready_makes_results_stale() {
        let t0 = Instant::now();
        let mut r = run(2, "");
        open(&mut r);
        r.on_event(Ev::TurnStarted(10, t0));
        r.on_event(Ev::TurnStarted(11, t0));
        r.on_event(fin(10, t0));
        r.on_event(fin(11, t0));
        r.on_event(Ev::DiffDone(0, Ok(DiffStat::default())));
        r.on_event(Ev::DiffDone(1, Ok(DiffStat::default())));
        assert_eq!(r.phase, Phase::Ready);
        r.on_event(Ev::TurnStarted(10, t0 + Duration::from_secs(5)));
        assert_eq!(r.phase, Phase::Running);
        assert!(r.cands[0].diff.is_none());
        let cmds = r.on_event(fin(10, t0 + Duration::from_secs(9)));
        assert_eq!(cmds, vec![Cmd::Diff(0), Cmd::Diff(1)]);
    }

    #[test]
    fn unknown_panes_are_ignored() {
        let mut r = run(2, "");
        open(&mut r);
        assert!(r.on_event(Ev::TurnStarted(999, Instant::now())).is_empty());
        assert!(r.on_event(fin(999, Instant::now())).is_empty());
        assert_eq!(r.index_of(11), Some(1));
        assert_eq!(r.panes(), vec![10, 11]);
    }
}
