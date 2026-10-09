//! Local model-server discovery.
//!
//! Probes the well-known loopback ports once at startup (in a background
//! thread, all probes in parallel, 300 ms each) and caches what answered.
//! Absent servers are silent: no logs, no errors.

use std::sync::Mutex;
use std::time::Duration;

use crate::ai::chat::json::Json;
use crate::ai::LlmConfig;

pub const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerKind {
    Ollama,
    LmStudio,
    LlamaCpp,
    Jan,
    Vllm,
}

impl ServerKind {
    pub fn label(self) -> &'static str {
        match self {
            ServerKind::Ollama => "Ollama",
            ServerKind::LmStudio => "LM Studio",
            ServerKind::LlamaCpp => "llama.cpp",
            ServerKind::Jan => "Jan",
            ServerKind::Vllm => "vLLM",
        }
    }

    /// `[llm] provider` value for this kind of server.
    pub fn provider(self) -> &'static str {
        match self {
            ServerKind::Ollama => "ollama",
            _ => "openai-compatible-local",
        }
    }

    fn models_path(self) -> &'static str {
        match self {
            ServerKind::Ollama => "/api/tags",
            _ => "/v1/models",
        }
    }
}

/// Default probe list: (kind, base URL).
pub fn default_probes() -> Vec<(ServerKind, String)> {
    [
        (ServerKind::Ollama, 11434),
        (ServerKind::LmStudio, 1234),
        (ServerKind::LlamaCpp, 8080),
        (ServerKind::Jan, 1337),
        (ServerKind::Vllm, 8000),
    ]
    .into_iter()
    .map(|(k, p)| (k, format!("http://127.0.0.1:{p}")))
    .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalServer {
    pub kind: ServerKind,
    /// Base URL without a path (`http://127.0.0.1:11434`).
    pub base_url: String,
    pub models: Vec<String>,
}

/// A concrete local model: what gets persisted as `[llm]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalChoice {
    pub provider: String,
    pub model: String,
    pub api_url: String,
    pub server: &'static str,
}

impl LocalChoice {
    pub fn to_config(&self) -> LlmConfig {
        LlmConfig { provider: self.provider.clone(), model: self.model.clone(), api_url: self.api_url.clone(), api_key: None, enabled: true }
    }
}

fn is_embedding(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("embed") || n.contains("rerank")
}

impl LocalServer {
    /// First model that can chat (embedding models are skipped).
    pub fn default_model(&self) -> Option<&str> {
        self.models.iter().map(String::as_str).find(|m| !is_embedding(m))
    }

    /// Chat-capable models.
    pub fn chat_models(&self) -> impl Iterator<Item = &str> {
        self.models.iter().map(String::as_str).filter(|m| !is_embedding(m))
    }

    pub fn choice(&self, model: &str) -> LocalChoice {
        LocalChoice {
            provider: self.kind.provider().into(),
            model: model.into(),
            api_url: self.base_url.clone(),
            server: self.kind.label(),
        }
    }
}

/// First chat-capable model across `servers`.
pub fn first_choice(servers: &[LocalServer]) -> Option<LocalChoice> {
    servers.iter().find_map(|s| s.default_model().map(|m| s.choice(m)))
}

// ── Parsing (pure) ──

fn dedup(mut v: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    v.retain(|m| !m.is_empty() && seen.insert(m.clone()));
    v
}

/// Ollama `GET /api/tags`: `{"models":[{"name":"llama3.2:latest",...}]}`.
pub fn parse_ollama_tags(body: &str) -> Vec<String> {
    let Some(j) = Json::parse(body.trim()) else { return Vec::new() };
    let Some(arr) = j.get("models").and_then(Json::as_arr) else { return Vec::new() };
    dedup(
        arr.iter()
            .filter_map(|m| m.get("name").or_else(|| m.get("model")).and_then(Json::as_str))
            .map(str::to_string)
            .collect(),
    )
}

/// OpenAI-style `GET /v1/models`: `{"data":[{"id":"..."}]}`.
pub fn parse_openai_models(body: &str) -> Vec<String> {
    let Some(j) = Json::parse(body.trim()) else { return Vec::new() };
    let Some(arr) = j.get("data").and_then(Json::as_arr) else { return Vec::new() };
    dedup(arr.iter().filter_map(|m| m.get("id").and_then(Json::as_str)).map(str::to_string).collect())
}

// ── Probing ──

fn http_get(url: &str, timeout: Duration) -> Option<String> {
    let mut resp = ureq::get(url)
        .config()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        // Loopback traffic must never be routed through an HTTP proxy.
        .proxy(None)
        .build()
        .call()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.body_mut().with_config().limit(1 << 20).read_to_string().ok()
}

/// Probe one server. `None` when nothing answers or it lists no models.
pub fn probe(kind: ServerKind, base_url: &str, timeout: Duration) -> Option<LocalServer> {
    let base = base_url.trim_end_matches('/');
    let body = http_get(&format!("{base}{}", kind.models_path()), timeout)?;
    let models = match kind {
        ServerKind::Ollama => parse_ollama_tags(&body),
        _ => parse_openai_models(&body),
    };
    (!models.is_empty()).then(|| LocalServer { kind, base_url: base.to_string(), models })
}

