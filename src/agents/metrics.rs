//! At-a-glance numbers for an agent card: model, tokens, cost, context use and
//! the rate-limit reset, read from the agent's own UI (status lines, footers,
//! spinner lines), plus the same-worktree collision check.
//!
//! The parsers live in one table ([`RULES`]); each rule looks at one screen
//! line and fills the fields it knows. Lines are visited bottom-up and the
//! first hit per field wins, so the footer beats a banner at the top.
//! Fixtures of real status lines are in the tests; add a rule + a fixture for a
//! new agent.

use std::collections::HashMap;

use super::{AgentKind, AgentSession};
use AgentKind::{Aider, ClaudeCode, Codex, Gemini, OpenCode};

/// Numbers read from one agent's screen. Every field is optional: agents show
/// different things.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metrics {
    pub model: Option<String>,
    pub tokens: Option<u64>,
    /// Dollars.
    pub cost: Option<f64>,
    /// 0..=100, how much of the context window is used.
    pub context_used: Option<u8>,
    /// Time until the usage window resets, e.g. "4h 37m".
    pub reset_in: Option<String>,
}

impl Metrics {
    pub fn is_empty(&self) -> bool {
        *self == Metrics::default()
    }
}

/// One status-line parser.
pub struct Rule {
    pub name: &'static str,
    /// Agents it is known from (empty = any).
    pub kinds: &'static [AgentKind],
    pub apply: fn(&str, &mut Metrics),
}

pub const RULES: &[Rule] = &[
    Rule { name: "model", kinds: &[], apply: rule_model },
    Rule { name: "tokens", kinds: &[], apply: rule_tokens },
    Rule { name: "codex-token-usage", kinds: &[Codex], apply: rule_codex_usage },
    Rule { name: "aider-tokens-cost", kinds: &[Aider], apply: rule_aider },
    Rule { name: "cost", kinds: &[], apply: rule_cost },
    Rule { name: "context", kinds: &[ClaudeCode, Codex, Gemini, OpenCode], apply: rule_context },
    Rule { name: "reset", kinds: &[], apply: rule_reset },
];

/// Rows (from the bottom) that are searched.
const SCAN_ROWS: usize = 40;

/// Parse the metrics visible on `lines` (screen rows, top to bottom).
pub fn parse_screen(kind: Option<AgentKind>, lines: &[String]) -> Metrics {
    let mut m = Metrics::default();
    let rows = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).rev().take(SCAN_ROWS);
    for line in rows {
        for rule in RULES {
            if !rule.kinds.is_empty() && kind.is_some_and(|k| !rule.kinds.contains(&k)) {
                continue;
            }
            let mut found = Metrics::default();
            (rule.apply)(line, &mut found);
            fill(&mut m, found);
        }
    }
    m
}

/// Copy fields of `from` into the empty fields of `into`.
fn fill(into: &mut Metrics, from: Metrics) {
    macro_rules! take {
        ($f:ident) => {
            if into.$f.is_none() {
                into.$f = from.$f;
            }
        };
    }
    take!(model);
    take!(tokens);
    take!(cost);
    take!(context_used);
    take!(reset_in);
}

/// New readings win over old ones field by field; fields the screen does not
/// show right now keep their last known value (a prompt box can hide the footer).
pub fn merge(old: &Metrics, new: &Metrics) -> Metrics {
    Metrics {
        model: new.model.clone().or_else(|| old.model.clone()),
        tokens: new.tokens.or(old.tokens),
        cost: new.cost.or(old.cost),
        context_used: new.context_used.or(old.context_used),
        reset_in: new.reset_in.clone().or_else(|| old.reset_in.clone()),
    }
}

// ───────────────────────────── number helpers ─────────────────────────────

/// "57929", "57,929", "3.4k", "1.2M", "↑3.4k" -> count.
pub fn parse_count(w: &str) -> Option<u64> {
    let w = w.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == ','));
    let (num, mult) = match w.chars().last()? {
        'k' | 'K' => (&w[..w.len() - 1], 1_000.0),
        'm' | 'M' => (&w[..w.len() - 1], 1_000_000.0),
        _ => (w, 1.0),
    };
    let num: String = num.chars().filter(|c| *c != ',').collect();
    if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit() || c == '.') || !num.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let v: f64 = num.parse().ok()?;
    Some((v * mult).round() as u64)
}

