use std::time::Duration;

use super::chat::guard;
use super::chat::json::{quote, Json};
use super::chat::stream::{http_error, snippet};
use super::context::TermContext;
use super::local::usage::{self, is_loopback_url};
use super::local::{self, Feature};
use super::{LlmConfig, Message};

/// Whole-request budget for the non-streaming calls (advisor, fix, `#`, teaching).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Local servers may need to load the model into memory first.
const LOCAL_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

pub fn complete(
    config: &LlmConfig,
    question: &str,
    ctx: &TermContext,
    profile: &str,
) -> Result<String, String> {
    let system_prompt = build_system_prompt(ctx, profile);
    let messages = vec![
        Message {
            role: "system",
            content: system_prompt,
        },
        Message {
            role: "user",
            content: question.to_string(),
        },
    ];
    call(config, &messages, Feature::Other, false)
}

/// Single-turn completion with a caller-supplied prompt and no terminal
/// context wrapping. Used by features that build their own full prompt —
/// e.g. the Advisor safety reviewer — instead of the main assistant's
/// terminal-aware system prompt.
///
/// The prompt is scrubbed here as the last line of defence: ANSI/control
/// bytes are stripped and secrets redacted, whatever the caller did.
pub fn complete_simple(config: &LlmConfig, prompt: &str) -> Result<String, String> {
    complete_with(config, prompt, Feature::Other, false)
}

/// [`complete_simple`] for a prompt that asks for a JSON object (fix, `#`,
/// advisor): Ollama is told to emit JSON only and sampling is made
/// conservative, which small local models need to keep the format.
pub fn complete_structured(config: &LlmConfig, prompt: &str, feature: Feature) -> Result<String, String> {
    complete_with(config, prompt, feature, true)
}

/// Single-turn completion attributed to `feature` in the privacy ledger.
pub fn complete_with(config: &LlmConfig, prompt: &str, feature: Feature, structured: bool) -> Result<String, String> {
    let messages = vec![Message {
        role: "user",
        content: prompt.to_string(),
    }];
    call(config, &messages, feature, structured)
}

fn call(config: &LlmConfig, messages: &[Message], feature: Feature, structured: bool) -> Result<String, String> {
    let ollama = config.provider == "ollama";
    let loopback = is_loopback_url(&config.api_url);
    let (url, body) = build_body(config, messages, structured);
    // Count the bytes before they leave (cloud) or stay (loopback).
    usage::record(feature, config, &body);

    let mut req = ureq::post(&url).header("Content-Type", "application/json");
    if !ollama {
        if let Some(key) = &config.api_key {
            req = req.header("Authorization", &format!("Bearer {key}"));
        }
    }
    let mut cfg = req
        .config()
        .http_status_as_error(false)
        .timeout_global(Some(if loopback { LOCAL_REQUEST_TIMEOUT } else { REQUEST_TIMEOUT }));
    if loopback {
        // A proxy from the environment must never see (or relay) local traffic.
        cfg = cfg.proxy(None);
    }
    let mut resp = cfg
        .build()
        .send(body.as_bytes())
        .map_err(|e| format!("{} request failed: {e}", if ollama { "Ollama" } else { "API" }))?;

    let status = resp.status().as_u16();
    let retry = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).map(str::to_string);
    let text = resp
        .body_mut()
        .with_config()
        .limit(4 << 20)
        .read_to_string()
        .map_err(|e| format!("Read body failed: {e}"))?;
    if !(200..300).contains(&status) {
        return Err(http_error(status, &text, retry.as_deref()));
    }
    parse_response(config, &text)
}

