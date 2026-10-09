//! Cheap, dependency-free git facts for the sidebar: repo root, repo name,
//! current branch. Reads `.git/HEAD` directly (no process spawn), so it is safe
//! to call from the UI thread.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoInfo {
    /// Top of the work tree containing the directory.
    pub root: PathBuf,
    /// Repository name: the main repository's directory name, even from a linked worktree.
    pub name: String,
    /// Branch name, or a short commit id on a detached HEAD.
    pub branch: Option<String>,
}

/// Find the git work tree containing `dir`.
pub fn repo_info(dir: &Path) -> Option<RepoInfo> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        let dot_git = d.join(".git");
        if let Ok(meta) = std::fs::metadata(&dot_git) {
            let (git_dir, main_root) = if meta.is_dir() {
                (dot_git.clone(), d.to_path_buf())
            } else {
                // Linked worktree / submodule: `.git` is a file "gitdir: <path>".
                let text = std::fs::read_to_string(&dot_git).ok()?;
                let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
                let gd = if Path::new(target).is_absolute() { PathBuf::from(target) } else { d.join(target) };
                // <main>/.git/worktrees/<name>  ->  <main>
                let main = gd
                    .ancestors()
                    .find(|a| a.file_name().is_some_and(|n| n == ".git"))
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| d.to_path_buf());
                (gd, main)
            };
            let branch = read_head(&git_dir);
            let name = main_root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            return Some(RepoInfo { root: d.to_path_buf(), name, branch });
        }
        cur = d.parent();
    }
    None
}

fn read_head(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(r) => {
            let r = r.trim();
            Some(r.strip_prefix("refs/heads/").unwrap_or(r).to_string())
        }
        None if head.len() >= 7 => Some(head[..7].to_string()),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("rift-git-{name}-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn plain_repo_branch_and_subdir() {
        let root = tmp("plain").join("rift");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        let i = repo_info(&root.join("src/deep")).unwrap();
        assert_eq!(i.root, root);
        assert_eq!(i.name, "rift");
        assert_eq!(i.branch.as_deref(), Some("feature/x"));
    }

    #[test]
    fn detached_head_shows_short_sha() {
        let root = tmp("detached").join("r");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "0123456789abcdef0123456789abcdef01234567\n").unwrap();
        assert_eq!(repo_info(&root).unwrap().branch.as_deref(), Some("0123456"));
    }

    #[test]
    fn linked_worktree_reports_the_main_repo_name() {
        let base = tmp("wt");
        let main = base.join("rift");
        let wt = base.join("rift-claude-1");
        std::fs::create_dir_all(main.join(".git/worktrees/rift-claude-1")).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(main.join(".git/worktrees/rift-claude-1/HEAD"), "ref: refs/heads/agent/claude-1\n").unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", main.join(".git/worktrees/rift-claude-1").display())).unwrap();
        let i = repo_info(&wt).unwrap();
        assert_eq!(i.root, wt);
        assert_eq!(i.name, "rift");
        assert_eq!(i.branch.as_deref(), Some("agent/claude-1"));
    }
}
