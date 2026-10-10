//! In-app update flow: Help > Check for Updates, the command palette's
//! "Rift: Upgrade to Version..." picker, and the opt-in daily startup check.
//!
//! Network and install work runs on background threads; results land in a
//! process-wide inbox and the thread wakes the event loop (`crate::wake`),
//! which drains it in [`poll`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use winit::event_loop::ActiveEventLoop;

use super::install::{self, Options, Outcome, Target};
use super::release::{self, Release};
use super::version::Version;
use crate::app::App;
use crate::ui::confirm::{ConfirmAction, ConfirmRequest};
use crate::ui::kit::Tone;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Purpose {
    /// Menu / palette "Check for Updates" (`interactive`) or the quiet startup check.
    Check { interactive: bool },
    /// Fetch, then open the version picker.
    Pick,
}

enum Msg {
    Fetched(Purpose, Result<Vec<Release>, String>),
    Installed(Result<Outcome, String>),
}

static INBOX: Mutex<Vec<Msg>> = Mutex::new(Vec::new());
/// Last fetched release list (feeds the palette's "Upgrade to X" entries).
static CACHE: Mutex<Vec<Release>> = Mutex::new(Vec::new());
static FETCHING: AtomicBool = AtomicBool::new(false);
/// Purposes waiting for the fetch in flight.
static WAITING: Mutex<Vec<Purpose>> = Mutex::new(Vec::new());
static INSTALLING: AtomicBool = AtomicBool::new(false);
static STARTUP_CHECKED: AtomicBool = AtomicBool::new(false);

/// Answer handlers for the update dialogs.
pub enum UpgradeConfirm {
    /// "X is available": Later / Other Versions... / Upgrade & Restart.
    Offer { tag: String },
    /// "Up to date": OK / Other Versions...
    UpToDate,
    /// Picked from the list: Cancel / Install & Restart.
    Install { tag: String },
}

fn post(msg: Msg) {
    if let Ok(mut q) = INBOX.lock() {
        q.push(msg);
    }
    crate::wake::wake();
}

fn fetch(purpose: Purpose) {
    if let Ok(mut w) = WAITING.lock() {
        if !w.contains(&purpose) {
            w.push(purpose);
        }
    }
    if FETCHING.swap(true, Ordering::SeqCst) {
        return; // the request in flight answers this purpose too
    }
    std::thread::Builder::new()
        .name("rift-update-check".into())
        .spawn(move || {
            let r = release::fetch_releases().map_err(|e| e.to_string());
            FETCHING.store(false, Ordering::SeqCst);
            let waiting = WAITING.lock().map(|mut w| std::mem::take(&mut *w)).unwrap_or_default();
            for p in waiting {
                post(Msg::Fetched(p, r.clone()));
            }
        })
        .ok();
}

/// Help > Check for Updates...
pub fn check_now(app: &mut App) {
    app.win.blocks_ui.show_toast("Checking for updates\u{2026}");
    fetch(Purpose::Check { interactive: true });
}

/// "Rift: Upgrade to Version..." (fetches, then opens the picker).
pub fn choose_version(app: &mut App) {
    app.win.blocks_ui.show_toast("Fetching releases\u{2026}");
    fetch(Purpose::Pick);
}

/// Palette rows for the cached releases: (name, detail, tag).
pub fn palette_items() -> Vec<(String, String, String)> {
    let Ok(cache) = CACHE.lock() else { return Vec::new() };
    picker_rows(&cache, &Version::current())
}

fn picker_rows(releases: &[Release], current: &Version) -> Vec<(String, String, String)> {
    let latest = release::latest(releases, false).map(|r| r.tag.clone());
    releases
        .iter()
        .filter(|r| r.version.is_some())
        .map(|r| {
            let v = r.version.as_ref().unwrap();
            let mut detail = vec![r.date().to_string()];
            if v == current {
                detail.push("current".into());
            } else if v < current {
                detail.push("downgrade".into());
            }
            if latest.as_deref() == Some(r.tag.as_str()) {
                detail.push("latest".into());
            }
            if r.prerelease {
                detail.push("pre-release".into());
            }
            detail.retain(|d| !d.is_empty());
            (format!("Upgrade to {}", r.version_str()), detail.join(" \u{00b7} "), r.tag.clone())
        })
        .collect()
}

