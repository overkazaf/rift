//! Blocking git and shell work of the workflows. Everything here runs on
//! background threads (the glue spawns them); nothing touches the UI.
//!
//! Safety rules (see also the confirmations in `workflow/mod.rs`):
//!
//! * A merge only happens in a clean main worktree (no staged / unstaged
//!   changes to tracked files) that is on the branch the run started from. The
//!   user's uncommitted work is never touched; conflicts abort the merge.
//! * Only `agent/bestof-*` branches and sibling `*-bestof-*` worktrees created
//!   by a run can be deleted, never the branch that is checked out.
//! * Command construction is separate from execution so it can be tested.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::bestof::{is_candidate_branch, DiffStat, TestStatus};
use crate::review::diff::{self, ParsedDiff};
use crate::review::git as rg;

const GIT_TIMEOUT: Duration = Duration::from_secs(120);
const QUICK: Duration = Duration::from_secs(20);

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    rg::run(dir, args, &[], None, GIT_TIMEOUT, 4 << 20).map(|o| o.text())
}

fn git_quick(dir: &Path, args: &[&str]) -> Result<String, String> {
    rg::run(dir, args, &[], None, QUICK, 1 << 20).map(|o| o.text().trim().to_string())
}

/// A git invocation: directory and arguments (without the `git` program name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCmd {
    pub dir: PathBuf,
    pub args: Vec<String>,
}

impl GitCmd {
    pub fn new(dir: &Path, args: &[&str]) -> GitCmd {
        GitCmd { dir: dir.to_path_buf(), args: args.iter().map(|s| s.to_string()).collect() }
    }

    /// For logs and tests: `git -C <dir> <args>`.
    #[cfg(test)]
    pub fn display(&self) -> String {
        format!("git -C {} {}", self.dir.display(), self.args.join(" "))
    }

    fn run(&self) -> Result<String, String> {
        let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        git(&self.dir, &args)
    }
}

// ───────────────────────────── repository facts ─────────────────────────────

/// Where a run starts from: (current branch if on one, full HEAD commit).
pub fn head_info(dir: &Path) -> Result<(Option<String>, String), String> {
    let commit = git_quick(dir, &["rev-parse", "--verify", "HEAD"]).map_err(|_| "the repository has no commits yet: commit something first".to_string())?;
    rg::check_sha(&commit)?;
    let branch = git_quick(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).ok().filter(|b| b != "HEAD" && !b.is_empty());
    Ok((branch, commit))
}

/// Tracked files with staged or unstaged changes (untracked files do not count).
pub fn dirty_tracked(dir: &Path) -> Result<Vec<String>, String> {
    let out = git_quick(dir, &["status", "--porcelain", "--untracked-files=no"])?;
    Ok(out.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

/// Any change at all, untracked files included.
pub fn dirty_any(dir: &Path) -> Result<usize, String> {
    Ok(git_quick(dir, &["status", "--porcelain"])?.lines().filter(|l| !l.trim().is_empty()).count())
}

// ───────────────────────────── candidate results ─────────────────────────────

/// Diff of a candidate's working tree (commits and uncommitted changes) against `base`.
pub fn candidate_diff(dir: &Path, base_commit: &str) -> Result<(DiffStat, ParsedDiff), String> {
    let tree = rg::snapshot_tree(dir)?;
    let out = rg::diff_trees(dir, base_commit, &tree)?;
    let parsed = diff::parse_unified(&out.text(), out.truncated);
    let (files, added, removed) = parsed.totals();
    Ok((DiffStat { files, added, removed }, parsed))
}

/// Unified diff between two trees (a writer's turn, for the reviewer prompt).
pub fn tree_diff_text(repo: &Path, a: &str, b: &str) -> Result<String, String> {
    Ok(rg::diff_trees(repo, a, b)?.text())
}

/// Run the test command in `dir` through the user's login shell. Output is
/// merged and the tail kept; the process is killed after `timeout`.
pub fn run_tests(dir: &Path, cmd: &str, timeout: Duration) -> TestStatus {
    match run_shell(dir, cmd, timeout) {
        Ok((true, _)) => TestStatus::Passed,
        Ok((false, out)) => TestStatus::Failed(super::fixtests::tail_lines(&out, super::fixtests::MAX_OUTPUT_LINES)),
        Err(e) => TestStatus::Failed(e),
    }
}

/// `(success, combined output)`.
pub fn run_shell(dir: &Path, cmd: &str, timeout: Duration) -> Result<(bool, String), String> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty() && Path::new(s).exists()).unwrap_or_else(|| "/bin/sh".into());
    let mut child = Command::new(&shell)
        .arg("-l")
        .arg("-c")
        .arg(format!("({cmd}) 2>&1"))
        .current_dir(dir)
        .env("CI", "1")
        .env("NO_COLOR", "1")
        .env("CARGO_TERM_COLOR", "never")
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run `{cmd}`: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let reader = std::thread::spawn(move || {
        // Keep the last 256 KB: failures are at the end.
        let mut keep: Vec<u8> = Vec::new();
        let mut buf = [0u8; 16 * 1024];
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 {
                break;
            }
            keep.extend_from_slice(&buf[..n]);
            if keep.len() > 512 * 1024 {
                let cut = keep.len() - 256 * 1024;
                keep.drain(..cut);
            }
        }
        keep
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("`{cmd}` timed out after {} minutes", timeout.as_secs() / 60));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("wait failed: {e}")),
        }
    };
    let out = reader.join().unwrap_or_default();
    Ok((status.success(), String::from_utf8_lossy(&out).into_owned()))
}

