//! Preview-Then-Accept — an interceptor for dangerous shell commands.
//!
//! When the user presses Enter on a command that looks destructive, the
//! keystroke is intercepted *before* it reaches the PTY (see
//! `app/shortcuts.rs`, step 7). Instead of executing immediately, a modal
//! preview is shown describing exactly what the command would do; the user
//! must explicitly confirm (or, for `Severity::Critical` commands, type
//! "yes") before the already-buffered command line is actually submitted.
//!
//! The command's characters have already been streamed to the PTY as the
//! user typed them (this is a real terminal, not a local-echo simulation),
//! so they're already sitting in the shell's line-editing buffer, uncommitted.
//! Confirming just sends the trailing `\r` to submit that buffer; canceling
//! sends nothing, leaving the line exactly as the user left it so they can
//! edit or clear it themselves.

use std::path::Path;
use std::process::Command;

/// A single consequence of running the previewed command, shown as a
/// bulleted line in the modal (`description`), optionally followed by a
/// dimmer detail line (`detail`) — e.g. a concrete path, file count, or
/// mitigation tip.
#[derive(Debug, Clone)]
pub struct Impact {
    pub description: String,
    pub detail: String,
}

impl Impact {
    fn new(description: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { description: description.into(), detail: detail.into() }
    }
    fn plain(description: impl Into<String>) -> Self {
        Self::new(description, String::new())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

/// State for the "Preview-Then-Accept" confirmation modal.
#[derive(Debug, Clone)]
pub struct ExecPreview {
    pub visible: bool,
    pub command: String,
    pub impacts: Vec<Impact>,
    pub severity: Severity,
    /// For `Severity::Critical`, the user must type "yes" before Enter is
    /// accepted. This buffers what they've typed so far.
    pub confirm_input: String,
}

pub enum ExecPreviewKey {
    Enter,
    Escape,
    Backspace,
    Char(char),
}

pub enum ExecPreviewAction {
    /// User confirmed — caller should submit the pending command (send `\r`).
    Execute,
    /// User backed out — caller should leave the PTY untouched.
    Cancel,
}

impl ExecPreview {
    pub fn hidden() -> Self {
        Self {
            visible: false,
            command: String::new(),
            impacts: Vec::new(),
            severity: Severity::Info,
            confirm_input: String::new(),
        }
    }

    /// Does `cmd` look dangerous enough to intercept? Returns a populated
    /// (but not-yet-visible) preview if so. Callers are expected to set
    /// `.visible = true` themselves before showing it.
    pub fn check_command(cmd: &str) -> Option<ExecPreview> {
        Self::check_command_in(cmd, None)
    }

    /// Like `check_command`, but live introspection (git, relative `rm`
    /// targets) runs in `cwd` — the shell's real directory as reported by
    /// OSC 7 — instead of Rift's own process directory.
    pub fn check_command_in(cmd: &str, cwd: Option<&str>) -> Option<ExecPreview> {
        PREVIEW_CWD.with(|c| *c.borrow_mut() = cwd.map(std::path::PathBuf::from));
        let result = Self::check_command_inner(cmd);
        PREVIEW_CWD.with(|c| *c.borrow_mut() = None);
        result
    }

    fn check_command_inner(cmd: &str) -> Option<ExecPreview> {
        let trimmed = cmd.trim();
        if trimmed.is_empty() {
            return None;
        }

        let (is_sudo, effective) = strip_sudo(trimmed);
        let mut matched = classify(effective, is_sudo);

        // `sudo rm ...` / `sudo dd ...` are always Critical, even when the
        // bare command wouldn't otherwise trip a rule on its own (e.g.
        // `rm one_file.txt` with no -r) — running as root removes the
        // filesystem's own permission safety net.
        if is_sudo {
            let first_word = effective.split_whitespace().next().unwrap_or("");
            if matched.is_none() && (first_word == "rm" || first_word == "dd") {
                matched = Some((
                    Severity::Warning,
                    vec![Impact::plain("Runs as root — bypasses normal permission checks")],
                ));
            }
            if let Some((sev, impacts)) = matched.as_mut() {
                *sev = Severity::Critical;
                impacts.insert(
                    0,
                    Impact::new(
                        "Running with sudo (root)",
                        "No permission check will stop this command",
                    ),
                );
            }
        }

        let (severity, impacts) = matched?;
        Some(ExecPreview {
            visible: false,
            command: trimmed.to_string(),
            impacts,
            severity,
            confirm_input: String::new(),
        })
    }

    /// Does this severity require the user to type "yes" rather than a bare
    /// Enter/Y keypress?
    fn needs_typed_confirm(&self) -> bool {
        matches!(self.severity, Severity::Critical)
    }

    pub fn handle_key(&mut self, key: ExecPreviewKey) -> Option<ExecPreviewAction> {
        if !self.visible {
            return None;
        }
        let typed_confirm = self.needs_typed_confirm();
        match key {
            ExecPreviewKey::Escape => {
                *self = ExecPreview::hidden();
                Some(ExecPreviewAction::Cancel)
            }
            ExecPreviewKey::Enter => {
                if typed_confirm {
                    if self.confirm_input.trim().eq_ignore_ascii_case("yes") {
                        *self = ExecPreview::hidden();
                        Some(ExecPreviewAction::Execute)
                    } else {
                        None // Not confirmed yet — stay open.
                    }
                } else {
                    *self = ExecPreview::hidden();
                    Some(ExecPreviewAction::Execute)
                }
            }
            ExecPreviewKey::Backspace => {
                self.confirm_input.pop();
                None
            }
            ExecPreviewKey::Char(c) => {
                if typed_confirm {
                    if c.is_ascii_alphabetic() && self.confirm_input.chars().count() < 10 {
                        self.confirm_input.push(c.to_ascii_lowercase());
                    }
                    None
                } else {
                    match c {
                        'y' | 'Y' => {
                            *self = ExecPreview::hidden();
                            Some(ExecPreviewAction::Execute)
                        }
                        'n' | 'N' => {
                            *self = ExecPreview::hidden();
                            Some(ExecPreviewAction::Cancel)
                        }
                        _ => None,
                    }
                }
            }
        }
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut crate::renderer::font::FontManager,
        theme: &crate::config::Theme,
    ) {
        use crate::ui::kit::{Ctx, PanelSpec, Rect, Tokens, Tone};
        if !self.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        // Blocking, modal decision: dim everything behind it.
        cx.backdrop(0.7);

        let (tone, label) = match self.severity {
            Severity::Critical => (Tone::Danger, "CRITICAL"),
            Severity::Warning => (Tone::Warning, "WARNING"),
            Severity::Info => (Tone::Accent, "NOTICE"),
        };
        let typed_confirm = self.needs_typed_confirm();
        let accent = tk.tone(tone);

        let impact_lines: usize = self
            .impacts
            .iter()
            .map(|i| 1 + if i.detail.is_empty() { 0 } else { 1 })
            .sum::<usize>()
            .max(1);
        let typed_h = if typed_confirm { tk.row_h + tk.input_h + tk.sp.sm } else { 0 };
        let want_h = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + tk.input_h + tk.sp.md + impact_lines * tk.row_h + typed_h;
        let rect = cx.centered_cols(72, want_h);

        let hints: &[(&str, &str)] = if typed_confirm {
            &[("Enter", "execute (after typing yes)"), ("Esc", "cancel")]
        } else {
            &[("Enter/Y", "execute"), ("Esc/N", "cancel")]
        };
        let spec = PanelSpec::new("Confirm command")
            .sub("review before running")
            .badge(label, tone)
            .edge(tone)
            .hints(hints);
        let body = cx.panel(rect, &spec);

        // Command line, boxed
        let boxr = Rect::new(body.x, body.y, body.w, tk.input_h);
        cx.well(boxr);
        let ty = cx.text_y(boxr.y, boxr.h);
        cx.text(boxr.x + tk.sp.md, ty, "$", tk.text_muted);
        let cmd_x = boxr.x + tk.sp.md + 2 * tk.cw;
        cx.text_fit(cmd_x, ty, boxr.right().saturating_sub(cmd_x + tk.sp.md), &self.command, tk.text);

        // Impacts
        let mut y = body.y + tk.input_h + tk.sp.md;
        let impacts_bottom = body.bottom().saturating_sub(typed_h);
        let marker = if matches!(self.severity, Severity::Critical) { "!!" } else { "!" };
        for impact in &self.impacts {
            let need = if impact.detail.is_empty() { tk.row_h } else { 2 * tk.row_h };
            if y + need > impacts_bottom {
                break;
            }
            cx.line(body.x + tk.sp.xs, y, marker, accent);
            let dx = body.x + tk.sp.xs + 3 * tk.cw;
            cx.line_fit(dx, y, body.right().saturating_sub(dx), &impact.description, tk.text);
            y += tk.row_h;
            if !impact.detail.is_empty() {
                cx.line_fit(dx, y, body.right().saturating_sub(dx), &impact.detail, tk.text_muted);
                y += tk.row_h;
            }
        }

        // Extra safety for Critical: require the literal word "yes".
        if typed_confirm {
            let y = body.bottom().saturating_sub(typed_h) + tk.sp.xs;
            cx.line(body.x, y, "Type \"yes\" to confirm", accent);
            let inp = Rect::new(body.x, y + tk.row_h, body.w, tk.input_h);
            cx.text_input(inp, &self.confirm_input, self.confirm_input.chars().count(), None, "yes", true);
        }
    }
}

// ── Sudo handling ──

/// If `cmd` starts with a whole `sudo` word, return `(true, rest-after-sudo)`;
/// otherwise `(false, cmd)`. Does not attempt to skip sudo's own flags
/// (e.g. `sudo -u root rm ...`) — only the bare `sudo <command>` form is
/// recognized.
fn strip_sudo(cmd: &str) -> (bool, &str) {
    if let Some(rest) = cmd.strip_prefix("sudo") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return (true, rest.trim_start());
        }
    }
    (false, cmd)
}

