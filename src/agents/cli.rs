//! `rift agent-event <state>`: tell the running Rift what an agent is doing.
//!
//! Meant for agent hooks. It connects to Rift's MCP socket (the same one
//! `rift mcp` uses) and sends a single JSON-RPC request, `rift/agent_event`.
//! The shell Rift started carries `$RIFT_PANE_ID`, so the event lands on the
//! right pane even with many agents running.
//!
//! ```text
//! rift agent-event working|waiting|done|idle|error
//!                  [--agent claude|codex|gemini|opencode|aider|cursor]
//!                  [--pane ID] [--message TEXT] [--cwd DIR] [--verbose] [JSON]
//! ```
//!
//! Hooks must never break the agent, so a missing Rift or socket is silent and
//! the exit code is 0; only a usage error exits 1. A hook's stdin JSON (Claude
//! Code) or a trailing JSON argument (Codex `notify`) supplies `cwd` and
//! `message`; with no state given, `hook_event_name` picks one.

use std::io::{BufRead, BufReader, Read, Write};
use std::time::Duration;

use super::inbox::METHOD;
use super::state::HookKind;
use super::AgentKind;
use crate::ai::chat::json::{quote, Json};

pub const USAGE: &str = "usage: rift agent-event <working|waiting|done|idle|error> [--agent NAME] [--pane ID] [--message TEXT] [--cwd DIR] [--verbose]";

/// Parsed command line.
#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub state: Option<HookKind>,
    pub agent: Option<AgentKind>,
    pub pane: Option<usize>,
    pub message: Option<String>,
    pub cwd: Option<String>,
    pub verbose: bool,
    pub socket: Option<String>,
    /// A positional JSON document (Codex `notify` appends one).
    pub json: Option<String>,
}

pub fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--agent" => {
                let v = value("--agent")?;
                a.agent = Some(AgentKind::parse(&v).ok_or_else(|| format!("unknown agent '{v}'"))?);
            }
            "--pane" => {
                let v = value("--pane")?;
                a.pane = Some(v.parse().map_err(|_| format!("bad pane id '{v}'"))?);
            }
            "--message" | "-m" => a.message = Some(value("--message")?),
            "--cwd" => a.cwd = Some(value("--cwd")?),
            "--socket" => a.socket = Some(value("--socket")?),
            "--verbose" | "-v" => a.verbose = true,
            "--help" | "-h" => return Err(USAGE.to_string()),
            s if s.starts_with('{') => a.json = Some(s.to_string()),
            s if s.starts_with('-') => return Err(format!("unknown option '{s}'")),
            s => match HookKind::parse(s) {
                Some(k) if a.state.is_none() => a.state = Some(k),
                Some(_) => return Err(format!("more than one state ('{s}')")),
                None => return Err(format!("unknown state '{s}'")),
            },
        }
    }
    Ok(a)
}

/// State implied by a Claude Code hook name.
pub fn state_for_hook_name(name: &str) -> Option<HookKind> {
    match name {
        "Notification" => Some(HookKind::Waiting),
        "Stop" | "SubagentStop" => Some(HookKind::Done),
        "UserPromptSubmit" => Some(HookKind::Working),
        "SessionEnd" => Some(HookKind::Idle),
        _ => None,
    }
}

/// Fill `cwd` / `message` / state from a hook payload.
pub fn apply_payload(a: &mut Args, payload: &str) {
    let Some(j) = Json::parse(payload.trim()) else { return };
    if a.cwd.is_none() {
        a.cwd = j.get("cwd").and_then(Json::as_str).map(str::to_string);
    }
    if a.message.is_none() {
        a.message = ["message", "last-assistant-message", "last_assistant_message", "reason"]
            .iter()
            .find_map(|k| j.get(k).and_then(Json::as_str))
            .map(str::to_string);
    }
    if a.state.is_none() {
        if let Some(k) = j.get("hook_event_name").and_then(Json::as_str).and_then(state_for_hook_name) {
            a.state = Some(k);
        }
        // Codex notify payload: {"type":"agent-turn-complete", ...}
        if a.state.is_none() && j.get("type").and_then(Json::as_str) == Some("agent-turn-complete") {
            a.state = Some(HookKind::Done);
            a.agent.get_or_insert(AgentKind::Codex);
        }
    }
}

