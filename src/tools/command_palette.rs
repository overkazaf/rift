//! Command Palette (Cmd+P): a VS Code / Raycast style launcher.
//!
//! * fuzzy subsequence matching with scoring + highlighted matches
//! * frecency (usage count x recency) persisted to `~/.config/rift/palette_history`
//! * sectioned list (Recent, Panes, Tabs, Tools, ...) with icon, category and shortcut
//! * parameterised commands: `theme `, `font 16`, `open <url>`, `ssh <alias>`,
//!   `cd <path>`, `> shell command`, `? question`
//!
//! The palette itself is pure state + drawing. Dispatch of the returned
//! [`PaletteAction`] lives in `app::overlays`, which maps most entries onto the
//! existing `ui::MenuAction` handlers.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::Theme;
use crate::network::browser::BrowserCmd;
use crate::renderer::font::FontManager;
use crate::ui::kit::draw::BOTTOM;
use crate::ui::kit::{Ctx, Rect, Tokens, Tone};
use crate::ui::MenuAction;
use crate::window::tab::{Direction, PaneCmd};

// ───────────────────────────── Fuzzy matching ─────────────────────────────

const SCORE_MATCH: i32 = 16;
const BONUS_WORD: i32 = 30;
const BONUS_CAMEL: i32 = 24;
const BONUS_PREFIX: i32 = 24;
const BONUS_CONSEC: i32 = 22;
const BONUS_CASE: i32 = 1;
const GAP_OPEN: i32 = 4;
const GAP_EXT: i32 = 1;
const LEADING_PENALTY_CAP: i32 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    pub score: i32,
    /// Char indices (into the haystack) of the matched characters, ascending.
    pub indices: Vec<usize>,
}

fn is_sep(c: char) -> bool {
    c.is_whitespace() || matches!(c, '-' | '_' | '/' | ':' | '.' | '>' | '(' | ')' | '[' | ']' | ',')
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn pos_bonus(t: &[char], j: usize) -> i32 {
    if j == 0 {
        return BONUS_WORD;
    }
    let (p, c) = (t[j - 1], t[j]);
    if is_sep(p) && !is_sep(c) {
        BONUS_WORD
    } else if p.is_lowercase() && c.is_uppercase() {
        BONUS_CAMEL
    } else {
        0
    }
}

/// Case-insensitive subsequence match of `query` (whitespace ignored) against
/// `text`, choosing the best-scoring alignment. Rewards a prefix match, word /
/// camelCase starts and consecutive runs; penalises gaps and leading offset.
pub fn fuzzy_match(query: &str, text: &str) -> Option<FuzzyMatch> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some(FuzzyMatch { score: 0, indices: Vec::new() });
    }
    let t: Vec<char> = text.chars().collect();
    let (m, n) = (q.len(), t.len());
    if m > n {
        return None;
    }
    let ql: Vec<char> = q.iter().map(|&c| lower(c)).collect();
    let tl: Vec<char> = t.iter().map(|&c| lower(c)).collect();

    // Cheap feasibility check before the DP.
    let mut qi = 0;
    for &c in &tl {
        if qi < m && c == ql[qi] {
            qi += 1;
        }
    }
    if qi < m {
        return None;
    }

    const NEG: i32 = i32::MIN / 2;
    let mut score = vec![NEG; m * n];
    let mut from = vec![usize::MAX; m * n];
    for i in 0..m {
        for j in i..n {
            if tl[j] != ql[i] {
                continue;
            }
            let mut s = SCORE_MATCH + pos_bonus(&t, j) + if t[j] == q[i] { BONUS_CASE } else { 0 };
            if i == 0 {
                if j == 0 {
                    s += BONUS_PREFIX;
                }
                s -= (j as i32).min(LEADING_PENALTY_CAP);
            } else {
                let mut best = NEG;
                let mut bk = usize::MAX;
                for k in (i - 1)..j {
                    let p = score[(i - 1) * n + k];
                    if p == NEG {
                        continue;
                    }
                    let c = if k + 1 == j {
                        p + BONUS_CONSEC
                    } else {
                        p - GAP_OPEN - GAP_EXT * (j - k - 2) as i32
                    };
                    if c > best {
                        best = c;
                        bk = k;
                    }
                }
                if best == NEG {
                    continue;
                }
                s += best;
                from[i * n + j] = bk;
            }
            score[i * n + j] = s;
        }
    }

    let (mut bj, mut best) = (usize::MAX, NEG);
    for j in 0..n {
        let s = score[(m - 1) * n + j];
        if s > best {
            best = s;
            bj = j;
        }
    }
    if bj == usize::MAX {
        return None;
    }
    let mut indices = vec![0usize; m];
    let mut j = bj;
    for i in (0..m).rev() {
        indices[i] = j;
        if i > 0 {
            j = from[i * n + j];
        }
    }
    // Shorter haystacks win ties.
    Some(FuzzyMatch { score: best - (n as i32) / 4, indices })
}

// ─────────────────────────────── Frecency ────────────────────────────────

const HISTORY_MAX: usize = 200;
const RECENT_MAX: usize = 5;
const RECENT_MIN_FRECENCY: f32 = 1.0;
const FRECENCY_BLEND_CAP: f32 = 40.0;
const HALF_LIFE_DAYS: f32 = 3.0;

/// Usage statistics: id -> (use count, last-used unix seconds).
#[derive(Default, Clone, Debug)]
pub struct History {
    map: HashMap<String, (u32, u64)>,
}

impl History {
    /// Format: one entry per line, `count<TAB>last_used<TAB>id`.
    pub fn parse(s: &str) -> Self {
        let mut map = HashMap::new();
        for line in s.lines() {
            let mut it = line.splitn(3, '\t');
            let (Some(c), Some(t), Some(id)) = (it.next(), it.next(), it.next()) else { continue };
            let (Ok(c), Ok(t)) = (c.parse::<u32>(), t.parse::<u64>()) else { continue };
            if !id.is_empty() {
                map.insert(id.to_string(), (c, t));
            }
        }
        Self { map }
    }

    pub fn serialize(&self) -> String {
        let mut rows: Vec<_> = self.map.iter().collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        let mut out = String::new();
        for (id, (c, t)) in rows {
            out.push_str(&format!("{c}\t{t}\t{id}\n"));
        }
        out
    }

    pub fn record(&mut self, id: &str, now: u64) {
        let e = self.map.entry(id.to_string()).or_insert((0, now));
        e.0 = e.0.saturating_add(1);
        e.1 = now;
        if self.map.len() > HISTORY_MAX {
            if let Some(oldest) = self.map.iter().min_by_key(|(_, v)| v.1).map(|(k, _)| k.clone()) {
                self.map.remove(&oldest);
            }
        }
    }

    /// `ln(1 + count) * 20 * 0.5^(age_days / 3)`: frequent AND recent wins.
    pub fn frecency(&self, id: &str, now: u64) -> f32 {
        let Some(&(count, last)) = self.map.get(id) else { return 0.0 };
        let age_days = now.saturating_sub(last) as f32 / 86_400.0;
        (count as f32).ln_1p() * 20.0 * 0.5f32.powf(age_days / HALF_LIFE_DAYS)
    }

    /// Frecency contribution to a fuzzy score (capped so strong matches still win).
    fn bonus(&self, id: &str, now: u64) -> i32 {
        self.frecency(id, now).min(FRECENCY_BLEND_CAP) as i32
    }
}

fn history_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".config").join("rift").join("palette_history"))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ───────────────────────────── Query parsing ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamKind {
    Theme,
    Font,
    Open,
    Ssh,
    Cd,
    Shell,
    Ask,
    /// `model <name>`: switch the AI model (local models first).
    Model,
    /// `agent <name>`: launch an AI coding agent (here / worktree / layout).
    Agent,
}

impl ParamKind {
    fn keyword(self) -> &'static str {
        match self {
            ParamKind::Theme => "theme",
            ParamKind::Font => "font",
            ParamKind::Open => "open",
            ParamKind::Ssh => "ssh",
            ParamKind::Cd => "cd",
            ParamKind::Shell => ">",
            ParamKind::Ask => "?",
            ParamKind::Model => "model",
            ParamKind::Agent => "agent",
        }
    }
}

const PARAM_WORDS: &[(&str, ParamKind)] = &[
    ("theme", ParamKind::Theme),
    ("font", ParamKind::Font),
    ("open", ParamKind::Open),
    ("ssh", ParamKind::Ssh),
    ("cd", ParamKind::Cd),
    ("model", ParamKind::Model),
    ("agent", ParamKind::Agent),
];

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed<'a> {
    Plain(&'a str),
    Param { kind: ParamKind, arg: &'a str },
}

/// `theme nord`, `font 16`, `open example.com`, `ssh prod`, `cd /tmp`,
/// `> ls -la`, `? how do I ...`. A keyword only switches mode once followed by
/// whitespace, so `theme` alone still searches normally.
pub fn parse_query(q: &str) -> Parsed<'_> {
    let t = q.trim_start();
    if let Some(rest) = t.strip_prefix('>') {
        return Parsed::Param { kind: ParamKind::Shell, arg: rest.trim() };
    }
    if let Some(rest) = t.strip_prefix('?') {
        return Parsed::Param { kind: ParamKind::Ask, arg: rest.trim() };
    }
    if let Some((word, rest)) = t.split_once(char::is_whitespace) {
        let w = word.to_lowercase();
        if let Some(&(_, kind)) = PARAM_WORDS.iter().find(|(k, _)| *k == w) {
            return Parsed::Param { kind, arg: rest.trim() };
        }
    }
    Parsed::Plain(q.trim())
}

/// "16", "16.5", "16px" -> 16.0 (None for junk / non-positive).
pub fn parse_font_size(arg: &str) -> Option<f32> {
    let s = arg.trim().trim_end_matches("px").trim_end_matches("pt").trim();
    let v: f32 = s.parse().ok()?;
    (v.is_finite() && v > 0.0).then_some(v)
}

/// `[user@]host[:port]` -> (user, host, port)
pub fn parse_ssh_spec(arg: &str) -> Option<(Option<String>, String, u16)> {
    let s = arg.trim();
    if s.is_empty() || s.contains(char::is_whitespace) {
        return None;
    }
    let (user, rest) = match s.split_once('@') {
        Some((u, r)) if !u.is_empty() => (Some(u.to_string()), r),
        Some(_) => return None,
        None => (None, s),
    };
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()?),
        None => (rest, 22),
    };
    if host.is_empty() {
        return None;
    }
    Some((user, host.to_string(), port))
}

/// Quote a path for `cd`: plain when safe, otherwise single-quoted. A leading
/// `~/` stays outside the quotes so the shell still expands it.
pub fn shell_quote_path(p: &str) -> String {
    let safe = |s: &str| {
        !s.is_empty()
            && s.chars().all(|c| c.is_ascii_alphanumeric() || "_./+-:@%=,~".contains(c))
            && !s.starts_with('-')
    };
    let p = p.trim();
    if p.is_empty() || p == "~" {
        return "~".to_string();
    }
    if p == "-" || safe(p) {
        return p.to_string();
    }
    let (head, tail) = match p.strip_prefix("~/") {
        Some(rest) => ("~/", rest),
        None => ("", p),
    };
    if tail.is_empty() {
        return head.to_string();
    }
    format!("{head}'{}'", tail.replace('\'', "'\\''"))
}

// ───────────────────────────── Data model ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Panes,
    Tabs,
    Windows,
    Tools,
    Ai,
    Agents,
    Browser,
    Effects,
    Themes,
    Settings,
}

impl Category {
    const ORDER: [Category; 10] = [
        Category::Panes,
        Category::Tabs,
        Category::Windows,
        Category::Tools,
        Category::Ai,
        Category::Agents,
        Category::Browser,
        Category::Effects,
        Category::Themes,
        Category::Settings,
    ];