// ───────────────────────────── merge ─────────────────────────────

/// Everything needed to merge one candidate into the main worktree.
#[derive(Clone, Debug)]
pub struct MergePlan {
    /// The user's main worktree.
    pub repo: PathBuf,
    /// Branch the run started from (the merge target); `None` = was detached.
    pub base_branch: Option<String>,
    pub cand_dir: PathBuf,
    pub cand_branch: String,
    pub label: String,
    pub task: String,
}

fn first_line(s: &str, max: usize) -> String {
    let l = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    l.chars().take(max).collect()
}

impl MergePlan {
    pub fn merge_message(&self) -> String {
        format!("Merge {} ({}) from best-of-N: {}", self.label, self.cand_branch, first_line(&self.task, 72))
    }

    pub fn commit_message(&self) -> String {
        format!("{}: {}", self.label, first_line(&self.task, 72))
    }

    /// Commands, in order, for a candidate with uncommitted work.
    pub fn commit_cmds(&self) -> Vec<GitCmd> {
        vec![
            GitCmd::new(&self.cand_dir, &["add", "-A"]),
            GitCmd::new(&self.cand_dir, &["commit", "-q", "-m", &self.commit_message()]),
        ]
    }

    pub fn merge_cmd(&self) -> GitCmd {
        GitCmd::new(&self.repo, &["merge", "--no-ff", "--no-edit", "-m", &self.merge_message(), &self.cand_branch])
    }

    pub fn abort_cmd(&self) -> GitCmd {
        GitCmd::new(&self.repo, &["merge", "--abort"])
    }
}

/// What a merge would do, for the confirmation dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preflight {
    pub target: String,
    /// Uncommitted changes in the candidate worktree (committed on its branch first).
    pub uncommitted: usize,
    pub commits_ahead: usize,
    pub stat: DiffStat,
}

