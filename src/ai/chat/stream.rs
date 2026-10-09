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
use std::sync::mpsc::{channel, sync_channel, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

    /// A handle with no worker behind it: the chat UI shows its "answering"
    /// state. Used by the headless screenshot renderer, which never polls it.
    pub fn detached() -> StreamHandle {
        let (_tx, rx) = channel();
        StreamHandle { rx, cancel: Arc::new(AtomicBool::new(false)) }
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

/// Best-effort, human-readable message for a failed HTTP exchange: the
/// provider's own message when the body has one, plus a hint for the usual
/// suspects (bad key, rate limit).
pub fn http_error(status: u16, body: &str, retry_after: Option<&str>) -> String {
    let parsed = Json::parse(body.trim()).and_then(|j| {
        j.get("error")
            .map(error_text)
            .or_else(|| j.get("message").and_then(Json::as_str).map(str::to_string))
            .or_else(|| j.get("detail").and_then(Json::as_str).map(str::to_string))
    });
    let msg = parsed.unwrap_or_else(|| snippet(body, 200));
    let hint = match status {
        401 | 403 => Some("check the API key in [llm]".to_string()),
        429 => Some(match retry_after.map(str::trim).filter(|v| !v.is_empty()) {
            Some(v) if v.chars().all(|c| c.is_ascii_digit()) => format!("rate limited, retry in {v}s"),
            Some(v) => format!("rate limited, retry after {v}"),
            None => "rate limited, wait a moment and retry".to_string(),
        }),
        _ => None,
    };
    let msg = match (msg.is_empty(), status) {
        (true, 401 | 403) => "Invalid API key".to_string(),
        (true, 429) => "Too many requests".to_string(),
        _ => msg,
    };
    let mut out = if msg.is_empty() { format!("HTTP {status}") } else { format!("HTTP {status}: {msg}") };
    if let Some(h) = hint {
        out.push_str(&format!(" ({h})"));
    }
    out
}

/// Single-line, ANSI-free, char-bounded excerpt of arbitrary text.
pub fn snippet(s: &str, max_chars: usize) -> String {
    let clean = super::guard::strip_ansi(s);
    let one = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > max_chars {
        let mut t: String = one.chars().take(max_chars.saturating_sub(1)).collect();
        t.push('\u{2026}');
        t
    } else {
        one
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
        // Final chokepoint before the wire: no ANSI/control bytes, no secrets.
        .map(|m| format!(r#"{{"role":"{}","content":{}}}"#, m.role, quote(&super::guard::scrub_outbound(&m.content))))
        .collect::<Vec<_>>()
        .join(",");
    let base = config.api_url.trim_end_matches('/');
    if config.provider == "ollama" {
        StreamRequest {
            url: format!("{base}/api/chat"),
            body: format!(
                r#"{{"model":{},"messages":[{}],"stream":true,"options":{{"num_ctx":{}}}}}"#,
                quote(&config.model),
                msgs,
                crate::ai::local::ollama_num_ctx()
            ),
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

// ── Timeouts ──

/// How long a stream may stay silent before it is reported as stalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// Until the HTTP response headers arrive. Hosted APIs answer at once; a
    /// local Ollama may need to load the model first.
    pub response: Duration,
    /// Until the first data line once the response started (queueing, prompt eval).
    pub first_token: Duration,
    /// Between data lines once the answer has started. SSE keep-alive
    /// comments do not count.
    pub idle: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self { response: Duration::from_secs(15), first_token: Duration::from_secs(30), idle: Duration::from_secs(12) }
    }
}

impl Timeouts {
    /// Defaults, overridable with `RIFT_AI_FIRST_TOKEN_SECS` (also raises the
    /// header wait to at least that) and `RIFT_AI_IDLE_SECS`. `ollama` allows
    /// a long header wait for model loading.
    pub fn from_env(ollama: bool) -> Self {
        let mut t = Self::default();
        if ollama {
            t.response = Duration::from_secs(60);
        }
        let get = |k: &str| std::env::var(k).ok().and_then(|v| v.trim().parse::<u64>().ok()).filter(|n| *n > 0);
        if let Some(n) = get("RIFT_AI_FIRST_TOKEN_SECS") {
            t.first_token = Duration::from_secs(n);
            t.response = t.response.max(t.first_token);
        }
        if let Some(n) = get("RIFT_AI_IDLE_SECS") {
            t.idle = Duration::from_secs(n);
        }
        t
    }
}

/// A single stream line may not exceed this (a server that never sends a
/// newline must not eat memory).
const MAX_LINE_BYTES: usize = 1 << 20;

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
        crate::ai::local::usage::record(crate::ai::local::Feature::Chat, &config, &req.body);
        if let Err(e) = run(&req, &flag, &emit, Timeouts::from_env(req.ndjson)) {
            if !flag.load(Ordering::Relaxed) {
                emit(StreamEvent::Error(e));
            }
        }
    });
    StreamHandle { rx, cancel }
}

fn run(req: &StreamRequest, cancel: &AtomicBool, emit: &dyn Fn(StreamEvent) -> bool, to: Timeouts) -> Result<(), String> {
    let mut builder = ureq::post(&req.url).header("Content-Type", "application/json");
    if req.ndjson {
        builder = builder.header("Accept", "application/x-ndjson");
    } else {
        builder = builder.header("Accept", "text/event-stream");
    }
    if let Some(key) = &req.bearer {
        builder = builder.header("Authorization", &format!("Bearer {key}"));
    }
    let mut cfg = builder
        .config()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(to.response))
        // Upper bound for one whole answer; the idle timer handles stalls.
        .timeout_global(Some(Duration::from_secs(900)));
    if crate::ai::local::usage::is_loopback_url(&req.url) {
        // A proxy from the environment must never see (or relay) local traffic.
        cfg = cfg.proxy(None);
    }
    let resp = cfg
        .build()
        .send(req.body.as_bytes())
        .map_err(|e| request_error(&e.to_string(), to))?;

    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let retry = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).map(str::to_string);
        let body = resp.into_body().with_config().limit(64 * 1024).read_to_string().unwrap_or_default();
        return Err(http_error(status, &body, retry.as_deref()));
    }

    let reader = BufReader::new(resp.into_body().into_reader());
    pump_timed(reader, req.ndjson, cancel, emit, to)
}