    fn label(self) -> &'static str {
        match self {
            Category::Panes => "Panes",
            Category::Tabs => "Tabs",
            Category::Windows => "Windows",
            Category::Tools => "Tools",
            Category::Ai => "AI",
            Category::Agents => "Agents",
            Category::Browser => "Browser",
            Category::Effects => "Effects",
            Category::Themes => "Themes",
            Category::Settings => "Settings",
        }
    }

    /// ASCII-safe glyphs (every monospace font has them).
    fn icon(self) -> char {
        match self {
            Category::Panes => '#',
            Category::Tabs => '+',
            Category::Windows => '^',
            Category::Tools => '*',
            Category::Ai => '@',
            Category::Agents => '!',
            Category::Browser => '~',
            Category::Effects => '%',
            Category::Themes => '&',
            Category::Settings => '=',
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Recent,
    Cat(Category),
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Section::Recent => "Recent",
            Section::Cat(c) => c.label(),
        }
    }
}

/// Everything the palette can trigger. Dispatch lives in `app::overlays`.
#[derive(Clone, Debug)]
pub enum PaletteAction {
    /// Any existing menu feature; dispatched through `shortcuts::handle_menu_action`.
    Menu(MenuAction),
    Theme(String),
    FontSize(f32),
    OpenUrl(String),
    /// Saved-host alias, or a `[user@]host[:port]` spec.
    Ssh(String),
    Cd(String),
    Shell(String),
    AskAi(String),
    SwitchTab(usize),
    NextTab,
    PrevTab,
    /// Switch the active AI model (local or cloud) and persist the choice.
    SelectModel(crate::ai::local::picker::ModelChoice),
    /// Show the AI Privacy Report overlay.
    PrivacyReport,
    /// "MCP Activity": what connected coding agents asked Rift to do.
    McpActivity,
    /// "New Agent": launch an agent CLI (tab / worktree / grid).
    Agent(crate::agents::runtime::Launch),
    /// Workflows: best of N, write & review, fix tests, queue, compare.
    Workflow(crate::workflow::WorkflowCmd),
    /// Autopilot: toggle, policy log, edit policy.
    Autopilot(crate::agents::autopilot::AutopilotCmd),
    /// Internal: completes the query to this keyword; never dispatched.
    Template(&'static str),
}

impl PaletteAction {
    /// Whether Cmd+Enter (run and keep the palette open) makes sense.
    fn keep_open_ok(&self) -> bool {
        match self {
            PaletteAction::Theme(_) | PaletteAction::FontSize(_) | PaletteAction::NextTab | PaletteAction::PrevTab => true,
            PaletteAction::Menu(m) => matches!(
                m,
                MenuAction::CrtEffect | MenuAction::GlitchEffect | MenuAction::NeonEffect
                    | MenuAction::MatrixEffect | MenuAction::AmberEffect | MenuAction::HologramEffect | MenuAction::NoEffect
                    | MenuAction::ZoomIn | MenuAction::ZoomOut | MenuAction::ZoomReset
                    | MenuAction::HudToggle | MenuAction::BroadcastToggle | MenuAction::SecretMask
                    | MenuAction::TeachingMode | MenuAction::ObserverMode | MenuAction::AdvisorMode
                    | MenuAction::Recording | MenuAction::ToggleFullScreen
            ) || matches!(m, MenuAction::Pane(c) if !matches!(c, PaneCmd::ClosePane)),
            _ => false,
        }
    }
}

pub struct PaletteItem {
    pub id: String,
    pub name: String,
    pub category: Category,
    pub shortcut: Option<String>,
    /// `shortcut` is descriptive metadata (e.g. "current"), not a key binding.
    pub meta: bool,
    pub action: PaletteAction,
}

/// One saved SSH host, as shown in the palette.
#[derive(Clone, Debug)]
pub struct SshHostInfo {
    pub alias: String,
    /// `user@host:port`
    pub detail: String,
}

/// Dynamic state captured from the app each time the palette opens.
#[derive(Clone, Debug, Default)]
pub struct PaletteContext {
    pub tabs: Vec<String>,
    pub active_tab: usize,
    pub ssh_hosts: Vec<SshHostInfo>,
    pub font_size: f32,
    /// Models for "AI: Select Model" (discovered local ones, then the cloud one).
    pub models: Vec<crate::ai::local::picker::ModelOption>,
    /// Agent CLIs found on this machine (for `agent ...`).
    pub agents: Vec<crate::agents::AgentKind>,
    /// The active pane sits inside a git repository (worktrees possible).
    pub in_git_repo: bool,
    /// The user's own workflow templates (`workflows.toml`): (name, detail).
    pub workflows: Vec<(String, String)>,
}

/// Live-preview request for the dispatcher (theme browsing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Preview {
    Theme(String),
    /// Put the committed theme back.
    Restore,
}

pub enum PaletteKey {
    Char(char),
    Backspace,
    Delete,
    /// Alt+Backspace
    DeleteWord,
    /// Cmd+Backspace
    DeleteToStart,
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
    Enter,
    /// Cmd+Enter: run and keep the palette open where sensible.
    EnterKeepOpen,
    Tab,
    Escape,
    Up,
    Down,
    PageUp,
    PageDown,
}

#[derive(Clone, Debug)]
struct Hit {
    id: Option<String>,
    name: String,
    category: Category,
    shortcut: Option<String>,
    meta: bool,
    action: PaletteAction,
    matches: Vec<usize>,
    section: Section,
}

#[derive(Clone, Copy, Debug)]
enum Row {
    Header(Section),
    Item(usize),
}

#[derive(Default)]
struct Layout {
    panel: (usize, usize, usize, usize),
    /// (y, height, hit index) for every drawn item row.
    rows: Vec<(usize, usize, usize)>,
}

const MAX_VISIBLE: usize = 12;

pub struct CommandPalette {
    pub visible: bool,
    pub query: String,
    /// Cursor position in chars.
    cursor: usize,
    items: Vec<PaletteItem>,
    dyn_items: Vec<PaletteItem>,
    ctx: PaletteContext,
    results: Vec<Hit>,
    rows: Vec<Row>,
    pub selected: usize,
    param: Option<(ParamKind, String)>,
    scroll: Cell<usize>,
    view_rows: Cell<usize>,
    layout: RefCell<Layout>,
    history: History,
    history_file: Option<PathBuf>,
    preview_applied: Option<String>,
    pending_preview: Option<Preview>,
}

