//! Workflow templates: the three built-ins plus the user's own from
//! `~/.config/rift/workflows.toml`, a tiny TOML reader for that file and the
//! `{task}` / `{branch}` / `{cwd}` placeholder expansion.
//!
//! ```toml
//! [[workflow]]
//! name = "Refactor with review"
//! description = "Claude writes, Codex reviews, up to 2 rounds"
//! strategy = "write-review"          # single | best-of | write-review | fix-tests
//! agents = ["claude", "codex"]       # writer first, reviewer second
//! worktree = true                    # run in a fresh git worktree
//! rounds = 2
//! test = "cargo test"                # optional
//! prompt = """
//! Task: {task}
//! You work on branch {branch} in {cwd}.
//! """
//! ```

use std::path::{Path, PathBuf};

use crate::agents::AgentKind;

// ───────────────────────────── model ─────────────────────────────

/// What a workflow does once its agents run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// One agent, one prompt.
    Single,
    /// Several agents get the same task in their own worktrees; compare at the end.
    BestOf,
    /// `agents[0]` writes, `agents[1]` reviews each turn.
    WriteReview,
    /// Run the test command, feed failures to the agent, repeat until green.
    FixTests,
}

impl Strategy {
    pub fn parse(s: &str) -> Option<Strategy> {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "single" | "one" => Some(Strategy::Single),
            "best-of" | "bestof" | "best-of-n" | "compare" | "parallel" => Some(Strategy::BestOf),
            "write-review" | "write-and-review" | "review" | "writer-reviewer" => Some(Strategy::WriteReview),
            "fix-tests" | "fix" | "fix-failing-tests" => Some(Strategy::FixTests),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Strategy::Single => "single agent",
            Strategy::BestOf => "best of N",
            Strategy::WriteReview => "write & review",
            Strategy::FixTests => "fix failing tests",
        }
    }
}

/// Default number of review rounds / fix attempts.
pub const DEFAULT_ROUNDS: u32 = 3;
pub const MAX_ROUNDS: u32 = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    pub name: String,
    pub description: String,
    pub strategy: Strategy,
    /// Agents to start. Empty = "ask": the wizard fills it from the default agent.
    pub agents: Vec<AgentKind>,
    /// Use a fresh git worktree per agent (needs a git repository).
    pub worktree: bool,
    /// Prompt sent to the agents; `{task}`, `{branch}`, `{cwd}` are expanded.
    pub prompt: String,
    /// Shell command that checks the result (tests); empty = none.
    pub test: String,
    /// Review rounds (write-review) or fix attempts (fix-tests).
    pub rounds: u32,
    /// Forward the reviewer's feedback to the writer without waiting for `f`.
    pub auto_forward: bool,
    pub builtin: bool,
}

impl Template {
    pub fn new(name: &str, strategy: Strategy) -> Template {
        Template {
            name: name.to_string(),
            description: String::new(),
            strategy,
            agents: Vec::new(),
            worktree: matches!(strategy, Strategy::BestOf | Strategy::WriteReview),
            prompt: "{task}".to_string(),
            test: String::new(),
            rounds: DEFAULT_ROUNDS,
            auto_forward: false,
            builtin: false,
        }
    }
}

/// The templates that ship with Rift.
pub fn builtins() -> Vec<Template> {
    let mut best = Template::new("Best of 3", Strategy::BestOf);
    best.description = "Three agents solve the same task in their own worktrees; compare and merge the best".into();
    best.builtin = true;
    let mut wr = Template::new("Write & Review", Strategy::WriteReview);
    wr.description = "One agent writes; a second reviews every turn and sends feedback back".into();
    wr.builtin = true;
    let mut fix = Template::new("Fix failing tests", Strategy::FixTests);
    fix.description = "Run the test command, hand failures to the agent, re-run until green".into();
    fix.worktree = false;
    fix.prompt = "{task}".into();
    fix.builtin = true;
    vec![best, wr, fix]
}

