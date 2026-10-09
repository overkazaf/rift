//! Approval prompts, parsed off an agent's screen so the dock can show what is
//! being asked and answer it without switching panes.
//!
//! * [`parse`] turns the bottom rows of a pane into an [`ApprovalPrompt`]: what
//!   is requested (a shell command, an edit, a new file ...), and the options
//!   the agent offers with their [`Role`] (approve / always / deny).
//! * [`plan_answer`] maps an option to the keystrokes the CLI expects (the digit
//!   for numbered menus, the `(y)` shortcut, arrows + Enter for selection
//!   lists) and [`encode_keys`] turns them into bytes with the same encoder the
//!   keyboard uses (`crate::input`), so kitty / application-cursor modes are
//!   honoured.
//!
//! Everything is table driven (`QUESTIONS`, `LABEL_ROLES`) and tested against
//! realistic fixtures of Claude Code, Codex CLI, Gemini CLI, Aider, opencode
//! and Cursor CLI. When nothing parses, [`raw_tail`] gives the last lines so
//! the card still shows *something* useful.

use super::AgentKind;
use crate::input::{self, EncodeOpts, EventType, KeyInput, Mods};
use winit::keyboard::{Key, KeyLocation, NamedKey};

// ───────────────────────────── model ─────────────────────────────

/// What answering an option means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Allow this once.
    Approve,
    /// Allow, and stop asking (this command / this session).
    Always,
    /// Refuse.
    Deny,
    /// Anything else (modify in an editor, type feedback ...).
    Other,
}

/// What the agent wants to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    Command,
    Edit,
    Create,
    Plan,
    Trust,
    Generic,
}

impl PromptKind {
    pub fn label(self) -> &'static str {
        match self {
            PromptKind::Command => "run command",
            PromptKind::Edit => "edit file",
            PromptKind::Create => "write file",
            PromptKind::Plan => "approve plan",
            PromptKind::Trust => "trust folder",
            PromptKind::Generic => "permission",
        }
    }
}

/// How an option is selected on the agent's screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    /// Numbered menu: pressing the digit picks it.
    Numbered,
    /// A one-key shortcut such as `(y)`; `enter` when the CLI also needs Enter.
    Shortcut { key: char, enter: bool },
    /// Esc is the shortcut (`(esc)`).
    Escape,
    /// Move the highlight vertically, then Enter.
    Vertical,
    /// Move the highlight horizontally, then Enter.
    Horizontal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptOption {
    /// The number the agent shows, when it numbers its options.
    pub number: Option<u8>,
    pub label: String,
    pub role: Role,
    pub nav: Nav,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalPrompt {
    pub agent: Option<AgentKind>,
    pub kind: PromptKind,
    /// The question as the agent phrases it.
    pub question: String,
    /// Short heading ("Bash command", "Edit file") when the agent shows one.
    pub title: String,
    /// What is requested: command lines, a file path ...
    pub subject: Vec<String>,
    /// The shell command to risk-check, for [`PromptKind::Command`].
    pub command: Option<String>,
    pub file: Option<String>,
    pub reason: Option<String>,
    pub options: Vec<PromptOption>,
    /// Index of the highlighted option (the `❯` marker), when visible.
    pub selected: Option<usize>,
}

impl ApprovalPrompt {
    /// First option with `role`.
    pub fn index_of(&self, role: Role) -> Option<usize> {
        self.options.iter().position(|o| o.role == role)
    }

    /// Option the digit key `d` stands for: the numbered option with that
    /// number, else (unnumbered menus) 1 = approve, 2 = always, 3 = deny.
    pub fn option_for_digit(&self, d: u8) -> Option<usize> {
        if let Some(i) = self.options.iter().position(|o| o.number == Some(d)) {
            return Some(i);
        }
        if self.options.iter().any(|o| o.number.is_some()) {
            return None;
        }
        match d {
            1 => self.index_of(Role::Approve),
            2 => self.index_of(Role::Always),
            3 => self.index_of(Role::Deny),
            _ => None,
        }
    }

    /// The key hint shown on a button: the agent's own number, else the digit
    /// the dock accepts.
    pub fn key_hint(&self, idx: usize) -> String {
        let o = &self.options[idx];
        if let Some(n) = o.number {
            return n.to_string();
        }
        match o.role {
            Role::Approve => "1".into(),
            Role::Always => "2".into(),
            Role::Deny => "3".into(),
            Role::Other => "?".into(),
        }
    }

    /// Does answering this option let the agent run / write something?
    pub fn grants(&self, idx: usize) -> bool {
        matches!(self.options.get(idx).map(|o| o.role), Some(Role::Approve | Role::Always))
    }
}

// ───────────────────────────── text helpers ─────────────────────────────

fn is_frame(c: char) -> bool {
    matches!(c, '\u{2500}'..='\u{257f}' | '\u{2580}'..='\u{259f}')
}

/// A screen line without its box frame, trimmed.
pub fn unframe(line: &str) -> String {
    let t: String = line.trim().trim_matches(|c: char| is_frame(c) || c.is_whitespace()).to_string();
    t
}

/// A horizontal frame line: `╭────╮`, `├╌╌╌┤`, `─────`.
fn is_rule(line: &str) -> bool {
    let t = line.trim();
    let horiz = t.chars().filter(|c| matches!(c, '\u{2500}' | '\u{2501}' | '\u{254c}' | '\u{254d}' | '\u{2504}'..='\u{2509}' | '\u{2550}')).count();
    horiz >= 3 && t.chars().all(|c| is_frame(c) || c.is_whitespace())
}

/// Marker kept for rule lines once the frame is stripped, so the block
/// structure (where a box starts) survives `unframe`.
const RULE: &str = "\u{2500}\u{2500}\u{2500}";

fn inner_of(raw: &str) -> String {
    if is_rule(raw) { RULE.to_string() } else { unframe(raw) }
}

const MARKERS: &[char] = &['\u{276f}', '\u{203a}', '>', '\u{25b6}', '\u{25cf}', '\u{25c9}', '\u{2192}', '*', '\u{2022}'];

/// Strip a leading selection marker; returns (rest, had marker).
fn strip_marker(s: &str) -> (&str, bool) {
    let t = s.trim_start();
    let mut chars = t.chars();
    match chars.next() {
        Some(c) if MARKERS.contains(&c) && chars.as_str().starts_with(' ') => (chars.as_str().trim_start(), true),
        Some('\u{25cb}') if chars.as_str().starts_with(' ') => (chars.as_str().trim_start(), false),
        _ => (t, false),
    }
}

/// "3. foo" / "3) foo" -> (3, "foo").
fn numbered(s: &str) -> Option<(u8, &str)> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 2 {
        return None;
    }
    let rest = &s[digits.len()..];
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    let label = rest.trim();
    if !rest.starts_with(' ') || label.is_empty() {
        return None;
    }
    Some((digits.parse().ok()?, label))
}