fn words(line: &str) -> Vec<&str> {
    line.split(|c: char| c.is_whitespace() || matches!(c, '\u{b7}' | '\u{2022}' | '|' | '(' | ')' | '\u{2502}')).filter(|w| !w.is_empty()).collect()
}

fn parse_dollars(w: &str) -> Option<f64> {
    let w = w.trim_matches(|c: char| matches!(c, ',' | '.' | ')' | '(' | ';' | ':'));
    let n = w.strip_prefix('$')?;
    if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ',') {
        return None;
    }
    n.replace(',', "").parse().ok()
}

// ───────────────────────────── rules ─────────────────────────────

/// "57929 tokens", "↑ 3.4k tokens", "12.3k tokens used".
fn rule_tokens(line: &str, m: &mut Metrics) {
    let w = words(line);
    for i in 1..w.len() {
        if w[i].to_lowercase().starts_with("token") {
            if let Some(n) = parse_count(w[i - 1]) {
                m.tokens = Some(n);
                return;
            }
        }
    }
}

/// "Token usage: total=1,234 input=1,000 (+ 5,000 cached) output=234".
fn rule_codex_usage(line: &str, m: &mut Metrics) {
    let low = line.to_lowercase();
    if let Some(at) = low.find("total=") {
        let rest = &line[at + 6..];
        if let Some(n) = rest.split_whitespace().next().and_then(parse_count) {
            m.tokens = Some(n);
        }
    } else if let Some(at) = low.find("token usage:") {
        // "Token usage: 1.2K total (1K input + 200 output)"
        let rest = &line[at + 12..];
        let w: Vec<&str> = rest.split_whitespace().collect();
        if let Some(i) = w.iter().position(|x| x.eq_ignore_ascii_case("total")) {
            if i > 0 {
                m.tokens = parse_count(w[i - 1]);
            }
        }
    }
}

/// "Tokens: 1.2k sent, 340 received. Cost: $0.02 message, $0.15 session."
fn rule_aider(line: &str, m: &mut Metrics) {
    let low = line.to_lowercase();
    if let Some(at) = low.find("tokens:") {
        let w = words(&line[at + 7..]);
        let (mut sent, mut recv) = (None, None);
        for i in 1..w.len() {
            let t = w[i].trim_end_matches(|c: char| c == ',' || c == '.').to_lowercase();
            if t == "sent" {
                sent = parse_count(w[i - 1]);
            } else if t == "received" {
                recv = parse_count(w[i - 1]);
            }
        }
        if sent.is_some() || recv.is_some() {
            m.tokens = Some(sent.unwrap_or(0) + recv.unwrap_or(0));
        }
    }
}

/// "$0.00 spent", "Cost: $0.15 session", "session cost $1.20".
fn rule_cost(line: &str, m: &mut Metrics) {
    let low = line.to_lowercase();
    if !(low.contains("spent") || low.contains("cost") || low.contains("session") || low.contains("total")) {
        return;
    }
    let w = words(line);
    // "$0.15 session" wins over "$0.02 message".
    for i in 0..w.len() {
        if let Some(v) = parse_dollars(w[i]) {
            if w.get(i + 1).is_some_and(|n| n.trim_matches(|c: char| !c.is_alphabetic()).eq_ignore_ascii_case("session")) {
                m.cost = Some(v);
                return;
            }
        }
    }
    if let Some(v) = w.iter().find_map(|x| parse_dollars(x)) {
        m.cost = Some(v);
    }
}

/// "74% context left", "Context left until auto-compact: 12%", "6% used".
fn rule_context(line: &str, m: &mut Metrics) {
    let low = line.to_lowercase();
    let Some(pct_at) = low.find('%') else { return };
    let digits: String = low[..pct_at].chars().rev().take_while(|c| c.is_ascii_digit() || *c == '.').collect::<Vec<_>>().into_iter().rev().collect();
    let Ok(v) = digits.parse::<f32>() else { return };
    let v = v.clamp(0.0, 100.0);
    let around = &low;
    let left = around.contains("left") || around.contains("remaining");
    let used = around.contains("used") || around.contains("ctx") || (around.contains("context") && !left);
    if left {
        m.context_used = Some((100.0 - v).round() as u8);
    } else if used {
        m.context_used = Some(v.round() as u8);
    }
}

