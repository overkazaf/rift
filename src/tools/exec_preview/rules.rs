//! Safety rules over parsed shell commands. Every simple command in the line
//! (pipeline stages, `&&`/`;` lists, `$(…)`/backtick bodies, `sh -c` strings,
//! `find -exec` payloads) is resolved through its wrappers (`sudo`, `env`,
//! `command`, `xargs`, absolute paths, quoting tricks) and then matched
//! against the rule set below.

use std::path::PathBuf;

use super::analysis::{self, format_size, Budget, Scan, BUDGET_MS};
use super::shell_parse::{self, Pipeline, Script, Simple, Word};
use super::{Impact, Severity};

pub struct Finding {
    pub severity: Severity,
    pub impacts: Vec<Impact>,
}

pub struct Env {
    pub cwd: Option<PathBuf>,
    /// Normalized absolute home directory.
    pub home: Option<String>,
    pub budget: Budget,
}

pub fn evaluate(script: &Script, env: &Env) -> Vec<Finding> {
    let mut e = Eng { env, out: Vec::new(), steps: 0 };
    e.script(script, 0);
    e.out
}

fn finding(severity: Severity, impacts: Vec<Impact>) -> Finding {
    Finding { severity, impacts }
}

fn one(severity: Severity, description: impl Into<String>, detail: impl Into<String>) -> Finding {
    finding(severity, vec![Impact::new(description, detail)])
}

// ── Command resolution (wrappers) ──

#[derive(Debug, Clone)]
struct Cmd {
    name: String,
    args: Vec<Word>,
    sudo: bool,
    /// Invoked through `xargs`: its path arguments come from stdin.
    xargs: bool,
    /// A shell run with `-c <script>` (does not read a script from stdin).
    shell_c: bool,
}

struct Resolved {
    cmd: Option<Cmd>,
    /// Scripts that will be evaluated by the command (`sh -c '…'`, `eval …`).
    inner: Vec<Script>,
}

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "ash", "fish", "csh", "tcsh"];
const INTERPRETERS: &[&str] = &["python", "python2", "python3", "perl", "ruby", "node", "php", "deno", "bun", "lua", "osascript"];

fn base_name(n: &str) -> &str {
    if n.starts_with('/') {
        n.rsplit('/').next().unwrap_or(n)
    } else {
        n
    }
}

/// The literal command name of a word; resolves `$(echo rm)` style indirection.
fn word_name(w: &Word) -> String {
    if (w.text == "$(…)" || w.text == "`…`") && w.subs.len() == 1 {
        let s = &w.subs[0];
        if s.pipelines.len() == 1 && s.pipelines[0].cmds.len() == 1 {
            let c = &s.pipelines[0].cmds[0];
            if let Some(first) = c.words.first() {
                if matches!(first.text.as_str(), "echo" | "printf") {
                    let parts: Vec<&str> = c
                        .words
                        .iter()
                        .skip(1)
                        .map(|w| w.text.as_str())
                        .filter(|t| !(t.starts_with('-') && t.len() <= 3) && !t.starts_with('%'))
                        .collect();
                    return parts.join(" ");
                }
            }
        }
    }
    w.text.clone()
}

fn is_assignment(t: &str) -> bool {
    match t.find('=') {
        Some(i) if i > 0 => t[..i].chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !t.starts_with(|c: char| c.is_ascii_digit()),
        _ => false,
    }
}

/// Skip option words of a wrapper. `with_arg` lists short options that take
/// a value (attached or as the next word).
fn skip_opts(words: &[Word], mut i: usize, with_arg: &str, long_with_arg: &[&str]) -> usize {
    while i < words.len() {
        let t = words[i].text.as_str();
        if t == "--" {
            return i + 1;
        }
        if let Some(l) = t.strip_prefix("--") {
            i += 1;
            if !l.contains('=') && long_with_arg.contains(&l) {
                i += 1;
            }
            continue;
        }
        if t.len() > 1 && t.starts_with('-') {
            i += 1;
            let cl: Vec<char> = t[1..].chars().collect();
            for (k, ch) in cl.iter().enumerate() {
                if with_arg.contains(*ch) {
                    if k + 1 == cl.len() {
                        i += 1; // value is the next word
                    }
                    break;
                }
            }
            continue;
        }
        break;
    }
    i
}

fn resolve(simple: &Simple) -> Resolved {
    let words = &simple.words;
    let mut sudo = false;
    let mut xargs = false;
    let mut i = 0;
    let mut inner = Vec::new();
    for _guard in 0..16 {
        if i >= words.len() {
            return Resolved { cmd: None, inner };
        }
        let raw = word_name(&words[i]);
        let base = base_name(&raw).to_string();
        match base.as_str() {
            "sudo" => {
                sudo = true;
                i = skip_opts(words, i + 1, "ughCDhprtTUR", &["user", "group", "host", "prompt", "role", "type", "close-from", "chdir", "other-user", "command-timeout", "chroot"]);
            }
            "doas" => {
                sudo = true;
                i = skip_opts(words, i + 1, "uC", &[]);
            }
            "pkexec" => {
                sudo = true;
                i = skip_opts(words, i + 1, "", &["user"]);
            }
            "env" => {
                i = skip_opts(words, i + 1, "uSCP", &["unset", "split-string", "chdir"]);
                while i < words.len() && is_assignment(&words[i].text) {
                    i += 1;
                }
            }
            "command" => {
                let j = skip_opts(words, i + 1, "", &[]);
                if words[i + 1..j].iter().any(|w| w.text.starts_with('-') && (w.text.contains('v') || w.text.contains('V'))) {
                    return Resolved { cmd: None, inner };
                }
                i = j;
            }
            "builtin" | "nohup" | "setsid" | "time" | "noglob" | "nocorrect" | "unbuffer" | "caffeinate" => {
                i = skip_opts(words, i + 1, if base == "caffeinate" { "tw" } else { "" }, &[]);
            }
            "exec" => i = skip_opts(words, i + 1, "a", &[]),
            "nice" => i = skip_opts(words, i + 1, "n", &["adjustment"]),
            "ionice" => i = skip_opts(words, i + 1, "cnp", &[]),
            "stdbuf" => i = skip_opts(words, i + 1, "ioe", &["input", "output", "error"]),
            "timeout" => {
                i = skip_opts(words, i + 1, "sk", &["signal", "kill-after"]);
                i += 1; // DURATION
            }
            "xargs" => {
                xargs = true;
                i = skip_opts(words, i + 1, "IJLnPsdEaRS", &["replace", "max-args", "max-procs", "max-lines", "max-chars", "delimiter", "arg-file", "eof"]);
            }
            n if SHELLS.contains(&n) => {
                // `sh -c 'script'`
                let mut k = i + 1;
                let mut script_at = None;
                while k < words.len() {
                    let t = words[k].text.as_str();
                    if t == "--" {
                        break;
                    }
                    if t.starts_with('-') && t.len() > 1 && !t.starts_with("--") {
                        if t[1..].contains('c') {
                            script_at = Some(k + 1);
                            break;
                        }
                        k += 1;
                        // `-o option` / `+o option` style values are rare; ignore.
                    } else {
                        break;
                    }
                }
                let args = words[i + 1..].to_vec();
                if let Some(s) = script_at.and_then(|k| words.get(k)) {
                    inner.push(shell_parse::parse(&s.text));
                }
                return Resolved { cmd: Some(Cmd { name: n.to_string(), args, sudo, xargs, shell_c: script_at.is_some() }), inner };
            }
            "eval" => {
                let joined: Vec<&str> = words[i + 1..].iter().map(|w| w.text.as_str()).collect();
                inner.push(shell_parse::parse(&joined.join(" ")));
                return Resolved { cmd: Some(Cmd { name: "eval".into(), args: words[i + 1..].to_vec(), sudo, xargs, shell_c: true }), inner };
            }
            _ => {
                return Resolved { cmd: Some(Cmd { name: base, args: words[i + 1..].to_vec(), sudo, xargs, shell_c: false }), inner };
            }
        }
    }
    Resolved { cmd: None, inner }
}