/// Split a trailing `(y)` / `(esc)` / `(shift+tab)` shortcut off a label.
fn split_shortcut(label: &str) -> (String, Option<String>) {
    let l = label.trim_end();
    if l.ends_with(')') {
        if let Some(open) = l.rfind('(') {
            let inner = l[open + 1..l.len() - 1].trim();
            let known = matches!(inner.to_ascii_lowercase().as_str(), "y" | "n" | "a" | "d" | "esc" | "tab" | "shift+tab" | "esc or n" | "enter");
            if known {
                return (l[..open].trim_end().to_string(), Some(inner.to_ascii_lowercase()));
            }
        }
    }
    (l.to_string(), None)
}

// ───────────────────────────── tables ─────────────────────────────

/// How the agent phrases its question -> what is being asked. First match wins;
/// needles are lowercase substrings of the question line.
const QUESTIONS: &[(&str, PromptKind)] = &[
    ("make this edit", PromptKind::Edit),
    ("make the following edits", PromptKind::Edit),
    ("apply this change", PromptKind::Edit),
    ("apply these changes", PromptKind::Edit),
    ("do you want to create", PromptKind::Create),
    ("do you want to overwrite", PromptKind::Create),
    ("allow execution of", PromptKind::Command),
    ("run the following command", PromptKind::Command),
    ("run this command", PromptKind::Command),
    ("allow command", PromptKind::Command),
    ("trust the files in this folder", PromptKind::Trust),
    ("trust this folder", PromptKind::Trust),
    ("trust the contents", PromptKind::Trust),
    ("work in this folder", PromptKind::Trust),
    ("ready to code", PromptKind::Plan),
    ("would you like to proceed", PromptKind::Plan),
    ("do you want to proceed", PromptKind::Generic),
    ("do you want to", PromptKind::Generic),
    ("would you like to", PromptKind::Generic),
    ("allow ", PromptKind::Generic),
];

/// Option label (lowercase) -> role. First match wins, so the more specific
/// "always" phrasings come before plain "yes".
const LABEL_ROLES: &[(&str, Role)] = &[
    ("yes, and don't ask again", Role::Always),
    ("yes, and do not ask again", Role::Always),
    ("yes, allow all", Role::Always),
    ("yes, allow always", Role::Always),
    ("allow always", Role::Always),
    ("always allow", Role::Always),
    ("don't ask again", Role::Always),
    ("for this session", Role::Always),
    ("during this session", Role::Always),
    ("allowlist", Role::Always),
    ("modify with external editor", Role::Other),
    ("type here", Role::Other),
    ("yes", Role::Approve),
    ("allow once", Role::Approve),
    ("run (once)", Role::Approve),
    ("proceed", Role::Approve),
    ("approve", Role::Approve),
    ("no", Role::Deny),
    ("reject", Role::Deny),
    ("deny", Role::Deny),
    ("skip", Role::Deny),
    ("cancel", Role::Deny),
];

fn classify(label: &str) -> Role {
    let l = label.to_lowercase();
    for (needle, role) in LABEL_ROLES {
        // Short words match at the start only ("no" must not match "notify ...").
        let hit = if needle.len() <= 3 { l == *needle || l.starts_with(&format!("{needle},")) || l.starts_with(&format!("{needle} ")) } else { l.contains(needle) };
        if hit {
            return *role;
        }
    }
    Role::Other
}

// ───────────────────────────── parsing ─────────────────────────────

const SCAN_ROWS: usize = 40;
/// Wrapped label lines tolerated between two numbered options (narrow panes).
const MAX_WRAP_LINES: usize = 3;

fn clean_rows(lines: &[String]) -> Vec<String> {
    let from = lines.len().saturating_sub(SCAN_ROWS);
    lines[from..].iter().map(|l| l.trim_end().to_string()).collect()
}

/// The last lines of the screen, for cards whose prompt cannot be parsed.
pub fn raw_tail(lines: &[String], n: usize) -> Vec<String> {
    let mut v: Vec<String> = lines.iter().map(|l| unframe(l)).filter(|l| !l.is_empty() && !is_rule(l)).collect();
    let skip = v.len().saturating_sub(n);
    v.drain(..skip);
    v
}

struct OptionLine {
    row: usize,
    marker: bool,
    number: u8,
    label: String,
}

fn is_question(l: &str) -> bool {
    let low = l.to_lowercase();
    QUESTIONS.iter().any(|(n, _)| low.contains(n)) || low.ends_with('?')
}

/// Parse the approval prompt visible in `lines` (screen rows, top to bottom).
/// `agent` only breaks ties; the layouts are recognised on their own.
pub fn parse(agent: Option<AgentKind>, lines: &[String]) -> Option<ApprovalPrompt> {
    let rows = clean_rows(lines);
    parse_numbered(agent, &rows).or_else(|| parse_shortcuts(agent, &rows)).or_else(|| parse_horizontal(agent, &rows)).or_else(|| parse_yes_no(agent, &rows))
}

