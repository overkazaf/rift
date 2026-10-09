//! The set of supervised agent sessions across all panes.
//!
//! `AgentRegistry` is deliberately independent of `App`: the runtime (see
//! `runtime.rs`) feeds it plain observations, which keeps it testable with
//! synthetic timelines. The public surface is documented in `agents/mod.rs`.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};

use super::detect::{self, Detection, ProcInfo};
use super::git::{self, RepoInfo};
use super::inbox::HookEvent;
use super::state::{self, classify_notification, Effect, HookKind, Machine, Timing};
use super::{AgentEvent, AgentKind, AgentSession, AgentState, AgentsConfig, DetectSource};

/// How often the foreground process of a busy pane is re-checked.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(1);
/// Minimum spacing of screen scans per pane.
pub const SCAN_INTERVAL: Duration = Duration::from_millis(250);
/// How long a finished session stays listed.
pub const LINGER: Duration = Duration::from_secs(60);
/// Git facts are re-read this often (branch switches).
const GIT_REFRESH: Duration = Duration::from_secs(5);
const MAX_EVENTS: usize = 1024;

/// What a foreground-process probe found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probe {
    /// A job other than the shell owns the terminal.
    Foreground(ProcInfo),
    /// The shell itself is in the foreground (prompt).
    AtPrompt,
    /// Could not tell (SSH pane, process gone, unsupported OS).
    Unknown,
}

/// What the runtime knows about one pane right now.
#[derive(Clone, Debug, Default)]
pub struct PaneObs {
    /// Stable id (`Pane::id`).
    pub uid: usize,
    pub tab_index: usize,
    /// OSC 133/633 text of the running command.
    pub running_cmd: Option<String>,
    pub block_running: bool,
    /// The shell speaks OSC 133 (so "no block running" means "at the prompt").
    pub osc_seen: bool,
    pub title: Option<String>,
    pub cwd: Option<String>,
    /// The pane's shell exited.
    pub shell_exited: bool,
    /// Exit code of the last finished block.
    pub last_exit: Option<i32>,
    /// Non-echo output bytes so far.
    pub bytes: u64,
    pub last_output: Option<Instant>,
    pub last_input: Option<Instant>,
    pub last_submit: Option<Instant>,
}

struct PaneTrack {
    next_probe: Instant,
    prev_running: bool,
    /// Latest probe outcome.
    probe: Probe,
    seen_bytes: u64,
    seen_input: Option<Instant>,
    seen_submit: Option<Instant>,
    scanned_bytes: u64,
    last_scan: Option<Instant>,
}

impl PaneTrack {
    fn new(now: Instant) -> Self {
        Self {
            next_probe: now,
            prev_running: false,
            probe: Probe::Unknown,
            seen_bytes: 0,
            seen_input: None,
            seen_submit: None,
            scanned_bytes: u64::MAX,
            last_scan: None,
        }
    }
}

