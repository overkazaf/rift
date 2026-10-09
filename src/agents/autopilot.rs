//! Autopilot: answer routine approval prompts by policy, visibly and
//! cancellably.
//!
//! * [`Autopilot`] (pure): per-agent switches (off by default), the countdown
//!   state machine, per-card counters. A decision other than "ask" starts a
//!   short countdown shown on the card; Esc (or any dock key) cancels it and
//!   the prompt stays a human question; at zero the answer is sent after the
//!   prompt is re-verified against the screen. Nothing acts while autopilot is
//!   off for that agent.
//! * [`Host`]: reads `~/.config/rift/policy.toml` and the per-repo
//!   `.rift/policy.toml` (the latter only once its hash is trusted), merges
//!   them, remembers trust, and appends the audit log.
//! * [`PolicyLog`]: the "Agents: Policy Log" overlay.
//! * Glue (`evaluate_all`, `tick`, `mcp_decision`, ...): the only parts that
//!   touch `App`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::control::Risk;
use super::policy::{self, Decision, LogEntry, PathEnv, Policy, Tool, TrustStore, Verdict};
use super::{AgentKind, AgentState};
use crate::app::App;
use crate::tools::exec_preview::Severity;
use crate::ui::kit::Tone;

// ───────────────────────────── state machine ─────────────────────────────

/// Countdown before an automatic answer, unless `[autopilot] countdown_ms` says otherwise.
pub const DEFAULT_COUNTDOWN_MS: u64 = 1500;

/// An automatic answer that is counting down.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    /// Identity of the prompt (`policy::signature`).
    pub sig: String,
    pub verdict: Verdict,
    /// Option to press.
    pub option: usize,
    /// What is requested, for the card ("cargo test").
    pub subject: String,
    pub tool: Option<Tool>,
    pub rule: String,
    pub reason: String,
    pub started: Instant,
    pub deadline: Instant,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub approved: u32,
    pub denied: u32,
}

pub struct Autopilot {
    global: bool,
    /// Per-agent switches that differ from the global one.
    overrides: HashMap<usize, bool>,
    pub countdown_ms: u64,
    pending: HashMap<usize, Pending>,
    /// Prompts the user stopped: they stay questions until the prompt changes.
    dismissed: HashMap<usize, String>,
    counts: HashMap<usize, Counts>,
    /// Countdowns that began since the last [`Autopilot::take_started`].
    started: Vec<usize>,
}

impl Default for Autopilot {
    fn default() -> Self {
        Autopilot { global: false, overrides: HashMap::new(), countdown_ms: DEFAULT_COUNTDOWN_MS, pending: HashMap::new(), dismissed: HashMap::new(), counts: HashMap::new(), started: Vec::new() }
    }
}

impl Autopilot {
    pub fn enabled(&self, uid: usize) -> bool {
        self.overrides.get(&uid).copied().unwrap_or(self.global)
    }

    pub fn global(&self) -> bool {
        self.global
    }

    /// Any switch on? (Gates all the work.)
    pub fn active(&self) -> bool {
        self.global || self.overrides.values().any(|v| *v)
    }

    /// Number of agents switched on individually while the global switch is off.
    pub fn individual_on(&self) -> usize {
        if self.global {
            0
        } else {
            self.overrides.values().filter(|v| **v).count()
        }
    }

    /// The global switch: sets every agent, forgets individual choices.
    pub fn set_global(&mut self, on: bool) {
        self.global = on;
        self.overrides.clear();
        if !on {
            self.pending.clear();
        }
    }

    /// Flip one agent; returns its new state.
    pub fn toggle_agent(&mut self, uid: usize) -> bool {
        let on = !self.enabled(uid);
        if on == self.global {
            self.overrides.remove(&uid);
        } else {
            self.overrides.insert(uid, on);
        }
        if !on {
            self.pending.remove(&uid);
        }
        on
    }

    /// Offer a fresh decision for the prompt `sig` on agent `uid`.
    pub fn consider(&mut self, uid: usize, sig: &str, o: &policy::Outcome, now: Instant) {
        if self.dismissed.get(&uid).is_some_and(|d| d != sig) {
            self.dismissed.remove(&uid);
        }
        let option = match (o.verdict, o.option) {
            (Verdict::Approve | Verdict::Deny, Some(i)) if self.enabled(uid) => i,
            _ => {
                self.pending.remove(&uid);
                return;
            }
        };
        if self.dismissed.get(&uid).is_some_and(|d| d == sig) {
            self.pending.remove(&uid);
            return;
        }
        if self.pending.get(&uid).is_some_and(|p| p.sig == sig && p.verdict == o.verdict && p.option == option && p.rule == o.rule) {
            return; // the same countdown keeps running
        }
        self.pending.insert(
            uid,
            Pending {
                sig: sig.to_string(),
                verdict: o.verdict,
                option,
                subject: o.subject.clone(),
                tool: o.tool,
                rule: o.rule.clone(),
                reason: o.reason.clone(),
                started: now,
                deadline: now + Duration::from_millis(self.countdown_ms),
            },
        );
        self.started.push(uid);
    }

    /// Countdowns that began since the last call (so the glue can announce
    /// them when the dock, which shows them, is closed).
    pub fn take_started(&mut self) -> Vec<(usize, Pending)> {
        let uids = std::mem::take(&mut self.started);
        uids.into_iter().filter_map(|u| self.pending.get(&u).map(|p| (u, p.clone()))).collect()
    }

    /// The agent is not at a prompt any more: forget countdown and dismissal.
    pub fn clear_prompt(&mut self, uid: usize) {
        self.pending.remove(&uid);
        self.dismissed.remove(&uid);
    }

    /// Drop a countdown without remembering a dismissal (the prompt may be re-evaluated).
    pub fn drop_pending(&mut self, uid: usize) {
        self.pending.remove(&uid);
    }

    /// Stop one agent's countdown for good (this prompt stays a question).
    pub fn dismiss(&mut self, uid: usize, sig: &str) {
        self.pending.remove(&uid);
        self.dismissed.insert(uid, sig.to_string());
    }

    /// The user pressed Esc / a dock key: stop every countdown. True when any was running.
    pub fn cancel_all(&mut self) -> bool {
        let mut any = false;
        for (uid, p) in self.pending.drain() {
            self.dismissed.insert(uid, p.sig);
            any = true;
        }
        any
    }

    /// Stop one agent's countdown. True when one was running.
    pub fn cancel(&mut self, uid: usize) -> bool {
        match self.pending.remove(&uid) {
            Some(p) => {
                self.dismissed.insert(uid, p.sig);
                true
            }
            None => false,
        }
    }

    pub fn any_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn pending(&self, uid: usize) -> Option<&Pending> {
        self.pending.get(&uid)
    }

    /// (agent, prompt, when its countdown started) of every running countdown.
    pub fn running(&self) -> Vec<(usize, String, Instant)> {
        self.pending.iter().map(|(u, p)| (*u, p.sig.clone(), p.started)).collect()
    }

    /// Countdowns that ran out (removed from the machine), by agent.
    pub fn due(&mut self, now: Instant) -> Vec<(usize, Pending)> {
        let mut uids: Vec<usize> = self.pending.iter().filter(|(_, p)| p.deadline <= now).map(|(u, _)| *u).collect();
        uids.sort_unstable();
        uids.into_iter().filter_map(|u| self.pending.remove(&u).map(|p| (u, p))).collect()
    }

    /// When the next countdown ends.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.values().map(|p| p.deadline).min()
    }

    pub fn record(&mut self, uid: usize, v: Verdict) {
        let c = self.counts.entry(uid).or_default();
        match v {
            Verdict::Approve => c.approved += 1,
            Verdict::Deny => c.denied += 1,
            Verdict::Ask => {}
        }
    }

    pub fn counts(&self, uid: usize) -> Counts {
        self.counts.get(&uid).copied().unwrap_or_default()
    }

    /// Forget agents that are gone.
    pub fn prune(&mut self, uids: &[usize]) {
        self.overrides.retain(|u, _| uids.contains(u));
        self.pending.retain(|u, _| uids.contains(u));
        self.dismissed.retain(|u, _| uids.contains(u));
        self.counts.retain(|u, _| uids.contains(u));
    }

    /// What a card shows.
    pub fn view(&self, uid: usize, now: Instant) -> AutoView {
        AutoView {
            enabled: self.enabled(uid),
            counts: self.counts(uid),
            countdown: self.pending.get(&uid).map(|p| (p.verdict, p.subject.clone(), p.deadline.saturating_duration_since(now))),
        }
    }
}

// ───────────────────────────── card text ─────────────────────────────