// ── Pattern classification ──

/// Try every rule against the (sudo-stripped) command text. First match
/// wins; returns `None` for anything that doesn't look dangerous.
fn classify(effective: &str, is_sudo: bool) -> Option<(Severity, Vec<Impact>)> {
    if is_fork_bomb(effective) {
        return Some((
            Severity::Critical,
            vec![Impact::new(
                "Fork bomb — recursively spawns processes with no limit",
                "Exhausts the process table / memory; usually needs a hard reboot",
            )],
        ));
    }
    if let Some(target) = disk_wipe_target(effective) {
        return Some((
            Severity::Critical,
            vec![Impact::new(
                format!("Overwrites raw device: {target}"),
                "Destroys the partition table and all data on that disk",
            )],
        ));
    }
    if let Some(r) = check_sql_drop(effective) {
        return Some(r);
    }

    let parts: Vec<&str> = effective.split_whitespace().collect();
    let first = *parts.first()?;

    match first {
        "rm" => check_rm(&parts, is_sudo),
        "git" => check_git_reset_hard(&parts)
            .or_else(|| check_git_push_force(&parts))
            .or_else(|| check_git_clean_fd(&parts)),
        "chmod" => check_chmod_777(&parts),
        "dd" => check_dd(&parts),
        "kill" | "killall" => check_kill(&parts),
        "docker" => check_docker_prune(&parts),
        _ if first == "mkfs" || first.starts_with("mkfs.") => check_mkfs(&parts),
        _ => None,
    }
}