/// "4h 37m until reset", "resets in 2h 10m".
fn rule_reset(line: &str, m: &mut Metrics) {
    let low = line.to_lowercase();
    let dur = |s: &str| -> Option<String> {
        let parts: Vec<&str> = s.split_whitespace().rev().take_while(|w| is_duration_part(w)).collect();
        if parts.is_empty() { None } else { Some(parts.into_iter().rev().collect::<Vec<_>>().join(" ")) }
    };
    if let Some(at) = low.find("until reset") {
        m.reset_in = dur(line[..at].trim_end_matches(|c: char| c == ' ' || c == '\u{b7}'));
    } else if let Some(at) = low.find("resets in ") {
        let rest = &line[at + 10..];
        let parts: Vec<&str> = rest.split_whitespace().take_while(|w| is_duration_part(w)).collect();
        if !parts.is_empty() {
            m.reset_in = Some(parts.join(" "));
        }
    }
}

fn is_duration_part(w: &str) -> bool {
    let w = w.trim_matches(|c: char| c == ',' || c == '.');
    w.len() >= 2 && w.ends_with(|c: char| matches!(c, 'h' | 'm' | 'd' | 's')) && w[..w.len() - 1].chars().all(|c| c.is_ascii_digit())
}

/// Opus 5.5, Sonnet 4.5, claude-opus-4-1, gpt-5-codex, gemini-2.5-pro, o3.
fn rule_model(line: &str, m: &mut Metrics) {
    let w = words(line);
    for i in 0..w.len() {
        let t = w[i].trim_matches(|c: char| matches!(c, ',' | ':' | ';' | '[' | ']' | '"' | '\''));
        let low = t.to_lowercase();
        let family = matches!(low.as_str(), "opus" | "sonnet" | "haiku" | "fable");
        if family {
            let mut name = capitalize(&low);
            if let Some(v) = w.get(i + 1).filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()) && v.chars().all(|c| c.is_ascii_digit() || c == '.')) {
                name.push(' ');
                name.push_str(v.trim_end_matches('.'));
            }
            m.model = Some(name);
            return;
        }
        let id_like = (low.starts_with("claude-") || low.starts_with("gpt-") || low.starts_with("gemini-") || low.starts_with("o3") || low.starts_with("o4-") || low.starts_with("deepseek") || low.starts_with("qwen"))
            && t.len() >= 2
            && t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | ':'));
        if id_like && (low.contains('-') || low == "o3") {
            m.model = Some(strip_date(t));
            return;
        }
    }
}

/// claude-opus-4-1-20250805 -> claude-opus-4-1.
fn strip_date(id: &str) -> String {
    match id.rsplit_once('-') {
        Some((head, tail)) if tail.len() == 8 && tail.chars().all(|c| c.is_ascii_digit()) => head.to_string(),
        _ => id.to_string(),
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

// ───────────────────────────── display ─────────────────────────────

/// 57929 -> "57.9k", 1_200_000 -> "1.2M", 840 -> "840".
pub fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

pub fn fmt_cost(c: f64) -> String {
    if c > 0.0 && c < 0.01 {
        "<$0.01".into()
    } else {
        format!("${c:.2}")
    }
}

/// (tokens, cost, agents reporting cost) across agents.
pub fn totals<'a>(all: impl Iterator<Item = &'a Metrics>) -> (u64, f64) {
    all.fold((0, 0.0), |(t, c), m| (t + m.tokens.unwrap_or(0), c + m.cost.unwrap_or(0.0)))
}

// ───────────────────────────── collisions ─────────────────────────────

/// A live agent's place in git: (pane, work-tree root, branch).
pub type Place = (usize, Option<String>, Option<String>);

/// Agents that share a work tree *and* branch with another live agent:
/// `uid -> [other uids]`. Linked worktrees have their own root, so agents
/// started through "New Agent -> worktree" never collide.
pub fn collisions_of(places: &[Place]) -> HashMap<usize, Vec<usize>> {
    let mut out: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, (uid, root, branch)) in places.iter().enumerate() {
        let Some(root) = root else { continue };
        for (ouid, oroot, obranch) in places.iter().skip(i + 1) {
            if oroot.as_ref() == Some(root) && obranch == branch {
                out.entry(*uid).or_default().push(*ouid);
                out.entry(*ouid).or_default().push(*uid);
            }
        }
    }
    out
}

