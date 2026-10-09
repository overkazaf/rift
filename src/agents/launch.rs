//! "New Agent": find installed CLIs, plan and create git worktrees, lay out
//! agent grids. Pure planning functions are separated from the I/O so they can
//! be tested (worktree command construction, grid shapes, PATH lookup).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::Instant;

use super::AgentKind;

// ───────────────────────────── installed CLIs ─────────────────────────────

/// Directories GUI-launched apps often miss on `PATH`.
fn extra_dirs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/opt/local/bin"].iter().map(PathBuf::from).collect();
    if let Some(h) = dirs::home_dir() {
        for d in [".local/bin", ".claude/local", ".bun/bin", ".npm-global/bin", ".cargo/bin", ".volta/bin", ".opencode/bin", ".cursor/bin", "go/bin"] {
            v.push(h.join(d));
        }
    }
    v
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}
#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// First executable called `bin` in `dirs`.
pub fn find_in_dirs(bin: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(bin)).find(|p| is_executable(p))
}

/// Search `$PATH`, then the usual install locations.
pub fn find_binary(bin: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(extra_dirs());
    find_in_dirs(bin, &dirs)
}

/// Agent CLIs found on this machine, in `AgentKind::ALL` order.
pub fn installed_agents() -> Vec<AgentKind> {
    AgentKind::ALL.into_iter().filter(|k| find_binary(k.binary()).is_some()).collect()
}

/// The command typed into the new shell.
pub fn launch_command(kind: AgentKind) -> String {
    kind.binary().to_string()
}

/// Tab title for a launched agent: "claude · agent/claude-1".
pub fn tab_title(kind: AgentKind, branch_or_dir: &str) -> String {
    format!("{} \u{b7} {}", kind.slug(), branch_or_dir)
}

// ───────────────────────────── worktrees ─────────────────────────────

/// One worktree to create.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreePlan {
    /// The repository work tree `git worktree add` runs in.
    pub repo_root: PathBuf,
    /// `../<repo>-<agent>-<n>`, resolved.
    pub path: PathBuf,
    /// `agent/<agent>-<n>`.
    pub branch: String,
    pub n: usize,
}

impl WorktreePlan {
    /// Arguments for `git` (without the program name): `-C <root> worktree add <path> -b <branch>`.
    pub fn git_args(&self) -> Vec<String> {
        vec![
            "-C".into(),
            self.repo_root.to_string_lossy().into_owned(),
            "worktree".into(),
            "add".into(),
            self.path.to_string_lossy().into_owned(),
            "-b".into(),
            self.branch.clone(),
        ]
    }

    /// The equivalent shell command, for logs and tooltips.
    pub fn shell_command(&self) -> String {
        format!(
            "git worktree add ../{} -b {}",
            self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            self.branch
        )
    }
}

/// Names for attempt number `n` (1-based).
pub fn plan_for(repo_root: &Path, repo_name: &str, kind: AgentKind, n: usize) -> WorktreePlan {
    let parent = repo_root.parent().unwrap_or(repo_root);
    WorktreePlan {
        repo_root: repo_root.to_path_buf(),
        path: parent.join(format!("{repo_name}-{}-{n}", kind.slug())),
        branch: format!("agent/{}-{n}", kind.slug()),
        n,
    }
}

/// Pick the first `count` free numbers, skipping any whose directory or branch
/// already exists (`taken(path, branch)`).
pub fn plan_free(repo_root: &Path, repo_name: &str, kind: AgentKind, count: usize, taken: &dyn Fn(&Path, &str) -> bool) -> Vec<WorktreePlan> {
    let mut out = Vec::new();
    let mut n = 1;
    while out.len() < count && n < 10_000 {
        let p = plan_for(repo_root, repo_name, kind, n);
        if !taken(&p.path, &p.branch) {
            out.push(p);
        }
        n += 1;
    }
    out
}

fn branch_exists(repo_root: &Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Create `count` worktrees (blocking; run on a worker thread).
pub fn create_worktrees(repo_root: &Path, repo_name: &str, kind: AgentKind, count: usize) -> Result<Vec<WorktreePlan>, String> {
    let root = repo_root.to_path_buf();
    let plans = plan_free(repo_root, repo_name, kind, count, &|p, b| p.exists() || branch_exists(&root, b));
    let mut done = Vec::new();
    for plan in plans {
        let out = std::process::Command::new("git")
            .args(plan.git_args())
            .output()
            .map_err(|e| format!("could not run git: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("git worktree add failed");
            return Err(format!("{}: {}", plan.shell_command(), first.trim()));
        }
        done.push(plan);
    }
    Ok(done)
}

// ───────────────────────────── layouts ─────────────────────────────

/// Where a finished launch should end up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// One new tab.
    Tab,
    /// One tab with a `cols x rows` grid of panes.
    Grid { cols: usize, rows: usize },
}

impl Target {
    pub fn count(self) -> usize {
        match self {
            Target::Tab => 1,
            Target::Grid { cols, rows } => cols * rows,
        }
    }
}

/// One split in building a grid from a single pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridStep {
    /// New column: split the last column's pane to the right.
    NewColumn,
    /// New row in column `col` (0-based): split that column's last pane downwards.
    NewRow { col: usize },
}

/// Steps that turn one pane into a `cols x rows` grid: first all columns (so
/// every column spans the full height), then each column is cut into rows. That
/// order yields a clean grid; the other way round leaves a full-width bottom row.
pub fn grid_steps(cols: usize, rows: usize) -> Vec<GridStep> {
    let (cols, rows) = (cols.max(1), rows.max(1));
    let mut v = vec![GridStep::NewColumn; cols - 1];
    for col in 0..cols {
        v.extend(std::iter::repeat(GridStep::NewRow { col }).take(rows - 1));
    }
    v
}