fn has_flag(parts: &[&str], short: char, long: &str) -> bool {
    parts.iter().any(|p| {
        if let Some(rest) = p.strip_prefix("--") {
            rest == long
        } else if let Some(rest) = p.strip_prefix('-') {
            !rest.is_empty() && !rest.starts_with('-') && rest.contains(short)
        } else {
            false
        }
    })
}

fn is_fork_bomb(cmd: &str) -> bool {
    let norm: String = cmd.chars().filter(|c| !c.is_whitespace()).collect();
    norm.contains(":(){:|:&};:")
}

/// Detects a bare (non-appending) redirect into a raw block device, e.g.
/// `cat foo > /dev/sda` or `> /dev/disk2`. Returns the device path if found.
fn disk_wipe_target(cmd: &str) -> Option<String> {
    let bytes = cmd.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] != b'>' {
            continue;
        }
        if i > 0 && bytes[i - 1] == b'>' {
            continue; // second `>` of `>>`
        }
        if bytes.get(i + 1) == Some(&b'>') {
            continue; // first `>` of `>>`
        }
        let rest = cmd[i + 1..].trim_start();
        const DEV_PREFIXES: &[&str] = &["/dev/sd", "/dev/disk", "/dev/rdisk", "/dev/hd", "/dev/nvme"];
        for prefix in DEV_PREFIXES {
            if rest.starts_with(prefix) {
                let target = rest.split_whitespace().next().unwrap_or(rest);
                return Some(target.to_string());
            }
        }
    }
    None
}

