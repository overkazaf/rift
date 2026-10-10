//! `rift upgrade [VERSION] [--list] [--check] [--pre] [--no-verify] [--yes]`.

use std::io::{IsTerminal, Write};

use super::install::{self, Options, Progress, Target};
use super::release::{self, Release};
use super::version::Version;

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub list: bool,
    pub check: bool,
    pub pre: bool,
    pub no_verify: bool,
    pub yes: bool,
    pub help: bool,
    pub version: Option<String>,
}

pub fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    for arg in args {
        match arg.as_str() {
            "--list" | "-l" => a.list = true,
            "--check" | "-c" => a.check = true,
            "--pre" | "--prerelease" => a.pre = true,
            "--no-verify" => a.no_verify = true,
            "--yes" | "-y" => a.yes = true,
            "--help" | "-h" => a.help = true,
            s if s.starts_with('-') => return Err(format!("unknown option: {s}")),
            s => {
                if a.version.replace(s.to_string()).is_some() {
                    return Err("only one VERSION may be given".into());
                }
            }
        }
    }
    Ok(a)
}

pub fn print_usage() {
    println!("USAGE: rift upgrade [VERSION] [OPTIONS]");
    println!();
    println!("Download a release from GitHub, verify its SHA-256 and install it in place.");
    println!();
    println!("  rift upgrade              Upgrade to the latest stable release");
    println!("  rift upgrade 0.4.1        Install a specific version (downgrades allowed, with a warning)");
    println!("  rift upgrade --list       List releases (marks the one you run)");
    println!("  rift upgrade --check      Only report whether a newer release exists (exit 0 = up to date, 10 = update available)");
    println!();
    println!("OPTIONS:");
    println!("  --pre          Consider pre-releases for \"latest\"");
    println!("  -y, --yes      Don't ask for confirmation");
    println!("  --no-verify    Install even if the release publishes no .sha256 (a mismatch is always refused)");
    println!();
    println!("macOS replaces the running Rift.app (or installs /Applications/Rift.app); Linux replaces this binary.");
    println!("Set GITHUB_TOKEN to avoid the anonymous API rate limit.");
}

/// Exit code: 0 ok, 1 error, 10 (`--check`) update available.
pub fn run(args: &[String]) -> i32 {
    let a = match parse_args(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("rift upgrade: {e}");
            eprintln!("Try 'rift upgrade --help'");
            return 1;
        }
    };
    if a.help {
        print_usage();
        return 0;
    }
    let current = Version::current();
    eprintln!("Checking {} ...", release::RELEASES_PAGE);
    let releases = match release::fetch_releases() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rift upgrade: {e}");
            return 1;
        }
    };
    if releases.is_empty() {
        eprintln!("No releases published yet.");
        return 1;
    }
    if a.list {
        print!("{}", list_table(&releases, &current));
        return 0;
    }
    let chosen = match &a.version {
        Some(v) => match release::find(&releases, v) {
            Some(r) => r,
            None => {
                eprintln!("rift upgrade: no release {v}. Available: {}", releases.iter().take(8).map(|r| r.tag.as_str()).collect::<Vec<_>>().join(", "));
                return 1;
            }
        },
        None => match release::latest(&releases, a.pre) {
            Some(r) => r,
            None => {
                eprintln!("rift upgrade: no stable release found (try --pre)");
                return 1;
            }
        },
    };
    let cmp = chosen.version.as_ref().map(|v| v.cmp(&current));
    if a.check {
        let latest = release::latest(&releases, a.pre).unwrap_or(chosen);
        return match latest.version.as_ref().map(|v| v > &current) {
            Some(true) => {
                println!("Rift {} is available (you have {current}).", latest.version_str());
                println!("Run `rift upgrade` to install it.");
                10
            }
            _ => {
                println!("Rift {current} is up to date (latest: {}).", latest.version_str());
                0
            }
        };
    }
    if a.version.is_none() && cmp != Some(std::cmp::Ordering::Greater) {
        println!("Rift {current} is up to date (latest: {}).", chosen.version_str());
        return 0;
    }
    match cmp {
        Some(std::cmp::Ordering::Less) => eprintln!(
            "WARNING: {} is older than the running {current}; this is a downgrade (config/session formats may differ).",
            chosen.version_str()
        ),
        Some(std::cmp::Ordering::Equal) => eprintln!("Note: {current} is already the running version; reinstalling."),
        _ => {}
    }
    let target = install::current_target();
    println!("Rift {current} -> {} ({})", chosen.version_str(), target.describe());
    if let Target::DevBuild { exe } = &target {
        println!("You are running a development build ({}); it will not be overwritten.", exe.display());
        println!("The verified package will be downloaded and its path printed.");
    }
    if !a.yes && std::io::stdin().is_terminal() && !confirm("Proceed?") {
        println!("Aborted.");
        return 1;
    }
    match do_upgrade(chosen, &target, a.no_verify) {
        Ok(out) => {
            if let Some(p) = out.installed {
                println!("Installed Rift {} at {}.", out.version, p.display());
                println!("Restart Rift to use the new version.");
            } else if let Some(p) = out.artifact {
                println!("Downloaded and verified: {}", p.display());
            }
            0
        }
        Err(e) => {
            eprintln!("rift upgrade: {e}");
            1
        }
    }
}

