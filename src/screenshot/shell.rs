//! Scripted shell sessions: builds the exact byte stream an interactive zsh
//! with Rift's shell integration would emit (OSC 133 A/B/C/D, OSC 7, a
//! powerline prompt with Nerd Font glyphs, SGR colours), to be fed through
//! the real VT parser with `Pane::feed`.

/// Expand `{name}` colour tokens into SGR escapes. Unknown `{...}` text
/// (Rust braces, JSON, ...) passes through untouched.
pub fn paint(s: &str) -> String {
    const TOKENS: &[(&str, &str)] = &[
        ("{0}", "\x1b[0m"),
        ("{bold}", "\x1b[1m"),
        ("{dim}", "\x1b[2m"),
        ("{ital}", "\x1b[3m"),
        ("{ul}", "\x1b[4m"),
        ("{black}", "\x1b[30m"),
        ("{red}", "\x1b[31m"),
        ("{green}", "\x1b[32m"),
        ("{yellow}", "\x1b[33m"),
        ("{blue}", "\x1b[34m"),
        ("{magenta}", "\x1b[35m"),
        ("{cyan}", "\x1b[36m"),
        ("{white}", "\x1b[37m"),
        ("{gray}", "\x1b[2m"),
        ("{bred}", "\x1b[91m"),
        ("{bgreen}", "\x1b[92m"),
        ("{byellow}", "\x1b[93m"),
        ("{bblue}", "\x1b[94m"),
        ("{bmagenta}", "\x1b[95m"),
        ("{bcyan}", "\x1b[96m"),
        ("{bwhite}", "\x1b[97m"),
        ("{rev}", "\x1b[7m"),
        ("{bgcyan}", "\x1b[46m"),
        ("{bggreen}", "\x1b[42m"),
        ("{bgred}", "\x1b[41m"),
    ];
    let mut out = String::with_capacity(s.len() + 32);
    let mut rest = s;
    'outer: while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        for (tok, esc) in TOKENS {
            if let Some(r) = rest.strip_prefix(tok) {
                out.push_str(esc);
                rest = r;
                continue 'outer;
            }
        }
        out.push('{');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

/// What the (starship-like) prompt shows.
#[derive(Clone)]
pub struct Prompt {
    /// Directory label shown in the first segment.
    pub dir: String,
    /// Absolute cwd reported through OSC 7.
    pub cwd: String,
    pub branch: Option<String>,
    /// Git status summary, e.g. `!2 ?1`.
    pub git_state: String,
    pub host: String,
    /// Exit status of the previous command (non-zero adds a red segment and
    /// turns the prompt character red).
    pub last_exit: i32,
}

impl Prompt {
    pub fn new(dir: &str, cwd: &str) -> Self {
        Self {
            dir: dir.into(),
            cwd: cwd.into(),
            branch: None,
            git_state: String::new(),
            host: "mbp".into(),
            last_exit: 0,
        }
    }

    pub fn git(mut self, branch: &str, state: &str) -> Self {
        self.branch = Some(branch.into());
        self.git_state = state.into();
        self
    }

    pub fn exit(mut self, code: i32) -> Self {
        self.last_exit = code;
        self
    }
}

const SEP: char = '\u{e0b0}';
const BRANCH: char = '\u{e0a0}';
const FOLDER: char = '\u{f07c}';

/// Byte-stream builder for one pane.
#[derive(Default)]
pub struct Script {
    pub bytes: Vec<u8>,
}

impl Script {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn raw(&mut self, s: &str) -> &mut Self {
        self.bytes.extend_from_slice(s.as_bytes());
        self
    }

    /// Output lines (colour tokens expanded), each terminated with CRLF.
    pub fn out(&mut self, text: &str) -> &mut Self {
        for l in text.split('\n') {
            let p = paint(l);
            self.raw(&p).raw("\r\n");
        }
        self
    }

    /// Start of a prompt: OSC 133;A, OSC 7, powerline prompt, OSC 133;B.
    pub fn prompt(&mut self, p: &Prompt) -> &mut Self {
        self.raw("\x1b]133;A\x07");
        self.raw(&format!("\x1b]7;file://{}{}\x07", p.host, p.cwd));
        let mut s = String::new();
        if p.last_exit != 0 {
            s.push_str(&format!("\x1b[41m\x1b[30m \u{2718} {} \x1b[31m\x1b[44m{SEP}", p.last_exit));
        }
        // Directory segment (blue), git segment (cyan), then the arrow.
        s.push_str(&format!("\x1b[44m\x1b[30m {FOLDER} {} ", p.dir));
        match &p.branch {
            Some(b) => {
                s.push_str(&format!("\x1b[34m\x1b[46m{SEP}\x1b[30m {BRANCH} {b}"));
                if !p.git_state.is_empty() {
                    s.push_str(&format!(" {}", p.git_state));
                }
                s.push_str(&format!(" \x1b[0m\x1b[36m{SEP}\x1b[0m"));
            }
            None => s.push_str(&format!("\x1b[0m\x1b[34m{SEP}\x1b[0m")),
        }
        let arrow = if p.last_exit != 0 { "\x1b[91m" } else { "\x1b[35m" };
        s.push_str(&format!(" {arrow}\u{276f}\x1b[0m "));
        self.raw(&s);
        self.raw("\x1b]133;B\x07");
        self
    }

    /// Text typed at the prompt (no Enter), with light zsh-syntax-highlight
    /// colouring: the command word green, flags/args default.
    pub fn typed(&mut self, cmd: &str) -> &mut Self {
        let mut it = cmd.splitn(2, ' ');
        let first = it.next().unwrap_or("");
        self.raw(&format!("\x1b[32m{first}\x1b[0m"));
        if let Some(rest) = it.next() {
            self.raw(&format!(" {rest}"));
        }
        self
    }

    /// Enter pressed: newline, then OSC 133;C (output starts).
    pub fn enter(&mut self) -> &mut Self {
        self.raw("\r\n\x1b]133;C\x07")
    }

    /// Command finished with `exit`: OSC 133;D.
    pub fn done(&mut self, exit: i32) -> &mut Self {
        self.raw(&format!("\x1b]133;D;{exit}\x07"))
    }

    /// A complete command: prompt, typed text, Enter, output, finish.
    pub fn run(&mut self, p: &Prompt, cmd: &str, output: &str, exit: i32) -> &mut Self {
        self.prompt(p).typed(cmd).enter();
        if !output.is_empty() {
            self.out(output);
        }
        self.done(exit)
    }
}