fn parse_numbered(agent: Option<AgentKind>, rows: &[String]) -> Option<ApprovalPrompt> {
    let inner: Vec<String> = rows.iter().map(|r| inner_of(r)).collect();
    // Bottom-most numbered line, then walk up while the run continues (one
    // wrapped label line may sit between two options).
    let last = inner.iter().rposition(|l| numbered(strip_marker(l).0).is_some())?;
    let mut found: Vec<OptionLine> = Vec::new();
    let mut gap = 0;
    let mut row = last as isize;
    while row >= 0 {
        let l = &inner[row as usize];
        let (rest, marker) = strip_marker(l);
        if let Some((n, label)) = numbered(rest) {
            found.push(OptionLine { row: row as usize, marker, number: n, label: label.to_string() });
            gap = 0;
        } else if l.is_empty() || is_rule(l) {
            break;
        } else {
            gap += 1;
            if gap > MAX_WRAP_LINES || found.is_empty() {
                break;
            }
        }
        row -= 1;
    }
    found.reverse();
    // An unrelated list just above the options: keep only the last run that starts at 1.
    if let Some(k) = found.iter().rposition(|o| o.number == 1) {
        found.drain(..k);
    }
    if found.first().map(|o| o.number) != Some(1) || found.len() < 2 {
        return None;
    }
    if !found.windows(2).all(|w| w[1].number == w[0].number + 1) {
        return None;
    }
    // Wrapped label lines belong to the option above them.
    let mut options = Vec::new();
    for (k, o) in found.iter().enumerate() {
        let next_row = found.get(k + 1).map_or(o.row + 1, |n| n.row);
        let mut label = o.label.clone();
        for r in o.row + 1..next_row {
            label.push(' ');
            label.push_str(&inner[r]);
        }
        let (label, shortcut) = split_shortcut(&label);
        let nav = Nav::Numbered;
        let _ = shortcut;
        options.push(PromptOption { number: Some(o.number), role: classify(&label), label, nav });
    }
    let first_row = found[0].row;
    let selected = found.iter().position(|o| o.marker);

    // The question: nearest known-phrase line above the options, else the
    // nearest line ending in '?'.
    let qrow = (0..first_row).rev().take(16).find(|&r| {
        let low = inner[r].to_lowercase();
        QUESTIONS.iter().any(|(n, _)| low.contains(n))
    }).or_else(|| (0..first_row).rev().take(6).find(|&r| inner[r].ends_with('?')))?;
    let question = inner[qrow].clone();

    // At least one option must read as a yes/no decision; otherwise this is
    // just a numbered list in the agent's answer.
    if !options.iter().any(|o| matches!(o.role, Role::Approve | Role::Deny | Role::Always)) {
        return None;
    }
    Some(build(agent, &inner, qrow, first_row, question, options, selected))
}

/// Fill kind, subject, command and file from the lines around the question.
fn build(agent: Option<AgentKind>, inner: &[String], qrow: usize, first_opt: usize, question: String, options: Vec<PromptOption>, selected: Option<usize>) -> ApprovalPrompt {
    let qlow = question.to_lowercase();
    let mut kind = QUESTIONS.iter().find(|(n, _)| qlow.contains(n)).map_or(PromptKind::Generic, |(_, k)| *k);

    // Lines after the question and before the options (Codex layout) ...
    let after: Vec<String> = inner[qrow + 1..first_opt].iter().filter(|l| !l.is_empty() && !is_rule(l)).cloned().collect();
    // ... and the block above it up to the frame top / a blank gap (Claude, Gemini).
    let mut above: Vec<String> = Vec::new();
    let mut r = qrow;
    while r > 0 {
        r -= 1;
        let l = &inner[r];
        if is_rule(&inner[r]) && above.iter().any(|x| !x.is_empty()) {
            break;
        }
        if is_rule(l) {
            continue;
        }
        above.push(l.clone());
        if above.len() >= 14 {
            break;
        }
    }
    above.reverse();
    // Drop leading blank lines and anything above a screen-clearing gap of 2 blanks.
    let mut block: Vec<String> = Vec::new();
    let mut blanks = 0;
    for l in above.iter().rev() {
        if l.is_empty() {
            blanks += 1;
            if blanks >= 2 && !block.is_empty() {
                break;
            }
            block.push(String::new());
        } else {
            blanks = 0;
            block.push(l.clone());
        }
    }
    block.reverse();
    let block: Vec<String> = block.into_iter().skip_while(|l| l.is_empty()).collect();

    let mut title = String::new();
    let mut subject: Vec<String> = Vec::new();
    let mut reason = None;

    let from_after = !after.is_empty();
    let source: Vec<String> = if from_after { after } else { block.iter().filter(|l| !l.is_empty()).cloned().collect() };
    for (i, l) in source.iter().enumerate() {
        if let Some(r) = l.strip_prefix("Reason:") {
            reason = Some(r.trim().to_string());
        } else if !from_after && i == 0 && is_heading(l) {
            title = l.clone();
        } else {
            subject.push(l.clone());
        }
    }
    // Gemini puts the command inside the question: Allow execution of: 'npm test'?
    let quoted = quoted_command(&question);

    let tl = title.to_lowercase();
    if matches!(kind, PromptKind::Generic) {
        if tl.contains("command") || tl.contains("bash") || tl.contains("shell") || subject.first().is_some_and(|l| l.starts_with("$ ")) {
            kind = PromptKind::Command;
        } else if tl.contains("edit") || tl.contains("update") {
            kind = PromptKind::Edit;
        } else if tl.contains("create") || tl.contains("write") {
            kind = PromptKind::Create;
        }
    }
    if kind == PromptKind::Create && qlow.contains("overwrite") {
        title = title.if_empty("Overwrite file");
    }

    let mut command = None;
    let mut file = None;
    match kind {
        PromptKind::Command => {
            // "$ git push", or Cursor's "Not in allowlist: npm test".
            let mut cmd_lines: Vec<String> = subject
                .iter()
                .map(|l| l.strip_prefix("$ ").unwrap_or(l))
                .map(|l| l.split_once("allowlist:").map_or(l, |(_, rest)| rest.trim()).to_string())
                .collect();
            // Claude Code adds a one-line description under the command.
            if cmd_lines.len() >= 2 && looks_like_description(cmd_lines.last().unwrap()) {
                cmd_lines.pop();
            }
            let joined = cmd_lines.join("\n");
            command = quoted.or(if joined.trim().is_empty() { None } else { Some(joined) });
            if let Some(c) = &command {
                subject = c.lines().map(str::to_string).collect();
            }
        }
        PromptKind::Edit | PromptKind::Create => {
            file = find_path(&subject).or_else(|| path_in_question(&question));
            let keep = file.clone();
            subject = keep.into_iter().collect();
        }
        _ => {}
    }
    subject.truncate(12);
    let _ = agent;
    ApprovalPrompt { agent, kind, question, title, subject, command, file, reason, options, selected }
}

