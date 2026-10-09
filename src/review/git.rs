//! Git plumbing for Change Review. Everything here is blocking and meant to run
//! on the review worker thread; each `git` call has a hard timeout.
//!
//! Checkpoints never touch the user's index, branches, stash or working tree:
//! a snapshot is a *tree object* written through a temporary `GIT_INDEX_FILE`
//! (a copy of the real index, so unchanged files are not re-hashed), after
//! `git add -A` on that copy (which respects `.gitignore`).

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::diff::{FileDiff, FileStatus};

pub const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(90);
pub const DIFF_TIMEOUT: Duration = Duration::from_secs(30);
pub const QUICK_TIMEOUT: Duration = Duration::from_secs(10);
/// Cap on diff text taken from git (the rest is dropped and flagged).
pub const MAX_DIFF_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub truncated: bool,
}

impl Output {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

fn drain<R: Read + Send + 'static>(mut r: R, cap: usize) -> std::thread::JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut trunc = false;
        let mut buf = [0u8; 16 * 1024];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out.len() < cap {
                        let take = n.min(cap - out.len());
                        out.extend_from_slice(&buf[..take]);
                        trunc |= take < n;
                    } else {
                        trunc = true; // keep draining so git never blocks on a full pipe
                    }
                }
            }
        }
        (out, trunc)
    })
}

/// Run `git <args>` in `dir`. `env` entries are added; repo-redirecting variables
/// inherited from our own environment are removed.
pub fn run(
    dir: &Path,
    args: &[&str],
    env: &[(&str, OsString)],
    stdin: Option<Vec<u8>>,
    timeout: Duration,
    max_out: usize,
) -> Result<Output, String> {
    let mut cmd = Command::new("git");
    cmd.args(["--no-pager", "-c", "core.quotepath=off", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().map_err(|e| format!("cannot run git: {e}"))?;
    if let (Some(data), Some(mut si)) = (stdin, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = si.write_all(&data);
        });
    }
    let out_h = drain(child.stdout.take().ok_or("no stdout")?, max_out);
    let err_h = drain(child.stderr.take().ok_or("no stderr")?, 64 * 1024);
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Do not join the readers: a grandchild (hook, filter) may still hold the
                    // pipes open; the threads end by themselves when it exits.
                    return Err(format!("git {} timed out after {}s", args.first().unwrap_or(&""), timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(format!("git wait failed: {e}")),
        }
    };
    let (stdout, truncated) = out_h.join().unwrap_or_default();
    let (stderr, _) = err_h.join().unwrap_or_default();
    if !status.success() {
        let msg = String::from_utf8_lossy(&stderr).trim().to_string();
        return Err(if msg.is_empty() { format!("git {} failed ({status})", args.first().unwrap_or(&"")) } else { msg });
    }
    Ok(Output { stdout, truncated })
}

fn quick(dir: &Path, args: &[&str]) -> Result<String, String> {
    run(dir, args, &[], None, QUICK_TIMEOUT, 1 << 20).map(|o| o.text().trim().to_string())
}

/// Repository top-level directory containing `dir` (`Err` for non-git directories).
pub fn repo_root(dir: &Path) -> Result<PathBuf, String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    let top = quick(dir, &["rev-parse", "--show-toplevel"]).map_err(|_| "not a git repository".to_string())?;
    if top.is_empty() {
        return Err("not a git repository (or a bare repository)".into());
    }
    Ok(PathBuf::from(top))
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A temporary index file inside the git dir, deleted on drop.
pub struct TempIndex {
    path: PathBuf,
}

impl TempIndex {
    /// New temp index seeded from the real index (fast; has stat data) or, failing
    /// that, from `HEAD`. May start empty in a repository without commits.
    pub fn seeded(repo: &Path) -> Result<TempIndex, String> {
        let git_dir = PathBuf::from(quick(repo, &["rev-parse", "--absolute-git-dir"])?);
        let n = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = git_dir.join(format!("rift-review-{}-{n}.idx", std::process::id()));
        let tmp = TempIndex { path };
        let real = git_dir.join("index");
        if std::fs::copy(&real, &tmp.path).is_err() {
            let _ = std::fs::remove_file(&tmp.path);
            if quick(repo, &["rev-parse", "--verify", "-q", "HEAD^{tree}"]).is_ok() {
                tmp.git(repo, &["read-tree", "HEAD"])?;
            }
        }
        Ok(tmp)
    }

