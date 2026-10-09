//! Recognise which coding agent (if any) runs in a pane.
//!
//! Three independent signals, strongest first:
//! 1. the command text the shell reported (OSC 633;E / OSC 133 block),
//! 2. the PTY's foreground process (`tcgetpgrp` -> pid -> path / argv),
//! 3. the window title (OSC 0 / 2).
//!
//! Everything here is pure except [`proc_info`], which asks the OS about a pid.

use super::{AgentKind, DetectSource};

/// A recognised agent and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Detection {
    pub kind: AgentKind,
    pub source: DetectSource,
}

// ───────────────────────────── command text ─────────────────────────────

/// Executable basename -> agent.
const BINARIES: &[(&str, AgentKind)] = &[
    ("claude", AgentKind::ClaudeCode),
    ("claude-code", AgentKind::ClaudeCode),
    ("codex", AgentKind::Codex),
    ("gemini", AgentKind::Gemini),
    ("opencode", AgentKind::OpenCode),
    ("aider", AgentKind::Aider),
    ("cursor-agent", AgentKind::CursorAgent),
];

/// npm / pip package name -> agent (what `npx`, `pnpm dlx`, `uvx`, `pipx run` take).
const PACKAGES: &[(&str, AgentKind)] = &[
    ("@anthropic-ai/claude-code", AgentKind::ClaudeCode),
    ("@openai/codex", AgentKind::Codex),
    ("@google/gemini-cli", AgentKind::Gemini),
    ("opencode-ai", AgentKind::OpenCode),
    ("opencode", AgentKind::OpenCode),
    ("aider-chat", AgentKind::Aider),
    ("aider-install", AgentKind::Aider),
    ("aider", AgentKind::Aider),
];

/// Commands that only wrap another command.
const WRAPPERS: &[&str] = &["sudo", "env", "command", "exec", "nohup", "time", "nice", "caffeinate", "stdbuf", "builtin", "noglob", "doas"];

/// Package runners: the first non-flag argument is the package / script.
/// Multi-word runners are listed as (first word, second word).
const RUNNERS: &[&str] = &["npx", "bunx", "pnpx", "uvx"];
const RUNNERS2: &[(&str, &str)] = &[
    ("pnpm", "dlx"), ("pnpm", "exec"), ("yarn", "dlx"), ("npm", "exec"), ("bun", "x"), ("pipx", "run"), ("uv", "tool"),
];
/// Interpreters whose script argument names the program.
const INTERPRETERS: &[&str] = &["node", "bun", "deno", "python", "python3", "python3.11", "python3.12", "python3.13"];

/// Subcommands / flags that never start an interactive session.
fn is_non_interactive(kind: AgentKind, rest: &[String]) -> bool {
    let first = rest.iter().find(|a| !a.starts_with('-')).map(String::as_str);
    if rest.iter().any(|a| matches!(a.as_str(), "--version" | "-v" | "-V" | "--help" | "-h" | "version" | "help")) {
        return true;
    }
    let subs: &[&str] = match kind {
        AgentKind::ClaudeCode => &["mcp", "config", "doctor", "update", "install", "migrate-installer", "setup-token", "plugin", "plugins", "api-key"],
        AgentKind::Codex => &["login", "logout", "mcp", "completion", "proto", "app"],
        AgentKind::Gemini => &["mcp", "extensions"],
        AgentKind::OpenCode => &["auth", "models", "upgrade", "uninstall", "mcp", "agent"],
        AgentKind::Aider => &["--list-models", "--models"],
        AgentKind::CursorAgent => &["login", "logout", "status", "update", "mcp", "about"],
    };
    first.is_some_and(|f| subs.contains(&f)) || rest.iter().any(|a| kind == AgentKind::Aider && subs.contains(&a.as_str()))
}

