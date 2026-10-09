//! Workflow overlays: the start wizard, the task-queue editor and the
//! best-of-N compare view. State and key handling are pure; drawing goes
//! through the UI kit (`Tokens` / `Ctx`), like every other panel.

use std::time::Duration;

use super::bestof::{CandState, TestStatus};
use super::queue::{self, TaskQueue};
use super::template::{Strategy, Template, MAX_CANDIDATES, MAX_ROUNDS};
use super::BestRun;
use crate::agents::control::Composer;
use crate::agents::AgentKind;
use crate::config::Theme;
use crate::renderer::font::FontManager;
use crate::review::diff::{FileDiff, FileStatus};
use crate::ui::kit::{ellipsize, mix, scroll_into_view, ButtonKind, ButtonState, Ctx, ListItem, PanelSpec, Rect, Tokens, Tone};

// ───────────────────────────── keys ─────────────────────────────

/// Keys the overlays understand (mapped from winit in `app/overlays.rs`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WKey {
    Char(char),
    /// Pasted or IME-committed text.
    Text(String),
    Enter,
    ShiftEnter,
    /// Cmd/Ctrl+Enter.
    Submit,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    BackTab,
    Escape,
    Ctrl(char),
}

/// Apply an editing key to a composer. Returns true when it was an editing key.
pub fn edit(c: &mut Composer, key: &WKey, multiline: bool) -> bool {
    match key {
        WKey::Char(ch) => c.insert_str(&ch.to_string()),
        WKey::Text(t) => c.insert_str(&if multiline { t.clone() } else { t.replace(['\n', '\r'], " ") }),
        WKey::Enter | WKey::ShiftEnter if multiline => c.newline(),
        WKey::Backspace => c.backspace(),
        WKey::Delete => c.delete(),
        WKey::Left => c.left(),
        WKey::Right => c.right(),
        WKey::Up if multiline => c.up(),
        WKey::Down if multiline => c.down(),
        WKey::Home | WKey::Ctrl('a') if multiline => c.line_home(),
        WKey::End | WKey::Ctrl('e') if multiline => c.line_end(),
        WKey::Home | WKey::Ctrl('a') => c.home(),
        WKey::End | WKey::Ctrl('e') => c.end(),
        WKey::Ctrl('u') => c.kill_to_start(),
        WKey::Ctrl('w') => c.kill_word(),
        _ => return false,
    }
    true
}

// ───────────────────────────── soft wrapping ─────────────────────────────

/// One visual row of a wrapped text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VRow {
    /// Char index of the row's first character in the whole text.
    pub start: usize,
    pub text: String,
}

/// Wrap `text` to `cols` characters per row; `\n` always breaks.
pub fn wrap_rows(text: &str, cols: usize) -> Vec<VRow> {
    let cols = cols.max(1);
    let mut rows = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0;
    let mut start = 0;
    let mut idx = 0;
    for ch in text.chars() {
        if ch == '\n' {
            rows.push(VRow { start, text: std::mem::take(&mut cur) });
            cur_len = 0;
            idx += 1;
            start = idx;
            continue;
        }
        if cur_len >= cols {
            rows.push(VRow { start, text: std::mem::take(&mut cur) });
            cur_len = 0;
            start = idx;
        }
        cur.push(ch);
        cur_len += 1;
        idx += 1;
    }
    rows.push(VRow { start, text: cur });
    rows
}

/// (row, column) of char index `cursor` in wrapped rows.
pub fn caret_in(rows: &[VRow], cursor: usize) -> (usize, usize) {
    for (i, r) in rows.iter().enumerate().rev() {
        if cursor >= r.start {
            return (i, cursor - r.start);
        }
    }
    (0, 0)
}

/// Draw a multi-line editor inside `r` (border, wrapped text, caret, placeholder).
/// Returns the caret rectangle for the IME.
fn draw_editor(cx: &mut Ctx, r: Rect, c: &Composer, placeholder: &str, focused: bool) -> Option<Rect> {
    let tk = cx.tk;
    let edge = if focused { tk.accent } else { tk.border_strong };
    cx.fill_rrect(r, tk.radius_sm, edge);
    cx.fill_rrect(r.inset(1, 1), tk.radius_sm.saturating_sub(1), tk.field);
    let pad = tk.sp.md;
    let x = r.x + pad;
    let cols = cx.cols(r.w.saturating_sub(2 * pad)).max(1);
    let rows_fit = (r.h.saturating_sub(2 * tk.sp.sm) / tk.row_h).max(1);
    let text = c.text();
    if text.is_empty() {
        cx.text_fit(x, r.y + tk.sp.sm + cx.text_y(0, tk.row_h), r.w.saturating_sub(2 * pad), placeholder, tk.text_faint);
        if focused {
            cx.fill(Rect::new(x, r.y + tk.sp.sm + cx.text_y(0, tk.row_h), tk.scale.max(1) * 2, tk.ch), tk.accent);
        }
        return Some(Rect::new(x, r.y + tk.sp.sm, tk.cw, tk.row_h));
    }
    let rows = wrap_rows(&text, cols);
    let (crow, ccol) = caret_in(&rows, c.cursor());
    let first = (crow + 1).saturating_sub(rows_fit);
    for (n, row) in rows.iter().skip(first).take(rows_fit).enumerate() {
        let y = r.y + tk.sp.sm + n * tk.row_h;
        cx.text(x, cx.text_y(y, tk.row_h), &row.text, tk.text);
    }
    let cy = r.y + tk.sp.sm + (crow - first) * tk.row_h;
    let cxp = x + ccol.min(cols) * tk.cw;
    if focused {
        cx.fill(Rect::new(cxp, cx.text_y(cy, tk.row_h), tk.scale.max(1) * 2, tk.ch), tk.accent);
    }
    if rows.len() > rows_fit {
        cx.scrollbar(r.inset(1, 1), rows.len(), rows_fit, first);
    }
    Some(Rect::new(cxp, cy, tk.cw, tk.row_h))
}

// ───────────────────────────── start wizard ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Task,
    Count,
    Agent(usize),
    Test,
    Rounds,
    Worktree,
}

/// What the wizard collected.
#[derive(Clone, Debug)]
pub struct Spec {
    pub tpl: Template,
    pub task: String,
    pub kinds: Vec<AgentKind>,
    pub test: String,
    pub rounds: u32,
    pub worktree: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WizardOutcome {
    None,
    Cancel,
    Start,
}

pub struct Wizard {
    pub tpl: Template,
    pub field: usize,
    pub task: Composer,
    pub test: Composer,
    pub kinds: Vec<AgentKind>,
    pub installed: Vec<AgentKind>,
    pub rounds: u32,
    pub worktree: bool,
    pub error: Option<String>,
    /// "aurora · main", shown next to the title.
    pub place: String,
    /// The directory is inside a git repository.
    pub in_git: bool,
    pub ime: Option<Rect>,
}

impl Wizard {
    pub fn new(tpl: Template, installed: Vec<AgentKind>, default_kind: AgentKind, place: String, in_git: bool, test_default: &str) -> Wizard {
        let want = match tpl.strategy {
            Strategy::BestOf => tpl.agents.len().clamp(2, MAX_CANDIDATES).max(if tpl.agents.is_empty() { 3 } else { 2 }),
            Strategy::WriteReview => 2,
            _ => 1,
        };
        let mut kinds: Vec<AgentKind> = tpl.agents.iter().copied().take(want).collect();
        while kinds.len() < want {
            kinds.push(kinds.last().copied().unwrap_or(default_kind));
        }
        let mut test = Composer::default();
        test.set_text(if tpl.test.is_empty() { test_default } else { &tpl.test });
        Wizard {
            rounds: tpl.rounds,
            worktree: tpl.worktree && in_git,
            tpl,
            field: 0,
            task: Composer::default(),
            test,
            kinds,
            installed,
            error: None,
            place,
            in_git,
            ime: None,
        }
    }

