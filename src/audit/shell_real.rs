//! Item 1: real shells (zsh with the user's real ~/.zshrc, bash) through a real
//! PTY, launched with Rift's own `build_shell_command()`.
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{pane, screen_text, Soft};
use crate::window::Pane;

pub static ENV_LOCK: Mutex<()> = Mutex::new(());

pub struct Sh {
    pub pane: Pane,
    pub raw: Vec<u8>,
    pub started: Instant,
}

impl Sh {
    /// `shell`: value for $SHELL (None = keep the user's). `no_si`: set RIFT_NO_SHELL_INTEGRATION.
    pub fn spawn(shell: Option<&str>, no_si: bool, cwd: &str, cols: u16, rows: u16) -> Sh {
        match shell {
            Some(s) => std::env::set_var("SHELL", s),
            None => {}
        }
        if no_si {
            std::env::set_var("RIFT_NO_SHELL_INTEGRATION", "1");
        } else {
            std::env::remove_var("RIFT_NO_SHELL_INTEGRATION");
        }
        let mut cmd = crate::shell_integration::build_shell_command();
        cmd.cwd(cwd);
        let pty = crate::pty::Pty::spawn_cmd(cols, rows, cmd, std::sync::Arc::new(|| {})).expect("spawn");
        // The pane owns the production PTY (bounded channel, reaper thread), not a replica.
        let mut pane = pane(cols as usize, rows as usize);
        pane.pty = crate::window::pane::PtyKind::Local(pty);
        Sh { pane, raw: Vec::new(), started: Instant::now() }
    }

    fn pty(&self) -> &crate::pty::Pty {
        match &self.pane.pty {
            crate::window::pane::PtyKind::Local(p) => p,
            _ => unreachable!("Sh always owns a local PTY"),
        }
    }

    /// One production parse pass (`Pane::process_output`: time-budgeted); returns whether
    /// output is still queued afterwards.
    pub fn budgeted_pass(&mut self) -> bool {
        self.pane.process_output();
        self.pane.backlog
    }

