//! Inline regex tester — type a pattern, test it against sample text, and
//! see matches (and capture groups) highlighted in real time.
//!
//! No external regex crate and no shelling out to `grep`: patterns are
//! parsed into a small AST and compiled to a flat backtracking bytecode
//! program (a classic Pike/Thompson-style VM, à la Russ Cox's regex
//! articles). It supports literals, `.`, `*` `+` `?`, `[...]` character
//! classes (with `\d \w \s` and negated forms), `^` `$` anchors, `|`
//! alternation and capturing `(...)` groups. Anything past that — bounded
//! `{n,m}` repetition, lazy quantifiers, lookaround, backreferences,
//! non-capturing/named groups — is deliberately rejected with a message
//! pointing at `grep -P` instead of half-implementing it.

use crate::config::{Rgb, Theme};
use crate::renderer::font::FontManager;

const COMPLEX_MSG: &str = "Complex regex — use grep for full PCRE";

// ── Public API ──

pub struct RegexPlayground {
    pub visible: bool,
    pub pattern: String,
    pub test_text: String,
    pub matches: Vec<MatchResult>,
    pub active_field: Field,
    pub error: Option<String>,
    pub flags: RegexFlags,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Pattern,
    TestText,
}

#[derive(Clone, Copy, Default)]
pub struct RegexFlags {
    pub case_insensitive: bool,
    pub multiline: bool,
    pub dot_all: bool,
}

#[derive(Clone)]
pub struct MatchResult {
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub groups: Vec<(usize, usize)>,
}

pub enum RegexKey {
    Char(char),
    Backspace,
    /// Switch the active field (Pattern <-> TestText).
    Enter,
    Escape,
    /// Switch the active field (Pattern <-> TestText).
    Tab,
    /// Toggle case-insensitive matching.
    CtrlI,
    /// Toggle multiline anchors (^ / $ match at line boundaries).
    CtrlM,
    /// Toggle dot-all (`.` matches newline too).
    CtrlS,
}