    pub fn fields(&self) -> Vec<Field> {
        let mut v = vec![Field::Task];
        match self.tpl.strategy {
            Strategy::BestOf => {
                v.push(Field::Count);
                v.extend((0..self.kinds.len()).map(Field::Agent));
                v.push(Field::Test);
            }
            Strategy::WriteReview => {
                v.extend([Field::Agent(0), Field::Agent(1), Field::Rounds, Field::Test, Field::Worktree]);
            }
            Strategy::FixTests => v.extend([Field::Agent(0), Field::Test, Field::Rounds, Field::Worktree]),
            Strategy::Single => v.extend([Field::Agent(0), Field::Worktree]),
        }
        v
    }

    pub fn current(&self) -> Field {
        let f = self.fields();
        f[self.field.min(f.len() - 1)]
    }

    pub fn text_input_active(&self) -> bool {
        matches!(self.current(), Field::Task | Field::Test)
    }

    pub fn insert_text(&mut self, text: &str) {
        match self.current() {
            Field::Task => self.task.insert_str(text),
            Field::Test => self.test.insert_str(&text.replace(['\n', '\r'], " ")),
            _ => {}
        }
    }

    fn set_count(&mut self, n: usize) {
        let n = n.clamp(2, MAX_CANDIDATES);
        while self.kinds.len() < n {
            let k = self.kinds.last().copied().unwrap_or(AgentKind::ClaudeCode);
            self.kinds.push(k);
        }
        self.kinds.truncate(n);
        self.field = self.field.min(self.fields().len() - 1);
    }

    fn cycle_kind(&mut self, slot: usize, delta: isize) {
        if self.installed.is_empty() || slot >= self.kinds.len() {
            return;
        }
        let pos = self.installed.iter().position(|k| *k == self.kinds[slot]).unwrap_or(0) as isize;
        let n = self.installed.len() as isize;
        self.kinds[slot] = self.installed[((pos + delta).rem_euclid(n)) as usize];
    }

    pub fn handle_key(&mut self, key: WKey) -> WizardOutcome {
        self.error = None;
        let fields = self.fields();
        match &key {
            WKey::Escape => return WizardOutcome::Cancel,
            WKey::Submit => return self.try_start(),
            WKey::Tab => {
                self.field = (self.field + 1) % fields.len();
                return WizardOutcome::None;
            }
            WKey::BackTab => {
                self.field = (self.field + fields.len() - 1) % fields.len();
                return WizardOutcome::None;
            }
            _ => {}
        }
        match self.current() {
            Field::Task => {
                edit(&mut self.task, &key, true);
            }
            Field::Test => {
                if key == WKey::Enter {
                    return self.try_start();
                }
                if key == WKey::Down {
                    self.field = (self.field + 1) % fields.len();
                } else if key == WKey::Up {
                    self.field = (self.field + fields.len() - 1) % fields.len();
                } else {
                    edit(&mut self.test, &key, false);
                }
            }
            Field::Count => match key {
                WKey::Left | WKey::Char('-') => self.set_count(self.kinds.len().saturating_sub(1)),
                WKey::Right | WKey::Char('+') | WKey::Char('=') => self.set_count(self.kinds.len() + 1),
                WKey::Char(c @ '2'..='4') => self.set_count(c as usize - '0' as usize),
                WKey::Down => self.field = (self.field + 1) % fields.len(),
                WKey::Up => self.field = (self.field + fields.len() - 1) % fields.len(),
                WKey::Enter => return self.try_start(),
                _ => {}
            },
            Field::Agent(i) => match key {
                WKey::Left => self.cycle_kind(i, -1),
                WKey::Right | WKey::Char(' ') => self.cycle_kind(i, 1),
                WKey::Down => self.field = (self.field + 1) % fields.len(),
                WKey::Up => self.field = (self.field + fields.len() - 1) % fields.len(),
                WKey::Enter => return self.try_start(),
                _ => {}
            },
            Field::Rounds => match key {
                WKey::Left | WKey::Char('-') => self.rounds = self.rounds.saturating_sub(1).max(1),
                WKey::Right | WKey::Char('+') | WKey::Char('=') => self.rounds = (self.rounds + 1).min(MAX_ROUNDS),
                WKey::Down => self.field = (self.field + 1) % fields.len(),
                WKey::Up => self.field = (self.field + fields.len() - 1) % fields.len(),
                WKey::Enter => return self.try_start(),
                _ => {}
            },
            Field::Worktree => match key {
                WKey::Left | WKey::Right | WKey::Char(' ') => {
                    if self.in_git {
                        self.worktree = !self.worktree;
                    } else {
                        self.error = Some("Not inside a git repository: no worktree possible".into());
                    }
                }
                WKey::Down => self.field = (self.field + 1) % fields.len(),
                WKey::Up => self.field = (self.field + fields.len() - 1) % fields.len(),
                WKey::Enter => return self.try_start(),
                _ => {}
            },
        }
        WizardOutcome::None
    }

    fn try_start(&mut self) -> WizardOutcome {
        match self.spec() {
            Ok(_) => WizardOutcome::Start,
            Err(e) => {
                self.error = Some(e);
                WizardOutcome::None
            }
        }
    }

    /// Validate and collect the answers.
    pub fn spec(&self) -> Result<Spec, String> {
        let task = self.task.text().trim().to_string();
        let test = self.test.text().trim().to_string();
        if self.installed.is_empty() {
            return Err("No agent CLI found on PATH (claude, codex, gemini, ...)".into());
        }
        if task.is_empty() && self.tpl.strategy != Strategy::FixTests {
            return Err("Describe the task first".into());
        }
        if self.tpl.strategy == Strategy::FixTests && test.is_empty() {
            return Err("Enter the test command to run".into());
        }
        if let Some(k) = self.kinds.iter().find(|k| !self.installed.contains(k)) {
            return Err(format!("{} is not installed", k.name()));
        }
        if matches!(self.tpl.strategy, Strategy::BestOf | Strategy::WriteReview) && !self.in_git {
            return Err("This workflow needs a git repository (it works in worktrees and diffs)".into());
        }
        Ok(Spec { tpl: self.tpl.clone(), task, kinds: self.kinds.clone(), test, rounds: self.rounds, worktree: self.worktree || self.tpl.strategy == Strategy::BestOf })
    }