/// Does this word (via `$(…)` / `<(…)` / backticks) download something?
fn word_downloads(w: &Word) -> bool {
    w.subs.iter().any(script_downloads)
}

fn script_downloads(s: &Script) -> bool {
    s.pipelines.iter().any(|p| {
        p.cmds.iter().any(|c| {
            resolve(c).cmd.map_or(false, |c| is_downloader(&c.name)) || c.words.iter().any(word_downloads)
        })
    })
}

fn is_downloader(n: &str) -> bool {
    matches!(n, "curl" | "wget" | "fetch" | "aria2c" | "http" | "https" | "xh")
}

// ── Flags ──

struct Flags {
    /// All short-option letters concatenated (`-rf -v` → "rfv").
    short: String,
    /// Long option names without `--` and without `=value`.
    longs: Vec<String>,
    pos: Vec<Word>,
}

impl Flags {
    fn has_short(&self, cs: &str) -> bool {
        self.short.chars().any(|c| cs.contains(c))
    }
    fn has_long(&self, l: &str) -> bool {
        self.longs.iter().any(|x| x == l)
    }
    fn has_long_prefix(&self, l: &str) -> bool {
        self.longs.iter().any(|x| x.starts_with(l))
    }
}

fn split_args(args: &[Word]) -> Flags {
    let mut f = Flags { short: String::new(), longs: Vec::new(), pos: Vec::new() };
    let mut dd = false;
    for w in args {
        let t = w.text.as_str();
        if dd {
            f.pos.push(w.clone());
        } else if t == "--" {
            dd = true;
        } else if let Some(l) = t.strip_prefix("--") {
            f.longs.push(l.split('=').next().unwrap_or("").to_string());
        } else if t.len() > 1 && t.starts_with('-') {
            f.short.push_str(&t[1..]);
        } else {
            f.pos.push(w.clone());
        }
    }
    f
}

// ── Paths ──

fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    format!("/{}", parts.join("/"))
}

const PROTECTED: &[&str] = &[
    "/", "/bin", "/sbin", "/usr", "/etc", "/var", "/lib", "/lib64", "/opt", "/private", "/System", "/Library", "/Applications", "/Users", "/home", "/root",
    "/boot", "/dev", "/proc", "/sys", "/Volumes", "/cores", "/Network", "/tmp", "/private/tmp", "/var/tmp", "/private/var/tmp", "/usr/bin", "/usr/sbin",
    "/usr/lib", "/usr/libexec", "/usr/local", "/usr/share", "/var/lib", "/var/log", "/var/db", "/private/var", "/private/etc", "/System/Library",
    "/Library/Frameworks", "/opt/homebrew", "/opt/local",
];

const HOME_PROTECTED: &[&str] = &[
    "Documents", "Desktop", "Downloads", "Library", "Pictures", "Movies", "Music", "Applications", "Public", ".ssh", ".gnupg", ".config", ".aws", ".kube",
    ".local", ".zshrc", ".bashrc", ".gitconfig",
];

const BUILD_DIRS: &[&str] = &[
    "node_modules", "target", "build", "dist", ".next", ".nuxt", ".svelte-kit", "__pycache__", ".pytest_cache", ".mypy_cache", ".ruff_cache", ".tox", ".venv",
    "venv", ".cache", ".gradle", ".parcel-cache", ".turbo", "out", "coverage", ".nyc_output", "DerivedData", "Pods", ".build", ".dart_tool", ".angular",
    ".eggs", "htmlcov", ".terraform", "bower_components", ".sass-cache", "tmp",
];

#[derive(Debug, Clone, PartialEq)]
enum Class {
    Root,
    Home,
    System(String),
    /// `*`, `.*`, `./*`, `.`: everything in the current directory.
    Here,
    Parent,
    /// Depends on a variable / substitution that is not `$HOME`.
    Dynamic,
    Build(String),
    Normal,
}

impl Class {
    fn is_critical(&self) -> bool {
        matches!(self, Class::Root | Class::Home | Class::System(_) | Class::Here | Class::Parent)
    }
}

/// Expand a leading `~` / `$HOME` / `${HOME}`.
fn expand_home(t: &str, home: Option<&str>) -> Option<String> {
    let home = home?;
    if t == "~" {
        return Some(home.to_string());
    }
    if let Some(r) = t.strip_prefix("~/") {
        return Some(format!("{home}/{r}"));
    }
    for p in ["$HOME", "${HOME}"] {
        if let Some(r) = t.strip_prefix(p) {
            if r.is_empty() || r.starts_with('/') {
                return Some(format!("{home}{r}"));
            }
        }
    }
    None
}

fn has_glob(t: &str) -> bool {
    t.contains(['*', '?', '['])
}