/// Probe all `probes` in parallel; results keep the order of `probes`.
pub fn discover_with(probes: &[(ServerKind, String)], timeout: Duration) -> Vec<LocalServer> {
    let handles: Vec<_> = probes
        .iter()
        .cloned()
        .map(|(kind, base)| std::thread::spawn(move || probe(kind, &base, timeout)))
        .collect();
    handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
}

// ── Cache ──

#[derive(Default)]
struct State {
    started: bool,
    done: bool,
    servers: Vec<LocalServer>,
}

static STATE: Mutex<State> = Mutex::new(State { started: false, done: false, servers: Vec::new() });

/// Start the one-time background discovery (idempotent).
pub fn start() {
    {
        let Ok(mut s) = STATE.lock() else { return };
        if s.started {
            return;
        }
        s.started = true;
    }
    std::thread::spawn(|| {
        let found = discover_with(&default_probes(), PROBE_TIMEOUT);
        if let Ok(mut s) = STATE.lock() {
            s.servers = found;
            s.done = true;
        }
        crate::wake::wake();
    });
}

/// Re-run discovery (e.g. when the model picker opens); the previous result
/// stays visible until the new one lands.
pub fn refresh() {
    std::thread::spawn(|| {
        let found = discover_with(&default_probes(), PROBE_TIMEOUT);
        if let Ok(mut s) = STATE.lock() {
            s.servers = found;
            s.started = true;
            s.done = true;
        }
        crate::wake::wake();
    });
}

/// Has the startup discovery finished?
pub fn finished() -> bool {
    STATE.lock().map(|s| s.done).unwrap_or(true)
}

pub fn started() -> bool {
    STATE.lock().map(|s| s.started).unwrap_or(false)
}

/// Cached result (empty until discovery finishes).
pub fn servers() -> Vec<LocalServer> {
    STATE.lock().map(|s| s.servers.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    const OLLAMA: &str = r#"{"models":[{"name":"qwen2.5-coder:7b","model":"qwen2.5-coder:7b","size":4683087332,"details":{"family":"qwen2"}},{"name":"nomic-embed-text:latest","model":"nomic-embed-text:latest"},{"name":"qwen2.5-coder:7b"}]}"#;
    const OPENAI: &str = r#"{"object":"list","data":[{"id":"text-embedding-nomic-embed-text-v1.5","object":"model"},{"id":"meta-llama-3.1-8b-instruct","object":"model","owned_by":"organization_owner"}]}"#;

    #[test]
    fn parses_ollama_tags() {
        assert_eq!(parse_ollama_tags(OLLAMA), vec!["qwen2.5-coder:7b", "nomic-embed-text:latest"]);
        assert!(parse_ollama_tags(r#"{"models":[]}"#).is_empty());
        assert!(parse_ollama_tags("not json").is_empty());
        assert!(parse_ollama_tags(r#"{"data":[{"id":"x"}]}"#).is_empty());
    }

    #[test]
    fn parses_openai_models() {
        assert_eq!(parse_openai_models(OPENAI).len(), 2);
        assert!(parse_openai_models(r#"{"data":[]}"#).is_empty());
        assert!(parse_openai_models("<html>").is_empty());
        assert!(parse_openai_models(OLLAMA).is_empty());
    }

    #[test]
    fn default_model_skips_embeddings() {
        let s = LocalServer { kind: ServerKind::LmStudio, base_url: "http://127.0.0.1:1234".into(), models: parse_openai_models(OPENAI) };
        assert_eq!(s.default_model(), Some("meta-llama-3.1-8b-instruct"));
        let only_embed = LocalServer { models: vec!["nomic-embed-text".into()], ..s.clone() };
        assert_eq!(only_embed.default_model(), None);
        let c = s.choice("m");
        assert_eq!((c.provider.as_str(), c.api_url.as_str()), ("openai-compatible-local", "http://127.0.0.1:1234"));
        assert_eq!(first_choice(&[only_embed, s]).unwrap().model, "meta-llama-3.1-8b-instruct");
    }

    /// One-shot HTTP server answering every request with `status`/`body`.
    fn serve(status: &'static str, body: &'static str) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mut s = s;
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    #[test]
    fn probes_live_servers_and_ignores_absent_ones() {
        let ollama = serve("200 OK", OLLAMA);
        let lms = serve("200 OK", OPENAI);
        let broken = serve("500 Internal Server Error", "{}");
        let empty = serve("200 OK", r#"{"data":[]}"#);
        // A port nobody listens on.
        let dead = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://127.0.0.1:{}", l.local_addr().unwrap().port())
        };
        let probes = vec![
            (ServerKind::Ollama, ollama.clone()),
            (ServerKind::LmStudio, lms.clone()),
            (ServerKind::Jan, broken),
            (ServerKind::Vllm, empty),
            (ServerKind::LlamaCpp, dead),
        ];
        let found = discover_with(&probes, Duration::from_millis(500));
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].kind, ServerKind::Ollama);
        assert_eq!(found[0].base_url, ollama);
        assert_eq!(found[1].kind, ServerKind::LmStudio);
        assert_eq!(found[1].models.len(), 2);
    }
}