    fn label_of(&self, f: Field) -> String {
        match f {
            Field::Task => match self.tpl.strategy {
                Strategy::FixTests => "Extra instructions (optional)".into(),
                _ => "Task".into(),
            },
            Field::Count => "Candidates".into(),
            Field::Agent(i) => match (self.tpl.strategy, i) {
                (Strategy::BestOf, i) => format!("Agent {}", i + 1),
                (Strategy::WriteReview, 0) => "Writer".into(),
                (Strategy::WriteReview, _) => "Reviewer".into(),
                _ => "Agent".into(),
            },
            Field::Test => match self.tpl.strategy {
                Strategy::FixTests => "Test command".into(),
                _ => "Test command (optional)".into(),
            },
            Field::Rounds => match self.tpl.strategy {
                Strategy::FixTests => "Max attempts".into(),
                _ => "Max review rounds".into(),
            },
            Field::Worktree => "Git worktree".into(),
        }
    }
}

// ───────────────────────────── wizard drawing ─────────────────────────────

fn field_box(cx: &mut Ctx, r: Rect, focused: bool) {
    let tk = cx.tk;
    let edge = if focused { tk.accent } else { tk.border_strong };
    cx.fill_rrect(r, tk.radius_sm, edge);
    cx.fill_rrect(r.inset(1, 1), tk.radius_sm.saturating_sub(1), tk.field);
}

/// "‹ Claude Code ›" selector with the agent chip.
fn draw_selector(cx: &mut Ctx, r: Rect, label: &str, kind: Option<AgentKind>, focused: bool) {
    let tk = cx.tk;
    field_box(cx, r, focused);
    let mut x = r.x + tk.sp.md;
    let ty = cx.text_y(r.y, r.h);
    if let Some(k) = kind {
        let chip = tk.ch + tk.sp.xs;
        crate::agents::dock::kind_chip(cx, x, r.y + r.h.saturating_sub(chip) / 2, chip, k);
        x += chip + tk.sp.sm;
    }
    cx.text_fit(x, ty, r.right().saturating_sub(x + tk.sp.md + 2 * tk.cw), label, tk.text);
    if focused {
        cx.text_right(r.right().saturating_sub(tk.sp.md), ty, "\u{2039} \u{203a}", tk.accent);
    }
}

impl Wizard {
    pub fn render(&mut self, buf: &mut [u32], w: usize, h: usize, font: &mut FontManager, theme: &Theme) {
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buf, w, h, font, &tk);
        cx.backdrop(tk.backdrop);
        let tk = cx.tk;
        let fields = self.fields();
        let ctl_h = tk.input_h;
        let task_h = 6 * tk.row_h + 2 * tk.sp.sm;
        let gap = tk.sp.sm;
        let label_w = 26 * tk.cw;
        let mut body_h = tk.sp.xs;
        for f in &fields {
            body_h += match f {
                Field::Task => tk.row_h + task_h + gap,
                _ => ctl_h + gap,
            };
        }
        body_h += tk.row_h + tk.sp.xs; // status line
        let want_h = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + body_h;
        let hints: &[(&str, &str)] = &[("Tab", "next field"), ("\u{2190}/\u{2192}", "change"), (if cfg!(target_os = "macos") { "Cmd+Enter" } else { "Ctrl+Enter" }, "start"), ("Esc", "cancel")];
        let strategy = self.tpl.strategy.label();
        let mut spec = PanelSpec::new(&self.tpl.name).sub(&self.place).hints(hints).no_close();
        if !self.tpl.name.eq_ignore_ascii_case(strategy) {
            spec = spec.badge(strategy, Tone::Accent);
        }
        let rect = cx.centered_cols(78, want_h.min(h.saturating_sub(2 * tk.sp.xl)));
        let body = cx.panel(rect, &spec);

        let mut y = body.y;
        self.ime = None;
        for (i, f) in fields.iter().enumerate() {
            let focused = i == self.field.min(fields.len() - 1);
            let label = self.label_of(*f);
            match f {
                Field::Task => {
                    cx.line(body.x, y, &label, if focused { tk.accent } else { tk.text_muted });
                    y += tk.row_h;
                    let r = Rect::new(body.x, y, body.w, task_h);
                    let ph = if self.tpl.strategy == Strategy::FixTests { "Anything the agent should know besides \"make the tests pass\"" } else { "What should the agent(s) do? Shift+Enter or Enter adds a line." };
                    self.ime = draw_editor(&mut cx, r, &self.task, ph, focused);
                    y += task_h + gap;
                }
                _ => {
                    let ty = cx.text_y(y, ctl_h);
                    cx.text(body.x, ty, &label, if focused { tk.accent } else { tk.text_muted });
                    let r = Rect::new(body.x + label_w, y, body.w.saturating_sub(label_w), ctl_h);
                    match f {
                        Field::Count => draw_selector(&mut cx, r, &format!("{} agents, one worktree each", self.kinds.len()), None, focused),
                        Field::Agent(k) => {
                            let kind = self.kinds[*k];
                            let missing = !self.installed.contains(&kind);
                            let name = if missing { format!("{} (not installed)", kind.name()) } else { kind.name().to_string() };
                            draw_selector(&mut cx, r, &name, Some(kind), focused);
                        }
                        Field::Test => {
                            let (t, c) = self.test.display();
                            cx.text_input(r, &t, c, None, "e.g. cargo test", focused);
                            if focused {
                                self.ime = Some(Rect::new(r.x + tk.sp.md + c * tk.cw, r.y, tk.cw, r.h));
                            }
                        }
                        Field::Rounds => draw_selector(&mut cx, r, &format!("{}", self.rounds), None, focused),
                        Field::Worktree => {
                            let txt = if self.worktree {
                                "yes: a fresh git worktree"
                            } else if self.in_git {
                                "no: work in the current directory"
                            } else {
                                "no: not a git repository"
                            };
                            draw_selector(&mut cx, r, txt, None, focused);
                        }
                        Field::Task => {}
                    }
                    y += ctl_h + gap;
                }
            }
        }
        let status = match (&self.error, self.installed.is_empty()) {
            (Some(e), _) => Some((tk.danger, e.clone())),
            (None, true) => Some((tk.warning, "No agent CLI found on PATH".to_string())),
            _ => None,
        };
        let sy = body.bottom().saturating_sub(tk.row_h);
        match status {
            Some((c, t)) => {
                cx.line_fit(body.x, sy, body.w, &t, c);
            }
            None => {
                let hint = match self.tpl.strategy {
                    Strategy::BestOf => "Each candidate works in its own worktree; you compare and merge the best.",
                    Strategy::WriteReview => "The reviewer sees every diff the writer produces; press f in the dock to forward its feedback.",
                    Strategy::FixTests => "Tests run in the agent's directory; failures are sent to it until they pass.",
                    Strategy::Single => "Starts the agent with this prompt.",
                };
                cx.line_fit(body.x, sy, body.w, hint, tk.text_faint);
            }
        }
    }
}

// ───────────────────────────── task queue editor ─────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueueAct {
    None,
    Close,
    Add(String),
    Replace(usize, String),
    Remove(usize),
    MoveUp(usize),
    MoveDown(usize),
    Clear,
    TogglePause,
}

#[derive(Clone, Debug)]
pub struct QueueEditor {
    pub uid: usize,
    /// "Claude Code \u{b7} aurora/main".
    pub title: String,
    pub sel: usize,
    /// Composer open: `Some(None)` adds, `Some(Some(i))` edits task `i`.
    pub editing: Option<Option<usize>>,
    pub composer: Composer,
    confirm_clear: bool,
    pub ime: Option<Rect>,
    scroll: usize,
}

impl QueueEditor {
    pub fn new(uid: usize, title: String) -> QueueEditor {
        QueueEditor { uid, title, sel: 0, editing: None, composer: Composer::default(), confirm_clear: false, ime: None, scroll: 0 }
    }

