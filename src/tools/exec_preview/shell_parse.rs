//! A small POSIX-ish shell lexer/parser, just good enough to answer "which
//! simple commands would this line actually run?" for the Preview-Then-Accept
//! safety rules. It is deliberately forgiving: it never fails, never
//! allocates unboundedly and never executes or expands anything.
//!
//! Handled: single/double quotes, `\` escapes (`\rm`, `r\m`), `$'..'`,
//! `$(...)`, `` `...` ``, `<(...)`/`>(...)` (all parsed recursively), `$((..))`
//! (opaque), `${...}` (opaque), pipes (`|`, `|&`), lists (`&&`, `||`, `;`,
//! `&`, newlines), subshells/groups, function-definition headers, reserved
//! words (`if`/`then`/`do`/`{`/...), leading `VAR=value` assignments,
//! redirections (`>`, `>>`, `>|`, `&>`, `2>`, `<`, `<<`, `<<<`, `>&2`, ...),
//! and here-document bodies.

/// One shell word after quote removal. Unresolved expansions keep their
/// source spelling (`$HOME`, `${X}`, `$(…)`) and set `dynamic`.
#[derive(Debug, Clone, Default)]
pub struct Word {
    pub text: String,
    /// Command/process substitutions found inside the word, parsed.
    pub subs: Vec<Script>,
    /// Any part of the word was quoted or escaped.
    pub quoted: bool,
    /// Contains `$var`, `${..}`, `$(..)`, backticks or `$((..))`.
    pub dynamic: bool,
    /// Contains an unquoted glob character (`* ? [`).
    pub glob: bool,
}

#[derive(Debug, Clone)]
pub struct Redir {
    pub fd: Option<u32>,
    /// `>`, `>>`, `>|`, `&>`, `&>>`, `>&`, `<`, `<<`, `<<-`, `<<<`, `<&`, `<>`.
    pub op: String,
    pub target: Word,
}

#[derive(Debug, Clone, Default)]
pub struct Simple {
    pub assigns: Vec<(String, Word)>,
    pub words: Vec<Word>,
    pub redirs: Vec<Redir>,
}