trait IfEmpty {
    fn if_empty(self, d: &str) -> String;
}
impl IfEmpty for String {
    fn if_empty(self, d: &str) -> String {
        if self.is_empty() { d.to_string() } else { self }
    }
}

/// "Bash command", "Edit file", "Create file" ...
fn is_heading(l: &str) -> bool {
    let low = l.to_lowercase();
    l.chars().count() <= 32
        && !l.contains('/')
        && !l.contains('$')
        && ["command", "edit", "create", "write", "update", "read", "tool", "fetch", "search", "plan", "overwrite", "delete", "mcp"].iter().any(|w| low.contains(w))
}

fn path_in_question(q: &str) -> Option<String> {
    q.trim_end_matches('?').split_whitespace().rev().map(|w| w.trim_matches(|c| c == ':' || c == ',')).find(|w| is_pathish(w)).map(str::to_string)
}

/// `src/a/b.rs`, `README.md`: has a directory part or a short alphabetic extension.
fn is_pathish(w: &str) -> bool {
    if w.is_empty() || w.contains(['(', ')', '<', '>', '{', '}', ';', '=']) {
        return false;
    }
    if w.contains('/') {
        return w.len() > 2;
    }
    match w.rsplit_once('.') {
        Some((stem, ext)) => !stem.is_empty() && (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphabetic()) && stem.chars().any(|c| c.is_alphabetic()),
        None => false,
    }
}

/// First path-looking word in `lines` (top to bottom).
fn find_path(lines: &[String]) -> Option<String> {
    lines.iter().flat_map(|l| l.split_whitespace()).map(|w| w.trim_matches(|c| c == ':' || c == ',')).find(|w| is_pathish(w)).map(str::to_string)
}

/// "Build the project" under a command: sentence case, no shell syntax.
fn looks_like_description(l: &str) -> bool {
    let first = l.chars().next();
    first.is_some_and(|c| c.is_ascii_uppercase())
        && !l.contains(['|', '&', ';', '>', '<', '$', '`', '='])
        && l.split_whitespace().count() >= 2
}

/// 'cmd' inside "Allow execution of: 'cmd'?" (also "...of [cmd]").
fn quoted_command(q: &str) -> Option<String> {
    let low = q.to_lowercase();
    let at = low.find("allow execution of")? + "allow execution of".len();
    let rest = q[at..].trim_start_matches([':', ' ']).trim_end_matches('?').trim();
    let rest = rest.trim_matches(|c| c == '\'' || c == '"' || c == '`');
    if rest.is_empty() { None } else { Some(rest.to_string()) }
}

/// Unnumbered vertical lists whose items end in a `(key)` shortcut (Cursor CLI).
fn parse_shortcuts(agent: Option<AgentKind>, rows: &[String]) -> Option<ApprovalPrompt> {
    let inner: Vec<String> = rows.iter().map(|r| inner_of(r)).collect();
    let last = inner.iter().rposition(|l| {
        let (rest, _) = strip_marker(l);
        split_shortcut(rest).1.is_some()
    })?;
    let mut items: Vec<(usize, bool, String, String)> = Vec::new();
    let mut row = last as isize;
    while row >= 0 {
        let (rest, marker) = strip_marker(&inner[row as usize]);
        match split_shortcut(rest) {
            (label, Some(key)) if !label.is_empty() => items.push((row as usize, marker, label, key)),
            _ => break,
        }
        row -= 1;
    }
    items.reverse();
    if items.len() < 2 {
        return None;
    }
    let first_row = items[0].0;
    let qrow = (0..first_row).rev().take(8).find(|&r| is_question(&inner[r]))?;
    let options: Vec<PromptOption> = items
        .iter()
        .map(|(_, _, label, key)| {
            let nav = match key.as_str() {
                "esc" | "esc or n" => Nav::Escape,
                k if k.chars().count() == 1 => Nav::Shortcut { key: k.chars().next().unwrap(), enter: false },
                _ => Nav::Vertical,
            };
            PromptOption { number: None, role: classify(label), label: label.clone(), nav }
        })
        .collect();
    if !options.iter().any(|o| matches!(o.role, Role::Approve | Role::Deny)) {
        return None;
    }
    let selected = items.iter().position(|i| i.1);
    let question = inner[qrow].clone();
    let mut p = build(agent, &inner, qrow, first_row, question, options, selected);
    if p.kind == PromptKind::Generic && p.command.is_none() {
        // "Run this command?" / "Not in allowlist: npm test"
        if let Some(l) = p.subject.iter().find_map(|l| l.split_once("allowlist:").map(|x| x.1.trim().to_string())) {
            p.kind = PromptKind::Command;
            p.command = Some(l.clone());
            p.subject = vec![l];
        }
    }
    Some(p)
}