    /// Temp index holding exactly `tree`.
    pub fn from_tree(repo: &Path, tree: &str) -> Result<TempIndex, String> {
        check_sha(tree)?;
        let git_dir = PathBuf::from(quick(repo, &["rev-parse", "--absolute-git-dir"])?);
        let n = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = TempIndex { path: git_dir.join(format!("rift-review-{}-{n}.idx", std::process::id())) };
        tmp.git(repo, &["read-tree", tree])?;
        Ok(tmp)
    }

    fn git(&self, repo: &Path, args: &[&str]) -> Result<Output, String> {
        self.git_in(repo, args, None, SNAPSHOT_TIMEOUT)
    }

    fn git_in(&self, repo: &Path, args: &[&str], stdin: Option<Vec<u8>>, t: Duration) -> Result<Output, String> {
        run(repo, args, &[("GIT_INDEX_FILE", self.path.clone().into_os_string())], stdin, t, 1 << 20)
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        // git may leave `<index>.lock` behind if it was killed on timeout.
        let mut lock = self.path.clone().into_os_string();
        lock.push(".lock");
        let _ = std::fs::remove_file(PathBuf::from(lock));
    }
}

/// Object names are hex; refuse anything else before it reaches a command line.
pub fn check_sha(s: &str) -> Result<(), String> {
    if (4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(format!("invalid object name '{s}'"))
    }
}

/// Write a tree object for the current working tree (tracked + untracked,
/// `.gitignore` respected) without touching the real index. The objects are
/// dangling (unreferenced) and are collected by `git gc` after its prune expiry.
pub fn snapshot_tree(repo: &Path) -> Result<String, String> {
    let idx = TempIndex::seeded(repo)?;
    idx.git(repo, &["add", "-A", "--", "."])?;
    let out = idx.git(repo, &["write-tree"])?;
    let sha = out.text().trim().to_string();
    check_sha(&sha)?;
    Ok(sha)
}

/// Unified diff `a..b` (both tree-ish), rename detection on.
pub fn diff_trees(repo: &Path, a: &str, b: &str) -> Result<Output, String> {
    check_sha(a)?;
    check_sha(b)?;
    run(
        repo,
        &[
            "diff", "--no-color", "--no-ext-diff", "--no-textconv", "-M", "-U3",
            "--src-prefix=a/", "--dst-prefix=b/", a, b, "--",
        ],
        &[],
        None,
        DIFF_TIMEOUT,
        MAX_DIFF_BYTES,
    )
}

// ───────────────────────── revert ─────────────────────────

/// Why a path may not be reverted.
#[derive(Debug, PartialEq, Eq)]
pub enum PathError {
    Empty,
    Absolute,
    Escapes,
    Symlink,
    Io(String),
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::Empty => write!(f, "empty path"),
            PathError::Absolute => write!(f, "absolute path refused"),
            PathError::Escapes => write!(f, "path escapes the repository"),
            PathError::Symlink => write!(f, "path goes through a symlink out of the repository"),
            PathError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// Validate a repo-relative path and return the absolute target inside `repo_root`.
///
/// Refused: empty, absolute, `..` components, `.git` internals, and any path
/// whose nearest existing ancestor resolves (through symlinks) outside the repository.
pub fn safe_target(repo_root: &Path, rel: &str) -> Result<PathBuf, PathError> {
    if rel.is_empty() || rel.contains('\0') {
        return Err(PathError::Empty);
    }
    let p = Path::new(rel);
    if p.is_absolute() || rel.starts_with('/') || rel.starts_with('\\') {
        return Err(PathError::Absolute);
    }
    let mut depth = 0usize;
    for c in p.components() {
        match c {
            Component::Normal(n) => {
                depth += 1;
                if n == ".git" {
                    return Err(PathError::Escapes);
                }
            }
            Component::CurDir => {}
            _ => return Err(PathError::Escapes),
        }
    }
    if depth == 0 {
        return Err(PathError::Empty);
    }
    let root = repo_root.canonicalize().map_err(|e| PathError::Io(format!("{}: {e}", repo_root.display())))?;
    let target = root.join(p);
    // Resolve the parent chain: the nearest existing ancestor must stay inside the root.
    let mut anc = target.parent().map(Path::to_path_buf).unwrap_or_else(|| root.clone());
    loop {
        match anc.canonicalize() {
            Ok(c) => {
                if !c.starts_with(&root) {
                    return Err(PathError::Symlink);
                }
                break;
            }
            Err(_) => {
                if !anc.pop() || !anc.starts_with(&root) {
                    return Err(PathError::Escapes);
                }
            }
        }
    }
    Ok(target)
}

/// What to do to one path to bring it back to the base tree.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RevertOp {
    /// Restore content/mode from the base tree.
    Restore(String),
    /// The file did not exist in the base tree: delete it.
    Delete(String),
}