/// Classify a path argument. Returns the class plus the normalized absolute
/// path when it is known.
fn classify_path(w: &Word, env: &Env) -> (Class, Option<String>) {
    let raw = w.text.as_str();
    let mut t = raw;
    while t.len() > 1 && t.ends_with('/') {
        t = &t[..t.len() - 1];
    }
    if matches!(t, "*" | ".*" | "./*" | "./.*" | ".[!.]*" | ".??*" | "..?*" | ".[^.]*" | "." | "./") {
        return (Class::Here, None);
    }
    // `dir/*` addresses the contents of `dir`.
    let mut base = t;
    for suf in ["/*", "/.*", "/.[!.]*", "/.??*", "/.[^.]*"] {
        if let Some(b) = base.strip_suffix(suf) {
            base = if b.is_empty() { "/" } else { b };
            break;
        }
    }
    if base == ".." || base.ends_with("/..") {
        return (Class::Parent, None);
    }
    if base == "~" || base == "$HOME" || base == "${HOME}" {
        return (Class::Home, env.home.clone());
    }
    let expanded = expand_home(base, env.home.as_deref());
    let abs = match expanded {
        Some(e) => Some(normalize(&e)),
        None if base.starts_with('/') => Some(normalize(base)),
        None if !w.dynamic && !has_glob(base) => env.cwd.as_ref().map(|c| normalize(&format!("{}/{}", c.display(), base))),
        None => None,
    };
    if let Some(abs) = &abs {
        if abs == "/" {
            return (Class::Root, abs.clone().into());
        }
        if env.home.as_deref() == Some(abs.as_str()) {
            return (Class::Home, Some(abs.clone()));
        }
        if PROTECTED.contains(&abs.as_str()) {
            return (Class::System(abs.clone()), Some(abs.clone()));
        }
        for parent in ["/Users", "/home"] {
            if let Some(rest) = abs.strip_prefix(parent).and_then(|r| r.strip_prefix('/')) {
                if !rest.contains('/') {
                    return (Class::System(abs.clone()), Some(abs.clone())); // somebody's home dir
                }
            }
        }
        if let Some(h) = env.home.as_deref() {
            if let Some(rest) = abs.strip_prefix(h).and_then(|r| r.strip_prefix('/')) {
                if HOME_PROTECTED.contains(&rest) {
                    return (Class::System(abs.clone()), Some(abs.clone()));
                }
            }
        }
    }
    if abs.is_none() && w.dynamic && (base.starts_with('$') || base.starts_with("`") || base.starts_with("<(")) {
        return (Class::Dynamic, None);
    }
    if has_glob(base) {
        return (Class::Normal, abs);
    }
    let last = base.rsplit('/').next().unwrap_or(base);
    if BUILD_DIRS.contains(&last) {
        return (Class::Build(last.to_string()), abs);
    }
    if abs.is_none() && w.dynamic {
        return (Class::Dynamic, None);
    }
    (Class::Normal, abs)
}

fn resolve_fs_path(w: &Word, abs: &Option<String>, env: &Env) -> PathBuf {
    match abs {
        Some(a) => PathBuf::from(a),
        None => match &env.cwd {
            Some(c) => c.join(&w.text),
            None => PathBuf::from(&w.text),
        },
    }
}

fn is_raw_device(p: &str) -> bool {
    let p = normalize(p);
    if !p.starts_with("/dev/") {
        return false;
    }
    let rest = &p[5..];
    const PSEUDO: &[&str] = &["null", "zero", "full", "random", "urandom", "stdin", "stdout", "stderr", "tty", "console", "ptmx", "tty0"];
    !(PSEUDO.contains(&rest) || rest.starts_with("fd/") || rest.starts_with("pts/") || rest.starts_with("ttys") || rest.starts_with("tty."))
}

fn is_system_file(p: &str) -> bool {
    let p = normalize(p);
    ["/etc/", "/System/", "/bin/", "/sbin/", "/boot/", "/usr/bin/", "/usr/sbin/", "/usr/lib/", "/private/etc/", "/Library/LaunchDaemons/"]
        .iter()
        .any(|pre| p.starts_with(pre))
}

fn fmt_count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn scan_detail(scan: &Scan) -> String {
    match scan {
        Scan::Skipped => String::new(),
        Scan::Missing => "not found from here — path may be relative to a different pane".into(),
        Scan::File(n) => format!("file — {}", format_size(*n)),
        Scan::Dir { entries, complete: true } => format!("directory — {} file(s)/dir(s) inside", fmt_count(*entries)),
        Scan::Dir { entries, complete: false } => {
            format!("directory — ≥ {} file(s)/dir(s) inside… (counted for {BUDGET_MS} ms, more not shown)", fmt_count(*entries))
        }
    }
}

// ── Engine ──

struct Eng<'a> {
    env: &'a Env,
    out: Vec<Finding>,
    steps: usize,
}

const MAX_TARGETS: usize = 400;
const MAX_STEPS: usize = 4000;
const MANY_FILES: usize = 1000;
const MANY_TARGETS: usize = 50;