/// Request URL and JSON body. Every string goes through [`quote`] (RFC 8259
/// escaping of all control characters) after ANSI/secret scrubbing.
fn build_body(config: &LlmConfig, messages: &[Message], structured: bool) -> (String, String) {
    let base = config.api_url.trim_end_matches('/');
    let msgs = messages_to_json(messages);
    if config.provider == "ollama" {
        // `format:"json"` constrains the output to valid JSON; `num_ctx` keeps
        // Ollama from silently truncating the prompt to its small default window.
        let format = if structured { r#""format":"json","# } else { "" };
        let temp = if structured { r#","temperature":0.2"# } else { "" };
        (
            format!("{base}/api/chat"),
            format!(
                r#"{{"model":{},"messages":[{}],"stream":false,{format}"options":{{"num_ctx":{}{temp}}}}}"#,
                quote(&config.model),
                msgs,
                local::ollama_num_ctx()
            ),
        )
    } else {
        let temp = if structured { "0.2" } else { "0.7" };
        (
            format!("{base}/v1/chat/completions"),
            format!(r#"{{"model":{},"messages":[{}],"temperature":{temp}}}"#, quote(&config.model), msgs),
        )
    }
}

/// OpenAI: `choices[0].message.content`; Ollama: `message.content`.
fn parse_response(config: &LlmConfig, text: &str) -> Result<String, String> {
    let j = Json::parse(text.trim()).ok_or_else(|| format!("Parse failed: {}", snippet(text, 200)))?;
    if let Some(err) = j.get("error") {
        let msg = match err {
            Json::Str(s) => s.clone(),
            o => o.get("message").and_then(Json::as_str).unwrap_or("unknown error").to_string(),
        };
        return Err(msg);
    }
    let content = if config.provider == "ollama" {
        j.get("message").and_then(|m| m.get("content"))
    } else {
        j.get("choices").and_then(|c| c.idx(0)).and_then(|c| c.get("message")).and_then(|m| m.get("content"))
    };
    match content {
        Some(Json::Str(s)) => Ok(s.clone()),
        // `content: null` happens with tool calls; there is no text to show.
        _ => Err(format!("Parse failed: no text in response: {}", snippet(text, 200))),
    }
}

fn messages_to_json(messages: &[Message]) -> String {
    messages
        .iter()
        .map(|m| format!(r#"{{"role":"{}","content":{}}}"#, m.role, quote(&guard::scrub_outbound(&m.content))))
        .collect::<Vec<_>>()
        .join(",")
}

fn build_system_prompt(ctx: &TermContext, profile: &str) -> String {
    let mut prompt = String::from(
        "You are rift AI, a terminal command expert. \
         Output the shell command on the FIRST line. \
         Brief explanation on following lines. \
         If dangerous (rm -rf, DROP, etc), add a WARNING line. \
         Be concise.\n",
    );
    prompt.push_str(guard::UNTRUSTED_NOTICE);
    prompt.push_str("\n\n");
    prompt.push_str(&format!("OS: {}, Shell: {}\n", ctx.os, ctx.shell));
    prompt.push_str(&format!("CWD: {}\n", ctx.cwd));
    if let Some(ref branch) = ctx.git_branch {
        prompt.push_str(&format!("Git branch: {branch}\n"));
    }
    if let Some(ref ptype) = ctx.project_type {
        prompt.push_str(&format!("Project: {ptype}\n"));
    }
    if !ctx.recent_commands.is_empty() {
        // Recent commands are terminal-derived: fence them as data.
        prompt.push_str(&format!(
            "Recent commands:\n{}\n",
            guard::wrap_untrusted("history", &guard::sanitize(&ctx.recent_commands.join("\n"), guard::MAX_ITEM_BYTES).0)
        ));
    }
    if !profile.is_empty() {
        prompt.push_str(&format!("User preferences: {profile}\n"));
    }
    prompt
}

/// Value of the first string-valued `"key": "..."` in `json`, fully decoded
/// (`\uXXXX` incl. surrogate pairs, `\b \f`, ...). `None` when the key is
/// missing or its value is not a string (e.g. `null`). Tolerates text
/// around the JSON, which is why it scans instead of parsing the document.
pub(crate) fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{key}\"");
    let mut from = 0;
    while let Some(p) = json[from..].find(&pattern) {
        let after = &json[from + p + pattern.len()..];
        from += p + pattern.len();
        let Some(v) = after.trim_start().strip_prefix(':') else { continue };
        let v = v.trim_start();
        if !v.starts_with('"') {
            return None;
        }
        // Find the closing quote, then let the real JSON parser decode it.
        let bytes = v.as_bytes();
        let mut i = 1;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 2,
                b'"' => return Json::parse(&v[..=i]).and_then(|j| j.as_str().map(str::to_string)),
                _ => i += 1,
            }
        }
        return None;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(provider: &str) -> LlmConfig {
        let mut c = LlmConfig::default();
        c.provider = provider.into();
        c.model = "m".into();
        c
    }

    #[test]
    fn body_is_valid_json_with_controls_and_scrubbed() {
        let msgs = vec![Message { role: "user", content: "a\x1b[31mred\x1b[0m\x07\u{8}\u{c}\0 \u{2028} token=abcdef123456 中😀".into() }];
        for p in ["openai", "ollama"] {
            let (_, body) = build_body(&cfg(p), &msgs, false);
            let j = Json::parse(&body).unwrap_or_else(|| panic!("invalid json: {body}"));
            let c = j.get("messages").unwrap().idx(0).unwrap().get("content").unwrap().as_str().unwrap().to_string();
            assert!(!c.contains('\x1b') && !c.contains("abcdef123456"), "{c}");
            assert!(c.starts_with("ared") && c.contains("中😀"), "{c}");
            assert!(body.chars().all(|ch| (ch as u32) >= 0x20 && ch != '\u{2028}'));
        }
    }

    #[test]
    fn ollama_bodies_set_context_and_json_format_for_structured_prompts() {
        let msgs = vec![Message { role: "user", content: "hi".into() }];
        let (url, plain) = build_body(&cfg("ollama"), &msgs, false);
        assert!(url.ends_with("/api/chat"));
        let j = Json::parse(&plain).unwrap();
        assert_eq!(j.get("options").and_then(|o| o.get("num_ctx")).and_then(Json::as_f64), Some(local::ollama_num_ctx() as f64));
        assert!(j.get("format").is_none(), "free text must not be forced into JSON");
        let (_, st) = build_body(&cfg("ollama"), &msgs, true);
        let j = Json::parse(&st).unwrap();
        assert_eq!(j.get("format").and_then(Json::as_str), Some("json"));
        assert_eq!(j.get("stream").and_then(Json::as_bool), Some(false));
        assert!(j.get("options").and_then(|o| o.get("temperature")).and_then(Json::as_f64).unwrap() <= 0.3);
        // OpenAI-compatible servers get no Ollama-only fields.
        let (_, oa) = build_body(&cfg("openai-compatible-local"), &msgs, true);
        let j = Json::parse(&oa).unwrap();
        assert!(j.get("format").is_none() && j.get("options").is_none());
    }

    /// In-process mock: answers one request, returns the raw request text.
    fn mock_server(reply_body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = s.read(&mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req).to_string();
                if let Some(hdr_end) = text.find("\r\n\r\n") {
                    let len = text[..hdr_end].lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok())).unwrap_or(0);
                    if req.len() >= hdr_end + 4 + len {
                        break;
                    }
                }
            }
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply_body}", reply_body.len());
            String::from_utf8_lossy(&req).to_string()
        });
        (url, h)
    }

    #[test]
    fn structured_call_to_local_ollama_sets_json_format_and_counts_local_bytes() {
        let (url, h) = mock_server(r#"{"message":{"content":"{\"command\":\"ls\"}"},"done":true}"#);
        let mut c = cfg("ollama");
        c.api_url = url;
        c.model = "tiny".into();
        let before = usage::today();
        let out = complete_structured(&c, "list files token=abcdef123456", Feature::Fix).unwrap();
        assert_eq!(out, r#"{"command":"ls"}"#);
        let req = h.join().unwrap();
        assert!(req.starts_with("POST /api/chat"), "{req}");
        let body = &req[req.find("\r\n\r\n").unwrap() + 4..];
        let j = Json::parse(body).unwrap();
        assert_eq!(j.get("format").and_then(Json::as_str), Some("json"));
        assert!(j.get("options").and_then(|o| o.get("num_ctx")).is_some());
        assert!(!body.contains("abcdef123456"), "secrets are redacted before the wire");
        let after = usage::today();
        assert_eq!(after.cloud_bytes, before.cloud_bytes, "loopback traffic is never counted as sent to cloud");
        assert!(after.local_reqs > before.local_reqs);
        let e = usage::recent(200).into_iter().find(|e| e.model == "tiny" && e.feature == Feature::Fix).expect("ledger entry");
        assert!(!e.cloud && e.bytes == body.len() as u64 && e.redactions >= 1, "{e:?}");
    }

    #[test]
    fn responses_decode_unicode_and_reject_null_content() {
        let c = cfg("openai");
        let r = parse_response(&c, r#"{"choices":[{"message":{"content":"中文 é 😀"}}]}"#);
        assert_eq!(r.as_deref(), Ok("中文 é 😀"));
        let r = parse_response(&c, r#"{"choices":[{"message":{"content":null,"tool_calls":[{"function":{"arguments":"{\"content\":\"WRONG\"}"}}]}}]}"#);
        assert!(r.is_err());
        assert_eq!(parse_response(&cfg("ollama"), r#"{"message":{"content":"ok 世界"},"done":true}"#).as_deref(), Ok("ok 世界"));
        assert_eq!(parse_response(&c, r#"{"error":{"message":"bad"}}"#), Err("bad".into()));
    }

    #[test]
    fn extract_json_string_decodes_everything() {
        let j = r#"noise {"risk": "caution!", "suggestion":"ls \"中\" 中😀\n\b", "n": null}"#;
        assert_eq!(extract_json_string(j, "risk").as_deref(), Some("caution!"));
        assert_eq!(extract_json_string(j, "suggestion").as_deref(), Some("ls \"中\" 中😀\n\u{8}"));
        assert_eq!(extract_json_string(j, "n"), None);
        assert_eq!(extract_json_string(j, "missing"), None);
    }

    #[test]
    fn system_prompt_marks_terminal_text_untrusted() {
        let ctx = TermContext::collect();
        let p = build_system_prompt(&ctx, "");
        assert!(p.contains("untrusted data"));
    }
}
