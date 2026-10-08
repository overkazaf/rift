//! Shell integration: embedded zsh/bash/fish scripts that make the shell emit
//! OSC 133 (semantic prompts) and OSC 7 (cwd), plus the logic that injects
//! them when spawning the user's shell. Injection never touches user rc files:
//!
//! * zsh  - `ZDOTDIR` points at our dir; its `.zshenv` restores the user's
//!          `ZDOTDIR`, sources their real `.zshenv`, then loads the hooks.
//! * bash - `bash --rcfile <rift.bash>`; it sources the user's profile/bashrc
//!          first, then installs `PROMPT_COMMAND` / `PS0` / DEBUG hooks.
//! * fish - `fish --login --init-command 'source <rift.fish>'`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use portable_pty::CommandBuilder;

const ZSH_ZSHENV: &str = include_str!("zsh/zshenv");
const ZSH_HOOKS: &str = include_str!("zsh/rift-integration.zsh");
const BASH_RC: &str = include_str!("bash/rift.bash");
const FISH_INIT: &str = include_str!("fish/rift.fish");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}

impl Shell {
    /// Detect from a shell path such as `$SHELL` (`/bin/zsh` -> Zsh).
    pub fn detect(shell_path: &str) -> Option<Shell> {
        match Path::new(shell_path).file_name()?.to_str()?.trim_start_matches('-') {
            "zsh" => Some(Shell::Zsh),
            "bash" => Some(Shell::Bash),
            "fish" => Some(Shell::Fish),
            _ => None,
        }
    }
}

/// `$XDG_CONFIG_HOME/rift/shell` or `~/.config/rift/shell`.
pub fn shell_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => dirs::home_dir()?.join(".config"),
    };
    Some(base.join("rift").join("shell"))
}

fn write_if_changed(path: &Path, content: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).map(|c| c == content).unwrap_or(false) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Write-then-rename so a concurrently spawning shell never reads a partial file.
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

/// Write the embedded scripts under [`shell_dir`]. Idempotent and cheap.
pub fn install_scripts(dir: &Path) -> std::io::Result<()> {
    write_if_changed(&dir.join("zsh").join(".zshenv"), ZSH_ZSHENV)?;
    write_if_changed(&dir.join("zsh").join("rift-integration.zsh"), ZSH_HOOKS)?;
    write_if_changed(&dir.join("bash").join("rift.bash"), BASH_RC)?;
    write_if_changed(&dir.join("fish").join("rift.fish"), FISH_INIT)?;
    Ok(())
}

/// Install once per process; returns the script directory on success.
fn installed_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = shell_dir()?;
        match install_scripts(&dir) {
            Ok(()) => Some(dir),
            Err(e) => {
                log::warn!("shell integration: cannot write {}: {e}", dir.display());
                None
            }
        }
    })
    .as_ref()
}

/// Build the command that launches the user's shell, with integration
/// injected unless `RIFT_NO_SHELL_INTEGRATION` is set or the shell is unknown.
pub fn build_shell_command() -> CommandBuilder {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty());
    let kind = shell.as_deref().and_then(Shell::detect);
    let disabled = std::env::var_os("RIFT_NO_SHELL_INTEGRATION").is_some();

    let dir = if disabled || kind.is_none() { None } else { installed_dir() };
    let mut cmd = match (kind, dir, shell.as_deref()) {
        (Some(Shell::Bash), Some(dir), Some(path)) => {
            let mut c = CommandBuilder::new(path);
            c.arg("--rcfile");
            c.arg(dir.join("bash").join("rift.bash"));
            c.arg("-i");
            c
        }
        (Some(Shell::Fish), Some(dir), Some(path)) => {
            let mut c = CommandBuilder::new(path);
            c.arg("--login");
            c.arg("--init-command");
            c.arg(format!("source {}", fish_quote(&dir.join("fish").join("rift.fish"))));
            c
        }
        // zsh (and everything else): default program, i.e. a login shell.
        _ => CommandBuilder::new_default_prog(),
    };

    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "rift");
    cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));

    if let (Some(Shell::Zsh), Some(dir)) = (kind, dir) {
        let zdir = dir.join("zsh");
        let orig = std::env::var("ZDOTDIR").unwrap_or_default();
        cmd.env("RIFT_ORIG_ZDOTDIR", orig);
        cmd.env("RIFT_SHELL_DIR", &zdir);
        cmd.env("ZDOTDIR", &zdir);
    }
    if dir.is_some() {
        cmd.env("RIFT_SHELL_INTEGRATION", "1");
    }
    cmd
}

fn fish_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_shells() {
        assert_eq!(Shell::detect("/bin/zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::detect("/usr/local/bin/bash"), Some(Shell::Bash));
        assert_eq!(Shell::detect("/opt/homebrew/bin/fish"), Some(Shell::Fish));
        assert_eq!(Shell::detect("-zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::detect("/bin/sh"), None);
    }

    #[test]
    fn scripts_install_idempotently() {
        let dir = std::env::temp_dir().join(format!("rift-si-test-{}", std::process::id()));
        install_scripts(&dir).unwrap();
        install_scripts(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("zsh/.zshenv")).unwrap(), ZSH_ZSHENV);
        assert!(dir.join("bash/rift.bash").exists());
        assert!(dir.join("fish/rift.fish").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fish_quoting() {
        assert_eq!(fish_quote(Path::new("/a b/it's")), "'/a b/it\\'s'");
    }
}