impl RegexPlayground {
    pub fn new() -> Self {
        Self {
            visible: false,
            pattern: String::new(),
            test_text: String::new(),
            matches: Vec::new(),
            active_field: Field::Pattern,
            error: None,
            flags: RegexFlags::default(),
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible && self.pattern.is_empty() && self.test_text.is_empty() {
            // Friendly first-run example — demonstrates classes, anchors
            // and multi-line text immediately instead of opening blank.
            self.pattern = r"\w+@\w+\.\w+".to_string();
            self.test_text =
                "contact: alice@example.com\nbackup: bob@example.org\n(no match on this line)"
                    .to_string();
            self.active_field = Field::Pattern;
            self.recompile();
        }
    }

    pub fn handle_key(&mut self, key: RegexKey) {
        match key {
            RegexKey::Char(c) => {
                match self.active_field {
                    Field::Pattern => self.pattern.push(c),
                    Field::TestText => self.test_text.push(c),
                }
                self.recompile();
            }
            RegexKey::Backspace => {
                match self.active_field {
                    Field::Pattern => {
                        self.pattern.pop();
                    }
                    Field::TestText => {
                        self.test_text.pop();
                    }
                }
                self.recompile();
            }
            RegexKey::Enter | RegexKey::Tab => {
                self.active_field = match self.active_field {
                    Field::Pattern => Field::TestText,
                    Field::TestText => Field::Pattern,
                };
            }
            RegexKey::Escape => self.visible = false,
            RegexKey::CtrlI => {
                self.flags.case_insensitive = !self.flags.case_insensitive;
                self.recompile();
            }
            RegexKey::CtrlM => {
                self.flags.multiline = !self.flags.multiline;
                self.recompile();
            }
            RegexKey::CtrlS => {
                self.flags.dot_all = !self.flags.dot_all;
                self.recompile();
            }
        }
    }

    /// Re-parse `pattern` and re-scan `test_text`. Called after every edit
    /// to pattern, test text, or flags so `matches`/`error` are always in
    /// sync with what's on screen.
    pub fn recompile(&mut self) {
        self.error = None;
        self.matches.clear();
        if self.pattern.is_empty() {
            return;
        }
        match compile_pattern(&self.pattern) {
            Ok(prog) => {
                let ctx = MatchCtx {
                    case_insensitive: self.flags.case_insensitive,
                    multiline: self.flags.multiline,
                    dot_all: self.flags.dot_all,
                };
                let (matches, overflowed) = find_all(&self.test_text, &prog, &ctx);
                self.matches = matches;
                if overflowed {
                    self.error = Some(COMPLEX_MSG.to_string());
                }
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    // ── Rendering ──

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut FontManager,
        theme: &Theme,
    ) {
        use crate::ui::kit::{Ctx, PanelSpec, Rect, Tokens, Tone};
        if !self.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(tk.backdrop);

        let rect = cx.centered_cols(96, height * 85 / 100);
        let n = self.matches.len();
        let count = if self.error.is_some() {
            ("error".to_string(), Tone::Danger)
        } else if self.pattern.is_empty() {
            ("ready".to_string(), Tone::Neutral)
        } else {
            (format!("{} match{}", n, if n == 1 { "" } else { "es" }), if n > 0 { Tone::Success } else { Tone::Neutral })
        };
        let spec = PanelSpec::new("Regex Playground")
            .sub("live matching")
            .badge(&count.0, count.1)
            .hints(&[
                ("Tab", "switch field"),
                ("Ctrl+I", "case"),
                ("Ctrl+M", "multiline"),
                ("Ctrl+S", "dot-all"),
                ("Esc", "close"),
            ]);
        let body = cx.panel(rect, &spec);
        let rh = tk.row_h;
        let mut y = body.y;

        // ── Pattern field
        cx.section(body.x, y, body.w, "Pattern");
        y += rh;
        let input = Rect::new(body.x, y, body.w, tk.input_h);
        cx.text_input(
            input,
            &self.pattern,
            self.pattern.chars().count(),
            None,
            "(type a pattern)",
            self.active_field == Field::Pattern,
        );
        y += tk.input_h + tk.sp.sm;

        // ── Flags + status
        let mut fx = body.x;
        for (label, on) in [
            ("[i] case-insensitive", self.flags.case_insensitive),
            ("[m] multiline", self.flags.multiline),
            ("[s] dot-all", self.flags.dot_all),
        ] {
            let w = cx.badge_w(label);
            if fx + w > body.right() {
                break;
            }
            cx.badge_line(fx, y, label, if on { Tone::Success } else { Tone::Neutral });
            fx += w + tk.sp.sm;
        }
        y += rh;
        if let Some(ref err) = self.error {
            cx.line_fit(body.x, y, body.w, err, tk.danger);
        } else if self.pattern.is_empty() {
            cx.line_fit(body.x, y, body.w, "Type a pattern to begin", tk.text_muted);
        }
        y += rh + tk.sp.xs;

        // ── Test text
        cx.section(body.x, y, body.w, "Test text");
        y += rh;
        let avail = body.bottom().saturating_sub(y);
        let box_h = (6 * tk.ch + 2 * tk.sp.md).min(avail / 2).max(2 * tk.ch);
        self.render_test_text(&mut cx, Rect::new(body.x, y, body.w, box_h));
        y += box_h + tk.sp.md;

        // ── Matches
        let header = format!("Matches ({})", n);
        cx.section(body.x, y, body.w, &header);
        y += rh;
        let bottom = body.bottom();
        let text_chars: Vec<char> = self.test_text.chars().collect();
        if self.matches.is_empty() {
            if self.error.is_none() && !self.pattern.is_empty() && y + rh <= bottom {
                cx.line(body.x, y, "No matches", tk.text_muted);
            }
            return;
        }
        let mut shown = 0usize;
        for (i, m) in self.matches.iter().enumerate() {
            let need = if m.groups.is_empty() { rh } else { 2 * rh };
            if y + need > bottom {
                break;
            }
            let color = match_color(&tk, i);
            let pos_str = format!("{}. [{}-{}]", i + 1, m.start, m.end);
            cx.line(body.x, y, &pos_str, color);
            let tx = body.x + cx.tw(&pos_str) + tk.sp.md;
            let quoted = format!("\"{}\"", m.text.replace('\n', "\\n"));
            cx.line_fit(tx, y, body.right().saturating_sub(tx), &quoted, tk.text);
            y += rh;
            if !m.groups.is_empty() {
                let parts: Vec<String> = m
                    .groups
                    .iter()
                    .enumerate()
                    .map(|(gi, &(s, e))| {
                        let gtext: String = text_chars.get(s..e).map(|sl| sl.iter().collect()).unwrap_or_default();
                        format!("${}=\"{}\"", gi + 1, gtext.replace('\n', "\\n"))
                    })
                    .collect();
                let gx = body.x + tk.sp.lg;
                cx.line_fit(gx, y, body.right().saturating_sub(gx), &format!("groups: {}", parts.join("  ")), tk.text_muted);
                y += rh;
            }
            shown += 1;
        }
        if n > shown && y + rh <= bottom + rh {
            let more = format!("+{} more", n - shown);
            let mw = cx.tw(&more);
            cx.text(body.right().saturating_sub(mw), bottom.saturating_sub(rh) + (rh - tk.ch) / 2, &more, tk.text_faint);
        }
    }

    fn render_test_text(&self, cx: &mut crate::ui::kit::Ctx, r: crate::ui::kit::Rect) {
        let tk = cx.tk;
        let (cw, ch) = (tk.cw, tk.ch);
        let is_active = self.active_field == Field::TestText;
        cx.fill_rrect(r, tk.radius_sm, if is_active { tk.accent } else { tk.border_strong });
        cx.fill_rrect(r.inset(1, 1), tk.radius_sm.saturating_sub(1), tk.field);

        let pad = tk.sp.md;
        let inner_x = r.x + pad;
        let inner_y = r.y + tk.sp.sm;
        let max_cols = (r.w.saturating_sub(2 * pad) / cw.max(1)).max(1);
        let max_rows = (r.h.saturating_sub(2 * tk.sp.sm) / ch.max(1)).max(1);

        if self.test_text.is_empty() {
            cx.text_fit(inner_x, inner_y, r.w.saturating_sub(2 * pad), "(type sample text to match against)", tk.text_faint);
        }

        let mut gi = 0usize; // global char offset into test_text
        let mut row = 0usize;
        'lines: for line in self.test_text.split('\n') {
            let line_chars: Vec<char> = line.chars().collect();
            let mut li = 0usize;
            loop {
                if row >= max_rows {
                    break 'lines;
                }
                let take = line_chars.len().saturating_sub(li).min(max_cols);
                let y = inner_y + row * ch;
                let slice_base = gi + li;

                // Render in runs of matched/unmatched so each match gets a
                // single highlighted background + one text draw call.
                let mut k = 0usize;
                while k < take {
                    let gpos0 = slice_base + k;
                    let midx0 = self.matches.iter().position(|m| gpos0 >= m.start && gpos0 < m.end);
                    let mut k2 = k + 1;
                    while k2 < take {
                        let gpos = slice_base + k2;
                        let midx = self.matches.iter().position(|m| gpos >= m.start && gpos < m.end);
                        if midx != midx0 {
                            break;
                        }
                        k2 += 1;
                    }
                    let run: String = line_chars[li + k..li + k2].iter().collect();
                    let x = inner_x + k * cw;
                    if let Some(mi) = midx0 {
                        let color = match_color(tk, mi);
                        let bg = crate::ui::kit::mix(tk.field, color, 0.28);
                        cx.fill(crate::ui::kit::Rect::new(x, y, (k2 - k) * cw, ch), bg);
                        cx.text(x, y, &run, color);
                    } else {
                        cx.text(x, y, &run, tk.text);
                    }
                    k = k2;
                }

                li += take;
                row += 1;
                if li >= line_chars.len() {
                    break;
                }
            }
            gi += line_chars.len() + 1; // +1 for the '\n' separator
        }

        if is_active {
            let nl_count = self.test_text.matches('\n').count();
            let last_line_len = self.test_text.split('\n').last().map(|l| l.chars().count()).unwrap_or(0);
            let cur_row = nl_count.min(max_rows.saturating_sub(1));
            let cur_col = last_line_len.min(max_cols.saturating_sub(1));
            cx.fill(
                crate::ui::kit::Rect::new(inner_x + cur_col * cw, inner_y + cur_row * ch, 2 * tk.scale, ch),
                tk.accent,
            );
        }
    }
}

/// Alternate success/accent per match index — also means two touching
/// matches (no gap between them) always render in visibly different colors.
fn match_color(tk: &crate::ui::kit::Tokens, idx: usize) -> Rgb {
    if idx % 2 == 0 { tk.success } else { tk.accent }
}

// ── Mini regex engine ──
//
// Pattern -> AST (`Node`) via a small recursive-descent `Parser`, AST ->
// flat bytecode (`Inst`) via `compile`, bytecode run with backtracking via
// `run`. Capturing groups are tracked as pairs of "save" slots, same trick
// real backtracking engines use. A step counter and a call-depth counter
// bound worst-case work so a pathological pattern degrades to the
// "complex" error message instead of freezing the UI thread or blowing
// the stack.

#[derive(Clone)]
enum Node {
    Char(char),
    Any,
    Class(ClassSpec),
    Start,
    End,
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Group(usize, Box<Node>),
    Star(Box<Node>),
    Plus(Box<Node>),
    Quest(Box<Node>),
}

#[derive(Clone)]
struct ClassSpec {
    negate: bool,
    ranges: Vec<(char, char)>,
}

impl ClassSpec {
    fn matches(&self, c: char, ci: bool) -> bool {
        let check = |ch: char| self.ranges.iter().any(|&(lo, hi)| ch >= lo && ch <= hi);
        let hit = if ci {
            check(c) || check(c.to_ascii_lowercase()) || check(c.to_ascii_uppercase())
        } else {
            check(c)
        };
        hit != self.negate
    }
}

fn digit_class(negate: bool) -> ClassSpec {
    ClassSpec { negate, ranges: vec![('0', '9')] }
}
fn word_class(negate: bool) -> ClassSpec {
    ClassSpec { negate, ranges: vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')] }
}
fn space_class(negate: bool) -> ClassSpec {
    ClassSpec { negate, ranges: vec![(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r'), ('\x0B', '\x0C')] }
}

enum ClassAtom {
    Single(char),
    Predefined(ClassSpec),
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    group_count: usize,
}

impl Parser {
    fn new(pattern: &str) -> Self {
        Self { chars: pattern.chars().collect(), pos: 0, group_count: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn peek_at(&self, off: usize) -> Option<char> {
        self.chars.get(self.pos + off).copied()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn parse(&mut self) -> Result<Node, String> {
        let node = self.parse_alt()?;
        if self.peek().is_some() {
            return Err("Unbalanced parentheses — unexpected ')'".to_string());
        }
        Ok(node)
    }

    fn parse_alt(&mut self) -> Result<Node, String> {
        let mut branches = vec![self.parse_concat()?];
        while self.peek() == Some('|') {
            self.bump();
            branches.push(self.parse_concat()?);
        }
        if branches.len() == 1 {
            Ok(branches.pop().unwrap())
        } else {
            Ok(Node::Alt(branches))
        }
    }

    fn parse_concat(&mut self) -> Result<Node, String> {
        let mut nodes = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            nodes.push(self.parse_repeat()?);
        }
        Ok(match nodes.len() {
            0 => Node::Concat(Vec::new()),
            1 => nodes.pop().unwrap(),
            _ => Node::Concat(nodes),
        })
    }

    fn parse_repeat(&mut self) -> Result<Node, String> {
        let atom = self.parse_atom()?;
        match self.peek() {
            Some('*') => {
                self.bump();
                self.reject_stacked_quantifier()?;
                Ok(Node::Star(Box::new(atom)))
            }
            Some('+') => {
                self.bump();
                self.reject_stacked_quantifier()?;
                Ok(Node::Plus(Box::new(atom)))
            }
            Some('?') => {
                self.bump();
                self.reject_stacked_quantifier()?;
                Ok(Node::Quest(Box::new(atom)))
            }
            Some('{') if self.looks_like_bounded_repeat() => Err(COMPLEX_MSG.to_string()),
            _ => Ok(atom),
        }
    }

    /// A second quantifier char right after the first (`a**`, or a lazy
    /// `a*?`) is outside what this engine supports.
    fn reject_stacked_quantifier(&mut self) -> Result<(), String> {
        if matches!(self.peek(), Some('*') | Some('+') | Some('?')) {
            return Err(COMPLEX_MSG.to_string());
        }
        Ok(())
    }

    /// We're positioned at `{`; does it look like `{n}`, `{n,}` or
    /// `{n,m}`? If so it's a bounded-repeat we don't support — caller
    /// bails rather than treating `{`/`}` as literals.
    fn looks_like_bounded_repeat(&self) -> bool {
        let mut i = self.pos + 1;
        let mut saw_digit = false;
        while matches!(self.chars.get(i), Some(c) if c.is_ascii_digit()) {
            saw_digit = true;
            i += 1;
        }
        if self.chars.get(i) == Some(&',') {
            i += 1;
            while matches!(self.chars.get(i), Some(c) if c.is_ascii_digit()) {
                saw_digit = true;
                i += 1;
            }
        }
        saw_digit && self.chars.get(i) == Some(&'}')
    }

    fn parse_atom(&mut self) -> Result<Node, String> {
        let c = self.peek().ok_or_else(|| "Unexpected end of pattern".to_string())?;
        match c {
            '*' | '+' | '?' => Err(COMPLEX_MSG.to_string()), // nothing to repeat
            '(' => {
                self.bump();
                if self.peek() == Some('?') {
                    // (?:...), (?=...), (?<name>...) etc — not supported.
                    return Err(COMPLEX_MSG.to_string());
                }
                self.group_count += 1;
                let gidx = self.group_count;
                let inner = self.parse_alt()?;
                if self.peek() != Some(')') {
                    return Err("Unbalanced parentheses — missing ')'".to_string());
                }
                self.bump();
                Ok(Node::Group(gidx, Box::new(inner)))
            }
            ')' => Err("Unbalanced parentheses — unexpected ')'".to_string()),
            '[' => self.parse_class(),
            '.' => {
                self.bump();
                Ok(Node::Any)
            }
            '^' => {
                self.bump();
                Ok(Node::Start)
            }
            '$' => {
                self.bump();
                Ok(Node::End)
            }
            '\\' => self.parse_escape(),
            _ => {
                self.bump();
                Ok(Node::Char(c))
            }
        }
    }

    fn parse_escape(&mut self) -> Result<Node, String> {
        self.bump(); // consume '\'
        let c = self.bump().ok_or_else(|| "Incomplete escape sequence at end of pattern".to_string())?;
        Ok(match c {
            '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\' | '/' => Node::Char(c),
            'n' => Node::Char('\n'),
            't' => Node::Char('\t'),
            'r' => Node::Char('\r'),
            'd' => Node::Class(digit_class(false)),
            'D' => Node::Class(digit_class(true)),
            'w' => Node::Class(word_class(false)),
            'W' => Node::Class(word_class(true)),
            's' => Node::Class(space_class(false)),
            'S' => Node::Class(space_class(true)),
            _ => return Err(COMPLEX_MSG.to_string()), // unknown escape / backreference
        })
    }

    fn parse_class(&mut self) -> Result<Node, String> {
        self.bump(); // consume '['
        let negate = if self.peek() == Some('^') {
            self.bump();
            true
        } else {
            false
        };
        let mut ranges = Vec::new();
        let mut first = true;
        loop {
            match self.peek() {
                None => return Err("Unterminated character class — missing ']'".to_string()),
                Some(']') if !first => {
                    self.bump();
                    break;
                }
                _ => {}
            }
            first = false;
            match self.class_char()? {
                ClassAtom::Predefined(spec) => ranges.extend(spec.ranges),
                ClassAtom::Single(lo) => {
                    let is_range = self.peek() == Some('-') && matches!(self.peek_at(1), Some(c) if c != ']');
                    if is_range {
                        self.bump(); // consume '-'
                        match self.class_char()? {
                            ClassAtom::Single(hi) => ranges.push((lo, hi)),
                            ClassAtom::Predefined(_) => return Err(COMPLEX_MSG.to_string()),
                        }
                    } else {
                        ranges.push((lo, lo));
                    }
                }
            }
        }
        if ranges.is_empty() {
            return Err("Empty character class".to_string());
        }
        Ok(Node::Class(ClassSpec { negate, ranges }))
    }

    fn class_char(&mut self) -> Result<ClassAtom, String> {
        let c = self.bump().ok_or_else(|| "Unterminated character class — missing ']'".to_string())?;
        if c != '\\' {
            return Ok(ClassAtom::Single(c));
        }
        let e = self.bump().ok_or_else(|| "Incomplete escape sequence in character class".to_string())?;
        Ok(match e {
            ']' | '\\' | '^' | '-' | '.' | '[' => ClassAtom::Single(e),
            'n' => ClassAtom::Single('\n'),
            't' => ClassAtom::Single('\t'),
            'r' => ClassAtom::Single('\r'),
            'd' => ClassAtom::Predefined(digit_class(false)),
            'w' => ClassAtom::Predefined(word_class(false)),
            's' => ClassAtom::Predefined(space_class(false)),
            _ => return Err(COMPLEX_MSG.to_string()),
        })
    }
}

// ── Bytecode ──

#[derive(Clone)]
enum SimpleAtom {
    Char(char),
    Any,
    Class(usize),
}

#[derive(Clone)]
enum Inst {
    Char(char),
    Any,
    Class(usize),
    Start,
    End,
    Save(usize),
    Jmp(usize),
    Split(usize, usize),
    /// Greedy repetition of a single char/any/class atom, consumed and
    /// backtracked iteratively (no call-stack growth per repetition) —
    /// the fast path for the extremely common `\d+`, `.*`, `[a-z]*` etc.
    RepeatSimple(SimpleAtom, u32),
    Match,
}

struct Program {
    insts: Vec<Inst>,
    classes: Vec<ClassSpec>,
    group_count: usize,
}

fn as_simple_atom(node: &Node, classes: &mut Vec<ClassSpec>) -> Option<SimpleAtom> {
    match node {
        Node::Char(c) => Some(SimpleAtom::Char(*c)),
        Node::Any => Some(SimpleAtom::Any),
        Node::Class(spec) => {
            let id = classes.len();
            classes.push(spec.clone());
            Some(SimpleAtom::Class(id))
        }
        _ => None,
    }
}

fn compile(node: &Node, group_count: usize) -> Program {
    let mut insts = Vec::new();
    let mut classes = Vec::new();
    insts.push(Inst::Save(0));
    emit(node, &mut insts, &mut classes);
    insts.push(Inst::Save(1));
    insts.push(Inst::Match);
    Program { insts, classes, group_count }
}

fn emit(node: &Node, insts: &mut Vec<Inst>, classes: &mut Vec<ClassSpec>) {
    match node {
        Node::Char(c) => insts.push(Inst::Char(*c)),
        Node::Any => insts.push(Inst::Any),
        Node::Class(spec) => {
            let id = classes.len();
            classes.push(spec.clone());
            insts.push(Inst::Class(id));
        }
        Node::Start => insts.push(Inst::Start),
        Node::End => insts.push(Inst::End),
        Node::Concat(nodes) => {
            for n in nodes {
                emit(n, insts, classes);
            }
        }
        Node::Alt(branches) => {
            let mut jmp_fixups = Vec::new();
            for (i, br) in branches.iter().enumerate() {
                if i + 1 < branches.len() {
                    let split_idx = insts.len();
                    insts.push(Inst::Split(0, 0));
                    let branch_start = insts.len();
                    emit(br, insts, classes);
                    let jmp_idx = insts.len();
                    insts.push(Inst::Jmp(0));
                    jmp_fixups.push(jmp_idx);
                    let next_branch_start = insts.len();
                    insts[split_idx] = Inst::Split(branch_start, next_branch_start);
                } else {
                    emit(br, insts, classes);
                }
            }
            let end = insts.len();
            for idx in jmp_fixups {
                insts[idx] = Inst::Jmp(end);
            }
        }
        Node::Group(gidx, inner) => {
            insts.push(Inst::Save(2 * gidx));
            emit(inner, insts, classes);
            insts.push(Inst::Save(2 * gidx + 1));
        }
        Node::Star(inner) => {
            if let Some(atom) = as_simple_atom(inner, classes) {
                insts.push(Inst::RepeatSimple(atom, 0));
            } else {
                let l1 = insts.len();
                insts.push(Inst::Split(0, 0));
                let l2 = insts.len();
                emit(inner, insts, classes);
                insts.push(Inst::Jmp(l1));
                let l3 = insts.len();
                insts[l1] = Inst::Split(l2, l3);
            }
        }
        Node::Plus(inner) => {
            if let Some(atom) = as_simple_atom(inner, classes) {
                insts.push(Inst::RepeatSimple(atom, 1));
            } else {
                let l1 = insts.len();
                emit(inner, insts, classes);
                let split_idx = insts.len();
                insts.push(Inst::Split(0, 0));
                let l2 = insts.len();
                insts[split_idx] = Inst::Split(l1, l2);
            }
        }
        Node::Quest(inner) => {
            let split_idx = insts.len();
            insts.push(Inst::Split(0, 0));
            let l1 = insts.len();
            emit(inner, insts, classes);
            let l2 = insts.len();
            insts[split_idx] = Inst::Split(l1, l2);
        }
    }
}

fn compile_pattern(pattern: &str) -> Result<Program, String> {
    let mut p = Parser::new(pattern);
    let ast = p.parse()?;
    Ok(compile(&ast, p.group_count))
}

/// Does `pattern` match anywhere in `text`? `Err` for an invalid pattern,
/// `Ok(None)` when it is too complex to decide (callers treat that as no match).
/// Used by the agent policy engine (`agents::policy`) for `command_regex`.
pub fn is_match(pattern: &str, text: &str) -> Result<Option<bool>, String> {
    let prog = compile_pattern(pattern)?;
    let ctx = MatchCtx { case_insensitive: false, multiline: false, dot_all: true };
    let (m, overflowed) = find_all(text, &prog, &ctx);
    Ok(if overflowed { None } else { Some(!m.is_empty()) })
}

// ── VM ──

struct MatchCtx {
    case_insensitive: bool,
    multiline: bool,
    dot_all: bool,
}

const STEP_BUDGET: u32 = 2_000_000;
const MAX_DEPTH: u32 = 4_000;
const MAX_MATCHES: usize = 500;

fn char_eq(a: char, b: char, ci: bool) -> bool {
    if ci {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

fn simple_atom_matches(atom: &SimpleAtom, c: char, classes: &[ClassSpec], ctx: &MatchCtx) -> bool {
    match atom {
        SimpleAtom::Char(ch) => char_eq(c, *ch, ctx.case_insensitive),
        SimpleAtom::Any => ctx.dot_all || c != '\n',
        SimpleAtom::Class(id) => classes[*id].matches(c, ctx.case_insensitive),
    }
}

/// Backtracking VM. Straight-line instructions loop in place (no
/// recursion); `Split` recurses once per choice point and `RepeatSimple`
/// backtracks its repetition count iteratively — the only *unbounded*
/// recursion is repeated complex (non-simple-atom) groups, which `depth`
/// caps well short of a stack overflow.
#[allow(clippy::too_many_arguments)]
fn run(
    insts: &[Inst],
    classes: &[ClassSpec],
    pc: usize,
    chars: &[char],
    pos: usize,
    saves: &mut Vec<Option<usize>>,
    ctx: &MatchCtx,
    steps: &mut u32,
    depth: &mut u32,
) -> Option<usize> {
    *depth += 1;
    if *depth > MAX_DEPTH {
        return None;
    }
    let mut pc = pc;
    let mut pos = pos;
    loop {
        *steps += 1;
        if *steps > STEP_BUDGET {
            return None;
        }
        match &insts[pc] {
            Inst::Match => return Some(pos),
            Inst::Char(c) => {
                if pos < chars.len() && char_eq(chars[pos], *c, ctx.case_insensitive) {
                    pos += 1;
                    pc += 1;
                } else {
                    return None;
                }
            }
            Inst::Any => {
                if pos < chars.len() && (ctx.dot_all || chars[pos] != '\n') {
                    pos += 1;
                    pc += 1;
                } else {
                    return None;
                }
            }
            Inst::Class(id) => {
                if pos < chars.len() && classes[*id].matches(chars[pos], ctx.case_insensitive) {
                    pos += 1;
                    pc += 1;
                } else {
                    return None;
                }
            }
            Inst::Start => {
                let ok = pos == 0 || (ctx.multiline && pos > 0 && chars[pos - 1] == '\n');
                if !ok {
                    return None;
                }
                pc += 1;
            }
            Inst::End => {
                let ok = pos == chars.len() || (ctx.multiline && chars[pos] == '\n');
                if !ok {
                    return None;
                }
                pc += 1;
            }
            Inst::Save(slot) => {
                if *slot < saves.len() {
                    saves[*slot] = Some(pos);
                }
                pc += 1;
            }
            Inst::Jmp(t) => pc = *t,
            Inst::Split(a, b) => {
                let snapshot = saves.clone();
                if let Some(end) = run(insts, classes, *a, chars, pos, saves, ctx, steps, depth) {
                    return Some(end);
                }
                *saves = snapshot;
                pc = *b;
            }
            Inst::RepeatSimple(atom, min) => {
                let mut count = 0u32;
                let mut p = pos;
                while p < chars.len() && simple_atom_matches(atom, chars[p], classes, ctx) {
                    p += 1;
                    count += 1;
                }
                *steps = steps.saturating_add(count);
                if *steps > STEP_BUDGET {
                    return None;
                }
                let next_pc = pc + 1;
                loop {
                    if count >= *min {
                        let snapshot = saves.clone();
                        if let Some(end) = run(insts, classes, next_pc, chars, p, saves, ctx, steps, depth) {
                            return Some(end);
                        }
                        *saves = snapshot;
                    }
                    if count == 0 {
                        return None;
                    }
                    count -= 1;
                    p -= 1;
                    *steps += 1;
                    if *steps > STEP_BUDGET {
                        return None;
                    }
                }
            }
        }
    }
}

/// Scan `text` left to right for every non-overlapping match. Returns the
/// matches found so far plus whether the step budget was exhausted (in
/// which case results may be incomplete and the caller should surface the
/// "too complex" message).
fn find_all(text: &str, prog: &Program, ctx: &MatchCtx) -> (Vec<MatchResult>, bool) {
    let chars: Vec<char> = text.chars().collect();
    let slot_count = 2 * (prog.group_count + 1);
    let mut results = Vec::new();
    let mut pos = 0usize;
    let mut steps = 0u32;
    let mut overflowed = false;

    while pos <= chars.len() {
        let mut saves: Vec<Option<usize>> = vec![None; slot_count];
        let mut depth = 0u32;
        match run(&prog.insts, &prog.classes, 0, &chars, pos, &mut saves, ctx, &mut steps, &mut depth) {
            Some(_) => {
                let mstart = saves.first().copied().flatten().unwrap_or(pos);
                let mend = saves.get(1).copied().flatten().unwrap_or(mstart);
                let text_slice: String = chars.get(mstart..mend).map(|s| s.iter().collect()).unwrap_or_default();
                let mut groups = Vec::with_capacity(prog.group_count);
                for g in 1..=prog.group_count {
                    let s = saves.get(2 * g).copied().flatten();
                    let e = saves.get(2 * g + 1).copied().flatten();
                    groups.push(match (s, e) {
                        (Some(s), Some(e)) => (s, e),
                        _ => (mend, mend), // group didn't participate (e.g. unmatched alt)
                    });
                }
                results.push(MatchResult { start: mstart, end: mend, text: text_slice, groups });
                pos = if mend > mstart { mend } else { mstart + 1 };
            }
            None => pos += 1,
        }
        if steps > STEP_BUDGET {
            overflowed = true;
            break;
        }
        if results.len() >= MAX_MATCHES {
            break;
        }
    }
    (results, overflowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_ctx() -> MatchCtx {
        MatchCtx { case_insensitive: false, multiline: false, dot_all: false }
    }

    fn run_pattern(pattern: &str, text: &str, ctx: &MatchCtx) -> Vec<MatchResult> {
        let prog = compile_pattern(pattern).expect("pattern should compile");
        find_all(text, &prog, ctx).0
    }

    fn texts(matches: &[MatchResult]) -> Vec<&str> {
        matches.iter().map(|m| m.text.as_str()).collect()
    }

    #[test]
    fn literal_substring() {
        let m = run_pattern("cat", "concatenate cats", &default_ctx());
        assert_eq!(texts(&m), vec!["cat", "cat"]);
        assert_eq!((m[0].start, m[0].end), (3, 6)); // "con[cat]enate cats"
    }

    #[test]
    fn dot_wildcard() {
        let m = run_pattern("c.t", "cat cot c\nt", &default_ctx());
        // dot_all is off, so "c\nt" must NOT match across the newline.
        assert_eq!(texts(&m), vec!["cat", "cot"]);
    }

    #[test]
    fn dot_all_flag_matches_newline() {
        let ctx = MatchCtx { dot_all: true, ..default_ctx() };
        let m = run_pattern("c.t", "c\nt", &ctx);
        assert_eq!(texts(&m), vec!["c\nt"]);
    }

    #[test]
    fn star_plus_question() {
        assert_eq!(texts(&run_pattern("ab*c", "ac abc abbbc", &default_ctx())), vec!["ac", "abc", "abbbc"]);
        assert_eq!(texts(&run_pattern("ab+c", "ac abc abbbc", &default_ctx())), vec!["abc", "abbbc"]);
        assert_eq!(texts(&run_pattern("colou?r", "color colour colouur", &default_ctx())), vec!["color", "colour"]);
    }

    #[test]
    fn character_classes() {
        assert_eq!(texts(&run_pattern("[a-c]+", "abcx def", &default_ctx())), vec!["abc"]);
        assert_eq!(texts(&run_pattern("[^0-9]+", "ab12cd34", &default_ctx())), vec!["ab", "cd"]);
        assert_eq!(texts(&run_pattern(r"\d+", "room 42, aisle 7", &default_ctx())), vec!["42", "7"]);
        assert_eq!(texts(&run_pattern(r"\w+", "foo_1 -- bar2", &default_ctx())), vec!["foo_1", "bar2"]);
    }

    #[test]
    fn anchors_without_multiline() {
        let ctx = default_ctx();
        // ^/$ only match string start/end when multiline is off.
        assert_eq!(texts(&run_pattern("^abc", "abc\nabc", &ctx)), vec!["abc"]);
        assert_eq!(texts(&run_pattern("abc$", "abc\nabc", &ctx)), vec!["abc"]);
    }

    #[test]
    fn anchors_with_multiline() {
        let ctx = MatchCtx { multiline: true, ..default_ctx() };
        assert_eq!(texts(&run_pattern("^abc", "abc\nabc", &ctx)), vec!["abc", "abc"]);
        assert_eq!(texts(&run_pattern("abc$", "abc\nabc", &ctx)), vec!["abc", "abc"]);
    }

    #[test]
    fn alternation() {
        assert_eq!(texts(&run_pattern("cat|dog", "I have a cat and a dog", &default_ctx())), vec!["cat", "dog"]);
    }

    #[test]
    fn case_insensitive_flag() {
        let ctx = MatchCtx { case_insensitive: true, ..default_ctx() };
        assert_eq!(texts(&run_pattern("cat", "CAT Cat caT", &ctx)), vec!["CAT", "Cat", "caT"]);
        assert!(run_pattern("cat", "CAT", &default_ctx()).is_empty());
    }

    #[test]
    fn capturing_groups() {
        let m = run_pattern(r"(\w+)@(\w+)", "mail me at alice@example", &default_ctx());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].text, "alice@example");
        assert_eq!(m[0].groups.len(), 2);
        let (s0, e0) = m[0].groups[0];
        let (s1, e1) = m[0].groups[1];
        // Group spans are char offsets into the original text, not into `m[0].text`.
        let chars: Vec<char> = "mail me at alice@example".chars().collect();
        let g0: String = chars[s0..e0].iter().collect();
        let g1: String = chars[s1..e1].iter().collect();
        assert_eq!(g0, "alice");
        assert_eq!(g1, "example");
    }

    #[test]
    fn alternation_inside_group() {
        assert_eq!(texts(&run_pattern("gr(a|e)y", "gray grey groy", &default_ctx())), vec!["gray", "grey"]);
    }

    #[test]
    fn email_like_pattern_from_first_run_sample() {
        let text = "contact: alice@example.com\nbackup: bob@example.org\n(no match on this line)";
        let m = run_pattern(r"\w+@\w+\.\w+", text, &default_ctx());
        assert_eq!(texts(&m), vec!["alice@example.com", "bob@example.org"]);
    }

    #[test]
    fn unsupported_constructs_report_complex_message() {
        for pattern in ["a{2,3}", "a*?", "(?:abc)", "(?=abc)", r"\1", "a**"] {
            match compile_pattern(pattern) {
                Err(e) => assert_eq!(e, COMPLEX_MSG, "pattern {pattern:?} should be rejected as complex"),
                Ok(_) => panic!("pattern {pattern:?} should have been rejected as complex"),
            }
        }
    }

    #[test]
    fn malformed_patterns_report_specific_errors() {
        assert!(compile_pattern("(abc").is_err());
        assert!(compile_pattern("abc)").is_err());
        assert!(compile_pattern("[abc").is_err());
        assert!(compile_pattern(r"abc\").is_err());
    }

    #[test]
    fn literal_braces_without_bounded_repeat_shape() {
        // "{" not shaped like {n} / {n,} / {n,m} is just a literal brace.
        assert_eq!(texts(&run_pattern(r"\{hello\}", "say {hello} now", &default_ctx())), vec!["{hello}"]);
    }

    #[test]
    fn empty_pattern_short_circuits_before_engine() {
        // RegexPlayground::recompile() guards this; verify the struct-level behavior.
        let mut rp = RegexPlayground::new();
        rp.test_text = "anything".to_string();
        rp.recompile();
        assert!(rp.matches.is_empty());
        assert!(rp.error.is_none());
    }

    #[test]
    fn playground_toggle_seeds_first_run_example_and_recompiles() {
        let mut rp = RegexPlayground::new();
        assert!(rp.pattern.is_empty());
        rp.toggle();
        assert!(rp.visible);
        assert!(!rp.pattern.is_empty());
        assert!(!rp.matches.is_empty(), "seeded example should already have matches");
    }

    #[test]
    fn handle_key_typing_and_field_switch() {
        let mut rp = RegexPlayground::new();
        rp.visible = true;
        for c in "a+".chars() {
            rp.handle_key(RegexKey::Char(c));
        }
        assert_eq!(rp.pattern, "a+");
        assert!(rp.error.is_none());

        rp.handle_key(RegexKey::Tab);
        assert_eq!(rp.active_field, Field::TestText);
        for c in "aaa bbb".chars() {
            rp.handle_key(RegexKey::Char(c));
        }
        assert_eq!(rp.test_text, "aaa bbb");
        assert_eq!(texts(&rp.matches), vec!["aaa"]);

        rp.handle_key(RegexKey::Backspace);
        assert_eq!(rp.test_text, "aaa bb");

        rp.handle_key(RegexKey::CtrlM);
        assert!(rp.flags.multiline);
        rp.handle_key(RegexKey::CtrlI);
        assert!(rp.flags.case_insensitive);
        rp.handle_key(RegexKey::CtrlS);
        assert!(rp.flags.dot_all);

        rp.handle_key(RegexKey::Escape);
        assert!(!rp.visible);
    }

    #[test]
    fn long_simple_repetition_does_not_blow_the_stack() {
        // Exercises the RepeatSimple fast path: a naive recursive-per-char
        // backtracking VM would overflow the stack here.
        let text = "a".repeat(200_000) + "b";
        let m = run_pattern("a+b", &text, &default_ctx());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].end - m[0].start, 200_001);
    }

    #[test]
    fn pathological_backtracking_degrades_gracefully() {
        // Classic catastrophic-backtracking shape, but over a complex
        // (grouped) repeated atom so it can't take the RepeatSimple fast
        // path. Must not hang or crash — either it fails to find a match
        // within budget, or it degrades to the overflow flag.
        let pattern = "(a|a)*b";
        let text = "a".repeat(40) + "c"; // no trailing 'b' -> forces full backtrack search
        let prog = compile_pattern(pattern).expect("pattern should compile");
        let ctx = default_ctx();
        let (_, _overflowed) = find_all(&text, &prog, &ctx);
        // Reaching this line at all (without a stack overflow/hang) is the test.
    }
}