/// Once per day at most (`check_updates = true`).
pub fn check_due(last: Option<u64>, now: u64) -> bool {
    last.map_or(true, |t| now.saturating_sub(t) >= 24 * 3600 || t > now)
}

fn state_file(name: &str) -> Option<std::path::PathBuf> {
    crate::config::toml::config_path().parent().map(|d| d.join(name))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Set before the post-upgrade relaunch so the new process restores the
/// session even with `restore_session = false`. Consumed (deleted) on read.
const RESTORE_MARKER: &str = ".restore-after-upgrade";

pub fn take_restore_marker() -> bool {
    let Some(p) = state_file(RESTORE_MARKER) else { return false };
    let fresh = std::fs::read_to_string(&p).ok().and_then(|s| s.trim().parse::<u64>().ok()).is_some_and(|t| unix_now().saturating_sub(t) < 600);
    let existed = std::fs::remove_file(&p).is_ok();
    existed && fresh
}

/// Drain finished jobs; kick off the quiet daily check. Never blocks.
pub fn poll(app: &mut App, event_loop: &ActiveEventLoop) {
    if !STARTUP_CHECKED.swap(true, Ordering::SeqCst) && app.config.check_updates {
        let stamp = state_file("last-update-check");
        let last = stamp.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| s.trim().parse().ok());
        if check_due(last, unix_now()) {
            if let Some(p) = &stamp {
                let _ = std::fs::write(p, unix_now().to_string());
            }
            fetch(Purpose::Check { interactive: false });
        }
    }
    let msgs: Vec<Msg> = match INBOX.lock() {
        Ok(mut q) if !q.is_empty() => std::mem::take(&mut *q),
        _ => return,
    };
    for m in msgs {
        match m {
            Msg::Fetched(purpose, Ok(list)) => {
                if let Ok(mut c) = CACHE.lock() {
                    *c = list.clone();
                }
                on_fetched(app, purpose, &list);
            }
            Msg::Fetched(Purpose::Check { interactive: false }, Err(e)) => log::info!("update check failed: {e}"),
            Msg::Fetched(_, Err(e)) => notice(app, "Couldn't check for updates", &[e, format!("Releases: {}", release::RELEASES_PAGE)], Tone::Warning),
            Msg::Installed(res) => {
                INSTALLING.store(false, Ordering::SeqCst);
                on_installed(app, event_loop, res);
            }
        }
    }
    app.request_redraw();
}

fn on_fetched(app: &mut App, purpose: Purpose, list: &[Release]) {
    let current = Version::current();
    match purpose {
        Purpose::Pick => {
            if list.iter().all(|r| r.version.is_none()) {
                notice(app, "No releases found", &[format!("Nothing is published at {}", release::RELEASES_PAGE)], Tone::Warning);
                return;
            }
            crate::app::overlays::open_command_palette(app);
            app.win.command_palette.set_query("Upgrade to ");
        }
        Purpose::Check { interactive } => {
            let Some(latest) = release::latest(list, false) else {
                if interactive {
                    notice(app, "No releases found", &[format!("Nothing is published at {}", release::RELEASES_PAGE)], Tone::Warning);
                }
                return;
            };
            let newer = latest.version.as_ref().is_some_and(|v| *v > current);
            if newer {
                offer(app, latest, &current, interactive);
            } else if interactive {
                app.win.confirm.push(ConfirmRequest {
                    title: "Rift is up to date".into(),
                    badge: Some((format!("v{current}"), Tone::Success)),
                    lines: vec![format!("Rift {current} is the latest release (newest published: {}, {}).", latest.version_str(), latest.date())],
                    buttons: vec!["OK".into(), "Other Versions\u{2026}".into()],
                    default_sel: 0,
                    esc_choice: Some(0),
                    tone: Tone::Success,
                    action: ConfirmAction::Upgrade(UpgradeConfirm::UpToDate),
                });
            }
        }
    }
}