/// Split a command line into segments (on `&&`, `||`, `;`, `|`, `&`) of words,
/// honouring single / double quotes and backslash escapes.
pub fn split_segments(cmd: &str) -> Vec<Vec<String>> {
    let mut segs: Vec<Vec<String>> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut it = cmd.chars().peekable();
    let flush = |cur: &mut String, in_word: &mut bool, words: &mut Vec<String>| {
        if *in_word {
            words.push(std::mem::take(cur));
            *in_word = false;
        }
    };
    while let Some(c) = it.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    if let Some(n) = it.next() {
                        cur.push(n);
                    }
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    in_word = true;
                }
                '\\' => {
                    if let Some(n) = it.next() {
                        cur.push(n);
                        in_word = true;
                    }
                }
                ';' | '|' | '&' | '\n' => {
                    flush(&mut cur, &mut in_word, &mut words);
                    // swallow the second char of && / ||
                    if matches!(c, '|' | '&') && it.peek() == Some(&c) {
                        it.next();
                    }
                    if !words.is_empty() {
                        segs.push(std::mem::take(&mut words));
                    }
                }
                c if c.is_whitespace() => flush(&mut cur, &mut in_word, &mut words),
                c => {
                    cur.push(c);
                    in_word = true;
                }
            },
        }
    }
    flush(&mut cur, &mut in_word, &mut words);
    if !words.is_empty() {
        segs.push(words);
    }
    segs
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn is_env_assignment(w: &str) -> bool {
    match w.split_once('=') {
        Some((k, _)) => !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !k.starts_with(|c: char| c.is_ascii_digit()),
        None => false,
    }
}

/// Strip an `@version` / `==version` suffix from a package spec.
fn package_name(spec: &str) -> &str {
    let (scoped, body) = match spec.strip_prefix('@') {
        Some(b) => (true, b),
        None => (false, spec),
    };
    let cut = body.find(['@', '=', '<', '>', '~']).unwrap_or(body.len());
    let end = cut + usize::from(scoped);
    &spec[..end]
}

fn kind_of_package(spec: &str) -> Option<AgentKind> {
    let name = package_name(spec).to_ascii_lowercase();
    PACKAGES.iter().find(|(p, _)| *p == name).map(|(_, k)| *k)
}

fn kind_of_binary(name: &str) -> Option<AgentKind> {
    let n = name.trim_end_matches(".exe").to_ascii_lowercase();
    BINARIES.iter().find(|(b, _)| *b == n).map(|(_, k)| *k)
}

/// Script path / argument that names an agent (`node /usr/lib/node_modules/@google/gemini-cli/dist/index.js`).
fn kind_of_script(arg: &str) -> Option<AgentKind> {
    let a = arg.to_ascii_lowercase();
    if a.contains("@anthropic-ai/claude-code") || a.contains("/claude-code/") {
        return Some(AgentKind::ClaudeCode);
    }
    if a.contains("@openai/codex") || a.contains("/codex-cli/") {
        return Some(AgentKind::Codex);
    }
    if a.contains("@google/gemini-cli") || a.contains("/gemini-cli/") {
        return Some(AgentKind::Gemini);
    }
    if a.contains("opencode-ai") || a.contains("/opencode/") {
        return Some(AgentKind::OpenCode);
    }
    if a.contains("cursor-agent") {
        return Some(AgentKind::CursorAgent);
    }
    let base = basename(&a).trim_end_matches(".js").trim_end_matches(".mjs").trim_end_matches(".py").to_string();
    kind_of_binary(&base).or_else(|| (base == "aider").then_some(AgentKind::Aider))
}