    pub fn composing(&self) -> bool {
        self.editing.is_some()
    }

    pub fn insert_text(&mut self, t: &str) {
        if self.composing() {
            self.composer.insert_str(t);
        }
    }

    /// Start composing a new task (also used when the dock's `t` opens the editor to add).
    pub fn begin_add(&mut self) {
        self.editing = Some(None);
        self.composer.clear();
    }

    pub fn handle_key(&mut self, key: WKey, q: &TaskQueue) -> QueueAct {
        if let Some(target) = self.editing {
            return match key {
                WKey::Escape => {
                    self.editing = None;
                    self.composer.clear();
                    QueueAct::None
                }
                WKey::Enter | WKey::Submit => {
                    let text = self.composer.text();
                    self.editing = None;
                    self.composer.clear();
                    if text.trim().is_empty() {
                        return QueueAct::None;
                    }
                    match target {
                        None => {
                            self.sel = q.len();
                            QueueAct::Add(text)
                        }
                        Some(i) => QueueAct::Replace(i, text),
                    }
                }
                WKey::ShiftEnter => {
                    self.composer.newline();
                    QueueAct::None
                }
                other => {
                    edit(&mut self.composer, &other, true);
                    QueueAct::None
                }
            };
        }
        let n = q.len();
        let clearing = std::mem::take(&mut self.confirm_clear);
        match key {
            WKey::Escape => QueueAct::Close,
            WKey::Up | WKey::Char('k') => {
                self.sel = self.sel.saturating_sub(1);
                QueueAct::None
            }
            WKey::Down | WKey::Char('j') => {
                self.sel = (self.sel + 1).min(n.saturating_sub(1));
                QueueAct::None
            }
            WKey::Char('a') | WKey::Char('n') | WKey::Char('+') => {
                self.begin_add();
                QueueAct::None
            }
            WKey::Enter | WKey::Char('e') if n > 0 => {
                self.editing = Some(Some(self.sel));
                self.composer.set_text(&q.tasks[self.sel.min(n - 1)]);
                QueueAct::None
            }
            WKey::Char('x') | WKey::Delete | WKey::Backspace if n > 0 => {
                let i = self.sel.min(n - 1);
                self.sel = i.saturating_sub(usize::from(i + 1 >= n));
                QueueAct::Remove(i)
            }
            WKey::Char('u') | WKey::Char('K') if n > 0 => {
                let i = self.sel.min(n - 1);
                self.sel = i.saturating_sub(1);
                QueueAct::MoveUp(i)
            }
            WKey::Char('d') | WKey::Char('J') if n > 0 => {
                let i = self.sel.min(n - 1);
                self.sel = (i + 1).min(n - 1);
                QueueAct::MoveDown(i)
            }
            WKey::Char('p') => QueueAct::TogglePause,
            WKey::Char('c') if n > 0 => {
                if clearing {
                    self.sel = 0;
                    QueueAct::Clear
                } else {
                    self.confirm_clear = true;
                    QueueAct::None
                }
            }
            _ => QueueAct::None,
        }
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.composing() {
            vec![("Enter", "save"), ("S-Enter", "new line"), ("Esc", "cancel")]
        } else {
            vec![("a", "add"), ("e", "edit"), ("x", "remove"), ("u/d", "reorder"), ("p", "pause"), ("c c", "clear"), ("Esc", "close")]
        }
    }

    pub fn render(&mut self, buf: &mut [u32], w: usize, h: usize, font: &mut FontManager, theme: &Theme, q: &TaskQueue) {
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buf, w, h, font, &tk);
        cx.backdrop(tk.backdrop);
        let tk = cx.tk;
        let hints = self.hints();
        let badge = if q.paused { "paused".to_string() } else { format!("{} queued", q.len()) };
        let spec = PanelSpec::new("Task queue").sub(&self.title).badge(&badge, if q.paused { Tone::Warning } else { Tone::Accent }).hints(&hints).no_close();
        let list_rows = 8usize.max(q.len().min(12));
        let comp_h = if self.composing() { 5 * tk.row_h + 2 * tk.sp.sm + tk.sp.md } else { 0 };
        let want_h = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + (list_rows + 2) * tk.row_h + comp_h;
        let rect = cx.centered_cols(80, want_h.min(h.saturating_sub(2 * tk.sp.xl)));
        let body = cx.panel(rect, &spec);

        let note = if q.paused { "Auto-send is paused. Press p to resume." } else { "Sent 3s after a turn ends and the agent is idle. Esc in that window pauses." };
        cx.line_fit(body.x, body.y, body.w, note, tk.text_muted);
        let list = Rect::new(body.x, body.y + tk.row_h + tk.sp.xs, body.w, body.h.saturating_sub(tk.row_h + tk.sp.xs + comp_h));
        if q.is_empty() {
            cx.empty_state(list, "No queued tasks", "Press a to add one. Multi-line text is fine.");
        } else {
            let rows = cx.rows_fit(list.h).max(1);
            self.sel = self.sel.min(q.len() - 1);
            self.scroll = scroll_into_view(self.sel, self.scroll, rows);
            let labels: Vec<(String, String)> = q
                .tasks
                .iter()
                .enumerate()
                .map(|(i, t)| (format!("{:>2}. {}", i + 1, queue::preview(t, cx.cols(list.w).saturating_sub(18))), if i == 0 && !q.paused { "next".to_string() } else { String::new() }))
                .collect();
            let items: Vec<ListItem> = labels.iter().map(|(l, m)| ListItem::new(l).meta(m)).collect();
            cx.list(list, &items, Some(self.sel), self.scroll, None);
        }
        self.ime = None;
        if self.composing() {
            let r = Rect::new(body.x, body.bottom().saturating_sub(comp_h - tk.sp.md), body.w, comp_h - tk.sp.md);
            let ph = if self.editing == Some(None) { "New task for the agent\u{2026}" } else { "Edit task\u{2026}" };
            self.ime = draw_editor(&mut cx, r, &self.composer, ph, true);
        }
    }
}

// ───────────────────────────── compare view ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareHit {
    Cand(usize),
    File(usize),
    Merge,
    DiscardOthers,
    Judge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompareAct {
    None,
    Close,
    Merge(usize),
    DiscardOthers(usize),
    DiscardThis(usize),
    Judge,
}

#[derive(Default)]
pub struct CompareState {
    pub run: u64,
    pub sel: usize,
    pub file: usize,
    pub scroll: usize,
    pub file_scroll: usize,
    pub page: usize,
    pub rects: Vec<(Rect, CompareHit)>,
}

impl CompareState {
    pub fn new(run: u64, sel: usize) -> CompareState {
        CompareState { run, sel, ..Default::default() }
    }