impl Simple {
    fn is_empty(&self) -> bool {
        self.words.is_empty() && self.assigns.is_empty() && self.redirs.is_empty()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Pipeline {
    pub cmds: Vec<Simple>,
    /// Text fed to stdin by here-documents / here-strings on this line.
    pub stdin_texts: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Script {
    pub pipelines: Vec<Pipeline>,
    /// Unquoted skeleton of the top-level text (quoted/substituted words are
    /// replaced by `Q`); used for structural patterns like the fork bomb.
    pub skeleton: String,
}

const MAX_CHARS: usize = 1 << 20;
const MAX_DEPTH: usize = 8;

pub fn parse(src: &str) -> Script {
    parse_depth(src, 0)
}

fn parse_depth(src: &str, depth: usize) -> Script {
    let chars: Vec<char> = src.chars().take(MAX_CHARS).collect();
    let mut p = Parser { chars, pos: 0, depth, skel: String::new(), heredocs: Vec::new() };
    let mut s = p.parse_list(false);
    s.skeleton = std::mem::take(&mut p.skel);
    s
}

struct PendingHeredoc {
    delim: String,
    strip_tabs: bool,
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    depth: usize,
    skel: String,
    heredocs: Vec<PendingHeredoc>,
}

const RESERVED: &[&str] = &["if", "then", "else", "elif", "fi", "do", "done", "while", "until", "esac", "{", "}", "!", "coproc"];

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\r'
}

fn is_word_end(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n' | ';' | '&' | '|' | '<' | '>' | '(' | ')')
}

fn is_assign_name(s: &str) -> bool {
    let mut it = s.chars();
    match it.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    it.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Parser {
    fn peek(&self, off: usize) -> Option<char> {
        self.chars.get(self.pos + off).copied()
    }

    fn skel_push(&mut self, s: &str) {
        if self.depth == 0 && self.skel.len() < 1 << 16 {
            self.skel.push_str(s);
        }
    }

    fn parse_list(&mut self, close_paren: bool) -> Script {
        let mut script = Script::default();
        let mut cur = Simple::default();
        let mut pipe = Pipeline::default();
        let mut line_start = 0usize;

        macro_rules! end_simple {
            () => {
                if !cur.is_empty() {
                    pipe.cmds.push(std::mem::take(&mut cur));
                }
            };
        }
        macro_rules! end_pipeline {
            () => {
                end_simple!();
                if !pipe.cmds.is_empty() || !pipe.stdin_texts.is_empty() {
                    script.pipelines.push(std::mem::take(&mut pipe));
                }
            };
        }

        loop {
            // blanks and line continuations
            while let Some(c) = self.peek(0) {
                if is_blank(c) {
                    self.pos += 1;
                } else if c == '\\' && self.peek(1) == Some('\n') {
                    self.pos += 2;
                } else {
                    break;
                }
            }
            let Some(c) = self.peek(0) else { break };
            match c {
                '#' => {
                    while let Some(c) = self.peek(0) {
                        if c == '\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                '\n' => {
                    self.pos += 1;
                    self.skel_push(";");
                    end_pipeline!();
                    if !self.heredocs.is_empty() {
                        let bodies = self.read_heredocs();
                        for b in bodies {
                            for p in script.pipelines.iter_mut().skip(line_start) {
                                p.stdin_texts.push(b.clone());
                            }
                        }
                    }
                    line_start = script.pipelines.len();
                }
                ';' => {
                    self.pos += 1;
                    if matches!(self.peek(0), Some(';') | Some('&')) {
                        self.pos += 1;
                    }
                    self.skel_push(";");
                    end_pipeline!();
                }
                '&' if self.peek(1) == Some('>') => self.parse_redirect(&mut cur, &mut pipe, None),
                '&' => {
                    self.pos += 1;
                    if self.peek(0) == Some('&') {
                        self.pos += 1;
                        self.skel_push("&&");
                    } else {
                        self.skel_push("&");
                    }
                    end_pipeline!();
                }
                '|' => {
                    self.pos += 1;
                    if self.peek(0) == Some('|') {
                        self.pos += 1;
                        self.skel_push("||");
                        end_pipeline!();
                    } else {
                        if self.peek(0) == Some('&') {
                            self.pos += 1;
                        }
                        self.skel_push("|");
                        end_simple!();
                    }
                }
                '(' => {
                    self.pos += 1;
                    self.skel_push("(");
                    // `name ()` function header: drop the name.
                    let mut k = 0;
                    while self.peek(k).map_or(false, is_blank) {
                        k += 1;
                    }
                    if !cur.words.is_empty() && self.peek(k) == Some(')') {
                        self.pos += k + 1;
                        self.skel_push(")");
                        cur = Simple::default();
                        continue;
                    }
                    end_simple!();
                    if self.depth >= MAX_DEPTH {
                        self.skip_balanced();
                        continue;
                    }
                    self.depth += 1;
                    let inner = self.parse_list(true);
                    self.depth -= 1;
                    script.pipelines.extend(inner.pipelines);
                    line_start = line_start.min(script.pipelines.len());
                }
                ')' => {
                    self.pos += 1;
                    self.skel_push(")");
                    end_pipeline!();
                    if close_paren {
                        break;
                    }
                }
                '<' | '>' if self.peek(1) != Some('(') => self.parse_redirect(&mut cur, &mut pipe, None),
                d if d.is_ascii_digit() && self.digits_then_redirect().is_some() => {
                    let (fd, len) = self.digits_then_redirect().unwrap();
                    self.pos += len;
                    self.parse_redirect(&mut cur, &mut pipe, Some(fd));
                }
                _ => {
                    let w = self.parse_word();
                    if w.text.is_empty() && !w.quoted && w.subs.is_empty() {
                        // Defensive: never loop without progress.
                        self.pos += 1;
                        continue;
                    }
                    let plain = !w.quoted && w.subs.is_empty();
                    if plain {
                        let t = w.text.clone();
                        self.skel_push(&t);
                        self.skel_push(" ");
                    } else {
                        self.skel_push("Q ");
                    }
                    if cur.words.is_empty() {
                        if plain && RESERVED.contains(&w.text.as_str()) && cur.assigns.is_empty() {
                            continue;
                        }
                        if let Some(eq) = w.text.find('=') {
                            let name = w.text[..eq].trim_end_matches('+');
                            if is_assign_name(name) && !(w.quoted && eq == 0) {
                                let mut val = w.clone();
                                val.text = w.text[eq + 1..].to_string();
                                cur.assigns.push((name.to_string(), val));
                                continue;
                            }
                        }
                    }
                    cur.words.push(w);
                }
            }
        }
        end_pipeline!();
        script
    }

    /// `2>`, `10<` ... : (fd, number of digit chars) when the digits are
    /// immediately followed by a redirection operator.
    fn digits_then_redirect(&self) -> Option<(u32, usize)> {
        let mut k = 0;
        while self.peek(k).map_or(false, |c| c.is_ascii_digit()) {
            k += 1;
            if k > 4 {
                return None;
            }
        }
        match self.peek(k) {
            Some('<') | Some('>') if self.peek(k + 1) != Some('(') => {
                let s: String = self.chars[self.pos..self.pos + k].iter().collect();
                s.parse().ok().map(|fd| (fd, k))
            }
            _ => None,
        }
    }

    fn parse_redirect(&mut self, cur: &mut Simple, pipe: &mut Pipeline, fd: Option<u32>) {
        let mut op = String::new();
        let c = self.peek(0).unwrap_or('>');
        if c == '&' {
            op.push('&');
            self.pos += 1;
            op.push('>');
            self.pos += 1;
            if self.peek(0) == Some('>') {
                op.push('>');
                self.pos += 1;
            }
        } else if c == '>' {
            op.push('>');
            self.pos += 1;
            match self.peek(0) {
                Some('>') => {
                    op.push('>');
                    self.pos += 1;
                }
                Some('|') => {
                    op.push('|');
                    self.pos += 1;
                }
                Some('&') => {
                    op.push('&');
                    self.pos += 1;
                }
                _ => {}
            }
        } else {
            op.push('<');
            self.pos += 1;
            match self.peek(0) {
                Some('<') => {
                    op.push('<');
                    self.pos += 1;
                    match self.peek(0) {
                        Some('<') => {
                            op.push('<');
                            self.pos += 1;
                        }
                        Some('-') => {
                            op.push('-');
                            self.pos += 1;
                        }
                        _ => {}
                    }
                }
                Some('&') => {
                    op.push('&');
                    self.pos += 1;
                }
                Some('>') => {
                    op.push('>');
                    self.pos += 1;
                }
                _ => {}
            }
        }
        while self.peek(0).map_or(false, is_blank) {
            self.pos += 1;
        }
        let target = if self.peek(0).map_or(true, |c| is_word_end(c) && c != '<' && c != '>') {
            Word::default()
        } else {
            self.parse_word()
        };
        if op == "<<" || op == "<<-" {
            self.heredocs.push(PendingHeredoc { delim: target.text.clone(), strip_tabs: op == "<<-" });
        } else if op == "<<<" {
            pipe.stdin_texts.push(target.text.clone());
        }
        cur.redirs.push(Redir { fd, op, target });
    }

    fn read_heredocs(&mut self) -> Vec<String> {
        let pending = std::mem::take(&mut self.heredocs);
        let mut bodies = Vec::new();
        for h in pending {
            let mut body = String::new();
            loop {
                if self.pos >= self.chars.len() {
                    break;
                }
                let start = self.pos;
                while self.pos < self.chars.len() && self.chars[self.pos] != '\n' {
                    self.pos += 1;
                }
                let line: String = self.chars[start..self.pos].iter().collect();
                if self.pos < self.chars.len() {
                    self.pos += 1;
                }
                let cmp = if h.strip_tabs { line.trim_start_matches('\t') } else { line.as_str() };
                if cmp == h.delim {
                    break;
                }
                if body.len() < 1 << 16 {
                    body.push_str(&line);
                    body.push('\n');
                }
            }
            bodies.push(body);
        }
        bodies
    }

    /// Skip a balanced `( ... )` group without parsing (depth limit hit).
    fn skip_balanced(&mut self) {
        let mut depth = 1usize;
        while let Some(c) = self.peek(0) {
            self.pos += 1;
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    fn parse_word(&mut self) -> Word {
        let mut w = Word::default();
        let start = self.pos;
        while let Some(c) = self.peek(0) {
            match c {
                '<' | '>' if self.pos == start && self.peek(1) == Some('(') => {
                    self.pos += 2;
                    self.sub_script(&mut w, if c == '<' { "<(…)" } else { ">(…)" });
                }
                c if is_word_end(c) => break,
                '\\' => match self.peek(1) {
                    Some('\n') => self.pos += 2,
                    Some(n) => {
                        w.text.push(n);
                        w.quoted = true;
                        self.pos += 2;
                    }
                    None => self.pos += 1,
                },
                '\'' => {
                    w.quoted = true;
                    self.pos += 1;
                    while let Some(c) = self.peek(0) {
                        self.pos += 1;
                        if c == '\'' {
                            break;
                        }
                        w.text.push(c);
                    }
                }
                '"' => {
                    w.quoted = true;
                    self.pos += 1;
                    self.parse_double_quoted(&mut w);
                }
                '$' => self.parse_dollar(&mut w, false),
                '`' => {
                    self.pos += 1;
                    self.parse_backtick(&mut w);
                }
                '*' | '?' | '[' => {
                    w.glob = true;
                    w.text.push(c);
                    self.pos += 1;
                }
                _ => {
                    w.text.push(c);
                    self.pos += 1;
                }
            }
        }
        w
    }

    fn parse_double_quoted(&mut self, w: &mut Word) {
        while let Some(c) = self.peek(0) {
            match c {
                '"' => {
                    self.pos += 1;
                    return;
                }
                '\\' => match self.peek(1) {
                    Some('\n') => self.pos += 2,
                    Some(n) if matches!(n, '$' | '`' | '"' | '\\') => {
                        w.text.push(n);
                        self.pos += 2;
                    }
                    _ => {
                        w.text.push('\\');
                        self.pos += 1;
                    }
                },
                '$' => self.parse_dollar(w, true),
                '`' => {
                    self.pos += 1;
                    self.parse_backtick(w);
                }
                _ => {
                    w.text.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    /// At a `$`. Handles `$(..)`, `$((..))`, `${..}`, `$name`, `$'..'`.
    fn parse_dollar(&mut self, w: &mut Word, in_dq: bool) {
        match self.peek(1) {
            Some('(') if self.peek(2) == Some('(') => {
                // arithmetic: opaque
                self.pos += 3;
                let mut depth = 2usize;
                w.text.push_str("$((…))");
                w.dynamic = true;
                while let Some(c) = self.peek(0) {
                    self.pos += 1;
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some('(') => {
                self.pos += 2;
                self.sub_script(w, "$(…)");
            }
            Some('{') => {
                w.dynamic = true;
                w.text.push_str("${");
                self.pos += 2;
                let mut depth = 1usize;
                while let Some(c) = self.peek(0) {
                    self.pos += 1;
                    w.text.push(c);
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some('\'') if !in_dq => {
                self.pos += 2;
                w.quoted = true;
                while let Some(c) = self.peek(0) {
                    self.pos += 1;
                    match c {
                        '\'' => break,
                        '\\' => {
                            let n = self.peek(0);
                            self.pos += 1;
                            match n {
                                Some('n') => w.text.push('\n'),
                                Some('t') => w.text.push('\t'),
                                Some('r') => w.text.push('\r'),
                                Some('e') | Some('E') => w.text.push('\x1b'),
                                Some('a') => w.text.push('\x07'),
                                Some('x') => {
                                    let mut v = 0u32;
                                    let mut n = 0;
                                    while n < 2 {
                                        match self.peek(0).and_then(|c| c.to_digit(16)) {
                                            Some(d) => {
                                                v = v * 16 + d;
                                                self.pos += 1;
                                                n += 1;
                                            }
                                            None => break,
                                        }
                                    }
                                    if let Some(ch) = char::from_u32(v) {
                                        w.text.push(ch);
                                    }
                                }
                                Some(o) => w.text.push(o),
                                None => {}
                            }
                        }
                        c => w.text.push(c),
                    }
                }
            }
            Some(n) if n.is_ascii_alphabetic() || n == '_' => {
                w.dynamic = true;
                w.text.push('$');
                self.pos += 1;
                while let Some(c) = self.peek(0) {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        w.text.push(c);
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
            Some(n) if matches!(n, '@' | '*' | '#' | '?' | '$' | '!' | '-') || n.is_ascii_digit() => {
                w.dynamic = true;
                w.text.push('$');
                w.text.push(n);
                self.pos += 2;
            }
            _ => {
                w.text.push('$');
                self.pos += 1;
            }
        }
    }

    /// After `$(` / `<(` / `>(`: parse until the matching `)`.
    fn sub_script(&mut self, w: &mut Word, placeholder: &str) {
        w.dynamic = true;
        w.text.push_str(placeholder);
        if self.depth >= MAX_DEPTH {
            self.skip_balanced();
            return;
        }
        self.depth += 1;
        let saved_heredocs = std::mem::take(&mut self.heredocs);
        let inner = self.parse_list(true);
        self.heredocs = saved_heredocs;
        self.depth -= 1;
        w.subs.push(inner);
    }

    /// After the opening backtick.
    fn parse_backtick(&mut self, w: &mut Word) {
        let mut inner = String::new();
        while let Some(c) = self.peek(0) {
            self.pos += 1;
            match c {
                '`' => break,
                '\\' => match self.peek(0) {
                    Some(n) if matches!(n, '`' | '\\' | '$') => {
                        inner.push(n);
                        self.pos += 1;
                    }
                    _ => inner.push('\\'),
                },
                c => inner.push(c),
            }
        }
        w.dynamic = true;
        w.text.push_str("`…`");
        if self.depth < MAX_DEPTH {
            w.subs.push(parse_depth(&inner, self.depth + 1));
        }
    }
}

/// True when the unquoted skeleton defines and runs a self-piping background
/// function (`:(){ :|:& };:` and renamed variants).
pub fn has_fork_bomb(skeleton: &str) -> bool {
    let s: String = skeleton.chars().filter(|c| !c.is_whitespace()).collect();
    let mut from = 0;
    while let Some(i) = s[from..].find("(){") {
        let at = from + i;
        let name: String = s[..at]
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '.' | '-'))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if !name.is_empty() {
            let tail = &s[at + 3..];
            let pat = format!("{name}|{name}&");
            if tail.contains(&pat) && (tail.contains(&format!("}};{name}")) || tail.contains(&format!("}}&{name}")) || tail.contains(&format!("}}&&{name}"))) {
                return true;
            }
        }
        from = at + 3;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<Vec<String>> {
        parse(s).pipelines.iter().flat_map(|p| p.cmds.iter()).map(|c| c.words.iter().map(|w| w.text.clone()).collect()).collect()
    }

    #[test]
    fn quoting_and_escapes() {
        assert_eq!(words("r\\m -rf /"), vec![vec!["rm", "-rf", "/"]]);
        assert_eq!(words("'rm' \"-rf\" /"), vec![vec!["rm", "-rf", "/"]]);
        assert_eq!(words("\\rm -rf ~"), vec![vec!["rm", "-rf", "~"]]);
        assert_eq!(words("echo 'a b' \"c  d\" e\\ f"), vec![vec!["echo", "a b", "c  d", "e f"]]);
        assert_eq!(words("echo $'a\\tb'"), vec![vec!["echo", "a\tb"]]);
        assert_eq!(words("echo \"it's\" 'say \"hi\"'"), vec![vec!["echo", "it's", "say \"hi\""]]);
    }

    #[test]
    fn lists_and_pipes() {
        assert_eq!(words("cd /tmp && rm -rf *"), vec![vec!["cd", "/tmp"], vec!["rm", "-rf", "*"]]);
        assert_eq!(words("echo hi; rm -rf ~"), vec![vec!["echo", "hi"], vec!["rm", "-rf", "~"]]);
        assert_eq!(words("a || b | c & d"), vec![vec!["a"], vec!["b"], vec!["c"], vec!["d"]]);
        let s = parse("ls | xargs rm -rf");
        assert_eq!(s.pipelines.len(), 1);
        assert_eq!(s.pipelines[0].cmds.len(), 2);
        let s = parse("a\nb\\\nc d");
        assert_eq!(s.pipelines.len(), 2);
        assert_eq!(s.pipelines[1].cmds[0].words[0].text, "bc");
    }

    #[test]
    fn substitutions_are_parsed_recursively() {
        let s = parse("echo $(rm -rf / ) `ls | wc`");
        let w = &s.pipelines[0].cmds[0].words;
        assert_eq!(w[1].subs.len(), 1);
        assert_eq!(w[1].subs[0].pipelines[0].cmds[0].words[0].text, "rm");
        assert_eq!(w[2].subs[0].pipelines[0].cmds.len(), 2);
        // inside double quotes too
        let s = parse("echo \"x $(rm -rf /) y\"");
        assert_eq!(s.pipelines[0].cmds[0].words[1].subs.len(), 1);
        // nested
        let s = parse("echo $(echo $(echo deep))");
        assert_eq!(s.pipelines[0].cmds[0].words[1].subs[0].pipelines[0].cmds[0].words[1].subs.len(), 1);
        // quoted $( is literal
        let s = parse("echo '$(rm -rf /)'");
        assert!(s.pipelines[0].cmds[0].words[1].subs.is_empty());
    }

    #[test]
    fn redirections_and_assignments() {
        let s = parse(": > /etc/passwd");
        let c = &s.pipelines[0].cmds[0];
        assert_eq!(c.words.len(), 1);
        assert_eq!(c.redirs[0].op, ">");
        assert_eq!(c.redirs[0].target.text, "/etc/passwd");
        let s = parse("FOO=1 BAR='x y' make 2>&1 >>log");
        let c = &s.pipelines[0].cmds[0];
        assert_eq!(c.assigns.len(), 2);
        assert_eq!(c.assigns[1].1.text, "x y");
        assert_eq!(c.words[0].text, "make");
        assert_eq!(c.redirs.len(), 2);
        assert_eq!((c.redirs[0].fd, c.redirs[0].op.as_str(), c.redirs[0].target.text.as_str()), (Some(2), ">&", "1"));
        assert_eq!(c.redirs[1].op, ">>");
        // `echo x > /dev/disk0`: the target is not a word of echo
        let s = parse("echo x > /dev/disk0");
        assert_eq!(s.pipelines[0].cmds[0].words.len(), 2);
        // arg that merely looks like an assignment after the command word
        let s = parse("dd if=/dev/zero of=./a");
        assert_eq!(s.pipelines[0].cmds[0].words.len(), 3);
    }

    #[test]
    fn heredoc_and_herestring_bodies() {
        let s = parse("psql <<EOF\nDROP TABLE x;\nEOF\necho done");
        assert_eq!(s.pipelines.len(), 2);
        assert!(s.pipelines[0].stdin_texts[0].contains("DROP TABLE x"));
        assert_eq!(s.pipelines[1].cmds[0].words[0].text, "echo");
        let s = parse("psql <<< 'DROP TABLE y'");
        assert_eq!(s.pipelines[0].stdin_texts[0], "DROP TABLE y");
    }

    #[test]
    fn reserved_words_groups_and_functions() {
        assert_eq!(words("if true; then rm -rf x; fi"), vec![vec!["true"], vec!["rm", "-rf", "x"]]);
        assert_eq!(words("for i in 1 2; do echo $i; done")[1], vec!["echo", "$i"]);
        assert_eq!(words("(cd /; rm -rf *)"), vec![vec!["cd", "/"], vec!["rm", "-rf", "*"]]);
        assert_eq!(words("{ rm -rf a; }"), vec![vec!["rm", "-rf", "a"]]);
        assert_eq!(words("f() { rm -rf a; }; f"), vec![vec!["rm", "-rf", "a"], vec!["f"]]);
        assert_eq!(words("case $x in a) rm -rf b ;; esac")[1], vec!["rm", "-rf", "b"]);
    }

    #[test]
    fn comments_and_unterminated_input() {
        assert_eq!(words("echo hi # rm -rf /"), vec![vec!["echo", "hi"]]);
        assert_eq!(words("echo a#b"), vec![vec!["echo", "a#b"]]);
        // never panics / hangs on junk
        for junk in ["'", "\"", "$(", "`", "((", "))", "<<", "$((", "${", "\\", "a |", "&&", "<(", "$'\\x", "<<E\nx"] {
            let _ = parse(junk);
        }
        let deep = "$(".repeat(200) + &")".repeat(200);
        let _ = parse(&format!("echo {deep}"));
    }

    #[test]
    fn fork_bomb_structure() {
        assert!(has_fork_bomb(&parse(":(){ :|:& };:").skeleton));
        assert!(has_fork_bomb(&parse("bomb() { bomb | bomb & }; bomb").skeleton));
        assert!(!has_fork_bomb(&parse("echo ':(){ :|:& };:'").skeleton));
        assert!(!has_fork_bomb(&parse("grep ':(){' file").skeleton));
        assert!(!has_fork_bomb(&parse(":(){ :|:& }").skeleton), "defined but never run");
    }
}