fn request_error(e: &str, to: Timeouts) -> String {
    if e.to_ascii_lowercase().contains("timeout") || e.to_ascii_lowercase().contains("timed out") {
        format!("Request failed: no response from the model within {}s ({e})", to.response.as_secs())
    } else {
        format!("Request failed: {e}")
    }
}

// ── Line reading / stream state machine ──

enum Line {
    Text(String),
    Eof,
}

/// Read one line (without the terminator) refusing to buffer more than
/// `MAX_LINE_BYTES`. Invalid UTF-8 is replaced, not fatal.
fn read_line_capped<R: BufRead>(r: &mut R) -> std::io::Result<Line> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let avail = r.fill_buf()?;
        if avail.is_empty() {
            return Ok(if buf.is_empty() { Line::Eof } else { Line::Text(String::from_utf8_lossy(&buf).into_owned()) });
        }
        let (take, found) = match avail.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (avail.len(), false),
        };
        buf.extend_from_slice(&avail[..take]);
        r.consume(take);
        if buf.len() > MAX_LINE_BYTES {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "a single line exceeded 1 MB (not a valid event stream)"));
        }
        if found {
            while matches!(buf.last(), Some(b'\n' | b'\r')) {
                buf.pop();
            }
            return Ok(Line::Text(String::from_utf8_lossy(&buf).into_owned()));
        }
    }
}

enum Flow {
    Continue,
    Finished,
    Failed(String),
}

/// Interprets lines and decides what an end-of-stream means.
struct Engine {
    ndjson: bool,
    deltas: usize,
    finish_seen: bool,
    junk: String,
}

impl Engine {
    fn new(ndjson: bool) -> Self {
        Self { ndjson, deltas: 0, finish_seen: false, junk: String::new() }
    }

    /// Does this line count as the model talking (vs. blank lines / `:` keep-alives)?
    fn is_activity(line: &str) -> bool {
        let t = line.trim();
        !t.is_empty() && !t.starts_with(':')
    }