    fn current_diff<'a>(&self, run: &'a BestRun) -> Option<&'a crate::review::diff::ParsedDiff> {
        run.parsed.get(self.sel).and_then(|p| p.as_ref())
    }

    fn select(&mut self, i: usize) {
        self.sel = i;
        self.file = 0;
        self.scroll = 0;
        self.file_scroll = 0;
    }

    pub fn handle_key(&mut self, key: WKey, run: &BestRun) -> CompareAct {
        let n = run.sm.cands.len().max(1);
        let files = self.current_diff(run).map_or(0, |d| d.files.len());
        match key {
            WKey::Escape => return CompareAct::Close,
            WKey::Left | WKey::Char('h') => self.select((self.sel + n - 1) % n),
            WKey::Right | WKey::Char('l') | WKey::Tab => self.select((self.sel + 1) % n),
            WKey::Char(c @ '1'..='9') if (c as usize - '0' as usize) <= n => self.select(c as usize - '1' as usize),
            WKey::Up | WKey::Char('k') => {
                self.file = self.file.saturating_sub(1);
                self.scroll = 0;
            }
            WKey::Down | WKey::Char('j') => {
                self.file = (self.file + 1).min(files.saturating_sub(1));
                self.scroll = 0;
            }
            WKey::PageDown | WKey::Char(' ') => self.scroll += self.page.max(1),
            WKey::PageUp => self.scroll = self.scroll.saturating_sub(self.page.max(1)),
            WKey::Home | WKey::Char('g') => self.scroll = 0,
            WKey::End | WKey::Char('G') => self.scroll = usize::MAX / 2,
            WKey::Char('n') | WKey::Char('p') => {
                let forward = key == WKey::Char('n');
                if let Some(f) = self.current_diff(run).and_then(|d| d.files.get(self.file)) {
                    let rows = f.hunk_rows();
                    let cur = self.scroll;
                    let target = if forward { rows.iter().copied().find(|&r| r > cur) } else { rows.iter().copied().rev().find(|&r| r < cur) };
                    if let Some(t) = target {
                        self.scroll = t;
                    }
                }
            }
            WKey::Char('m') => return CompareAct::Merge(self.sel),
            WKey::Char('d') => return CompareAct::DiscardOthers(self.sel),
            WKey::Char('D') => return CompareAct::DiscardThis(self.sel),
            WKey::Char('a') => return CompareAct::Judge,
            _ => {}
        }
        CompareAct::None
    }

    pub fn click(&mut self, x: usize, y: usize) -> CompareAct {
        let hit = self.rects.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h);
        match hit {
            Some(CompareHit::Cand(i)) => {
                self.select(i);
                CompareAct::None
            }
            Some(CompareHit::File(i)) => {
                self.file = i;
                self.scroll = 0;
                CompareAct::None
            }
            Some(CompareHit::Merge) => CompareAct::Merge(self.sel),
            Some(CompareHit::DiscardOthers) => CompareAct::DiscardOthers(self.sel),
            Some(CompareHit::Judge) => CompareAct::Judge,
            None => CompareAct::None,
        }
    }
}

pub fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// Text lines for one candidate card: (text, tone, strong).
pub fn card_lines(run: &BestRun, i: usize) -> Vec<(String, Tone)> {
    let c = &run.sm.cands[i];
    let mut v = Vec::new();
    v.push((ellipsize(&c.branch, 64), Tone::Neutral));
    v.push(match (&c.diff, &c.diff_err) {
        (Some(d), _) if d.files == 0 => ("no changes".to_string(), Tone::Warning),
        (Some(d), _) => (format!("{} file{}  +{}  -{}", d.files, if d.files == 1 { "" } else { "s" }, d.added, d.removed), Tone::Success),
        (None, Some(e)) => (format!("diff failed: {e}"), Tone::Danger),
        (None, None) => ("diff pending\u{2026}".to_string(), Tone::Neutral),
    });
    v.push(match &c.tests {
        TestStatus::None => ("no test command".to_string(), Tone::Neutral),
        TestStatus::Pending => ("tests queued".to_string(), Tone::Neutral),
        TestStatus::Running => ("tests running\u{2026}".to_string(), Tone::Accent),
        TestStatus::Passed => ("tests passed".to_string(), Tone::Success),
        TestStatus::Failed(_) => ("tests failed".to_string(), Tone::Danger),
    });
    let time = c.duration().map(fmt_duration).unwrap_or_else(|| "\u{2014}".to_string());
    let cost = c.cost.map(crate::agents::metrics::fmt_cost).unwrap_or_else(|| "\u{2014}".to_string());
    v.push((format!("{time}  \u{b7}  {cost}"), Tone::Neutral));
    v
}

fn state_badge(c: &super::bestof::Cand) -> (&'static str, Tone) {
    if c.merged {
        return ("MERGED", Tone::Success);
    }
    if c.removed {
        return ("REMOVED", Tone::Neutral);
    }
    match c.state {
        CandState::Finished => ("done", Tone::Success),
        CandState::Exited => ("exited", Tone::Warning),
        CandState::Working => ("working", Tone::Accent),
        _ => ("starting", Tone::Neutral),
    }
}

fn file_tone(s: FileStatus) -> Tone {
    match s {
        FileStatus::Added | FileStatus::Copied => Tone::Success,
        FileStatus::Deleted => Tone::Danger,
        FileStatus::Renamed => Tone::Accent,
        FileStatus::Modified => Tone::Warning,
    }
}

fn tail_fit(s: &str, cols: usize) -> String {
    let n = s.chars().count();
    if n <= cols || cols < 2 {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - (cols - 1)).collect();
    format!("\u{2026}{tail}")
}

impl CompareState {
    pub fn render(&mut self, buf: &mut [u32], w: usize, h: usize, font: &mut FontManager, theme: &Theme, run: &BestRun) {
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buf, w, h, font, &tk);
        cx.backdrop(tk.backdrop);
        let tk = cx.tk;
        self.rects.clear();
        let n = run.sm.cands.len();
        if n == 0 {
            return;
        }
        self.sel = self.sel.min(n - 1);

        let base = match (&run.sm.base_branch, run.sm.base_commit.get(..7)) {
            (Some(b), Some(c)) => format!("{b}@{c}"),
            (None, Some(c)) => c.to_string(),
            _ => String::new(),
        };
        let task1 = run.sm.task.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        let sub = format!("{} \u{b7} base {base}", ellipsize(&task1, 48));
        let badge = format!("{n} candidates");
        let hints: [(&str, &str); 4] = [("\u{2190}/\u{2192}", "candidate"), ("j/k", "file"), ("n/p", "hunk"), ("Esc", "close")];
        let spec = PanelSpec::new("Compare candidates").sub(&sub).badge(&badge, Tone::Accent).hints(&hints);
        let rect = cx.centered(96, 4000, 92);
        let body = cx.panel(rect, &spec);

