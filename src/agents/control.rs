//! Keyboard state machine of the Mission Control dock: browsing, answering
//! approval prompts, replying, broadcasting, confirmations and the context
//! menu. Pure (no `App`, no drawing): the dock asks it what a key means and
//! the runtime carries the resulting [`Action`] out. Tested in this file.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::metrics::Metrics;
use super::prompt::ApprovalPrompt;
use crate::review::TurnDigest;

/// After an interrupt, a second Esc within this long hands the keyboard back.
pub const ESC_WINDOW: Duration = Duration::from_secs(3);

// ───────────────────────────── per-agent data ─────────────────────────────

/// How dangerous the command in an approval prompt is.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Risk {
    /// Nothing to check, or checked and routine.
    #[default]
    Safe,
    /// The command check is still running.
    Pending,
    /// Flagged (`Severity::Warning`): red badge.
    Risky(Vec<String>),
    /// Flagged `Severity::Critical`: red badge, needs a second press.
    Critical(Vec<String>),
}

impl Risk {
    pub fn is_flagged(&self) -> bool {
        matches!(self, Risk::Risky(_) | Risk::Critical(_))
    }

    pub fn impacts(&self) -> &[String] {
        match self {
            Risk::Risky(v) | Risk::Critical(v) => v,
            _ => &[],
        }
    }
}

/// Everything the dock knows about an agent beyond its registry session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaneInfo {
    /// The approval prompt on screen, when one parses.
    pub prompt: Option<ApprovalPrompt>,
    /// The prompt sits at the bottom of the screen (autopilot acts only then).
    pub settled: bool,
    /// Last lines of the screen, shown when a waiting agent's prompt does not parse.
    pub raw_tail: Vec<String>,
    pub metrics: Metrics,
    pub risk: Risk,
    pub turns: Vec<TurnDigest>,
    /// (files changed in the latest turn, files over all turns).
    pub files: (Option<usize>, usize),
    /// Command that starts this agent again (restart).
    pub launch: Option<String>,
    /// Workflow state: queue, countdown, reviewer note.
    pub wf: crate::workflow::CardInfo,
}

impl PaneInfo {
    /// Can the dock answer this agent's prompt?
    pub fn answerable(&self) -> bool {
        self.prompt.as_ref().is_some_and(|p| !p.options.is_empty())
    }
}

pub type InfoMap = HashMap<usize, PaneInfo>;

// ───────────────────────────── keys & actions ─────────────────────────────

/// Keys the dock understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockKey {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Enter,
    /// Shift+Enter: a line break in the composer.
    ShiftEnter,
    Escape,
    Tab,
    Space,
    Backspace,
    Delete,
    /// Case is kept (`R` restarts, `r` replies).
    Char(char),
    /// Ctrl+letter (composer editing); other modes treat it as a chord.
    Ctrl(char),
    /// A modifier chord (Cmd/Alt/...): never ours.
    Chord,
}

/// What a key press asks the runtime to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Consumed, nothing else to do (selection moved, mode changed).
    None,
    /// Jump to this pane.
    Jump(usize),
    /// Cycle to the next agent that needs the user.
    NextAttention,
    /// Give the keyboard back to the terminal (dock stays open).
    Blur,
    /// Not a dock key: unfocus and let the terminal have it.
    PassThrough,
    /// Answer option `option` of the agent's approval prompt.
    Answer { uid: usize, option: usize },
    /// ESC to the agent's pane.
    Interrupt(usize),
    /// Ctrl+C to the agent's pane.
    CtrlC(usize),
    /// Type `text` and submit it in every target pane.
    Send { uids: Vec<usize>, text: String },
    Review(usize),
    Restart(usize),
    Close(usize),
    /// Open the task-queue editor for this agent.
    Queue(usize),
    /// Send the reviewer's feedback to the writer.
    Forward(usize),
    /// Stop the workflow loop this agent belongs to.
    StopWorkflow(usize),
    ToggleDensity,
    /// Autopilot on / off for this agent.
    ToggleAutopilot(usize),
    /// Autopilot on / off for every agent (the dock header switch).
    ToggleAutopilotAll,
    /// Offer to add a policy rule for the approval prompt on this card.
    AlwaysAllow(usize),
    /// Show a one-line message.
    Notice(String),
}

/// Items of the per-card context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    Jump,
    Interrupt,
    CtrlC,
    Reply,
    Review,
    Autopilot,
    AlwaysAllow,
    Mark,
    Restart,
    Close,
}

impl MenuItem {
    pub const ALL: [MenuItem; 10] = [
        MenuItem::Jump,
        MenuItem::Interrupt,
        MenuItem::CtrlC,
        MenuItem::Reply,
        MenuItem::Review,
        MenuItem::Autopilot,
        MenuItem::AlwaysAllow,
        MenuItem::Mark,
        MenuItem::Restart,
        MenuItem::Close,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MenuItem::Jump => "Go to pane",
            MenuItem::Interrupt => "Interrupt",
            MenuItem::CtrlC => "Send Ctrl+C",
            MenuItem::Reply => "Reply\u{2026}",
            MenuItem::Review => "Review changes",
            MenuItem::Autopilot => "Toggle autopilot",
            MenuItem::AlwaysAllow => "Always allow this\u{2026}",
            MenuItem::Mark => "Toggle broadcast mark",
            MenuItem::Restart => "Restart",
            MenuItem::Close => "Close pane",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            MenuItem::Jump => "Enter",
            MenuItem::Interrupt => "Esc",
            MenuItem::CtrlC => "^C",
            MenuItem::Reply => "r",
            MenuItem::Review => "v",
            MenuItem::Autopilot => "p",
            MenuItem::AlwaysAllow => "w",
            MenuItem::Mark => "Space",
            MenuItem::Restart => "R",
            MenuItem::Close => "x",
        }
    }
}