impl Eng<'_> {
    fn script(&mut self, s: &Script, depth: usize) {
        if depth > 10 {
            return;
        }
        if !s.skeleton.is_empty() && shell_parse::has_fork_bomb(&s.skeleton) {
            self.out.push(one(
                Severity::Critical,
                "Fork bomb — recursively spawns processes with no limit",
                "Exhausts the process table / memory; usually needs a hard reboot",
            ));
        }
        for p in &s.pipelines {
            self.pipeline(p, depth);
        }
    }

    fn pipeline(&mut self, p: &Pipeline, depth: usize) {
        let mut stages: Vec<(Option<Cmd>, &Simple)> = Vec::new();
        for simple in &p.cmds {
            self.steps += 1;
            if self.steps > MAX_STEPS {
                return;
            }
            let words = simple.words.iter().chain(simple.assigns.iter().map(|a| &a.1)).chain(simple.redirs.iter().map(|r| &r.target));
            for w in words {
                for sub in &w.subs {
                    self.script(sub, depth + 1);
                }
            }
            let r = resolve(simple);
            for inner in &r.inner {
                self.script(inner, depth + 1);
            }
            if let Some(cmd) = &r.cmd {
                self.command(cmd);
            }
            self.redirects(simple);
            self.bare_sql(simple);
            stages.push((r.cmd, simple));
        }
        self.pipeline_rules(&stages, p);
    }

    fn command(&mut self, cmd: &Cmd) {
        let found = self.rules(cmd);
        // `sudo rm` / `sudo dd` is risky even when the bare command is routine.
        if cmd.sudo && found.is_empty() && matches!(cmd.name.as_str(), "rm" | "dd" | "shred") {
            self.out.push(one(
                Severity::Warning,
                "Runs as root — bypasses normal permission checks",
                format!("`{}` will not be stopped by file permissions", cmd.name),
            ));
        }
        for mut f in found {
            if cmd.sudo {
                f.severity = match f.severity {
                    Severity::Info => Severity::Warning,
                    _ => Severity::Critical,
                };
                f.impacts.insert(0, Impact::new("Running with sudo (root)", "No permission check will stop this command"));
            }
            self.out.push(f);
        }
    }

    fn rules(&mut self, cmd: &Cmd) -> Vec<Finding> {
        let n = cmd.name.as_str();
        let mut v = Vec::new();
        let push = |v: &mut Vec<Finding>, f: Option<Finding>| v.extend(f);
        match n {
            "rm" | "unlink" | "rmdir" => push(&mut v, self.rule_rm(cmd)),
            "find" => v.extend(self.rule_find(cmd)),
            "shred" | "srm" => push(&mut v, self.rule_shred(cmd)),
            "dd" => push(&mut v, self.rule_dd(cmd)),
            "truncate" => push(&mut v, self.rule_truncate(cmd)),
            "diskutil" => push(&mut v, rule_diskutil(cmd)),
            "chmod" | "chown" | "chgrp" => push(&mut v, self.rule_chmod(cmd)),
            "git" => push(&mut v, self.rule_git(cmd)),
            "rsync" => push(&mut v, self.rule_rsync(cmd)),
            "kubectl" | "oc" => push(&mut v, rule_kubectl(cmd)),
            "helm" => push(&mut v, rule_helm(cmd)),
            "terraform" | "tofu" | "terragrunt" => push(&mut v, rule_terraform(cmd)),
            "docker" | "podman" | "nerdctl" => push(&mut v, rule_docker(cmd)),
            "kill" => push(&mut v, rule_kill(cmd)),
            "killall" | "pkill" => push(&mut v, rule_killall(cmd)),
            "dropdb" | "dropuser" => v.push(one(Severity::Critical, "Permanently deletes a database", "Every table and row in it is destroyed — no undo without a backup")),
            "mysqladmin" if cmd.args.iter().any(|w| w.text == "drop") => {
                v.push(one(Severity::Critical, "Permanently deletes an entire database", "Every table, row, and index in it is destroyed — no undo without a backup"))
            }
            "redis-cli" => push(&mut v, rule_redis(cmd)),
            "mongo" | "mongosh" => {
                let all: String = cmd.args.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
                if all.contains("dropDatabase") || all.contains(".drop()") {
                    v.push(one(Severity::Critical, "Permanently deletes a MongoDB database/collection", "All documents are destroyed — no undo without a backup"));
                }
            }
            "source" | "." => {
                if cmd.args.iter().any(word_downloads) {
                    v.push(download_exec());
                }
            }
            n if n.starts_with("mkfs") || matches!(n, "mke2fs" | "mkswap" | "mkdosfs" | "mkntfs" | "wipefs" | "newfs") || n.starts_with("newfs_") => {
                v.push(rule_mkfs(cmd))
            }
            n if SHELLS.contains(&n) || n == "eval" => {
                if cmd.args.iter().any(word_downloads) {
                    v.push(download_exec());
                }
            }
            n if is_sql_client(n) => v.extend(self.rule_sql_args(cmd)),
            _ => {}
        }
        // `xargs <cmd>` for commands whose targets are not rm-like: nothing extra.
        v
    }

    // ── rm ──

    fn rule_rm(&mut self, cmd: &Cmd) -> Option<Finding> {
        let f = split_args(&cmd.args);
        let rec = f.has_short("rR") || f.has_long("recursive");
        let force = f.has_short("f") || f.has_long("force");
        let nopreserve = f.has_long("no-preserve-root");
        if cmd.xargs {
            let (sev, head) = if rec {
                (Severity::Critical, "Recursively deletes every path piped into xargs")
            } else {
                (Severity::Warning, "Deletes every path piped into xargs")
            };
            return Some(one(sev, head, "The list is only known at run time — run the producer alone first (drop `| xargs rm …`) to see what it would delete"));
        }
        if f.pos.is_empty() {
            return None;
        }
        let truncated = f.pos.len() > MAX_TARGETS;
        let classes: Vec<(Class, Option<String>)> = f.pos.iter().take(MAX_TARGETS).map(|w| classify_path(w, self.env)).collect();
        let any_crit = classes.iter().any(|(c, _)| c.is_critical());
        let many = f.pos.len() > MANY_TARGETS;

        if !rec {
            if any_crit || many {
                return Some(finding(
                    Severity::Warning,
                    vec![Impact::new(
                        if many { format!("Deletes {} files at once", fmt_count(f.pos.len())) } else { "Deletes a protected path or every file here".to_string() },
                        "Permanent — bypasses the Trash",
                    )],
                ));
            }
            return None;
        }

        let all_build = classes.iter().all(|(c, _)| matches!(c, Class::Build(_)));
        let mut sev = if any_crit || many {
            Severity::Critical
        } else if all_build {
            Severity::Info
        } else {
            Severity::Warning
        };

        let mut impacts = Vec::new();
        let how = if force { "Force-deletes" } else { "Deletes" };
        if sev == Severity::Info {
            impacts.push(Impact::plain(format!("{how} build/cache output recursively — normally regenerable")));
        } else {
            let n = f.pos.len();
            impacts.push(Impact::plain(format!(
                "{how} {} recursively — permanent, no Trash{}",
                if n == 1 { "1 path".to_string() } else { format!("{} paths", fmt_count(n)) },
                if force { ", no prompts" } else { "" }
            )));
        }
        if nopreserve {
            impacts.push(Impact::new("--no-preserve-root", "Disables the safeguard that normally makes rm refuse to delete /"));
        }

        let mut scanned = 0;
        for (w, (class, abs)) in f.pos.iter().zip(classes.iter()).take(8) {
            let shown = w.text.clone();
            match class {
                Class::Root => impacts.push(Impact::new(shown, "the filesystem root — everything on this volume would be removed")),
                Class::Home => impacts.push(Impact::new(shown, "your home directory — documents, keys, configs; not recoverable")),
                Class::System(p) => impacts.push(Impact::new(shown, format!("system/user directory {p} — not scanned; software or the OS may break"))),
                Class::Here => {
                    let here = self.env.cwd.as_ref().map(|c| c.display().to_string()).unwrap_or_else(|| "the current directory".into());
                    impacts.push(Impact::new(shown, format!("everything in {here}")));
                }
                Class::Parent => impacts.push(Impact::new(shown, "the parent directory and everything below it")),
                Class::Dynamic => impacts.push(Impact::new(shown, "target comes from a shell expansion — if it is empty or wrong this can hit far more than intended")),
                Class::Build(_) | Class::Normal => {
                    let mut detail = String::new();
                    if sev != Severity::Info && scanned < 6 && !has_glob(&w.text) {
                        scanned += 1;
                        let scan = analysis::scan_path(&resolve_fs_path(w, abs, self.env), &self.env.budget);
                        if let Scan::Dir { entries, .. } = scan {
                            if entries >= MANY_FILES && sev == Severity::Warning {
                                sev = Severity::Critical;
                            }
                        }
                        detail = scan_detail(&scan);
                    }
                    impacts.push(Impact::new(shown, detail));
                }
            }
        }
        if truncated || f.pos.len() > 8 {
            impacts.push(Impact::plain(format!("...and {} more path(s)", f.pos.len().saturating_sub(8))));
        }
        Some(finding(sev, impacts))
    }

    // ── find ──

    fn rule_find(&mut self, cmd: &Cmd) -> Vec<Finding> {
        let words = &cmd.args;
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        let start: Vec<&Word> = words
            .iter()
            .take_while(|w| !(w.text.starts_with('-') && w.text.len() > 1) && w.text != "(" && w.text != "!" && w.text != "\\(")
            .collect();
        const FILTERS: &[&str] = &[
            "-name", "-iname", "-path", "-ipath", "-regex", "-iregex", "-type", "-mtime", "-mmin", "-newer", "-size", "-user", "-perm", "-empty", "-atime",
            "-ctime", "-cmin", "-lname", "-wholename", "-newermt",
        ];
        let filtered = texts.iter().any(|t| FILTERS.contains(t));
        let has_delete = texts.contains(&"-delete");
        let start_crit = start.iter().any(|w| classify_path(w, self.env).0.is_critical()) && !start.is_empty()
            && start.iter().any(|w| !matches!(classify_path(w, self.env).0, Class::Here) || !filtered);
        let mut inner_found: Vec<Finding> = Vec::new();
        let mut inner_rm = false;
        let mut i = 0;
        while i < texts.len() {
            if matches!(texts[i], "-exec" | "-execdir" | "-ok" | "-okdir") {
                let mut j = i + 1;
                let mut payload: Vec<Word> = Vec::new();
                while j < texts.len() && texts[j] != ";" && texts[j] != "+" {
                    if texts[j] != "{}" {
                        payload.push(words[j].clone());
                    }
                    j += 1;
                }
                let simple = Simple { words: payload, ..Default::default() };
                if let Some(c) = resolve(&simple).cmd {
                    if matches!(c.name.as_str(), "rm" | "shred" | "unlink" | "rmdir") {
                        inner_rm = true;
                    }
                    inner_found.extend(self.rules(&c));
                }
                i = j;
            }
            i += 1;
        }
        let mut out = Vec::new();
        if has_delete || inner_rm {
            let sev = if !filtered || start_crit { Severity::Critical } else { Severity::Warning };
            let where_ = if start.is_empty() { ".".to_string() } else { start.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ") };
            let what = if has_delete { "-delete" } else { "-exec rm" };
            out.push(one(
                sev,
                if filtered { format!("Deletes every file under {where_} that matches the find filter ({what})") } else { format!("Deletes EVERY file under {where_} ({what} with no filter)") },
                "Permanent — run the same find without the delete action first to list what matches",
            ));
        }
        for f in inner_found {
            // Mass application of a risky inner command is at least a warning.
            out.push(finding(if f.severity == Severity::Info { Severity::Warning } else { f.severity }, f.impacts));
        }
        out
    }

    // ── disks ──

    fn rule_shred(&mut self, cmd: &Cmd) -> Option<Finding> {
        let f = split_args(&cmd.args);
        let crit = f.pos.iter().any(|w| is_raw_device(&w.text) || is_system_file(&w.text) || classify_path(w, self.env).0.is_critical());
        if f.pos.is_empty() {
            return None;
        }
        Some(one(
            if crit { Severity::Critical } else { Severity::Warning },
            if crit { "Overwrites a device or protected path with random data" } else { "Overwrites file contents so they cannot be recovered" },
            "Unlike rm, the data is destroyed, not just unlinked — no undo",
        ))
    }

    fn rule_dd(&mut self, cmd: &Cmd) -> Option<Finding> {
        let arg = |p: &str| cmd.args.iter().find_map(|w| w.text.strip_prefix(p).map(|s| s.to_string()));
        let of = arg("of=")?;
        let crit = is_raw_device(&of) || is_system_file(&of);
        if !crit {
            return None;
        }
        let mut impacts = vec![Impact::new(
            format!("Overwrites raw device: {of}"),
            "Destroys the partition table and all data on that disk",
        )];
        if let Some(i) = arg("if=") {
            impacts.push(Impact::plain(format!("Source: {i}")));
        }
        Some(finding(Severity::Critical, impacts))
    }

    fn rule_truncate(&mut self, cmd: &Cmd) -> Option<Finding> {
        let f = split_args(&cmd.args);
        let bad = f.pos.iter().find(|w| is_raw_device(&w.text) || is_system_file(&w.text))?;
        Some(one(Severity::Critical, format!("Truncates {}", bad.text), "System file or raw device — contents are lost"))
    }

    // ── permissions ──

    fn rule_chmod(&mut self, cmd: &Cmd) -> Option<Finding> {
        let f = split_args(&cmd.args);
        if !(f.has_short("R") || f.has_long("recursive")) {
            return None;
        }
        let skip = if cmd.name == "chgrp" || cmd.name == "chown" || cmd.name == "chmod" { 1 } else { 0 };
        let targets: Vec<&Word> = f.pos.iter().skip(skip).collect();
        if targets.iter().any(|w| classify_path(w, self.env).0.is_critical()) {
            return Some(one(
                Severity::Critical,
                format!("Recursively changes {} on /, your home directory or a system path", if cmd.name == "chmod" { "permissions" } else { "ownership" }),
                "Can break sudo, ssh keys and the OS itself; there is no undo",
            ));
        }
        if cmd.name == "chmod" {
            let mode = f.pos.first().map(|w| w.text.as_str()).unwrap_or("");
            if mode.trim_start_matches('0') == "777" || matches!(mode, "a+rwx" | "ugo+rwx" | "o+rwx" | "a=rwx") {
                return Some(one(
                    Severity::Warning,
                    "Grants read/write/execute to EVERYONE, recursively",
                    "Common privilege-escalation vector — rarely what you actually want",
                ));
            }
        }
        None
    }

    // ── git ──

    fn rule_git(&mut self, cmd: &Cmd) -> Option<Finding> {
        let a = &cmd.args;
        let mut i = 0;
        while i < a.len() {
            let t = a[i].text.as_str();
            if matches!(t, "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path" | "--super-prefix" | "--config-env") {
                i += 2;
            } else if t.starts_with('-') {
                i += 1;
            } else {
                break;
            }
        }
        let sub = a.get(i)?.text.as_str();
        let rest = &a[i + 1..];
        let f = split_args(rest);
        let cwd = self.env.cwd.clone();
        let budget = self.env.budget;
        match sub {
            "push" => {
                let lease = f.has_long_prefix("force-with-lease") || f.has_long("force-if-includes");
                let short_force = {
                    // `-f`, `-fu`, `-uf` (clusters made only of push short flags)
                    rest.iter().any(|w| {
                        let t = w.text.as_str();
                        t.len() > 1 && t.starts_with('-') && !t.starts_with("--") && t[1..].chars().all(|c| "fundvqk46".contains(c)) && t[1..].contains('f')
                    })
                };
                let refspecs: Vec<&str> = f.pos.iter().skip(1).map(|w| w.text.as_str()).collect();
                let plus = refspecs.iter().any(|r| r.starts_with('+'));
                let force = f.has_long("force") || short_force || plus;
                let mirror = f.has_long("mirror");
                let delete = f.has_long("delete") || refspecs.iter().any(|r| r.starts_with(':'));
                if !force && !lease && !mirror && !delete {
                    return None;
                }
                let protected = |r: &str| {
                    let dst = r.trim_start_matches('+');
                    let dst = dst.rsplit(':').next().unwrap_or(dst).trim_start_matches("refs/heads/");
                    matches!(dst, "main" | "master" | "trunk" | "develop" | "development" | "dev" | "prod" | "production" | "stable" | "HEAD")
                        || dst.starts_with("release")
                };
                let hits_protected = refspecs.iter().any(|r| protected(r)) || (refspecs.is_empty() && (f.has_long("all") || f.has_long("tags")));
                let mut impacts = Vec::new();
                let mut sev = Severity::Warning;
                if mirror {
                    sev = Severity::Critical;
                    impacts.push(Impact::new("Mirrors the local repo over the remote", "Remote branches and tags that do not exist locally are deleted"));
                } else if force && !lease {
                    if hits_protected {
                        sev = Severity::Critical;
                    }
                    impacts.push(Impact::new(
                        if hits_protected { "Force-pushes over a shared branch (main/master/release…)" } else { "Overwrites remote branch history" },
                        "Anyone who already pulled the old history can lose work merging back",
                    ));
                } else if lease {
                    impacts.push(Impact::new(
                        "Force-pushes with a lease — rewrites remote history",
                        "Safer variant: aborts if the remote moved since your last fetch",
                    ));
                } else if delete {
                    impacts.push(Impact::new("Deletes a remote branch/tag", "Other clones keep working, but the branch is gone from the remote"));
                }
                if let Some(out) = analysis::git(cwd.as_deref(), &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"], &budget) {
                    let mut it = out.split_whitespace();
                    if let (Some(Ok(behind)), Some(Ok(ahead))) = (it.next().map(str::parse::<usize>), it.next().map(str::parse::<usize>)) {
                        if behind > 0 {
                            impacts.push(Impact::new(format!("Remote has {behind} commit(s) not in your local branch"), "Those commits would be discarded from the remote branch"));
                        }
                        if ahead > 0 {
                            impacts.push(Impact::plain(format!("Local branch is ahead by {ahead} commit(s)")));
                        }
                    }
                }
                Some(finding(sev, impacts))
            }
            "reset" if f.has_long("hard") => {
                let mut impacts = vec![Impact::new("Discards ALL uncommitted changes permanently", "Working directory and index are reset to match the target commit")];
                let mut sev = Severity::Warning;
                if let Some(out) = analysis::git(cwd.as_deref(), &["status", "--porcelain"], &budget) {
                    let lines: Vec<&str> = out.lines().collect();
                    if lines.is_empty() {
                        impacts.push(Impact::new("Working tree is clean", "Nothing uncommitted to lose — only HEAD/branch moves"));
                    } else {
                        sev = Severity::Critical;
                        for l in lines.iter().take(6) {
                            impacts.push(Impact::plain(l.trim().to_string()));
                        }
                        if lines.len() > 6 {
                            impacts.push(Impact::plain(format!("...and {} more changed file(s)", lines.len() - 6)));
                        }
                    }
                } else if budget.enabled {
                    impacts.push(Impact::new("Could not inspect the working tree in time", "(not a git repo, git missing, or the repo is very large)"));
                }
                Some(finding(sev, impacts))
            }
            "clean" => {
                let force = f.has_short("f") || f.has_long("force");
                let dry = f.has_short("n") || f.has_long("dry-run");
                if !force || dry {
                    return None;
                }
                let ignored = f.has_short("xX");
                let mut impacts = vec![Impact::new(
                    if ignored { "Permanently removes untracked AND git-ignored files" } else { "Permanently removes untracked files" },
                    if ignored { "Includes .env files, build output, local config — never committed, not recoverable" } else { "Not recoverable from git — these files were never committed" },
                )];
                let dry_args: &[&str] = if f.has_short("x") { &["clean", "-fdxn"] } else if f.has_short("X") { &["clean", "-fdXn"] } else { &["clean", "-fdn"] };
                if let Some(out) = analysis::git(cwd.as_deref(), dry_args, &budget) {
                    let files: Vec<&str> = out.lines().filter_map(|l| l.strip_prefix("Would remove ")).collect();
                    if files.is_empty() {
                        impacts.push(Impact::plain("No untracked files found"));
                    }
                    for fl in files.iter().take(8) {
                        impacts.push(Impact::plain((*fl).to_string()));
                    }
                    if files.len() > 8 {
                        impacts.push(Impact::plain(format!("...and {} more", files.len() - 8)));
                    }
                }
                Some(finding(if ignored { Severity::Critical } else { Severity::Warning }, impacts))
            }
            "checkout" => {
                let all = f.pos.iter().any(|w| matches!(w.text.as_str(), "." | "./" | ":/" | "*"));
                if all {
                    Some(one(Severity::Warning, "Discards all uncommitted changes to tracked files here", "Restores them from the index/commit — local edits are lost"))
                } else if f.has_short("f") || f.has_long("force") {
                    Some(one(Severity::Warning, "Forced checkout discards local modifications", "Uncommitted changes that conflict are overwritten"))
                } else {
                    None
                }
            }
            "restore" => {
                let all = f.pos.iter().any(|w| matches!(w.text.as_str(), "." | "./" | ":/" | "*"));
                let staged_only = (f.has_long("staged") || f.has_short("S")) && !(f.has_long("worktree") || f.has_short("W"));
                if all && !staged_only {
                    Some(one(Severity::Warning, "Discards all uncommitted changes to tracked files here", "Restores them from the index/commit — local edits are lost"))
                } else {
                    None
                }
            }
            "stash" if f.pos.first().map_or(false, |w| w.text == "clear") => {
                Some(one(Severity::Warning, "Deletes ALL stashes", "Stashed work becomes unreachable (recoverable only via fsck)"))
            }
            _ => None,
        }
    }

    // ── rsync ──

    fn rule_rsync(&mut self, cmd: &Cmd) -> Option<Finding> {
        let f = split_args(&cmd.args);
        let del = f.longs.iter().any(|l| l == "del" || l.starts_with("delete")) || f.has_long("remove-source-files");
        if !del || f.has_long("dry-run") || f.has_short("n") {
            return None;
        }
        let dest = f.pos.last().filter(|_| f.pos.len() >= 2)?;
        let crit = classify_path(dest, self.env).0.is_critical();
        Some(one(
            if crit { Severity::Critical } else { Severity::Warning },
            format!("Deletes files in the destination ({}) that are missing from the source", dest.text),
            if crit { "The destination is your home or a system path — a wrong source (e.g. an empty dir) wipes it" } else { "Check source and destination; try --dry-run first" },
        ))
    }

    // ── sql ──

    fn rule_sql_args(&mut self, cmd: &Cmd) -> Option<Finding> {
        let texts: Vec<&str> = cmd.args.iter().map(|w| w.text.as_str()).collect();
        sql_finding(texts.iter().copied())
    }

    fn bare_sql(&mut self, simple: &Simple) {
        // Typed straight into a SQL prompt: `DROP TABLE users;` (unquoted).
        let w = &simple.words;
        if w.len() >= 2 && !w[0].quoted && (w[0].text.eq_ignore_ascii_case("drop") || w[0].text.eq_ignore_ascii_case("truncate")) {
            let t: Vec<&str> = w.iter().map(|w| w.text.as_str()).collect();
            let joined = t.join(" ");
            if let Some(f) = sql_finding(std::iter::once(joined.as_str())) {
                self.out.push(f);
            }
        }
    }

    // ── redirections ──

    fn redirects(&mut self, simple: &Simple) {
        for r in &simple.redirs {
            let out = r.op.starts_with('>') || r.op.starts_with("&>");
            if !out || r.op == ">&" && (r.target.text.chars().all(|c| c.is_ascii_digit() || c == '-')) {
                continue;
            }
            let t = r.target.text.as_str();
            if is_raw_device(t) {
                self.out.push(finding(
                    Severity::Critical,
                    vec![Impact::new(format!("Overwrites raw device: {t}"), "Destroys the partition table and all data on that disk")],
                ));
            } else if is_system_file(t) {
                if r.op == ">>" {
                    self.out.push(one(Severity::Warning, format!("Appends to system file {t}"), "A bad line can break boot, login or networking"));
                } else {
                    self.out.push(one(Severity::Critical, format!("Truncates/overwrites system file {t}"), "Its previous contents are lost — can break the OS or logins"));
                }
            }
        }
    }

    // ── pipelines ──

    fn pipeline_rules(&mut self, stages: &[(Option<Cmd>, &Simple)], p: &Pipeline) {
        for (j, (cj, _)) in stages.iter().enumerate() {
            let Some(cj) = cj else { continue };
            let reads_script = (SHELLS.contains(&cj.name.as_str()) || INTERPRETERS.contains(&cj.name.as_str())) && !cj.shell_c && {
                let f = split_args(&cj.args);
                // `sh script.sh` runs a file; `sh`, `sh -`, `sh -s -- args`, `python -` read stdin.
                f.pos.is_empty() || f.has_short("s") || f.pos.first().map_or(false, |w| w.text == "-")
            };
            if reads_script
                && stages[..j].iter().any(|(c, _)| c.as_ref().map_or(false, |c| is_downloader(&c.name)))
            {
                let mut f = download_exec();
                if cj.sudo {
                    f.impacts.insert(0, Impact::new("Running with sudo (root)", "The downloaded script gets full root access"));
                }
                self.out.push(f);
            }
            // producers feeding a SQL client (`echo 'DROP TABLE x' | psql`)
            if is_sql_client(&cj.name) {
                let mut texts: Vec<&str> = p.stdin_texts.iter().map(String::as_str).collect();
                for (c, _) in &stages[..j] {
                    if let Some(c) = c {
                        if matches!(c.name.as_str(), "echo" | "printf" | "cat") {
                            texts.extend(c.args.iter().map(|w| w.text.as_str()));
                        }
                    }
                }
                if let Some(f) = sql_finding(texts.into_iter()) {
                    self.out.push(f);
                }
            }
        }
    }
}

