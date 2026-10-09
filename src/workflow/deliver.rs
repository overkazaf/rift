//! Getting a prompt into an agent: as a command-line argument when the CLI
//! supports it (`claude "task"`, `codex "task"`, `gemini -i "task"`), else typed
//! into its input box once it is ready. Which flag a CLI takes is read from its
//! own `--help` at runtime, never assumed.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::agents::AgentKind;

/// How a prompt reaches an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// `<bin> "<prompt>"`.
    Positional,
    /// `<bin> <flag> "<prompt>"` (the flag keeps the session interactive).
    Flag(&'static str),
    /// Type it into the running agent.
    Typed,
}

/// Prompts longer than this, or with line breaks, are typed instead of passed
/// on a shell command line.
pub const MAX_ARG_CHARS: usize = 1500;

/// Read `--help` output: is there an interactive initial-prompt argument?
pub fn parse_help(kind: AgentKind, help: &str) -> Delivery {
    let h = help.to_lowercase();
    match kind {
        AgentKind::Gemini => {
            if h.contains("--prompt-interactive") {
                Delivery::Flag("-i")
            } else {
                Delivery::Typed
            }
        }
        AgentKind::ClaudeCode | AgentKind::Codex | AgentKind::OpenCode | AgentKind::CursorAgent => {
            // `claude [options] [command] [prompt]`, `codex [OPTIONS] [PROMPT]`
            let usage = h.lines().find(|l| l.trim_start().starts_with("usage:")).unwrap_or("");
            if usage.contains("[prompt]") || h.contains("[prompt]") && usage.contains(kind.binary()) {
                Delivery::Positional
            } else {
                Delivery::Typed
            }
        }
        // Aider takes --message (which exits afterwards) rather than an interactive start prompt.
        AgentKind::Aider => Delivery::Typed,
    }
}

/// Run `<bin> --help` (blocking, at most `timeout`) and decide. Unknown or
/// failing CLIs fall back to typing.
pub fn probe(kind: AgentKind, timeout: Duration) -> Delivery {
    let Some(bin) = crate::agents::launch::find_binary(kind.binary()) else { return Delivery::Typed };
    let Ok(mut child) = Command::new(bin).arg("--help").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() else {
        return Delivery::Typed;
    };
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut s = String::new();
        if let Some(o) = out.as_mut() {
            let mut b = Vec::new();
            let _ = o.read_to_end(&mut b);
            s.push_str(&String::from_utf8_lossy(&b));
        }
        if let Some(e) = err.as_mut() {
            let mut b = Vec::new();
            let _ = e.read_to_end(&mut b);
            s.push('\n');
            s.push_str(&String::from_utf8_lossy(&b));
        }
        s
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Delivery::Typed;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return Delivery::Typed,
        }
    }
    parse_help(kind, &reader.join().unwrap_or_default())
}

/// Single-quote for a POSIX-style shell (bash, zsh, and fish all read `'\''` the same way).
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Can `prompt` go on a command line at all?
pub fn fits_argument(prompt: &str) -> bool {
    !prompt.is_empty() && prompt.chars().count() <= MAX_ARG_CHARS && !prompt.chars().any(|c| c.is_control())
}

/// The command to type for `kind` plus whether the prompt is inside it.
/// `(command, delivered_as_argument)`.
pub fn launch_with_prompt(kind: AgentKind, delivery: Delivery, prompt: &str) -> (String, bool) {
    let bin = crate::agents::launch::launch_command(kind);
    if !fits_argument(prompt) {
        return (bin, false);
    }
    match delivery {
        Delivery::Positional => (format!("{bin} {}", shell_quote(prompt)), true),
        Delivery::Flag(f) => (format!("{bin} {f} {}", shell_quote(prompt)), true),
        Delivery::Typed => (bin, false),
    }
}

// ───────────────────────────── typed delivery ─────────────────────────────

/// A prompt waiting for an agent to become ready for input.
#[derive(Clone, Debug)]
pub struct PendingSend {
    pub uid: usize,
    pub text: String,
    pub since: Instant,
    pub tag: SendTag,
}

/// Who to tell once the text went in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendTag {
    /// A best-of-N candidate's task.
    Candidate { run: u64 },
    Writer { run: u64 },
    Reviewer { run: u64 },
    Fixer { run: u64 },
    /// A queued task (the queue already popped it).
    Queue,
}

/// How long a Starting -> Idle agent must have been settled before typing.
pub const SETTLE: Duration = Duration::from_millis(700);
/// Give up on an agent that never gets ready.
pub const GIVE_UP: Duration = Duration::from_secs(120);

/// What to do with a pending send right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ready {
    Wait,
    Send,
    GiveUp,
}