/// Built-ins first, then the user's. A user template with a built-in's name replaces it.
pub fn merge_templates(user: Vec<Template>) -> Vec<Template> {
    let mut all = builtins();
    for t in user {
        match all.iter().position(|x| x.name.eq_ignore_ascii_case(&t.name)) {
            Some(i) => all[i] = t,
            None => all.push(t),
        }
    }
    all
}

// ───────────────────────────── placeholders ─────────────────────────────

/// Values for the placeholders.
#[derive(Clone, Debug, Default)]
pub struct Vars {
    pub task: String,
    pub branch: String,
    pub cwd: String,
}

/// Replace `{task}`, `{branch}` and `{cwd}`. Single pass: a value that itself
/// contains `{branch}` is not expanded again, and unknown `{...}` stays as is.
pub fn expand(template: &str, vars: &Vars) -> String {
    let mut out = String::with_capacity(template.len() + vars.task.len());
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let hit = ["task", "branch", "cwd"].into_iter().find(|n| after.starts_with(n) && after[n.len()..].starts_with('}'));
        match hit {
            Some(name) => {
                out.push_str(match name {
                    "task" => &vars.task,
                    "branch" => &vars.branch,
                    _ => &vars.cwd,
                });
                rest = &after[name.len() + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

// ───────────────────────────── file ─────────────────────────────

/// `~/.config/rift/workflows.toml`.
pub fn workflows_path() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".config").join("rift").join("workflows.toml")
}

/// Result of reading the user's file: templates plus one message per problem.
#[derive(Debug, Default)]
pub struct Loaded {
    pub templates: Vec<Template>,
    pub warnings: Vec<String>,
}

pub fn load_user_templates() -> Loaded {
    match std::fs::read_to_string(workflows_path()) {
        Ok(text) => parse_workflows(&text),
        Err(_) => Loaded::default(),
    }
}

/// Parse the contents of a `workflows.toml`.
pub fn parse_workflows(text: &str) -> Loaded {
    let mut out = Loaded::default();
    let (tables, errors) = parse_tables(text);
    out.warnings.extend(errors);
    for (n, t) in tables.into_iter().enumerate() {
        match template_from_table(&t) {
            Ok(tpl) => out.templates.push(tpl),
            Err(e) => out.warnings.push(format!("workflow #{}: {e}", n + 1)),
        }
    }
    out
}

fn template_from_table(t: &Table) -> Result<Template, String> {
    let name = t.str("name").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).ok_or("missing name")?;
    let agents: Vec<AgentKind> = match t.get("agents") {
        None => Vec::new(),
        Some(Value::Array(items)) => {
            let mut v = Vec::new();
            for it in items {
                let s = match it {
                    Value::Str(s) => s,
                    _ => return Err(format!("{name}: agents must be strings")),
                };
                v.push(AgentKind::parse(s).ok_or_else(|| format!("{name}: unknown agent '{s}'"))?);
            }
            v
        }
        Some(Value::Str(s)) => vec![AgentKind::parse(s).ok_or_else(|| format!("{name}: unknown agent '{s}'"))?],
        Some(_) => return Err(format!("{name}: agents must be a list")),
    };
    let strategy = match t.str("strategy") {
        Some(s) => Strategy::parse(s).ok_or_else(|| format!("{name}: unknown strategy '{s}'"))?,
        None if agents.len() >= 2 => Strategy::BestOf,
        None => Strategy::Single,
    };
    let mut tpl = Template::new(&name, strategy);
    tpl.agents = agents;
    tpl.description = t.str("description").unwrap_or("").to_string();
    if let Some(b) = t.bool("worktree") {
        tpl.worktree = b;
    } else if strategy == Strategy::Single {
        tpl.worktree = false;
    }
    if let Some(p) = t.str("prompt").filter(|p| !p.trim().is_empty()) {
        tpl.prompt = p.trim_end().to_string();
    }
    tpl.test = t.str("test").unwrap_or("").trim().to_string();
    if let Some(r) = t.int("rounds") {
        tpl.rounds = r.clamp(1, MAX_ROUNDS as i64) as u32;
    }
    tpl.auto_forward = t.bool("auto_forward").unwrap_or(false);
    match strategy {
        Strategy::WriteReview if tpl.agents.len() == 1 => tpl.agents.push(tpl.agents[0]),
        Strategy::WriteReview if tpl.agents.len() > 2 => return Err(format!("{name}: write-review takes a writer and a reviewer")),
        Strategy::BestOf if tpl.agents.len() > MAX_CANDIDATES => return Err(format!("{name}: at most {MAX_CANDIDATES} agents")),
        Strategy::FixTests | Strategy::Single if tpl.agents.len() > 1 => return Err(format!("{name}: {} takes one agent", strategy.label())),
        _ => {}
    }
    if strategy == Strategy::FixTests && tpl.test.is_empty() {
        // The wizard offers an auto-detected command; the file may leave it out.
    }
    Ok(tpl)
}

/// Most candidates in a best-of-N run.
pub const MAX_CANDIDATES: usize = 4;

// ───────────────────────────── tiny TOML ─────────────────────────────
//
// Just the subset workflows.toml needs: `[[workflow]]` tables with string
// (basic, literal, multi-line), integer, boolean and string-array values.

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    Array(Vec<Value>),
}

