//! Streaming chat-completion backend.
//!
//! * OpenAI-compatible `POST {api_url}/v1/chat/completions` with
//!   `"stream": true` -> Server-Sent Events (`data: {...}` lines, `[DONE]`).
//! * Ollama `POST {api_url}/api/chat` with `"stream": true` -> NDJSON.
//!
//! A background thread reads the response line by line, parses each line with
//! the pure functions [`parse_sse_line`] / [`parse_ndjson_line`] (unit-tested
//! with fixtures) and sends [`StreamEvent`]s over an mpsc channel, waking the
//! winit event loop after each one. Dropping the [`StreamHandle`] (or calling
//! [`StreamHandle::cancel`]) stops the thread at the next line.

use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use winit::event_loop::EventLoopProxy;

use super::json::{quote, Json};
use crate::ai::LlmConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// Next piece of assistant text.
    Delta(String),
    /// The model finished normally.
    Done,
    /// The request or stream failed.
    Error(String),
}

/// One line of a stream, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Delta(String),
    Done,
    Error(String),
    /// Keep-alive, comment, role-only chunk, unknown event...
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiMessage {
    pub role: &'static str,
    pub content: String,
}

impl ApiMessage {
    pub fn new(role: &'static str, content: impl Into<String>) -> Self {
        Self { role, content: content.into() }
    }
}

/// Receiving end of a running stream plus its cancel switch.
pub struct StreamHandle {
    pub rx: Receiver<StreamEvent>,
    cancel: Arc<AtomicBool>,
}

impl StreamHandle {
    /// Ask the worker thread to stop at the next line.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// A handle fed by hand (tests / replaying fixtures).
    #[cfg(test)]
    pub fn manual() -> (std::sync::mpsc::Sender<StreamEvent>, StreamHandle) {
        let (tx, rx) = channel();
        (tx, StreamHandle { rx, cancel: Arc::new(AtomicBool::new(false)) })
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

// ── Line parsers (pure) ──

/// Decode one SSE line from an OpenAI-compatible stream.
pub fn parse_sse_line(line: &str) -> Parsed {
    let line = line.trim();
    let Some(data) = line.strip_prefix("data:") else {
        // Blank separators, `event:` / `id:` / `: comment` lines.
        return Parsed::Skip;
    };
    let data = data.trim();
    if data == "[DONE]" {
        return Parsed::Done;
    }
    let Some(j) = Json::parse(data) else { return Parsed::Skip };
    if let Some(err) = j.get("error") {
        return Parsed::Error(error_text(err));
    }
    let choice = j.get("choices").and_then(|c| c.idx(0));
    let text = choice
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("content"))
        .and_then(Json::as_str)
        // Some servers stream whole `message`s instead of `delta`s.
        .or_else(|| choice.and_then(|c| c.get("message")).and_then(|m| m.get("content")).and_then(Json::as_str))
        .or_else(|| choice.and_then(|c| c.get("text")).and_then(Json::as_str));
    match text {
        Some(t) if !t.is_empty() => Parsed::Delta(t.to_string()),
        _ => Parsed::Skip,
    }
}

/// Decode one NDJSON line from Ollama's `/api/chat`.
pub fn parse_ndjson_line(line: &str) -> Parsed {
    let line = line.trim();
    if line.is_empty() {
        return Parsed::Skip;
    }
    let Some(j) = Json::parse(line) else { return Parsed::Skip };
    if let Some(err) = j.get("error") {
        return Parsed::Error(error_text(err));
    }
    let text = j.get("message").and_then(|m| m.get("content")).and_then(Json::as_str).unwrap_or("");
    if j.get("done").and_then(Json::as_bool) == Some(true) {
        // The final object normally carries an empty content; keep any text anyway.
        return if text.is_empty() { Parsed::Done } else { Parsed::Delta(text.to_string()) };
    }
    if text.is_empty() {
        Parsed::Skip
    } else {
        Parsed::Delta(text.to_string())
    }
}

fn error_text(err: &Json) -> String {
    match err {
        Json::Str(s) => s.clone(),
        other => other
            .get("message")
            .and_then(Json::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "unknown error".into()),
    }
}

/// Best-effort message out of an HTTP error body.
fn error_from_body(status: u16, body: &str) -> String {
    let msg = Json::parse(body.trim())
        .and_then(|j| j.get("error").map(error_text).or_else(|| j.get("message").and_then(Json::as_str).map(str::to_string)))
        .unwrap_or_else(|| crate::ui::trunc(body.trim(), 200).to_string());
    if msg.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {msg}")
    }
}

// ── Request construction (pure) ──

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamRequest {
    pub url: String,
    pub body: String,
    pub bearer: Option<String>,
    pub ndjson: bool,
}