/// "Allow once  Allow always  Reject" on one line (opencode).
fn parse_horizontal(agent: Option<AgentKind>, rows: &[String]) -> Option<ApprovalPrompt> {
    let inner: Vec<String> = rows.iter().map(|r| inner_of(r)).collect();
    let row = inner.iter().rposition(|l| {
        let low = l.to_lowercase();
        low.contains("allow once") && low.contains("allow always") && (low.contains("reject") || low.contains("deny"))
    })?;
    let line = &inner[row];
    let low = line.to_ascii_lowercase();
    let mut spots: Vec<(usize, &str, Role)> = Vec::new();
    for (needle, role) in [("allow once", Role::Approve), ("allow always", Role::Always), ("reject", Role::Deny), ("deny", Role::Deny)] {
        if let Some(at) = low.find(needle) {
            spots.push((at, &line[at..at + needle.len()], role));
        }
    }
    spots.sort_by_key(|s| s.0);
    spots.dedup_by_key(|s| s.2);
    let options = spots
        .iter()
        .map(|(_, label, role)| PromptOption { number: None, label: capitalize(label), role: *role, nav: Nav::Horizontal })
        .collect();
    let above: Vec<String> = (0..row).rev().take(8).map(|r| inner[r].clone()).filter(|l| !l.is_empty() && !is_rule(l)).take(3).collect::<Vec<_>>().into_iter().rev().collect();
    let title = above.first().cloned().unwrap_or_default();
    let subject: Vec<String> = above.iter().skip(1).cloned().collect();
    let command = subject.iter().find_map(|l| {
        let low = l.to_ascii_lowercase();
        ["run command:", "command:", "bash:", "$ "].iter().find_map(|p| low.starts_with(p).then(|| l[p.len()..].trim().to_string()))
    });
    Some(ApprovalPrompt {
        agent,
        kind: if command.is_some() { PromptKind::Command } else { PromptKind::Generic },
        question: title.clone(),
        title,
        subject,
        command,
        file: None,
        reason: None,
        options,
        selected: Some(0),
    })
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// `[y/n]`, `(Y)es/(N)o` and friends (Aider and generic CLIs): one letter + Enter.
fn parse_yes_no(agent: Option<AgentKind>, rows: &[String]) -> Option<ApprovalPrompt> {
    let inner: Vec<String> = rows.iter().map(|r| inner_of(r)).collect();
    let row = inner.iter().rposition(|l| {
        let low = l.to_lowercase();
        low.contains("[y/n]") || low.contains("(y/n)") || low.contains("(y)es/(n)o") || low.contains("[yes/no]")
    })?;
    let line = &inner[row];
    let low = line.to_lowercase();
    let mut options = vec![
        PromptOption { number: None, label: "Yes".into(), role: Role::Approve, nav: Nav::Shortcut { key: 'y', enter: true } },
        PromptOption { number: None, label: "No".into(), role: Role::Deny, nav: Nav::Shortcut { key: 'n', enter: true } },
    ];
    // Aider: (A)ll / (S)kip all / (D)on't ask again.
    if low.contains("(a)ll") {
        options.insert(1, PromptOption { number: None, label: "All".into(), role: Role::Always, nav: Nav::Shortcut { key: 'a', enter: true } });
    }
    let question = line.trim_end_matches(':').trim().to_string();
    // The thing being asked about is usually on the line(s) above.
    let subject: Vec<String> = (0..row).rev().take(3).map(|r| inner[r].clone()).filter(|l| !l.is_empty() && !is_rule(l)).take(2).collect::<Vec<_>>().into_iter().rev().collect();
    Some(ApprovalPrompt { agent, kind: PromptKind::Generic, question, title: String::new(), subject, command: None, file: None, reason: None, options, selected: None })
}

// ───────────────────────────── answering ─────────────────────────────

/// One key to press on the agent's screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keystroke {
    Digit(u8),
    Char(char),
    Enter,
    Esc,
    Up,
    Down,
    Left,
    Right,
}

/// Keys that choose option `idx`, or `None` when the screen does not tell
/// where the highlight is (the button is then disabled rather than guessing).
pub fn plan_answer(p: &ApprovalPrompt, idx: usize) -> Option<Vec<Keystroke>> {
    let o = p.options.get(idx)?;
    match o.nav {
        Nav::Numbered => Some(vec![Keystroke::Digit(o.number?)]),
        Nav::Shortcut { key, enter } => {
            let mut v = vec![Keystroke::Char(key)];
            if enter {
                v.push(Keystroke::Enter);
            }
            Some(v)
        }
        Nav::Escape => Some(vec![Keystroke::Esc]),
        Nav::Vertical | Nav::Horizontal => {
            let cur = p.selected?;
            let (fwd, back) = if o.nav == Nav::Vertical { (Keystroke::Down, Keystroke::Up) } else { (Keystroke::Right, Keystroke::Left) };
            let mut v = Vec::new();
            if idx >= cur {
                v.extend(std::iter::repeat(fwd).take(idx - cur));
            } else {
                v.extend(std::iter::repeat(back).take(cur - idx));
            }
            v.push(Keystroke::Enter);
            Some(v)
        }
    }
}

fn named(n: NamedKey) -> KeyInput {
    KeyInput {
        key: Key::Named(n),
        base: None,
        base_layout: None,
        text: None,
        location: KeyLocation::Standard,
        state: EventType::Press,
        mods: Mods::default(),
        alt_side: input::AltSide::Unknown,
    }
}