pub struct AgentRegistry {
    sessions: Vec<AgentSession>,
    events: VecDeque<AgentEvent>,
    tap: VecDeque<AgentEvent>,
    tracks: HashMap<usize, PaneTrack>,
    git_cache: HashMap<String, (Instant, Option<RepoInfo>)>,
    timing: Timing,
    approval_extra: Vec<String>,
    enabled: bool,
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            events: VecDeque::new(),
            tap: VecDeque::new(),
            tracks: HashMap::new(),
            git_cache: HashMap::new(),
            timing: Timing::default(),
            approval_extra: Vec::new(),
            enabled: true,
        }
    }

    /// Apply `[agents]` settings.
    pub fn configure(&mut self, cfg: &AgentsConfig) {
        self.enabled = cfg.enabled;
        self.approval_extra = cfg.approval_patterns.clone();
    }

    #[cfg(test)]
    pub fn with_timing(timing: Timing) -> Self {
        Self { timing, ..Self::new() }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    // ───────────── public API ─────────────

    /// All sessions, oldest first.
    pub fn sessions(&self) -> &[AgentSession] {
        &self.sessions
    }

    pub fn session(&self, pane_uid: usize) -> Option<&AgentSession> {
        self.sessions.iter().find(|s| s.pane_uid == pane_uid)
    }

    /// Lifecycle events since the last call.
    pub fn drain_events(&mut self) -> Vec<AgentEvent> {
        self.events.drain(..).collect()
    }

    /// Number of live sessions waiting for the user.
    pub fn attention_count(&self) -> usize {
        self.sessions.iter().filter(|s| s.needs_attention).count()
    }

    /// Number of live (not finished) sessions.
    pub fn live_count(&self) -> usize {
        self.sessions.iter().filter(|s| s.state.is_live()).count()
    }

    /// The next waiting session after `after` (cyclic, in list order).
    pub fn next_attention(&self, after: Option<usize>) -> Option<usize> {
        let n = self.sessions.len();
        if n == 0 {
            return None;
        }
        let start = after.and_then(|u| self.sessions.iter().position(|s| s.pane_uid == u)).map_or(0, |i| i + 1);
        (0..n).map(|k| (start + k) % n).map(|i| &self.sessions[i]).find(|s| s.needs_attention).map(|s| s.pane_uid)
    }

    // ───────────── crate-internal API (runtime / UI) ─────────────

    /// Internal tap used for desktop notifications (separate from the public queue).
    pub(crate) fn drain_tap(&mut self) -> Vec<AgentEvent> {
        self.tap.drain(..).collect()
    }

    /// Any session that is animated (working / waiting / starting).
    pub fn animating(&self) -> bool {
        self.sessions.iter().any(|s| matches!(s.state, AgentState::Working | AgentState::WaitingForUser | AgentState::Starting))
    }

    /// Earliest moment a time-based transition could happen.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.sessions.iter().filter_map(|s| s.machine.next_deadline()).min()
    }

    /// Panes that currently have a live session.
    pub fn has_live(&self, uid: usize) -> bool {
        self.session(uid).is_some_and(|s| s.state.is_live())
    }

    fn push_event(&mut self, ev: AgentEvent) {
        for q in [&mut self.events, &mut self.tap] {
            if q.len() >= MAX_EVENTS {
                q.pop_front();
            }
            q.push_back(ev.clone());
        }
    }

    fn repo_for(&mut self, cwd: &str, now: Instant) -> Option<RepoInfo> {
        if let Some((at, info)) = self.git_cache.get(cwd) {
            if now.saturating_duration_since(*at) < GIT_REFRESH {
                return info.clone();
            }
        }
        let info = git::repo_info(Path::new(cwd));
        if self.git_cache.len() > 64 {
            self.git_cache.clear();
        }
        self.git_cache.insert(cwd.to_string(), (now, info.clone()));
        info
    }

    fn refresh_meta(&mut self, idx: usize, cwd: Option<&str>, now: Instant) {
        let info = cwd.and_then(|c| self.repo_for(c, now));
        let s = &mut self.sessions[idx];
        s.cwd = cwd.map(str::to_string).or_else(|| s.cwd.take());
        match info {
            Some(i) => {
                s.git_root = Some(i.root.to_string_lossy().into_owned());
                s.repo = Some(i.name);
                s.branch = i.branch;
            }
            None => {
                s.git_root = None;
                s.repo = None;
                s.branch = None;
            }
        }
    }

    /// Headless screenshots: pin where an agent runs without asking git.
    pub(crate) fn pin_place(&mut self, uid: usize, git_root: &str, repo: &str, branch: &str) {
        if let Some(i) = self.index_of(uid) {
            let s = &mut self.sessions[i];
            s.git_root = Some(git_root.to_string());
            s.repo = Some(repo.to_string());
            s.branch = Some(branch.to_string());
        }
    }

    fn index_of(&self, uid: usize) -> Option<usize> {
        self.sessions.iter().position(|s| s.pane_uid == uid)
    }

    fn create(&mut self, det: Detection, obs: &PaneObs, now: Instant) -> usize {
        // A finished session in the same pane is replaced.
        self.sessions.retain(|s| s.pane_uid != obs.uid);
        let mut machine = Machine::with_timing(now, self.timing);
        machine.last_activity = now;
        let session = AgentSession {
            pane_uid: obs.uid,
            tab_index: obs.tab_index,
            kind: det.kind,
            state: AgentState::Starting,
            title: det.kind.name().to_string(),
            started_at: now,
            last_activity: now,
            cwd: obs.cwd.clone(),
            git_root: None,
            repo: None,
            branch: None,
            needs_attention: false,
            turn_counter: 0,
            last_turn_started_at: None,
            waiting_reason: None,
            preview: String::new(),
            source: det.source,
            state_since: now,
            machine,
        };
        self.sessions.push(session);
        let idx = self.sessions.len() - 1;
        self.refresh_meta(idx, obs.cwd.as_deref(), now);
        let t = self.tracks.entry(obs.uid).or_insert_with(|| PaneTrack::new(now));
        // Inputs before detection (the Enter that launched the agent) don't count.
        t.seen_bytes = obs.bytes;
        t.seen_input = obs.last_input;
        t.seen_submit = obs.last_submit;
        t.scanned_bytes = u64::MAX;
        idx
    }

    /// Copy machine state to the public fields and emit events.
    fn apply(&mut self, idx: usize, effects: Vec<Effect>) {
        let (uid, kind, cwd, git_root) = {
            let s = &self.sessions[idx];
            (s.pane_uid, s.kind, s.cwd.clone(), s.git_root.clone())
        };
        let mut evs = Vec::new();
        {
            let s = &mut self.sessions[idx];
            let m = &s.machine;
            s.state = m.state;
            s.state_since = m.since;
            s.needs_attention = m.state == AgentState::WaitingForUser;
            s.waiting_reason = m.waiting_reason.clone();
            s.turn_counter = m.turn;
            s.last_turn_started_at = m.last_turn_started;
            s.last_activity = m.last_activity;
            for e in effects {
                evs.push(match e {
                    Effect::TurnStarted => AgentEvent::TurnStarted { pane_uid: uid, kind, turn: s.turn_counter, cwd: cwd.clone(), git_root: git_root.clone() },
                    Effect::NeedsUser(reason) => AgentEvent::NeedsUser { pane_uid: uid, kind, reason, cwd: cwd.clone(), git_root: git_root.clone() },
                    Effect::TurnFinished(elapsed) => AgentEvent::TurnFinished {
                        pane_uid: uid,
                        kind,
                        turn: s.turn_counter,
                        elapsed,
                        cwd: cwd.clone(),
                        git_root: git_root.clone(),
                    },
                    Effect::Exited(exit) => AgentEvent::Exited { pane_uid: uid, kind, exit, cwd: cwd.clone(), git_root: git_root.clone() },
                });
            }
        }
        for e in evs {
            self.push_event(e);
        }
    }

    /// Sessions whose pane no longer exists are ended and dropped.
    pub fn retain_panes(&mut self, alive: &[usize], now: Instant) {
        let gone: Vec<usize> = self.sessions.iter().map(|s| s.pane_uid).filter(|u| !alive.contains(u)).collect();
        for uid in gone {
            if let Some(i) = self.index_of(uid) {
                let fx = self.sessions[i].machine.on_exit(now, None);
                self.apply(i, fx);
            }
            self.sessions.retain(|s| s.pane_uid != uid);
        }
        self.tracks.retain(|u, _| alive.contains(u));
    }

    /// Detection + liveness + activity for one pane. `probe` is called at most
    /// once, and only when a foreground-process check is due.
    pub fn observe_pane(&mut self, obs: &PaneObs, probe: &mut dyn FnMut() -> Probe, now: Instant) {
        if !self.enabled {
            return;
        }
        let has_session = self.has_live(obs.uid);
        let track = self.tracks.entry(obs.uid).or_insert_with(|| PaneTrack::new(now));
        let block_start = obs.block_running && !track.prev_running;
        track.prev_running = obs.block_running;
        // Probe while something runs, when there is no shell integration to tell,
        // or to confirm a known session.
        let want_probe = obs.block_running || !obs.osc_seen || has_session;
        if want_probe && (block_start || now >= track.next_probe) && !obs.shell_exited {
            track.next_probe = now + PROBE_INTERVAL;
            track.probe = probe();
        } else if !want_probe {
            track.probe = Probe::Unknown;
        }
        let probe_now = track.probe.clone();

        // Candidate agent for this pane.
        let from_cmd = obs.running_cmd.as_deref().filter(|_| obs.block_running).and_then(detect::detect_from_command);
        let from_proc = match &probe_now {
            Probe::Foreground(p) => detect::detect_from_process(p),
            _ => None,
        };
        let title_ok = obs.block_running || !obs.osc_seen;
        let from_title = obs.title.as_deref().filter(|_| title_ok).and_then(detect::detect_from_title);
        let det = from_cmd
            .map(|kind| Detection { kind, source: DetectSource::Command })
            .or(from_proc.map(|kind| Detection { kind, source: DetectSource::Process }))
            .or(from_title.map(|kind| Detection { kind, source: DetectSource::Title }));

        // Is the agent's command still the pane's foreground job?
        let running = if obs.osc_seen {
            obs.block_running
        } else {
            matches!(probe_now, Probe::Foreground(_)) || (matches!(probe_now, Probe::Unknown) && det.is_some())
        };

        match self.index_of(obs.uid) {
            Some(i) if self.sessions[i].state.is_live() => {
                if obs.shell_exited || !running {
                    let fx = self.sessions[i].machine.on_exit(now, obs.last_exit.filter(|_| obs.osc_seen));
                    self.apply(i, fx);
                } else {
                    // Upgrade a weak detection (title / hook) to a stronger one.
                    if let Some(d) = det {
                        let s = &mut self.sessions[i];
                        if matches!(s.source, DetectSource::Title | DetectSource::Hook) && d.source != DetectSource::Title && d.kind != s.kind {
                            s.kind = d.kind;
                            s.title = d.kind.name().to_string();
                            s.source = d.source;
                        }
                    }
                    self.feed(i, obs, now);
                }
            }
            _ => {
                if let Some(d) = det {
                    if running && !obs.shell_exited {
                        let i = self.create(d, obs, now);
                        self.feed(i, obs, now);
                    }
                }
            }
        }
    }

    /// Output / input / metadata of a live session.
    fn feed(&mut self, idx: usize, obs: &PaneObs, now: Instant) {
        let uid = obs.uid;
        let (new_bytes, new_input, new_submit) = {
            let t = self.tracks.get_mut(&uid).expect("track exists for observed pane");
            let nb = obs.bytes.saturating_sub(t.seen_bytes);
            t.seen_bytes = obs.bytes;
            let ni = obs.last_input.filter(|i| t.seen_input.map_or(true, |s| *i > s));
            if ni.is_some() {
                t.seen_input = obs.last_input;
            }
            let ns = obs.last_submit.filter(|i| t.seen_submit.map_or(true, |s| *i > s));
            if ns.is_some() {
                t.seen_submit = obs.last_submit;
            }
            (nb, ni, ns)
        };
        let mut fx = Vec::new();
        // Input first: an answer typed before the output that follows it.
        if let Some(t) = new_input {
            self.sessions[idx].machine.on_user_input(t, new_submit.is_some());
        }
        if new_bytes > 0 {
            let at = obs.last_output.unwrap_or(now);
            fx.extend(self.sessions[idx].machine.on_activity(at, new_bytes));
        }
        self.sessions[idx].tab_index = obs.tab_index;
        if obs.cwd.is_some() {
            let cwd = obs.cwd.clone();
            self.refresh_meta(idx, cwd.as_deref(), now);
        }
        self.apply(idx, fx);
    }

    /// Output arrived since the last screen scan (possibly throttled right now).
    pub fn scan_pending(&self, uid: usize, bytes: u64) -> bool {
        self.has_live(uid) && self.tracks.get(&uid).is_some_and(|t| t.scanned_bytes != bytes)
    }

    /// Should the runtime hand over the screen text of this pane now?
    pub fn wants_scan(&self, uid: usize, bytes: u64, now: Instant) -> bool {
        let Some(s) = self.session(uid).filter(|s| s.state.is_live()) else { return false };
        let _ = s;
        let Some(t) = self.tracks.get(&uid) else { return false };
        let due = t.last_scan.map_or(true, |l| now.saturating_duration_since(l) >= SCAN_INTERVAL);
        due && t.scanned_bytes != bytes
    }

    /// Feed a screen snapshot (visible rows, top to bottom).
    pub fn observe_screen(&mut self, uid: usize, lines: &[String], bytes: u64, now: Instant) {
        let Some(i) = self.index_of(uid) else { return };
        if let Some(t) = self.tracks.get_mut(&uid) {
            t.scanned_bytes = bytes;
            t.last_scan = Some(now);
        }
        let prompt = state::match_approval(lines, &self.approval_extra);
        let has_prompt = prompt.is_some();
        self.sessions[i].machine.on_screen(now, prompt);
        // A spinner on screen = working, whatever the byte stream looked like.
        let busy = !has_prompt && state::screen_busy(lines);
        let mut fx = self.sessions[i].machine.on_screen_busy(now, busy);
        if let Some(p) = preview_line(lines) {
            self.sessions[i].preview = p;
        }
        fx.extend(self.sessions[i].machine.tick(now));
        self.apply(i, fx);
    }

    /// OSC 9 / 777 notification from the pane.
    pub fn observe_notification(&mut self, uid: usize, title: &str, body: &str, now: Instant) {
        let Some(i) = self.index_of(uid) else { return };
        if !body.trim().is_empty() {
            self.sessions[i].preview = body.trim().chars().take(160).collect();
        }
        let fx = self.sessions[i].machine.on_notification(now, classify_notification(title, body));
        self.apply(i, fx);
    }

    /// Resolve the pane a hook belongs to: explicit id, else a unique cwd match.
    fn hook_target(&self, ev: &HookEvent, pane_exists: &dyn Fn(usize) -> bool) -> Option<usize> {
        if let Some(id) = ev.pane_id {
            if pane_exists(id) {
                return Some(id);
            }
        }
        let cwd = ev.cwd.as_deref()?;
        let c = Path::new(cwd);
        let mut hits = self.sessions.iter().filter(|s| {
            s.state.is_live()
                && (s.cwd.as_deref().is_some_and(|x| Path::new(x) == c)
                    || s.git_root.as_deref().is_some_and(|g| c.starts_with(g)))
        });
        let first = hits.next()?;
        hits.next().is_none().then_some(first.pane_uid)
    }

    /// `rift agent-event` hook. `obs_for` gives a fresh observation for a pane id
    /// (used when a hook is the first sign of an agent there).
    pub fn observe_hook(&mut self, ev: &HookEvent, pane_exists: &dyn Fn(usize) -> bool, obs_for: &dyn Fn(usize) -> Option<PaneObs>, now: Instant) {
        if !self.enabled {
            return;
        }
        let Some(uid) = self.hook_target(ev, pane_exists) else { return };
        let idx = match self.index_of(uid).filter(|i| self.sessions[*i].state.is_live()) {
            Some(i) => i,
            None => {
                // Only "something is happening" hooks may conjure a session.
                if !matches!(ev.state, HookKind::Working | HookKind::Waiting) {
                    return;
                }
                let Some(obs) = obs_for(uid) else { return };
                let det = Detection { kind: ev.agent.unwrap_or(AgentKind::ClaudeCode), source: DetectSource::Hook };
                self.create(det, &obs, now)
            }
        };
        if let Some(k) = ev.agent {
            // A hook that names its agent beats a weak guess.
            let s = &mut self.sessions[idx];
            if matches!(s.source, DetectSource::Title | DetectSource::Hook) && s.kind != k {
                s.kind = k;
                s.title = k.name().to_string();
            }
        }
        if let Some(m) = ev.message.as_deref() {
            self.sessions[idx].preview = m.chars().take(160).collect();
        }
        let fx = self.sessions[idx].machine.on_hook(now, ev.state, ev.message.as_deref());
        self.apply(idx, fx);
    }

    /// Time-based transitions and expiry of finished sessions.
    pub fn tick(&mut self, now: Instant) {
        for i in 0..self.sessions.len() {
            if self.sessions[i].state.is_live() {
                let fx = self.sessions[i].machine.tick(now);
                self.apply(i, fx);
            }
        }
        self.sessions.retain(|s| s.state.is_live() || now.saturating_duration_since(s.state_since) < LINGER);
    }
}