fn do_upgrade(r: &Release, target: &Target, no_verify: bool) -> Result<install::Outcome, String> {
    let tty = std::io::stderr().is_terminal();
    let mut last_pct = u64::MAX;
    let mut progress = |p: Progress| match p {
        Progress::Stage(s) => {
            if tty && last_pct != u64::MAX {
                eprintln!();
            }
            last_pct = u64::MAX;
            eprintln!("==> {s}");
        }
        Progress::Bytes(got, total) => {
            let pct = if total > 0 { got * 100 / total } else { 0 };
            if pct != last_pct {
                last_pct = pct;
                if tty {
                    eprint!("\r    {:>3}%  {} / {}", pct, install::fmt_bytes(got), install::fmt_bytes(total));
                    let _ = std::io::stderr().flush();
                } else if pct % 25 == 0 {
                    eprintln!("    {pct}%");
                }
            }
        }
    };
    let res = install::upgrade(r, target, &Options { no_verify }, &mut progress);
    if tty && last_pct != u64::MAX {
        eprintln!();
    }
    res
}

fn confirm(q: &str) -> bool {
    eprint!("{q} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).is_ok() && matches!(s.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// `--list` output (pure, tested).
pub fn list_table(releases: &[Release], current: &Version) -> String {
    let latest = release::latest(releases, false).map(|r| r.tag.clone());
    let mut out = String::new();
    for r in releases {
        let mut tags = Vec::new();
        if r.version.as_ref() == Some(current) {
            tags.push("current");
        }
        if latest.as_deref() == Some(r.tag.as_str()) {
            tags.push("latest");
        }
        if r.prerelease {
            tags.push("pre-release");
        }
        let mark = if r.version.as_ref() == Some(current) { '*' } else { ' ' };
        let title = if r.name.is_empty() || r.name == r.tag { String::new() } else { format!("  {}", r.name) };
        let tags = if tags.is_empty() { String::new() } else { format!("  ({})", tags.join(", ")) };
        out.push_str(&format!("{mark} {:<14} {:<10}{title}{tags}\n", r.tag, r.date()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_upgrade_args() {
        assert_eq!(parse_args(&[]).unwrap(), Args::default());
        let a = parse_args(&s(&["0.4.1", "--yes", "--no-verify"])).unwrap();
        assert_eq!(a.version.as_deref(), Some("0.4.1"));
        assert!(a.yes && a.no_verify && !a.list);
        assert!(parse_args(&s(&["--list"])).unwrap().list);
        assert!(parse_args(&s(&["--check", "--pre"])).unwrap().pre);
        assert!(parse_args(&s(&["--bogus"])).is_err());
        assert!(parse_args(&s(&["1.0.0", "2.0.0"])).is_err());
    }

    #[test]
    fn list_marks_current_and_latest() {
        let r = release::parse_releases(release::tests::SAMPLE).unwrap();
        let t = list_table(&r, &Version::parse("0.4.0").unwrap());
        let line = |tag: &str| t.lines().find(|l| l.contains(tag)).unwrap().to_string();
        assert!(line("v0.4.0").starts_with('*') && line("v0.4.0").contains("current"), "{t}");
        assert!(line("v0.4.1").contains("latest") && line("v0.4.1").contains("2026-10-01"), "{t}");
        assert!(line("v0.5.0-rc.1").contains("pre-release"), "{t}");
        assert_eq!(t.lines().filter(|l| l.starts_with('*')).count(), 1);
    }
}