fn state_name(k: HookKind) -> &'static str {
    match k {
        HookKind::Working => "working",
        HookKind::Waiting => "waiting",
        HookKind::Done => "done",
        HookKind::Idle => "idle",
        HookKind::Error => "error",
    }
}

/// The JSON-RPC request line (no trailing newline).
pub fn build_request(a: &Args, state: HookKind, pane_from_env: Option<usize>) -> String {
    let mut p = format!(r#"{{"state":{}"#, quote(state_name(state)));
    if let Some(id) = a.pane.or(pane_from_env) {
        p.push_str(&format!(r#","pane_id":{id}"#));
    }
    if let Some(k) = a.agent {
        p.push_str(&format!(r#","agent":{}"#, quote(k.slug())));
    }
    if let Some(m) = a.message.as_deref().filter(|m| !m.is_empty()) {
        let m: String = m.chars().take(200).collect();
        p.push_str(&format!(r#","message":{}"#, quote(&m)));
    }
    if let Some(c) = a.cwd.as_deref().filter(|c| !c.is_empty()) {
        p.push_str(&format!(r#","cwd":{}"#, quote(c)));
    }
    p.push('}');
    format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{METHOD}","params":{p}}}"#)
}

/// stdin payload of a hook, read with a short timeout so a terminal or an
/// unclosed pipe can never hang the hook.
#[cfg(unix)]
fn read_stdin_payload() -> Option<String> {
    // SAFETY: isatty on fd 0 has no preconditions.
    if unsafe { libc::isatty(0) } == 1 {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::stdin().lock().take(64 * 1024).read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    rx.recv_timeout(Duration::from_millis(300)).ok().filter(|s| !s.trim().is_empty())
}

#[cfg(not(unix))]
fn read_stdin_payload() -> Option<String> {
    None
}

/// Entry point: `args` are the arguments after `agent-event`. Returns the exit code.
#[cfg(unix)]
pub fn run(args: &[String]) -> i32 {
    let mut a = match parse_args(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("rift agent-event: {e}\n{USAGE}");
            return 1;
        }
    };
    if let Some(j) = a.json.clone() {
        apply_payload(&mut a, &j);
    } else if let Some(p) = read_stdin_payload() {
        apply_payload(&mut a, &p);
    }
    if a.cwd.is_none() {
        a.cwd = std::env::current_dir().ok().map(|p| p.to_string_lossy().into_owned());
    }
    let Some(state) = a.state else {
        eprintln!("rift agent-event: no state given and the hook payload names none\n{USAGE}");
        return 1;
    };
    let env_pane = std::env::var("RIFT_PANE_ID").ok().and_then(|v| v.trim().parse().ok());
    let line = build_request(&a, state, env_pane);
    let path = a.socket.as_deref().map(std::path::PathBuf::from).unwrap_or_else(crate::mcp::socket_path);
    match send(&path, &line) {
        Ok(reply) => {
            if a.verbose {
                eprintln!("rift agent-event: {}", reply.trim());
            }
        }
        Err(e) => {
            if a.verbose {
                eprintln!("rift agent-event: {}", crate::mcp::bridge::not_running_message(&path, &e.to_string()));
            }
        }
    }
    0
}

#[cfg(not(unix))]
pub fn run(_args: &[String]) -> i32 {
    eprintln!("rift agent-event needs a Unix platform");
    0
}

#[cfg(unix)]
fn send(path: &std::path::Path, line: &str) -> std::io::Result<String> {
    use std::os::unix::net::UnixStream;
    let mut s = UnixStream::connect(path)?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    s.set_write_timeout(Some(Duration::from_secs(2)))?;
    s.write_all(line.as_bytes())?;
    s.write_all(b"\n")?;
    s.flush()?;
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply)?;
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_flags_and_state() {
        let a = parse_args(&v(&["waiting", "--agent", "claude", "--pane", "7", "-m", "needs you", "--verbose"])).unwrap();
        assert_eq!(a.state, Some(HookKind::Waiting));
        assert_eq!(a.agent, Some(AgentKind::ClaudeCode));
        assert_eq!(a.pane, Some(7));
        assert_eq!(a.message.as_deref(), Some("needs you"));
        assert!(a.verbose);
    }

    #[test]
    fn usage_errors() {
        assert!(parse_args(&v(&["bogus"])).is_err());
        assert!(parse_args(&v(&["done", "waiting"])).is_err());
        assert!(parse_args(&v(&["--pane"])).is_err());
        assert!(parse_args(&v(&["--pane", "x"])).is_err());
        assert!(parse_args(&v(&["--agent", "emacs"])).is_err());
        assert!(parse_args(&v(&["--wat"])).is_err());
    }

    #[test]
    fn codex_notify_json_is_a_positional() {
        let a = parse_args(&v(&["done", "--agent", "codex", r#"{"type":"agent-turn-complete","cwd":"/w","last-assistant-message":"All green"}"#])).unwrap();
        let mut a = a;
        let j = a.json.clone().unwrap();
        apply_payload(&mut a, &j);
        assert_eq!(a.cwd.as_deref(), Some("/w"));
        assert_eq!(a.message.as_deref(), Some("All green"));
        assert_eq!(a.state, Some(HookKind::Done));
    }

    #[test]
    fn claude_payload_infers_state_from_hook_name() {
        let mut a = Args::default();
        apply_payload(&mut a, r#"{"hook_event_name":"Notification","cwd":"/p","message":"Claude needs your permission to use Bash","session_id":"s"}"#);
        assert_eq!(a.state, Some(HookKind::Waiting));
        assert_eq!(a.cwd.as_deref(), Some("/p"));
        assert!(a.message.as_deref().unwrap().contains("permission"));
        let mut a = Args::default();
        apply_payload(&mut a, r#"{"hook_event_name":"Stop"}"#);
        assert_eq!(a.state, Some(HookKind::Done));
        // An explicit state is never overridden.
        let mut a = Args { state: Some(HookKind::Idle), ..Args::default() };
        apply_payload(&mut a, r#"{"hook_event_name":"Stop"}"#);
        assert_eq!(a.state, Some(HookKind::Idle));
        apply_payload(&mut a, "not json");
    }

    #[test]
    fn request_is_valid_json_rpc() {
        let a = Args { message: Some("say \"hi\"\n".into()), cwd: Some("/a b".into()), agent: Some(AgentKind::Gemini), ..Args::default() };
        let line = build_request(&a, HookKind::Waiting, Some(4));
        assert!(!line.contains('\n'));
        let j = Json::parse(&line).expect("valid json");
        assert_eq!(j.get("method").and_then(Json::as_str), Some(METHOD));
        let p = j.get("params").unwrap();
        assert_eq!(p.get("state").and_then(Json::as_str), Some("waiting"));
        assert_eq!(p.get("pane_id").and_then(Json::as_f64), Some(4.0));
        assert_eq!(p.get("agent").and_then(Json::as_str), Some("gemini"));
        assert_eq!(p.get("cwd").and_then(Json::as_str), Some("/a b"));
        // And the server side accepts exactly what the client sends.
        let ev = super::super::inbox::parse_params(p).unwrap();
        assert_eq!(ev.state, HookKind::Waiting);
        assert_eq!(ev.pane_id, Some(4));
        // --pane beats the environment.
        let a = Args { pane: Some(9), ..Args::default() };
        assert!(build_request(&a, HookKind::Done, Some(4)).contains(r#""pane_id":9"#));
    }

    #[cfg(unix)]
    #[test]
    fn talks_to_a_socket_and_survives_a_missing_one() {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("rift-cli-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("s.sock");
        let _ = std::fs::remove_file(&path);
        let l = UnixListener::bind(&path).unwrap();
        let h = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            let mut s = s;
            s.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n").unwrap();
            line
        });
        let a = Args::default();
        let req = build_request(&a, HookKind::Done, Some(2));
        let reply = send(&path, &req).unwrap();
        assert!(reply.contains("result"));
        assert!(h.join().unwrap().contains(METHOD));
        assert!(send(&dir.join("missing.sock"), &req).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