fn is_sql_client(n: &str) -> bool {
    matches!(
        n,
        "psql" | "pgcli" | "mysql" | "mycli" | "mariadb" | "sqlite3" | "sqlcmd" | "usql" | "clickhouse-client" | "clickhouse" | "cockroach" | "sqlplus" | "bq" | "snowsql" | "duckdb" | "litecli"
    )
}

fn download_exec() -> Finding {
    one(
        Severity::Critical,
        "Downloads a script and executes it immediately",
        "You never see what runs — download it to a file and read it first",
    )
}

/// DROP/TRUNCATE statements inside any of the texts.
fn sql_finding<'a>(texts: impl Iterator<Item = &'a str>) -> Option<Finding> {
    for t in texts {
        let lower = t.to_ascii_lowercase();
        let toks: Vec<&str> = lower.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').filter(|s| !s.is_empty()).collect();
        for pair in toks.windows(2) {
            match (pair[0], pair[1]) {
                ("drop", "table") => {
                    return Some(one(Severity::Critical, "Permanently deletes a database table", "All rows and the schema are destroyed — no undo without a backup"));
                }
                ("drop", "database") | ("drop", "schema") => {
                    return Some(one(Severity::Critical, "Permanently deletes an entire database", "Every table, row, and index in it is destroyed — no undo without a backup"));
                }
                ("truncate", "table") => {
                    return Some(one(Severity::Critical, "Removes every row from a table", "TRUNCATE cannot be rolled back on most engines"));
                }
                _ => {}
            }
        }
    }
    None
}