fn detect_segment(words: &[String]) -> Option<AgentKind> {
    let mut i = 0;
    // leading VAR=value assignments
    while i < words.len() && is_env_assignment(&words[i]) {
        i += 1;
    }
    // wrappers and their flags / assignments
    while i < words.len() {
        let w = words[i].as_str();
        if WRAPPERS.contains(&basename(w)) {
            i += 1;
            while i < words.len() && (words[i].starts_with('-') || is_env_assignment(&words[i])) {
                i += 1;
            }
        } else {
            break;
        }
    }
    let head = words.get(i)?.as_str();
    let head_base = basename(head);
    let rest = &words[i + 1..];

    // Package runners: npx -y @anthropic-ai/claude-code --flag
    let runner = RUNNERS.contains(&head_base)
        || RUNNERS2.iter().any(|(a, b)| *a == head_base && rest.first().map(String::as_str) == Some(*b));
    if runner {
        let skip = usize::from(!RUNNERS.contains(&head_base));
        let args = &rest[skip.min(rest.len())..];
        let mut j = 0;
        while j < args.len() {
            let a = args[j].as_str();
            if a == "--from" || a == "--package" || a == "-p" {
                // `uvx --from aider-chat aider`
                if let Some(k) = args.get(j + 1).and_then(|p| kind_of_package(p)) {
                    return (!is_non_interactive(k, &args[j + 2..])).then_some(k);
                }
                j += 2;
                continue;
            }
            if a.starts_with('-') {
                j += 1;
                continue;
            }
            let k = kind_of_package(a).or_else(|| kind_of_binary(a))?;
            return (!is_non_interactive(k, &args[j + 1..])).then_some(k);
        }
        return None;
    }

    // Interpreters: python -m aider, node /path/to/gemini
    if INTERPRETERS.contains(&head_base) || head_base.starts_with("python") {
        let mut j = 0;
        while j < rest.len() {
            let a = rest[j].as_str();
            if a == "-m" {
                let m = rest.get(j + 1)?.as_str();
                let k = (m == "aider").then_some(AgentKind::Aider)?;
                return (!is_non_interactive(k, &rest[j + 2..])).then_some(k);
            }
            if a.starts_with('-') {
                j += 1;
                continue;
            }
            let k = kind_of_script(a)?;
            return (!is_non_interactive(k, &rest[j + 1..])).then_some(k);
        }
        return None;
    }

    let k = kind_of_binary(head_base)?;
    (!is_non_interactive(k, rest)).then_some(k)
}

/// Agent started by this command line, if any (`cd x && claude --resume`,
/// `ANTHROPIC_API_KEY=... claude`, `npx @anthropic-ai/claude-code`, ...).
pub fn detect_from_command(cmd: &str) -> Option<AgentKind> {
    split_segments(cmd).iter().find_map(|s| detect_segment(s))
}

// ───────────────────────────── process ─────────────────────────────

/// What the OS reports about a process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: u32,
    /// Executable path (or `comm` when the path is unavailable).
    pub path: String,
    /// argv (argv[0] first), best effort.
    pub args: Vec<String>,
}

impl ProcInfo {
    pub fn name(&self) -> &str {
        basename(&self.path)
    }
}

/// Agent behind a foreground process.
pub fn detect_from_process(p: &ProcInfo) -> Option<AgentKind> {
    let name = p.name().to_ascii_lowercase();
    let argv0 = p.args.first().map(|a| basename(a).to_ascii_lowercase()).unwrap_or_default();
    // Direct binaries (`claude`, `codex`, `aider` ...), by exe name or argv[0].
    if let Some(k) = kind_of_binary(&name).or_else(|| kind_of_binary(&argv0)) {
        return Some(k);
    }
    // Native installers put the real binary in a versioned dir:
    //   ~/.local/share/claude/versions/2.0.5
    let lower = p.path.to_ascii_lowercase();
    let comps: Vec<&str> = lower.split('/').collect();
    if comps.windows(2).any(|w| (w[0] == "claude" && w[1] == "versions") || w[0] == "claude-code") {
        return Some(AgentKind::ClaudeCode);
    }
    // Runtimes: node / bun / python running a known script.
    if INTERPRETERS.contains(&name.as_str()) || name.starts_with("python") || name == "tsx" {
        let mut skip_next = false;
        for a in p.args.iter().skip(1) {
            if skip_next {
                skip_next = false;
                continue;
            }
            if a == "-m" {
                if let Some(pos) = p.args.iter().position(|x| x == a) {
                    if p.args.get(pos + 1).map(String::as_str) == Some("aider") {
                        return Some(AgentKind::Aider);
                    }
                }
                skip_next = true;
                continue;
            }
            if a.starts_with('-') {
                continue;
            }
            return kind_of_script(a);
        }
    }
    None
}

