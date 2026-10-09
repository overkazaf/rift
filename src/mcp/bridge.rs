//! `rift mcp`: stdio <-> Rift's MCP socket.
//!
//! Claude Code / Codex launch MCP servers as subprocesses speaking JSON-RPC on
//! stdin/stdout. Rift is a GUI app, so this tiny mode just connects to the
//! running instance's socket and pipes lines both ways. If Rift is not
//! running it still answers every request with a clear JSON-RPC error, so the
//! client shows a useful message instead of a bare "connection closed".

use std::io::{self, BufRead, Write};
use std::path::Path;

use super::protocol::{error_response, ser};
use crate::ai::chat::json::Json;

/// JSON-RPC error code used when Rift is not reachable.
pub const NOT_RUNNING: i32 = -32000;

pub fn not_running_message(path: &Path, why: &str) -> String {
    format!(
        "Rift is not running or its MCP server is off (could not connect to {}: {why}). Start the Rift app, make sure [mcp] enabled = true in ~/.config/rift/config.toml, then retry.",
        path.display()
    )
}

/// Reply to one client line while Rift is unreachable: an error for requests,
/// nothing for notifications.
pub fn offline_reply(line: &str, message: &str) -> Option<String> {
    let msg = Json::parse(line.trim())?;
    msg.get("method")?;
    let id = match msg.get("id") {
        Some(v @ (Json::Num(_) | Json::Str(_))) => ser(v),
        _ => return None,
    };
    Some(error_response(&id, NOT_RUNNING, message))
}

/// Entry point for `rift mcp`; returns the process exit code.
#[cfg(unix)]
pub fn run() -> i32 {
    use std::os::unix::net::UnixStream;
    let path = super::socket_path();
    match UnixStream::connect(&path) {
        Ok(sock) => pipe(sock),
        Err(e) => {
            let msg = not_running_message(&path, &e.to_string());
            eprintln!("rift mcp: {msg}");
            answer_offline(io::stdin().lock(), io::stdout().lock(), &msg);
            1
        }
    }
}

#[cfg(not(unix))]
pub fn run() -> i32 {
    eprintln!("rift mcp: the MCP bridge needs a Unix platform");
    1
}

/// Answer every request on `input` with the "not running" error until EOF.
pub fn answer_offline(input: impl BufRead, mut out: impl Write, message: &str) {
    for line in input.lines() {
        let Ok(line) = line else { break };
        if let Some(r) = offline_reply(&line, message) {
            if writeln!(out, "{r}").and_then(|_| out.flush()).is_err() {
                break;
            }
        }
    }
}

#[cfg(unix)]
fn pipe(sock: std::os::unix::net::UnixStream) -> i32 {
    use std::io::{BufReader, Read};
    let Ok(mut to_rift) = sock.try_clone() else {
        eprintln!("rift mcp: cannot duplicate the socket");
        return 1;
    };
    let stdin_eof = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let eof_flag = stdin_eof.clone();
    // stdin -> socket. Forward whole lines; on EOF half-close so Rift sees it.
    let up = std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut stdin = stdin.lock();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match stdin.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if buf.last() != Some(&b'\n') {
                        buf.push(b'\n');
                    }
                    if to_rift.write_all(&buf).and_then(|_| to_rift.flush()).is_err() {
                        break;
                    }
                }
            }
        }
        eof_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = to_rift.shutdown(std::net::Shutdown::Write);
    });
    // socket -> stdout
    let mut from_rift = BufReader::new(sock);
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut line = Vec::new();
    let mut code = 0;
    loop {
        line.clear();
        match from_rift.by_ref().read_until(b'\n', &mut line) {
            Ok(0) => {
                // Rift closed first (quit/crash), or our stdin hit EOF and it finished replying.
                if !stdin_eof.load(std::sync::atomic::Ordering::SeqCst) {
                    eprintln!("rift mcp: Rift closed the connection");
                    code = 1;
                }
                break;
            }
            Ok(_) => {
                if stdout.write_all(&line).and_then(|_| stdout.flush()).is_err() {
                    break;
                }
            }
            Err(e) => {
                eprintln!("rift mcp: socket error: {e}");
                code = 1;
                break;
            }
        }
    }
    if code == 0 {
        let _ = up.join();
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_reply_errors_requests_and_ignores_notifications() {
        let msg = not_running_message(Path::new("/x/mcp.sock"), "No such file");
        let r = offline_reply(r#"{"jsonrpc":"2.0","id":7,"method":"initialize","params":{}}"#, &msg).unwrap();
        let j = Json::parse(&r).unwrap();
        assert_eq!(j.get("id").and_then(Json::as_f64), Some(7.0));
        let e = j.get("error").unwrap();
        assert_eq!(e.get("code").and_then(Json::as_f64), Some(NOT_RUNNING as f64));
        let text = e.get("message").and_then(Json::as_str).unwrap();
        assert!(text.contains("Rift is not running") && text.contains("/x/mcp.sock"), "{text}");
        let r = offline_reply(r#"{"jsonrpc":"2.0","id":"a","method":"ping"}"#, &msg).unwrap();
        assert!(r.contains(r#""id":"a""#));
        assert!(offline_reply(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, &msg).is_none());
        assert!(offline_reply("garbage", &msg).is_none());
        assert!(offline_reply(r#"{"id":1,"result":{}}"#, &msg).is_none());
    }

    #[test]
    fn answer_offline_replies_per_request_until_eof() {
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n";
        let mut out = Vec::new();
        answer_offline(&input[..], &mut out, "down");
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains(r#""id":1"#) && lines[1].contains(r#""id":2"#));
    }
}