        // ── candidate cards, side by side
        let gap = tk.sp.md;
        let card_w = (body.w.saturating_sub(gap * (n - 1))) / n;
        let pad = tk.sp.sm;
        let card_h = pad * 2 + tk.row_h * 5 + tk.sp.xs;
        let suggested = run.sm.suggested();
        let any_merged = run.sm.cands.iter().any(|c| c.merged);
        for (i, c) in run.sm.cands.iter().enumerate() {
            let r = Rect::new(body.x + i * (card_w + gap), body.y, card_w, card_h);
            let selected = i == self.sel;
            let bg = if selected { mix(tk.surface, tk.accent, 0.10) } else { tk.surface_alt };
            cx.fill_rrect(r, tk.radius_sm, if selected { tk.accent } else { tk.border });
            cx.fill_rrect(r.inset(1, 1), tk.radius_sm.saturating_sub(1), bg);
            let x = r.x + tk.sp.md;
            let inner_w = r.w.saturating_sub(2 * tk.sp.md);
            let mut y = r.y + pad;
            // Header: chip, label, state badge.
            let chip = tk.ch + tk.sp.xs;
            crate::agents::dock::kind_chip(&mut cx, x, y + tk.row_h.saturating_sub(chip) / 2, chip, c.kind);
            let (btext, btone) = state_badge(c);
            let bw = cx.badge_w(btext);
            cx.badge(r.right().saturating_sub(tk.sp.md + bw), y, btext, btone, tk.row_h);
            let name_x = x + chip + tk.sp.sm;
            cx.text_fit(name_x, cx.text_y(y, tk.row_h), r.right().saturating_sub(tk.sp.md + bw + tk.sp.sm + name_x), &format!("{} {}", i + 1, c.label), if selected { tk.accent } else { tk.text });
            y += tk.row_h;
            for (k, (text, tone)) in card_lines(run, i).into_iter().enumerate() {
                let colour = match tone {
                    Tone::Neutral => {
                        if k == 0 {
                            tk.text_faint
                        } else {
                            tk.text_muted
                        }
                    }
                    t => tk.tone(t),
                };
                let shown = if k == 0 { tail_fit(&text, cx.cols(inner_w)) } else { text };
                cx.text_fit(x, cx.text_y(y, tk.row_h), inner_w, &shown, colour);
                y += tk.row_h;
            }
            if !any_merged && i == suggested && run.sm.cands.len() > 1 && c.diff.is_some_and(|d| d.files > 0) {
                cx.text_right(r.right().saturating_sub(tk.sp.md), cx.text_y(r.y + pad + 4 * tk.row_h, tk.row_h), "suggested", tk.text_faint);
            }
            self.rects.push((r, CompareHit::Cand(i)));
        }

        // ── bottom: status line + actions
        let btn_h = tk.button_h;
        let actions_y = body.bottom().saturating_sub(btn_h);
        let status_y = actions_y.saturating_sub(tk.row_h + tk.sp.xs);
        let busy = run.busy.is_some();
        let cand = &run.sm.cands[self.sel];
        let can_act = !busy && !cand.removed;
        let mut bx = body.x;
        let mut button = |cx: &mut Ctx, label: &str, kind: ButtonKind, enabled: bool, hit: CompareHit, rects: &mut Vec<(Rect, CompareHit)>| {
            let bw = cx.button_w(label);
            let r = Rect::new(bx, actions_y, bw, btn_h);
            cx.button(r, label, kind, if enabled { ButtonState::Normal } else { ButtonState::Disabled });
            if enabled {
                rects.push((r, hit));
            }
            bx += bw + tk.sp.md;
        };
        let merge_label = format!("m  Merge {}", cand.label);
        button(&mut cx, &merge_label, ButtonKind::Primary, can_act && !cand.merged, CompareHit::Merge, &mut self.rects);
        button(&mut cx, "d  Discard others", ButtonKind::Danger, can_act && n > 1, CompareHit::DiscardOthers, &mut self.rects);
        button(&mut cx, "a  Ask AI to judge", ButtonKind::Secondary, !busy, CompareHit::Judge, &mut self.rects);
        if let Some(b) = &run.busy {
            cx.text_right(body.right(), cx.text_y(actions_y, btn_h), &format!("{b}\u{2026}"), tk.accent);
        }
        // Status line: the last result, else why the selected candidate's tests failed.
        let failing = match &cand.tests {
            TestStatus::Failed(out) => out.lines().rev().find(|l| !l.trim().is_empty()).map(|l| format!("tests failed: {}", l.trim())),
            _ => None,
        };
        match (&run.message, failing) {
            (Some((tone, msg)), _) => {
                cx.line_fit(body.x, status_y, body.w, msg, tk.tone(*tone));
            }
            (None, Some(f)) => {
                cx.line_fit(body.x, status_y, body.w, &f, tk.danger);
            }
            (None, None) => {
                cx.line_fit(body.x, status_y, body.w, "Merging asks first; a conflict aborts the merge and changes nothing.", tk.text_faint);
            }
        }

        // ── middle: files + diff of the selected candidate
        let top = body.y + card_h + tk.sp.md;
        let mid = Rect::new(body.x, top, body.w, status_y.saturating_sub(top + tk.sp.xs));
        if mid.h < 3 * tk.row_h {
            return;
        }
        let parsed = self.current_diff(run);
        match parsed {
            None => {
                let (msg, hint) = match (&cand.diff_err, cand.removed) {
                    (_, true) => ("This candidate was removed".to_string(), String::new()),
                    (Some(e), _) => ("Could not compute the diff".to_string(), e.clone()),
                    _ => ("Diff not available yet".to_string(), String::new()),
                };
                cx.empty_state(mid, &msg, &hint);
            }
            Some(d) if d.files.is_empty() => cx.empty_state(mid, "No changes", "This candidate left the worktree as it found it"),
            Some(d) => {
                let left_w = (mid.w / 3).clamp(24 * tk.cw, 44 * tk.cw).min(mid.w / 2);
                let left = Rect::new(mid.x, mid.y, left_w, mid.h);
                let right = Rect::new(mid.x + left_w + tk.sp.md, mid.y, mid.w.saturating_sub(left_w + tk.sp.md), mid.h);
                cx.vdivider(mid.x + left_w + tk.sp.md / 2, mid.y, mid.h);
                self.file = self.file.min(d.files.len() - 1);
                let vis = cx.rows_fit(left.h).max(1);
                self.file_scroll = scroll_into_view(self.file, self.file_scroll, vis);
                let cols = cx.cols(left.w).saturating_sub(14);
                let labels: Vec<(String, String, Tone)> = d
                    .files
                    .iter()
                    .map(|f| {
                        let meta = if f.binary { "bin".to_string() } else { format!("+{} -{}", f.added, f.removed) };
                        (format!("{} {}", f.status.letter(), tail_fit(&f.new_path, cols.max(8))), meta, file_tone(f.status))
                    })
                    .collect();
                let items: Vec<ListItem> = labels.iter().map(|(l, m, t)| ListItem::new(l).meta(m).tone(*t)).collect();
                cx.list(left, &items, Some(self.file), self.file_scroll, None);
                for k in 0..vis.min(d.files.len().saturating_sub(self.file_scroll)) {
                    self.rects.push((Rect::new(left.x, left.y + k * tk.row_h, left.w, tk.row_h), CompareHit::File(self.file_scroll + k)));
                }
                let f: &FileDiff = &d.files[self.file];
                let counts = if f.binary { "binary".to_string() } else { format!("+{} -{}", f.added, f.removed) };
                let cw_counts = cx.tw(&counts);
                cx.line_fit(right.x, right.y, right.w.saturating_sub(cw_counts + tk.sp.md), &f.display_path(), tk.text);
                cx.text_right(right.right(), cx.text_y(right.y, tk.row_h), &counts, if f.binary { tk.text_muted } else { tk.success });
                cx.divider(right.x, right.y + tk.row_h, right.w);
                let area = Rect::new(right.x, right.y + tk.row_h + 1 + tk.sp.xs, right.w, right.h.saturating_sub(tk.row_h + 1 + tk.sp.xs));
                if f.hunks.is_empty() {
                    cx.empty_state(area, if f.binary { "Binary file \u{2014} no textual diff" } else { "No textual changes" }, "");
                } else {
                    let paint = crate::review::draw_file_diff(&mut cx, area, f, self.scroll, usize::MAX / 4);
                    self.scroll = paint.scroll;
                    self.page = paint.page;
                }
            }
        }
    }
}