#[derive(Default, Debug)]
struct Table {
    entries: Vec<(String, Value)>,
}

impl Table {
    fn get(&self, k: &str) -> Option<&Value> {
        self.entries.iter().rev().find(|(key, _)| key == k).map(|(_, v)| v)
    }
    fn str(&self, k: &str) -> Option<&str> {
        match self.get(k) {
            Some(Value::Str(s)) => Some(s),
            _ => None,
        }
    }
    fn int(&self, k: &str) -> Option<i64> {
        match self.get(k) {
            Some(Value::Int(i)) => Some(*i),
            _ => None,
        }
    }
    fn bool(&self, k: &str) -> Option<bool> {
        match self.get(k) {
            Some(Value::Bool(b)) => Some(*b),
            Some(Value::Str(s)) => match s.to_ascii_lowercase().as_str() {
                "yes" | "true" => Some(true),
                "no" | "false" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }
}

struct Cursor<'a> {
    s: &'a str,
    pos: usize,
    line: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<char> {
        self.s[self.pos..].chars().next()
    }
    fn starts(&self, p: &str) -> bool {
        self.s[self.pos..].starts_with(p)
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }
    fn skip_inline_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\r')) {
            self.bump();
        }
    }
    /// Whitespace, newlines and comments (inside arrays).
    fn skip_ws_all(&mut self) {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\r' | '\n') => {
                    self.bump();
                }
                Some('#') => self.skip_line(),
                _ => break,
            }
        }
    }
    fn skip_line(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            self.bump();
        }
    }
}

fn parse_tables(text: &str) -> (Vec<Table>, Vec<String>) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut cur = Cursor { s: text, pos: 0, line: 1 };
    let mut tables: Vec<Table> = Vec::new();
    let mut errors = Vec::new();
    let mut in_workflow = false;
    loop {
        cur.skip_ws_all();
        let Some(c) = cur.peek() else { break };
        if c == '[' {
            let line = cur.line;
            let mut name = String::new();
            while let Some(c) = cur.peek() {
                if c == '\n' || c == '#' {
                    break;
                }
                name.push(c);
                cur.bump();
            }
            cur.skip_line();
            let name = name.trim();
            if name.replace(' ', "") == "[[workflow]]" {
                tables.push(Table::default());
                in_workflow = true;
            } else {
                in_workflow = false;
                errors.push(format!("line {line}: ignoring section {name}"));
            }
            continue;
        }
        // key = value
        let line = cur.line;
        let mut key = String::new();
        while let Some(c) = cur.peek() {
            if c == '=' || c == '\n' {
                break;
            }
            key.push(c);
            cur.bump();
        }
        if cur.peek() != Some('=') {
            errors.push(format!("line {line}: expected key = value"));
            continue;
        }
        cur.bump();
        let key = key.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        cur.skip_inline_ws();
        match parse_value(&mut cur) {
            Ok(v) => {
                cur.skip_inline_ws();
                if cur.peek() == Some('#') {
                    cur.skip_line();
                }
                if in_workflow {
                    if let Some(t) = tables.last_mut() {
                        t.entries.push((key, v));
                    }
                }
            }
            Err(e) => {
                errors.push(format!("line {line}: {e}"));
                cur.skip_line();
            }
        }
    }
    (tables, errors)
}