fn rule_mkfs(cmd: &Cmd) -> Finding {
    let f = split_args(&cmd.args);
    let mut impacts = vec![Impact::plain("Formats a filesystem — ALL DATA on the target is destroyed")];
    if let Some(t) = f.pos.last() {
        impacts.push(Impact::new(format!("Target: {}", t.text), "Double-check this is the right device, not your main disk"));
    }
    finding(Severity::Critical, impacts)
}

fn rule_diskutil(cmd: &Cmd) -> Option<Finding> {
    let sub = cmd.args.first()?.text.to_ascii_lowercase();
    let second = cmd.args.get(1).map(|w| w.text.to_ascii_lowercase()).unwrap_or_default();
    let bad = sub.starts_with("erase")
        || matches!(sub.as_str(), "zerodisk" | "randomdisk" | "secureerase" | "partitiondisk" | "repartitiondisk" | "reformat" | "mergepartitions" | "splitpartition")
        || (sub == "apfs" && (second.starts_with("delete") || second.starts_with("erase")));
    if !bad {
        return None;
    }
    let target = cmd.args.last().map(|w| w.text.clone()).unwrap_or_default();
    Some(finding(
        Severity::Critical,
        vec![Impact::new(format!("diskutil {sub}: erases or repartitions a disk/volume"), format!("Target: {target} — all data on it is destroyed"))],
    ))
}