impl CommandPalette {
    pub fn new() -> Self {
        let history = history_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| History::parse(&s))
            .unwrap_or_default();
        Self::with_parts(history, history_path())
    }

    pub(crate) fn with_parts(history: History, history_file: Option<PathBuf>) -> Self {
        let mut p = Self {
            visible: false,
            query: String::new(),
            cursor: 0,
            items: catalog(),
            dyn_items: Vec::new(),
            ctx: PaletteContext::default(),
            results: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            param: None,
            scroll: Cell::new(0),
            view_rows: Cell::new(MAX_VISIBLE),
            layout: RefCell::new(Layout::default()),
            history,
            history_file,
            preview_applied: None,
            pending_preview: None,
        };
        p.refilter();
        p
    }

    /// Open (or re-open) with fresh dynamic entries (tabs, saved hosts).
    pub fn open(&mut self, ctx: PaletteContext) {
        self.dyn_items = dynamic_items(&ctx);
        self.ctx = ctx;
        self.visible = true;
        self.query.clear();
        self.cursor = 0;
        self.preview_applied = None;
        self.pending_preview = None;
        self.scroll.set(0);
        self.on_query_changed();
    }

    /// Close without running anything; a live theme preview is rolled back.
    pub fn close(&mut self) {
        self.visible = false;
        if self.preview_applied.take().is_some() {
            self.pending_preview = Some(Preview::Restore);
        }
    }

    /// Close after running an action (the action itself supersedes any preview).
    fn close_committed(&mut self) {
        self.visible = false;
        self.preview_applied = None;
        self.pending_preview = None;
    }

    /// Preview request produced by the last key / mouse event, if any.
    pub fn take_preview(&mut self) -> Option<Preview> {
        self.pending_preview.take()
    }

    fn all_items(&self) -> impl Iterator<Item = &PaletteItem> {
        self.items.iter().chain(self.dyn_items.iter())
    }

    // ── filtering ──

    fn on_query_changed(&mut self) {
        self.refilter();
        self.selected = 0;
        self.scroll.set(0);
        self.update_preview();
    }

    fn refilter(&mut self) {
        let now = unix_now();
        let query = self.query.clone();
        let (hits, param) = match parse_query(&query) {
            Parsed::Plain(q) => (self.plain_hits(q, now), None),
            Parsed::Param { kind, arg } => (self.param_hits(kind, arg, now), Some((kind, arg.to_string()))),
        };
        let headers = param.is_none() && query.trim().is_empty();
        let mut rows = Vec::with_capacity(hits.len() + 10);
        let mut last: Option<Section> = None;
        for (i, h) in hits.iter().enumerate() {
            if headers && last != Some(h.section) {
                rows.push(Row::Header(h.section));
                last = Some(h.section);
            }
            rows.push(Row::Item(i));
        }
        self.results = hits;
        self.rows = rows;
        self.param = param;
        if self.selected >= self.results.len() {
            self.selected = 0;
        }
    }

    fn plain_hits(&self, q: &str, now: u64) -> Vec<Hit> {
        let mk = |it: &PaletteItem, matches: Vec<usize>, section: Section| Hit {
            id: Some(it.id.clone()).filter(|_| !matches!(it.action, PaletteAction::Template(_))),
            name: it.name.clone(),
            category: it.category,
            shortcut: it.shortcut.clone(),
            meta: it.meta,
            action: it.action.clone(),
            matches,
            section,
        };
        if q.is_empty() {
            let mut recent: Vec<(f32, &PaletteItem)> = self
                .all_items()
                .filter(|it| !matches!(it.action, PaletteAction::Template(_)))
                .map(|it| (self.history.frecency(&it.id, now), it))
                .filter(|(f, _)| *f >= RECENT_MIN_FRECENCY)
                .collect();
            recent.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            recent.truncate(RECENT_MAX);
            let mut out: Vec<Hit> = recent.iter().map(|(_, it)| mk(it, Vec::new(), Section::Recent)).collect();
            let used: Vec<&str> = recent.iter().map(|(_, it)| it.id.as_str()).collect();
            for cat in Category::ORDER {
                for it in self.all_items().filter(|it| it.category == cat && !used.contains(&it.id.as_str())) {
                    out.push(mk(it, Vec::new(), Section::Cat(cat)));
                }
            }
            return out;
        }
        let mut scored: Vec<(i32, usize, Hit)> = Vec::new();
        for (order, it) in self.all_items().enumerate() {
            if let Some(m) = fuzzy_match(q, &it.name) {
                let bonus = self.history.bonus(&it.id, now);
                scored.push((m.score + bonus, order, mk(it, m.indices, Section::Cat(it.category))));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, _, h)| h).collect()
    }

    fn param_hits(&self, kind: ParamKind, arg: &str, now: u64) -> Vec<Hit> {
        let sec = |c| Section::Cat(c);
        let one = |name: String, cat: Category, action: PaletteAction| {
            vec![Hit { id: None, name, category: cat, shortcut: None, meta: false, action, matches: Vec::new(), section: sec(cat) }]
        };
        match kind {
            ParamKind::Theme => {
                let mut v: Vec<(i32, usize, Hit)> = Vec::new();
                for (order, &name) in crate::config::Config::available_themes().iter().enumerate() {
                    let label = titlecase(name);
                    let Some(m) = fuzzy_match(arg, &label).or_else(|| fuzzy_match(arg, name)) else { continue };
                    let id = format!("Theme: {label}");
                    let bonus = self.history.bonus(&id, now);
                    v.push((m.score + bonus, order, Hit {
                        id: Some(id),
                        name: label,
                        category: Category::Themes,
                        shortcut: None,
                        meta: false,
                        action: PaletteAction::Theme(name.to_string()),
                        matches: m.indices,
                        section: sec(Category::Themes),
                    }));
                }
                v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
                v.into_iter().map(|(_, _, h)| h).collect()
            }
            ParamKind::Font => {
                if arg.is_empty() {
                    [11.0f32, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0, 24.0]
                        .iter()
                        .map(|&s| Hit {
                            id: None,
                            name: format!("{s} px"),
                            category: Category::Settings,
                            shortcut: (s == self.ctx.font_size).then(|| "current".to_string()),
                            meta: true,
                            action: PaletteAction::FontSize(s),
                            matches: Vec::new(),
                            section: sec(Category::Settings),
                        })
                        .collect()
                } else if let Some(s) = parse_font_size(arg) {
                    let c = s.clamp(8.0, 32.0);
                    let name = if c == s { format!("Set font size to {s}") } else { format!("Set font size to {c} (clamped from {s})") };
                    one(name, Category::Settings, PaletteAction::FontSize(c))
                } else {
                    Vec::new()
                }
            }
            ParamKind::Open if !arg.is_empty() => {
                one(format!("Open {arg}"), Category::Browser, PaletteAction::OpenUrl(arg.to_string()))
            }
            ParamKind::Cd if !arg.is_empty() => {
                one(format!("cd {arg}"), Category::Tools, PaletteAction::Cd(arg.to_string()))
            }
            ParamKind::Shell if !arg.is_empty() => {
                one(format!("Run: {arg}"), Category::Tools, PaletteAction::Shell(arg.to_string()))
            }
            ParamKind::Ask if !arg.is_empty() => {
                one(format!("Ask AI: {arg}"), Category::Ai, PaletteAction::AskAi(arg.to_string()))
            }
            ParamKind::Model => {
                let mut v: Vec<(i32, usize, Hit)> = Vec::new();
                for (order, m) in self.ctx.models.iter().enumerate() {
                    let Some(fm) = fuzzy_match(arg, &m.label) else { continue };
                    v.push((fm.score, order, Hit {
                        id: None,
                        name: m.label.clone(),
                        category: Category::Ai,
                        shortcut: Some(if m.active { format!("{} - current", m.detail) } else { m.detail.clone() }),
                        meta: true,
                        action: PaletteAction::SelectModel(m.choice.clone()),
                        matches: fm.indices,
                        section: sec(Category::Ai),
                    }));
                }
                v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
                v.into_iter().map(|(_, _, h)| h).collect()
            }
            ParamKind::Agent => self.agent_hits(arg),
            ParamKind::Ssh => {
                let mut v: Vec<(i32, usize, Hit)> = Vec::new();
                for (order, h) in self.ctx.ssh_hosts.iter().enumerate() {
                    let Some(m) = fuzzy_match(arg, &h.alias) else { continue };
                    let id = format!("ssh:{}", h.alias);
                    let bonus = self.history.bonus(&id, now);
                    v.push((m.score + bonus, order, Hit {
                        id: Some(id),
                        name: h.alias.clone(),
                        category: Category::Tools,
                        shortcut: Some(h.detail.clone()),
                        meta: true,
                        action: PaletteAction::Ssh(h.alias.clone()),
                        matches: m.indices,
                        section: sec(Category::Tools),
                    }));
                }
                v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
                let mut out: Vec<Hit> = v.into_iter().map(|(_, _, h)| h).collect();
                let exact = self.ctx.ssh_hosts.iter().any(|h| h.alias == arg);
                let looks_adhoc = out.is_empty() || arg.contains('@') || arg.contains(':');
                if !exact && looks_adhoc && parse_ssh_spec(arg).is_some() {
                    out.push(Hit {
                        id: None,
                        name: format!("Connect to {arg}"),
                        category: Category::Tools,
                        shortcut: None,
                        meta: false,
                        action: PaletteAction::Ssh(arg.to_string()),
                        matches: Vec::new(),
                        section: sec(Category::Tools),
                    });
                }
                out
            }
            _ => Vec::new(),
        }
    }

    /// `agent <query>`: installed agents x {current dir, new worktree}, plus grid layouts.
    fn agent_hits(&self, arg: &str) -> Vec<Hit> {
        use crate::agents::runtime::Launch;
        let sec = Section::Cat(Category::Agents);
        let mk = |name: String, detail: String, action: PaletteAction, matches: Vec<usize>| Hit {
            id: None,
            name,
            category: Category::Agents,
            shortcut: Some(detail),
            meta: true,
            action,
            matches,
            section: sec,
        };
        let mut scored: Vec<(i32, usize, Hit)> = Vec::new();
        let mut push = |label: String, detail: &str, action: PaletteAction| {
            let order = scored.len();
            if let Some(m) = fuzzy_match(arg, &label) {
                scored.push((m.score, order, mk(label, detail.to_string(), action, m.indices)));
            }
        };
        for &k in &self.ctx.agents {
            push(format!("{}: current directory", k.name()), "new tab", PaletteAction::Agent(Launch::Here(k)));
            if self.ctx.in_git_repo {
                push(
                    format!("{}: new git worktree", k.name()),
                    "git worktree add ../<repo>-<agent>-<n>",
                    PaletteAction::Agent(Launch::Worktree(k)),
                );
            }
        }
        if let Some(&k) = self.ctx.agents.first() {
            for (c, r) in [(2usize, 1usize), (2, 2), (3, 2)] {
                push(
                    format!("Layout {c}\u{d7}{r}: {} x{}", k.name(), c * r),
                    if self.ctx.in_git_repo { "one worktree each" } else { "shared directory" },
                    PaletteAction::Agent(Launch::Layout { kind: k, cols: c, rows: r }),
                );
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let mut out: Vec<Hit> = scored.into_iter().map(|(_, _, h)| h).collect();
        if self.ctx.agents.is_empty() {
            out.push(mk(
                "No agent CLI found on PATH".into(),
                "claude, codex, gemini, opencode, aider, cursor-agent".into(),
                PaletteAction::Template("agent "),
                Vec::new(),
            ));
        }
        out
    }

    // ── theme live preview ──

    fn update_preview(&mut self) {
        let desired = match (&self.param, self.results.get(self.selected)) {
            (Some((ParamKind::Theme, _)), Some(Hit { action: PaletteAction::Theme(n), .. })) if self.visible => Some(n.clone()),
            _ => None,
        };
        if desired == self.preview_applied {
            return;
        }
        self.pending_preview = Some(match &desired {
            Some(n) => Preview::Theme(n.clone()),
            None => Preview::Restore,
        });
        self.preview_applied = desired;
    }

    // ── selection / scrolling ──

    fn row_of_selected(&self) -> Option<usize> {
        self.rows.iter().position(|r| matches!(r, Row::Item(i) if *i == self.selected))
    }

    fn ensure_visible(&self) {
        let Some(r) = self.row_of_selected() else { return };
        let view = self.view_rows.get().max(1);
        let mut s = self.scroll.get();
        if r < s {
            s = r;
            if r > 0 && matches!(self.rows[r - 1], Row::Header(_)) {
                s = r - 1;
            }
        } else if r >= s + view {
            s = r + 1 - view;
        }
        self.scroll.set(s.min(self.rows.len().saturating_sub(view)));
    }

    fn move_selection(&mut self, delta: isize, wrap: bool) {
        let n = self.results.len() as isize;
        if n == 0 {
            return;
        }
        let mut next = self.selected as isize + delta;
        if wrap {
            next = next.rem_euclid(n);
        } else {
            next = next.clamp(0, n - 1);
        }
        self.selected = next as usize;
        self.ensure_visible();
        self.update_preview();
    }

    // ── editing ──

    pub(crate) fn set_query(&mut self, q: &str) {
        self.query = q.to_string();
        self.cursor = self.query.chars().count();
        self.on_query_changed();
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.query.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(self.query.len())
    }

    fn delete_range(&mut self, from: usize, to: usize) {
        if from >= to {
            return;
        }
        let (b0, b1) = (self.byte_at(from), self.byte_at(to));
        self.query.replace_range(b0..b1, "");
        self.cursor = from;
        self.on_query_changed();
    }

    fn word_left(&self) -> usize {
        let chars: Vec<char> = self.query.chars().collect();
        let mut i = self.cursor.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    fn word_right(&self) -> usize {
        let chars: Vec<char> = self.query.chars().collect();
        let mut i = self.cursor.min(chars.len());
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        i
    }

    /// Tab: complete a parameterised command keyword (or the highlighted theme / host).
    fn complete(&mut self) {
        if let Some((kind, _)) = &self.param {
            if matches!(kind, ParamKind::Theme | ParamKind::Ssh | ParamKind::Model) {
                if let Some(h) = self.results.get(self.selected) {
                    let q = format!("{} {}", kind.keyword(), h.name);
                    self.set_query(&q);
                }
            }
            return;
        }
        if let Some(Hit { action: PaletteAction::Template(t), .. }) = self.results.get(self.selected) {
            let t = *t;
            self.set_query(t);
            return;
        }
        let q = self.query.trim().to_lowercase();
        if !q.is_empty() {
            if let Some(&(kw, _)) = PARAM_WORDS.iter().find(|(k, _)| k.starts_with(&q)) {
                self.set_query(&format!("{kw} "));
            }
        }
    }

    fn activate(&mut self, keep_open: bool) -> Option<PaletteAction> {
        let hit = self.results.get(self.selected)?.clone();
        if let PaletteAction::Template(t) = hit.action {
            self.set_query(t);
            return None;
        }
        if let Some(id) = &hit.id {
            self.history.record(id, unix_now());
            self.save_history();
        }
        if keep_open && hit.action.keep_open_ok() {
            // Re-rank (frecency changed) but keep the same entry highlighted.
            self.preview_applied = None;
            self.pending_preview = None;
            self.refilter();
            if let Some(i) = self.results.iter().position(|h| h.name == hit.name && h.category == hit.category) {
                self.selected = i;
            }
            self.ensure_visible();
            self.update_preview();
        } else {
            self.close_committed();
        }
        Some(hit.action)
    }

    fn save_history(&self) {
        let Some(path) = &self.history_file else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(path, self.history.serialize()) {
            log::warn!("palette: cannot save history: {e}");
        }
    }

    pub fn handle_key(&mut self, key: PaletteKey) -> Option<PaletteAction> {
        match key {
            PaletteKey::Char(c) => {
                if !c.is_control() {
                    let b = self.byte_at(self.cursor);
                    self.query.insert(b, c);
                    self.cursor += 1;
                    self.on_query_changed();
                }
            }
            PaletteKey::Backspace => {
                if self.cursor > 0 {
                    self.delete_range(self.cursor - 1, self.cursor);
                }
            }
            PaletteKey::Delete => {
                if self.cursor < self.query.chars().count() {
                    self.delete_range(self.cursor, self.cursor + 1);
                }
            }
            PaletteKey::DeleteWord => {
                let from = self.word_left();
                self.delete_range(from, self.cursor);
            }
            PaletteKey::DeleteToStart => self.delete_range(0, self.cursor),
            PaletteKey::Left => self.cursor = self.cursor.saturating_sub(1),
            PaletteKey::Right => self.cursor = (self.cursor + 1).min(self.query.chars().count()),
            PaletteKey::WordLeft => self.cursor = self.word_left(),
            PaletteKey::WordRight => self.cursor = self.word_right(),
            PaletteKey::Home => self.cursor = 0,
            PaletteKey::End => self.cursor = self.query.chars().count(),
            PaletteKey::Up => self.move_selection(-1, true),
            PaletteKey::Down => self.move_selection(1, true),
            PaletteKey::PageUp => self.move_selection(-(self.view_rows.get() as isize), false),
            PaletteKey::PageDown => self.move_selection(self.view_rows.get() as isize, false),
            PaletteKey::Tab => self.complete(),
            PaletteKey::Enter => return self.activate(false),
            PaletteKey::EnterKeepOpen => return self.activate(true),
            PaletteKey::Escape => {
                if self.param.is_some() {
                    self.set_query("");
                } else {
                    self.close();
                }
            }
        }
        None
    }

    // ── mouse ──

    fn row_at(&self, x: usize, y: usize) -> Option<usize> {
        let l = self.layout.borrow();
        let (px, _, pw, _) = l.panel;
        if x < px || x >= px + pw {
            return None;
        }
        let hit = l.rows.iter().find(|(ry, rh, _)| y >= *ry && y < ry + rh).map(|r| r.2);
        hit
    }

    /// Hover: returns true when the highlighted row changed.
    pub fn mouse_move(&mut self, x: usize, y: usize) -> bool {
        match self.row_at(x, y) {
            Some(i) if i != self.selected => {
                self.selected = i;
                self.update_preview();
                true
            }
            _ => false,
        }
    }

    /// Click: runs the row under the cursor, or closes when clicking outside.
    pub fn mouse_click(&mut self, x: usize, y: usize, keep_open: bool) -> Option<PaletteAction> {
        let inside = {
            let l = self.layout.borrow();
            let (px, py, pw, ph) = l.panel;
            pw > 0 && x >= px && x < px + pw && y >= py && y < py + ph
        };
        if !inside {
            self.close();
            return None;
        }
        let i = self.row_at(x, y)?;
        self.selected = i;
        self.activate(keep_open)
    }

    /// Wheel: positive scrolls up.
    pub fn scroll_lines(&self, lines: i32) {
        let max = self.rows.len().saturating_sub(self.view_rows.get());
        let s = self.scroll.get() as i64 - lines as i64;
        self.scroll.set(s.clamp(0, max as i64) as usize);
    }

    // ── rendering ──

    /// (badge, description) shown under the input while a parameterised command is active.
    fn hint_parts(&self) -> Option<(&'static str, String)> {
        let (kind, _) = self.param.as_ref()?;
        let desc = match kind {
            ParamKind::Theme => "Up/Down previews live, Enter applies, Esc cancels".to_string(),
            ParamKind::Font => format!("Set font size 8-32 (now {})", self.ctx.font_size),
            ParamKind::Open => "URL or search terms, opens in the Rift browser".to_string(),
            ParamKind::Ssh => "Saved host alias, or user@host[:port]".to_string(),
            ParamKind::Cd => "Change directory in the active pane".to_string(),
            ParamKind::Shell => "Run a shell command in the active pane".to_string(),
            ParamKind::Ask => "Ask the AI assistant".to_string(),
            ParamKind::Model => "Local models run on this machine (nothing is sent out); Enter switches".to_string(),
            ParamKind::Agent => "Launch an AI coding agent: current directory, new git worktree, or a grid".to_string(),
        };
        Some((kind.keyword(), desc))
    }

    fn empty_text(&self) -> &'static str {
        match &self.param {
            Some((ParamKind::Theme, _)) => "No matching theme",
            Some((ParamKind::Ssh, _)) => "No matching saved host - try user@host[:port]",
            Some((ParamKind::Font, _)) => "Enter a font size, e.g. font 16",
            Some((ParamKind::Open, _)) => "Type a URL or search terms",
            Some((ParamKind::Cd, _)) => "Type a directory",
            Some((ParamKind::Shell, _)) => "Type a shell command",
            Some((ParamKind::Ask, _)) => "Type a question",
            Some((ParamKind::Agent, _)) => "No matching agent",
            Some((ParamKind::Model, _)) => "No model found - start Ollama / LM Studio, or set [llm] in config.toml",
            None => "No matching commands",
        }
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut FontManager,
        theme: &Theme,
    ) {
        if !self.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width.max(1), font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(tk.backdrop);

        let (cw, sp, row_h, input_h) = (tk.cw, tk.sp, tk.row_h, tk.input_h);
        let hint = self.hint_parts();
        let hint_h = if hint.is_some() { row_h } else { 0 };
        let footer_h = row_h + sp.xs;
        // input block + hint + paddings + footer (everything except the list)
        let chrome = sp.md + input_h + sp.xs + hint_h + sp.xs + sp.xs + footer_h;

        let total_rows = self.rows.len();
        let max_rows = ((height * 3 / 4).saturating_sub(chrome) / row_h).clamp(3, MAX_VISIBLE);
        let view = total_rows.max(1).min(max_rows);
        self.view_rows.set(view);
        self.ensure_visible();
        let scroll = self.scroll.get().min(total_rows.saturating_sub(view));
        self.scroll.set(scroll);

        let pw = (width * 6 / 10).clamp(52 * cw, 96 * cw).min(width.saturating_sub(2 * sp.lg).max(1));
        let ph = (chrome + view * row_h).min(height.saturating_sub(2 * sp.lg).max(1));
        let px = width.saturating_sub(pw) / 2;
        let py = height.saturating_sub(ph) / 4;
        let panel = Rect::new(px, py, pw, ph);

        // ── Panel ──
        cx.shadow(panel, tk.radius);
        cx.fill_rrect(panel, tk.radius, tk.border_strong);
        cx.fill_rrect(panel.inset(1, 1), tk.radius.saturating_sub(1), tk.surface);

        // ── Input ──
        let field = Rect::new(px + sp.md, py + sp.md, pw.saturating_sub(2 * sp.md), input_h);
        cx.text_input(
            field,
            &self.query,
            self.cursor,
            None,
            "Type a command, or: theme  font  open  ssh  cd  >  ?",
            true,
        );

        // ── Parameter hint ──
        let hint_y = field.bottom() + sp.xs;
        if let Some((badge, desc)) = &hint {
            let bw = cx.badge(px + sp.md, hint_y, badge, Tone::Accent, hint_h);
            let tx = px + sp.md + bw + sp.sm;
            let ty = cx.text_y(hint_y, hint_h);
            cx.text_fit(tx, ty, (px + pw).saturating_sub(tx + sp.md), desc, tk.text_muted);
        }

        // ── List ──
        let list_top = hint_y + hint_h + sp.xs;
        let list_rect = Rect::new(px + sp.sm, list_top, pw.saturating_sub(2 * sp.sm), view * row_h);
        let mut layout = Layout { panel: (px, py, pw, ph), rows: Vec::new() };
        if self.results.is_empty() {
            let ty = cx.text_y(list_top, row_h);
            cx.text_fit(list_rect.x + sp.md, ty, list_rect.w.saturating_sub(2 * sp.md), self.empty_text(), tk.text_faint);
        } else {
            let end = (scroll + view).min(total_rows);
            let gutter = if total_rows > view { 4 * tk.scale + sp.xs } else { 0 };
            let right_edge = list_rect.right().saturating_sub(sp.md + gutter);
            // shortcut column width = widest shortcut among the visible rows
            let sc_col = self.rows[scroll..end]
                .iter()
                .filter_map(|r| if let Row::Item(i) = r { Some(&self.results[*i]) } else { None })
                .map(|h| shortcut_width(&cx, h, pw))
                .max()
                .unwrap_or(0);
            let show_cat = pw >= 70 * cw;
            let cat_w = 9 * cw;

            for (n, row) in self.rows[scroll..end].iter().enumerate() {
                let ry = list_top + n * row_h;
                let ty = cx.text_y(ry, row_h);
                let rect = Rect::new(list_rect.x, ry, list_rect.w.saturating_sub(gutter), row_h);
                let x0 = rect.x + sp.md;
                match *row {
                    Row::Header(sec) => {
                        let c = if sec == Section::Recent { tk.accent } else { tk.text_faint };
                        cx.text(x0, ty, &sec.label().to_uppercase(), c);
                    }
                    Row::Item(i) => {
                        let h = &self.results[i];
                        let sel = i == self.selected;
                        layout.rows.push((ry, row_h, i));
                        cx.row_bg(rect, sel, false);

                        let mut ib = [0u8; 4];
                        let icon_c = if sel { tk.accent } else { tk.text_muted };
                        cx.text(x0 + sp.xs, ty, h.category.icon().encode_utf8(&mut ib), icon_c);

                        let name_x = x0 + sp.xs + 3 * cw;
                        let right_cols = sc_col + if show_cat { cat_w } else { 0 } + cw;
                        let name_max = right_edge.saturating_sub(name_x + right_cols) / cw;
                        let mut nb = [0u8; 4];
                        for (ci, c) in h.name.chars().enumerate().take(name_max) {
                            if c == ' ' {
                                continue;
                            }
                            let x = name_x + ci * cw;
                            let s = c.encode_utf8(&mut nb);
                            if h.matches.binary_search(&ci).is_ok() {
                                // accent + faux bold
                                cx.text(x, ty, s, tk.accent);
                                cx.text(x + 1, ty, s, tk.accent);
                            } else {
                                cx.text(x, ty, s, tk.text);
                            }
                        }

                        // shortcut (right-aligned) then category (muted) to its left
                        let sc_x = right_edge.saturating_sub(sc_col);
                        draw_shortcut(&mut cx, h, right_edge, rect.y, rect.h, pw);
                        if show_cat {
                            let cx_pos = sc_x.saturating_sub(cat_w);
                            let name_end = name_x + h.name.chars().count().min(name_max) * cw;
                            if cx_pos > name_end + cw {
                                cx.text(cx_pos, ty, h.category.label(), tk.text_faint);
                            }
                        }
                    }
                }
            }
            cx.scrollbar(Rect::new(list_rect.x, list_top, list_rect.w, view * row_h), total_rows, view, scroll);
        }
        *self.layout.borrow_mut() = layout;

        // ── Footer ──
        let foot_top = list_top + view * row_h + sp.xs;
        let fr = Rect::new(px + 1, foot_top, pw.saturating_sub(2), footer_h);
        cx.fill_rrect_ex(fr, tk.radius.saturating_sub(1), tk.surface_alt, 255, BOTTOM);
        cx.hline(fr.x, fr.y, fr.w, tk.border);
        let count = format!("{}/{}", if self.results.is_empty() { 0 } else { self.selected + 1 }, self.results.len());
        let count_w = cx.tw(&count);
        let keep = format!("{}+Enter", crate::config::mod_key());
        let hints: [(&str, &str); 5] = [
            ("Up/Down", "navigate"),
            ("Enter", "run"),
            (keep.as_str(), "keep open"),
            ("Tab", "complete"),
            ("Esc", "close"),
        ];
        cx.hint_row(px + sp.lg, foot_top + 1, pw.saturating_sub(3 * sp.lg + count_w), footer_h.saturating_sub(1), &hints);
        cx.text_right(px + pw - sp.lg, cx.text_y(foot_top, footer_h), &count, tk.text_muted);
    }
}

/// Shortcut split into key-cap parts (`Cmd+Shift+T` -> Cmd, Shift, T), or None
/// when it should be shown as plain muted text (metadata, or too wide).
fn shortcut_parts<'a>(cx: &Ctx, h: &'a Hit, panel_w: usize) -> Option<Vec<&'a str>> {
    let sc = h.shortcut.as_deref()?;
    if h.meta {
        return None;
    }
    let parts: Vec<&str> = if sc.ends_with("++") || sc == "+" { vec![sc] } else { sc.split('+').collect() };
    let w = chips_width(cx, &parts);
    (w <= panel_w / 3).then_some(parts)
}