fn character(c: char, mods: Mods) -> KeyInput {
    KeyInput {
        key: Key::Character(c.to_string().into()),
        base: Some(c.to_ascii_lowercase()),
        base_layout: Some(c.to_ascii_lowercase()),
        text: if mods.ctrl { None } else { Some(c.to_string()) },
        location: KeyLocation::Standard,
        state: EventType::Press,
        mods,
        alt_side: input::AltSide::Unknown,
    }
}

/// Bytes for a key sequence, produced by the terminal's own key encoder.
pub fn encode_keys(keys: &[Keystroke], opts: &EncodeOpts) -> Vec<u8> {
    let mut out = Vec::new();
    for k in keys {
        let ki = match *k {
            Keystroke::Digit(d) => character((b'0' + d % 10) as char, Mods::default()),
            Keystroke::Char(c) => character(c, Mods::default()),
            Keystroke::Enter => named(NamedKey::Enter),
            Keystroke::Esc => named(NamedKey::Escape),
            Keystroke::Up => named(NamedKey::ArrowUp),
            Keystroke::Down => named(NamedKey::ArrowDown),
            Keystroke::Left => named(NamedKey::ArrowLeft),
            Keystroke::Right => named(NamedKey::ArrowRight),
        };
        if let Some(b) = input::encode(&ki, opts) {
            out.extend(b);
        }
    }
    out
}

/// ESC: interrupts Claude Code / Codex / Gemini mid-turn.
pub fn interrupt_bytes(opts: &EncodeOpts) -> Vec<u8> {
    encode_keys(&[Keystroke::Esc], opts)
}

/// Ctrl+C.
pub fn ctrl_c_bytes(opts: &EncodeOpts) -> Vec<u8> {
    input::encode(&character('c', Mods { ctrl: true, ..Mods::default() }), opts).unwrap_or_else(|| vec![0x03])
}