/// Rows x columns for a palette entry like "2×2".
pub fn parse_grid(s: &str) -> Option<(usize, usize)> {
    let s = s.trim().to_lowercase().replace('\u{d7}', "x");
    let (a, b) = s.split_once('x')?;
    let (c, r) = (a.trim().parse::<usize>().ok()?, b.trim().parse::<usize>().ok()?);
    ((1..=4).contains(&c) && (1..=4).contains(&r) && c * r > 1).then_some((c, r))
}

// ───────────────────────────── background job ─────────────────────────────

/// A worktree creation running on a worker thread.
pub struct Job {
    pub rx: Receiver<Result<Vec<WorktreePlan>, String>>,
    pub kind: AgentKind,
    pub target: Target,
    pub started: Instant,
    /// "Creating worktree rift-claude-1..." for the progress toast.
    pub label: String,
}

pub fn start_job(repo_root: PathBuf, repo_name: String, kind: AgentKind, target: Target) -> Job {
    let (tx, rx) = channel();
    let n = target.count();
    let label = if n == 1 {
        format!("Creating worktree for {}...", kind.name())
    } else {
        format!("Creating {n} worktrees for {}...", kind.name())
    };
    std::thread::spawn(move || {
        let _ = tx.send(create_worktrees(&repo_root, &repo_name, kind, n));
        crate::wake::wake();
    });
    Job { rx, kind, target, started: Instant::now(), label }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_command_construction() {
        let p = plan_for(Path::new("/home/me/code/rift"), "rift", AgentKind::ClaudeCode, 1);
        assert_eq!(p.path, Path::new("/home/me/code/rift-claude-1"));
        assert_eq!(p.branch, "agent/claude-1");
        assert_eq!(
            p.git_args(),
            vec!["-C", "/home/me/code/rift", "worktree", "add", "/home/me/code/rift-claude-1", "-b", "agent/claude-1"]
        );
        assert_eq!(p.shell_command(), "git worktree add ../rift-claude-1 -b agent/claude-1");
        let p = plan_for(Path::new("/w/app"), "app", AgentKind::CursorAgent, 3);
        assert_eq!(p.branch, "agent/cursor-3");
        assert!(p.shell_command().contains("../app-cursor-3"));
    }

    #[test]
    fn numbering_skips_taken_names() {
        let taken = |p: &Path, b: &str| p.ends_with("rift-codex-1") || b == "agent/codex-2";
        let v = plan_free(Path::new("/x/rift"), "rift", AgentKind::Codex, 3, &taken);
        let ns: Vec<usize> = v.iter().map(|p| p.n).collect();
        assert_eq!(ns, vec![3, 4, 5]);
        let none = plan_free(Path::new("/x/rift"), "rift", AgentKind::Codex, 2, &|_, _| false);
        assert_eq!(none.iter().map(|p| p.n).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn titles_and_commands() {
        assert_eq!(tab_title(AgentKind::ClaudeCode, "agent/claude-1"), "claude \u{b7} agent/claude-1");
        assert_eq!(launch_command(AgentKind::CursorAgent), "cursor-agent");
        assert_eq!(launch_command(AgentKind::Aider), "aider");
    }

    #[test]
    fn grid_shapes() {
        assert_eq!(grid_steps(1, 1), vec![]);
        assert_eq!(grid_steps(2, 1), vec![GridStep::NewColumn]);
        assert_eq!(
            grid_steps(2, 2),
            vec![GridStep::NewColumn, GridStep::NewRow { col: 0 }, GridStep::NewRow { col: 1 }]
        );
        assert_eq!(grid_steps(3, 2).len(), 5);
        assert_eq!(grid_steps(3, 2).len() + 1, Target::Grid { cols: 3, rows: 2 }.count());
        assert_eq!(parse_grid("2\u{d7}2"), Some((2, 2)));
        assert_eq!(parse_grid("3x2"), Some((3, 2)));
        assert_eq!(parse_grid("1x1"), None);
        assert_eq!(parse_grid("9x9"), None);
        assert_eq!(parse_grid("axb"), None);
    }

    #[cfg(unix)]
    #[test]
    fn path_lookup_requires_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("rift-launch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("claude");
        let plain = dir.join("codex");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        std::fs::write(&plain, "x").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
        let dirs = vec![dir.clone()];
        assert_eq!(find_in_dirs("claude", &dirs), Some(exe));
        assert_eq!(find_in_dirs("codex", &dirs), None);
        assert_eq!(find_in_dirs("gemini", &dirs), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Real git: create worktrees in a scratch repo (skipped when git is missing).
    #[test]
    fn creates_real_worktrees() {
        let git_ok = std::process::Command::new("git").arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
        if !git_ok {
            return;
        }
        let base = std::env::temp_dir().join(format!("rift-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("proj");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        };
        run(&["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "x").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
        let made = create_worktrees(&repo, "proj", AgentKind::ClaudeCode, 2).expect("worktrees");
        assert_eq!(made.len(), 2);
        assert!(base.join("proj-claude-1/a.txt").exists());
        assert!(base.join("proj-claude-2").is_dir());
        // A second call skips the taken numbers.
        let more = create_worktrees(&repo, "proj", AgentKind::ClaudeCode, 1).expect("worktrees");
        assert_eq!(more[0].n, 3);
        // The sidebar's git reader sees the branch and the main repo's name.
        let info = super::super::git::repo_info(&base.join("proj-claude-1")).unwrap();
        assert_eq!(info.name, "proj");
        assert_eq!(info.branch.as_deref(), Some("agent/claude-1"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