/// Decide from the session's state: type only into an agent that sits idle at its
/// input box and has been quiet for [`SETTLE`]. Approval prompts and working agents
/// are never typed into.
pub fn readiness(state: Option<crate::agents::AgentState>, state_age: Duration, waited: Duration) -> Ready {
    use crate::agents::AgentState as S;
    match state {
        Some(S::Idle) if state_age >= SETTLE => Ready::Send,
        Some(S::Done { .. }) | Some(S::Error) => Ready::GiveUp,
        _ if waited >= GIVE_UP => Ready::GiveUp,
        _ => Ready::Wait,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentState;

    const CLAUDE_HELP: &str = "Usage: claude [options] [command] [prompt]\n\nClaude Code - starts an interactive session by default, use -p/--print for\nnon-interactive output\n\nArguments:\n  prompt   Your prompt\n";
    const CODEX_HELP: &str = "Codex CLI\n\nUsage: codex [OPTIONS] [PROMPT]\n       codex [OPTIONS] <COMMAND> [ARGS]\n";
    const GEMINI_HELP: &str = "Usage: gemini [options] [command]\n\nPositionals:\n  query  Positional prompt. Defaults to one-shot; use -i/--prompt-interactive for interactive.\n\nOptions:\n  -p, --prompt  Prompt.\n  -i, --prompt-interactive  Execute the provided prompt and continue in interactive mode  [string]\n";
    const GEMINI_OLD: &str = "Usage: gemini [options]\n\nOptions:\n  -p, --prompt  Prompt\n";

    #[test]
    fn help_output_decides_the_delivery() {
        assert_eq!(parse_help(AgentKind::ClaudeCode, CLAUDE_HELP), Delivery::Positional);
        assert_eq!(parse_help(AgentKind::Codex, CODEX_HELP), Delivery::Positional);
        assert_eq!(parse_help(AgentKind::Gemini, GEMINI_HELP), Delivery::Flag("-i"));
        // An older Gemini without -i: type it (a positional prompt would run one-shot and exit).
        assert_eq!(parse_help(AgentKind::Gemini, GEMINI_OLD), Delivery::Typed);
        assert_eq!(parse_help(AgentKind::ClaudeCode, "Usage: claude [options]\n"), Delivery::Typed);
        assert_eq!(parse_help(AgentKind::Aider, "Usage: aider [prompt]"), Delivery::Typed);
        assert_eq!(parse_help(AgentKind::Codex, ""), Delivery::Typed);
    }

    #[test]
    fn quoting_survives_the_shell() {
        assert_eq!(shell_quote("fix it"), "'fix it'");
        assert_eq!(shell_quote("it's \"x\" $HOME `ls` !!"), "'it'\\''s \"x\" $HOME `ls` !!'");
        // Round trip through a real shell.
        for s in ["plain", "it's", "a 'b' \"c\" $d `e` \\f", "\u{4e2d}\u{6587} \u{2713}", "semi; rm -rf x && echo"] {
            let out = Command::new("/bin/sh").arg("-c").arg(format!("printf %s {}", shell_quote(s))).output().unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout), s);
        }
    }

    #[test]
    fn commands_per_delivery() {
        let (c, arg) = launch_with_prompt(AgentKind::ClaudeCode, Delivery::Positional, "add a retry");
        assert_eq!((c.as_str(), arg), ("claude 'add a retry'", true));
        let (c, arg) = launch_with_prompt(AgentKind::Gemini, Delivery::Flag("-i"), "it's done?");
        assert_eq!((c.as_str(), arg), ("gemini -i 'it'\\''s done?'", true));
        let (c, arg) = launch_with_prompt(AgentKind::Codex, Delivery::Typed, "x");
        assert_eq!((c.as_str(), arg), ("codex", false));
        // Multi-line and long prompts are typed, not put on the command line.
        let (c, arg) = launch_with_prompt(AgentKind::ClaudeCode, Delivery::Positional, "line one\nline two");
        assert_eq!((c.as_str(), arg), ("claude", false));
        let (c, arg) = launch_with_prompt(AgentKind::ClaudeCode, Delivery::Positional, &"x".repeat(MAX_ARG_CHARS + 1));
        assert_eq!((c.as_str(), arg), ("claude", false));
        let (c, arg) = launch_with_prompt(AgentKind::ClaudeCode, Delivery::Positional, "");
        assert_eq!((c.as_str(), arg), ("claude", false));
        assert!(!fits_argument("tab\there"));
    }

    #[test]
    fn readiness_waits_for_a_settled_idle_agent() {
        let ms = Duration::from_millis;
        assert_eq!(readiness(None, ms(0), ms(100)), Ready::Wait, "no session yet");
        assert_eq!(readiness(Some(AgentState::Starting), ms(5000), ms(5000)), Ready::Wait);
        assert_eq!(readiness(Some(AgentState::Idle), ms(100), ms(2000)), Ready::Wait, "still settling");
        assert_eq!(readiness(Some(AgentState::Idle), ms(800), ms(2000)), Ready::Send);
        assert_eq!(readiness(Some(AgentState::Working), ms(5000), ms(5000)), Ready::Wait);
        assert_eq!(readiness(Some(AgentState::WaitingForUser), ms(5000), ms(5000)), Ready::Wait, "never type into an approval menu");
        assert_eq!(readiness(Some(AgentState::Error), ms(0), ms(0)), Ready::GiveUp);
        assert_eq!(readiness(Some(AgentState::Done { exit: Some(0) }), ms(0), ms(0)), Ready::GiveUp);
        assert_eq!(readiness(Some(AgentState::Starting), ms(0), GIVE_UP), Ready::GiveUp);
    }
}