    pub fn pump(&mut self, ms: u64) {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            self.drain();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn drain(&mut self) -> bool {
        let mut any = false;
        while let Some(d) = self.pane.pty.try_read() {
            self.raw.extend_from_slice(&d);
            self.pane.feed(&d);
            any = true;
        }
        // Answer terminal queries (CPR, DA) like the app's flush_responses does.
        let resp: Vec<Vec<u8>> = self.pane.terminal.response_queue.drain(..).collect();
        for r in resp {
            self.pane.pty.write(&r);
        }
        any
    }

    pub fn wait_until(&mut self, secs: u64, f: impl Fn(&crate::terminal::Terminal) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            self.drain();
            if f(&self.pane.terminal) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        self.drain();
        f(&self.pane.terminal)
    }

    /// Wait until output has been quiet for `quiet_ms`.
    pub fn wait_quiet(&mut self, quiet_ms: u64, max_secs: u64) {
        let end = Instant::now() + Duration::from_secs(max_secs);
        let mut last = Instant::now();
        while Instant::now() < end {
            if self.drain() {
                last = Instant::now();
            }
            if last.elapsed() > Duration::from_millis(quiet_ms) {
                return;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    pub fn send(&mut self, s: &str) {
        self.pane.pty.write(s.as_bytes());
    }

    pub fn prompt_ready(&mut self, secs: u64) -> bool {
        self.wait_until(secs, |t| t.at_shell_prompt())
    }

    /// Type `cmd` + Enter and wait until a new finished block appears and the prompt is back.
    pub fn run(&mut self, cmd: &str, secs: u64) -> bool {
        let n = self.pane.terminal.blocks.blocks().len();
        self.send(cmd);
        self.send("\r");
        self.wait_until(secs, |t| t.blocks.blocks().len() > n && !t.blocks.is_running() && t.at_shell_prompt())
    }

    pub fn last_block_output(&self) -> String {
        let t = &self.pane.terminal;
        match t.blocks.blocks().last() {
            Some(b) => crate::blocks_ui::output_text(t, b.output_start, b.output_end),
            None => String::new(),
        }
    }

    pub fn alive(&mut self) -> bool {
        self.pty().exit_status().is_none()
    }

    pub fn kill(self) {
        self.pty().hangup();
    }
}

fn describe_blocks(s: &Soft, sh: &Sh) {
    let t = &sh.pane.terminal;
    for (i, b) in t.blocks.blocks().iter().enumerate() {
        let out = crate::blocks_ui::output_text(t, b.output_start, b.output_end);
        s.info(
            "blocks",
            format!(
                "#{i} cmd={:?} exit={:?} dur={}ms prompt_line={} cmd_line={} out={}..{} out_text={:?}",
                b.command, b.exit_code, b.duration_ms, b.prompt_line, b.command_line, b.output_start, b.output_end,
                out.chars().take(120).collect::<String>()
            ),
        );
    }
}

fn exercise(label: &'static str, shell: Option<&str>, no_si: bool) -> (Soft, Option<String>) {
    exercise_in(label, shell, no_si, None)
}

fn exercise_in(label: &'static str, shell: Option<&str>, no_si: bool, zdotdir: Option<&std::path::Path>) -> (Soft, Option<String>) {
    match zdotdir {
        Some(z) => std::env::set_var("ZDOTDIR", z),
        None => std::env::remove_var("ZDOTDIR"),
    }
    let mut s = Soft::new("shell");
    let dir = std::env::temp_dir().join(format!("rift-audit-sh-{}-{}", label, std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let dir_s = dir.to_string_lossy().to_string();
    let mut sh = Sh::spawn(shell, no_si, &dir_s, 100, 30);
    let id = |x: &str| format!("{label}.{x}");

    if no_si {
        // A heavy rc (oh-my-zsh + p10k) can take seconds before its first byte:
        // wait for output first, then for it to go quiet.
        sh.wait_until(30, |t| !screen_text(t).trim().is_empty() || t.blocks.osc_seen());
        sh.wait_quiet(1500, 25);
        let t = &sh.pane.terminal;
        s.info(&id("startup"), format!("quiet after {:?}", sh.started.elapsed()));
        // The *user's own* rc may source iTerm2 shell integration, which emits
        // OSC 133 regardless of RIFT_NO_SHELL_INTEGRATION; only rc files without
        // it can be expected to stay silent.
        let user_rc_emits_osc133 = zdotdir.is_none()
            && label.starts_with("zsh")
            && std::env::var("HOME").ok().and_then(|h| std::fs::read_to_string(format!("{h}/.zshrc")).ok()).map_or(false, |rc| rc.contains("iterm2_shell_integration"));
        if user_rc_emits_osc133 {
            s.info(&id("no_osc133"), format!("skipped: ~/.zshrc sources iTerm2 integration, which emits OSC 133 itself (osc_seen={} marks={})", t.blocks.osc_seen(), t.marks.len()));
        } else {
            s.check(&id("no_osc133"), !t.blocks.osc_seen() && t.marks.is_empty(), format!("osc_seen={} marks={}", t.blocks.osc_seen(), t.marks.len()));
        }
        s.check(&id("prompt_drawn"), !screen_text(t).trim().is_empty(), format!("screen non-empty; screen=\n{}", screen_text(t)));
        // env fingerprint for comparison
        sh.send("echo \"FP:ZD=[${ZDOTDIR:+SET}]:ZSH=[$ZSH]:AL=$(alias | wc -l | tr -d ' '):RSI=[$RIFT_SHELL_INTEGRATION]:TP=[$TERM_PROGRAM]\"\r");
        sh.wait_quiet(1200, 10);
        let fp = screen_text(&sh.pane.terminal).lines().rev().find(|l| l.starts_with("FP:")).map(str::to_string);
        s.info(&id("fingerprint"), format!("{fp:?}"));
        sh.send("exit\r");
        sh.pump(500);
        sh.kill();
        let _ = std::fs::remove_dir_all(&dir);
        return (s, fp);
    }

    let ok = sh.prompt_ready(30);
    s.info(&id("time_to_first_prompt"), format!("{:?} ok={ok}", sh.started.elapsed()));
    s.check(&id("first_prompt"), ok, format!("OSC133 prompt reached; screen:\n{}", screen_text(&sh.pane.terminal)));
    if !ok {
        s.info(&id("raw_tail"), format!("{:?}", String::from_utf8_lossy(&sh.raw[sh.raw.len().saturating_sub(600)..])));
        sh.kill();
        return (s, None);
    }
    sh.pump(300);
    // The prompt itself should be visible (rc files loaded).
    s.check(&id("prompt_text_visible"), !screen_text(&sh.pane.terminal).trim().is_empty(), "prompt rendered");

    // 1) rc-file fingerprint
    sh.run("echo \"FP:ZD=[${ZDOTDIR:+SET}]:ZSH=[$ZSH]:AL=$(alias | wc -l | tr -d ' '):RSI=[$RIFT_SHELL_INTEGRATION]:TP=[$TERM_PROGRAM]\"", 20);
    let fp_line = sh.last_block_output().lines().find(|l| l.starts_with("FP:")).map(str::to_string);
    s.info(&id("fingerprint"), format!("{fp_line:?}"));

    // 2) ls
    let ok = sh.run("echo hi; echo there", 15);
    s.check(&id("simple_block"), ok, "block finished");
    {
        let t = &sh.pane.terminal;
        let b = t.blocks.blocks().last();
        s.check(&id("simple_cmd_text"), b.map_or(false, |b| b.command == "echo hi; echo there"), format!("command={:?}", b.map(|b| b.command.clone())));
        s.check(&id("simple_exit0"), b.map_or(false, |b| b.exit_code == Some(0)), format!("exit={:?}", b.map(|b| b.exit_code)));
        let out = sh.last_block_output();
        s.check(&id("simple_output"), out == "hi\nthere", format!("output={out:?}"));
    }
    // 3) false -> exit 1
    sh.run("false", 15);
    {
        let b = sh.pane.terminal.blocks.blocks().last().map(|b| (b.command.clone(), b.exit_code));
        s.check(&id("false_exit1"), b == Some(("false".into(), Some(1))), format!("{b:?}"));
    }
    // 4) exit code 127 + 42
    sh.run("nonexistent_cmd_xyz", 15);
    {
        let b = sh.pane.terminal.blocks.blocks().last().map(|b| (b.command.clone(), b.exit_code));
        s.check(&id("notfound_127"), b.as_ref().map_or(false, |b| b.1 == Some(127)), format!("{b:?}"));
    }
    sh.run("(exit 42)", 15);
    {
        let b = sh.pane.terminal.blocks.blocks().last().map(|b| (b.command.clone(), b.exit_code));
        s.check(&id("exit42"), b.as_ref().map_or(false, |b| b.1 == Some(42)), format!("{b:?}"));
    }
    // 5) cd /tmp -> cwd via OSC 7
    sh.run("cd /tmp", 15);
    {
        let cwd = sh.pane.terminal.cwd.clone();
        s.check(&id("cwd_tmp"), cwd.as_deref() == Some("/tmp"), format!("terminal.cwd={cwd:?}"));
    }
    sh.run("cd '/tmp/rift audit dir with space é'", 15); // nonexistent: cwd must not change
    sh.run("mkdir -p '/tmp/rift audit é 中' && cd '/tmp/rift audit é 中'", 15);
    {
        let cwd = sh.pane.terminal.cwd.clone();
        s.check(&id("cwd_unicode_space"), cwd.as_deref() == Some("/tmp/rift audit é 中"), format!("terminal.cwd={cwd:?}"));
    }
    sh.run("cd /tmp; rmdir '/tmp/rift audit é 中'", 15);
    // 6) multi-line command (backslash continuation) and for-loop
    let n0 = sh.pane.terminal.blocks.blocks().len();
    sh.send("echo one \\\r");
    sh.pump(400);
    sh.send("two\r");
    sh.wait_until(15, |t| t.blocks.blocks().len() > n0 && t.at_shell_prompt());
    {
        let t = &sh.pane.terminal;
        let b = t.blocks.blocks().last();
        s.info(&id("multiline_cmd"), format!("command={:?} exit={:?} output={:?}", b.map(|b| b.command.clone()), b.map(|b| b.exit_code), sh.last_block_output()));
        s.check(&id("multiline_cmd_text"), b.map_or(false, |b| b.command.contains("echo one") && b.command.contains("two") && !b.command.contains("> ")),
            format!("command={:?}", b.map(|b| b.command.clone())));
        s.check(&id("multiline_output"), sh.last_block_output() == "one two", format!("output={:?}", sh.last_block_output()));
    }
    let n0 = sh.pane.terminal.blocks.blocks().len();
    sh.send("for i in 1 2; do\r");
    sh.pump(300);
    sh.send("echo loop$i\r");
    sh.pump(300);
    sh.send("done\r");
    sh.wait_until(15, |t| t.blocks.blocks().len() > n0 && t.at_shell_prompt());
    {
        let t = &sh.pane.terminal;
        let b = t.blocks.blocks().last();
        s.info(&id("forloop_cmd"), format!("command={:?} output={:?}", b.map(|b| b.command.clone()), sh.last_block_output()));
        s.check(&id("forloop_output"), sh.last_block_output() == "loop1\nloop2", format!("output={:?}", sh.last_block_output()));
        s.check(&id("forloop_cmd_text"), b.map_or(false, |b| b.command.contains("for i in 1 2") && b.command.contains("done")), format!("command={:?}", b.map(|b| b.command.clone())));
    }
    // 7) long-running sleep 2 -> duration ~2000ms, running state visible
    let n0 = sh.pane.terminal.blocks.blocks().len();
    sh.send("sleep 2\r");
    let saw_running = sh.wait_until(5, |t| t.blocks.is_running());
    s.check(&id("running_flag"), saw_running, "blocks.is_running() while sleeping");
    sh.wait_until(15, |t| t.blocks.blocks().len() > n0 && t.at_shell_prompt());
    {
        let b = sh.pane.terminal.blocks.blocks().last().map(|b| (b.command.clone(), b.exit_code, b.duration_ms));
        s.check(&id("sleep2_duration"), b.as_ref().map_or(false, |b| b.1 == Some(0) && (1900..=3200).contains(&b.2)), format!("{b:?}"));
    }
    // 8) Ctrl+C a running sleep
    let n0 = sh.pane.terminal.blocks.blocks().len();
    sh.send("sleep 30\r");
    sh.wait_until(5, |t| t.blocks.is_running());
    sh.pump(400);
    sh.send("\x03");
    let back = sh.wait_until(10, |t| t.blocks.blocks().len() > n0 && t.at_shell_prompt());
    s.check(&id("ctrl_c_prompt_back"), back, "prompt returns after ^C");
    {
        let b = sh.pane.terminal.blocks.blocks().last().map(|b| (b.command.clone(), b.exit_code, b.duration_ms));
        s.check(&id("ctrl_c_exit130"), b.as_ref().map_or(false, |b| b.1 == Some(130) && b.2 < 5000), format!("{b:?}"));
    }
    // 9) ^C at an empty prompt must not create a bogus block
    let n0 = sh.pane.terminal.blocks.blocks().len();
    sh.send("\x03");
    sh.pump(600);
    s.check(&id("ctrl_c_empty_prompt_no_block"), sh.pane.terminal.blocks.blocks().len() == n0 && !sh.pane.terminal.blocks.is_running(),
        format!("blocks {}->{} running={}", n0, sh.pane.terminal.blocks.blocks().len(), sh.pane.terminal.blocks.is_running()));
    s.check(&id("still_at_prompt"), sh.pane.terminal.at_shell_prompt(), "at_shell_prompt after ^C on empty line");
    // 10) empty enter
    let n0 = sh.pane.terminal.blocks.blocks().len();
    sh.send("\r");
    sh.pump(600);
    s.check(&id("empty_enter_no_block"), sh.pane.terminal.blocks.blocks().len() == n0 && !sh.pane.terminal.blocks.is_running(), "empty Enter makes no block");
    // 11) big output + color
    sh.run("seq 1 3000", 20);
    {
        let b = sh.pane.terminal.blocks.blocks().last().map(|b| (b.command.clone(), b.exit_code, b.output_end.saturating_sub(b.output_start) + 1));
        s.check(&id("seq3000_lines"), b.as_ref().map_or(false, |b| b.1 == Some(0) && b.2 == 3000), format!("{b:?}"));
    }
    // 12) alt-screen program (less) -> no bogus prompt, returns cleanly
    sh.run("printf 'a\\nb\\n' | less", 5); // less exits immediately on short input with -F? not necessarily
    sh.pump(300);
    if sh.pane.terminal.is_alt_screen() {
        sh.send("q");
        sh.wait_until(10, |t| !t.is_alt_screen());
    }
    s.check(&id("after_less_prompt"), sh.wait_until(10, |t| t.at_shell_prompt()), "prompt back after pager");

    describe_blocks(&s, &sh);
    s.info(&id("marks_total"), format!("{}", sh.pane.terminal.marks.len()));
    let cwd_final = sh.pane.terminal.cwd.clone();
    s.info(&id("cwd_final"), format!("{cwd_final:?}"));
    sh.send("exit\r");
    sh.pump(500);
    s.check(&id("exit_closes_shell"), { let a = sh.alive(); !a }, "shell exited after `exit`");
    sh.kill();
    let _ = std::fs::remove_dir_all(&dir);
    (s, fp_line)
}

#[test]
fn zsh_real_rc_integrated() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (s, fp_i) = exercise("zsh", Some("/bin/zsh"), false);
    let (s2, fp_b) = exercise("zsh_baseline", Some("/bin/zsh"), true);
    println!("AUDIT|shell|zsh.fp_compare|INFO|integrated={fp_i:?} baseline={fp_b:?}");
    // alias count / ZDOTDIR / ZSH must match baseline apart from the RSI/TP markers
    let norm = |f: &Option<String>| f.as_ref().map(|f| f.split(":RSI=").next().unwrap().to_string());
    let mut s3 = Soft::new("shell");
    s3.check("zsh.rc_env_identical_to_baseline", norm(&fp_i) == norm(&fp_b) && norm(&fp_i).is_some(), format!("{:?} vs {:?}", norm(&fp_i), norm(&fp_b)));
    let mut all = Vec::new();
    all.extend(s.fails);
    all.extend(s2.fails);
    all.extend(s3.fails);
    assert!(all.is_empty(), "{} failed:\n{}", all.len(), all.join("\n"));
}

pub fn tmp_zdotdir(name: &str, zshrc: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("rift-audit-zd-{}-{}", name, std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(".zshrc"), zshrc).unwrap();
    d
}

#[test]
fn zsh_minimal_rc() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let zd = tmp_zdotdir("min", "PS1='%~ %# '\nalias ll='ls -l'\nexport MYVAR=hello\n");
    let (s, fp_i) = exercise_in("zsh_min", Some("/bin/zsh"), false, Some(&zd));
    let (s2, fp_b) = exercise_in("zsh_min_baseline", Some("/bin/zsh"), true, Some(&zd));
    println!("AUDIT|shell|zsh_min.fp_compare|INFO|integrated={fp_i:?} baseline={fp_b:?}");
    let norm = |f: &Option<String>| f.as_ref().map(|f| f.split(":RSI=").next().unwrap().to_string());
    let mut s3 = Soft::new("shell");
    s3.check("zsh_min.rc_env_identical_to_baseline", norm(&fp_i) == norm(&fp_b) && norm(&fp_i).is_some(), format!("{:?} vs {:?}", norm(&fp_i), norm(&fp_b)));
    let mut all = Vec::new();
    all.extend(s.fails); all.extend(s2.fails); all.extend(s3.fails);
    std::env::remove_var("ZDOTDIR");
    assert!(all.is_empty(), "{} failed:\n{}", all.len(), all.join("\n"));
}

#[test]
fn zsh_user_rc_without_iterm2_line() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = std::env::var("HOME").unwrap();
    // Needs the user's real ~/.zshrc; skip under a sandboxed HOME.
    let Ok(rc) = std::fs::read_to_string(format!("{home}/.zshrc")) else {
        println!("AUDIT|shell|zsh_noiterm|INFO|skipped: no ~/.zshrc under HOME={home}");
        return;
    };
    let filtered: String = rc.lines().filter(|l| !l.contains("iterm2_shell_integration")).collect::<Vec<_>>().join("\n");
    let zd = tmp_zdotdir("noiterm", &filtered);
    let (s, _) = exercise_in("zsh_noiterm", Some("/bin/zsh"), false, Some(&zd));
    std::env::remove_var("ZDOTDIR");
    s.finish();
}

#[test]
fn bash_integrated() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bash = ["/opt/homebrew/bin/bash", "/usr/local/bin/bash", "/bin/bash"].into_iter().find(|p| std::path::Path::new(p).exists()).unwrap();
    println!("AUDIT|shell|bash.path|INFO|{bash}");
    let (s, _) = exercise("bash", Some(bash), false);
    s.finish();
}

#[test]
fn bash_system_3x_integrated() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // macOS /bin/bash is 3.2: no PS0 -> DEBUG-trap fallback path.
    let (s, _) = exercise("bash32", Some("/bin/bash"), false);
    s.finish();
}

fn esc(b: &[u8]) -> String {
    let mut s = String::new();
    for &c in b {
        match c {
            0x1b => s.push_str("\\e"),
            0x07 => s.push_str("\\a"),
            b'\r' => s.push_str("\\r"),
            b'\n' => s.push_str("\\n"),
            c if c < 0x20 => s.push_str(&format!("\\x{c:02x}")),
            c => s.push(c as char),
        }
    }
    s
}

#[test]
fn zsh_real_rc_raw_marks_for_one_command() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut sh = Sh::spawn(Some("/bin/zsh"), false, "/tmp", 100, 30);
    assert!(sh.prompt_ready(30));
    sh.pump(500);
    let start = sh.raw.len();
    sh.run("echo hi", 15);
    sh.pump(300);
    let seg = &sh.raw[start..];
    // only the OSC 133 / OSC 7 sequences, in order
    let txt = esc(seg);
    let mut marks = Vec::new();
    let mut i = 0;
    while let Some(p) = txt[i..].find("\\e]") {
        let s = i + p;
        let e = txt[s..].find("\\a").map(|x| s + x).unwrap_or(txt.len());
        marks.push(txt[s..e].to_string());
        i = e;
    }
    println!("AUDIT|shell|zsh.raw_marks|INFO|{}", marks.join(" , "));
    println!("AUDIT|shell|zsh.raw_stream|INFO|{}", txt);
    sh.kill();
}

#[test]
fn bash32_raw_marks_probe() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut sh = Sh::spawn(Some("/bin/bash"), false, "/tmp", 100, 30);
    assert!(sh.prompt_ready(30));
    sh.pump(300);
    for cmd in ["(exit 42)", "mkdir -p '/tmp/rift é' && cd '/tmp/rift é'"] {
        let start = sh.raw.len();
        sh.send(cmd);
        sh.send("\r");
        sh.pump(1500);
        println!("AUDIT|shell|bash32.probe|INFO|cmd={cmd:?} raw={}", esc(&sh.raw[start..]));
        println!("AUDIT|shell|bash32.probe_state|INFO|blocks={} running={} cwd={:?}", sh.pane.terminal.blocks.blocks().len(), sh.pane.terminal.blocks.is_running(), sh.pane.terminal.cwd);
    }
    sh.kill();
    let _ = std::fs::remove_dir_all("/tmp/rift é");
}