fn parse_value(cur: &mut Cursor) -> Result<Value, String> {
    match cur.peek() {
        Some('"') => parse_basic_string(cur).map(Value::Str),
        Some('\'') => parse_literal_string(cur).map(Value::Str),
        Some('[') => {
            cur.bump();
            let mut items = Vec::new();
            loop {
                cur.skip_ws_all();
                match cur.peek() {
                    Some(']') => {
                        cur.bump();
                        break;
                    }
                    Some(',') => {
                        cur.bump();
                    }
                    None => return Err("unterminated array".into()),
                    _ => items.push(parse_value(cur)?),
                }
            }
            Ok(Value::Array(items))
        }
        Some(_) => {
            let mut word = String::new();
            while let Some(c) = cur.peek() {
                if c.is_whitespace() || c == '#' || c == ',' || c == ']' {
                    break;
                }
                word.push(c);
                cur.bump();
            }
            match word.as_str() {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                w => w.replace('_', "").parse::<i64>().map(Value::Int).map_err(|_| format!("cannot read value '{w}'")),
            }
        }
        None => Err("missing value".into()),
    }
}

fn parse_basic_string(cur: &mut Cursor) -> Result<String, String> {
    let multi = cur.starts("\"\"\"");
    cur.bump();
    if multi {
        cur.bump();
        cur.bump();
        // A newline right after the opening quotes is dropped.
        if cur.starts("\r\n") {
            cur.bump();
            cur.bump();
        } else if cur.starts("\n") {
            cur.bump();
        }
    }
    let mut out = String::new();
    loop {
        if multi && cur.starts("\"\"\"") {
            for _ in 0..3 {
                cur.bump();
            }
            return Ok(out);
        }
        if !multi && cur.peek() == Some('\n') {
            return Err("newline in a single-line string".into());
        }
        match cur.bump() {
            None => return Err("unterminated string".into()),
            Some('"') if !multi => return Ok(out),
            Some('\\') => match cur.bump() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                // Line-ending backslash in a multi-line string trims the break and indentation.
                Some('\n') if multi => {
                    while matches!(cur.peek(), Some(' ' | '\t' | '\n' | '\r')) {
                        cur.bump();
                    }
                }
                Some(c) => {
                    out.push('\\');
                    out.push(c);
                }
                None => return Err("unterminated string".into()),
            },
            Some(c) => out.push(c),
        }
    }
}

fn parse_literal_string(cur: &mut Cursor) -> Result<String, String> {
    let multi = cur.starts("'''");
    cur.bump();
    if multi {
        cur.bump();
        cur.bump();
        if cur.starts("\r\n") {
            cur.bump();
            cur.bump();
        } else if cur.starts("\n") {
            cur.bump();
        }
    }
    let mut out = String::new();
    loop {
        if multi && cur.starts("'''") {
            for _ in 0..3 {
                cur.bump();
            }
            return Ok(out);
        }
        if !multi && cur.peek() == Some('\n') {
            return Err("newline in a single-line string".into());
        }
        match cur.bump() {
            None => return Err("unterminated string".into()),
            Some('\'') if !multi => return Ok(out),
            Some(c) => out.push(c),
        }
    }
}

// ───────────────────────────── test command detection ─────────────────────────────

