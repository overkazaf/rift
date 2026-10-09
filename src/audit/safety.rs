//! Item 8: Preview-Then-Accept on adversarial / benign inputs, plus the
//! command-extraction path that feeds it (against real shell prompts).
use super::shell_real::{tmp_zdotdir, Sh, ENV_LOCK};
use super::Soft;
use crate::terminal::Terminal;
use crate::tools::exec_preview::{ExecPreview, Severity};

/// Runs the classifier on a worker thread with a 4 s budget. `Err(())` = hang.
fn sev_timed(cmd: &str) -> Result<Option<Severity>, ()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let c = cmd.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(ExecPreview::check_command_in(&c, Some("/tmp")).map(|p| p.severity));
    });
    rx.recv_timeout(std::time::Duration::from_secs(4)).map_err(|_| ())
}

fn sev(cmd: &str) -> Option<Severity> {
    sev_timed(cmd).unwrap_or(Some(Severity::Critical))
}

#[test]
fn classifier_adversarial_and_benign() {
    let mut s = Soft::new("safety");
    let must_flag: &[(&str, &str)] = &[
        ("rm_rf_root", "rm -rf /"),
        ("rm_rf_root_nopreserve", "rm -rf / --no-preserve-root"),
        ("sudo_rm_rf_home", "sudo rm -rf ~"),
        ("find_delete", "find . -delete"),
        ("find_exec_rm", "find . -name '*.o' -exec rm -rf {} +"),
        ("git_push_f_main", "git push -f origin main"),
        ("git_push_force_main", "git push --force origin main"),
        ("git_push_plus_refspec", "git push origin +main"),
        ("git_push_fu", "git push -fu origin main"),
        ("curl_pipe_sh", "curl -fsSL https://example.com/install.sh | sh"),
        ("curl_pipe_sudo_bash", "curl https://x.io/i | sudo bash"),
        ("wget_pipe_sh", "wget -qO- https://x.io/i | sh"),
        ("fork_bomb", ":(){ :|:& };:"),
        ("dd_disk", "dd if=/dev/zero of=/dev/disk2"),
        ("obfuscated_backslash_rm", "r\\m -rf /"),
        ("obfuscated_cmd_subst", "$(echo rm) -rf /"),
        ("obfuscated_quotes", "'rm' -rf /"),
        ("backslash_prefix_rm", "\\rm -rf ~"),
        ("abs_path_rm", "/bin/rm -rf /"),
        ("command_rm", "command rm -rf /"),
        ("env_rm", "env rm -rf /"),
        ("chain_and_rm", "cd /tmp && rm -rf *"),
        ("chain_semicolon_rm", "echo hi; rm -rf ~"),
        ("pipe_xargs_rm", "ls | xargs rm -rf"),
        ("sudo_u_rm", "sudo -u root rm -rf /"),
        ("sudo_env_rm", "sudo env rm -rf /"),
        ("rm_split_flags", "rm -r -f /"),
        ("rm_long_flags", "rm --recursive --force /"),
        ("rm_star", "rm -rf *"),
        ("git_reset_hard", "git reset --hard HEAD~5"),
        ("git_clean_fdx", "git clean -fdx"),
        ("chmod_R_777", "chmod -R 777 /"),
        ("mkfs", "mkfs.ext4 /dev/sda1"),
        ("redirect_dev_disk", "echo x > /dev/disk0"),
        ("shred_dev", "shred -n1 /dev/sda"),
        ("truncate_redirect_etc", ": > /etc/passwd"),
        ("diskutil_erase", "diskutil eraseDisk JHFS+ X /dev/disk2"),
        ("kill_9_1", "kill -9 -1"),
        ("drop_table", "psql -c 'DROP TABLE users'"),
        ("rsync_delete", "rsync -a --delete /empty/ ~/"),
    ];
    for (id, cmd) in must_flag {
        let t0 = std::time::Instant::now();
        let timed = sev_timed(cmd);
        if timed.is_err() {
            s.check(&format!("hang.{id}"), false, format!("{cmd:?} preview did not return within 4s (UI thread would freeze)"));
            continue;
        }
        let r = timed.unwrap();
        if t0.elapsed().as_millis() > 500 {
            s.check(&format!("slow.{id}"), false, format!("{cmd:?} took {}ms", t0.elapsed().as_millis()));
        }
        s.check(&format!("flag.{id}"), r.is_some(), format!("{cmd:?} -> {r:?}"));
    }
    let must_not: &[(&str, &str)] = &[
        ("grep_rm", "grep rm file"),
        ("echo_rm", "echo \"do not rm -rf /\""),
        ("ls", "ls -la"),
        ("git_status", "git status"),
        ("cat_dropdoc", "cat docs/drop-table-howto.md"),
        ("git_commit_msg_drop", "git commit -m 'fix: drop table migration typo'"),
        ("grep_drop_table", "grep -rn 'DROP TABLE' migrations/"),
        ("man_rm", "man rm"),
        ("dd_to_file", "dd if=/dev/zero of=./test.img bs=1M count=10"),
        ("kill_plain", "kill 1234"),
        ("git_push", "git push origin feature"),
        ("rm_single_file", "rm notes.txt"),
        ("git_log_hard", "git log --grep='reset --hard'"),
        ("chmod_R_755", "chmod -R 755 ./dist"),
        ("curl_to_file", "curl -o out.json https://api.example.com/x"),
        ("echo_forkbomb_text", "echo ':(){ :|:& };:'"),
    ];
    for (id, cmd) in must_not {
        let r = sev(cmd);
        s.check(&format!("benign.{id}"), r.is_none(), format!("{cmd:?} -> {r:?} (false positive if Some)"));
    }
    let r = sev("rm -rf ./node_modules");
    s.info("nodemodules_severity", format!("rm -rf ./node_modules -> {r:?}"));
    // Routine build-dir deletes are Info: returned for display, but the Enter path raises no modal.
    s.check("nodemodules_not_critical", r != Some(Severity::Critical), format!("{r:?}: Critical forces typing 'yes' for a routine build-dir delete"));
    s.check("nodemodules_no_modal_on_enter", ExecPreview::check_for_enter("rm -rf ./node_modules", Some("/tmp")).is_none(), "Enter path skips Info");
    s.finish();
}