// ───────────────────────────── overlay state ─────────────────────────────

/// Which workflow overlay is open (at most one).
#[derive(Default)]
pub struct WorkflowUi {
    pub wizard: Option<Wizard>,
    pub queue: Option<QueueEditor>,
    pub compare: Option<CompareState>,
}

impl WorkflowUi {
    pub fn visible(&self) -> bool {
        self.wizard.is_some() || self.queue.is_some() || self.compare.is_some()
    }

    pub fn close_all(&mut self) {
        self.wizard = None;
        self.queue = None;
        self.compare = None;
    }

    /// An overlay with a text field has the keyboard (IME needs a caret rectangle).
    pub fn text_input_active(&self) -> bool {
        self.wizard.as_ref().is_some_and(|w| w.text_input_active()) || self.queue.as_ref().is_some_and(|q| q.composing())
    }

    pub fn insert_text(&mut self, text: &str) -> bool {
        if let Some(w) = self.wizard.as_mut() {
            w.insert_text(text);
            return true;
        }
        if let Some(q) = self.queue.as_mut() {
            q.insert_text(text);
            return true;
        }
        false
    }

    pub fn ime_rect(&self) -> Option<(usize, usize, usize, usize)> {
        let r = self.wizard.as_ref().and_then(|w| w.ime).or_else(|| self.queue.as_ref().and_then(|q| q.ime))?;
        Some((r.x, r.y, r.w, r.h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn wiz(strategy: Strategy) -> Wizard {
        let tpl = Template::new("T", strategy);
        Wizard::new(tpl, vec![AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Gemini], AgentKind::ClaudeCode, "aurora \u{b7} main".into(), true, "cargo test")
    }

    fn type_text(w: &mut Wizard, s: &str) {
        for c in s.chars() {
            w.handle_key(WKey::Char(c));
        }
    }

    #[test]
    fn wrapping_and_caret() {
        let rows = wrap_rows("abcdef\nxy", 4);
        assert_eq!(rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["abcd", "ef", "xy"]);
        assert_eq!(rows.iter().map(|r| r.start).collect::<Vec<_>>(), [0, 4, 7]);
        assert_eq!(caret_in(&rows, 0), (0, 0));
        assert_eq!(caret_in(&rows, 4), (1, 0), "a wrap boundary belongs to the next row");
        assert_eq!(caret_in(&rows, 6), (1, 2));
        assert_eq!(caret_in(&rows, 7), (2, 0));
        assert_eq!(caret_in(&rows, 9), (2, 2));
        assert_eq!(wrap_rows("", 10), vec![VRow { start: 0, text: String::new() }]);
        let rows = wrap_rows("a\n", 3);
        assert_eq!(rows.len(), 2);
        assert_eq!(caret_in(&rows, 2), (1, 0), "after a trailing newline");
    }

    #[test]
    fn editing_keys() {
        let mut c = Composer::default();
        assert!(edit(&mut c, &WKey::Text("one\r\ntwo".into()), true));
        assert_eq!(c.text(), "one\ntwo");
        edit(&mut c, &WKey::Up, true);
        edit(&mut c, &WKey::End, true);
        edit(&mut c, &WKey::Char('!'), true);
        assert_eq!(c.text(), "one!\ntwo");
        let mut single = Composer::default();
        edit(&mut single, &WKey::Text("a\nb".into()), false);
        assert_eq!(single.text(), "a b");
        assert!(!edit(&mut single, &WKey::Tab, false));
        assert!(!edit(&mut single, &WKey::Enter, false), "Enter is not an edit in a single-line field");
    }

    #[test]
    fn best_of_wizard_flow() {
        let mut w = wiz(Strategy::BestOf);
        assert_eq!(w.kinds.len(), 3);
        assert_eq!(w.fields(), vec![Field::Task, Field::Count, Field::Agent(0), Field::Agent(1), Field::Agent(2), Field::Test]);
        // Empty task is refused.
        assert_eq!(w.handle_key(WKey::Submit), WizardOutcome::None);
        assert_eq!(w.error.as_deref(), Some("Describe the task first"));
        type_text(&mut w, "add retry");
        w.handle_key(WKey::Enter); // newline in the task field
        type_text(&mut w, "with backoff");
        assert_eq!(w.task.text(), "add retry\nwith backoff");
        w.handle_key(WKey::Tab);
        assert_eq!(w.current(), Field::Count);
        w.handle_key(WKey::Left);
        assert_eq!(w.kinds.len(), 2);
        w.handle_key(WKey::Left);
        assert_eq!(w.kinds.len(), 2, "never below 2");
        w.handle_key(WKey::Char('4'));
        assert_eq!(w.kinds.len(), 4);
        w.handle_key(WKey::Right);
        assert_eq!(w.kinds.len(), 4, "never above 4");
        // Mix agents: slot 1 -> Codex, slot 2 -> Gemini.
        w.handle_key(WKey::Tab);
        w.handle_key(WKey::Tab);
        assert_eq!(w.current(), Field::Agent(1));
        w.handle_key(WKey::Right);
        w.handle_key(WKey::Tab);
        w.handle_key(WKey::Right);
        w.handle_key(WKey::Right);
        assert_eq!(&w.kinds[..3], &[AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Gemini]);
        let spec = w.spec().unwrap();
        assert_eq!(spec.task, "add retry\nwith backoff");
        assert_eq!(spec.test, "cargo test");
        assert!(spec.worktree, "best-of always uses worktrees");
        assert_eq!(w.handle_key(WKey::Submit), WizardOutcome::Start);
        // Back-tab wraps to the last field.
        w.field = 0;
        w.handle_key(WKey::BackTab);
        assert_eq!(w.current(), Field::Test);
        assert_eq!(w.handle_key(WKey::Enter), WizardOutcome::Start, "Enter starts from a non-task field");
        assert_eq!(w.handle_key(WKey::Escape), WizardOutcome::Cancel);
    }

    #[test]
    fn shrinking_the_count_keeps_the_field_valid() {
        let mut w = wiz(Strategy::BestOf);
        w.field = w.fields().len() - 1; // Test
        w.field = 1;
        w.set_count(2);
        assert!(w.field < w.fields().len());
        w.field = w.fields().len() - 1;
        w.set_count(4);
        assert!(w.field < w.fields().len());
    }

    #[test]
    fn validation_messages() {
        let mut w = wiz(Strategy::FixTests);
        w.test.clear();
        assert_eq!(w.spec().unwrap_err(), "Enter the test command to run");
        w.test.set_text("npm test");
        assert!(w.spec().is_ok(), "the task is optional for fix-tests");
        let mut w = wiz(Strategy::WriteReview);
        w.task.set_text("x");
        w.in_git = false;
        assert!(w.spec().unwrap_err().contains("git repository"));
        w.in_git = true;
        w.kinds[1] = AgentKind::Aider;
        assert_eq!(w.spec().unwrap_err(), "Aider is not installed");
        w.installed.clear();
        assert!(w.spec().unwrap_err().contains("No agent CLI"));
        let mut s = wiz(Strategy::Single);
        s.task.set_text("hi");
        assert!(s.spec().is_ok());
        assert_eq!(s.kinds.len(), 1);
    }

    #[test]
    fn writer_reviewer_wizard_fields() {
        let w = wiz(Strategy::WriteReview);
        assert_eq!(w.kinds.len(), 2);
        assert_eq!(w.label_of(Field::Agent(0)), "Writer");
        assert_eq!(w.label_of(Field::Agent(1)), "Reviewer");
        let mut w = w;
        w.field = 3;
        assert_eq!(w.current(), Field::Rounds);
        w.handle_key(WKey::Right);
        assert_eq!(w.rounds, 4);
        for _ in 0..20 {
            w.handle_key(WKey::Right);
        }
        assert_eq!(w.rounds, MAX_ROUNDS);
        for _ in 0..20 {
            w.handle_key(WKey::Left);
        }
        assert_eq!(w.rounds, 1);
    }

    #[test]
    fn queue_editor_keys() {
        let mut q = TaskQueue::default();
        q.push("one");
        q.push("two");
        let mut e = QueueEditor::new(7, "Claude Code".into());
        assert_eq!(e.handle_key(WKey::Down, &q), QueueAct::None);
        assert_eq!(e.sel, 1);
        assert_eq!(e.handle_key(WKey::Char('u'), &q), QueueAct::MoveUp(1));
        assert_eq!(e.sel, 0);
        assert_eq!(e.handle_key(WKey::Char('d'), &q), QueueAct::MoveDown(0));
        assert_eq!(e.sel, 1);
        assert_eq!(e.handle_key(WKey::Char('p'), &q), QueueAct::TogglePause);
        // Add, multi-line.
        assert_eq!(e.handle_key(WKey::Char('a'), &q), QueueAct::None);
        assert!(e.composing());
        for c in "fix".chars() {
            e.handle_key(WKey::Char(c), &q);
        }
        e.handle_key(WKey::ShiftEnter, &q);
        for c in "it".chars() {
            e.handle_key(WKey::Char(c), &q);
        }
        assert_eq!(e.handle_key(WKey::Enter, &q), QueueAct::Add("fix\nit".into()));
        assert!(!e.composing());
        // Edit the selected one.
        e.sel = 0;
        e.handle_key(WKey::Char('e'), &q);
        assert_eq!(e.composer.text(), "one");
        e.handle_key(WKey::Char('!'), &q);
        assert_eq!(e.handle_key(WKey::Enter, &q), QueueAct::Replace(0, "one!".into()));
        // Escape while composing cancels only the composer.
        e.handle_key(WKey::Char('a'), &q);
        assert_eq!(e.handle_key(WKey::Escape, &q), QueueAct::None);
        assert_eq!(e.handle_key(WKey::Escape, &q), QueueAct::Close);
        // Remove, and clear needs two presses.
        e.sel = 1;
        assert_eq!(e.handle_key(WKey::Char('x'), &q), QueueAct::Remove(1));
        assert_eq!(e.sel, 0);
        assert_eq!(e.handle_key(WKey::Char('c'), &q), QueueAct::None);
        assert_eq!(e.handle_key(WKey::Char('c'), &q), QueueAct::Clear);
        assert_eq!(e.handle_key(WKey::Char('c'), &q), QueueAct::None, "armed again");
        assert_eq!(e.handle_key(WKey::Char('j'), &q), QueueAct::None);
        assert_eq!(e.handle_key(WKey::Char('c'), &q), QueueAct::None, "any other key disarms");
        // Empty queue: item keys do nothing.
        let empty = TaskQueue::default();
        assert_eq!(e.handle_key(WKey::Char('x'), &empty), QueueAct::None);
        assert_eq!(e.handle_key(WKey::Enter, &empty), QueueAct::None);
        // Blank input adds nothing.
        e.handle_key(WKey::Char('a'), &empty);
        e.handle_key(WKey::Char(' '), &empty);
        assert_eq!(e.handle_key(WKey::Enter, &empty), QueueAct::None);
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(Duration::from_secs(9)), "9s");
        assert_eq!(fmt_duration(Duration::from_secs(252)), "4m12s");
        assert_eq!(fmt_duration(Duration::from_secs(3720)), "1h02m");
    }

    #[test]
    fn compare_keys_and_clicks() {
        let run = super::super::sample::best_run(3);
        let mut st = CompareState::new(run.sm.id, 0);
        assert_eq!(st.handle_key(WKey::Right, &run), CompareAct::None);
        assert_eq!(st.sel, 1);
        st.handle_key(WKey::Left, &run);
        st.handle_key(WKey::Left, &run);
        assert_eq!(st.sel, 2, "wraps around");
        st.handle_key(WKey::Char('1'), &run);
        assert_eq!(st.sel, 0);
        st.handle_key(WKey::Char('9'), &run);
        assert_eq!(st.sel, 0, "no such candidate");
        assert_eq!(st.handle_key(WKey::Char('m'), &run), CompareAct::Merge(0));
        assert_eq!(st.handle_key(WKey::Char('d'), &run), CompareAct::DiscardOthers(0));
        assert_eq!(st.handle_key(WKey::Char('D'), &run), CompareAct::DiscardThis(0));
        assert_eq!(st.handle_key(WKey::Char('a'), &run), CompareAct::Judge);
        assert_eq!(st.handle_key(WKey::Escape, &run), CompareAct::Close);
        // Files of the selected candidate.
        st.handle_key(WKey::Down, &run);
        assert_eq!(st.file, 1);
        for _ in 0..10 {
            st.handle_key(WKey::Down, &run);
        }
        assert_eq!(st.file, run.parsed[0].as_ref().unwrap().files.len() - 1);
        st.rects.push((Rect::new(10, 10, 50, 20), CompareHit::Merge));
        st.rects.push((Rect::new(100, 10, 50, 20), CompareHit::Cand(2)));
        assert_eq!(st.click(20, 15), CompareAct::Merge(0));
        assert_eq!(st.click(110, 15), CompareAct::None);
        assert_eq!(st.sel, 2);
        assert_eq!(st.click(900, 900), CompareAct::None);
        let _ = PathBuf::new();
    }

    #[test]
    fn card_text_reflects_the_results() {
        let run = super::super::sample::best_run(3);
        let d = run.sm.cands[0].diff.unwrap();
        let l0 = card_lines(&run, 0);
        assert_eq!(l0[1], (format!("{} files  +{}  -{}", d.files, d.added, d.removed), Tone::Success));
        assert_eq!(l0[2], ("tests passed".to_string(), Tone::Success));
        assert!(l0[3].0.contains("4m12s") && l0[3].0.contains("$0.42"), "{:?}", l0[3]);
        let l1 = card_lines(&run, 1);
        assert_eq!(l1[2], ("tests failed".to_string(), Tone::Danger));
        let l2 = card_lines(&run, 2);
        assert!(l2[3].0.contains("3m40s") && l2[3].0.contains('\u{2014}'), "no cost known: {:?}", l2[3]);
        // Not collected yet.
        let mut r = super::super::sample::best_run(2);
        r.sm.cands[0].diff = None;
        r.sm.cands[0].tests = TestStatus::Running;
        r.sm.cands[1].diff = None;
        r.sm.cands[1].diff_err = Some("boom".into());
        r.sm.cands[1].tests = TestStatus::None;
        assert_eq!(card_lines(&r, 0)[1].0, "diff pending\u{2026}");
        assert_eq!(card_lines(&r, 0)[2].1, Tone::Accent);
        assert_eq!(card_lines(&r, 1)[1], ("diff failed: boom".to_string(), Tone::Danger));
        assert_eq!(card_lines(&r, 1)[2].0, "no test command");
    }
}