/// Look a process up. `None` when it is gone or the OS refuses.
pub fn proc_info(pid: u32) -> Option<ProcInfo> {
    #[cfg(target_os = "macos")]
    {
        mac::proc_info(pid)
    }
    #[cfg(target_os = "linux")]
    {
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        let path = std::fs::read_link(format!("/proc/{pid}/exe"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| comm.trim().to_string());
        let args = std::fs::read(format!("/proc/{pid}/cmdline"))
            .map(|b| split_nul(&b))
            .unwrap_or_default();
        Some(ProcInfo { pid, path, args })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
fn split_nul(b: &[u8]) -> Vec<String> {
    b.split(|c| *c == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect()
}

/// Parse a `KERN_PROCARGS2` buffer: `argc: i32`, exec path, NUL padding, argv...
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub fn parse_procargs2(buf: &[u8]) -> Option<(String, Vec<String>)> {
    if buf.len() < 5 {
        return None;
    }
    let argc = i32::from_ne_bytes(buf[0..4].try_into().ok()?).max(0) as usize;
    let rest = &buf[4..];
    let end = rest.iter().position(|b| *b == 0)?;
    let exec = String::from_utf8_lossy(&rest[..end]).into_owned();
    let mut p = end;
    while p < rest.len() && rest[p] == 0 {
        p += 1;
    }
    let mut args = Vec::new();
    while args.len() < argc && p < rest.len() {
        let e = rest[p..].iter().position(|b| *b == 0).map_or(rest.len(), |x| p + x);
        args.push(String::from_utf8_lossy(&rest[p..e]).into_owned());
        p = e + 1;
    }
    Some((exec, args))
}

#[cfg(target_os = "macos")]
mod mac {
    use super::{parse_procargs2, ProcInfo};

    pub fn proc_info(pid: u32) -> Option<ProcInfo> {
        let mut buf = vec![0u8; 4096];
        // SAFETY: `buf` is a valid writable buffer of the stated length.
        let n = unsafe { libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32) };
        let path = if n > 0 { String::from_utf8_lossy(&buf[..n as usize]).into_owned() } else { String::new() };
        let args = procargs(pid).map(|(_, a)| a).unwrap_or_default();
        if path.is_empty() && args.is_empty() {
            return None;
        }
        Some(ProcInfo { pid, path, args })
    }

    fn procargs(pid: u32) -> Option<(String, Vec<String>)> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
        let mut size: libc::size_t = 0;
        // SAFETY: standard two-call sysctl; the first call only asks for the size.
        let rc = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) };
        if rc != 0 || size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size.min(1 << 20)];
        let mut len: libc::size_t = buf.len();
        // SAFETY: `buf` holds `len` writable bytes.
        let rc = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr() as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) };
        if rc != 0 {
            return None;
        }
        buf.truncate(len);
        parse_procargs2(&buf)
    }
}

// ───────────────────────────── title ─────────────────────────────

fn is_spinner(c: char) -> bool {
    ('\u{2800}'..='\u{28ff}').contains(&c)
}

/// Agent hinted by a window title. Weak evidence: only used while a command is
/// running (or the shell has no integration).
pub fn detect_from_title(title: &str) -> Option<AgentKind> {
    let t = title.trim();
    if t.is_empty() {
        return None;
    }
    let l = t.to_ascii_lowercase();
    let first = t.chars().next()?;
    // Claude Code titles its terminal "✳ Claude Code" and later "<spinner|✳> <task>".
    if l.contains("claude code") || l == "claude" || l.starts_with("claude ") {
        return Some(AgentKind::ClaudeCode);
    }
    if (is_spinner(first) || first == '\u{2733}') && t.chars().nth(1) == Some(' ') {
        return Some(AgentKind::ClaudeCode);
    }
    if l.contains("gemini cli") || l.starts_with("gemini ") || l == "gemini" || l.starts_with("\u{2726} gemini") {
        return Some(AgentKind::Gemini);
    }
    if l == "codex" || l.starts_with("codex ") || l.contains("openai codex") {
        return Some(AgentKind::Codex);
    }
    if l == "opencode" || l.starts_with("opencode ") || l.starts_with("oc | ") {
        return Some(AgentKind::OpenCode);
    }
    if l == "aider" || l.starts_with("aider ") || l.starts_with("aider:") {
        return Some(AgentKind::Aider);
    }
    if l.contains("cursor agent") || l.contains("cursor-agent") {
        return Some(AgentKind::CursorAgent);
    }
    None
}