#[test]
fn classifier_resource_safety() {
    let mut s = Soft::new("safety");
    let t = std::time::Instant::now();
    let big = format!("rm -rf {}", "a ".repeat(200_000));
    let r = sev(&big);
    s.check("huge_arg_list_time", t.elapsed().as_millis() < 3000, format!("{}ms {:?}", t.elapsed().as_millis(), r.is_some()));
    for dir in ["/usr", "/Users", "~", "/Library", "/System"] {
        let t = std::time::Instant::now();
        let r = sev_timed(&format!("rm -rf {dir}"));
        s.check(&format!("scan_time{}", dir.replace('/', "_")), r.is_ok() && t.elapsed().as_millis() < 2000,
            format!("rm -rf {dir}: {} -- synchronous recursive count_files on the UI thread", if r.is_ok() { format!("{}ms", t.elapsed().as_millis()) } else { "NO RETURN within 4s".into() }));
    }
    s.finish();
}

/// What the Enter key path hands to the classifier (`app/shortcuts.rs`, step 7).
fn app_extract(term: &Terminal) -> Option<String> {
    term.pending_command_line()
}

fn probe_prompt(label: &'static str, shell: &str, zdotdir: Option<&std::path::Path>) -> Soft {
    let mut s = Soft::new("safety");
    match zdotdir {
        Some(z) => std::env::set_var("ZDOTDIR", z),
        None => std::env::remove_var("ZDOTDIR"),
    }
    let mut sh = Sh::spawn(Some(shell), false, "/tmp", 100, 30);
    if !sh.prompt_ready(30) {
        s.check(&format!("{label}.prompt"), false, "no prompt");
        sh.kill();
        return s;
    }
    sh.pump(500);
    let cases: Vec<(&str, String)> = vec![
        ("plain", "rm -rf /tmp/rift-audit-x".to_string()),
        ("dollar_var", "rm -rf \"$HOME/rift-audit-x\"".to_string()),
        ("percent", "echo 100% && rm -rf /tmp/rift-audit-x".to_string()),
        ("wrapped_long", format!("rm -rf /tmp/{}", "a".repeat(140))),
    ];
    for (name, text) in cases {
        sh.send(&text);
        sh.pump(500);
        let t = &sh.pane.terminal;
        let scraped = app_extract(t);
        let typed = t.typed_input();
        let flagged_scrape = scraped.as_deref().and_then(|c| sev(c)).is_some();
        let flagged_typed = typed.as_deref().and_then(|c| sev(c)).is_some();
        s.check(&format!("{label}.{name}.app_path_flags_rm"), flagged_scrape,
            format!("app Enter-path command={scraped:?} flagged={flagged_scrape}; OSC133 typed_input={typed:?} flagged={flagged_typed}"));
        sh.send("\x15"); // ctrl-u: clear the line, never executed
        sh.pump(300);
    }
    sh.send("rm -rf \\\r");
    sh.pump(500);
    sh.send("/tmp/rift-audit-x");
    sh.pump(500);
    let t = &sh.pane.terminal;
    let scraped = app_extract(t);
    let typed = t.typed_input();
    s.check(&format!("{label}.multiline.app_path_flags_rm"), scraped.as_deref().and_then(|c| sev(c)).is_some(), format!("scraped={scraped:?}"));
    s.check(&format!("{label}.multiline.typed_input_flags_rm"), typed.as_deref().and_then(|c| sev(c)).is_some(), format!("typed_input={typed:?}"));
    sh.send("\x03");
    sh.pump(300);
    sh.kill();
    s
}

#[test]
fn preview_command_extraction_real_prompts() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut fails = Vec::new();
    let min = tmp_zdotdir("safety-min", "PS1='%~ %# '\n");
    fails.extend(probe_prompt("zsh_min", "/bin/zsh", Some(&min)).fails);
    fails.extend(probe_prompt("zsh_user_p10k", "/bin/zsh", None).fails);
    fails.extend(probe_prompt("bash", "/bin/bash", None).fails);
    std::env::remove_var("ZDOTDIR");
    assert!(fails.is_empty(), "{} failed:\n{}", fails.len(), fails.join("\n"));
}