/// The dock's modal state.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Browse,
    /// Typing a message; `broadcast` = to every marked agent.
    Compose { broadcast: bool },
    /// "Send to N agents?" (text stays in the composer).
    ConfirmSend { targets: Vec<usize> },
    ConfirmClose(usize),
    ConfirmRestart(usize),
    /// A critical command: press the same option again.
    ConfirmRisk { uid: usize, option: usize },
    Menu { uid: usize, sel: usize },
}

/// What the key handler needs to know about the world.
pub trait Env {
    /// Session order in the dock.
    fn uids(&self) -> &[usize];
    fn live(&self, uid: usize) -> bool;
    /// Something an ESC would stop (working, waiting, starting).
    fn can_interrupt(&self, uid: usize) -> bool;
    /// Waiting on an approval prompt the dock can answer.
    fn answerable(&self, uid: usize) -> bool;
    /// Option the digit stands for in that agent's prompt.
    fn option_for_digit(&self, uid: usize, digit: u8) -> Option<usize>;
    /// Does answering `option` let the agent proceed (approve / always)?
    fn grants(&self, uid: usize, option: usize) -> bool;
    fn risk(&self, uid: usize) -> Risk;
    /// Restarting would interrupt work in progress.
    fn busy(&self, uid: usize) -> bool;
    fn now(&self) -> Instant;
}

// ───────────────────────────── composer ─────────────────────────────

/// Multi-line text field state (rendered on one line, breaks shown as `¶`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Composer {
    text: Vec<char>,
    cursor: usize,
}

impl Composer {
    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.text.iter().all(|c| c.is_whitespace())
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Insert at the cursor. `\r\n` and `\r` become `\n`; tabs become spaces;
    /// other control characters are dropped.
    pub fn insert_str(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        for c in s.chars() {
            let c = if c == '\t' { ' ' } else { c };
            if c.is_control() && c != '\n' {
                continue;
            }
            self.text.insert(self.cursor, c);
            self.cursor += 1;
        }
    }

    pub fn newline(&mut self) {
        self.insert_str("\n");
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.text.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.len());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    pub fn kill_to_start(&mut self) {
        self.text.drain(..self.cursor);
        self.cursor = 0;
    }

    /// Delete the word before the cursor.
    pub fn kill_word(&mut self) {
        let mut i = self.cursor;
        while i > 0 && self.text[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !self.text[i - 1].is_whitespace() {
            i -= 1;
        }
        self.text.drain(i..self.cursor);
        self.cursor = i;
    }

    /// Replace the whole text; the cursor goes to the end.
    pub fn set_text(&mut self, s: &str) {
        self.clear();
        self.insert_str(s);
    }

    /// (line, column) of the cursor, counting `\n`-separated lines.
    pub fn line_col(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor.min(self.text.len())];
        let line = before.iter().filter(|c| **c == '\n').count();
        let col = before.iter().rev().take_while(|c| **c != '\n').count();
        (line, col)
    }

    /// Move the cursor one line up, keeping the column where possible.
    pub fn up(&mut self) {
        let (line, col) = self.line_col();
        if line == 0 {
            self.cursor = 0;
            return;
        }
        let starts = self.line_starts();
        let prev_len = starts[line] - starts[line - 1] - 1;
        self.cursor = starts[line - 1] + col.min(prev_len);
    }

    /// Move the cursor one line down, keeping the column where possible.
    pub fn down(&mut self) {
        let (line, col) = self.line_col();
        let starts = self.line_starts();
        if line + 1 >= starts.len() {
            self.cursor = self.text.len();
            return;
        }
        let next_end = starts.get(line + 2).map_or(self.text.len(), |s| s - 1);
        let next_len = next_end - starts[line + 1];
        self.cursor = starts[line + 1] + col.min(next_len);
    }

    /// Cursor to the start of its line.
    pub fn line_home(&mut self) {
        let (_, col) = self.line_col();
        self.cursor -= col;
    }

    /// Cursor to the end of its line.
    pub fn line_end(&mut self) {
        while self.cursor < self.text.len() && self.text[self.cursor] != '\n' {
            self.cursor += 1;
        }
    }

    /// Char index where each line starts.
    fn line_starts(&self) -> Vec<usize> {
        let mut v = vec![0];
        for (i, c) in self.text.iter().enumerate() {
            if *c == '\n' {
                v.push(i + 1);
            }
        }
        v
    }

    /// The single-line rendering and the cursor column in it.
    pub fn display(&self) -> (String, usize) {
        let mut s = String::new();
        let mut col = 0;
        for (i, c) in self.text.iter().enumerate() {
            if i == self.cursor {
                col = s.chars().count();
            }
            if *c == '\n' {
                s.push('\u{b6}');
            } else {
                s.push(*c);
            }
        }
        if self.cursor >= self.text.len() {
            col = s.chars().count();
        }
        (s, col)
    }
}

// ───────────────────────────── targeting ─────────────────────────────

/// Who a broadcast reaches: (targets, skipped). Agents that are gone or
/// finished are skipped, and so are agents waiting on an approval menu, where
/// typed text would act as menu keys (a "1" in a message would approve).
pub fn broadcast_targets(marked: &[usize], env: &dyn Env) -> (Vec<usize>, Vec<usize>) {
    let mut targets = Vec::new();
    let mut skipped = Vec::new();
    for &uid in marked {
        if env.live(uid) && !env.answerable(uid) {
            if !targets.contains(&uid) {
                targets.push(uid);
            }
        } else {
            skipped.push(uid);
        }
    }
    (targets, skipped)
}

// ───────────────────────────── the machine ─────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct Control {
    pub mode: Mode,
    /// Cards marked for broadcast, in marking order.
    pub marked: Vec<usize>,
    pub composer: Composer,
    esc_armed: Option<Instant>,
}

impl Control {
    pub fn composing(&self) -> bool {
        matches!(self.mode, Mode::Compose { .. } | Mode::ConfirmSend { .. })
    }