/// The main worktree must be clean and on the run's branch.
fn check_main(plan: &MergePlan) -> Result<String, String> {
    let current = git_quick(&plan.repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if let Some(want) = &plan.base_branch {
        if &current != want {
            return Err(format!("The main worktree is on '{current}', but this run started from '{want}'. Check out '{want}' first. Nothing was merged."));
        }
    }
    let dirty = dirty_tracked(&plan.repo)?;
    if !dirty.is_empty() {
        let first: Vec<&str> = dirty.iter().take(3).map(String::as_str).collect();
        return Err(format!(
            "The main worktree has {} uncommitted change{} ({}{}). Commit or stash them first. Nothing was merged.",
            dirty.len(),
            if dirty.len() == 1 { "" } else { "s" },
            first.join(", "),
            if dirty.len() > 3 { ", ..." } else { "" }
        ));
    }
    Ok(current)
}

pub fn preflight(plan: &MergePlan, base_commit: &str) -> Result<Preflight, String> {
    let target = check_main(plan)?;
    let uncommitted = dirty_any(&plan.cand_dir)?;
    let ahead = git_quick(&plan.repo, &["rev-list", "--count", &format!("{base_commit}..{}", plan.cand_branch)])?.parse::<usize>().unwrap_or(0);
    let (stat, _) = candidate_diff(&plan.cand_dir, base_commit)?;
    if stat.files == 0 && ahead == 0 && uncommitted == 0 {
        return Err("This candidate changed nothing: there is nothing to merge.".into());
    }
    Ok(Preflight { target, uncommitted, commits_ahead: ahead, stat })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeDone {
    pub sha: String,
    /// The candidate's uncommitted work was committed on its branch first.
    pub committed: bool,
}

/// Commit the candidate's pending work, then `git merge --no-ff` it into the
/// main worktree. A conflict (or any failure) aborts the merge and reports why.
pub fn execute_merge(plan: &MergePlan) -> Result<MergeDone, String> {
    check_main(plan)?;
    let mut committed = false;
    if dirty_any(&plan.cand_dir)? > 0 {
        let [add, commit] = <[GitCmd; 2]>::try_from(plan.commit_cmds()).map_err(|_| "internal error")?;
        add.run().map_err(|e| format!("could not stage the candidate's changes: {e}"))?;
        if let Err(e) = commit.run() {
            if e.contains("tell me who you are") || e.contains("empty ident") || e.contains("Author identity unknown") {
                let msg = plan.commit_message();
                git(&plan.cand_dir, &["-c", "user.name=Rift", "-c", "user.email=rift@localhost", "commit", "-q", "-m", &msg])
                    .map_err(|e| format!("could not commit the candidate's changes: {e}"))?;
            } else {
                return Err(format!("could not commit the candidate's changes: {e}"));
            }
        }
        committed = true;
    }
    let merge = plan.merge_cmd();
    match merge.run() {
        Ok(_) => {
            let sha = git_quick(&plan.repo, &["rev-parse", "HEAD"])?;
            Ok(MergeDone { sha, committed })
        }
        Err(e) => {
            let conflicts: Vec<String> = git_quick(&plan.repo, &["diff", "--name-only", "--diff-filter=U"])
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .filter(|l| !l.is_empty())
                .collect();
            let aborted = plan.abort_cmd().run().is_ok();
            if conflicts.is_empty() {
                Err(format!("The merge failed: {}{}", first_line(&e, 200), if aborted { "" } else { " (could not abort; run `git merge --abort`)" }))
            } else {
                let shown: Vec<&str> = conflicts.iter().take(5).map(String::as_str).collect();
                Err(format!(
                    "Merge conflict in {}{}. The merge was aborted and '{}' is unchanged. Resolve it by hand: git merge --no-ff {}{}",
                    shown.join(", "),
                    if conflicts.len() > 5 { ", ..." } else { "" },
                    plan.base_branch.as_deref().unwrap_or("HEAD"),
                    plan.cand_branch,
                    if aborted { "" } else { " (could not abort; run `git merge --abort`)" }
                ))
            }
        }
    }
}

// ───────────────────────────── discard ─────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscardItem {
    pub dir: PathBuf,
    pub branch: String,
    pub label: String,
}

/// May this worktree + branch be deleted? Only things a best-of run created.
pub fn validate_discard(repo: &Path, it: &DiscardItem, current_branch: Option<&str>) -> Result<(), String> {
    if !is_candidate_branch(&it.branch) {
        return Err(format!("'{}' is not a best-of-N branch: refusing to delete it", it.branch));
    }
    if current_branch == Some(it.branch.as_str()) {
        return Err(format!("'{}' is checked out in the main worktree: refusing to delete it", it.branch));
    }
    let name = it.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if it.dir == repo || repo.starts_with(&it.dir) || !name.contains("-bestof-") {
        return Err(format!("{} is not a best-of-N worktree: refusing to remove it", it.dir.display()));
    }
    Ok(())
}

/// `git worktree remove --force <dir>` then `git branch -D <branch>`, run in the main repo.
pub fn discard_cmds(repo: &Path, it: &DiscardItem) -> Vec<GitCmd> {
    vec![
        GitCmd { dir: repo.to_path_buf(), args: vec!["worktree".into(), "remove".into(), "--force".into(), it.dir.to_string_lossy().into_owned()] },
        GitCmd::new(repo, &["branch", "-D", &it.branch]),
    ]
}

/// Remove the items. Returns one result per item (label, outcome).
pub fn execute_discard(repo: &Path, items: &[DiscardItem]) -> Vec<(String, Result<(), String>)> {
    let current = git_quick(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).ok();
    items
        .iter()
        .map(|it| {
            let res = (|| {
                validate_discard(repo, it, current.as_deref())?;
                let [remove, delete] = <[GitCmd; 2]>::try_from(discard_cmds(repo, it)).map_err(|_| "internal error")?;
                if let Err(e) = remove.run() {
                    // Already gone from disk: let git forget it and carry on.
                    if it.dir.exists() {
                        return Err(format!("could not remove {}: {}", it.dir.display(), first_line(&e, 160)));
                    }
                    let _ = git(repo, &["worktree", "prune"]);
                }
                if let Err(e) = delete.run() {
                    if !e.contains("not found") {
                        return Err(format!("could not delete branch {}: {}", it.branch, first_line(&e, 160)));
                    }
                }
                Ok(())
            })();
            (it.label.clone(), res)
        })
        .collect()
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::git::testutil::TempRepo;

    fn plan(repo: &Path, branch: &str, dir: &Path) -> MergePlan {
        MergePlan {
            repo: repo.to_path_buf(),
            base_branch: Some("main".into()),
            cand_dir: dir.to_path_buf(),
            cand_branch: branch.into(),
            label: "Codex".into(),
            task: "add a retry\nwith details".into(),
        }
    }

    #[test]
    fn merge_commands_are_constructed_as_documented() {
        let p = plan(Path::new("/w/app"), "agent/bestof-x-2", Path::new("/w/app-bestof-x-2"));
        assert_eq!(p.merge_cmd().display(), "git -C /w/app merge --no-ff --no-edit -m Merge Codex (agent/bestof-x-2) from best-of-N: add a retry agent/bestof-x-2");
        assert_eq!(p.merge_cmd().args.last().map(String::as_str), Some("agent/bestof-x-2"));
        assert_eq!(p.merge_cmd().args[4], "Merge Codex (agent/bestof-x-2) from best-of-N: add a retry", "the message is one argument");
        let c = p.commit_cmds();
        assert_eq!(c[0], GitCmd::new(Path::new("/w/app-bestof-x-2"), &["add", "-A"]));
        assert_eq!(c[1].args, ["commit", "-q", "-m", "Codex: add a retry"]);
        assert_eq!(p.abort_cmd().args, ["merge", "--abort"]);
        let d = discard_cmds(Path::new("/w/app"), &DiscardItem { dir: "/w/app-bestof-x-1".into(), branch: "agent/bestof-x-1".into(), label: "A".into() });
        assert_eq!(d[0].display(), "git -C /w/app worktree remove --force /w/app-bestof-x-1");
        assert_eq!(d[1].display(), "git -C /w/app branch -D agent/bestof-x-1");
    }

    #[test]
    fn discard_refuses_anything_that_is_not_a_candidate() {
        let repo = Path::new("/w/app");
        let ok = DiscardItem { dir: "/w/app-bestof-x-1".into(), branch: "agent/bestof-x-1".into(), label: "A".into() };
        assert!(validate_discard(repo, &ok, Some("main")).is_ok());
        let bad_branch = DiscardItem { branch: "main".into(), ..ok.clone() };
        assert!(validate_discard(repo, &bad_branch, Some("feature")).unwrap_err().contains("not a best-of-N branch"));
        let checked_out = DiscardItem { ..ok.clone() };
        assert!(validate_discard(repo, &checked_out, Some("agent/bestof-x-1")).unwrap_err().contains("checked out"));
        let main_dir = DiscardItem { dir: repo.to_path_buf(), ..ok.clone() };
        assert!(validate_discard(repo, &main_dir, None).is_err());
        let parent = DiscardItem { dir: "/w".into(), ..ok.clone() };
        assert!(validate_discard(repo, &parent, None).is_err());
        let other = DiscardItem { dir: "/w/documents".into(), ..ok };
        assert!(validate_discard(repo, &other, None).unwrap_err().contains("not a best-of-N worktree"));
    }

    struct Fixture {
        main: TempRepo,
        base: String,
        wts: Vec<crate::agents::launch::WorktreePlan>,
        slug: String,
    }

    fn fixture(n: usize) -> Fixture {
        let main = TempRepo::new("bestof");
        main.write("src/lib.txt", "line1\nline2\nline3\n");
        main.write("README.md", "hello\n");
        main.commit_all("init");
        main.git(&["branch", "-M", "main"]);
        let (branch, base) = head_info(&main.dir).unwrap();
        assert_eq!(branch.as_deref(), Some("main"));
        let name = main.dir.file_name().unwrap().to_string_lossy().into_owned();
        let none = |p: &Path, b: &str| p.exists() || crate::agents::launch::branch_exists(&main.dir, b);
        let (slug, plans) = crate::agents::launch::plan_bestof(&main.dir, &name, "retry", n, &none);
        let wts = crate::agents::launch::run_plans(plans).expect("worktrees");
        Fixture { main, base, wts, slug }
    }

    impl Fixture {
        fn plan(&self, i: usize) -> MergePlan {
            MergePlan {
                repo: self.main.dir.clone(),
                base_branch: Some("main".into()),
                cand_dir: self.wts[i].path.clone(),
                cand_branch: self.wts[i].branch.clone(),
                label: format!("Claude Code #{}", i + 1),
                task: "add retry".into(),
            }
        }

        fn cleanup(&self) {
            for w in &self.wts {
                let _ = std::fs::remove_dir_all(&w.path);
            }
        }
    }

    fn git_ok() -> bool {
        Command::new("git").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    #[test]
    fn merges_an_uncommitted_candidate_with_no_ff() {
        if !git_ok() {
            return;
        }
        let f = fixture(2);
        assert_eq!(f.slug, "retry");
        std::fs::write(f.wts[0].path.join("src/retry.txt"), "retry v1\n").unwrap();
        std::fs::write(f.wts[1].path.join("README.md"), "other idea\n").unwrap();
        let p = f.plan(0);
        // The compare view's diff sees uncommitted work.
        let (stat, parsed) = candidate_diff(&p.cand_dir, &f.base).unwrap();
        assert_eq!((stat.files, stat.added, stat.removed), (1, 1, 0));
        assert!(parsed.full_patch().contains("retry v1"));
        let pre = preflight(&p, &f.base).unwrap();
        assert_eq!((pre.target.as_str(), pre.uncommitted, pre.commits_ahead, pre.stat.files), ("main", 1, 0, 1));
        let done = execute_merge(&p).unwrap();
        assert!(done.committed);
        assert_eq!(f.main.read("src/retry.txt"), "retry v1\n");
        assert_eq!(f.main.git(&["rev-parse", "HEAD"]).trim(), done.sha);
        let parents = f.main.git(&["log", "-1", "--format=%p"]);
        assert_eq!(parents.split_whitespace().count(), 2, "--no-ff makes a merge commit");
        assert!(f.main.git(&["log", "-1", "--format=%s"]).contains("from best-of-N: add retry"));
        // The other candidate and the main README are untouched.
        assert_eq!(f.main.read("README.md"), "hello\n");
        assert_eq!(f.main.git(&["status", "--porcelain"]).trim(), "");
        f.cleanup();
    }

    #[test]
    fn dirty_main_worktree_refuses_and_keeps_the_users_work() {
        if !git_ok() {
            return;
        }
        let f = fixture(1);
        std::fs::write(f.wts[0].path.join("new.txt"), "x\n").unwrap();
        f.main.write("README.md", "my unsaved thoughts\n");
        let err = execute_merge(&f.plan(0)).unwrap_err();
        assert!(err.contains("uncommitted change") && err.contains("README.md") && err.contains("Nothing was merged"), "{err}");
        assert!(preflight(&f.plan(0), &f.base).is_err());
        assert_eq!(f.main.read("README.md"), "my unsaved thoughts\n", "the user's edit survives");
        assert!(!f.main.exists("new.txt"));
        // The candidate was not committed either.
        assert!(dirty_any(&f.wts[0].path).unwrap() > 0);
        f.cleanup();
    }

    #[test]
    fn wrong_branch_refuses() {
        if !git_ok() {
            return;
        }
        let f = fixture(1);
        std::fs::write(f.wts[0].path.join("new.txt"), "x\n").unwrap();
        f.main.git(&["checkout", "-q", "-b", "elsewhere"]);
        let err = execute_merge(&f.plan(0)).unwrap_err();
        assert!(err.contains("'elsewhere'") && err.contains("'main'"), "{err}");
        f.cleanup();
    }

    #[test]
    fn conflicts_abort_cleanly() {
        if !git_ok() {
            return;
        }
        let f = fixture(1);
        std::fs::write(f.wts[0].path.join("src/lib.txt"), "line1\nCANDIDATE\nline3\n").unwrap();
        // Meanwhile main moved on and touched the same line.
        f.main.write("src/lib.txt", "line1\nMAIN\nline3\n");
        f.main.commit_all("main edits line2");
        let before = f.main.git(&["rev-parse", "HEAD"]);
        let err = execute_merge(&f.plan(0)).unwrap_err();
        assert!(err.contains("Merge conflict in src/lib.txt") && err.contains("aborted") && err.contains("git merge --no-ff agent/bestof-retry-1"), "{err}");
        assert_eq!(f.main.git(&["rev-parse", "HEAD"]), before, "main is where it was");
        assert_eq!(f.main.read("src/lib.txt"), "line1\nMAIN\nline3\n");
        assert!(!f.main.dir.join(".git/MERGE_HEAD").exists(), "no half-finished merge is left");
        assert_eq!(f.main.git(&["status", "--porcelain"]).trim(), "");
        f.cleanup();
    }

    #[test]
    fn an_unchanged_candidate_has_nothing_to_merge() {
        if !git_ok() {
            return;
        }
        let f = fixture(1);
        assert!(preflight(&f.plan(0), &f.base).unwrap_err().contains("nothing to merge"));
        f.cleanup();
    }

    #[test]
    fn discard_removes_worktrees_and_branches_but_not_the_rest() {
        if !git_ok() {
            return;
        }
        let f = fixture(3);
        std::fs::write(f.wts[1].path.join("wip.txt"), "unmerged\n").unwrap();
        let items: Vec<DiscardItem> = [1usize, 2].iter().map(|&i| DiscardItem { dir: f.wts[i].path.clone(), branch: f.wts[i].branch.clone(), label: format!("c{i}") }).collect();
        let res = execute_discard(&f.main.dir, &items);
        assert!(res.iter().all(|(_, r)| r.is_ok()), "{res:?}");
        assert!(!f.wts[1].path.exists() && !f.wts[2].path.exists());
        assert!(f.wts[0].path.exists(), "the kept candidate stays");
        let branches = f.main.git(&["branch", "--list"]);
        assert!(branches.contains("agent/bestof-retry-1") && !branches.contains("agent/bestof-retry-2") && !branches.contains("agent/bestof-retry-3"), "{branches}");
        assert!(branches.contains("main"));
        // Discarding again (already gone) is not an error.
        let again = execute_discard(&f.main.dir, &items);
        assert!(again.iter().all(|(_, r)| r.is_ok()), "{again:?}");
        // A non-candidate branch is refused even if someone passes it in.
        let evil = execute_discard(&f.main.dir, &[DiscardItem { dir: f.wts[0].path.clone(), branch: "main".into(), label: "evil".into() }]);
        assert!(evil[0].1.is_err());
        assert!(f.main.git(&["branch", "--list"]).contains("main"));
        f.cleanup();
    }

    #[test]
    fn failed_worktree_creation_rolls_back() {
        if !git_ok() {
            return;
        }
        let main = TempRepo::new("rollback");
        main.write("a.txt", "a\n");
        main.commit_all("init");
        let name = main.dir.file_name().unwrap().to_string_lossy().into_owned();
        let (_, mut plans) = crate::agents::launch::plan_bestof(&main.dir, &name, "x", 2, &|_, _| false);
        // The second plan reuses the first one's branch: git refuses it.
        plans[1].branch = plans[0].branch.clone();
        let err = crate::agents::launch::run_plans(plans.clone()).unwrap_err();
        assert!(err.contains("git worktree add"), "{err}");
        assert!(!plans[0].path.exists(), "the first worktree was rolled back");
        assert!(!main.git(&["branch", "--list"]).contains("agent/bestof"));
    }

    #[test]
    fn test_runner_reports_pass_fail_and_timeout() {
        let dir = std::env::temp_dir();
        assert_eq!(run_tests(&dir, "exit 0", Duration::from_secs(20)), TestStatus::Passed);
        match run_tests(&dir, "echo boom; echo 'FAILED x'; exit 3", Duration::from_secs(20)) {
            TestStatus::Failed(out) => assert!(out.contains("boom") && out.contains("FAILED x"), "{out}"),
            other => panic!("{other:?}"),
        }
        match run_tests(&dir, "sleep 5", Duration::from_millis(300)) {
            TestStatus::Failed(out) => assert!(out.contains("timed out"), "{out}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn head_info_needs_a_commit() {
        if !git_ok() {
            return;
        }
        let r = TempRepo::new("nocommit");
        assert!(head_info(&r.dir).unwrap_err().contains("no commits"));
    }
}