fn check_sql_drop(cmd: &str) -> Option<(Severity, Vec<Impact>)> {
    let upper = cmd.to_ascii_uppercase();
    if upper.contains("DROP TABLE") {
        Some((
            Severity::Critical,
            vec![Impact::new(
                "Permanently deletes a database table",
                "All rows and the schema are destroyed — no undo without a backup",
            )],
        ))
    } else if upper.contains("DROP DATABASE") {
        Some((
            Severity::Critical,
            vec![Impact::new(
                "Permanently deletes an entire database",
                "Every table, row, and index in it is destroyed — no undo without a backup",
            )],
        ))
    } else {
        None
    }
}

fn check_rm(parts: &[&str], is_sudo: bool) -> Option<(Severity, Vec<Impact>)> {
    let has_r = has_flag(parts, 'r', "recursive");
    let has_f = has_flag(parts, 'f', "force");
    if !has_r && !is_sudo {
        return None; // plain `rm file` is routine — don't interrupt the user
    }

    let targets: Vec<&str> = parts.iter().skip(1).filter(|p| !p.starts_with('-')).copied().collect();

    let headline = if has_r && has_f {
        "Force-deletes files/directories recursively — no prompts, no trash"
    } else if has_r {
        "Deletes files/directories recursively — permanent, no trash"
    } else {
        "Deletes file(s) as root — bypasses normal permission checks"
    };
    let mut impacts = vec![Impact::plain(headline)];

    if targets.is_empty() {
        impacts.push(Impact::new(
            "No target path given",
            "Command as typed would likely error — double-check before confirming",
        ));
    } else {
        for target in &targets {
            let resolved = match PREVIEW_CWD.with(|d| d.borrow().clone()) {
                Some(base) if !Path::new(target).is_absolute() && !target.starts_with('~') => base.join(target),
                _ => std::path::PathBuf::from(target),
            };
            let path = resolved.as_path();
            let detail = if path.is_dir() {
                match count_files_recursive(path) {
                    Ok(n) => format!("directory — {n} file(s)/dir(s) inside"),
                    Err(_) => "directory — contents unknown (permission denied?)".to_string(),
                }
            } else if let Ok(meta) = std::fs::symlink_metadata(path) {
                format!("file — {}", format_size(meta.len()))
            } else {
                "not found from here — path may be relative to a different pane".to_string()
            };
            impacts.push(Impact::new((*target).to_string(), detail));
        }
    }
    Some((Severity::Critical, cap_impacts(impacts, 8)))
}