    fn feed(&mut self, line: &str, emit: &dyn Fn(StreamEvent) -> bool) -> Flow {
        let t = line.trim();
        let parsed = if self.ndjson {
            parse_ndjson_line(t)
        } else if t.starts_with('{') {
            // A server that ignored `stream: true` answers with one JSON object.
            parse_sse_line(&format!("data: {t}"))
        } else {
            parse_sse_line(t)
        };
        if !self.ndjson && (t.contains("\"finish_reason\":\"") || t.contains("\"finish_reason\": \"")) {
            self.finish_seen = true;
        }
        match parsed {
            Parsed::Delta(d) => {
                self.deltas += 1;
                if !emit(StreamEvent::Delta(d)) {
                    return Flow::Finished; // receiver dropped
                }
            }
            Parsed::Done => {
                emit(StreamEvent::Done);
                return Flow::Finished;
            }
            Parsed::Error(e) => return Flow::Failed(e),
            Parsed::Skip => {
                let known = t.is_empty()
                    || t.starts_with("data:")
                    || (!self.ndjson && (t.starts_with(':') || t.starts_with("event:") || t.starts_with("id:") || t.starts_with("retry:")));
                if !known && self.junk.len() < 400 {
                    if !self.junk.is_empty() {
                        self.junk.push(' ');
                    }
                    self.junk.push_str(&snippet(t, 200));
                }
            }
        }
        Flow::Continue
    }

    /// The stream ended without `[DONE]` / `done:true`.
    fn eof(&mut self, emit: &dyn Fn(StreamEvent) -> bool) -> Result<(), String> {
        if self.finish_seen && self.deltas > 0 {
            emit(StreamEvent::Done);
            return Ok(());
        }
        if self.deltas == 0 {
            return Err(if self.junk.is_empty() {
                "The server closed the connection without sending an answer".into()
            } else {
                format!("Unexpected response (not a streaming reply): {}", snippet(&self.junk, 160))
            });
        }
        Err("Connection closed before the answer was complete".into())
    }
}

/// Read lines from `reader`, forwarding parsed events until done / cancelled.
/// No idle timeout (the reader blocks); split out of [`run`] so it can be
/// exercised with in-memory fixtures. [`pump_timed`] is the production path.
pub fn pump<R: BufRead>(
    mut reader: R,
    ndjson: bool,
    cancel: &AtomicBool,
    emit: &dyn Fn(StreamEvent) -> bool,
) -> Result<(), String> {
    let mut eng = Engine::new(ndjson);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        match read_line_capped(&mut reader).map_err(|e| format!("Stream interrupted: {e}"))? {
            Line::Eof => return eng.eof(emit),
            Line::Text(l) => match eng.feed(&l, emit) {
                Flow::Continue => {}
                Flow::Finished => return Ok(()),
                Flow::Failed(e) => return Err(e),
            },
        }
    }
}