fn rule_kubectl(cmd: &Cmd) -> Option<Finding> {
    let f = split_args(&cmd.args);
    if f.pos.first().map(|w| w.text.as_str()) != Some("delete") || f.has_long("help") || f.has_short("h") {
        return None;
    }
    let res = f.pos.get(1).map(|w| w.text.to_ascii_lowercase()).unwrap_or_default();
    let all = f.has_long("all") || f.has_long("all-namespaces") || f.has_short("A");
    let big = matches!(
        res.split(['/', ',']).next().unwrap_or(""),
        "namespace" | "namespaces" | "ns" | "node" | "nodes" | "pv" | "persistentvolume" | "persistentvolumes" | "crd" | "crds" | "customresourcedefinition" | "customresourcedefinitions" | "all"
    ) || res.contains("cluster");
    let sev = if all || big { Severity::Critical } else { Severity::Warning };
    Some(one(
        sev,
        if all { "Deletes ALL matching resources".to_string() } else if big { format!("Deletes a cluster-scoped or namespace-level resource ({res})") } else { "Deletes Kubernetes resources".to_string() },
        if sev == Severity::Critical { "Everything inside (pods, volumes, secrets) goes with it; check your current context first" } else { "Check `kubectl config current-context` — this runs against whichever cluster is selected" },
    ))
}