    pub fn toggle_mark(&mut self, uid: usize) {
        match self.marked.iter().position(|u| *u == uid) {
            Some(i) => {
                self.marked.remove(i);
            }
            None => self.marked.push(uid),
        }
    }

    pub fn is_marked(&self, uid: usize) -> bool {
        self.marked.contains(&uid)
    }

    /// Forget marks of agents that no longer exist; leave a mode whose target vanished.
    pub fn prune(&mut self, uids: &[usize]) {
        self.marked.retain(|u| uids.contains(u));
        let gone = match &self.mode {
            Mode::ConfirmClose(u) | Mode::ConfirmRestart(u) | Mode::ConfirmRisk { uid: u, .. } | Mode::Menu { uid: u, .. } => !uids.contains(u),
            _ => false,
        };
        if gone {
            self.mode = Mode::Browse;
        }
    }

    /// Text typed by an IME commit or a paste.
    pub fn insert_text(&mut self, text: &str) -> bool {
        if matches!(self.mode, Mode::Compose { .. }) {
            self.composer.insert_str(text);
            true
        } else {
            false
        }
    }

    /// Open the composer for `uid` (single) or for the marked cards.
    pub fn start_compose(&mut self, broadcast: bool) {
        self.mode = Mode::Compose { broadcast };
        self.esc_armed = None;
    }

    pub fn open_menu(&mut self, uid: usize) {
        self.mode = Mode::Menu { uid, sel: 0 };
    }

    fn selected(&self, sel: Option<usize>, env: &dyn Env) -> Option<usize> {
        sel.filter(|u| env.uids().contains(u)).or_else(|| env.uids().first().copied())
    }

    /// Handle one key. `selected` is the card the selection rests on; the
    /// caller applies `Action::None` after moving it (see [`Control::step`]).
    pub fn on_key(&mut self, key: DockKey, selected: &mut Option<usize>, env: &dyn Env) -> Action {
        let now = env.now();
        let armed = self.esc_armed.take().filter(|t| now.saturating_duration_since(*t) <= ESC_WINDOW);
        let mode = std::mem::take(&mut self.mode);
        match mode {
            Mode::Browse => self.browse(key, selected, env, armed),
            Mode::Compose { broadcast } => self.compose(key, broadcast, selected, env),
            Mode::ConfirmSend { targets } => {
                let ok = matches!(key, DockKey::Enter | DockKey::Char('y') | DockKey::Char('Y'));
                if ok {
                    let text = self.composer.text();
                    self.composer.clear();
                    Action::Send { uids: targets, text }
                } else {
                    self.mode = Mode::Compose { broadcast: true };
                    Action::None
                }
            }
            Mode::ConfirmClose(uid) => match key {
                DockKey::Enter | DockKey::Char('y') | DockKey::Char('Y') | DockKey::Char('x') => Action::Close(uid),
                _ => Action::None,
            },
            Mode::ConfirmRestart(uid) => match key {
                DockKey::Enter | DockKey::Char('y') | DockKey::Char('Y') | DockKey::Char('R') => Action::Restart(uid),
                _ => Action::None,
            },
            Mode::ConfirmRisk { uid, option } => {
                let again = match key {
                    DockKey::Enter | DockKey::Char('y') | DockKey::Char('Y') => true,
                    DockKey::Char(c) if c.is_ascii_digit() => env.option_for_digit(uid, c as u8 - b'0') == Some(option),
                    _ => false,
                };
                if again {
                    Action::Answer { uid, option }
                } else {
                    Action::None
                }
            }
            Mode::Menu { uid, sel } => self.menu(key, uid, sel, selected, env),
        }
    }