fn chip_width(cx: &Ctx, key: &str) -> usize {
    cx.tw(key) + 2 * (cx.tk.sp.xs + 2 * cx.tk.scale)
}

fn chips_width(cx: &Ctx, parts: &[&str]) -> usize {
    parts.iter().map(|p| chip_width(cx, p)).sum::<usize>() + parts.len().saturating_sub(1) * cx.tk.sp.xs
}

fn shortcut_width(cx: &Ctx, h: &Hit, panel_w: usize) -> usize {
    match (&h.shortcut, shortcut_parts(cx, h, panel_w)) {
        (_, Some(parts)) => chips_width(cx, &parts),
        (Some(s), None) => cx.tw(s),
        _ => 0,
    }
}

fn draw_shortcut(cx: &mut Ctx, h: &Hit, right: usize, y: usize, row_h: usize, panel_w: usize) {
    let tk = cx.tk;
    if let Some(parts) = shortcut_parts(cx, h, panel_w) {
        let mut x = right.saturating_sub(chips_width(cx, &parts));
        for p in &parts {
            x += cx.kbd_chip(x, y, row_h, p) + tk.sp.xs;
        }
    } else if let Some(s) = &h.shortcut {
        let ty = cx.text_y(y, row_h);
        cx.text_right(right, ty, s, tk.text_muted);
    }
}