fn rule_helm(cmd: &Cmd) -> Option<Finding> {
    let f = split_args(&cmd.args);
    matches!(f.pos.first().map(|w| w.text.as_str()), Some("uninstall" | "delete")).then(|| {
        one(Severity::Warning, "Uninstalls a Helm release", "All resources it manages are deleted from the current cluster")
    })
}

fn rule_terraform(cmd: &Cmd) -> Option<Finding> {
    let f = split_args(&cmd.args);
    let destroy = f.pos.iter().any(|w| w.text == "destroy") || ((f.has_long("destroy") || f.short.contains("destroy")) && f.pos.iter().any(|w| w.text == "apply"));
    if !destroy || f.pos.iter().any(|w| w.text == "plan") {
        return None;
    }
    let auto = f.has_long("auto-approve");
    Some(one(
        Severity::Critical,
        "Destroys managed infrastructure",
        if auto { "-auto-approve: no further confirmation — every resource in the state is deleted" } else { "Every resource in the state is deleted after terraform's own prompt" },
    ))
}

fn rule_docker(cmd: &Cmd) -> Option<Finding> {
    let f = split_args(&cmd.args);
    let p0 = f.pos.first().map(|w| w.text.as_str());
    let p1 = f.pos.get(1).map(|w| w.text.as_str());
    let all = f.has_short("a") || f.has_long("all");
    match (p0, p1) {
        (Some("system"), Some("prune")) => Some(one(
            Severity::Warning,
            "Removes all unused Docker data",
            if all { "Stopped containers, unused networks, dangling AND unused images, build cache" } else { "Stopped containers, unused networks, dangling images, build cache" },
        )),
        (Some("volume"), Some("prune")) => Some(one(Severity::Warning, "Removes all unused Docker volumes", "Data stored in volumes no container references is lost")),
        (Some("compose"), Some("down")) if f.has_short("v") || f.has_long("volumes") => {
            Some(one(Severity::Warning, "Stops the stack and deletes its volumes", "Database and other persisted data in named volumes is lost"))
        }
        _ => None,
    }
}

fn rule_kill(cmd: &Cmd) -> Option<Finding> {
    let toks: Vec<&str> = cmd.args.iter().map(|w| w.text.as_str()).collect();
    let sig9 = toks.first().map_or(false, |t| *t == "-9" || t.eq_ignore_ascii_case("-sigkill") || t.eq_ignore_ascii_case("-kill"))
        || (toks.first() == Some(&"-s") && toks.get(1).map_or(false, |s| s.eq_ignore_ascii_case("kill") || *s == "9"));
    if !sig9 {
        return None;
    }
    let start = if toks.first() == Some(&"-s") { 2 } else { 1 };
    let pids: Vec<&str> = toks.iter().skip(start).copied().collect();
    if pids.iter().any(|p| matches!(*p, "-1" | "0" | "1")) {
        return Some(one(Severity::Critical, "Force-kills EVERY process you can signal (or init)", "Logs you out and loses all unsaved state"));
    }
    Some(one(
        Severity::Warning,
        "Force-kills a process (SIGKILL)",
        format!("PID {}: no cleanup handlers run, unsaved state is lost", pids.first().copied().unwrap_or("?")),
    ))
}

fn rule_killall(cmd: &Cmd) -> Option<Finding> {
    let f = split_args(&cmd.args);
    let forced = f.has_short("9")
        || cmd.args.windows(2).any(|w| w[0].text == "-s" && w[1].text.eq_ignore_ascii_case("kill"))
        || cmd.args.iter().any(|w| w.text.eq_ignore_ascii_case("-kill") || w.text.eq_ignore_ascii_case("-sigkill"));
    if !forced {
        return None;
    }
    let name = f.pos.first().map(|w| w.text.as_str()).unwrap_or("?");
    Some(one(Severity::Warning, "Force-kills EVERY process matching this name", format!("Target: {name} — may hit more processes than intended")))
}

fn rule_redis(cmd: &Cmd) -> Option<Finding> {
    for w in &cmd.args {
        match w.text.to_ascii_lowercase().as_str() {
            "flushall" => return Some(one(Severity::Critical, "Deletes every key in every Redis database", "No undo unless persistence/backups exist")),
            "flushdb" => return Some(one(Severity::Warning, "Deletes every key in the current Redis database", "No undo unless persistence/backups exist")),
            _ => {}
        }
    }
    None
}