fn offer(app: &mut App, r: &Release, current: &Version, interactive: bool) {
    let mut lines = vec![format!("You have {current}. Released {}.", r.date())];
    let notes = release::notes_excerpt(&r.body, 6);
    if !notes.is_empty() {
        lines.push(String::new());
        lines.extend(notes);
    }
    lines.push(String::new());
    lines.push(target_line(&install::current_target()));
    app.win.confirm.push(ConfirmRequest {
        title: format!("Rift {} is available", r.version_str()),
        badge: Some(("UPDATE".into(), Tone::Accent)),
        lines,
        buttons: vec!["Later".into(), "Other Versions\u{2026}".into(), "Upgrade & Restart".into()],
        default_sel: if interactive { 2 } else { 0 },
        esc_choice: Some(0),
        tone: Tone::Accent,
        action: ConfirmAction::Upgrade(UpgradeConfirm::Offer { tag: r.tag.clone() }),
    });
}

fn target_line(t: &Target) -> String {
    match t {
        Target::DevBuild { exe } => format!("Development build ({}): the verified package is downloaded but nothing is replaced.", exe.display()),
        Target::Unsupported => format!("Self-upgrade isn't supported here; download from {}.", release::RELEASES_PAGE),
        t => format!("Downloads, verifies the SHA-256, will {} and restarts with your session.", t.describe()),
    }
}

/// Palette: "Upgrade to 0.4.1" picked.
pub fn confirm_version(app: &mut App, tag: &str) {
    let found = CACHE.lock().ok().and_then(|c| release::find(&c, tag).cloned());
    let Some(r) = found else {
        notice(app, "Unknown release", &[format!("{tag} is not in the release list; try \"Rift: Upgrade to Version\u{2026}\" again.")], Tone::Warning);
        return;
    };
    let current = Version::current();
    let ord = r.version.as_ref().map(|v| v.cmp(&current));
    let (title, tone, default_sel, mut lines) = match ord {
        Some(std::cmp::Ordering::Less) => (
            format!("Downgrade to Rift {}?", r.version_str()),
            Tone::Warning,
            0,
            vec![format!("{} is older than the running {current}. Config or session formats may differ.", r.version_str())],
        ),
        Some(std::cmp::Ordering::Equal) => (format!("Reinstall Rift {}?", r.version_str()), Tone::Neutral, 0, vec![format!("{current} is the version you are running.")]),
        _ => (format!("Upgrade to Rift {}?", r.version_str()), Tone::Accent, 1, vec![format!("You have {current}. Released {}.", r.date())]),
    };
    if r.prerelease {
        lines.push("This is a pre-release.".into());
    }
    lines.push(target_line(&install::current_target()));
    app.win.confirm.push(ConfirmRequest {
        title,
        badge: Some((if tone == Tone::Warning { "DOWNGRADE" } else { "UPDATE" }.into(), tone)),
        lines,
        buttons: vec!["Cancel".into(), "Install & Restart".into()],
        default_sel,
        esc_choice: Some(0),
        tone,
        action: ConfirmAction::Upgrade(UpgradeConfirm::Install { tag: r.tag.clone() }),
    });
}

pub fn resolve_confirm(app: &mut App, c: UpgradeConfirm, choice: Option<usize>) {
    match (c, choice) {
        (UpgradeConfirm::Offer { tag }, Some(2)) | (UpgradeConfirm::Install { tag }, Some(1)) => start_install(app, &tag),
        (UpgradeConfirm::Offer { .. }, Some(1)) | (UpgradeConfirm::UpToDate, Some(1)) => choose_version(app),
        _ => {}
    }
}