    fn browse(&mut self, key: DockKey, selected: &mut Option<usize>, env: &dyn Env, armed: Option<Instant>) -> Action {
        let uids = env.uids();
        let n = uids.len();
        let cur = selected.and_then(|s| uids.iter().position(|u| *u == s)).or(if n > 0 { Some(0) } else { None });
        let mut pick = |i: usize| *selected = uids.get(i).copied();
        let sel_uid = cur.map(|i| uids[i]);
        match key {
            DockKey::Chord | DockKey::Ctrl(_) => Action::PassThrough,
            DockKey::Escape => {
                match sel_uid {
                    Some(uid) if armed.is_none() && env.can_interrupt(uid) => {
                        self.esc_armed = Some(env.now());
                        Action::Interrupt(uid)
                    }
                    _ => Action::Blur,
                }
            }
            DockKey::Up | DockKey::Char('k') => {
                if let Some(i) = cur {
                    pick(if i == 0 { n - 1 } else { i - 1 });
                }
                Action::None
            }
            DockKey::Down | DockKey::Char('j') => {
                if let Some(i) = cur {
                    pick((i + 1) % n);
                }
                Action::None
            }
            DockKey::Home => {
                pick(0);
                Action::None
            }
            DockKey::End => {
                if n > 0 {
                    pick(n - 1);
                }
                Action::None
            }
            DockKey::Enter => sel_uid.map_or(Action::None, Action::Jump),
            DockKey::Tab => Action::ToggleDensity,
            DockKey::Space => {
                if let Some(u) = sel_uid {
                    self.toggle_mark(u);
                }
                Action::None
            }
            DockKey::Char('n') => Action::NextAttention,
            DockKey::Char(c) if c.is_ascii_digit() && c != '0' => {
                let d = c as u8 - b'0';
                // An answerable approval prompt owns the digits; otherwise they jump.
                if let Some(uid) = sel_uid.filter(|u| env.answerable(*u)) {
                    return match env.option_for_digit(uid, d) {
                        Some(option) => self.answer(uid, option, env),
                        None => Action::Notice(format!("No option {d} in this prompt")),
                    };
                }
                let i = d as usize - 1;
                if i < n {
                    Action::Jump(uids[i])
                } else {
                    Action::None
                }
            }
            DockKey::Char('r') => match sel_uid {
                Some(uid) if env.answerable(uid) => Action::Notice("Answer the approval first (1 / 2 / 3)".into()),
                Some(uid) if !env.live(uid) => Action::Notice("That agent has finished: Restart it with Shift+R".into()),
                Some(_) => {
                    self.start_compose(false);
                    Action::None
                }
                None => Action::None,
            },
            DockKey::Char('b') => {
                if self.marked.is_empty() {
                    self.marked = uids.iter().copied().filter(|u| env.live(*u)).collect();
                }
                let (targets, _) = broadcast_targets(&self.marked, env);
                if targets.is_empty() {
                    Action::Notice("No agent can take a message right now".into())
                } else {
                    self.start_compose(true);
                    Action::None
                }
            }
            DockKey::Char('v') => sel_uid.map_or(Action::None, Action::Review),
            DockKey::Char('p') => sel_uid.map_or(Action::None, Action::ToggleAutopilot),
            DockKey::Char('P') => Action::ToggleAutopilotAll,
            DockKey::Char('w') => match sel_uid {
                Some(uid) if env.answerable(uid) => Action::AlwaysAllow(uid),
                Some(_) => Action::Notice("\"Always allow\" needs an approval prompt on the card".into()),
                None => Action::None,
            },
            DockKey::Char('t') => sel_uid.map_or(Action::None, Action::Queue),
            DockKey::Char('f') => sel_uid.map_or(Action::None, Action::Forward),
            DockKey::Char('S') => sel_uid.map_or(Action::None, Action::StopWorkflow),
            DockKey::Char('x') => match sel_uid {
                Some(uid) => {
                    self.mode = Mode::ConfirmClose(uid);
                    Action::None
                }
                None => Action::None,
            },
            DockKey::Char('R') => match sel_uid {
                Some(uid) if env.busy(uid) => {
                    self.mode = Mode::ConfirmRestart(uid);
                    Action::None
                }
                Some(uid) => Action::Restart(uid),
                None => Action::None,
            },
            DockKey::Char('m') => {
                if let Some(u) = sel_uid {
                    self.open_menu(u);
                }
                Action::None
            }
            DockKey::Char('a') => {
                // Mark every live agent (then `b` / `r` to type once).
                self.marked = uids.iter().copied().filter(|u| env.live(*u)).collect();
                Action::None
            }
            DockKey::Char('c') => {
                if self.marked.is_empty() {
                    return Action::None;
                }
                self.marked.clear();
                Action::None
            }
            _ => Action::PassThrough,
        }
    }

    /// Approve / always need a clean command check; critical ones a second press.
    fn answer(&mut self, uid: usize, option: usize, env: &dyn Env) -> Action {
        if !env.grants(uid, option) {
            return Action::Answer { uid, option };
        }
        match env.risk(uid) {
            Risk::Pending => Action::Notice("Checking the command first\u{2026}".into()),
            Risk::Critical(_) => {
                self.mode = Mode::ConfirmRisk { uid, option };
                Action::None
            }
            _ => Action::Answer { uid, option },
        }
    }

    fn compose(&mut self, key: DockKey, broadcast: bool, selected: &mut Option<usize>, env: &dyn Env) -> Action {
        self.mode = Mode::Compose { broadcast };
        match key {
            DockKey::Escape => {
                self.mode = Mode::Browse;
                self.composer.clear();
                Action::None
            }
            DockKey::Enter => {
                if self.composer.is_empty() {
                    return Action::None;
                }
                let text = self.composer.text();
                let text = text.trim_end().to_string();
                if broadcast {
                    let (targets, skipped) = broadcast_targets(&self.marked, env);
                    if targets.is_empty() {
                        return Action::Notice("Nobody to send to".into());
                    }
                    let _ = skipped;
                    if targets.len() > 1 {
                        self.mode = Mode::ConfirmSend { targets };
                        return Action::None;
                    }
                    self.mode = Mode::Browse;
                    self.composer.clear();
                    return Action::Send { uids: targets, text };
                }
                match self.selected(*selected, env) {
                    Some(uid) if env.live(uid) && !env.answerable(uid) => {
                        self.mode = Mode::Browse;
                        self.composer.clear();
                        Action::Send { uids: vec![uid], text }
                    }
                    Some(_) => Action::Notice("That agent cannot take text right now".into()),
                    None => Action::None,
                }
            }
            DockKey::ShiftEnter => {
                self.composer.newline();
                Action::None
            }
            DockKey::Backspace => {
                self.composer.backspace();
                Action::None
            }
            DockKey::Delete => {
                self.composer.delete();
                Action::None
            }
            DockKey::Left => {
                self.composer.left();
                Action::None
            }
            DockKey::Right => {
                self.composer.right();
                Action::None
            }
            DockKey::Home | DockKey::Up => {
                self.composer.home();
                Action::None
            }
            DockKey::End | DockKey::Down => {
                self.composer.end();
                Action::None
            }
            DockKey::Tab => Action::None,
            DockKey::Space => {
                self.composer.insert_str(" ");
                Action::None
            }
            DockKey::Char(c) => {
                self.composer.insert_str(&c.to_string());
                Action::None
            }
            DockKey::Ctrl(c) => {
                match c {
                    'u' => self.composer.kill_to_start(),
                    'w' => self.composer.kill_word(),
                    'a' => self.composer.home(),
                    'e' => self.composer.end(),
                    _ => {}
                }
                Action::None
            }
            DockKey::Chord => Action::PassThrough,
        }
    }

    fn menu(&mut self, key: DockKey, uid: usize, sel: usize, selected: &mut Option<usize>, env: &dyn Env) -> Action {
        let n = MenuItem::ALL.len();
        match key {
            DockKey::Escape => Action::None,
            DockKey::Up | DockKey::Char('k') => {
                self.mode = Mode::Menu { uid, sel: if sel == 0 { n - 1 } else { sel - 1 } };
                Action::None
            }
            DockKey::Down | DockKey::Char('j') => {
                self.mode = Mode::Menu { uid, sel: (sel + 1) % n };
                Action::None
            }
            DockKey::Enter => self.run_item(MenuItem::ALL[sel.min(n - 1)], uid, selected, env),
            _ => {
                self.mode = Mode::Menu { uid, sel };
                Action::None
            }
        }
    }