/// Strip box-drawing characters and collapse whitespace.
fn clean_line(l: &str) -> String {
    l.chars()
        .map(|c| if matches!(c, '\u{2500}'..='\u{259f}') { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Status lines and footers agents draw around their conversation: never a
/// useful preview (vim-mode indicator, shortcut hints, mode toggles, token
/// counters, model / cost status lines).
pub fn is_chrome_line(clean: &str) -> bool {
    let l = clean.to_lowercase();
    let words = l.split_whitespace().count();
    // "-- INSERT --", "-- NORMAL --", "-- VISUAL LINE --"
    if l.starts_with("--") && l.ends_with("--") {
        return true;
    }
    const HINTS: &[&str] = &[
        "? for shortcuts", "for shortcuts", "auto mode", "auto-accept", "accept edits", "bypass permissions",
        "plan mode", "shift+tab", "to cycle", "to interrupt", "ctrl+v to paste", "image in clipboard",
        "ctrl+o", "ctrl+r", "ctrl+c", "ctrl+d", "esc to", "tab to", "press enter", "press esc", "/help",
        "for newline", "? for", "context left", "no sandbox", "(shift", "⏵⏵",
    ];
    if HINTS.iter().any(|h| l.contains(h)) {
        return true;
    }
    // Token counters: "57929 tokens", "↓ 1.2k tokens", "12.3k tokens · 3s".
    if l.contains("tokens") && words <= 8 && l.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    // Model / cost / context status lines: "Opus 4.1 | ctx 12% | $0.42".
    let model = ["opus", "sonnet", "haiku", "gpt-", "gemini"].iter().any(|m| l.contains(m));
    if (model || l.contains("ctx") || l.contains("context:")) && (l.contains('%') || l.contains('$') || l.contains('|')) && words <= 12 {
        return true;
    }
    if l.starts_with('$') && words <= 3 {
        return true;
    }
    false
}

fn meaningful(l: &str) -> bool {
    l.chars().filter(|c| c.is_alphanumeric()).count() >= 3
}

/// Last meaningful line of the screen for the sidebar preview: skips the
/// agent's own status lines and footers (see [`is_chrome_line`]). A spinner
/// line is only used when nothing better is on screen (and then without its
/// timer / token parenthesis).
pub fn preview_line(lines: &[String]) -> Option<String> {
    let cleaned: Vec<String> = lines.iter().rev().map(|l| clean_line(l)).collect();
    let pick = |l: &String| l.chars().take(160).collect::<String>();
    if let Some(l) = cleaned
        .iter()
        .find(|l| meaningful(l) && !is_chrome_line(l) && !state::is_spinner_line(&l.to_lowercase()))
    {
        return Some(pick(l));
    }
    cleaned.iter().find(|l| state::is_spinner_line(&l.to_lowercase())).map(|l| {
        let base = l.split(" (").next().unwrap_or(l);
        pick(&base.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn obs(uid: usize) -> PaneObs {
        PaneObs { uid, osc_seen: true, ..Default::default() }
    }

    fn no_probe() -> Probe {
        Probe::Unknown
    }

    fn drain_kinds(r: &mut AgentRegistry) -> Vec<&'static str> {
        r.drain_events()
            .iter()
            .map(|e| match e {
                AgentEvent::TurnStarted { .. } => "start",
                AgentEvent::NeedsUser { .. } => "needs",
                AgentEvent::TurnFinished { .. } => "finish",
                AgentEvent::Exited { .. } => "exit",
            })
            .collect()
    }

    fn tmp_repo(name: &str, branch: &str) -> String {
        let root = std::env::temp_dir().join(format!("rift-reg-{name}-{}", std::process::id())).join("proj");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
        root.to_string_lossy().into_owned()
    }

    #[test]
    fn detects_from_block_command_and_ends_with_the_block() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        let cwd = tmp_repo("cmd", "main");
        let mut o = obs(7);
        o.cwd = Some(cwd.clone());
        // At the prompt: nothing.
        r.observe_pane(&o, &mut no_probe, t0);
        assert!(r.sessions().is_empty());
        // `claude` starts.
        o.block_running = true;
        o.running_cmd = Some("claude --resume".into());
        r.observe_pane(&o, &mut no_probe, t0 + ms(100));
        let s = r.session(7).expect("session");
        assert_eq!(s.kind, AgentKind::ClaudeCode);
        assert_eq!(s.state, AgentState::Starting);
        assert_eq!(s.source, DetectSource::Command);
        assert_eq!(s.repo.as_deref(), Some("proj"));
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.git_root.as_deref(), Some(cwd.as_str()));
        // It exits with 0.
        o.block_running = false;
        o.running_cmd = None;
        o.last_exit = Some(0);
        r.observe_pane(&o, &mut no_probe, t0 + ms(5000));
        assert_eq!(r.session(7).unwrap().state, AgentState::Done { exit: Some(0) });
        let evs = r.drain_events();
        assert!(matches!(evs.as_slice(), [AgentEvent::Exited { pane_uid: 7, exit: Some(0), git_root: Some(g), .. }] if *g == cwd));
        // The finished session lingers, then disappears.
        r.tick(t0 + ms(5000) + LINGER - ms(1));
        assert_eq!(r.sessions().len(), 1);
        r.tick(t0 + ms(5000) + LINGER + ms(1));
        assert!(r.sessions().is_empty());
    }

    #[test]
    fn non_agent_commands_are_ignored() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        let mut o = obs(1);
        o.block_running = true;
        o.running_cmd = Some("cargo build".into());
        r.observe_pane(&o, &mut no_probe, t0);
        r.observe_pane(&o, &mut no_probe, t0 + ms(2000));
        assert!(r.sessions().is_empty());
    }

    #[test]
    fn nonzero_exit_is_an_error() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        let mut o = obs(1);
        o.block_running = true;
        o.running_cmd = Some("codex".into());
        r.observe_pane(&o, &mut no_probe, t0);
        o.block_running = false;
        o.last_exit = Some(2);
        r.observe_pane(&o, &mut no_probe, t0 + ms(100));
        assert_eq!(r.session(1).unwrap().state, AgentState::Error);
    }

    #[test]
    fn detects_from_foreground_process_when_the_command_is_an_alias() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        let mut o = obs(2);
        o.block_running = true;
        o.running_cmd = Some("cc".into()); // alias
        let probes = std::cell::Cell::new(0);
        let mut probe = || {
            probes.set(probes.get() + 1);
            Probe::Foreground(ProcInfo { pid: 99, path: "/usr/local/bin/claude".into(), args: vec!["claude".into()] })
        };
        r.observe_pane(&o, &mut probe, t0);
        assert_eq!(r.session(2).unwrap().source, DetectSource::Process);
        // Probing is rate limited to ~1/s.
        r.observe_pane(&o, &mut probe, t0 + ms(300));
        r.observe_pane(&o, &mut probe, t0 + ms(600));
        assert_eq!(probes.get(), 1);
        r.observe_pane(&o, &mut probe, t0 + ms(1100));
        assert_eq!(probes.get(), 2);
    }

    #[test]
    fn works_without_shell_integration_via_process_probe() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        let mut o = obs(3);
        o.osc_seen = false;
        let agent = Probe::Foreground(ProcInfo { pid: 5, path: "/usr/bin/node".into(), args: vec!["node".into(), "/usr/local/bin/gemini".into()] });
        let a2 = agent.clone();
        r.observe_pane(&o, &mut || a2.clone(), t0);
        assert_eq!(r.session(3).unwrap().kind, AgentKind::Gemini);
        // Shell back in the foreground: the agent is gone.
        r.observe_pane(&o, &mut || Probe::AtPrompt, t0 + ms(1500));
        assert!(matches!(r.session(3).unwrap().state, AgentState::Done { .. }));
        assert_eq!(drain_kinds(&mut r), vec!["exit"]);
    }

    #[test]
    fn title_detection_is_gated_by_a_running_command() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        let mut o = obs(4);
        o.title = Some("\u{2733} Claude Code".into());
        // At a shell prompt with OSC 133: a leftover title means nothing.
        r.observe_pane(&o, &mut no_probe, t0);
        assert!(r.sessions().is_empty());
        o.block_running = true;
        o.running_cmd = Some("./run.sh".into());
        r.observe_pane(&o, &mut no_probe, t0 + ms(10));
        assert_eq!(r.session(4).unwrap().source, DetectSource::Title);
        // Later the probe identifies the real program: upgraded.
        let p = Probe::Foreground(ProcInfo { pid: 1, path: "/x/codex".into(), args: vec!["codex".into()] });
        r.observe_pane(&o, &mut || p.clone(), t0 + ms(1500));
        let s = r.session(4).unwrap();
        assert_eq!((s.kind, s.source), (AgentKind::Codex, DetectSource::Process));
    }

    #[test]
    fn a_full_turn_emits_events_in_order_with_cwd_and_git_root() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        let cwd = tmp_repo("turn", "feat");
        let mut o = obs(5);
        o.cwd = Some(cwd.clone());
        o.block_running = true;
        o.running_cmd = Some("claude".into());
        r.observe_pane(&o, &mut no_probe, t0);
        // UI paint, then quiet.
        o.bytes = 3000;
        o.last_output = Some(t0 + ms(200));
        r.observe_pane(&o, &mut no_probe, t0 + ms(250));
        r.tick(t0 + ms(2000));
        assert_eq!(r.session(5).unwrap().state, AgentState::Idle);
        // The user types and submits a prompt.
        o.last_input = Some(t0 + ms(5000));
        o.last_submit = Some(t0 + ms(5100));
        r.observe_pane(&o, &mut no_probe, t0 + ms(5150));
        o.bytes = 3800;
        o.last_output = Some(t0 + ms(5300));
        r.observe_pane(&o, &mut no_probe, t0 + ms(5350));
        assert_eq!(r.session(5).unwrap().state, AgentState::Working);
        assert_eq!(r.session(5).unwrap().turn_counter, 1);
        assert!(r.session(5).unwrap().last_turn_started_at.is_some());
        // Quiet: the turn finishes.
        r.tick(t0 + ms(8500));
        assert_eq!(r.session(5).unwrap().state, AgentState::Idle);
        let evs = r.drain_events();
        assert_eq!(evs.len(), 2, "{evs:?}");
        assert!(matches!(&evs[0], AgentEvent::TurnStarted { pane_uid: 5, turn: 1, cwd: Some(c), git_root: Some(g), .. } if *c == cwd && *g == cwd));
        assert!(matches!(&evs[1], AgentEvent::TurnFinished { pane_uid: 5, turn: 1, cwd: Some(_), git_root: Some(_), .. }));
        assert!(r.drain_events().is_empty(), "drained once");
    }

    #[test]
    fn the_launching_enter_does_not_start_a_turn() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        let mut o = obs(6);
        // Enter submitted `claude` just before the block started.
        o.last_input = Some(t0);
        o.last_submit = Some(t0);
        o.bytes = 100;
        o.block_running = true;
        o.running_cmd = Some("claude".into());
        r.observe_pane(&o, &mut no_probe, t0 + ms(50));
        o.bytes = 5000;
        o.last_output = Some(t0 + ms(300));
        r.observe_pane(&o, &mut no_probe, t0 + ms(350));
        r.tick(t0 + ms(2500));
        assert_eq!(r.session(6).unwrap().state, AgentState::Idle);
        assert_eq!(r.session(6).unwrap().turn_counter, 0);
        assert!(r.drain_events().is_empty());
    }

    #[test]
    fn approval_prompt_on_screen_sets_needs_attention() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        let mut o = obs(8);
        o.block_running = true;
        o.running_cmd = Some("claude".into());
        r.observe_pane(&o, &mut no_probe, t0);
        o.bytes = 500;
        o.last_output = Some(t0 + ms(100));
        r.observe_pane(&o, &mut no_probe, t0 + ms(120));
        assert!(r.wants_scan(8, 500, t0 + ms(130)));
        let screen: Vec<String> = ["Edit file", "Do you want to make this edit to a.rs?", "❯ 1. Yes", "  2. No"].iter().map(|s| s.to_string()).collect();
        r.observe_screen(8, &screen, 500, t0 + ms(130));
        assert!(!r.wants_scan(8, 500, t0 + ms(900)), "unchanged output is not rescanned");
        r.observe_screen(8, &screen, 500, t0 + ms(800));
        let s = r.session(8).unwrap();
        assert_eq!(s.state, AgentState::WaitingForUser);
        assert!(s.needs_attention);
        assert_eq!(s.waiting_reason.as_deref(), Some("approve edit"));
        assert_eq!(r.attention_count(), 1);
        assert_eq!(drain_kinds(&mut r), vec!["needs"]);
    }

    #[test]
    fn hooks_create_and_drive_sessions() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        let cwd = tmp_repo("hook", "dev");
        let exists = |id: usize| id == 3;
        let mk = |_: usize| Some(PaneObs { uid: 3, osc_seen: true, cwd: Some(cwd.clone()), ..Default::default() });
        let hook = |state, msg: Option<&str>| HookEvent { state, pane_id: Some(3), agent: None, message: msg.map(str::to_string), cwd: None };
        // `done` for an unknown pane session conjures nothing.
        r.observe_hook(&hook(HookKind::Done, None), &exists, &mk, t0);
        assert!(r.sessions().is_empty());
        // `working` does (Claude Code is the default).
        r.observe_hook(&hook(HookKind::Working, None), &exists, &mk, t0 + ms(10));
        assert_eq!(r.session(3).unwrap().kind, AgentKind::ClaudeCode);
        assert_eq!(r.session(3).unwrap().state, AgentState::Working);
        r.observe_hook(&hook(HookKind::Waiting, Some("Claude needs your permission to use Bash")), &exists, &mk, t0 + ms(2000));
        assert_eq!(r.session(3).unwrap().state, AgentState::WaitingForUser);
        assert_eq!(r.session(3).unwrap().preview, "Claude needs your permission to use Bash");
        r.observe_hook(&hook(HookKind::Done, None), &exists, &mk, t0 + ms(4000));
        assert_eq!(r.session(3).unwrap().state, AgentState::Idle);
        assert_eq!(drain_kinds(&mut r), vec!["start", "needs", "finish"]);
        // A hook for a pane that does not exist is dropped.
        let other = HookEvent { pane_id: Some(99), ..hook(HookKind::Working, None) };
        r.observe_hook(&other, &exists, &mk, t0 + ms(5000));
        assert_eq!(r.sessions().len(), 1);
    }

    #[test]
    fn hooks_find_their_pane_by_cwd_when_the_id_is_missing() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        let cwd = tmp_repo("hookcwd", "m");
        let mut o = obs(1);
        o.cwd = Some(cwd.clone());
        o.block_running = true;
        o.running_cmd = Some("codex".into());
        r.observe_pane(&o, &mut no_probe, t0);
        let mut o2 = obs(2);
        o2.cwd = Some("/definitely/elsewhere".into());
        o2.block_running = true;
        o2.running_cmd = Some("aider".into());
        r.observe_pane(&o2, &mut no_probe, t0);
        let ev = HookEvent { state: HookKind::Waiting, pane_id: None, agent: None, message: None, cwd: Some(format!("{cwd}/src")) };
        r.observe_hook(&ev, &|_| false, &|_| None, t0 + ms(10));
        assert_eq!(r.session(1).unwrap().state, AgentState::WaitingForUser);
        assert_eq!(r.session(2).unwrap().state, AgentState::Starting);
        // Ambiguous cwd (two agents in the same repo): ignored, never guessed.
        let mut o3 = obs(3);
        o3.cwd = Some(cwd.clone());
        o3.block_running = true;
        o3.running_cmd = Some("gemini".into());
        r.observe_pane(&o3, &mut no_probe, t0 + ms(20));
        let ev = HookEvent { state: HookKind::Done, pane_id: None, agent: None, message: None, cwd: Some(cwd) };
        r.observe_hook(&ev, &|_| false, &|_| None, t0 + ms(30));
        assert_eq!(r.session(1).unwrap().state, AgentState::WaitingForUser);
        assert_eq!(r.session(3).unwrap().state, AgentState::Starting);
    }

    #[test]
    fn osc_notification_marks_waiting() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        let mut o = obs(1);
        o.block_running = true;
        o.running_cmd = Some("claude".into());
        r.observe_pane(&o, &mut no_probe, t0);
        r.observe_notification(1, "Claude Code", "Claude needs your permission to use Bash", t0 + ms(100));
        assert_eq!(r.session(1).unwrap().state, AgentState::WaitingForUser);
        // Notifications for panes without an agent are ignored.
        r.observe_notification(42, "x", "needs your approval", t0);
        assert_eq!(r.sessions().len(), 1);
    }

    #[test]
    fn closing_a_pane_ends_its_session() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        for uid in [1, 2] {
            let mut o = obs(uid);
            o.block_running = true;
            o.running_cmd = Some("claude".into());
            r.observe_pane(&o, &mut no_probe, t0);
        }
        r.retain_panes(&[2], t0 + ms(10));
        assert_eq!(r.sessions().len(), 1);
        assert_eq!(r.sessions()[0].pane_uid, 2);
        assert!(matches!(r.drain_events().as_slice(), [AgentEvent::Exited { pane_uid: 1, exit: None, .. }]));
    }

    #[test]
    fn next_attention_cycles() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        assert_eq!(r.next_attention(None), None);
        for uid in [10, 11, 12, 13] {
            let mut o = obs(uid);
            o.block_running = true;
            o.running_cmd = Some("claude".into());
            r.observe_pane(&o, &mut no_probe, t0);
        }
        assert_eq!(r.next_attention(None), None);
        r.observe_notification(11, "", "needs your permission", t0);
        r.observe_notification(13, "", "needs your permission", t0);
        assert_eq!(r.attention_count(), 2);
        assert_eq!(r.next_attention(None), Some(11));
        assert_eq!(r.next_attention(Some(11)), Some(13));
        assert_eq!(r.next_attention(Some(13)), Some(11), "wraps");
        assert_eq!(r.next_attention(Some(10)), Some(11));
        assert_eq!(r.next_attention(Some(99)), Some(11), "unknown start = from the top");
        // A single waiting session returns itself.
        r.observe_notification(11, "", "Agent turn complete", t0 + ms(5));
        assert_eq!(r.next_attention(Some(13)), Some(13));
    }

    #[test]
    fn disabled_registry_observes_nothing() {
        let mut r = AgentRegistry::new();
        r.configure(&AgentsConfig { enabled: false, ..Default::default() });
        let mut o = obs(1);
        o.block_running = true;
        o.running_cmd = Some("claude".into());
        r.observe_pane(&o, &mut no_probe, Instant::now());
        assert!(r.sessions().is_empty());
    }

    #[test]
    fn user_approval_patterns_are_used() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        r.configure(&AgentsConfig { approval_patterns: vec!["type deploy to continue".into()], ..Default::default() });
        let mut o = obs(1);
        o.block_running = true;
        o.running_cmd = Some("aider".into());
        r.observe_pane(&o, &mut no_probe, t0);
        let screen = vec!["Type DEPLOY to continue".to_string()];
        r.observe_screen(1, &screen, 1, t0 + ms(10));
        r.observe_screen(1, &screen, 2, t0 + ms(700));
        assert_eq!(r.session(1).unwrap().state, AgentState::WaitingForUser);
    }

    #[test]
    fn preview_table() {
        let cases: &[(&[&str], Option<&str>)] = &[
            // vim-mode footer under the prompt box
            (&["● I'll look at the parser first.", "╭────────╮", "│ >      │", "╰────────╯", "  -- INSERT --"], Some("● I'll look at the parser first.")),
            (&["Reading src/lib.rs", "  ? for shortcuts"], Some("Reading src/lib.rs")),
            (&["Edited 3 files", "  ⏵⏵ auto mode on (shift+tab to cycle)"], Some("Edited 3 files")),
            (&["Edited 3 files", "  auto mode on"], Some("Edited 3 files")),
            (&["Done refactoring", "Image in clipboard · ctrl+v to paste", "57929 tokens"], Some("Done refactoring")),
            (&["Done refactoring", "  ↓ 1.2k tokens"], Some("Done refactoring")),
            (&["Done refactoring", "  Opus 4.1 | ctx 12% | $0.42"], Some("Done refactoring")),
            (&["Done refactoring", "  Sonnet 4.5 · 23% context"], Some("Done refactoring")),
            // spinner lines: real text wins, spinner is the fallback
            (&["● Searching the codebase", "✻ Meandering… (1s · ↓ 12 tokens · esc to interrupt)", "  -- INSERT --"], Some("● Searching the codebase")),
            (&["✻ Meandering… (1s · esc to interrupt)", "  -- INSERT --"], Some("✻ Meandering…")),
            (&["-- INSERT --"], None),
            (&["────────", "   ", ""], None),
            (&[], None),
        ];
        for (screen, want) in cases {
            let l: Vec<String> = screen.iter().map(|s| s.to_string()).collect();
            assert_eq!(preview_line(&l).as_deref(), *want, "{screen:?}");
        }
    }

    #[test]
    fn spinner_on_screen_keeps_or_makes_the_agent_working() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        let mut o = obs(1);
        o.block_running = true;
        o.running_cmd = Some("claude".into());
        r.observe_pane(&o, &mut no_probe, t0);
        let busy: Vec<String> = ["> who are you", "", "✻ Meandering… (1s · ↓ 3 tokens · esc to interrupt)", "  -- INSERT --"]
            .iter().map(|s| s.to_string()).collect();
        // Startup settled to Idle with no submit seen (e.g. hooks missing)...
        r.tick(t0 + ms(9000));
        assert_eq!(r.session(1).unwrap().state, AgentState::Idle);
        // ...but the spinner says otherwise.
        r.observe_screen(1, &busy, 1, t0 + ms(9100));
        assert_eq!(r.session(1).unwrap().state, AgentState::Working);
        assert_eq!(r.session(1).unwrap().preview, "> who are you");
        // A silent stretch (no new bytes, no scan) does not flip it to Idle while the spinner was last seen.
        r.tick(t0 + ms(9100) + ms(10_000));
        assert_eq!(r.session(1).unwrap().state, AgentState::Working);
        // Spinner gone + quiet + ready prompt: Idle.
        let ready: Vec<String> = ["● Hello!", "╭────╮", "│ >  │", "╰────╯", "  -- INSERT --"].iter().map(|s| s.to_string()).collect();
        r.observe_screen(1, &ready, 2, t0 + ms(30_000));
        r.tick(t0 + ms(30_000) + ms(3000));
        assert_eq!(r.session(1).unwrap().state, AgentState::Idle);
        assert_eq!(r.session(1).unwrap().preview, "● Hello!");
    }

    #[test]
    fn preview_skips_decoration() {
        let l: Vec<String> = ["Reading src/main.rs", "╭──────╮", "│ >    │", "╰──────╯", ""].iter().map(|s| s.to_string()).collect();
        assert_eq!(preview_line(&l).as_deref(), Some("Reading src/main.rs"));
        assert_eq!(preview_line(&[]), None);
    }

    #[test]
    fn deadlines_and_animation_flags() {
        let t0 = Instant::now();
        let mut r = AgentRegistry::new();
        assert!(!r.animating());
        assert_eq!(r.next_deadline(), None);
        let mut o = obs(1);
        o.block_running = true;
        o.running_cmd = Some("claude".into());
        r.observe_pane(&o, &mut no_probe, t0);
        assert!(r.animating());
        assert!(r.next_deadline().is_some());
    }
}