fn check_git_reset_hard(parts: &[&str]) -> Option<(Severity, Vec<Impact>)> {
    if parts.first() != Some(&"git") || parts.get(1) != Some(&"reset") {
        return None;
    }
    if !parts.iter().any(|p| *p == "--hard") {
        return None;
    }

    let mut impacts = vec![Impact::new(
        "Discards ALL uncommitted changes permanently",
        "Working directory and index are reset to match the target commit",
    )];
    let changed = git_status_porcelain();
    if changed.is_empty() {
        impacts.push(Impact::new(
            "Working tree looks clean",
            "(or this isn't a git repo, or git isn't on PATH)",
        ));
    } else {
        for line in changed.iter().take(6) {
            impacts.push(Impact::plain(line.trim().to_string()));
        }
        if changed.len() > 6 {
            impacts.push(Impact::plain(format!("...and {} more changed file(s)", changed.len() - 6)));
        }
    }
    Some((Severity::Critical, impacts))
}

fn check_git_push_force(parts: &[&str]) -> Option<(Severity, Vec<Impact>)> {
    if parts.first() != Some(&"git") || parts.get(1) != Some(&"push") {
        return None;
    }
    let lease = parts.iter().any(|p| *p == "--force-with-lease");
    let force = parts.iter().any(|p| *p == "--force" || *p == "-f");
    if !force && !lease {
        return None;
    }

    let mut impacts = vec![Impact::new(
        "Overwrites remote branch history",
        if lease {
            "Safer variant (--force-with-lease) — still rewrites history, aborts if remote moved"
        } else {
            "Anyone who already pulled the old history can lose work merging back"
        },
    )];
    if let Some((ahead, behind)) = git_push_divergence() {
        if behind > 0 {
            impacts.push(Impact::new(
                format!("Remote has {behind} commit(s) not in your local branch"),
                "Those commits would be discarded from the remote branch",
            ));
        }
        if ahead > 0 {
            impacts.push(Impact::plain(format!("Local branch is ahead by {ahead} commit(s)")));
        }
    }
    Some((Severity::Critical, impacts))
}

fn check_git_clean_fd(parts: &[&str]) -> Option<(Severity, Vec<Impact>)> {
    if parts.first() != Some(&"git") || parts.get(1) != Some(&"clean") {
        return None;
    }
    let has_f = has_flag(parts, 'f', "force");
    let has_d = has_flag(parts, 'd', "directories");
    if !(has_f && has_d) {
        return None;
    }

    let files = git_clean_dry_run();
    let mut impacts = vec![Impact::new(
        "Permanently removes untracked files and directories",
        "Not recoverable from git — these files were never committed",
    )];
    if files.is_empty() {
        impacts.push(Impact::new("No untracked files found", "(or this isn't a git repo)"));
    } else {
        for f in files.iter().take(8) {
            impacts.push(Impact::plain(f.clone()));
        }
        if files.len() > 8 {
            impacts.push(Impact::plain(format!("...and {} more", files.len() - 8)));
        }
    }
    Some((Severity::Warning, impacts))
}

fn check_chmod_777(parts: &[&str]) -> Option<(Severity, Vec<Impact>)> {
    if parts.first() != Some(&"chmod") {
        return None;
    }
    let recursive = parts.iter().any(|p| *p == "-R" || *p == "--recursive");
    let mode_777 = parts.iter().any(|p| p.trim_start_matches('0') == "777");
    if !(recursive && mode_777) {
        return None;
    }
    Some((
        Severity::Warning,
        vec![Impact::new(
            "Grants read/write/execute to EVERYONE, recursively",
            "Common privilege-escalation vector — rarely what you actually want",
        )],
    ))
}

fn check_dd(parts: &[&str]) -> Option<(Severity, Vec<Impact>)> {
    if parts.first() != Some(&"dd") {
        return None;
    }
    let find_arg = |prefix: &str| parts.iter().find_map(|p| p.strip_prefix(prefix).map(|s| s.to_string()));
    let input = find_arg("if=");
    let output = find_arg("of=");

    let mut impacts = vec![Impact::new(
        "Low-level block-device copy — bypasses the filesystem, no undo",
        "A typo in 'of=' can silently overwrite an entire disk",
    )];
    if let Some(of) = output {
        impacts.push(Impact::new(format!("Destination: {of}"), "Every existing byte there will be overwritten"));
    }
    if let Some(if_) = input {
        impacts.push(Impact::plain(format!("Source: {if_}")));
    }
    Some((Severity::Critical, impacts))
}