    /// Run a context-menu item on `uid` (also used by mouse clicks on the menu).
    pub fn run_item(&mut self, item: MenuItem, uid: usize, selected: &mut Option<usize>, env: &dyn Env) -> Action {
        self.mode = Mode::Browse;
        *selected = Some(uid);
        match item {
            MenuItem::Jump => Action::Jump(uid),
            MenuItem::Interrupt if env.live(uid) => Action::Interrupt(uid),
            MenuItem::CtrlC if env.live(uid) => Action::CtrlC(uid),
            MenuItem::Interrupt | MenuItem::CtrlC => Action::Notice("That agent has finished".into()),
            MenuItem::Reply => {
                if env.answerable(uid) {
                    Action::Notice("Answer the approval first (1 / 2 / 3)".into())
                } else if env.live(uid) {
                    self.start_compose(false);
                    Action::None
                } else {
                    Action::Notice("That agent has finished".into())
                }
            }
            MenuItem::Review => Action::Review(uid),
            MenuItem::Autopilot => Action::ToggleAutopilot(uid),
            MenuItem::AlwaysAllow if env.answerable(uid) => Action::AlwaysAllow(uid),
            MenuItem::AlwaysAllow => Action::Notice("\"Always allow\" needs an approval prompt on the card".into()),
            MenuItem::Mark => {
                self.toggle_mark(uid);
                Action::None
            }
            MenuItem::Restart => {
                if env.busy(uid) {
                    self.mode = Mode::ConfirmRestart(uid);
                    Action::None
                } else {
                    Action::Restart(uid)
                }
            }
            MenuItem::Close => {
                self.mode = Mode::ConfirmClose(uid);
                Action::None
            }
        }
    }

    /// Answer `option` by mouse (same safety rules as the digit keys).
    pub fn click_answer(&mut self, uid: usize, option: usize, env: &dyn Env) -> Action {
        self.answer(uid, option, env)
    }

    /// Footer hints for the current mode: (key, label).
    pub fn hints(&self, waiting_selected: bool) -> Vec<(&'static str, &'static str)> {
        match &self.mode {
            Mode::Browse if waiting_selected => vec![("1-3", "answer"), ("w", "allow rule"), ("p", "autopilot"), ("Esc", "interrupt"), ("r", "reply"), ("v", "review"), ("Enter", "jump")],
            Mode::Browse => vec![("Enter", "jump"), ("r", "reply"), ("p", "autopilot"), ("b", "broadcast"), ("v", "review"), ("t", "queue"), ("Esc", "interrupt"), ("x", "close")],
            Mode::Compose { .. } => vec![("Enter", "send"), ("Esc", "cancel"), ("S-Enter", "newline")],
            Mode::ConfirmSend { .. } | Mode::ConfirmClose(_) | Mode::ConfirmRestart(_) | Mode::ConfirmRisk { .. } => vec![("Enter", "confirm"), ("Esc", "cancel")],
            Mode::Menu { .. } => vec![("Enter", "run"), ("Esc", "close")],
        }
    }

    /// Kind of the card hint, used by the empty state (nothing to select).
    pub fn is_browsing(&self) -> bool {
        self.mode == Mode::Browse
    }
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct World {
        uids: Vec<usize>,
        live: Vec<usize>,
        /// uid -> (digit->option map, grants all, risk)
        prompts: HashMap<usize, (Vec<u8>, Risk)>,
        busy: Vec<usize>,
        /// Live but not doing anything (nothing to interrupt).
        idle: Vec<usize>,
        now: Instant,
    }

    impl World {
        fn new(uids: &[usize]) -> Self {
            World { uids: uids.to_vec(), live: uids.to_vec(), prompts: HashMap::new(), busy: vec![], idle: vec![], now: Instant::now() }
        }
        fn with_prompt(mut self, uid: usize, risk: Risk) -> Self {
            // Claude style: options numbered 1..=3 = approve, always, deny.
            self.prompts.insert(uid, (vec![1, 2, 3], risk));
            self
        }
    }

    impl Env for World {
        fn uids(&self) -> &[usize] {
            &self.uids
        }
        fn live(&self, uid: usize) -> bool {
            self.live.contains(&uid)
        }
        fn can_interrupt(&self, uid: usize) -> bool {
            self.live.contains(&uid) && !self.idle.contains(&uid)
        }
        fn answerable(&self, uid: usize) -> bool {
            self.prompts.contains_key(&uid)
        }
        fn option_for_digit(&self, uid: usize, d: u8) -> Option<usize> {
            self.prompts.get(&uid)?.0.iter().position(|x| *x == d)
        }
        fn grants(&self, _uid: usize, option: usize) -> bool {
            option < 2
        }
        fn risk(&self, uid: usize) -> Risk {
            self.prompts.get(&uid).map(|p| p.1.clone()).unwrap_or_default()
        }
        fn busy(&self, uid: usize) -> bool {
            self.busy.contains(&uid)
        }
        fn now(&self) -> Instant {
            self.now
        }
    }

    fn press(c: &mut Control, sel: &mut Option<usize>, w: &World, keys: &[DockKey]) -> Action {
        let mut last = Action::None;
        for k in keys {
            last = c.on_key(*k, sel, w);
        }
        last
    }