/// Everything the card needs to draw its autopilot line(s).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoView {
    pub enabled: bool,
    pub counts: Counts,
    pub countdown: Option<(Verdict, String, Duration)>,
}

/// `1.5s`, rounded up to a tenth; `now` at zero.
pub fn fmt_remaining(d: Duration) -> String {
    let tenths = (d.as_millis() as u64).div_ceil(100);
    if tenths == 0 {
        "now".into()
    } else {
        format!("{}.{}s", tenths / 10, tenths % 10)
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('\u{2026}');
        t
    }
}

/// "Auto-approving `cargo test` in 1.5s — Esc to stop", fitted to `cols`
/// (the subject gives way first; narrow docks use two lines).
pub fn countdown_lines(verdict: Verdict, subject: &str, remaining: Duration, cols: usize) -> Vec<String> {
    let verb = if verdict == Verdict::Deny { "Auto-denying" } else { "Auto-approving" };
    let subject = one_line(subject);
    let tail = format!(" in {} \u{2014} Esc to stop", fmt_remaining(remaining));
    let fixed = verb.chars().count() + 3 + tail.chars().count();
    if cols >= fixed + 8 {
        return vec![format!("{verb} `{}`{tail}", cut(&subject, cols - fixed))];
    }
    vec![cut(&format!("{verb} `{subject}`"), cols), cut(&format!("in {} \u{2014} Esc to stop", fmt_remaining(remaining)), cols)]
}

/// Lines of a card's autopilot block. `selected` adds the "how to turn it on" hint to a quiet card.
pub fn auto_lines(v: &AutoView, cols: usize, selected: bool) -> Vec<(String, Tone)> {
    let c = v.counts;
    let mut out: Vec<(String, Tone)> = Vec::new();
    if let Some((verdict, subject, remaining)) = &v.countdown {
        let tone = if *verdict == Verdict::Deny { Tone::Danger } else { Tone::Warning };
        out.extend(countdown_lines(*verdict, subject, *remaining, cols).into_iter().map(|l| (l, tone)));
        // The running totals stay visible while a countdown runs.
        if c.approved + c.denied > 0 {
            let mut t = format!("{} auto-approved", c.approved);
            if c.denied > 0 {
                t.push_str(&format!(" \u{b7} {} denied", c.denied));
            }
            out.push((cut(&t, cols), Tone::Neutral));
        }
        return out;
    }
    let mut parts: Vec<String> = Vec::new();
    if v.enabled {
        parts.push("autopilot on".into());
    } else if c.approved + c.denied > 0 || selected {
        parts.push("autopilot off".into());
    } else {
        return out;
    }
    if c.approved > 0 {
        parts.push(format!("{} auto-approved", c.approved));
    }
    if c.denied > 0 {
        parts.push(format!("{} denied", c.denied));
    }
    if !v.enabled && c.approved + c.denied == 0 {
        parts.push("p to turn on".into());
    }
    let tone = if v.enabled { Tone::Accent } else { Tone::Neutral };
    out.push((cut(&parts.join(" \u{b7} "), cols), tone));
    out
}

/// The dock header's switch label.
pub fn header_label(a: &Autopilot) -> (String, Tone) {
    if a.global() {
        ("AUTO ON".into(), Tone::Warning)
    } else if a.individual_on() > 0 {
        (format!("AUTO {}", a.individual_on()), Tone::Warning)
    } else {
        ("AUTO off".into(), Tone::Neutral)
    }
}

// ───────────────────────────── firing ─────────────────────────────