/// Bytes that type `text` and submit it. Line breaks use the configured
/// Shift+Enter encoding (what multi-line prompts need), the submit is Enter.
pub fn reply_bytes(text: &str, opts: &EncodeOpts) -> Vec<u8> {
    let shift_enter = input::encode(&KeyInput { mods: Mods { shift: true, ..Mods::default() }, ..named(NamedKey::Enter) }, opts).unwrap_or_else(|| b"\n".to_vec());
    let enter = input::encode(&named(NamedKey::Enter), opts).unwrap_or_else(|| b"\r".to_vec());
    let mut out = Vec::new();
    let mut first = true;
    for line in text.split('\n') {
        if !first {
            out.extend(&shift_enter);
        }
        first = false;
        out.extend(line.trim_end_matches('\r').as_bytes());
    }
    out.extend(enter);
    out
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
pub(crate) mod fixtures {
    pub fn lines(s: &str) -> Vec<String> {
        s.lines().map(str::to_string).collect()
    }

    pub const CLAUDE_BASH: &str = "
● I'll run the unit tests now.

╭──────────────────────────────────────────────────────────────╮
│ Bash command                                                 │
│                                                              │
│   cargo test --bin rift                                      │
│   Run the unit tests                                         │
│                                                              │
│ Do you want to proceed?                                      │
│ ❯ 1. Yes                                                     │
│   2. Yes, and don't ask again for cargo test commands in     │
│   /Users/maya/dev/rterm                                      │
│   3. No, and tell Claude what to do differently (esc)        │
│                                                              │
╰──────────────────────────────────────────────────────────────╯";

    pub const CLAUDE_EDIT: &str = "
╭──────────────────────────────────────────────────────────────╮
│ Edit file                                                    │
│ ╭──────────────────────────────────────────────────────────╮ │
│ │ src/agents/ui.rs                                         │ │
│ │                                                          │ │
│ │   41 -    let w = 34;                                    │ │
│ │   41 +    let w = 36;                                    │ │
│ ╰──────────────────────────────────────────────────────────╯ │
│ Do you want to make this edit to ui.rs?                      │
│ ❯ 1. Yes                                                     │
│   2. Yes, allow all edits during this session (shift+tab)    │
│   3. No, and tell Claude what to do differently (esc)        │
╰──────────────────────────────────────────────────────────────╯";

    pub const CLAUDE_RM: &str = "
╭──────────────────────────────────────────────────────────────╮
│ Bash command                                                 │
│                                                              │
│   rm -rf ~/projects/build && git reset --hard                │
│   Wipe build output and reset                                │
│                                                              │
│ Do you want to proceed?                                      │
│   1. Yes                                                     │
│ ❯ 2. Yes, and don't ask again for rm commands in /tmp        │
│   3. No, and tell Claude what to do differently (esc)        │
╰──────────────────────────────────────────────────────────────╯";

    pub const CODEX_CMD: &str = "
  Would you like to run the following command?

  Reason: command failed; retry without sandbox?

  $ git push origin main

› 1. Yes, proceed (y)
  2. Yes, and don't ask again for this command (a)
  3. No, and tell Codex what to do differently (esc)

  Press enter to confirm or esc to cancel";

    pub const CODEX_EDITS: &str = "
  Would you like to make the following edits?

  Reason: this is a test

  README.md (+3 -1)

› 1. Yes, proceed (y)
  2. Yes, and don't ask again for these files (a)
  3. No, and tell Codex what to do differently (esc)";

    pub const GEMINI_SHELL: &str = "
╭──────────────────────────────────────────────────────────────╮
│ Shell Command                                                │
│ Allow execution of: 'npm install'?                           │
│                                                              │
│ ● 1. Yes, allow once                                         │
│   2. Yes, allow always \"npm install\"                         │
│   3. Modify with external editor                             │
│   4. No, suggest changes (esc)                               │
╰──────────────────────────────────────────────────────────────╯";

    pub const GEMINI_EDIT: &str = "
╭──────────────────────────────────────────────────────────────╮
│ Edit src/main.rs: fn main() => fn main() -> Result<()>       │
│                                                              │
│ 1 - fn main() {                                              │
│ 1 + fn main() -> Result<()> {                                │
│                                                              │
│ Apply this change?                                           │
│                                                              │
│ ● 1. Yes, allow once                                         │
│   2. Yes, allow always                                       │
│   3. Modify with external editor                             │
│   4. No, suggest changes (esc)                               │
╰──────────────────────────────────────────────────────────────╯";

    pub const AIDER_YN: &str = "
Add src/main.rs to the chat?
Add file to the chat? (Y)es/(N)o/(A)ll/(S)kip all/(D)on't ask again [Yes]:";

    pub const OPENCODE: &str = "
┃ Permission required
┃ Run command: bun test
┃
┃   Allow once     Allow always     Reject
┃  ←/→ select  enter confirm";

    pub const CURSOR: &str = "
  Run this command?
  Not in allowlist: npm test

→ Run (once) (y)
  Add Shell(npm) to allowlist (tab)
  Skip (esc or n)";

    pub const NOT_A_PROMPT: &str = "
Here are two ways forward:
1. Refactor the parser
2. Keep the current design
What do you think?";
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    fn opts() -> EncodeOpts {
        EncodeOpts::default()
    }

    #[test]
    fn claude_bash_command() {
        let p = parse(Some(AgentKind::ClaudeCode), &lines(CLAUDE_BASH)).expect("prompt");
        assert_eq!(p.kind, PromptKind::Command);
        assert_eq!(p.title, "Bash command");
        assert_eq!(p.question, "Do you want to proceed?");
        assert_eq!(p.command.as_deref(), Some("cargo test --bin rift"), "description line is not part of the command");
        assert_eq!(p.options.len(), 3);
        assert_eq!(p.options.iter().map(|o| o.role).collect::<Vec<_>>(), vec![Role::Approve, Role::Always, Role::Deny]);
        assert_eq!(p.options[1].label, "Yes, and don't ask again for cargo test commands in /Users/maya/dev/rterm", "wrapped label is joined");
        assert_eq!(p.options[2].number, Some(3));
        assert_eq!(p.selected, Some(0));
    }

    #[test]
    fn claude_edit_names_the_file() {
        let p = parse(None, &lines(CLAUDE_EDIT)).expect("prompt");
        assert_eq!(p.kind, PromptKind::Edit);
        assert_eq!(p.file.as_deref(), Some("src/agents/ui.rs"));
        assert!(p.command.is_none());
        assert_eq!(p.index_of(Role::Always), Some(1));
        assert_eq!(p.options[1].number, Some(2));
    }

    #[test]
    fn claude_highlight_can_sit_on_another_option() {
        let p = parse(None, &lines(CLAUDE_RM)).unwrap();
        assert_eq!(p.selected, Some(1));
        assert_eq!(p.command.as_deref(), Some("rm -rf ~/projects/build && git reset --hard"));
    }

    #[test]
    fn codex_command_with_reason() {
        let p = parse(Some(AgentKind::Codex), &lines(CODEX_CMD)).expect("prompt");
        assert_eq!(p.kind, PromptKind::Command);
        assert_eq!(p.command.as_deref(), Some("git push origin main"));
        assert_eq!(p.reason.as_deref(), Some("command failed; retry without sandbox?"));
        assert_eq!(p.question, "Would you like to run the following command?");
        assert_eq!(p.options.len(), 3);
        assert_eq!(p.options[0].label, "Yes, proceed (y)".trim_end_matches(" (y)"));
        assert_eq!(p.options[2].role, Role::Deny);
        assert_eq!(p.selected, Some(0));
    }

    #[test]
    fn codex_edits() {
        let p = parse(Some(AgentKind::Codex), &lines(CODEX_EDITS)).expect("prompt");
        assert_eq!(p.kind, PromptKind::Edit);
        assert_eq!(p.reason.as_deref(), Some("this is a test"));
        assert_eq!(p.options[1].role, Role::Always);
    }

    #[test]
    fn gemini_shell_takes_the_command_from_the_question() {
        let p = parse(Some(AgentKind::Gemini), &lines(GEMINI_SHELL)).expect("prompt");
        assert_eq!(p.kind, PromptKind::Command);
        assert_eq!(p.command.as_deref(), Some("npm install"));
        assert_eq!(p.options.len(), 4);
        assert_eq!(p.options[2].role, Role::Other, "modify in an external editor is not a decision");
        assert_eq!(p.options[3].role, Role::Deny);
        assert_eq!(p.selected, Some(0));
    }

    #[test]
    fn gemini_edit() {
        let p = parse(Some(AgentKind::Gemini), &lines(GEMINI_EDIT)).expect("prompt");
        assert_eq!(p.kind, PromptKind::Edit);
        assert_eq!(p.question, "Apply this change?");
        assert_eq!(p.file.as_deref(), Some("src/main.rs"));
        assert_eq!(p.index_of(Role::Always), Some(1));
    }

    #[test]
    fn aider_letters_need_enter() {
        let p = parse(Some(AgentKind::Aider), &lines(AIDER_YN)).expect("prompt");
        assert_eq!(p.options.iter().map(|o| o.role).collect::<Vec<_>>(), vec![Role::Approve, Role::Always, Role::Deny]);
        assert_eq!(plan_answer(&p, 0), Some(vec![Keystroke::Char('y'), Keystroke::Enter]));
        assert_eq!(plan_answer(&p, 2), Some(vec![Keystroke::Char('n'), Keystroke::Enter]));
    }

    #[test]
    fn opencode_horizontal_choice_uses_arrows() {
        let p = parse(Some(AgentKind::OpenCode), &lines(OPENCODE)).expect("prompt");
        assert_eq!(p.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), vec!["Allow once", "Allow always", "Reject"]);
        assert_eq!(plan_answer(&p, 0), Some(vec![Keystroke::Enter]));
        assert_eq!(plan_answer(&p, 2), Some(vec![Keystroke::Right, Keystroke::Right, Keystroke::Enter]));
    }

    #[test]
    fn cursor_shortcuts() {
        let p = parse(Some(AgentKind::CursorAgent), &lines(CURSOR)).expect("prompt");
        assert_eq!(p.kind, PromptKind::Command);
        assert_eq!(p.command.as_deref(), Some("npm test"));
        assert_eq!(plan_answer(&p, 0), Some(vec![Keystroke::Char('y')]));
        assert_eq!(plan_answer(&p, 2), Some(vec![Keystroke::Esc]));
        assert_eq!(p.options[1].role, Role::Always);
    }

    #[test]
    fn a_numbered_list_in_prose_is_not_a_prompt() {
        assert!(parse(None, &lines(NOT_A_PROMPT)).is_none());
        assert!(parse(None, &[]).is_none());
        assert!(parse(None, &lines("$ ls\nCargo.toml  src")).is_none());
    }

    #[test]
    fn an_old_prompt_far_above_does_not_count() {
        let mut v = lines(CLAUDE_BASH);
        v.extend((0..60).map(|i| format!("log line {i}")));
        assert!(parse(None, &v).is_none());
    }

    #[test]
    fn digit_mapping_prefers_the_agents_numbers() {
        let p = parse(None, &lines(CLAUDE_BASH)).unwrap();
        assert_eq!(p.option_for_digit(1), Some(0));
        assert_eq!(p.option_for_digit(2), Some(1));
        assert_eq!(p.option_for_digit(3), Some(2));
        assert_eq!(p.option_for_digit(4), None);
        // Gemini's option 2 is "always", its 4 is "deny": digits follow the screen.
        let g = parse(None, &lines(GEMINI_SHELL)).unwrap();
        assert_eq!(g.option_for_digit(4), Some(3));
        assert_eq!(g.key_hint(3), "4");
        // Unnumbered menus fall back to 1/2/3 = approve/always/deny.
        let a = parse(None, &lines(AIDER_YN)).unwrap();
        assert_eq!(a.option_for_digit(3), Some(2));
        assert_eq!(a.key_hint(0), "1");
        let o = parse(None, &lines(OPENCODE)).unwrap();
        assert_eq!(o.option_for_digit(2), Some(1));
    }

    #[test]
    fn option_to_bytes() {
        let p = parse(None, &lines(CLAUDE_BASH)).unwrap();
        let plain = opts();
        assert_eq!(encode_keys(&plan_answer(&p, 0).unwrap(), &plain), b"1");
        assert_eq!(encode_keys(&plan_answer(&p, 1).unwrap(), &plain), b"2");
        assert_eq!(encode_keys(&plan_answer(&p, 2).unwrap(), &plain), b"3");
        let o = parse(None, &lines(OPENCODE)).unwrap();
        assert_eq!(encode_keys(&plan_answer(&o, 1).unwrap(), &plain), b"\x1b[C\r");
        // Application cursor mode switches arrows to SS3.
        let app = EncodeOpts { app_cursor: true, ..plain };
        assert_eq!(encode_keys(&plan_answer(&o, 1).unwrap(), &app), b"\x1bOC\r");
        let cur = parse(None, &lines(CURSOR)).unwrap();
        assert_eq!(encode_keys(&plan_answer(&cur, 2).unwrap(), &plain), b"\x1b");
    }

    #[test]
    fn kitty_protocol_changes_escape_and_enter() {
        let kitty = EncodeOpts { kitty_flags: 1, ..opts() };
        assert_eq!(interrupt_bytes(&opts()), b"\x1b");
        assert_eq!(interrupt_bytes(&kitty), b"\x1b[27u");
        assert_eq!(encode_keys(&[Keystroke::Enter], &kitty), b"\r", "disambiguate mode keeps a bare Enter");
        assert_eq!(ctrl_c_bytes(&opts()), vec![0x03]);
        assert_eq!(ctrl_c_bytes(&kitty), b"\x1b[99;5u");
    }

    #[test]
    fn reply_encoding_follows_shift_enter_config() {
        let o = opts();
        assert_eq!(reply_bytes("fix the test", &o), b"fix the test\r");
        assert_eq!(reply_bytes("line one\nline two", &o), b"line one\x1b\rline two\r");
        let csi = EncodeOpts { shift_enter: input::ShiftEnter::CsiU, ..opts() };
        assert_eq!(reply_bytes("a\nb", &csi), b"a\x1b[13;2ub\r");
        let lf = EncodeOpts { shift_enter: input::ShiftEnter::Lf, ..opts() };
        assert_eq!(reply_bytes("a\nb", &lf), b"a\nb\r");
        let kitty = EncodeOpts { kitty_flags: 1, ..opts() };
        assert_eq!(reply_bytes("a\nb", &kitty), b"a\x1b[13;2ub\r");
    }

    #[test]
    fn unparsable_screens_degrade_to_raw_lines() {
        let screen = lines("some output\n\n╭────╮\n│ Please confirm the deploy to prod │\n╰────╯\nWaiting...");
        assert!(parse(None, &screen).is_none());
        assert_eq!(raw_tail(&screen, 2), vec!["Please confirm the deploy to prod".to_string(), "Waiting...".to_string()]);
    }

    #[test]
    fn role_classification() {
        assert_eq!(classify("Yes"), Role::Approve);
        assert_eq!(classify("Yes, proceed"), Role::Approve);
        assert_eq!(classify("Yes, and don't ask again for this command"), Role::Always);
        assert_eq!(classify("No, and tell Claude what to do differently"), Role::Deny);
        assert_eq!(classify("No"), Role::Deny);
        assert_eq!(classify("Notify me later"), Role::Other, "short words match whole words only");
        assert_eq!(classify("Modify with external editor"), Role::Other);
    }
}