impl RevertOp {
    pub fn path(&self) -> &str {
        match self {
            RevertOp::Restore(p) | RevertOp::Delete(p) => p,
        }
    }
}

/// Operations that undo `f` (a rename restores the old path and deletes the new one).
pub fn ops_for(f: &FileDiff) -> Vec<RevertOp> {
    match f.status {
        FileStatus::Added | FileStatus::Copied => vec![RevertOp::Delete(f.new_path.clone())],
        FileStatus::Modified | FileStatus::Deleted => vec![RevertOp::Restore(f.new_path.clone())],
        FileStatus::Renamed => {
            let mut v = vec![RevertOp::Restore(f.old_path.clone())];
            if f.new_path != f.old_path {
                v.push(RevertOp::Delete(f.new_path.clone()));
            }
            v
        }
    }
}

/// Apply `ops` against `base_tree`. Every path is validated first; nothing is
/// modified unless all paths pass. Returns the number of paths changed.
/// Never touches the real index or HEAD.
pub fn revert(repo_root: &Path, base_tree: &str, ops: &[RevertOp]) -> Result<usize, String> {
    check_sha(base_tree)?;
    let mut targets = Vec::with_capacity(ops.len());
    for op in ops {
        let t = safe_target(repo_root, op.path()).map_err(|e| format!("{}: {e}", op.path()))?;
        targets.push(t);
    }
    // Restores first (a rename's old path), then deletions.
    let restores: Vec<&str> = ops.iter().filter_map(|o| if let RevertOp::Restore(p) = o { Some(p.as_str()) } else { None }).collect();
    let mut n = 0;
    if !restores.is_empty() {
        let idx = TempIndex::from_tree(repo_root, base_tree)?;
        let mut input = Vec::new();
        for p in &restores {
            input.extend_from_slice(p.as_bytes());
            input.push(0);
        }
        idx.git_in(repo_root, &["checkout-index", "-f", "-q", "-z", "--stdin"], Some(input), QUICK_TIMEOUT)?;
        n += restores.len();
    }
    for (op, t) in ops.iter().zip(&targets) {
        if let RevertOp::Delete(_) = op {
            match std::fs::symlink_metadata(t) {
                Ok(m) if m.is_dir() => return Err(format!("{}: is a directory now; not deleting", op.path())),
                Ok(_) => {
                    std::fs::remove_file(t).map_err(|e| format!("{}: {e}", op.path()))?;
                    n += 1;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", op.path())),
            }
        }
    }
    Ok(n)
}

#[cfg(test)]
pub mod testutil {
    use super::*;

    pub struct TempRepo {
        pub dir: PathBuf,
    }

    impl TempRepo {
        pub fn new(tag: &str) -> TempRepo {
            let n = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("rift-review-test-{}-{tag}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            // Resolve /var -> /private/var on macOS so paths compare equal.
            let dir = dir.canonicalize().unwrap();
            let r = TempRepo { dir };
            r.git(&["init", "-q"]);
            r.git(&["config", "user.email", "t@example.com"]);
            r.git(&["config", "user.name", "T"]);
            r.git(&["config", "commit.gpgsign", "false"]);
            r
        }

        pub fn git(&self, args: &[&str]) -> String {
            let o = Command::new("git").args(args).current_dir(&self.dir).output().expect("git available");
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).into_owned()
        }

        pub fn write(&self, rel: &str, content: &str) {
            let p = self.dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }

        pub fn read(&self, rel: &str) -> String {
            std::fs::read_to_string(self.dir.join(rel)).unwrap()
        }

        pub fn exists(&self, rel: &str) -> bool {
            self.dir.join(rel).exists()
        }

        pub fn commit_all(&self, msg: &str) {
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "-m", msg]);
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::TempRepo;
    use super::*;
    use crate::review::diff::parse_unified;

    fn diff(repo: &Path, a: &str, b: &str) -> crate::review::diff::ParsedDiff {
        let o = diff_trees(repo, a, b).unwrap();
        parse_unified(&o.text(), o.truncated)
    }

    #[test]
    fn repo_root_and_non_git_dir() {
        let r = TempRepo::new("root");
        r.write("sub/a.txt", "x");
        assert_eq!(repo_root(&r.dir.join("sub")).unwrap(), r.dir);
        let plain = std::env::temp_dir().join(format!("rift-review-plain-{}", std::process::id()));
        std::fs::create_dir_all(&plain).unwrap();
        // /tmp itself might sit inside a repo on exotic setups; only assert when it does not.
        if quick(&plain, &["rev-parse", "--git-dir"]).is_err() {
            assert!(repo_root(&plain).is_err());
        }
        let _ = std::fs::remove_dir_all(&plain);
        assert!(repo_root(&r.dir.join("missing")).is_err());
    }

    #[test]
    fn snapshot_includes_untracked_respects_gitignore_and_leaves_index_alone() {
        let r = TempRepo::new("snap");
        r.write(".gitignore", "target/\n*.log\n");
        r.write("a.txt", "one\n");
        r.commit_all("init");
        r.write("a.txt", "two\n"); // unstaged edit
        r.write("new.txt", "fresh\n"); // untracked
        r.write("target/big.o", "junk");
        r.write("x.log", "log");
        let status_before = r.git(&["status", "--porcelain"]);
        let tree = snapshot_tree(&r.dir).unwrap();
        assert_eq!(r.git(&["status", "--porcelain"]), status_before, "index/worktree untouched");
        let listing = r.git(&["ls-tree", "-r", "--name-only", &tree]);
        let names: Vec<&str> = listing.lines().collect();
        assert!(names.contains(&"a.txt") && names.contains(&"new.txt") && names.contains(&".gitignore"), "{names:?}");
        assert!(!names.iter().any(|n| n.starts_with("target/") || n.ends_with(".log")), "{names:?}");
        assert_eq!(r.git(&["show", &format!("{tree}:a.txt")]), "two\n");
        // no temp index left behind
        let leftovers = std::fs::read_dir(r.dir.join(".git")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("rift-review-")).count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn snapshot_works_in_a_repo_without_commits() {
        let r = TempRepo::new("nocommit");
        r.write("a.txt", "hi\n");
        let t = snapshot_tree(&r.dir).unwrap();
        assert!(r.git(&["ls-tree", "--name-only", &t]).contains("a.txt"));
    }

    #[test]
    fn diff_between_checkpoint_and_now_covers_add_modify_delete_rename_binary() {
        let r = TempRepo::new("diff");
        r.write("keep.txt", "1\n2\n3\n");
        r.write("gone.txt", "bye\n");
        r.write("moved.txt", "a long enough body\nthat survives a rename\nwith several lines\nof stable text\n");
        r.commit_all("init");
        let before = snapshot_tree(&r.dir).unwrap();
        assert_eq!(diff(&r.dir, &before, &before).files.len(), 0);

        r.write("keep.txt", "1\n2 changed\n3\n");
        std::fs::remove_file(r.dir.join("gone.txt")).unwrap();
        std::fs::rename(r.dir.join("moved.txt"), r.dir.join("renamed.txt")).unwrap();
        r.write("sub/new.txt", "n1\nn2\n");
        std::fs::write(r.dir.join("blob.bin"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();
        let after = snapshot_tree(&r.dir).unwrap();
        let d = diff(&r.dir, &before, &after);
        let by = |p: &str| d.files.iter().find(|f| f.new_path == p).unwrap_or_else(|| panic!("{p} missing in {:?}", d.files.iter().map(|f| &f.new_path).collect::<Vec<_>>()));
        assert_eq!(by("keep.txt").status, FileStatus::Modified);
        assert_eq!((by("keep.txt").added, by("keep.txt").removed), (1, 1));
        assert_eq!(by("gone.txt").status, FileStatus::Deleted);
        assert_eq!(by("sub/new.txt").status, FileStatus::Added);
        assert_eq!(by("sub/new.txt").added, 2);
        assert!(by("blob.bin").binary);
        let rn = by("renamed.txt");
        assert_eq!((rn.status, rn.old_path.as_str()), (FileStatus::Renamed, "moved.txt"));
    }

    #[test]
    fn revert_restores_modified_deleted_and_removes_new_files() {
        let r = TempRepo::new("revert");
        r.write("keep.txt", "orig\n");
        r.write("gone.txt", "bye\n");
        r.write("mv.txt", "line one\nline two\nline three\nline four\nline five\n");
        r.commit_all("init");
        // A staged edit must survive: reverting works on the working tree only.
        r.write("staged.txt", "s\n");
        r.git(&["add", "staged.txt"]);
        let base = snapshot_tree(&r.dir).unwrap();
        let staged_before = r.git(&["diff", "--cached", "--name-only"]);

        r.write("keep.txt", "edited\n");
        std::fs::remove_file(r.dir.join("gone.txt")).unwrap();
        std::fs::rename(r.dir.join("mv.txt"), r.dir.join("mv2.txt")).unwrap();
        r.write("deep/er/new.txt", "new\n");
        let now = snapshot_tree(&r.dir).unwrap();
        let d = diff(&r.dir, &base, &now);
        assert_eq!(d.files.len(), 4, "{:?}", d.files.iter().map(|f| (&f.new_path, f.status)).collect::<Vec<_>>());

        for f in &d.files {
            revert(&r.dir, &base, &ops_for(f)).unwrap();
        }
        assert_eq!(r.read("keep.txt"), "orig\n");
        assert_eq!(r.read("gone.txt"), "bye\n");
        assert!(r.exists("mv.txt") && !r.exists("mv2.txt"));
        assert!(!r.exists("deep/er/new.txt"));
        assert_eq!(r.git(&["diff", "--cached", "--name-only"]), staged_before, "user's index untouched");
        // Everything is back: a fresh snapshot equals the base tree.
        assert_eq!(snapshot_tree(&r.dir).unwrap(), base);
    }

    #[test]
    fn revert_preserves_executable_bit_and_other_files() {
        let r = TempRepo::new("mode");
        r.write("run.sh", "#!/bin/sh\n");
        r.write("other.txt", "keep me\n");
        r.commit_all("init");
        let base = snapshot_tree(&r.dir).unwrap();
        r.write("run.sh", "#!/bin/sh\necho hi\n");
        r.write("other.txt", "user edit\n");
        revert(&r.dir, &base, &[RevertOp::Restore("run.sh".into())]).unwrap();
        assert_eq!(r.read("run.sh"), "#!/bin/sh\n");
        assert_eq!(r.read("other.txt"), "user edit\n", "only the requested path is touched");
    }

    #[test]
    fn path_escape_is_refused() {
        let r = TempRepo::new("escape");
        r.write("a.txt", "a\n");
        r.commit_all("init");
        let base = snapshot_tree(&r.dir).unwrap();
        let outside = r.dir.parent().unwrap().join(format!("rift-outside-{}.txt", std::process::id()));
        std::fs::write(&outside, "precious").unwrap();

        for bad in ["../x", "a/../../x", "/etc/passwd", "", ".git/config", "./../x"] {
            assert!(safe_target(&r.dir, bad).is_err(), "{bad:?} must be refused");
            assert!(revert(&r.dir, &base, &[RevertOp::Delete(bad.into())]).is_err());
            assert!(revert(&r.dir, &base, &[RevertOp::Restore(bad.into())]).is_err());
        }
        let rel_outside = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        assert!(revert(&r.dir, &base, &[RevertOp::Delete(rel_outside)]).is_err());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "precious");

        // One bad path vetoes the whole batch: the good file is not touched.
        r.write("a.txt", "changed\n");
        let ops = [RevertOp::Restore("a.txt".into()), RevertOp::Delete("../zzz".into())];
        assert!(revert(&r.dir, &base, &ops).is_err());
        assert_eq!(r.read("a.txt"), "changed\n");

        #[cfg(unix)]
        {
            // A symlinked directory pointing outside the repo is not a way out.
            let outdir = r.dir.parent().unwrap().join(format!("rift-outdir-{}", std::process::id()));
            std::fs::create_dir_all(&outdir).unwrap();
            std::fs::write(outdir.join("victim.txt"), "safe").unwrap();
            std::os::unix::fs::symlink(&outdir, r.dir.join("link")).unwrap();
            assert_eq!(safe_target(&r.dir, "link/victim.txt"), Err(PathError::Symlink));
            assert!(revert(&r.dir, &base, &[RevertOp::Delete("link/victim.txt".into())]).is_err());
            assert_eq!(std::fs::read_to_string(outdir.join("victim.txt")).unwrap(), "safe");
            let _ = std::fs::remove_dir_all(&outdir);
        }
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn safe_target_accepts_normal_and_not_yet_existing_paths() {
        let r = TempRepo::new("safe");
        assert!(safe_target(&r.dir, "a/b/c.txt").unwrap().starts_with(&r.dir));
        assert!(safe_target(&r.dir, "./a.txt").is_ok());
        assert!(check_sha("deadbeef").is_ok() && check_sha("--output=x").is_err() && check_sha("").is_err());
    }

    #[test]
    fn timeout_kills_a_slow_git_and_output_is_capped() {
        let r = TempRepo::new("timeout");
        let started = Instant::now();
        let e = run(&r.dir, &["-c", "alias.slow=!sleep 20", "slow"], &[], None, Duration::from_millis(150), 1024);
        assert!(e.unwrap_err().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(10), "the child was killed, not waited for");
        let big = run(&r.dir, &["--version"], &[], None, Duration::from_secs(5), 3).unwrap();
        assert!(big.truncated && big.stdout.len() == 3, "output cap flags truncation");
    }
}