// ───────────────────────────── Catalog ─────────────────────────────

fn entry(name: &str, cat: Category, shortcut: Option<String>, action: PaletteAction) -> PaletteItem {
    PaletteItem { id: name.to_string(), name: name.to_string(), category: cat, shortcut, meta: false, action }
}

fn dynamic_items(ctx: &PaletteContext) -> Vec<PaletteItem> {
    let mut v = Vec::new();
    for m in &ctx.models {
        let mut it = entry(&format!("AI Model: {}", m.label), Category::Ai, Some(if m.active { format!("{} - current", m.detail) } else { m.detail.clone() }), PaletteAction::SelectModel(m.choice.clone()));
        it.meta = true;
        it.id = format!("ai-model:{}", m.label);
        v.push(it);
    }
    for (i, t) in ctx.tabs.iter().enumerate() {
        let mut it = entry(&format!("Tab {}: {}", i + 1, t), Category::Tabs, (i == ctx.active_tab).then(|| "current".to_string()), PaletteAction::SwitchTab(i));
        it.meta = true;
        it.id = format!("tab-dyn:{i}"); // index-based ids are unstable; keep them out of frecency
        v.push(it);
    }
    for (name, detail) in &ctx.workflows {
        let mut it = entry(&format!("Workflow: {name}"), Category::Agents, Some(detail.clone()), PaletteAction::Workflow(crate::workflow::WorkflowCmd::Template(name.clone())));
        it.meta = true;
        it.id = format!("workflow:{name}");
        v.push(it);
    }
    for h in &ctx.ssh_hosts {
        let mut it = entry(&format!("SSH: {}", h.alias), Category::Tools, Some(h.detail.clone()), PaletteAction::Ssh(h.alias.clone()));
        it.meta = true;
        it.id = format!("ssh:{}", h.alias);
        v.push(it);
    }
    v
}

fn catalog() -> Vec<PaletteItem> {
    use Category::*;
    use PaletteAction::Menu;
    let m = crate::config::mod_key();
    let s = |k: &str| Some(format!("{m}+{k}"));
    let mut v: Vec<PaletteItem> = Vec::new();

    // Panes
    v.push(entry("Split Right", Panes, s("D"), Menu(MenuAction::SplitH)));
    v.push(entry("Split Down", Panes, s("Shift+D"), Menu(MenuAction::SplitV)));
    v.push(entry("Close Pane", Panes, s("W"), Menu(MenuAction::Pane(PaneCmd::ClosePane))));
    v.push(entry("Zoom Pane", Panes, s("Shift+Enter"), Menu(MenuAction::Pane(PaneCmd::Zoom))));
    v.push(entry("Equalize Panes", Panes, s("Ctrl+="), Menu(MenuAction::Pane(PaneCmd::Equalize))));
    v.push(entry("Next Pane", Panes, s("]"), Menu(MenuAction::Pane(PaneCmd::FocusNext))));
    v.push(entry("Previous Pane", Panes, s("["), Menu(MenuAction::Pane(PaneCmd::FocusPrev))));
    for (label, d, arrow) in [("Left", Direction::Left, "Left"), ("Right", Direction::Right, "Right"), ("Up", Direction::Up, "Up"), ("Down", Direction::Down, "Down")] {
        v.push(entry(&format!("Focus Pane {label}"), Panes, s(&format!("Alt+{arrow}")), Menu(MenuAction::Pane(PaneCmd::Focus(d)))));
    }
    for (label, d, arrow) in [("Left", Direction::Left, "Left"), ("Right", Direction::Right, "Right"), ("Up", Direction::Up, "Up"), ("Down", Direction::Down, "Down")] {
        v.push(entry(&format!("Swap Pane {label}"), Panes, s(&format!("Ctrl+Shift+{arrow}")), Menu(MenuAction::Pane(PaneCmd::Swap(d)))));
    }
    for (label, d, arrow) in [("Left", Direction::Left, "Left"), ("Right", Direction::Right, "Right"), ("Up", Direction::Up, "Up"), ("Down", Direction::Down, "Down")] {
        v.push(entry(&format!("Resize Pane {label}"), Panes, s(&format!("Ctrl+{arrow}")), Menu(MenuAction::Pane(PaneCmd::Resize(d)))));
    }
    v.push(entry("Toggle Broadcast Input", Panes, s("Shift+P"), Menu(MenuAction::BroadcastToggle)));
    v.push(entry("Compare Pane Output", Panes, s("Shift+K"), Menu(MenuAction::CompareOutput)));

    // Tabs
    v.push(entry("New Tab", Tabs, s("Shift+T"), Menu(MenuAction::NewTab)));
    v.push(entry("Close Tab", Tabs, s("Shift+W"), Menu(MenuAction::CloseTab)));
    // Windows
    v.push(entry("New Window", Windows, s("N"), Menu(MenuAction::NewWindow)));
    v.push(entry("Close Window", Windows, s("Alt+W"), Menu(MenuAction::CloseWindow)));

    v.push(entry("Next Tab", Tabs, Some("Ctrl+Tab".into()), PaletteAction::NextTab));
    v.push(entry("Previous Tab", Tabs, Some("Ctrl+Shift+Tab".into()), PaletteAction::PrevTab));

    // Tools
    v.push(entry("Find in Terminal", Tools, s("F"), Menu(MenuAction::Find)));
    v.push(entry("Clear Buffer", Tools, s("Alt+K"), Menu(MenuAction::ClearBuffer)));
    v.push(entry("SSH Connect...", Tools, s("Shift+S"), Menu(MenuAction::SshConnect)));
    v.push(entry("Toggle Recording", Tools, s("Shift+R"), Menu(MenuAction::Recording)));
    v.push(entry("Toggle HUD", Tools, s("Shift+H"), Menu(MenuAction::HudToggle)));
    v.push(entry("Time Warp", Tools, s("Shift+Z"), Menu(MenuAction::TimeWarp)));
    v.push(entry("File Manager", Tools, s("Shift+E"), Menu(MenuAction::FileManager)));
    v.push(entry("Git Panel", Tools, s("Shift+G"), Menu(MenuAction::GitPanel)));
    v.push(entry("Docker Panel", Tools, s("Shift+O"), Menu(MenuAction::DockerPanel)));
    v.push(entry("CI/CD Panel", Tools, s("Shift+I"), Menu(MenuAction::CicdPanel)));
    v.push(entry("Network Monitor", Tools, None, Menu(MenuAction::NetworkMonitor)));
    v.push(entry("Process Tree", Tools, None, Menu(MenuAction::ProcessTree)));
    v.push(entry("System Info", Tools, None, Menu(MenuAction::SystemInfo)));
    v.push(entry("Port Dashboard", Tools, None, Menu(MenuAction::PortDashboard)));
    v.push(entry("Regex Playground", Tools, s("Shift+X"), Menu(MenuAction::RegexPlayground)));
    v.push(entry("Command Heatmap", Tools, s("Shift+Y"), Menu(MenuAction::Heatmap)));
    v.push(entry("Secret Masking", Tools, s("Shift+M"), Menu(MenuAction::SecretMask)));
    v.push(entry("Audit Log", Tools, s("Shift+U"), Menu(MenuAction::AuditLog)));
    v.push(entry("Teaching Mode", Tools, s("Shift+L"), Menu(MenuAction::TeachingMode)));

    // AI
    v.push(entry("AI Assistant", Ai, s("Shift+A"), Menu(MenuAction::AiAssistant)));
    v.push(entry("Observer Mode", Ai, s("Shift+N"), Menu(MenuAction::ObserverMode)));
    v.push(entry("Advisor Mode", Ai, None, Menu(MenuAction::AdvisorMode)));
    v.push(entry("Ask AI About This", Ai, if cfg!(target_os = "macos") { s("K") } else { None }, Menu(MenuAction::AskAboutThis)));
    // Agents (Mission Control)
    v.push(entry("Agent Mission Control", Agents, s("Shift+;"), Menu(MenuAction::AgentMissionControl)));
    v.push(entry("Next Agent Needing Attention", Agents, s("Shift+."), Menu(MenuAction::AgentNextAttention)));
    v.push(entry("New Agent...", Agents, None, Menu(MenuAction::AgentNew)));
    v.push(entry("Agent Layout: 2\u{d7}2", Agents, None, Menu(MenuAction::AgentLayout2x2)));
    {
        use crate::agents::autopilot::AutopilotCmd as A;
        v.push(entry("Agents: Toggle Autopilot", Agents, None, PaletteAction::Autopilot(A::Toggle)));
        v.push(entry("Agents: Policy Log", Agents, None, PaletteAction::Autopilot(A::Log)));
        v.push(entry("Agents: Edit Policy", Agents, None, PaletteAction::Autopilot(A::EditPolicy)));
    }
    {
        use crate::workflow::WorkflowCmd as W;
        v.push(entry("Workflow: Best of N\u{2026}", Agents, None, PaletteAction::Workflow(W::BestOfN)));
        v.push(entry("Workflow: Best of 3", Agents, None, PaletteAction::Workflow(W::Template("Best of 3".into()))));
        v.push(entry("Workflow: Write & Review", Agents, None, PaletteAction::Workflow(W::Template("Write & Review".into()))));
        v.push(entry("Workflow: Fix failing tests", Agents, None, PaletteAction::Workflow(W::Template("Fix failing tests".into()))));
        v.push(entry("Workflow: Compare Candidates", Agents, None, PaletteAction::Workflow(W::Compare)));
        v.push(entry("Workflow: Task Queue\u{2026}", Agents, None, PaletteAction::Workflow(W::Queue)));
        v.push(entry("Workflow: Stop", Agents, None, PaletteAction::Workflow(W::Stop)));
    }
    v.push(entry("Toggle Auto Fix Suggestions", Ai, None, Menu(MenuAction::AutoFixToggle)));
    v.push(entry("Toggle # Natural Language", Ai, None, Menu(MenuAction::NaturalLanguageToggle)));
    v.push(entry("AI: Select Model", Ai, None, PaletteAction::Template("model ")));
    v.push(entry("AI: Privacy Report", Ai, None, PaletteAction::PrivacyReport));

    // Browser
    let mac = |k: &str| if cfg!(target_os = "macos") { s(k) } else { None };
    v.push(entry("Toggle WebView", Browser, s("Shift+B"), Menu(MenuAction::WebView)));
    v.push(entry("Browser: Back", Browser, mac("["), Menu(MenuAction::Browser(BrowserCmd::Back))));
    v.push(entry("Browser: Forward", Browser, mac("]"), Menu(MenuAction::Browser(BrowserCmd::Forward))));
    v.push(entry("Browser: Reload", Browser, mac("R"), Menu(MenuAction::Browser(BrowserCmd::Reload))));
    v.push(entry("Browser: Focus Address Bar", Browser, mac("L"), Menu(MenuAction::Browser(BrowserCmd::FocusAddress))));
    v.push(entry("Browser: Close", Browser, mac("W"), Menu(MenuAction::Browser(BrowserCmd::Close))));

    // Effects
    let effects: [(&str, MenuAction, Option<&str>); 7] = [
        ("Effect: CRT", MenuAction::CrtEffect, Some("Ctrl+Shift+1")),
        ("Effect: Glitch", MenuAction::GlitchEffect, Some("Ctrl+Shift+2")),
        ("Effect: Neon Glow", MenuAction::NeonEffect, Some("Ctrl+Shift+3")),
        ("Effect: Matrix Rain", MenuAction::MatrixEffect, Some("Ctrl+Shift+4")),
        ("Effect: Amber", MenuAction::AmberEffect, Some("Ctrl+Shift+5")),
        ("Effect: Hologram", MenuAction::HologramEffect, Some("Ctrl+Shift+6")),
        ("Effect: Off", MenuAction::NoEffect, Some("Ctrl+Shift+0")),
    ];
    for (name, act, sc) in effects {
        v.push(entry(name, Effects, sc.map(str::to_string), Menu(act)));
    }

    // Themes
    for &name in crate::config::Config::available_themes() {
        v.push(entry(&format!("Theme: {}", titlecase(name)), Themes, None, PaletteAction::Theme(name.to_string())));
    }

    // Settings
    v.push(entry("Preferences", Settings, s(","), Menu(MenuAction::Preferences)));
    v.push(entry("Welcome Guide", Settings, s("Shift+/"), Menu(MenuAction::Welcome)));
    v.push(entry("Toggle Full Screen", Settings, Some(format!("Ctrl+{m}+F")), Menu(MenuAction::ToggleFullScreen)));
    v.push(entry("Zoom In", Settings, s("="), Menu(MenuAction::ZoomIn)));
    v.push(entry("Zoom Out", Settings, Some("Ctrl+-".into()), Menu(MenuAction::ZoomOut)));
    v.push(entry("Reset Zoom", Settings, s("0"), Menu(MenuAction::ZoomReset)));
    v.push(entry("UI Gallery", Settings, None, Menu(MenuAction::UiGallery)));
    v.push(entry("MCP Activity", Tools, None, PaletteAction::McpActivity));
    v.push(entry("Review: Changes Since Checkpoint", Tools, s("Shift+J"), Menu(MenuAction::ReviewChanges)));
    v.push(entry("Review: Mark Checkpoint", Tools, None, Menu(MenuAction::ReviewMark)));

    // Parameterised commands (Tab / Enter completes the keyword)
    v.push(entry("theme <name>", Themes, None, PaletteAction::Template("theme ")));
    v.push(entry("font <size>", Settings, None, PaletteAction::Template("font ")));
    v.push(entry("open <url>", Browser, None, PaletteAction::Template("open ")));
    v.push(entry("ssh <alias>", Tools, None, PaletteAction::Template("ssh ")));
    v.push(entry("cd <path>", Tools, None, PaletteAction::Template("cd ")));
    v.push(entry("> shell command", Tools, None, PaletteAction::Template("> ")));
    v.push(entry("? ask AI", Ai, None, PaletteAction::Template("? ")));
    v.push(entry("agent <name>", Agents, None, PaletteAction::Template("agent ")));
    v
}