/// A sensible test command for the project in `dir`, if one is recognisable.
pub fn detect_test_command(dir: &Path) -> Option<String> {
    let has = |f: &str| dir.join(f).is_file();
    if has("Cargo.toml") {
        return Some("cargo test".into());
    }
    if has("package.json") {
        let pkg = std::fs::read_to_string(dir.join("package.json")).unwrap_or_default();
        if pkg.contains("\"test\"") {
            let runner = if has("pnpm-lock.yaml") {
                "pnpm test"
            } else if has("yarn.lock") {
                "yarn test"
            } else {
                "npm test"
            };
            return Some(runner.into());
        }
    }
    if has("go.mod") {
        return Some("go test ./...".into());
    }
    if has("pyproject.toml") || has("pytest.ini") || has("setup.py") || has("tox.ini") {
        return Some("pytest".into());
    }
    if let Ok(mk) = std::fs::read_to_string(dir.join("Makefile")) {
        if mk.lines().any(|l| l.starts_with("test:")) {
            return Some("make test".into());
        }
    }
    None
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vars {
        Vars { task: "add a retry".into(), branch: "agent/bestof-add-a-retry-1".into(), cwd: "/w/app".into() }
    }

    #[test]
    fn placeholders_expand_once() {
        assert_eq!(expand("{task}", &vars()), "add a retry");
        assert_eq!(expand("On {branch} in {cwd}: {task}.", &vars()), "On agent/bestof-add-a-retry-1 in /w/app: add a retry.");
        // Unknown placeholders and stray braces survive.
        assert_eq!(expand("{nope} {} { {\"a\":1} {task", &vars()), "{nope} {} { {\"a\":1} {task");
        // Values are not re-expanded.
        let v = Vars { task: "use {branch}".into(), branch: "b".into(), cwd: String::new() };
        assert_eq!(expand("{task} / {branch}", &v), "use {branch} / b");
        assert_eq!(expand("", &vars()), "");
        assert_eq!(expand("{task}{task}", &vars()), "add a retryadd a retry");
        assert_eq!(expand("\u{4e2d}\u{6587} {task} \u{2713}", &vars()), "\u{4e2d}\u{6587} add a retry \u{2713}");
    }

    #[test]
    fn builtins_are_the_three_documented_ones() {
        let b = builtins();
        let names: Vec<&str> = b.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Best of 3", "Write & Review", "Fix failing tests"]);
        assert_eq!(b[0].strategy, Strategy::BestOf);
        assert_eq!(b[1].strategy, Strategy::WriteReview);
        assert_eq!(b[2].strategy, Strategy::FixTests);
        assert!(b.iter().all(|t| t.builtin && t.rounds == DEFAULT_ROUNDS));
        assert!(b[0].worktree && b[1].worktree && !b[2].worktree);
    }

    const FILE: &str = r#"
# my workflows
[[workflow]]
name = "Refactor with review"
description = "Claude writes, Codex reviews"
strategy = "write-review"
agents = ["claude", "codex"]
worktree = true
rounds = 2
auto_forward = true
test = "cargo test --bin rift"   # trailing comment
prompt = """
Task: {task}
Branch {branch} at {cwd}
"""

[[workflow]]
name = "Quick fix"
agents = ["gemini"]
prompt = 'literal {task} with \ backslash'

[[workflow]]
name = "Trio"
agents = [
  "claude",   # first
  "codex",
  "gemini",
]
"#;

    #[test]
    fn parses_a_realistic_file() {
        let l = parse_workflows(FILE);
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        assert_eq!(l.templates.len(), 3);
        let a = &l.templates[0];
        assert_eq!(a.name, "Refactor with review");
        assert_eq!(a.strategy, Strategy::WriteReview);
        assert_eq!(a.agents, vec![AgentKind::ClaudeCode, AgentKind::Codex]);
        assert!(a.worktree && a.auto_forward);
        assert_eq!(a.rounds, 2);
        assert_eq!(a.test, "cargo test --bin rift");
        assert_eq!(a.prompt, "Task: {task}\nBranch {branch} at {cwd}");
        let b = &l.templates[1];
        assert_eq!(b.strategy, Strategy::Single);
        assert!(!b.worktree, "single agents default to the current directory");
        assert_eq!(b.prompt, "literal {task} with \\ backslash");
        let c = &l.templates[2];
        assert_eq!(c.strategy, Strategy::BestOf, "several agents imply best-of");
        assert_eq!(c.agents.len(), 3);
        assert!(c.worktree);
    }

    #[test]
    fn bad_entries_are_skipped_with_a_warning() {
        let l = parse_workflows("[[workflow]]\nagents = [\"claude\"]\n[[workflow]]\nname = \"x\"\nagents = [\"vim\"]\n[[workflow]]\nname = \"ok\"\n[other]\nfoo = 1\n[[workflow]]\nname = \"s\"\nstrategy = \"magic\"\n");
        assert_eq!(l.templates.len(), 1);
        assert_eq!(l.templates[0].name, "ok");
        assert_eq!(l.warnings.len(), 4, "{:?}", l.warnings);
        assert!(l.warnings.iter().any(|w| w.contains("missing name")));
        assert!(l.warnings.iter().any(|w| w.contains("unknown agent 'vim'")));
        assert!(l.warnings.iter().any(|w| w.contains("unknown strategy")));
        assert!(l.warnings.iter().any(|w| w.contains("ignoring section")));
    }

    #[test]
    fn syntax_errors_do_not_poison_the_rest() {
        let l = parse_workflows("[[workflow]]\nname = \"a\"\nrounds = banana\nprompt = \"unterminated\n[[workflow]]\nname = \"b\"\n");
        assert_eq!(l.templates.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert!(!l.warnings.is_empty());
        assert!(parse_workflows("").templates.is_empty());
        assert!(parse_workflows("\u{feff}[[workflow]]\nname = \"bom\"").templates.len() == 1);
    }

    #[test]
    fn rounds_are_clamped_and_agent_counts_validated() {
        let l = parse_workflows("[[workflow]]\nname = \"r\"\nrounds = 99\n[[workflow]]\nname = \"w\"\nstrategy = \"write-review\"\nagents = [\"claude\"]\n[[workflow]]\nname = \"many\"\nstrategy = \"best-of\"\nagents = [\"claude\",\"claude\",\"claude\",\"claude\",\"claude\"]\n[[workflow]]\nname = \"fx\"\nstrategy = \"fix-tests\"\nagents = [\"claude\",\"codex\"]\n");
        assert_eq!(l.templates.len(), 2, "{:?}", l.warnings);
        assert_eq!(l.templates[0].rounds, MAX_ROUNDS);
        // A single agent reviews its own work in a second pane.
        assert_eq!(l.templates[1].agents, vec![AgentKind::ClaudeCode, AgentKind::ClaudeCode]);
        assert_eq!(l.warnings.len(), 2);
    }

    #[test]
    fn user_templates_extend_and_override_builtins() {
        let user = parse_workflows("[[workflow]]\nname = \"best of 3\"\nagents = [\"codex\"]\n[[workflow]]\nname = \"Mine\"\n").templates;
        let all = merge_templates(user);
        assert_eq!(all.len(), 4);
        assert_eq!(all[0].name, "best of 3");
        assert!(!all[0].builtin);
        assert_eq!(all[3].name, "Mine");
    }

    #[test]
    fn strategy_names() {
        assert_eq!(Strategy::parse("Best_Of"), Some(Strategy::BestOf));
        assert_eq!(Strategy::parse("write-review"), Some(Strategy::WriteReview));
        assert_eq!(Strategy::parse("fix_tests"), Some(Strategy::FixTests));
        assert_eq!(Strategy::parse("x"), None);
    }

    #[test]
    fn test_command_detection() {
        let dir = std::env::temp_dir().join(format!("rift-wf-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(detect_test_command(&dir), None);
        std::fs::write(dir.join("package.json"), "{\"scripts\":{\"test\":\"jest\"}}").unwrap();
        assert_eq!(detect_test_command(&dir).as_deref(), Some("npm test"));
        std::fs::write(dir.join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(detect_test_command(&dir).as_deref(), Some("pnpm test"));
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        assert_eq!(detect_test_command(&dir).as_deref(), Some("cargo test"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