    #[test]
    fn browsing_wraps_and_jumps() {
        let w = World::new(&[10, 20, 30]);
        let (mut c, mut sel) = (Control::default(), None);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Down]), Action::None);
        assert_eq!(sel, Some(20));
        press(&mut c, &mut sel, &w, &[DockKey::Char('j'), DockKey::Down]);
        assert_eq!(sel, Some(10), "wraps");
        press(&mut c, &mut sel, &w, &[DockKey::Up]);
        assert_eq!(sel, Some(30));
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Enter]), Action::Jump(30));
        press(&mut c, &mut sel, &w, &[DockKey::Home]);
        assert_eq!(sel, Some(10));
        press(&mut c, &mut sel, &w, &[DockKey::End]);
        assert_eq!(sel, Some(30));
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('2')]), Action::Jump(20), "no prompt: digits jump");
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('9')]), Action::None);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('n')]), Action::NextAttention);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('z')]), Action::PassThrough);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Chord]), Action::PassThrough);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Tab]), Action::ToggleDensity);
    }

    #[test]
    fn empty_dock_is_safe() {
        let w = World::new(&[]);
        let (mut c, mut sel) = (Control::default(), None);
        for k in [DockKey::Up, DockKey::Down, DockKey::Home, DockKey::End, DockKey::Enter, DockKey::Char('x'), DockKey::Char('r'), DockKey::Char('v'), DockKey::Space, DockKey::Char('R')] {
            let a = press(&mut c, &mut sel, &w, &[k]);
            assert!(matches!(a, Action::None | Action::PassThrough), "{k:?} -> {a:?}");
        }
        assert_eq!(sel, None);
        assert!(c.is_browsing());
    }

    #[test]
    fn digits_answer_the_selected_agents_prompt() {
        let w = World::new(&[1, 2]).with_prompt(2, Risk::Safe);
        let (mut c, mut sel) = (Control::default(), Some(2));
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('1')]), Action::Answer { uid: 2, option: 0 });
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('2')]), Action::Answer { uid: 2, option: 1 });
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('3')]), Action::Answer { uid: 2, option: 2 });
        assert!(matches!(press(&mut c, &mut sel, &w, &[DockKey::Char('4')]), Action::Notice(_)));
        // The other agent has no prompt: digits still jump.
        sel = Some(1);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('2')]), Action::Jump(2));
    }

    #[test]
    fn pending_risk_blocks_approval_but_not_denial() {
        let w = World::new(&[1]).with_prompt(1, Risk::Pending);
        let (mut c, mut sel) = (Control::default(), Some(1));
        assert!(matches!(press(&mut c, &mut sel, &w, &[DockKey::Char('1')]), Action::Notice(_)));
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('3')]), Action::Answer { uid: 1, option: 2 });
    }

    #[test]
    fn risky_answers_at_once_critical_needs_a_second_press() {
        let w = World::new(&[1]).with_prompt(1, Risk::Risky(vec!["deletes files".into()]));
        let (mut c, mut sel) = (Control::default(), Some(1));
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('1')]), Action::Answer { uid: 1, option: 0 }, "warning level: badge only");

        let w = World::new(&[1]).with_prompt(1, Risk::Critical(vec!["rm -rf /".into()]));
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('1')]), Action::None);
        assert_eq!(c.mode, Mode::ConfirmRisk { uid: 1, option: 0 });
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('1')]), Action::Answer { uid: 1, option: 0 }, "same key again confirms");

        // A different key cancels without answering.
        press(&mut c, &mut sel, &w, &[DockKey::Char('1')]);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('2')]), Action::None);
        assert!(c.is_browsing());
        // Enter confirms too.
        press(&mut c, &mut sel, &w, &[DockKey::Char('2')]);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Enter]), Action::Answer { uid: 1, option: 1 });
        // Denying a critical command never asks.
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('3')]), Action::Answer { uid: 1, option: 2 });
    }

    #[test]
    fn escape_interrupts_then_second_escape_unfocuses() {
        let mut w = World::new(&[1, 2]);
        let (mut c, mut sel) = (Control::default(), Some(2));
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Escape]), Action::Interrupt(2));
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Escape]), Action::Blur);
        // Any other key disarms: the next Esc interrupts again.
        press(&mut c, &mut sel, &w, &[DockKey::Escape]);
        press(&mut c, &mut sel, &w, &[DockKey::Char('j')]);
        assert!(matches!(press(&mut c, &mut sel, &w, &[DockKey::Escape]), Action::Interrupt(_)));
        // The window expires.
        w.now += ESC_WINDOW + Duration::from_secs(1);
        let t0 = w.now;
        press(&mut c, &mut sel, &w, &[DockKey::Char('j')]);
        press(&mut c, &mut sel, &w, &[DockKey::Escape]);
        w.now = t0 + ESC_WINDOW + Duration::from_secs(1);
        assert!(matches!(press(&mut c, &mut sel, &w, &[DockKey::Escape]), Action::Interrupt(_)), "stale arm does not count");
        // An idle agent has nothing to interrupt: Esc hands the keyboard back at once.
        let mut c = Control::default();
        w.idle.push(2);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Escape]), Action::Blur);
        // So does a finished one.
        w.idle.clear();
        w.live.clear();
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Escape]), Action::Blur);
    }

    #[test]
    fn reply_composes_and_sends_to_the_selected_agent() {
        let w = World::new(&[1, 2]);
        let (mut c, mut sel) = (Control::default(), Some(2));
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('r')]), Action::None);
        assert_eq!(c.mode, Mode::Compose { broadcast: false });
        // j / r / digits are text in the composer.
        for ch in "fix 1 test".chars() {
            press(&mut c, &mut sel, &w, &[if ch == ' ' { DockKey::Space } else { DockKey::Char(ch) }]);
        }
        assert_eq!(c.composer.text(), "fix 1 test");
        press(&mut c, &mut sel, &w, &[DockKey::ShiftEnter]);
        press(&mut c, &mut sel, &w, &[DockKey::Char('x')]);
        assert_eq!(c.composer.text(), "fix 1 test\nx");
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Enter]), Action::Send { uids: vec![2], text: "fix 1 test\nx".into() });
        assert!(c.is_browsing() && c.composer.is_empty());
    }

    #[test]
    fn composer_escape_cancels_and_empty_enter_is_ignored() {
        let w = World::new(&[1]);
        let (mut c, mut sel) = (Control::default(), Some(1));
        press(&mut c, &mut sel, &w, &[DockKey::Char('r')]);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Enter]), Action::None);
        assert!(c.composing());
        press(&mut c, &mut sel, &w, &[DockKey::Char('h'), DockKey::Char('i')]);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Escape]), Action::None);
        assert!(c.is_browsing());
        assert!(c.composer.is_empty(), "cancelled text is dropped");
    }

    #[test]
    fn reply_is_refused_while_a_prompt_is_open_or_after_exit() {
        let mut w = World::new(&[1, 2]).with_prompt(1, Risk::Safe);
        let (mut c, mut sel) = (Control::default(), Some(1));
        assert!(matches!(press(&mut c, &mut sel, &w, &[DockKey::Char('r')]), Action::Notice(_)));
        assert!(c.is_browsing());
        w.live.retain(|u| *u != 2);
        sel = Some(2);
        assert!(matches!(press(&mut c, &mut sel, &w, &[DockKey::Char('r')]), Action::Notice(_)));
    }

    #[test]
    fn broadcast_targeting_skips_prompts_and_finished_agents() {
        let mut w = World::new(&[1, 2, 3, 4]).with_prompt(2, Risk::Safe);
        w.live.retain(|u| *u != 4);
        let (t, s) = broadcast_targets(&[1, 2, 3, 4, 1], &w);
        assert_eq!(t, vec![1, 3], "deduplicated, waiting and finished agents left out");
        assert_eq!(s, vec![2, 4]);
        assert_eq!(broadcast_targets(&[], &w), (vec![], vec![]));
    }

    #[test]
    fn broadcast_marks_confirm_and_send() {
        let w = World::new(&[1, 2, 3]);
        let (mut c, mut sel) = (Control::default(), Some(1));
        press(&mut c, &mut sel, &w, &[DockKey::Space, DockKey::Down, DockKey::Space]);
        assert_eq!(c.marked, vec![1, 2]);
        press(&mut c, &mut sel, &w, &[DockKey::Space]);
        assert_eq!(c.marked, vec![1], "space toggles");
        press(&mut c, &mut sel, &w, &[DockKey::Space]);
        assert_eq!(c.marked, vec![1, 2]);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('b')]), Action::None);
        assert_eq!(c.mode, Mode::Compose { broadcast: true });
        press(&mut c, &mut sel, &w, &[DockKey::Char('g'), DockKey::Char('o')]);
        // More than one target: confirm first.
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Enter]), Action::None);
        assert_eq!(c.mode, Mode::ConfirmSend { targets: vec![1, 2] });
        // Esc goes back to editing, text intact.
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Escape]), Action::None);
        assert_eq!(c.mode, Mode::Compose { broadcast: true });
        assert_eq!(c.composer.text(), "go");
        press(&mut c, &mut sel, &w, &[DockKey::Enter]);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Enter]), Action::Send { uids: vec![1, 2], text: "go".into() });
        assert!(c.is_browsing());
    }

    #[test]
    fn a_single_broadcast_target_sends_without_confirmation() {
        let w = World::new(&[1, 2]);
        let (mut c, mut sel) = (Control::default(), Some(1));
        press(&mut c, &mut sel, &w, &[DockKey::Space, DockKey::Char('b'), DockKey::Char('o'), DockKey::Char('k')]);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Enter]), Action::Send { uids: vec![1], text: "ok".into() });
    }

    #[test]
    fn b_without_marks_targets_every_live_agent() {
        let w = World::new(&[1, 2]);
        let (mut c, mut sel) = (Control::default(), Some(1));
        press(&mut c, &mut sel, &w, &[DockKey::Char('b')]);
        assert_eq!(c.marked, vec![1, 2]);
        assert!(c.composing());
        let w = World::new(&[]);
        let (mut c, mut sel) = (Control::default(), None);
        assert!(matches!(press(&mut c, &mut sel, &w, &[DockKey::Char('b')]), Action::Notice(_)));
    }

    #[test]
    fn close_and_restart_confirm() {
        let mut w = World::new(&[1, 2]);
        let (mut c, mut sel) = (Control::default(), Some(2));
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('x')]), Action::None);
        assert_eq!(c.mode, Mode::ConfirmClose(2));
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Escape]), Action::None, "Esc cancels");
        assert!(c.is_browsing());
        press(&mut c, &mut sel, &w, &[DockKey::Char('x')]);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Enter]), Action::Close(2));
        // Restart: an idle agent restarts at once, a busy one asks first.
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('R')]), Action::Restart(2));
        w.busy.push(2);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('R')]), Action::None);
        assert_eq!(c.mode, Mode::ConfirmRestart(2));
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('y')]), Action::Restart(2));
    }

    #[test]
    fn autopilot_keys_and_menu_items() {
        let w = World::new(&[5, 6]).with_prompt(6, Risk::Safe);
        let (mut c, mut sel) = (Control::default(), Some(5));
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('p')]), Action::ToggleAutopilot(5));
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('P')]), Action::ToggleAutopilotAll);
        // "Always allow" needs an approval prompt on the selected card.
        assert!(matches!(press(&mut c, &mut sel, &w, &[DockKey::Char('w')]), Action::Notice(_)));
        sel = Some(6);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('w')]), Action::AlwaysAllow(6));
        assert_eq!(c.run_item(MenuItem::Autopilot, 5, &mut sel, &w), Action::ToggleAutopilot(5));
        assert_eq!(c.run_item(MenuItem::AlwaysAllow, 6, &mut sel, &w), Action::AlwaysAllow(6));
        assert!(matches!(c.run_item(MenuItem::AlwaysAllow, 5, &mut sel, &w), Action::Notice(_)));
        // The footer advertises them.
        assert!(c.hints(false).iter().any(|h| h.0 == "p" && h.1 == "autopilot"));
        assert!(c.hints(true).iter().any(|h| h.0 == "w"));
        // The context menu keeps Close last (the wrap-around test relies on it).
        assert_eq!(MenuItem::ALL.last(), Some(&MenuItem::Close));
    }

    #[test]
    fn review_key() {
        let w = World::new(&[5]);
        let (mut c, mut sel) = (Control::default(), None);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Char('v')]), Action::Review(5));
    }

    #[test]
    fn context_menu_navigation_and_items() {
        let w = World::new(&[1, 2]);
        let (mut c, mut sel) = (Control::default(), Some(1));
        press(&mut c, &mut sel, &w, &[DockKey::Char('m')]);
        assert_eq!(c.mode, Mode::Menu { uid: 1, sel: 0 });
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Enter]), Action::Jump(1));
        assert!(c.is_browsing());
        press(&mut c, &mut sel, &w, &[DockKey::Char('m'), DockKey::Down]);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Enter]), Action::Interrupt(1));
        press(&mut c, &mut sel, &w, &[DockKey::Char('m'), DockKey::Down, DockKey::Down]);
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Enter]), Action::CtrlC(1));
        press(&mut c, &mut sel, &w, &[DockKey::Char('m'), DockKey::Up]);
        assert_eq!(c.mode, Mode::Menu { uid: 1, sel: MenuItem::ALL.len() - 1 }, "wraps");
        assert_eq!(press(&mut c, &mut sel, &w, &[DockKey::Enter]), Action::None);
        assert_eq!(c.mode, Mode::ConfirmClose(1), "close asks");
        press(&mut c, &mut sel, &w, &[DockKey::Escape]);
        press(&mut c, &mut sel, &w, &[DockKey::Char('m'), DockKey::Escape]);
        assert!(c.is_browsing());
        // Direct item run (mouse) selects the card and may mark it.
        let a = c.run_item(MenuItem::Mark, 2, &mut sel, &w);
        assert_eq!((a, sel, c.marked.clone()), (Action::None, Some(2), vec![2]));
        assert_eq!(c.run_item(MenuItem::Review, 2, &mut sel, &w), Action::Review(2));
    }

    #[test]
    fn composer_editing() {
        let mut e = Composer::default();
        e.insert_str("hello wor\tld\u{7}");
        assert_eq!(e.text(), "hello wor ld");
        e.kill_word();
        assert_eq!(e.text(), "hello wor ");
        e.kill_word();
        assert_eq!(e.text(), "hello ");
        e.home();
        e.insert_str("oh ");
        assert_eq!(e.text(), "oh hello ");
        e.end();
        e.backspace();
        e.left();
        e.delete();
        assert_eq!(e.text(), "oh hell");
        e.kill_to_start();
        assert_eq!(e.text(), "");
        e.insert_str("a\r\nb\rc");
        assert_eq!(e.text(), "a\nb\nc", "line breaks normalised");
        assert_eq!(e.display(), ("a\u{b6}b\u{b6}c".to_string(), 5));
        e.home();
        assert_eq!(e.display().1, 0);
        let mut w = Composer::default();
        w.insert_str("你好");
        w.left();
        w.backspace();
        assert_eq!(w.text(), "好");
        assert!(Composer::default().is_empty());
        let mut sp = Composer::default();
        sp.insert_str("  \n ");
        assert!(sp.is_empty(), "whitespace only does not send");
    }

    #[test]
    fn ime_commit_goes_to_the_composer_only_while_composing() {
        let w = World::new(&[1]);
        let (mut c, mut sel) = (Control::default(), Some(1));
        assert!(!c.insert_text("你好"));
        press(&mut c, &mut sel, &w, &[DockKey::Char('r')]);
        assert!(c.insert_text("你好"));
        assert_eq!(c.composer.text(), "你好");
    }

    #[test]
    fn prune_drops_stale_marks_and_modes() {
        let mut c = Control::default();
        c.marked = vec![1, 2, 3];
        c.mode = Mode::ConfirmClose(2);
        c.prune(&[1, 3]);
        assert_eq!(c.marked, vec![1, 3]);
        assert!(c.is_browsing());
        c.mode = Mode::Compose { broadcast: true };
        c.prune(&[1]);
        assert!(c.composing(), "composing survives");
    }

    #[test]
    fn hints_follow_the_mode() {
        let mut c = Control::default();
        assert!(c.hints(true).iter().any(|h| h.0 == "1-3"));
        assert!(c.hints(false).iter().any(|h| h.1 == "broadcast"));
        c.mode = Mode::Compose { broadcast: false };
        assert!(c.hints(false).iter().any(|h| h.1 == "send"));
    }

    #[test]
    fn composer_moves_between_lines() {
        let mut c = Composer::default();
        c.insert_str("abc\nde\nfghij");
        assert_eq!(c.line_col(), (2, 5));
        c.up();
        assert_eq!(c.line_col(), (1, 2), "column clamps to the shorter line");
        c.up();
        assert_eq!(c.line_col(), (0, 2));
        c.up();
        assert_eq!(c.cursor(), 0, "up on the first line goes home");
        c.down();
        assert_eq!(c.line_col(), (1, 0));
        c.down();
        assert_eq!(c.line_col(), (2, 0));
        c.down();
        assert_eq!(c.cursor(), 12, "down on the last line goes to the end");
        c.line_home();
        assert_eq!(c.line_col(), (2, 0));
        c.line_end();
        assert_eq!(c.line_col(), (2, 5));
        c.up();
        c.line_end();
        assert_eq!(c.line_col(), (1, 2));
        c.up();
        c.line_home();
        assert_eq!(c.line_col(), (0, 0));
        c.set_text("x\ny");
        assert_eq!(c.text(), "x\ny");
        assert_eq!(c.line_col(), (1, 1));
    }
}