fn check_mkfs(parts: &[&str]) -> Option<(Severity, Vec<Impact>)> {
    let target = parts.iter().skip(1).find(|p| !p.starts_with('-')).copied();
    let mut impacts = vec![Impact::plain("Formats a filesystem — ALL DATA on the target is destroyed")];
    if let Some(t) = target {
        impacts.push(Impact::new(
            format!("Target: {t}"),
            "Double-check this is the right device, not your main disk",
        ));
    }
    Some((Severity::Critical, impacts))
}

fn check_kill(parts: &[&str]) -> Option<(Severity, Vec<Impact>)> {
    match parts.first().copied() {
        Some("kill") => {
            let sig9 = parts
                .iter()
                .any(|p| *p == "-9" || p.eq_ignore_ascii_case("-sigkill") || p.eq_ignore_ascii_case("-kill"));
            if !sig9 {
                return None;
            }
            let pid = parts.iter().skip(1).find(|p| !p.starts_with('-')).copied().unwrap_or("?");
            Some((
                Severity::Warning,
                vec![Impact::new(
                    "Force-kills a process (SIGKILL)",
                    format!("PID {pid}: no cleanup handlers run, unsaved state is lost"),
                )],
            ))
        }
        Some("killall") => {
            let name = parts.get(1).copied().unwrap_or("?");
            Some((
                Severity::Warning,
                vec![Impact::new(
                    "Force-kills EVERY process matching this name",
                    format!("Target: {name} — may hit more processes than intended"),
                )],
            ))
        }
        _ => None,
    }
}

fn check_docker_prune(parts: &[&str]) -> Option<(Severity, Vec<Impact>)> {
    if parts.first() != Some(&"docker") || parts.get(1) != Some(&"system") || parts.get(2) != Some(&"prune") {
        return None;
    }
    let all = parts.iter().any(|p| *p == "-a" || *p == "--all");
    Some((
        Severity::Warning,
        vec![Impact::new(
            "Removes all unused Docker data",
            if all {
                "Stopped containers, unused networks, dangling AND unused images, build cache"
            } else {
                "Stopped containers, unused networks, dangling images, build cache"
            },
        )],
    ))
}

fn cap_impacts(mut impacts: Vec<Impact>, max: usize) -> Vec<Impact> {
    if impacts.len() > max {
        let remaining = impacts.len() - max;
        impacts.truncate(max);
        impacts.push(Impact::plain(format!("...and {remaining} more")));
    }
    impacts
}

// ── Live introspection (best-effort; failures just fall back to generic text) ──

thread_local! {
    /// Working directory for the command currently being previewed.
    static PREVIEW_CWD: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// `git` command rooted at the previewed shell's cwd when known.
fn git_cmd() -> Command {
    let mut c = Command::new("git");
    if let Some(dir) = PREVIEW_CWD.with(|d| d.borrow().clone()) {
        c.current_dir(dir);
    }
    c
}

fn git_status_porcelain() -> Vec<String> {
    git_cmd()
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(|l| l.to_string()).collect())
        .unwrap_or_default()
}

fn git_clean_dry_run() -> Vec<String> {
    git_cmd()
        .args(["clean", "-fdn"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.strip_prefix("Would remove ").map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// `(ahead, behind)` of HEAD relative to its upstream, if one is configured.
fn git_push_divergence() -> Option<(usize, usize)> {
    let out = git_cmd()
        .args(["rev-list", "--left-right", "--count", "@{upstream}...HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let mut it = s.split_whitespace();
    let behind = it.next()?.parse().ok()?;
    let ahead = it.next()?.parse().ok()?;
    Some((ahead, behind))
}

fn count_files_recursive(path: &Path) -> std::io::Result<usize> {
    let mut count = 0;
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            count += 1;
            if entry.file_type()?.is_dir() {
                count += count_files_recursive(&entry.path()).unwrap_or(0);
            }
        }
    }
    Ok(count)
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.0}K", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}M", bytes as f64 / (1024.0 * 1024.0))
    }
}