/// What to do with a countdown that ran out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FireCheck {
    Send(usize),
    /// Do nothing (the reason is for the log / tests).
    Drop(&'static str),
    /// The user typed in the pane meanwhile: stop for good.
    Dismiss(&'static str),
}

/// Everything that must still be true at the moment of answering.
pub fn fire_check(p: &Pending, enabled: bool, waiting: bool, typed_since: bool, current_sig: Option<&str>, fresh: Option<&policy::Outcome>) -> FireCheck {
    if !enabled {
        return FireCheck::Drop("autopilot is off");
    }
    if !waiting {
        return FireCheck::Drop("the agent is not waiting any more");
    }
    if typed_since {
        return FireCheck::Dismiss("you typed in the pane");
    }
    if current_sig != Some(p.sig.as_str()) {
        return FireCheck::Drop("the prompt changed");
    }
    match fresh {
        Some(o) if o.verdict == p.verdict && o.option == Some(p.option) => FireCheck::Send(p.option),
        _ => FireCheck::Drop("the policy changed its mind"),
    }
}

// ───────────────────────────── policy host (files, trust) ─────────────────────────────

/// Where the policy lives on disk.
#[derive(Clone, Debug)]
pub struct Paths {
    pub policy: PathBuf,
    pub trust: PathBuf,
    pub log: PathBuf,
}

impl Paths {
    pub fn in_dir(dir: &Path) -> Paths {
        Paths { policy: dir.join("policy.toml"), trust: dir.join("policy-trust"), log: dir.join("policy.log") }
    }

    /// `~/.config/rift/`
    pub fn user() -> Paths {
        let dir = dirs::home_dir().map(|h| h.join(".config").join("rift")).unwrap_or_else(|| PathBuf::from(".rift-config"));
        Paths::in_dir(&dir)
    }
}

/// Re-read policy files at most this often.
const RELOAD_EVERY: Duration = Duration::from_secs(2);

fn read_limited(path: &Path) -> Option<String> {
    use std::io::Read;
    let f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    f.take(policy::MAX_POLICY_BYTES as u64 + 1).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// A repo policy that is not trusted (yet).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustNeed {
    pub root: String,
    pub hash: String,
    /// The user trusted an earlier version of this file.
    pub changed: bool,
    /// Human summary of the rules (for the confirm modal).
    pub summary: Vec<String>,
}

pub struct Loaded {
    pub policy: Arc<Policy>,
    /// Set once per (repo, file hash) per session.
    pub trust_needed: Option<TrustNeed>,
}

struct RepoFile {
    checked: Instant,
    text: Option<String>,
    hash: String,
}

pub struct Host {
    pub paths: Paths,
    user: Option<(Instant, Option<String>)>,
    user_gen: u64,
    repos: HashMap<String, RepoFile>,
    trust: Option<TrustStore>,
    asked: HashSet<(String, String)>,
    merged: HashMap<String, (u64, Option<String>, Arc<Policy>)>,
    reported_errors: String,
}

impl Default for Host {
    fn default() -> Self {
        Host::new(Paths::user())
    }
}

impl Host {
    pub fn new(paths: Paths) -> Host {
        Host { paths, user: None, user_gen: 0, repos: HashMap::new(), trust: None, asked: HashSet::new(), merged: HashMap::new(), reported_errors: String::new() }
    }

    fn trust(&mut self) -> &mut TrustStore {
        let path = self.paths.trust.clone();
        self.trust.get_or_insert_with(|| TrustStore::load(&path))
    }

    fn user_text(&mut self, now: Instant) -> Option<String> {
        let stale = self.user.as_ref().map_or(true, |(t, _)| now.saturating_duration_since(*t) >= RELOAD_EVERY);
        if stale {
            let text = read_limited(&self.paths.policy);
            if self.user.as_ref().map(|(_, t)| t) != Some(&text) {
                self.user_gen += 1;
            }
            self.user = Some((now, text));
        }
        self.user.as_ref().and_then(|(_, t)| t.clone())
    }

    /// The merged policy for an agent working in `root` (its git root).
    pub fn policy_for(&mut self, root: Option<&str>, now: Instant) -> Loaded {
        let user = self.user_text(now);
        let key = root.unwrap_or("").to_string();
        let mut repo_text: Option<String> = None;
        let mut need: Option<TrustNeed> = None;
        if let Some(r) = root {
            let stale = self.repos.get(r).map_or(true, |f| now.saturating_duration_since(f.checked) >= RELOAD_EVERY);
            if stale {
                let text = read_limited(&Path::new(r).join(".rift").join("policy.toml"));
                let hash = text.as_deref().map(|t| policy::sha256_hex(t.as_bytes())).unwrap_or_default();
                self.repos.insert(r.to_string(), RepoFile { checked: now, text, hash });
            }
            let (text, hash) = {
                let f = &self.repos[r];
                (f.text.clone(), f.hash.clone())
            };
            if let Some(t) = text {
                if self.trust().is_trusted(r, &hash) {
                    repo_text = Some(t.clone());
                } else if self.asked.insert((r.to_string(), hash.clone())) {
                    let changed = self.trust().trusted_other(r, &hash);
                    need = Some(TrustNeed { root: r.to_string(), hash: hash.clone(), changed, summary: summarize(&t) });
                }
            }
        }
        let repo_hash = repo_text.as_deref().map(|t| policy::sha256_hex(t.as_bytes()));
        let fresh = self.merged.get(&key).is_some_and(|(g, h, _)| *g == self.user_gen && *h == repo_hash);
        if !fresh {
            let p = Arc::new(policy::load_merged(user.as_deref(), repo_text.as_deref()));
            self.merged.insert(key.clone(), (self.user_gen, repo_hash, p));
        }
        Loaded { policy: self.merged[&key].2.clone(), trust_needed: need }
    }

    /// Policy problems to tell the user about, once per distinct set.
    pub fn take_errors(&mut self, p: &Policy) -> Option<String> {
        let joined = p.errors.join("\n");
        if joined == self.reported_errors {
            return None;
        }
        self.reported_errors = joined;
        p.errors.first().map(|e| format!("Policy: {e}{}", if p.errors.len() > 1 { format!(" (+{} more)", p.errors.len() - 1) } else { String::new() }))
    }

    /// The user answered the trust question.
    pub fn trust_repo(&mut self, root: &str, hash: &str) -> std::io::Result<()> {
        let path = self.paths.trust.clone();
        let t = self.trust();
        t.trust(root, hash);
        let r = t.save(&path);
        self.merged.clear();
        r
    }

    /// Forget caches (after the policy file was edited by Rift itself).
    pub fn invalidate(&mut self) {
        self.user = None;
        self.repos.clear();
        self.merged.clear();
    }

    pub fn log(&self, e: &LogEntry) {
        if let Err(err) = policy::append_log(&self.paths.log, e) {
            log::warn!("policy log: {err}");
        }
    }
}

/// Rule list for the trust modal.
pub fn summarize(text: &str) -> Vec<String> {
    let p = policy::parse_policy(text, policy::Source::Repo, ".rift/policy.toml");
    let mut v: Vec<String> = Vec::new();
    let approving = p.rules.iter().filter(|r| r.action == policy::Action::Approve).count();
    v.push(format!("{} rule{}, {} would auto-approve.", p.rules.len(), if p.rules.len() == 1 { "" } else { "s" }, approving));
    for r in p.rules.iter().take(6) {
        v.push(format!("  {}", r.describe()));
    }
    if p.rules.len() > 6 {
        v.push(format!("  \u{2026} and {} more", p.rules.len() - 6));
    }
    let broad = p.rules.iter().filter(|r| r.is_broad()).count();
    if broad > 0 {
        v.push(format!("WARNING: {broad} approving rule{} match(es) very broadly.", if broad == 1 { "" } else { "s" }).replace("match(es)", if broad == 1 { "matches" } else { "match" }));
    }
    for e in p.errors.iter().take(2) {
        v.push(format!("Problem: {e}"));
    }
    v
}

// ───────────────────────────── MCP ─────────────────────────────

/// What the policy says about an MCP `run_command`.
#[derive(Clone, Debug, PartialEq)]
pub enum McpAuto {
    Approve(Decision),
    Deny(Decision),
    Ask,
}

/// Pure core of the MCP path: `enabled` is whether autopilot covers that pane.
/// `danger` is `approval::assess`'s verdict (critical / risky / routine).
pub fn mcp_verdict(policy: &Policy, env: &PathEnv, command: &str, critical: Option<bool>, enabled: bool, agent: Option<AgentKind>) -> McpAuto {
    if !enabled {
        return McpAuto::Ask;
    }
    let sev = match critical {
        Some(true) => Some(Severity::Critical),
        Some(false) => Some(Severity::Warning),
        None => None,
    };
    let req = policy::Request { tool: Tool::Bash, subject: command.to_string(), agent };
    let d = policy::evaluate(policy, &req, env, sev);
    match d.verdict {
        Verdict::Approve => McpAuto::Approve(d),
        Verdict::Deny => McpAuto::Deny(d),
        Verdict::Ask => McpAuto::Ask,
    }
}

// ───────────────────────────── policy log overlay ─────────────────────────────

#[derive(Default)]
pub struct PolicyLog {
    pub visible: bool,
    scroll: usize,
    /// Oldest first.
    pub entries: Vec<LogEntry>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogKey {
    Escape,
    Up,
    Down,
    PageUp,
    PageDown,
}

/// Entries kept for the overlay.
const LOG_VIEW_MAX: usize = 400;

impl PolicyLog {
    pub fn open(&mut self, file: &Path) {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        self.entries = policy::tail_entries(&text, LOG_VIEW_MAX);
        self.visible = true;
        self.scroll = 0;
    }

    /// A decision just made (kept live while the overlay is open).
    pub fn push(&mut self, e: LogEntry) {
        if self.visible {
            self.entries.push(e);
            if self.entries.len() > LOG_VIEW_MAX {
                self.entries.remove(0);
            }
        }
    }

    pub fn handle_key(&mut self, key: LogKey) {
        let total = self.entries.len();
        match key {
            LogKey::Escape => self.visible = false,
            LogKey::Up => self.scroll = self.scroll.saturating_sub(1),
            LogKey::Down => self.scroll = (self.scroll + 1).min(total.saturating_sub(1)),
            LogKey::PageUp => self.scroll = self.scroll.saturating_sub(10),
            LogKey::PageDown => self.scroll = (self.scroll + 10).min(total.saturating_sub(1)),
        }
    }

    #[cfg(test)]
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Table rows, newest first: `MM-DD HH:MM:SS`, agent, pane, request, decision, rule.
    pub fn rows(&self) -> Vec<[String; 6]> {
        self.entries
            .iter()
            .rev()
            .map(|e| {
                let iso = policy::iso_utc(e.ts);
                [iso[5..10].to_string() + " " + &iso[11..19], e.agent.clone(), format!("#{}", e.pane), e.request.clone(), e.decision.clone(), e.rule.clone()]
            })
            .collect()
    }

    /// Draw the overlay inside `area` (the part of the window that is not the dock),
    /// leaving the rest of the frame untouched so a running countdown stays visible.
    pub fn render(&self, buffer: &mut [u32], width: usize, height: usize, font: &mut crate::renderer::font::FontManager, theme: &crate::config::Theme, area: Option<crate::ui::kit::Rect>) {
        use crate::ui::kit::{Column, Ctx, PanelSpec, Rect, TableRow, Tokens, Width};
        if !self.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        let area = area.unwrap_or(Rect::new(0, 0, width, height));
        let rows = self.rows();
        let want_h = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + (rows.len().max(3) + 1) * tk.row_h;
        let w = (92 * tk.cw + 2 * tk.sp.lg).min(area.w.saturating_sub(2 * tk.sp.xl)).max(30 * tk.cw.min(area.w / 30));
        let h = want_h.min(area.h.saturating_sub(2 * tk.sp.xl)).max(tk.row_h * 6);
        let rect = Rect::new(area.x + area.w.saturating_sub(w) / 2, area.y + area.h.saturating_sub(h) / 2, w.min(area.w), h.min(area.h));
        let approved = self.entries.iter().filter(|e| e.decision == "approve").count();
        let denied = self.entries.iter().filter(|e| e.decision == "deny").count();
        let badge = format!("{approved} approved \u{b7} {denied} denied");
        let spec = PanelSpec::new("Policy Log").badge(&badge, if denied > 0 { Tone::Warning } else { Tone::Accent }).hints(&[("\u{2191}/\u{2193}", "scroll"), ("Esc", "close")]);
        let body = cx.panel(rect, &spec);
        if rows.is_empty() {
            cx.empty_state(body, "No automatic decisions yet", "Turn autopilot on with p (one agent) or P (all) in Mission Control");
            return;
        }
        let table: Vec<TableRow> = rows
            .iter()
            .map(|r| {
                let tone = match r[4].as_str() {
                    "approve" => Tone::Success,
                    "deny" => Tone::Danger,
                    _ => Tone::Warning,
                };
                TableRow::new(r.to_vec()).cell_tone(4, tone)
            })
            .collect();
        cx.table(
            body,
            &[
                Column::new("Time (UTC)", Width::Cols(14)),
                Column::new("Agent", Width::Cols(8)),
                Column::new("Pane", Width::Cols(5)),
                Column::new("Requested", Width::Flex(3)),
                Column::new("Decision", Width::Cols(8)),
                Column::new("Rule", Width::Flex(3)),
            ],
            &table,
            None,
            self.scroll,
        );
    }
}

// ───────────────────────────── glue with App ─────────────────────────────

/// Command-palette entries ("Agents: ...").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutopilotCmd {
    Toggle,
    Log,
    EditPolicy,
}

pub fn run_command(app: &mut App, cmd: AutopilotCmd) {
    match cmd {
        AutopilotCmd::Toggle => toggle_global(app),
        AutopilotCmd::Log => open_log(app),
        AutopilotCmd::EditPolicy => edit_policy(app),
    }
}

fn toast(app: &mut App, msg: impl Into<String>) {
    app.blocks_ui.show_toast(msg);
    app.request_redraw();
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

fn severity_of(risk: &Risk) -> Option<Option<Severity>> {
    match risk {
        Risk::Pending => None,
        Risk::Safe => Some(None),
        Risk::Risky(_) => Some(Some(Severity::Warning)),
        Risk::Critical(_) => Some(Some(Severity::Critical)),
    }
}

struct Sess {
    uid: usize,
    kind: AgentKind,
    waiting: bool,
    cwd: Option<String>,
    root: Option<String>,
}

fn sessions(app: &App) -> Vec<Sess> {
    app.agents
        .sessions()
        .iter()
        .map(|s| Sess { uid: s.pane_uid, kind: s.kind, waiting: s.state == AgentState::WaitingForUser, cwd: s.cwd.clone(), root: s.git_root.clone() })
        .collect()
}

fn show_trust_modal(app: &mut App, need: TrustNeed) {
    use crate::ui::confirm::{ConfirmAction, ConfirmRequest};
    let name = Path::new(&need.root).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| need.root.clone());
    let mut lines = vec![need.root.clone(), if need.changed { "This repo's policy changed since you trusted it.".to_string() } else { "This repo ships its own auto-approval policy (.rift/policy.toml).".to_string() }, String::new()];
    lines.extend(need.summary.iter().cloned());
    lines.push(String::new());
    lines.push("Trusting lets this file auto-approve your agents' prompts in this repo (after the countdown). Critical commands are never auto-approved, whatever it says. Until you trust it, it is ignored.".into());
    app.confirm.push(ConfirmRequest {
        title: format!("Trust policy from {name}?"),
        badge: Some(("SECURITY".into(), Tone::Warning)),
        lines,
        buttons: vec!["Trust policy".into(), "Not now".into()],
        default_sel: 1,
        esc_choice: Some(1),
        tone: Tone::Warning,
        action: ConfirmAction::Policy(Box::new(PolicyConfirm::Trust { root: need.root, hash: need.hash })),
    });
    app.request_redraw();
}

/// Questions the policy asks through the confirm modal.
pub enum PolicyConfirm {
    Trust { root: String, hash: String },
    AlwaysAllow { rules: Vec<String> },
}

pub fn resolve_confirm(app: &mut App, c: PolicyConfirm, choice: Option<usize>) {
    match c {
        PolicyConfirm::Trust { root, hash } => {
            if choice == Some(0) {
                match app.agents_rt.auto_host.trust_repo(&root, &hash) {
                    Ok(()) => toast(app, format!("Trusted the policy of {root}")),
                    Err(e) => toast(app, format!("Could not save trust: {e}")),
                }
            }
        }
        PolicyConfirm::AlwaysAllow { rules } => {
            if choice == Some(0) {
                let path = app.agents_rt.auto_host.paths.policy.clone();
                match policy::append_rules(&path, &rules) {
                    Ok(()) => {
                        app.agents_rt.auto_host.invalidate();
                        toast(app, format!("Added {} rule{} to {}", rules.len(), if rules.len() == 1 { "" } else { "s" }, path.display()));
                    }
                    Err(e) => toast(app, format!("Could not write the policy: {e}")),
                }
            }
        }
    }
    app.request_redraw();
}

/// Decide every waiting agent's prompt and (re)start countdowns. Called from
/// the console refresh whenever autopilot is on for anyone.
pub fn evaluate_all(app: &mut App, now: Instant) {
    let list = sessions(app);
    let uids: Vec<usize> = list.iter().map(|s| s.uid).collect();
    app.agents_ui.auto.prune(&uids);
    for s in &list {
        if !s.waiting {
            app.agents_ui.auto.clear_prompt(s.uid);
            continue;
        }
        if !app.agents_ui.auto.enabled(s.uid) {
            app.agents_ui.auto.drop_pending(s.uid);
            continue;
        }
        let Some(info) = app.agents_ui.info.get(&s.uid) else { continue };
        let Some(p) = info.prompt.clone().filter(|_| info.settled) else {
            // No prompt, or one that is not the live UI at the bottom of the screen.
            app.agents_ui.auto.drop_pending(s.uid);
            continue;
        };
        let Some(sev) = severity_of(&info.risk) else { continue }; // still being checked
        let root = s.root.clone().or_else(|| s.cwd.clone());
        let loaded = app.agents_rt.auto_host.policy_for(root.as_deref(), now);
        if let Some(msg) = app.agents_rt.auto_host.take_errors(&loaded.policy) {
            toast(app, msg);
        }
        if let Some(need) = loaded.trust_needed {
            show_trust_modal(app, need);
        }
        app.agents_ui.auto.countdown_ms = loaded.policy.countdown_ms.unwrap_or(DEFAULT_COUNTDOWN_MS);
        let env = PathEnv::new(root.as_deref(), s.cwd.as_deref(), home());
        let mut prompt = p;
        prompt.agent = prompt.agent.or(Some(s.kind));
        let outcome = policy::decide(&loaded.policy, &prompt, &env, sev);
        let sig = policy::signature(&prompt);
        app.agents_ui.auto.consider(s.uid, &sig, &outcome, now);
    }
    // With the dock closed nobody sees the card's countdown: say it once, with the way out.
    let started = app.agents_ui.auto.take_started();
    if !app.agents_ui.visible {
        if let Some((_, p)) = started.last() {
            let verb = if p.verdict == Verdict::Deny { "denying" } else { "approving" };
            let secs = fmt_remaining(p.deadline.saturating_duration_since(now));
            toast(app, format!("Autopilot {verb} `{}` in {secs} \u{2014} type in the pane to stop", cut(&one_line(&p.subject), 50)));
        }
    }
}

/// Send the answers whose countdown ran out. Called every supervision pass.
pub fn tick(app: &mut App, now: Instant) {
    // Typing in an agent's pane means a human is on it: that prompt stays theirs.
    for (uid, sig, started) in app.agents_ui.auto.running() {
        let typed = app.wm.locate_pane(uid).and_then(|(ti, pi)| app.wm.tabs[ti].pane(pi)).and_then(|p| p.act.last_input).is_some_and(|t| t > started);
        if typed {
            app.agents_ui.auto.dismiss(uid, &sig);
            app.request_redraw();
        }
    }
    for (uid, pend) in app.agents_ui.auto.due(now) {
        fire(app, uid, pend, now);
    }
}

fn fire(app: &mut App, uid: usize, pend: Pending, now: Instant) {
    let Some(s) = sessions(app).into_iter().find(|s| s.uid == uid) else { return };
    let typed_since = app.wm.locate_pane(uid).and_then(|(ti, pi)| app.wm.tabs[ti].pane(pi)).and_then(|p| p.act.last_input).is_some_and(|t| t > pend.started);
    let info = app.agents_ui.info.get(&uid).cloned().unwrap_or_default();
    let prompt = info.prompt.clone().filter(|_| info.settled);
    let current_sig = prompt.as_ref().map(policy::signature);
    let fresh = prompt.as_ref().and_then(|p| {
        let sev = severity_of(&info.risk)?;
        let root = s.root.clone().or_else(|| s.cwd.clone());
        let loaded = app.agents_rt.auto_host.policy_for(root.as_deref(), now);
        let env = PathEnv::new(root.as_deref(), s.cwd.as_deref(), home());
        Some(policy::decide(&loaded.policy, p, &env, sev))
    });
    let enabled = app.agents_ui.auto.enabled(uid);
    match fire_check(&pend, enabled, s.waiting, typed_since, current_sig.as_deref(), fresh.as_ref()) {
        FireCheck::Drop(_) => {}
        FireCheck::Dismiss(_) => app.agents_ui.auto.dismiss(uid, &pend.sig),
        FireCheck::Send(option) => {
            let Some(p) = prompt else { return };
            match super::console::send_verified(app, uid, &p, option, true) {
                Ok(()) => finish(app, &s, &pend),
                Err(_) => {} // the next pass re-evaluates whatever is on screen now
            }
        }
    }
}

fn request_text(pend: &Pending) -> String {
    format!("{}: {}", pend.tool.map_or("request", |t| t.label()), one_line(&pend.subject))
}

fn finish(app: &mut App, s: &Sess, pend: &Pending) {
    let e = LogEntry {
        ts: now_unix(),
        agent: s.kind.slug().to_string(),
        pane: s.uid,
        request: request_text(pend),
        decision: pend.verdict.label().to_string(),
        rule: pend.rule.clone(),
        reason: pend.reason.clone(),
    };
    app.agents_rt.auto_host.log(&e);
    app.agents_ui.policy_log.push(e);
    app.agents_ui.auto.record(s.uid, pend.verdict);
    if pend.verdict == Verdict::Deny {
        let body = format!("{} was denied automatically: {} ({})", s.kind.name(), cut(&one_line(&pend.subject), 60), pend.reason);
        super::notify::post("Rift autopilot", &body, app.config.agents.sound);
        toast(app, format!("Autopilot denied `{}`: {}", cut(&one_line(&pend.subject), 50), pend.reason));
    }
    app.request_redraw();
}

pub fn toggle_global(app: &mut App) {
    let on = !app.agents_ui.auto.global();
    app.agents_ui.auto.set_global(on);
    toast(app, if on { "Autopilot on for all agents (Critical is never auto-approved)" } else { "Autopilot off for all agents" });
}

pub fn toggle_agent(app: &mut App, uid: usize) {
    let on = app.agents_ui.auto.toggle_agent(uid);
    let name = app.agents.session(uid).map_or("agent", |s| s.kind.name());
    let msg = format!("Autopilot {} for {name}", if on { "on" } else { "off" });
    toast(app, msg);
}

/// A click on a card's autopilot line: stop the countdown, else flip the switch.
pub fn click_card(app: &mut App, uid: usize) {
    if app.agents_ui.auto.cancel(uid) {
        app.request_redraw();
    } else {
        toggle_agent(app, uid);
    }
}

pub fn open_log(app: &mut App) {
    let file = app.agents_rt.auto_host.paths.log.clone();
    app.agents_ui.policy_log.open(&file);
    app.request_redraw();
}

/// "Always allow this": show the rule text, add it on confirmation.
pub fn always_allow(app: &mut App, uid: usize) {
    let Some(s) = sessions(app).into_iter().find(|s| s.uid == uid) else { return };
    let Some(info) = app.agents_ui.info.get(&uid).cloned() else { return };
    let Some(p) = info.prompt else { return toast(app, "No approval prompt on this card") };
    let sev = match severity_of(&info.risk) {
        Some(s) => s,
        None => return toast(app, "Still checking the command, try again in a moment"),
    };
    let root = s.root.clone().or_else(|| s.cwd.clone());
    let env = PathEnv::new(root.as_deref(), s.cwd.as_deref(), home());
    let loaded = app.agents_rt.auto_host.policy_for(root.as_deref(), Instant::now());
    // Already allowed by policy: nothing to add.
    let out = policy::decide(&loaded.policy, &p, &env, sev);
    if out.verdict == Verdict::Approve {
        return toast(app, format!("Already allowed by {}", out.rule));
    }
    if out.verdict == Verdict::Deny {
        return toast(app, format!("Not allowed: {}", out.reason));
    }
    let rules = policy::suggest_rules(&loaded.policy, &p, &env, sev);
    if rules.is_empty() {
        return toast(app, "This request cannot be allowed automatically (protected, critical or not understood)");
    }
    let mut lines = vec![format!("Add to {}:", app.agents_rt.auto_host.paths.policy.display()), String::new()];
    for r in &rules {
        lines.extend(r.lines().map(str::to_string));
        lines.push(String::new());
    }
    lines.push("Autopilot still has to be on for the rule to act, and Critical commands are never auto-approved.".into());
    app.confirm.push(crate::ui::confirm::ConfirmRequest {
        title: "Always allow this?".into(),
        badge: Some(("POLICY".into(), Tone::Accent)),
        lines,
        buttons: vec!["Add rule".into(), "Cancel".into()],
        default_sel: 1,
        esc_choice: Some(1),
        tone: Tone::Warning,
        action: crate::ui::confirm::ConfirmAction::Policy(Box::new(PolicyConfirm::AlwaysAllow { rules })),
    });
    app.request_redraw();
}

/// Starter file for "Edit Policy" when none exists.
pub const STARTER: &str = r##"# Rift auto-approval policy (Mission Control autopilot).
#
# Rules are tried in order, first match wins: repo policy, then this file, then
# the built-in defaults (read-only commands, edits inside the repo, ...).
# Autopilot is off until you turn it on (p / P in Mission Control).
# Critical commands (rm -rf /, curl | sh, ...) are never auto-approved.
#
# [autopilot]
# countdown_ms = 1500      # 0 = answer immediately
#
# [policy]
# defaults = true          # false drops the built-in rules
#
# [[rule]]
# id = "allow-make-test"
# tool = "bash"            # bash | edit | write | file | read | web_fetch | mcp
# program = "make"
# subcommand = "test"
# action = "approve"       # approve | deny | ask
# reason = "tests are safe here"
#
# [[rule]]
# id = "never-print-secrets"
# tool = "bash"
# program = ["cat", "rg", "grep"]
# command = "* .env*"
# action = "deny"
"##;

/// Open the user's policy in `$EDITOR` in a new pane (creating a starter file first).
pub fn edit_policy(app: &mut App) {
    let path = app.agents_rt.auto_host.paths.policy.clone();
    if !path.exists() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&path, STARTER) {
            return toast(app, format!("Could not create {}: {e}", path.display()));
        }
    }
    let quoted = format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
    let cmd = format!("${{EDITOR:-vi}} {quoted}");
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    super::runtime::open_command_tab(app, &dir, "policy.toml", cmd);
    app.agents_rt.auto_host.invalidate();
}