fn start_install(app: &mut App, tag: &str) {
    let Some(r) = CACHE.lock().ok().and_then(|c| release::find(&c, tag).cloned()) else { return };
    let target = install::current_target();
    if target == Target::Unsupported {
        notice(app, "Can't upgrade here", &[target_line(&target)], Tone::Warning);
        return;
    }
    if INSTALLING.swap(true, Ordering::SeqCst) {
        notice(app, "Upgrade in progress", &["An upgrade is already running.".into()], Tone::Neutral);
        return;
    }
    let ver = r.version_str();
    std::thread::Builder::new()
        .name("rift-upgrade".into())
        .spawn(move || {
            let res = install::upgrade(&r, &target, &Options { no_verify: false }, &mut |p| {
                if let install::Progress::Stage(s) = p {
                    log::info!("upgrade: {s}");
                }
            });
            post(Msg::Installed(res));
        })
        .ok();
    notice(
        app,
        &format!("Upgrading to Rift {ver}\u{2026}"),
        &["Downloading and verifying in the background. Rift saves your session and restarts when it's done.".into()],
        Tone::Accent,
    );
}

fn on_installed(app: &mut App, event_loop: &ActiveEventLoop, res: Result<Outcome, String>) {
    match res {
        Ok(Outcome { installed: Some(path), version, .. }) => {
            log::info!("upgraded to {version} at {}; relaunching", path.display());
            if !app.config.restore_session {
                if let Some(p) = state_file(RESTORE_MARKER) {
                    let _ = std::fs::write(p, unix_now().to_string());
                }
            }
            crate::app::window_ops::save_all(app);
            match install::relaunch(&path) {
                Ok(()) => event_loop.exit(),
                Err(e) => {
                    if let Some(p) = state_file(RESTORE_MARKER) {
                        let _ = std::fs::remove_file(p);
                    }
                    notice(app, &format!("Rift {version} installed"), &[format!("Couldn't restart automatically ({e}). Quit and reopen {}.", path.display())], Tone::Success);
                }
            }
        }
        Ok(Outcome { artifact, version, .. }) => {
            let mut lines = vec!["This is a development build, so nothing was replaced.".into()];
            if let Some(a) = artifact {
                lines.push(format!("Verified package: {}", a.display()));
            }
            notice(app, &format!("Rift {version} downloaded"), &lines, Tone::Neutral);
        }
        Err(e) => notice(app, "Upgrade failed", &[e], Tone::Danger),
    }
}

fn notice(app: &mut App, title: &str, lines: &[String], tone: Tone) {
    app.win.confirm.push(ConfirmRequest {
        title: title.into(),
        badge: Some(("UPDATE".into(), tone)),
        lines: lines.to_vec(),
        buttons: vec!["OK".into()],
        default_sel: 0,
        esc_choice: Some(0),
        tone,
        action: ConfirmAction::SshHostKey { reply: None }, // no-op on close
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_check_gate() {
        let day = 24 * 3600;
        assert!(check_due(None, 1_000_000));
        assert!(!check_due(Some(1_000_000), 1_000_000 + day - 1));
        assert!(check_due(Some(1_000_000), 1_000_000 + day));
        assert!(check_due(Some(2_000_000), 1_000_000), "clock went backwards");
    }

    #[test]
    fn picker_rows_mark_current_latest_and_downgrades() {
        let r = release::parse_releases(release::tests::SAMPLE).unwrap();
        let rows = picker_rows(&r, &Version::parse("0.4.0").unwrap());
        let names: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(names, ["Upgrade to 0.5.0-rc.1", "Upgrade to 0.4.1", "Upgrade to 0.4.0", "Upgrade to 0.3.0"], "non-version tags skipped");
        assert!(rows[0].1.contains("pre-release"));
        assert!(rows[1].1.contains("latest") && rows[1].1.starts_with("2026-10-01"), "{}", rows[1].1);
        assert!(rows[2].1.contains("current"));
        assert!(rows[3].1.contains("downgrade"));
        assert_eq!(rows[1].2, "v0.4.1");
    }
}