/// Like [`pump`] but the blocking reads happen on a helper thread so the
/// caller can enforce cancellation and the idle timeout while the socket is
/// silent. The helper exits at its next line (or EOF/error) after we return.
fn pump_timed<R: BufRead + Send + 'static>(
    mut reader: R,
    ndjson: bool,
    cancel: &AtomicBool,
    emit: &dyn Fn(StreamEvent) -> bool,
    to: Timeouts,
) -> Result<(), String> {
    enum Msg {
        Line(String),
        Eof,
        Err(String),
    }
    let (tx, rx) = sync_channel::<Msg>(64);
    let abort = Arc::new(AtomicBool::new(false));
    let abort_r = abort.clone();
    std::thread::spawn(move || loop {
        if abort_r.load(Ordering::Relaxed) {
            return;
        }
        let msg = match read_line_capped(&mut reader) {
            Ok(Line::Text(l)) => Msg::Line(l),
            Ok(Line::Eof) => Msg::Eof,
            Err(e) => Msg::Err(format!("Stream interrupted: {e}")),
        };
        let last = !matches!(msg, Msg::Line(_));
        if tx.send(msg).is_err() || last {
            return;
        }
    });
    struct Abort(Arc<AtomicBool>);
    impl Drop for Abort {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let _guard = Abort(abort);

    let mut eng = Engine::new(ndjson);
    let mut started = false; // saw the first data line
    let mut last = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Msg::Line(l)) => {
                if Engine::is_activity(&l) {
                    started = true;
                    last = Instant::now();
                }
                match eng.feed(&l, emit) {
                    Flow::Continue => {}
                    Flow::Finished => return Ok(()),
                    Flow::Failed(e) => return Err(e),
                }
            }
            Ok(Msg::Eof) => return eng.eof(emit),
            Ok(Msg::Err(e)) => return Err(e),
            Err(RecvTimeoutError::Timeout) => {
                let limit = if started { to.idle } else { to.first_token };
                if last.elapsed() >= limit {
                    return Err(if started {
                        format!("The model stopped responding (no data for {}s); partial answer kept", limit.as_secs())
                    } else {
                        format!("The model did not start answering within {}s", limit.as_secs())
                    });
                }
            }
            Err(RecvTimeoutError::Disconnected) => return eng.eof(emit),
        }
    }
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
        // No terminator at all after partial content: an error, partial text kept.
        let ev = collect(r#"{"message":{"content":"a"},"done":false}"#, true, false);
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0], StreamEvent::Delta("a".into()));
        assert!(matches!(&ev[1], StreamEvent::Error(e) if e.contains("before the answer was complete")), "{ev:?}");
        // SSE that ends right after a finish_reason is fine (some servers omit [DONE]).
        let ev = collect("data: {\"choices\":[{\"delta\":{\"content\":\"z\"}}]}\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n", false, false);
        assert_eq!(ev, vec![StreamEvent::Delta("z".into()), StreamEvent::Done]);
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
        assert!(j.get("options").and_then(|o| o.get("num_ctx")).and_then(Json::as_f64).unwrap() >= 4096.0);
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
        assert_eq!(http_error(401, r#"{"error":{"message":"bad key"}}"#, None), "HTTP 401: bad key (check the API key in [llm])");
        assert_eq!(http_error(401, "", None), "HTTP 401: Invalid API key (check the API key in [llm])");
        assert_eq!(http_error(500, "", None), "HTTP 500");
        assert!(http_error(502, "<html>gateway</html>", None).starts_with("HTTP 502: <html>"));
        assert_eq!(http_error(429, "", Some("3")), "HTTP 429: Too many requests (rate limited, retry in 3s)");
        assert!(http_error(429, r#"{"error":"slow down"}"#, None).contains("slow down"));
    }

    #[test]
    fn non_stream_bodies_become_errors_with_snippet() {
        let ev = collect("<html>not an sse stream</html>\n", false, false);
        assert!(matches!(ev.as_slice(), [StreamEvent::Error(e)] if e.contains("not a streaming reply") && e.contains("<html>")), "{ev:?}");
        let ev = collect("", false, false);
        assert!(matches!(ev.as_slice(), [StreamEvent::Error(_)]));
        // One JSON object from a server that ignored stream:true is accepted.
        let ev = collect(r#"{"choices":[{"message":{"content":"hi"},"finish_reason":"stop"}]}"#, false, false);
        assert_eq!(ev, vec![StreamEvent::Delta("hi".into()), StreamEvent::Done]);
        // Truncated NDJSON line after content.
        let ev = collect("{\"message\":{\"content\":\"cut\"},\"done\":false}\n{\"message\":{\"content\":\"half", true, false);
        assert!(matches!(ev.last(), Some(StreamEvent::Error(_))));
    }

    #[test]
    fn giant_line_is_rejected_not_buffered() {
        struct Endless;
        impl std::io::Read for Endless {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                b.fill(b'a');
                Ok(b.len())
            }
        }
        let ev = collect_reader(BufReader::new(Endless), false);
        assert!(matches!(ev.last(), Some(StreamEvent::Error(e)) if e.contains("1 MB")), "{ev:?}");
    }

    fn collect_reader<R: BufRead>(r: R, ndjson: bool) -> Vec<StreamEvent> {
        let out = RefCell::new(Vec::new());
        let flag = AtomicBool::new(false);
        let res = pump(r, ndjson, &flag, &|e| {
            out.borrow_mut().push(e);
            true
        });
        let mut v = out.into_inner();
        if let Err(e) = res {
            v.push(StreamEvent::Error(e));
        }
        v
    }

    #[test]
    fn timed_pump_reports_stall_and_ignores_keepalives() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let (mut w, r) = UnixStream::pair().unwrap();
        let to = Timeouts { response: Duration::from_secs(5), first_token: Duration::from_secs(5), idle: Duration::from_millis(600) };
        std::thread::spawn(move || {
            let _ = w.write_all(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n");
            for _ in 0..40 {
                std::thread::sleep(Duration::from_millis(100));
                if w.write_all(b": keepalive\n\n").is_err() {
                    return;
                }
            }
        });
        let out = RefCell::new(Vec::new());
        let flag = AtomicBool::new(false);
        let t = Instant::now();
        let res = pump_timed(BufReader::new(r), false, &flag, &|e| { out.borrow_mut().push(e); true }, to);
        assert!(matches!(&res, Err(e) if e.contains("stopped responding")), "{res:?}");
        assert!(t.elapsed() < Duration::from_secs(3));
        assert_eq!(out.into_inner(), vec![StreamEvent::Delta("hi".into())]);
    }
}