/// Claude Code animates a Braille spinner in the title while it works and shows
/// `✳` when idle. `Some(true)` = working, `Some(false)` = idle, `None` = unknown.
pub fn title_working_hint(title: &str) -> Option<bool> {
    let first = title.trim_start().chars().next()?;
    if is_spinner(first) {
        Some(true)
    } else if first == '\u{2733}' {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AgentKind::*;

    #[test]
    fn plain_commands() {
        for (cmd, k) in [
            ("claude", ClaudeCode),
            ("claude --resume", ClaudeCode),
            ("claude -p \"fix the bug\"", ClaudeCode),
            ("codex", Codex),
            ("codex exec \"add tests\"", Codex),
            ("gemini", Gemini),
            ("opencode", OpenCode),
            ("aider --model sonnet", Aider),
            ("cursor-agent", CursorAgent),
            ("/opt/homebrew/bin/claude", ClaudeCode),
            ("~/.local/bin/claude --dangerously-skip-permissions", ClaudeCode),
        ] {
            assert_eq!(detect_from_command(cmd), Some(k), "{cmd}");
        }
    }

    #[test]
    fn wrappers_env_and_chains() {
        for (cmd, k) in [
            ("ANTHROPIC_API_KEY=sk-x claude", ClaudeCode),
            ("FOO=1 BAR=2 codex", Codex),
            ("env FOO=1 gemini", Gemini),
            ("sudo -E aider", Aider),
            ("time claude", ClaudeCode),
            ("cd ~/proj && claude", ClaudeCode),
            ("git pull; codex", Codex),
            ("exec claude", ClaudeCode),
            ("nohup opencode", OpenCode),
        ] {
            assert_eq!(detect_from_command(cmd), Some(k), "{cmd}");
        }
    }

    #[test]
    fn package_runners() {
        assert_eq!(detect_from_command("npx @anthropic-ai/claude-code"), Some(ClaudeCode));
        assert_eq!(detect_from_command("npx -y @anthropic-ai/claude-code@latest --resume"), Some(ClaudeCode));
        assert_eq!(detect_from_command("npx @openai/codex"), Some(Codex));
        assert_eq!(detect_from_command("npx @google/gemini-cli"), Some(Gemini));
        assert_eq!(detect_from_command("bunx opencode-ai"), Some(OpenCode));
        assert_eq!(detect_from_command("pnpm dlx @anthropic-ai/claude-code"), Some(ClaudeCode));
        assert_eq!(detect_from_command("uvx --from aider-chat aider"), Some(Aider));
        assert_eq!(detect_from_command("pipx run aider-chat"), Some(Aider));
        assert_eq!(detect_from_command("python -m aider"), Some(Aider));
        assert_eq!(detect_from_command("python3 -m aider --model gpt-4o"), Some(Aider));
        assert_eq!(detect_from_command("node /usr/local/lib/node_modules/@google/gemini-cli/dist/index.js"), Some(Gemini));
        assert_eq!(detect_from_command("npx cowsay hi"), None);
    }

    #[test]
    fn not_agents() {
        for cmd in [
            "",
            "ls -la",
            "echo claude",
            "cat claude.md",
            "git commit -m \"ask claude\"",
            "which claude",
            "vim codex.toml",
            "grep gemini notes.txt",
            "claude --version",
            "claude -v",
            "codex --help",
            "claude mcp add rift -- rift mcp",
            "claude doctor",
            "codex login",
            "python script.py",
            "node server.js",
            "aider --version",
        ] {
            assert_eq!(detect_from_command(cmd), None, "{cmd:?}");
        }
    }

    #[test]
    fn quotes_do_not_hide_or_invent_commands() {
        assert_eq!(detect_from_command("'claude'"), Some(ClaudeCode));
        assert_eq!(detect_from_command("echo \"a && claude\""), None);
        assert_eq!(split_segments("a && b | c; d").len(), 4);
    }

    fn proc(path: &str, args: &[&str]) -> ProcInfo {
        ProcInfo { pid: 1, path: path.into(), args: args.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn process_names() {
        assert_eq!(detect_from_process(&proc("/usr/local/bin/claude", &["claude"])), Some(ClaudeCode));
        assert_eq!(detect_from_process(&proc("/opt/homebrew/bin/codex", &["codex"])), Some(Codex));
        assert_eq!(detect_from_process(&proc("/home/u/.local/share/claude/versions/2.0.5", &["claude"])), Some(ClaudeCode));
        assert_eq!(detect_from_process(&proc("/Users/u/.local/share/claude/versions/2.0.5", &["2.0.5"])), Some(ClaudeCode));
        assert_eq!(detect_from_process(&proc("/usr/bin/zsh", &["-zsh"])), None);
        assert_eq!(detect_from_process(&proc("/usr/bin/vim", &["vim", "claude.md"])), None);
    }

    #[test]
    fn process_behind_a_runtime() {
        assert_eq!(
            detect_from_process(&proc("/usr/local/bin/node", &["node", "/usr/local/bin/gemini"])),
            Some(Gemini)
        );
        assert_eq!(
            detect_from_process(&proc("/usr/local/bin/node", &["node", "--no-warnings", "/opt/lib/node_modules/@anthropic-ai/claude-code/cli.js"])),
            Some(ClaudeCode)
        );
        assert_eq!(
            detect_from_process(&proc("/usr/bin/python3", &["python3", "-m", "aider", "--yes"])),
            Some(Aider)
        );
        assert_eq!(detect_from_process(&proc("/usr/local/bin/node", &["node", "server.js"])), None);
        assert_eq!(detect_from_process(&proc("/usr/local/bin/node", &["node"])), None);
    }

    #[test]
    fn titles() {
        assert_eq!(detect_from_title("\u{2733} Claude Code"), Some(ClaudeCode));
        assert_eq!(detect_from_title("\u{2802} Fix flaky test"), Some(ClaudeCode));
        assert_eq!(detect_from_title("\u{2733} Refactor parser"), Some(ClaudeCode));
        assert_eq!(detect_from_title("Gemini CLI (~/proj)"), Some(Gemini));
        assert_eq!(detect_from_title("codex"), Some(Codex));
        assert_eq!(detect_from_title("OC | Fix tests"), Some(OpenCode));
        assert_eq!(detect_from_title("aider"), Some(Aider));
        assert_eq!(detect_from_title("vim claude.md"), None);
        assert_eq!(detect_from_title("zsh"), None);
        assert_eq!(detect_from_title(""), None);
    }

    #[test]
    fn title_hints() {
        assert_eq!(title_working_hint("\u{2802} thinking"), Some(true));
        assert_eq!(title_working_hint("\u{2733} Claude Code"), Some(false));
        assert_eq!(title_working_hint("zsh"), None);
    }

    #[test]
    fn procargs2_parsing() {
        let mut b = Vec::new();
        b.extend_from_slice(&3i32.to_ne_bytes());
        b.extend_from_slice(b"/usr/local/bin/node\0\0\0");
        b.extend_from_slice(b"node\0--flag\0/usr/local/bin/gemini\0PATH=/bin\0");
        let (exec, args) = parse_procargs2(&b).unwrap();
        assert_eq!(exec, "/usr/local/bin/node");
        assert_eq!(args, vec!["node", "--flag", "/usr/local/bin/gemini"]);
        assert!(parse_procargs2(&[1, 2]).is_none());
    }

    #[test]
    fn package_name_strips_versions() {
        assert_eq!(package_name("@anthropic-ai/claude-code@1.2.3"), "@anthropic-ai/claude-code");
        assert_eq!(package_name("aider-chat==0.50"), "aider-chat");
        assert_eq!(package_name("opencode-ai@latest"), "opencode-ai");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn proc_info_sees_this_process() {
        let me = proc_info(std::process::id()).expect("own process is visible");
        assert!(!me.path.is_empty());
        assert!(!me.name().is_empty());
        assert!(proc_info(u32::MAX - 7).is_none());
    }

    #[test]
    fn nul_split() {
        assert_eq!(split_nul(b"a\0b\0\0c\0"), vec!["a", "b", "c"]);
    }
}