/// MCP `run_command`: should policy answer it? Returns the decision and the
/// agent that owns the pane (for the log).
pub fn mcp_decision(app: &mut App, pane_id: usize, command: &str, cwd: Option<&str>, critical: Option<bool>) -> McpAuto {
    let owner = app.agents.session(pane_id).map(|s| (s.pane_uid, s.kind, s.git_root.clone()));
    let enabled = match &owner {
        Some((uid, ..)) => app.agents_ui.auto.enabled(*uid),
        None => app.agents_ui.auto.global(),
    };
    if !enabled {
        return McpAuto::Ask;
    }
    let root = owner.as_ref().and_then(|o| o.2.clone()).or_else(|| cwd.map(str::to_string));
    let loaded = app.agents_rt.auto_host.policy_for(root.as_deref(), Instant::now());
    if let Some(msg) = app.agents_rt.auto_host.take_errors(&loaded.policy) {
        toast(app, msg);
    }
    if let Some(need) = loaded.trust_needed {
        show_trust_modal(app, need);
    }
    let env = PathEnv::new(root.as_deref(), cwd, home());
    mcp_verdict(&loaded.policy, &env, command, critical, true, owner.map(|o| o.1))
}

/// Log an automatic MCP decision and count it for the owning agent.
pub fn mcp_record(app: &mut App, pane_id: usize, command: &str, verdict: Verdict, d: &Decision) {
    let owner = app.agents.session(pane_id).map(|s| s.pane_uid);
    let e = LogEntry {
        ts: now_unix(),
        agent: "mcp".into(),
        pane: pane_id,
        request: format!("bash: {}", one_line(command)),
        decision: verdict.label().to_string(),
        rule: d.rule.clone(),
        reason: d.reason.clone(),
    };
    app.agents_rt.auto_host.log(&e);
    app.agents_ui.policy_log.push(e);
    if let Some(uid) = owner {
        app.agents_ui.auto.record(uid, verdict);
    }
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::policy::Outcome;

    fn out(verdict: Verdict, option: Option<usize>, rule: &str) -> Outcome {
        Outcome { verdict, option, rule: rule.into(), reason: "because".into(), subject: "cargo test".into(), tool: Some(Tool::Bash) }
    }

    fn ap(on: bool) -> Autopilot {
        let mut a = Autopilot::default();
        if on {
            a.set_global(true);
        }
        a
    }

    fn secs(n: f64) -> Duration {
        Duration::from_millis((n * 1000.0) as u64)
    }

    // ── switches ──

    #[test]
    fn autopilot_is_off_by_default_for_everyone() {
        let a = Autopilot::default();
        assert!(!a.enabled(1) && !a.enabled(99) && !a.global() && !a.active());
        assert_eq!(header_label(&a).0, "AUTO off");
    }

    #[test]
    fn nothing_starts_while_it_is_off() {
        let mut a = ap(false);
        let t0 = Instant::now();
        a.consider(1, "s", &out(Verdict::Approve, Some(0), "r"), t0);
        assert!(!a.any_pending() && a.due(t0 + secs(60.0)).is_empty());
        assert!(a.next_deadline().is_none());
    }

    #[test]
    fn per_agent_and_global_switches() {
        let mut a = ap(false);
        assert!(a.toggle_agent(2));
        assert!(a.enabled(2) && !a.enabled(1) && a.active());
        assert_eq!((a.individual_on(), header_label(&a).0.as_str()), (1, "AUTO 1"));
        assert!(!a.toggle_agent(2));
        assert!(!a.active(), "back to nothing");
        a.set_global(true);
        assert!(a.enabled(1) && a.enabled(7));
        assert_eq!(header_label(&a), ("AUTO ON".to_string(), Tone::Warning));
        // One agent opts out while the global switch is on.
        assert!(!a.toggle_agent(3));
        assert!(!a.enabled(3) && a.enabled(4));
        // Flipping it back removes the override rather than pinning it.
        assert!(a.toggle_agent(3));
        assert!(a.enabled(3));
        // Global off clears individual choices and countdowns.
        a.toggle_agent(5);
        a.consider(1, "s", &out(Verdict::Approve, Some(0), "r"), Instant::now());
        a.set_global(false);
        assert!(!a.enabled(1) && !a.enabled(5) && !a.any_pending());
    }

    // ── countdown ──

    #[test]
    fn countdown_runs_out_and_fires_once() {
        let mut a = ap(true);
        let t0 = Instant::now();
        a.consider(1, "s", &out(Verdict::Approve, Some(0), "default:cargo-checks"), t0);
        assert!(a.any_pending());
        assert_eq!(a.next_deadline(), Some(t0 + secs(1.5)));
        assert!(a.due(t0 + secs(1.4)).is_empty());
        let due = a.due(t0 + secs(1.5));
        assert_eq!(due.len(), 1);
        assert_eq!((due[0].0, due[0].1.option, due[0].1.rule.as_str()), (1, 0, "default:cargo-checks"));
        assert!(a.due(t0 + secs(9.0)).is_empty(), "fires once");
        assert!(!a.any_pending());
    }

    #[test]
    fn an_unchanged_decision_keeps_its_timer() {
        let mut a = ap(true);
        let t0 = Instant::now();
        let o = out(Verdict::Approve, Some(0), "r");
        a.consider(1, "s", &o, t0);
        a.consider(1, "s", &o, t0 + secs(1.0));
        assert_eq!(a.pending(1).unwrap().deadline, t0 + secs(1.5), "refreshes do not restart the countdown");
        // A different prompt (or verdict) restarts it.
        a.consider(1, "other", &o, t0 + secs(1.0));
        assert_eq!(a.pending(1).unwrap().deadline, t0 + secs(2.5));
        a.consider(1, "other", &out(Verdict::Deny, Some(2), "r"), t0 + secs(1.2));
        assert_eq!(a.pending(1).unwrap().verdict, Verdict::Deny);
    }

    #[test]
    fn zero_countdown_is_immediate() {
        let mut a = ap(true);
        a.countdown_ms = 0;
        let t0 = Instant::now();
        a.consider(1, "s", &out(Verdict::Approve, Some(0), "r"), t0);
        assert_eq!(a.due(t0).len(), 1);
    }

    #[test]
    fn ask_or_a_missing_option_cancels_the_countdown() {
        let mut a = ap(true);
        let t0 = Instant::now();
        a.consider(1, "s", &out(Verdict::Approve, Some(0), "r"), t0);
        a.consider(1, "s", &out(Verdict::Ask, None, "r"), t0 + secs(0.5));
        assert!(!a.any_pending());
        a.consider(1, "s", &out(Verdict::Approve, None, "r"), t0);
        assert!(!a.any_pending(), "no option to press, nothing to count down to");
    }

    #[test]
    fn escape_cancels_and_the_prompt_stays_a_question() {
        let mut a = ap(true);
        let t0 = Instant::now();
        a.consider(1, "s", &out(Verdict::Approve, Some(0), "r"), t0);
        a.consider(2, "t", &out(Verdict::Deny, Some(2), "r"), t0);
        assert!(a.cancel_all());
        assert!(!a.any_pending());
        assert!(!a.cancel_all(), "nothing left to stop");
        // The next refresh offers the very same prompt: it must not restart.
        a.consider(1, "s", &out(Verdict::Approve, Some(0), "r"), t0 + secs(0.3));
        a.consider(2, "t", &out(Verdict::Deny, Some(2), "r"), t0 + secs(0.3));
        assert!(!a.any_pending());
        assert!(a.due(t0 + secs(60.0)).is_empty());
        // A new prompt on the same agent is a new question.
        a.consider(1, "s2", &out(Verdict::Approve, Some(0), "r"), t0 + secs(5.0));
        assert!(a.pending(1).is_some());
        // The old dismissal is gone for good once the agent leaves the prompt.
        a.cancel(1);
        a.clear_prompt(1);
        a.consider(1, "s2", &out(Verdict::Approve, Some(0), "r"), t0 + secs(9.0));
        assert!(a.pending(1).is_some());
    }

    #[test]
    fn cancel_one_agent_only() {
        let mut a = ap(true);
        let t0 = Instant::now();
        a.consider(1, "s", &out(Verdict::Approve, Some(0), "r"), t0);
        a.consider(2, "t", &out(Verdict::Approve, Some(0), "r"), t0);
        assert!(a.cancel(1));
        assert!(!a.cancel(1));
        assert!(a.pending(2).is_some());
    }

    #[test]
    fn switching_an_agent_off_stops_its_countdown() {
        let mut a = ap(false);
        a.toggle_agent(1);
        let t0 = Instant::now();
        a.consider(1, "s", &out(Verdict::Approve, Some(0), "r"), t0);
        a.toggle_agent(1);
        assert!(!a.any_pending());
    }

    #[test]
    fn started_countdowns_are_announced_once() {
        let mut a = ap(true);
        let t0 = Instant::now();
        a.consider(1, "s", &out(Verdict::Approve, Some(0), "r"), t0);
        a.consider(1, "s", &out(Verdict::Approve, Some(0), "r"), t0 + secs(0.2));
        a.consider(2, "t", &out(Verdict::Ask, None, "r"), t0);
        let s = a.take_started();
        assert_eq!(s.iter().map(|(u, _)| *u).collect::<Vec<_>>(), [1], "a refresh is not a new countdown, and an ask is none");
        assert!(a.take_started().is_empty());
        a.consider(1, "s2", &out(Verdict::Approve, Some(0), "r"), t0 + secs(1.0));
        assert_eq!(a.take_started().len(), 1, "a new prompt is");
    }

    #[test]
    fn counters_and_prune() {
        let mut a = ap(true);
        a.record(1, Verdict::Approve);
        a.record(1, Verdict::Approve);
        a.record(1, Verdict::Deny);
        a.record(1, Verdict::Ask);
        assert_eq!(a.counts(1), Counts { approved: 2, denied: 1 });
        a.toggle_agent(2);
        a.prune(&[2]);
        assert_eq!(a.counts(1), Counts::default());
        assert!(!a.enabled(2) || a.enabled(2) == a.global());
    }

    #[test]
    fn due_orders_by_agent() {
        let mut a = ap(true);
        let t0 = Instant::now();
        for uid in [5, 3, 4] {
            a.consider(uid, "s", &out(Verdict::Approve, Some(0), "r"), t0);
        }
        assert_eq!(a.due(t0 + secs(2.0)).iter().map(|d| d.0).collect::<Vec<_>>(), [3, 4, 5]);
    }

    // ── re-verification at the moment of answering ──

    #[test]
    fn firing_rechecks_everything() {
        let t0 = Instant::now();
        let mut a = ap(true);
        a.consider(1, "sig", &out(Verdict::Approve, Some(1), "r"), t0);
        let p = a.due(t0 + secs(2.0)).remove(0).1;
        let same = out(Verdict::Approve, Some(1), "r");
        assert_eq!(fire_check(&p, true, true, false, Some("sig"), Some(&same)), FireCheck::Send(1));
        assert!(matches!(fire_check(&p, false, true, false, Some("sig"), Some(&same)), FireCheck::Drop(_)), "autopilot switched off meanwhile");
        assert!(matches!(fire_check(&p, true, false, false, Some("sig"), Some(&same)), FireCheck::Drop(_)), "agent moved on");
        assert!(matches!(fire_check(&p, true, true, true, Some("sig"), Some(&same)), FireCheck::Dismiss(_)), "you typed in the pane");
        assert!(matches!(fire_check(&p, true, true, false, Some("other"), Some(&same)), FireCheck::Drop(_)), "prompt changed");
        assert!(matches!(fire_check(&p, true, true, false, None, Some(&same)), FireCheck::Drop(_)));
        let asks = out(Verdict::Ask, None, "r");
        assert!(matches!(fire_check(&p, true, true, false, Some("sig"), Some(&asks)), FireCheck::Drop(_)), "policy edited meanwhile");
        let other_opt = out(Verdict::Approve, Some(0), "r");
        assert!(matches!(fire_check(&p, true, true, false, Some("sig"), Some(&other_opt)), FireCheck::Drop(_)));
        assert!(matches!(fire_check(&p, true, true, false, Some("sig"), None), FireCheck::Drop(_)));
    }

    // ── card text ──

    #[test]
    fn remaining_time_formats_like_a_countdown() {
        assert_eq!(fmt_remaining(secs(1.5)), "1.5s");
        assert_eq!(fmt_remaining(Duration::from_millis(1401)), "1.5s", "rounded up");
        assert_eq!(fmt_remaining(Duration::from_millis(1)), "0.1s");
        assert_eq!(fmt_remaining(Duration::ZERO), "now");
        assert_eq!(fmt_remaining(secs(12.0)), "12.0s");
    }

    #[test]
    fn countdown_text_matches_the_spec_and_fits_the_card() {
        let l = countdown_lines(Verdict::Approve, "cargo test", secs(1.5), 60);
        assert_eq!(l, vec!["Auto-approving `cargo test` in 1.5s \u{2014} Esc to stop"]);
        let l = countdown_lines(Verdict::Deny, "rm -rf /", secs(0.3), 60);
        assert_eq!(l, vec!["Auto-denying `rm -rf /` in 0.3s \u{2014} Esc to stop"]);
        // Long commands give way, the countdown and the way out stay.
        let l = countdown_lines(Verdict::Approve, "cargo test --bin rift -- --test-threads=4 agents::policy", secs(1.5), 50);
        assert_eq!(l.len(), 1);
        assert!(l[0].chars().count() <= 50 && l[0].contains('\u{2026}') && l[0].ends_with("in 1.5s \u{2014} Esc to stop"), "{l:?}");
        // Very narrow: two lines, nothing wider than the card.
        let l = countdown_lines(Verdict::Approve, "cargo test", secs(1.5), 28);
        assert_eq!(l.len(), 2);
        assert!(l.iter().all(|x| x.chars().count() <= 28), "{l:?}");
        assert!(l[1].starts_with("in 1.5s") && l[1].contains("Esc to stop"), "{l:?}");
        // Newlines in a command never break the card.
        assert!(countdown_lines(Verdict::Approve, "a\nb", secs(1.0), 60)[0].contains("`a b`"));
    }

    #[test]
    fn auto_lines_for_every_state() {
        let t = |enabled, a, d, cd| AutoView { enabled, counts: Counts { approved: a, denied: d }, countdown: cd };
        assert!(auto_lines(&t(false, 0, 0, None), 40, false).is_empty(), "quiet cards show nothing");
        assert_eq!(auto_lines(&t(false, 0, 0, None), 40, true)[0].0, "autopilot off \u{b7} p to turn on");
        assert_eq!(auto_lines(&t(true, 0, 0, None), 40, false)[0], ("autopilot on".to_string(), Tone::Accent));
        assert_eq!(auto_lines(&t(true, 12, 0, None), 40, false)[0].0, "autopilot on \u{b7} 12 auto-approved");
        assert_eq!(auto_lines(&t(true, 12, 2, None), 60, false)[0].0, "autopilot on \u{b7} 12 auto-approved \u{b7} 2 denied");
        assert_eq!(auto_lines(&t(false, 3, 0, None), 40, false)[0].0, "autopilot off \u{b7} 3 auto-approved");
        let l = auto_lines(&t(true, 0, 0, Some((Verdict::Approve, "cargo test".into(), secs(1.5)))), 60, false);
        assert_eq!((l.len(), l[0].0.as_str(), l[0].1), (1, "Auto-approving `cargo test` in 1.5s \u{2014} Esc to stop", Tone::Warning));
        // Totals stay visible under a running countdown.
        let l = auto_lines(&t(true, 12, 1, Some((Verdict::Approve, "cargo test".into(), secs(1.5)))), 60, false);
        assert_eq!((l.len(), l[1].0.as_str()), (2, "12 auto-approved \u{b7} 1 denied"));
        let l = auto_lines(&t(true, 0, 0, Some((Verdict::Deny, "rm -rf /".into(), secs(1.0)))), 60, false);
        assert_eq!(l[0].1, Tone::Danger);
        assert!(auto_lines(&t(true, 99, 99, None), 12, false)[0].0.chars().count() <= 12);
    }

    #[test]
    fn view_reads_the_machine() {
        let mut a = ap(true);
        let t0 = Instant::now();
        a.consider(1, "s", &out(Verdict::Approve, Some(0), "r"), t0);
        a.record(1, Verdict::Approve);
        let v = a.view(1, t0 + secs(0.5));
        assert!(v.enabled);
        assert_eq!(v.counts.approved, 1);
        let (verdict, subject, left) = v.countdown.unwrap();
        assert_eq!((verdict, subject.as_str(), left), (Verdict::Approve, "cargo test", secs(1.0)));
        assert!(a.view(2, t0).countdown.is_none());
    }

    // ── host: files, merge, trust ──

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rift-autopilot-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(&d).unwrap()
    }

    const REPO_POLICY: &str = "[[rule]]\nid = \"deploy\"\nprogram = \"make\"\nsubcommand = \"deploy\"\naction = \"approve\"\n";

    fn env_for(dir: &Path) -> PathEnv {
        PathEnv::new(dir.to_str(), dir.to_str(), Some(PathBuf::from("/Users/test")))
    }

    #[test]
    fn user_policy_is_loaded_merged_and_reloaded() {
        let cfg = scratch("user");
        let mut h = Host::new(Paths::in_dir(&cfg));
        let t0 = Instant::now();
        let env = env_for(&cfg);
        let ask = |p: &Policy, c: &str| policy::evaluate(p, &policy::Request { tool: Tool::Bash, subject: c.into(), agent: None }, &env, None).verdict;
        // No file: just the defaults.
        let l = h.policy_for(None, t0);
        assert_eq!(ask(&l.policy, "cargo check"), Verdict::Approve);
        assert_eq!(ask(&l.policy, "make deploy"), Verdict::Ask);
        // A file appears: picked up after the reload interval.
        std::fs::write(&h.paths.policy, "[autopilot]\ncountdown_ms = 250\n[[rule]]\nprogram = \"make\"\naction = \"approve\"\n").unwrap();
        assert_eq!(ask(&h.policy_for(None, t0 + secs(0.5)).policy, "make deploy"), Verdict::Ask, "cached for a moment");
        let l = h.policy_for(None, t0 + secs(3.0));
        assert_eq!(ask(&l.policy, "make deploy"), Verdict::Approve);
        assert_eq!(l.policy.countdown_ms, Some(250));
        let _ = std::fs::remove_dir_all(&cfg);
    }

    #[test]
    fn a_repo_policy_is_ignored_until_trusted_and_asked_about_once() {
        let cfg = scratch("trust-cfg");
        let repo = scratch("trust-repo");
        std::fs::create_dir_all(repo.join(".rift")).unwrap();
        std::fs::write(repo.join(".rift/policy.toml"), REPO_POLICY).unwrap();
        let root = repo.to_str().unwrap();
        let mut h = Host::new(Paths::in_dir(&cfg));
        let t0 = Instant::now();
        let env = env_for(&repo);
        let verdict = |p: &Policy| policy::evaluate(p, &policy::Request { tool: Tool::Bash, subject: "make deploy".into(), agent: None }, &env, None).verdict;

        let l = h.policy_for(Some(root), t0);
        assert_eq!(verdict(&l.policy), Verdict::Ask, "an untrusted repo cannot approve itself");
        let need = l.trust_needed.expect("first sight asks");
        assert_eq!(need.root, root);
        assert!(!need.changed);
        assert_eq!(need.hash, policy::sha256_hex(REPO_POLICY.as_bytes()));
        assert!(need.summary[0].starts_with("1 rule, 1 would auto-approve"), "{:?}", need.summary);
        assert!(need.summary.iter().any(|l| l.contains("approve make deploy")), "{:?}", need.summary);
        // Asked once per session, however often it is evaluated.
        assert!(h.policy_for(Some(root), t0 + secs(5.0)).trust_needed.is_none());
        assert_eq!(verdict(&h.policy_for(Some(root), t0 + secs(6.0)).policy), Verdict::Ask);

        // Trusting applies it (and persists across hosts).
        h.trust_repo(root, &need.hash).unwrap();
        let l = h.policy_for(Some(root), t0 + secs(7.0));
        assert_eq!(verdict(&l.policy), Verdict::Approve);
        assert!(l.policy.rules[0].id == "repo:deploy");
        let mut h2 = Host::new(Paths::in_dir(&cfg));
        assert_eq!(verdict(&h2.policy_for(Some(root), t0).policy), Verdict::Approve, "trust is remembered");

        // Any edit to the file revokes it and asks again, saying it changed.
        std::fs::write(repo.join(".rift/policy.toml"), format!("{REPO_POLICY}\n# sneaky\n")).unwrap();
        let l = h2.policy_for(Some(root), t0 + secs(10.0));
        assert_eq!(verdict(&l.policy), Verdict::Ask);
        assert!(l.trust_needed.expect("changed file asks again").changed);
        let _ = std::fs::remove_dir_all(&cfg);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn a_trusted_hash_of_another_repo_does_not_carry_over() {
        let cfg = scratch("trust-other-cfg");
        let a = scratch("trust-a");
        let b = scratch("trust-b");
        for d in [&a, &b] {
            std::fs::create_dir_all(d.join(".rift")).unwrap();
            std::fs::write(d.join(".rift/policy.toml"), REPO_POLICY).unwrap();
        }
        let mut h = Host::new(Paths::in_dir(&cfg));
        let t0 = Instant::now();
        let hash = policy::sha256_hex(REPO_POLICY.as_bytes());
        h.trust_repo(a.to_str().unwrap(), &hash).unwrap();
        assert!(h.policy_for(Some(a.to_str().unwrap()), t0).trust_needed.is_none());
        assert!(h.policy_for(Some(b.to_str().unwrap()), t0).trust_needed.is_some(), "same file, other repo: ask");
        for d in [&cfg, &a, &b] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn repo_countdown_and_defaults_switch_are_ignored() {
        let cfg = scratch("repo-limits-cfg");
        let repo = scratch("repo-limits");
        std::fs::create_dir_all(repo.join(".rift")).unwrap();
        let text = "[autopilot]\ncountdown_ms = 0\n[policy]\ndefaults = false\n";
        std::fs::write(repo.join(".rift/policy.toml"), text).unwrap();
        let root = repo.to_str().unwrap();
        let mut h = Host::new(Paths::in_dir(&cfg));
        h.trust_repo(root, &policy::sha256_hex(text.as_bytes())).unwrap();
        let l = h.policy_for(Some(root), Instant::now());
        assert_eq!(l.policy.countdown_ms, None);
        assert!(!l.policy.rules.is_empty(), "defaults stay");
        let _ = std::fs::remove_dir_all(&cfg);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn policy_errors_are_reported_once() {
        let cfg = scratch("errors");
        let mut h = Host::new(Paths::in_dir(&cfg));
        std::fs::write(&h.paths.policy, "[[rule]]\nbogus = 1\naction = \"ask\"\n").unwrap();
        let l = h.policy_for(None, Instant::now());
        let msg = h.take_errors(&l.policy).expect("reported");
        assert!(msg.starts_with("Policy: policy.toml: rule 1") && msg.contains("bogus"), "{msg}");
        assert!(h.take_errors(&l.policy).is_none(), "not again");
        let _ = std::fs::remove_dir_all(&cfg);
    }

    #[test]
    fn summary_flags_broad_approvals() {
        let s = summarize("[[rule]]\naction = \"approve\"\n[[rule]]\nprogram = \"ls\"\naction = \"approve\"\n");
        assert!(s.iter().any(|l| l.starts_with("WARNING: 1 approving rule matches very broadly")), "{s:?}");
        assert!(s[0].starts_with("2 rules, 2 would auto-approve"));
    }

    // ── MCP ──

    #[test]
    fn mcp_commands_follow_the_same_policy() {
        let dir = scratch("mcp");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let env = env_for(&dir);
        let p = policy::load_merged(None, None);
        let go = |cmd: &str, crit: Option<bool>, on: bool| mcp_verdict(&p, &env, cmd, crit, on, None);
        assert!(matches!(go("cargo test", None, true), McpAuto::Approve(d) if d.rule == "default:cargo-checks"));
        assert_eq!(go("cargo test", None, false), McpAuto::Ask, "autopilot off: the modal, as before");
        assert_eq!(go("git push", None, true), McpAuto::Ask);
        assert!(matches!(go("rm -rf /", Some(true), true), McpAuto::Deny(d) if d.rule == "builtin:critical"));
        assert!(matches!(go("ls", Some(true), true), McpAuto::Deny(_)), "the safety engine's Critical is a floor over the rules");
        assert_eq!(go("make deploy", Some(false), true), McpAuto::Ask);
        // A Warning only passes an explicit rule naming the program.
        let named = policy::load_merged(Some("[[rule]]\nprogram = \"git\"\naction = \"approve\"\n"), None);
        assert!(matches!(mcp_verdict(&named, &env, "git push --force", Some(false), true, None), McpAuto::Approve(_)));
        let broad = policy::load_merged(Some("[[rule]]\naction = \"approve\"\n"), None);
        assert_eq!(mcp_verdict(&broad, &env, "git push --force", Some(false), true, None), McpAuto::Ask);
        // Agent-scoped rules see the owner of the pane.
        let scoped = policy::load_merged(Some("[[rule]]\nagent = \"codex\"\nprogram = \"make\"\naction = \"approve\"\n"), None);
        assert!(matches!(mcp_verdict(&scoped, &env, "make x", None, true, Some(AgentKind::Codex)), McpAuto::Approve(_)));
        assert_eq!(mcp_verdict(&scoped, &env, "make x", None, true, None), McpAuto::Ask);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── log overlay ──

    fn entry(ts: u64, decision: &str) -> LogEntry {
        LogEntry { ts, agent: "claude".into(), pane: 3, request: "bash: cargo test".into(), decision: decision.into(), rule: "default:cargo-checks".into(), reason: "cargo check/test".into() }
    }

    #[test]
    fn log_rows_are_newest_first_and_formatted() {
        let mut l = PolicyLog { visible: true, ..Default::default() };
        l.push(entry(1_700_000_000, "approve"));
        l.push(entry(1_700_000_060, "deny"));
        let rows = l.rows();
        assert_eq!(rows[0], ["11-14 22:14:20", "claude", "#3", "bash: cargo test", "deny", "default:cargo-checks"]);
        assert_eq!(rows[1][4], "approve");
        // Entries only accumulate while the overlay is open (it reads the file when it opens).
        let mut closed = PolicyLog::default();
        closed.push(entry(1, "approve"));
        assert!(closed.entries.is_empty());
    }

    #[test]
    fn log_overlay_reads_the_file_and_scrolls() {
        let dir = scratch("logview");
        let file = dir.join("policy.log");
        for i in 0..30u64 {
            policy::append_log(&file, &entry(1_700_000_000 + i, "approve")).unwrap();
        }
        let mut l = PolicyLog::default();
        l.open(&file);
        assert!(l.visible);
        assert_eq!(l.entries.len(), 30);
        l.handle_key(LogKey::Up);
        assert_eq!(l.scroll(), 0);
        for _ in 0..50 {
            l.handle_key(LogKey::Down);
        }
        assert_eq!(l.scroll(), 29);
        l.handle_key(LogKey::PageUp);
        assert_eq!(l.scroll(), 19);
        l.handle_key(LogKey::Escape);
        assert!(!l.visible);
        // A missing file is an empty log, not an error.
        l.open(&dir.join("nope.log"));
        assert!(l.entries.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_overlay_draws_in_every_theme() {
        use crate::ui::kit::gallery::qa::each_theme;
        use crate::ui::kit::Rect;
        let mut l = PolicyLog { visible: true, ..Default::default() };
        each_theme("policy-log-empty", |b, w, h, f, t| l.render(b, w, h, f, t, Some(Rect::new(w / 4, 0, w * 3 / 4, h))));
        for i in 0..40 {
            l.push(entry(1_700_000_000 + i, if i % 7 == 0 { "deny" } else { "approve" }));
        }
        each_theme("policy-log", |b, w, h, f, t| l.render(b, w, h, f, t, Some(Rect::new(w / 4, 0, w * 3 / 4, h))));
        each_theme("policy-log-full", |b, w, h, f, t| l.render(b, w, h, f, t, None));
    }

    #[test]
    fn starter_file_parses_cleanly() {
        let p = policy::parse_policy(STARTER, policy::Source::User, "starter");
        assert!(p.errors.is_empty() && p.rules.is_empty(), "{:?}", p.errors);
        // Uncommenting its examples gives valid rules.
        let un: String = STARTER.lines().map(|l| l.strip_prefix("# ").unwrap_or(l)).collect::<Vec<_>>().join("\n");
        let p = policy::parse_policy(&un.replace("0 = answer immediately", ""), policy::Source::User, "starter");
        assert!(p.rules.len() == 2 && p.rules.iter().all(|r| r.invalid.is_none()), "{:?} {:?}", p.errors, p.rules.iter().map(|r| &r.invalid).collect::<Vec<_>>());
    }
}