pub fn build_request(config: &LlmConfig, messages: &[ApiMessage]) -> StreamRequest {
    let msgs = messages
        .iter()
        .map(|m| format!(r#"{{"role":"{}","content":{}}}"#, m.role, quote(&m.content)))
        .collect::<Vec<_>>()
        .join(",");
    let base = config.api_url.trim_end_matches('/');
    if config.provider == "ollama" {
        StreamRequest {
            url: format!("{base}/api/chat"),
            body: format!(r#"{{"model":{},"messages":[{}],"stream":true}}"#, quote(&config.model), msgs),
            bearer: None,
            ndjson: true,
        }
    } else {
        StreamRequest {
            url: format!("{base}/v1/chat/completions"),
            body: format!(
                r#"{{"model":{},"messages":[{}],"temperature":0.7,"stream":true}}"#,
                quote(&config.model),
                msgs
            ),
            bearer: config.api_key.clone(),
            ndjson: false,
        }
    }
}

// ── Worker ──

/// Start streaming. `build` runs on the worker thread (so slow context
/// collection such as `git branch` never blocks the UI) and yields the
/// messages to send. The event loop is woken after every event.
pub fn spawn(
    config: &LlmConfig,
    build: impl FnOnce() -> Vec<ApiMessage> + Send + 'static,
    proxy: EventLoopProxy<()>,
) -> StreamHandle {
    spawn_with_wake(config, build, move || {
        let _ = proxy.send_event(());
    })
}

pub fn spawn_with_wake(
    config: &LlmConfig,
    build: impl FnOnce() -> Vec<ApiMessage> + Send + 'static,
    wake: impl Fn() + Send + 'static,
) -> StreamHandle {
    let config = config.clone();
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    std::thread::spawn(move || {
        let emit = |ev: StreamEvent| -> bool {
            let ok = tx.send(ev).is_ok();
            wake();
            ok
        };
        let req = build_request(&config, &build());
        if flag.load(Ordering::Relaxed) {
            return;
        }
        if let Err(e) = run(&req, &flag, &emit) {
            if !flag.load(Ordering::Relaxed) {
                emit(StreamEvent::Error(e));
            }
        }
    });
    StreamHandle { rx, cancel }
}

fn run(req: &StreamRequest, cancel: &AtomicBool, emit: &dyn Fn(StreamEvent) -> bool) -> Result<(), String> {
    let mut builder = ureq::post(&req.url).header("Content-Type", "application/json");
    if req.ndjson {
        builder = builder.header("Accept", "application/x-ndjson");
    } else {
        builder = builder.header("Accept", "text/event-stream");
    }
    if let Some(key) = &req.bearer {
        builder = builder.header("Authorization", &format!("Bearer {key}"));
    }
    let resp = builder
        .config()
        .http_status_as_error(false)
        .build()
        .send(req.body.as_bytes())
        .map_err(|e| format!("Request failed: {e}"))?;

    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let body = resp.into_body().read_to_string().unwrap_or_default();
        return Err(error_from_body(status, &body));
    }

    let reader = BufReader::new(resp.into_body().into_reader());
    pump(reader, req.ndjson, cancel, emit)
}

/// Read lines from `reader`, forwarding parsed events until done / cancelled.
/// Split out of [`run`] so it can be exercised with in-memory fixtures.
pub fn pump<R: BufRead>(
    reader: R,
    ndjson: bool,
    cancel: &AtomicBool,
    emit: &dyn Fn(StreamEvent) -> bool,
) -> Result<(), String> {
    for line in reader.lines() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let line = line.map_err(|e| format!("Stream interrupted: {e}"))?;
        let parsed = if ndjson { parse_ndjson_line(&line) } else { parse_sse_line(&line) };
        match parsed {
            Parsed::Delta(t) => {
                if !emit(StreamEvent::Delta(t)) {
                    return Ok(()); // receiver dropped
                }
            }
            Parsed::Done => {
                emit(StreamEvent::Done);
                return Ok(());
            }
            Parsed::Error(e) => return Err(e),
            Parsed::Skip => {}
        }
    }
    // Stream ended without an explicit terminator (some servers do this).
    emit(StreamEvent::Done);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const SSE: &str = r#"data: {"id":"c1","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}

: keep-alive
data: {"id":"c1","choices":[{"index":0,"delta":{"content":"Use "},"finish_reason":null}]}

data: {"id":"c1","choices":[{"index":0,"delta":{"content":"`ls 中`"},"finish_reason":null}]}

data: {"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]
"#;

    #[test]
    fn sse_deltas_and_done() {
        let parsed: Vec<Parsed> = SSE.lines().map(parse_sse_line).collect();
        let deltas: Vec<_> = parsed.iter().filter_map(|p| if let Parsed::Delta(t) = p { Some(t.as_str()) } else { None }).collect();
        assert_eq!(deltas, vec!["Use ", "`ls 中`"]);
        assert_eq!(parsed.last(), Some(&Parsed::Done));
    }

    #[test]
    fn sse_skips_noise_and_surfaces_errors() {
        assert_eq!(parse_sse_line(""), Parsed::Skip);
        assert_eq!(parse_sse_line("event: ping"), Parsed::Skip);
        assert_eq!(parse_sse_line("data: not json"), Parsed::Skip);
        assert_eq!(parse_sse_line(r#"data: {"choices":[{"delta":{"content":null}}]}"#), Parsed::Skip);
        assert_eq!(parse_sse_line(r#"data: {"choices":[{"delta":{"reasoning_content":"hm"}}]}"#), Parsed::Skip);
        assert_eq!(
            parse_sse_line(r#"data: {"error":{"message":"rate limited","type":"x"}}"#),
            Parsed::Error("rate limited".into())
        );
        // No space after the colon is legal SSE.
        assert_eq!(parse_sse_line(r#"data:{"choices":[{"delta":{"content":"x"}}]}"#), Parsed::Delta("x".into()));
    }

    const NDJSON: &str = r#"{"model":"llama3.2","created_at":"t","message":{"role":"assistant","content":"Hel"},"done":false}
{"model":"llama3.2","created_at":"t","message":{"role":"assistant","content":"lo\n"},"done":false}
{"model":"llama3.2","created_at":"t","message":{"role":"assistant","content":""},"done":true,"total_duration":5}
"#;

    #[test]
    fn ndjson_deltas_and_done() {
        let parsed: Vec<Parsed> = NDJSON.lines().map(parse_ndjson_line).collect();
        assert_eq!(parsed[0], Parsed::Delta("Hel".into()));
        assert_eq!(parsed[1], Parsed::Delta("lo\n".into()));
        assert_eq!(parsed[2], Parsed::Done);
        assert_eq!(parse_ndjson_line(r#"{"error":"model not found"}"#), Parsed::Error("model not found".into()));
        assert_eq!(parse_ndjson_line("  "), Parsed::Skip);
    }

    fn collect(input: &str, ndjson: bool, cancel: bool) -> Vec<StreamEvent> {
        let out = RefCell::new(Vec::new());
        let flag = AtomicBool::new(cancel);
        let r = pump(input.as_bytes(), ndjson, &flag, &|e| {
            out.borrow_mut().push(e);
            true
        });
        let mut v = out.into_inner();
        if let Err(e) = r {
            v.push(StreamEvent::Error(e));
        }
        v
    }

    #[test]
    fn pump_streams_sse_to_completion() {
        let ev = collect(SSE, false, false);
        assert_eq!(
            ev,
            vec![StreamEvent::Delta("Use ".into()), StreamEvent::Delta("`ls 中`".into()), StreamEvent::Done]
        );
    }

    #[test]
    fn pump_streams_ndjson_and_handles_missing_terminator() {
        let ev = collect(NDJSON, true, false);
        assert_eq!(ev.last(), Some(&StreamEvent::Done));
        assert_eq!(ev.len(), 3);
        let ev = collect(r#"{"message":{"content":"a"},"done":false}"#, true, false);
        assert_eq!(ev, vec![StreamEvent::Delta("a".into()), StreamEvent::Done]);
    }

    #[test]
    fn pump_stops_when_cancelled_and_reports_errors() {
        assert!(collect(SSE, false, true).is_empty());
        let ev = collect("data: {\"error\":\"boom\"}\n", false, false);
        assert_eq!(ev, vec![StreamEvent::Error("boom".into())]);
    }

    #[test]
    fn request_bodies_per_provider() {
        let mut cfg = LlmConfig::default();
        cfg.provider = "ollama".into();
        cfg.api_url = "http://localhost:11434/".into();
        let msgs = vec![ApiMessage::new("system", "be \"brief\""), ApiMessage::new("user", "hi\n中")];
        let r = build_request(&cfg, &msgs);
        assert_eq!(r.url, "http://localhost:11434/api/chat");
        assert!(r.ndjson && r.bearer.is_none());
        let j = Json::parse(&r.body).unwrap();
        assert_eq!(j.get("stream").and_then(Json::as_bool), Some(true));
        assert_eq!(j.get("messages").unwrap().idx(1).unwrap().get("content").unwrap().as_str(), Some("hi\n中"));

        cfg.provider = "openai".into();
        cfg.api_url = "https://api.deepseek.com".into();
        cfg.api_key = Some("sk-x".into());
        let r = build_request(&cfg, &msgs);
        assert_eq!(r.url, "https://api.deepseek.com/v1/chat/completions");
        assert_eq!(r.bearer.as_deref(), Some("sk-x"));
        assert!(!r.ndjson);
        let j = Json::parse(&r.body).unwrap();
        assert_eq!(j.get("stream").and_then(Json::as_bool), Some(true));
        assert_eq!(j.get("messages").unwrap().idx(0).unwrap().get("content").unwrap().as_str(), Some("be \"brief\""));
    }

    #[test]
    fn http_error_bodies_are_summarised() {
        assert_eq!(error_from_body(401, r#"{"error":{"message":"bad key"}}"#), "HTTP 401: bad key");
        assert_eq!(error_from_body(500, ""), "HTTP 500");
        assert!(error_from_body(502, "<html>gateway</html>").starts_with("HTTP 502: <html>"));
    }
}