/// "tokyo-night" -> "Tokyo Night"
fn titlecase(s: &str) -> String {
    s.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ───────────────────────────── Tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn pal() -> CommandPalette {
        CommandPalette::with_parts(History::default(), None)
    }

    fn open(p: &mut CommandPalette) {
        p.open(PaletteContext {
            tabs: vec!["zsh".into(), "build".into()],
            active_tab: 0,
            ssh_hosts: vec![
                SshHostInfo { alias: "prod".into(), detail: "root@prod.example.com:22".into() },
                SshHostInfo { alias: "staging".into(), detail: "dev@stg.example.com:2222".into() },
            ],
            font_size: 15.0,
            agents: vec![crate::agents::AgentKind::ClaudeCode, crate::agents::AgentKind::Codex],
            in_git_repo: true,
            workflows: vec![("Refactor with review".into(), "write & review \u{b7} workflows.toml".into())],
            models: vec![
                crate::ai::local::picker::ModelOption {
                    label: "qwen2.5-coder:7b".into(),
                    detail: "Ollama - local".into(),
                    active: true,
                    choice: crate::ai::local::picker::ModelChoice { provider: "ollama".into(), model: "qwen2.5-coder:7b".into(), api_url: "http://127.0.0.1:11434".into(), local: true },
                },
                crate::ai::local::picker::ModelOption {
                    label: "gpt-4o-mini".into(),
                    detail: "api.openai.com - cloud".into(),
                    active: false,
                    choice: crate::ai::local::picker::ModelChoice { provider: "openai".into(), model: "gpt-4o-mini".into(), api_url: "https://api.openai.com".into(), local: false },
                },
            ],
        });
    }

    #[test]
    fn select_model_lists_local_and_cloud_and_dispatches() {
        let mut p = CommandPalette::with_parts(History::default(), None);
        open(&mut p);
        // The catalog entry completes to model mode.
        type_str(&mut p, "ai: select");
        assert!(matches!(p.results[0].action, PaletteAction::Template("model ")), "{:?}", names(&p));
        p.set_query("model ");
        assert!(matches!(p.param, Some((ParamKind::Model, _))));
        assert_eq!(names(&p), ["qwen2.5-coder:7b", "gpt-4o-mini"]);
        assert_eq!(p.results[0].shortcut.as_deref(), Some("Ollama - local - current"));
        p.set_query("model gpt");
        assert_eq!(names(&p), ["gpt-4o-mini"]);
        let a = p.handle_key(PaletteKey::Enter);
        assert!(matches!(a, Some(PaletteAction::SelectModel(ref c)) if c.model == "gpt-4o-mini" && !c.local), "{a:?}");
    }

    #[test]
    fn privacy_report_and_direct_model_entries_are_in_the_catalog() {
        let mut p = CommandPalette::with_parts(History::default(), None);
        open(&mut p);
        type_str(&mut p, "privacy report");
        assert!(matches!(p.results[0].action, PaletteAction::PrivacyReport));
        p.set_query("ai model qwen");
        assert!(matches!(&p.results[0].action, PaletteAction::SelectModel(c) if c.model == "qwen2.5-coder:7b"));
    }

    fn type_str(p: &mut CommandPalette, s: &str) {
        for c in s.chars() {
            p.handle_key(PaletteKey::Char(c));
        }
    }

    fn names(p: &CommandPalette) -> Vec<&str> {
        p.results.iter().map(|h| h.name.as_str()).collect()
    }

    // ── fuzzy ──

    #[test]
    fn fuzzy_no_match_and_empty() {
        assert!(fuzzy_match("xyz", "New Tab").is_none());
        assert!(fuzzy_match("tn", "New Tab").is_none()); // order matters
        assert_eq!(fuzzy_match("", "anything").unwrap().indices.len(), 0);
        assert!(fuzzy_match("abcd", "abc").is_none());
    }

    #[test]
    fn fuzzy_is_case_insensitive_and_ignores_query_spaces() {
        assert!(fuzzy_match("NEWTAB", "New Tab").is_some());
        assert!(fuzzy_match("new tab", "New Tab").is_some());
    }

    #[test]
    fn fuzzy_prefix_beats_mid_word() {
        let a = fuzzy_match("set", "Settings").unwrap().score;
        let b = fuzzy_match("set", "Reset Zoom").unwrap().score;
        assert!(a > b, "{a} <= {b}");
    }

    #[test]
    fn fuzzy_consecutive_beats_scattered() {
        let a = fuzzy_match("term", "Terminal").unwrap().score;
        let b = fuzzy_match("term", "Toggle Eternal Realm").unwrap().score;
        assert!(a > b, "{a} <= {b}");
    }

    #[test]
    fn fuzzy_word_start_beats_inside() {
        let a = fuzzy_match("nt", "New Tab").unwrap().score;
        let b = fuzzy_match("nt", "Network Monitor").unwrap().score;
        assert!(a > b, "{a} <= {b}");
    }

    #[test]
    fn fuzzy_camel_case_boundary() {
        let a = fuzzy_match("sb", "SplitBorder").unwrap().score;
        let b = fuzzy_match("sb", "Subtle").unwrap().score; // no boundary for b
        assert!(a > b, "{a} <= {b}");
    }

    #[test]
    fn fuzzy_gap_penalty_prefers_tighter() {
        let a = fuzzy_match("ab", "a-b").unwrap().score;
        let b = fuzzy_match("ab", "a----b").unwrap().score;
        assert!(a > b);
    }

    #[test]
    fn fuzzy_shorter_wins_ties() {
        let a = fuzzy_match("tab", "Tab").unwrap().score;
        let b = fuzzy_match("tab", "Tab Something Longer").unwrap().score;
        assert!(a > b);
    }

    #[test]
    fn fuzzy_highlight_indices() {
        assert_eq!(fuzzy_match("sr", "Split Right").unwrap().indices, vec![0, 6]);
        assert_eq!(fuzzy_match("split", "Split Right").unwrap().indices, vec![0, 1, 2, 3, 4]);
        // picks the word-start alignment, not the first 'n'
        assert_eq!(fuzzy_match("nt", "New Tab").unwrap().indices, vec![0, 4]);
        assert_eq!(fuzzy_match("ctrl", "Effect: CRT Lamp").unwrap().indices.len(), 4);
    }

    #[test]
    fn ranking_in_palette() {
        let mut p = pal();
        open(&mut p);
        type_str(&mut p, "split");
        let n = names(&p);
        assert!(n[0].starts_with("Split"), "{n:?}");
        assert!(p.results[0].matches == vec![0, 1, 2, 3, 4]);
        p.set_query("zzzz");
        assert!(p.results.is_empty());
    }

    // ── frecency ──

    #[test]
    fn history_roundtrip() {
        let mut h = History::default();
        h.record("Toggle HUD", 1000);
        h.record("Toggle HUD", 2000);
        h.record("Theme: Nord", 3000);
        let h2 = History::parse(&h.serialize());
        assert_eq!(h2.map.get("Toggle HUD"), Some(&(2, 2000)));
        assert_eq!(h2.map.get("Theme: Nord"), Some(&(1, 3000)));
        let h3 = History::parse("garbage\n1\tx\tbad\n3\t10\tOK Id With Spaces\n");
        assert_eq!(h3.map.len(), 1);
        assert!(h3.map.contains_key("OK Id With Spaces"));
    }

    #[test]
    fn frecency_prefers_recent_and_frequent() {
        let now = 10_000_000;
        let mut h = History::default();
        for _ in 0..5 {
            h.record("often-old", now - 30 * 86_400);
        }
        h.record("once-now", now);
        for _ in 0..5 {
            h.record("often-now", now);
        }
        let (a, b, c) = (h.frecency("often-old", now), h.frecency("once-now", now), h.frecency("often-now", now));
        assert!(c > b && b > a, "{a} {b} {c}");
        assert_eq!(h.frecency("never", now), 0.0);
    }

    #[test]
    fn history_is_bounded() {
        let mut h = History::default();
        for i in 0..(HISTORY_MAX + 20) {
            h.record(&format!("id{i}"), i as u64);
        }
        assert_eq!(h.map.len(), HISTORY_MAX);
        assert!(!h.map.contains_key("id0"));
    }

    #[test]
    fn empty_query_lists_recent_first() {
        let now = unix_now();
        let mut h = History::default();
        h.record("Toggle HUD", now);
        h.record("Toggle HUD", now);
        h.record("Zoom In", now);
        let mut p = CommandPalette::with_parts(h, None);
        open(&mut p);
        assert_eq!(p.results[0].name, "Toggle HUD");
        assert_eq!(p.results[0].section, Section::Recent);
        assert_eq!(p.results[1].name, "Zoom In");
        assert_eq!(p.results[2].section, Section::Cat(Category::Panes));
        // recent entries aren't repeated in their own section
        assert_eq!(p.results.iter().filter(|h| h.name == "Toggle HUD").count(), 1);
        // headers: Recent, then each category
        assert!(matches!(p.rows[0], Row::Header(Section::Recent)));
        let headers = p.rows.iter().filter(|r| matches!(r, Row::Header(_))).count();
        assert_eq!(headers, 1 + Category::ORDER.len());
    }

    #[test]
    fn frecency_blends_into_search() {
        let now = unix_now();
        let mut h = History::default();
        for _ in 0..6 {
            h.record("Network Monitor", now);
        }
        let mut p = CommandPalette::with_parts(h, None);
        open(&mut p);
        // "n" matches a lot; frequently used entries float up
        type_str(&mut p, "ne");
        let n = names(&p);
        let pos_net = n.iter().position(|x| *x == "Network Monitor").unwrap();
        let pos_new = n.iter().position(|x| *x == "New Tab").unwrap();
        assert!(pos_net < pos_new, "{n:?}");
        // without history, New Tab (word-start prefix) beats Network Monitor
        let mut q = pal();
        open(&mut q);
        type_str(&mut q, "ne");
        let n = names(&q);
        assert!(n.iter().position(|x| *x == "New Tab") < n.iter().position(|x| *x == "Network Monitor"));
    }

    #[test]
    fn running_an_item_records_it() {
        let mut p = pal();
        open(&mut p);
        type_str(&mut p, "toggle hud");
        let act = p.handle_key(PaletteKey::Enter);
        assert!(matches!(act, Some(PaletteAction::Menu(MenuAction::HudToggle))));
        assert!(!p.visible);
        assert!(p.history.frecency("Toggle HUD", unix_now()) > 0.0);
    }

    // ── parameter parsing ──

    #[test]
    fn parse_params() {
        assert_eq!(parse_query("theme "), Parsed::Param { kind: ParamKind::Theme, arg: "" });
        assert_eq!(parse_query("Theme  nord "), Parsed::Param { kind: ParamKind::Theme, arg: "nord" });
        assert_eq!(parse_query("font 16"), Parsed::Param { kind: ParamKind::Font, arg: "16" });
        assert_eq!(parse_query("open https://a.b/c d"), Parsed::Param { kind: ParamKind::Open, arg: "https://a.b/c d" });
        assert_eq!(parse_query("ssh prod"), Parsed::Param { kind: ParamKind::Ssh, arg: "prod" });
        assert_eq!(parse_query("cd ~/work"), Parsed::Param { kind: ParamKind::Cd, arg: "~/work" });
        assert_eq!(parse_query("> ls -la"), Parsed::Param { kind: ParamKind::Shell, arg: "ls -la" });
        assert_eq!(parse_query(">ls"), Parsed::Param { kind: ParamKind::Shell, arg: "ls" });
        assert_eq!(parse_query("? why is the sky blue"), Parsed::Param { kind: ParamKind::Ask, arg: "why is the sky blue" });
        // no trailing whitespace yet -> still a normal search
        assert_eq!(parse_query("theme"), Parsed::Plain("theme"));
        assert_eq!(parse_query("themes nord"), Parsed::Plain("themes nord"));
        assert_eq!(parse_query("split right"), Parsed::Plain("split right"));
    }

    #[test]
    fn font_size_parsing() {
        assert_eq!(parse_font_size("16"), Some(16.0));
        assert_eq!(parse_font_size(" 16.5 "), Some(16.5));
        assert_eq!(parse_font_size("14px"), Some(14.0));
        assert_eq!(parse_font_size("abc"), None);
        assert_eq!(parse_font_size("-3"), None);
        assert_eq!(parse_font_size(""), None);
    }

    #[test]
    fn ssh_spec_parsing() {
        assert_eq!(parse_ssh_spec("host"), Some((None, "host".into(), 22)));
        assert_eq!(parse_ssh_spec("me@host:2200"), Some((Some("me".into()), "host".into(), 2200)));
        assert_eq!(parse_ssh_spec("@host"), None);
        assert_eq!(parse_ssh_spec("host:port"), None);
        assert_eq!(parse_ssh_spec("a b"), None);
    }

    #[test]
    fn cd_quoting() {
        assert_eq!(shell_quote_path("/tmp/x"), "/tmp/x");
        assert_eq!(shell_quote_path("~"), "~");
        assert_eq!(shell_quote_path("~/work"), "~/work");
        assert_eq!(shell_quote_path("-"), "-");
        assert_eq!(shell_quote_path("my dir"), "'my dir'");
        assert_eq!(shell_quote_path("~/my dir"), "~/'my dir'");
        assert_eq!(shell_quote_path("it's"), "'it'\\''s'");
        assert_eq!(shell_quote_path("-rf"), "'-rf'");
        assert_eq!(shell_quote_path("a;rm -rf /"), "'a;rm -rf /'");
    }

    // ── palette flows ──

    #[test]
    fn theme_mode_lists_themes_and_previews() {
        let mut p = pal();
        open(&mut p);
        assert!(p.take_preview().is_none());
        type_str(&mut p, "theme ");
        assert_eq!(p.results.len(), crate::config::Config::available_themes().len());
        let first = match &p.results[0].action {
            PaletteAction::Theme(n) => n.clone(),
            a => panic!("{a:?}"),
        };
        assert_eq!(p.take_preview(), Some(Preview::Theme(first)));
        p.handle_key(PaletteKey::Down);
        let second = match &p.results[1].action {
            PaletteAction::Theme(n) => n.clone(),
            a => panic!("{a:?}"),
        };
        assert_eq!(p.take_preview(), Some(Preview::Theme(second)));
        // filtering narrows + highlights
        type_str(&mut p, "nord");
        assert_eq!(p.results[0].name, "Nord");
        assert_eq!(p.results[0].matches, vec![0, 1, 2, 3]);
    }

    #[test]
    fn escape_exits_param_mode_then_closes_and_restores_theme() {
        let mut p = pal();
        open(&mut p);
        type_str(&mut p, "theme ");
        p.take_preview();
        p.handle_key(PaletteKey::Escape);
        assert!(p.visible);
        assert_eq!(p.query, "");
        assert_eq!(p.take_preview(), Some(Preview::Restore));
        p.handle_key(PaletteKey::Escape);
        assert!(!p.visible);
    }

    #[test]
    fn close_while_previewing_restores() {
        let mut p = pal();
        open(&mut p);
        type_str(&mut p, "theme ");
        p.take_preview();
        p.close();
        assert_eq!(p.take_preview(), Some(Preview::Restore));
    }

    #[test]
    fn enter_on_theme_commits_without_restore() {
        let mut p = pal();
        open(&mut p);
        type_str(&mut p, "theme dra");
        p.take_preview();
        let a = p.handle_key(PaletteKey::Enter);
        assert!(matches!(a, Some(PaletteAction::Theme(ref n)) if n == "dracula"));
        assert!(!p.visible);
        assert!(p.take_preview().is_none());
    }

    #[test]
    fn param_actions() {
        let mut p = pal();
        open(&mut p);
        p.set_query("font 40");
        assert!(matches!(p.results[0].action, PaletteAction::FontSize(f) if f == 32.0));
        p.set_query("font 17");
        assert!(matches!(p.results[0].action, PaletteAction::FontSize(f) if f == 17.0));
        p.set_query("font ");
        assert!(p.results.len() > 3);
        p.set_query("font nope");
        assert!(p.results.is_empty());
        p.set_query("open rust-lang.org");
        assert!(matches!(&p.results[0].action, PaletteAction::OpenUrl(u) if u == "rust-lang.org"));
        p.set_query("open ");
        assert!(p.results.is_empty());
        p.set_query("> echo hi");
        assert!(matches!(&p.results[0].action, PaletteAction::Shell(c) if c == "echo hi"));
        p.set_query("cd /tmp");
        assert!(matches!(&p.results[0].action, PaletteAction::Cd(c) if c == "/tmp"));
        p.set_query("? what is a pty");
        assert!(matches!(&p.results[0].action, PaletteAction::AskAi(q) if q == "what is a pty"));
    }

    #[test]
    fn ssh_mode_lists_saved_hosts_and_adhoc() {
        let mut p = pal();
        open(&mut p);
        p.set_query("ssh ");
        assert_eq!(names(&p), vec!["prod", "staging"]);
        p.set_query("ssh stg");
        assert_eq!(names(&p), vec!["staging"]);
        p.set_query("ssh me@box:2222");
        assert_eq!(p.results.len(), 1);
        assert!(matches!(&p.results[0].action, PaletteAction::Ssh(s) if s == "me@box:2222"));
        p.set_query("ssh prod");
        assert_eq!(p.results.len(), 1); // exact alias: no duplicate ad-hoc row
        p.set_query("ssh brandnew");
        assert_eq!(names(&p), vec!["Connect to brandnew"]); // nothing saved matches: offer ad-hoc
    }

    #[test]
    fn tab_completes_param_keyword() {
        let mut p = pal();
        open(&mut p);
        type_str(&mut p, "the");
        p.handle_key(PaletteKey::Tab);
        // first result for "the" is a Theme: entry or the template; either way Tab
        // on a non-template falls back to keyword completion.
        assert!(p.query == "theme " || p.query == "the");
        p.set_query("fo");
        p.handle_key(PaletteKey::Tab);
        assert_eq!(p.query, "font ");
        assert_eq!(p.cursor, 5);
        // Tab inside theme mode fills the highlighted theme name
        p.set_query("theme ");
        p.handle_key(PaletteKey::Down);
        let want = p.results[1].name.clone();
        p.handle_key(PaletteKey::Tab);
        assert_eq!(p.query, format!("theme {want}"));
    }

    #[test]
    fn enter_on_template_completes_instead_of_running() {
        let mut p = pal();
        open(&mut p);
        p.set_query("cd <path");
        // "cd <path" is not a param (kw must be followed by space AND be exact 'cd'); here it is.
        assert!(matches!(p.param, Some((ParamKind::Cd, _))));
        p.set_query("open<url");
        let i = p.results.iter().position(|h| matches!(h.action, PaletteAction::Template("open "))).unwrap();
        p.selected = i;
        assert!(p.handle_key(PaletteKey::Enter).is_none());
        assert_eq!(p.query, "open ");
        assert!(p.visible);
    }

    #[test]
    fn keep_open_only_for_sensible_actions() {
        let mut p = pal();
        open(&mut p);
        type_str(&mut p, "effect: crt");
        let a = p.handle_key(PaletteKey::EnterKeepOpen);
        assert!(matches!(a, Some(PaletteAction::Menu(MenuAction::CrtEffect))));
        assert!(p.visible);
        assert_eq!(p.results[p.selected].name, "Effect: CRT");
        p.set_query("preferences");
        let a = p.handle_key(PaletteKey::EnterKeepOpen);
        assert!(matches!(a, Some(PaletteAction::Menu(MenuAction::Preferences))));
        assert!(!p.visible);
    }

    #[test]
    fn dynamic_tabs_and_hosts_are_searchable() {
        let mut p = pal();
        open(&mut p);
        type_str(&mut p, "tab 2");
        let hit = &p.results[0];
        assert!(matches!(hit.action, PaletteAction::SwitchTab(1)), "{:?}", names(&p));
        p.set_query("ssh: stag");
        assert!(matches!(&p.results[0].action, PaletteAction::Ssh(a) if a == "staging"));
    }

    // ── editing ──

    #[test]
    fn cursor_editing() {
        let mut p = pal();
        open(&mut p);
        type_str(&mut p, "helo");
        p.handle_key(PaletteKey::Left);
        p.handle_key(PaletteKey::Char('l'));
        assert_eq!(p.query, "hello");
        assert_eq!(p.cursor, 4);
        p.handle_key(PaletteKey::Home);
        p.handle_key(PaletteKey::Delete);
        assert_eq!(p.query, "ello");
        p.handle_key(PaletteKey::End);
        p.handle_key(PaletteKey::Backspace);
        assert_eq!(p.query, "ell");
        type_str(&mut p, " world foo");
        p.handle_key(PaletteKey::DeleteWord);
        assert_eq!(p.query, "ell world ");
        p.handle_key(PaletteKey::WordLeft);
        assert_eq!(p.cursor, 4);
        p.handle_key(PaletteKey::WordRight);
        assert_eq!(p.cursor, 9);
        p.handle_key(PaletteKey::DeleteToStart);
        assert_eq!(p.query, " ");
        // multibyte / IME commit
        p.set_query("");
        type_str(&mut p, "终端");
        p.handle_key(PaletteKey::Left);
        p.handle_key(PaletteKey::Backspace);
        assert_eq!(p.query, "端");
        // control chars are ignored
        p.handle_key(PaletteKey::Char('\u{7f}'));
        assert_eq!(p.query, "端");
    }

    #[test]
    fn navigation_wraps_and_pages() {
        let mut p = pal();
        open(&mut p);
        let n = p.results.len();
        p.handle_key(PaletteKey::Up);
        assert_eq!(p.selected, n - 1);
        p.handle_key(PaletteKey::Down);
        assert_eq!(p.selected, 0);
        p.handle_key(PaletteKey::PageDown);
        assert_eq!(p.selected, MAX_VISIBLE);
        p.handle_key(PaletteKey::PageUp);
        p.handle_key(PaletteKey::PageUp);
        assert_eq!(p.selected, 0);
        // scrolling keeps the selection in view
        for _ in 0..30 {
            p.handle_key(PaletteKey::Down);
        }
        let r = p.row_of_selected().unwrap();
        assert!(r >= p.scroll.get() && r < p.scroll.get() + p.view_rows.get());
    }

    #[test]
    fn mouse_hover_click_and_outside() {
        let mut p = pal();
        open(&mut p);
        *p.layout.borrow_mut() = Layout { panel: (100, 50, 400, 300), rows: vec![(100, 20, 0), (120, 20, 1), (140, 20, 2)] };
        assert!(p.mouse_move(150, 125));
        assert_eq!(p.selected, 1);
        assert!(!p.mouse_move(150, 125));
        let a = p.mouse_click(150, 145, false);
        assert!(a.is_some());
        assert!(!p.visible);
        open(&mut p);
        *p.layout.borrow_mut() = Layout { panel: (100, 50, 400, 300), rows: vec![] };
        assert!(p.mouse_click(5, 5, false).is_none());
        assert!(!p.visible);
    }

    #[test]
    fn wheel_scrolls_view() {
        let mut p = pal();
        open(&mut p);
        p.scroll_lines(-3);
        assert_eq!(p.scroll.get(), 3);
        p.scroll_lines(10);
        assert_eq!(p.scroll.get(), 0);
        p.scroll_lines(-10_000);
        assert_eq!(p.scroll.get(), p.rows.len() - p.view_rows.get());
    }

    // ── catalog coverage ──

    /// Compile-time guard: adding a `MenuAction` variant breaks this match,
    /// reminding you to add a palette entry (and list it below).
    #[allow(dead_code)]
    fn exhaustive(m: MenuAction) {
        match m {
            MenuAction::NewTab | MenuAction::CloseTab | MenuAction::NewWindow | MenuAction::CloseWindow | MenuAction::SshConnect | MenuAction::ToggleFullScreen
            | MenuAction::ZoomIn | MenuAction::ZoomOut | MenuAction::ZoomReset | MenuAction::SplitH
            | MenuAction::SplitV | MenuAction::Recording | MenuAction::CrtEffect | MenuAction::GlitchEffect
            | MenuAction::NeonEffect | MenuAction::MatrixEffect | MenuAction::AmberEffect
            | MenuAction::HologramEffect
            | MenuAction::NoEffect | MenuAction::Preferences | MenuAction::Welcome | MenuAction::WebView
            | MenuAction::Browser(_) | MenuAction::FileManager | MenuAction::GitPanel
            | MenuAction::DockerPanel | MenuAction::CicdPanel | MenuAction::NetworkMonitor
            | MenuAction::ProcessTree | MenuAction::SystemInfo | MenuAction::PortDashboard
            | MenuAction::RegexPlayground | MenuAction::Heatmap | MenuAction::SecretMask
            | MenuAction::AuditLog | MenuAction::TeachingMode | MenuAction::AiAssistant
            | MenuAction::ObserverMode | MenuAction::AdvisorMode | MenuAction::AskAboutThis
            | MenuAction::AutoFixToggle | MenuAction::NaturalLanguageToggle | MenuAction::TimeWarp
            | MenuAction::HudToggle | MenuAction::BroadcastToggle | MenuAction::Find | MenuAction::ClearBuffer
            | MenuAction::CompareOutput | MenuAction::UiGallery | MenuAction::ReviewChanges
            | MenuAction::ReviewMark | MenuAction::Pane(_)
            | MenuAction::AgentMissionControl | MenuAction::AgentNextAttention | MenuAction::AgentNew
            | MenuAction::AgentLayout2x2 => {}
        }
    }

    #[test]
    fn every_menu_action_has_a_palette_entry() {
        use MenuAction as M;
        let mut expected = vec![
            M::NewTab, M::CloseTab, M::NewWindow, M::CloseWindow, M::SshConnect, M::ToggleFullScreen, M::ZoomIn, M::ZoomOut, M::ZoomReset,
            M::SplitH, M::SplitV, M::Recording, M::CrtEffect, M::GlitchEffect, M::NeonEffect, M::MatrixEffect,
            M::AmberEffect, M::HologramEffect, M::NoEffect, M::Preferences, M::Welcome, M::WebView, M::FileManager,
            M::GitPanel, M::DockerPanel, M::CicdPanel, M::NetworkMonitor, M::ProcessTree, M::SystemInfo,
            M::PortDashboard, M::RegexPlayground, M::Heatmap, M::SecretMask, M::AuditLog, M::TeachingMode,
            M::AiAssistant, M::ObserverMode, M::AdvisorMode, M::AskAboutThis, M::AutoFixToggle,
            M::NaturalLanguageToggle, M::TimeWarp, M::HudToggle, M::BroadcastToggle,
            M::Find, M::ClearBuffer, M::CompareOutput, M::UiGallery, M::ReviewChanges, M::ReviewMark,
            M::AgentMissionControl, M::AgentNextAttention, M::AgentNew, M::AgentLayout2x2,
        ];
        for c in [BrowserCmd::Back, BrowserCmd::Forward, BrowserCmd::Reload, BrowserCmd::FocusAddress, BrowserCmd::Close] {
            expected.push(M::Browser(c));
        }
        let dirs = [Direction::Left, Direction::Right, Direction::Up, Direction::Down];
        for c in [PaneCmd::ClosePane, PaneCmd::Zoom, PaneCmd::Equalize, PaneCmd::FocusNext, PaneCmd::FocusPrev] {
            expected.push(M::Pane(c));
        }
        for d in dirs {
            expected.push(M::Pane(PaneCmd::Focus(d)));
            expected.push(M::Pane(PaneCmd::Swap(d)));
            expected.push(M::Pane(PaneCmd::Resize(d)));
        }
        let have: Vec<String> = catalog()
            .iter()
            .filter_map(|it| match &it.action {
                PaletteAction::Menu(m) => Some(format!("{m:?}")),
                _ => None,
            })
            .collect();
        for m in expected {
            assert!(have.contains(&format!("{m:?}")), "missing palette entry for {m:?}");
        }
    }

    #[test]
    fn window_entries_are_searchable_and_dispatch_window_actions() {
        let mut p = pal();
        open(&mut p);
        p.set_query("new window");
        assert_eq!(names(&p).first().copied(), Some("New Window"));
        let act = p.handle_key(PaletteKey::Enter);
        assert!(matches!(act, Some(PaletteAction::Menu(MenuAction::NewWindow))), "{act:?}");
        open(&mut p);
        p.set_query("close window");
        assert_eq!(names(&p).first().copied(), Some("Close Window"));
        let act = p.handle_key(PaletteKey::Enter);
        assert!(matches!(act, Some(PaletteAction::Menu(MenuAction::CloseWindow))), "{act:?}");
    }

    #[test]
    fn workflow_entries_are_searchable() {
        use crate::workflow::WorkflowCmd;
        let mut p = pal();
        open(&mut p);
        p.set_query("workflow best");
        let n = names(&p);
        assert!(n.contains(&"Workflow: Best of N\u{2026}"), "{n:?}");
        assert!(n.contains(&"Workflow: Best of 3"), "{n:?}");
        p.set_query("workflow write");
        assert!(names(&p).contains(&"Workflow: Write & Review"));
        p.set_query("workflow fix");
        assert!(names(&p).contains(&"Workflow: Fix failing tests"));
        // The user's own template from workflows.toml.
        p.set_query("workflow refactor");
        let n = names(&p);
        assert_eq!(n.first().copied(), Some("Workflow: Refactor with review"), "{n:?}");
        let act = p.handle_key(PaletteKey::Enter);
        assert!(matches!(act, Some(PaletteAction::Workflow(WorkflowCmd::Template(ref t))) if t == "Refactor with review"), "{act:?}");
        open(&mut p);
        p.set_query("workflow compare");
        assert!(names(&p).contains(&"Workflow: Compare Candidates"));
        p.set_query("workflow stop");
        assert!(names(&p).contains(&"Workflow: Stop"));
    }

    #[test]
    fn agent_mode_lists_installed_agents_worktrees_and_layouts() {
        use crate::agents::runtime::Launch;
        use crate::agents::AgentKind;
        let mut p = pal();
        open(&mut p);
        p.set_query("agent ");
        let n = names(&p);
        assert!(n.contains(&"Claude Code: current directory"), "{n:?}");
        assert!(n.contains(&"Claude Code: new git worktree"));
        assert!(n.contains(&"Codex: current directory"));
        assert!(n.iter().any(|x| x.starts_with("Layout 2\u{d7}2")));
        p.set_query("agent codex wor");
        assert_eq!(names(&p).first().copied(), Some("Codex: new git worktree"));
        let act = p.handle_key(PaletteKey::Enter);
        assert!(matches!(act, Some(PaletteAction::Agent(Launch::Worktree(AgentKind::Codex)))));
        // Outside a repo only "current directory" is offered.
        let mut p = pal();
        p.open(PaletteContext { agents: vec![AgentKind::Aider], in_git_repo: false, ..Default::default() });
        p.set_query("agent ");
        assert!(!names(&p).iter().any(|x| x.contains("worktree")));
        // Nothing installed: an explanatory row instead of an empty list.
        p.open(PaletteContext::default());
        p.set_query("agent ");
        assert_eq!(names(&p), vec!["No agent CLI found on PATH"]);
    }

    #[test]
    fn catalog_ids_are_unique_and_themes_complete() {
        let items = catalog();
        let mut ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        ids.sort();
        let n = ids.len();
        ids.dedup();
        assert_eq!(n, ids.len(), "duplicate palette ids");
        for t in crate::config::Config::available_themes() {
            assert!(items.iter().any(|i| matches!(&i.action, PaletteAction::Theme(n) if n == t)));
        }
    }

    /// Visual QA: `cargo test --bin rift palette_snapshot -- --ignored` writes PPMs to $RIFT_SNAP_DIR.
    #[test]
    #[ignore]
    fn palette_snapshot() {
        let dir = std::env::var("RIFT_SNAP_DIR").unwrap_or_else(|_| ".".into());
        let (w, h) = (1100usize, 700usize);
        let mut font = FontManager::new(&crate::config::find_font_path(), 28.0);
        let theme = crate::config::Config::theme_by_name("tokyo-night").unwrap();
        let now = unix_now();
        let mut hist = History::default();
        hist.record("Toggle HUD", now);
        hist.record("Theme: Nord", now);
        hist.record("Theme: Nord", now);
        let mut p = CommandPalette::with_parts(hist, None);
        open(&mut p);
        let shots: [(&str, &str); 4] = [("empty", ""), ("fuzzy", "splr"), ("theme", "theme "), ("ssh", "ssh pr")];
        for (name, q) in shots {
            p.set_query(q);
            if name == "theme" {
                p.handle_key(PaletteKey::Down);
            }
            let mut buf = vec![0x00202030u32; w * h];
            p.render(&mut buf, w, h, &mut font, &theme);
            let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
            for px in &buf {
                out.extend_from_slice(&[(px >> 16) as u8, (px >> 8) as u8, *px as u8]);
            }
            std::fs::write(format!("{dir}/palette_{name}.ppm"), out).unwrap();
        }
    }
}