pub fn collisions(sessions: &[AgentSession]) -> HashMap<usize, Vec<usize>> {
    let places: Vec<Place> = sessions.iter().filter(|s| s.state.is_live()).map(|s| (s.pane_uid, s.git_root.as_ref().map(|r| r.trim_end_matches('/').to_string()), s.branch.clone())).collect();
    collisions_of(&places)
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(str::to_string).collect()
    }

    #[test]
    fn count_parsing() {
        assert_eq!(parse_count("57929"), Some(57_929));
        assert_eq!(parse_count("57,929"), Some(57_929));
        assert_eq!(parse_count("3.4k"), Some(3_400));
        assert_eq!(parse_count("1.2M"), Some(1_200_000));
        assert_eq!(parse_count("\u{2191}3.4k"), Some(3_400));
        assert_eq!(parse_count("tokens"), None);
        assert_eq!(parse_count("..."), None);
        assert_eq!(parse_count(""), None);
    }

    #[test]
    fn claude_code_status_line() {
        // A custom status line in the style of the issue ("57929 tokens", "$0.00 spent", "4h 37m until reset").
        let screen = lines("\n  ? for shortcuts\n  Opus 5.5 \u{b7} 57929 tokens \u{b7} $0.00 spent \u{b7} 4h 37m until reset\n");
        let m = parse_screen(Some(ClaudeCode), &screen);
        assert_eq!(m.model.as_deref(), Some("Opus 5.5"));
        assert_eq!(m.tokens, Some(57_929));
        assert_eq!(m.cost, Some(0.0));
        assert_eq!(m.reset_in.as_deref(), Some("4h 37m"));
    }

    #[test]
    fn claude_code_spinner_and_autocompact() {
        let screen = lines("\u{2736} Thinking\u{2026} (12s \u{b7} \u{2191} 3.4k tokens \u{b7} esc to interrupt)\n\n  Context left until auto-compact: 12%");
        let m = parse_screen(Some(ClaudeCode), &screen);
        assert_eq!(m.tokens, Some(3_400));
        assert_eq!(m.context_used, Some(88));
        assert_eq!(m.model, None);
    }

    #[test]
    fn claude_code_model_id_drops_the_date() {
        let m = parse_screen(Some(ClaudeCode), &lines("model: claude-opus-4-1-20250805"));
        assert_eq!(m.model.as_deref(), Some("claude-opus-4-1"));
    }

    #[test]
    fn codex_footer_and_usage() {
        let screen = lines(
            "\u{2502} model:     gpt-5-codex medium  \u{2502}\n\
             \n\
             Token usage: total=12,345 input=10,000 (+ 5,000 cached) output=2,345\n\
             \u{23ce} send   \u{2303}J newline   \u{2303}C quit    74% context left",
        );
        let m = parse_screen(Some(Codex), &screen);
        assert_eq!(m.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(m.tokens, Some(12_345));
        assert_eq!(m.context_used, Some(26));
    }

    #[test]
    fn codex_status_command_format() {
        let m = parse_screen(Some(Codex), &lines("Token usage: 1.2K total (1K input + 200 output)"));
        assert_eq!(m.tokens, Some(1_200));
    }

    #[test]
    fn gemini_footer() {
        let m = parse_screen(Some(Gemini), &lines("~/dev/aurora (main*)     no sandbox (see /docs)     gemini-2.5-pro (93% context left)"));
        assert_eq!(m.model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(m.context_used, Some(7));
    }

    #[test]
    fn aider_message_and_session_cost() {
        let m = parse_screen(Some(Aider), &lines("Tokens: 1.2k sent, 340 received. Cost: $0.02 message, $0.15 session."));
        assert_eq!(m.tokens, Some(1_540));
        assert_eq!(m.cost, Some(0.15), "session cost, not message cost");
    }

    #[test]
    fn opencode_sidebar() {
        let screen = lines("Context\n12,345 tokens\n6% used\n$0.12 spent");
        let m = parse_screen(Some(OpenCode), &screen);
        assert_eq!(m.tokens, Some(12_345));
        assert_eq!(m.context_used, Some(6));
        assert_eq!(m.cost, Some(0.12));
    }

    #[test]
    fn resets_in_phrase() {
        let m = parse_screen(None, &lines("Usage limit reached \u{b7} resets in 2h 10m"));
        assert_eq!(m.reset_in.as_deref(), Some("2h 10m"));
    }

    #[test]
    fn footer_beats_banner_and_prose_is_ignored() {
        let screen = lines("Welcome to Sonnet 4.5\nI changed 3 tokens in the lexer and 100% of tests pass\nOpus 5.5 | 100 tokens");
        let m = parse_screen(None, &screen);
        assert_eq!(m.model.as_deref(), Some("Opus 5.5"), "bottom-most line wins");
        assert_eq!(m.tokens, Some(100));
        assert_eq!(m.context_used, None, "'100% of tests' is not a context reading");
        assert_eq!(parse_screen(None, &lines("$ cargo build\n   Compiling rift v0.3.0")), Metrics::default());
        assert_eq!(parse_screen(None, &[]), Metrics::default());
    }

    #[test]
    fn merge_keeps_last_known_values() {
        let old = Metrics { model: Some("Opus 5.5".into()), tokens: Some(10), cost: Some(1.0), ..Default::default() };
        let new = Metrics { tokens: Some(20), ..Default::default() };
        let m = merge(&old, &new);
        assert_eq!((m.model.as_deref(), m.tokens, m.cost), (Some("Opus 5.5"), Some(20), Some(1.0)));
    }

    #[test]
    fn display_formats() {
        assert_eq!(fmt_tokens(840), "840");
        assert_eq!(fmt_tokens(1_540), "1.5k");
        assert_eq!(fmt_tokens(57_929), "57.9k");
        assert_eq!(fmt_tokens(1_200_000), "1.2M");
        assert_eq!(fmt_cost(0.0), "$0.00");
        assert_eq!(fmt_cost(0.004), "<$0.01");
        assert_eq!(fmt_cost(1.234), "$1.23");
        let a = Metrics { tokens: Some(100), cost: Some(0.5), ..Default::default() };
        let b = Metrics { tokens: Some(50), ..Default::default() };
        let (t, c) = totals([&a, &b].into_iter());
        assert_eq!((t, c), (150, 0.5));
    }

    fn place(uid: usize, root: Option<&str>, branch: Option<&str>) -> Place {
        (uid, root.map(str::to_string), branch.map(str::to_string))
    }

    #[test]
    fn same_root_and_branch_collide() {
        let c = collisions_of(&[place(1, Some("/r/aurora"), Some("main")), place(2, Some("/r/aurora"), Some("main")), place(3, Some("/r/aurora"), Some("feature"))]);
        assert_eq!(c.get(&1), Some(&vec![2]));
        assert_eq!(c.get(&2), Some(&vec![1]));
        assert!(!c.contains_key(&3), "other branch");
    }

    #[test]
    fn linked_worktrees_do_not_collide() {
        let c = collisions_of(&[place(1, Some("/r/aurora"), Some("main")), place(2, Some("/r/aurora-claude-1"), Some("agent/claude-1"))]);
        assert!(c.is_empty());
        // Same branch name in different roots is still two checkouts.
        let c = collisions_of(&[place(1, Some("/a"), Some("main")), place(2, Some("/b"), Some("main"))]);
        assert!(c.is_empty());
    }

    #[test]
    fn outside_git_never_collides_and_three_way_lists_both_others() {
        assert!(collisions_of(&[place(1, None, None), place(2, None, None)]).is_empty());
        let c = collisions_of(&[place(1, Some("/r"), Some("m")), place(2, Some("/r"), Some("m")), place(3, Some("/r"), Some("m"))]);
        assert_eq!(c[&1], vec![2, 3]);
        assert_eq!(c[&3], vec![1, 2]);
        // Detached HEAD in the same tree: same (None) branch collides too.
        let c = collisions_of(&[place(1, Some("/r"), None), place(2, Some("/r"), None)]);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn session_wrapper_ignores_finished_agents() {
        use crate::agents::registry::{PaneObs, Probe};
        let t0 = std::time::Instant::now();
        let mut reg = crate::agents::AgentRegistry::new();
        for uid in [1usize, 2] {
            let o = PaneObs { uid, osc_seen: true, block_running: true, running_cmd: Some("claude".into()), ..Default::default() };
            reg.observe_pane(&o, &mut || Probe::Unknown, t0);
        }
        // No cwd -> no git root: nothing to compare.
        assert!(collisions(reg.sessions()).is_empty());
    }
}
