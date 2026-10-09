//! Budgeted impact analysis (file counts, git state). Everything here runs on
//! a worker thread and is abandoned when the shared time budget runs out, so
//! the UI thread never waits more than [`BUDGET_MS`] in total, whatever the
//! size of the tree or the state of the repository.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Total wall-clock budget for all analyses of one preview.
pub const BUDGET_MS: u64 = 300;
/// The directory walker gives up after this many entries.
const MAX_ENTRIES: usize = 500_000;

#[derive(Clone, Copy)]
pub struct Budget {
    deadline: Instant,
    /// When false every analysis is skipped (classification-only pass).
    pub enabled: bool,
}

impl Budget {
    pub fn new(enabled: bool) -> Self {
        Self { deadline: Instant::now() + Duration::from_millis(BUDGET_MS), enabled }
    }

    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scan {
    Skipped,
    Missing,
    File(u64),
    /// Entries (files + dirs) seen below the directory; `complete == false`
    /// means the walk stopped early, so the real count is at least `entries`.
    Dir { entries: usize, complete: bool },
}

pub fn scan_path(path: &Path, budget: &Budget) -> Scan {
    if !budget.enabled {
        return Scan::Skipped;
    }
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return Scan::Missing,
    };
    if !meta.is_dir() {
        return Scan::File(meta.len());
    }
    let counter = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::channel::<bool>();
    let root: PathBuf = path.to_path_buf();
    let c2 = counter.clone();
    let deadline = budget.deadline;
    std::thread::spawn(move || {
        let mut stack = vec![root];
        let mut complete = true;
        let mut n = 0usize;
        'walk: while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for entry in rd.flatten() {
                n += 1;
                if n & 0xFF == 0 {
                    c2.store(n, Ordering::Relaxed);
                    if Instant::now() >= deadline || n >= MAX_ENTRIES {
                        complete = false;
                        break 'walk;
                    }
                }
                // Symlinks are counted but never followed.
                if entry.file_type().map_or(false, |t| t.is_dir()) {
                    stack.push(entry.path());
                }
            }
        }
        c2.store(n, Ordering::Relaxed);
        let _ = tx.send(complete);
    });
    match rx.recv_timeout(budget.remaining()) {
        Ok(complete) => Scan::Dir { entries: counter.load(Ordering::Relaxed), complete },
        Err(_) => Scan::Dir { entries: counter.load(Ordering::Relaxed), complete: false },
    }
}

/// Run `git <args>` in `cwd`; `None` on failure, non-zero exit or timeout.
pub fn git(cwd: Option<&Path>, args: &[&str], budget: &Budget) -> Option<String> {
    if !budget.enabled || budget.remaining().is_zero() {
        return None;
    }
    let mut cmd = Command::new("git");
    cmd.arg("--no-optional-locks").args(args).stdin(Stdio::null()).stderr(Stdio::null());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(cmd.output());
    });
    match rx.recv_timeout(budget.remaining()) {
        Ok(Ok(o)) if o.status.success() => Some(String::from_utf8_lossy(&o.stdout).into_owned()),
        _ => None,
    }
}

pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.0}K", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}M", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_counts_and_respects_budget() {
        let d = std::env::temp_dir().join(format!("rift-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("a/b/f"), b"x").unwrap();
        std::fs::write(d.join("g"), b"yy").unwrap();
        let b = Budget::new(true);
        assert_eq!(scan_path(&d, &b), Scan::Dir { entries: 4, complete: true });
        assert_eq!(scan_path(&d.join("g"), &b), Scan::File(2));
        assert_eq!(scan_path(&d.join("nope"), &b), Scan::Missing);
        assert_eq!(scan_path(&d, &Budget::new(false)), Scan::Skipped);
        let _ = std::fs::remove_dir_all(&d);
    }
}
