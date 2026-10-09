use std::time::Duration;

use super::chat::guard;
use super::chat::json::{quote, Json};
use super::chat::stream::{http_error, snippet};
use super::context::TermContext;
use super::{LlmConfig, Message};

/// Whole-request budget for the non-streaming calls (advisor, fix, `#`, teaching).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

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
    call(config, &messages)
}

/// Single-turn completion with a caller-supplied prompt and no terminal
/// context wrapping. Used by features that build their own full prompt —
/// e.g. the Advisor safety reviewer — instead of the main assistant's
/// terminal-aware system prompt.
///
/// The prompt is scrubbed here as the last line of defence: ANSI/control
/// bytes are stripped and secrets redacted, whatever the caller did.
pub fn complete_simple(config: &LlmConfig, prompt: &str) -> Result<String, String> {
    let messages = vec![Message {
        role: "user",
        content: prompt.to_string(),
    }];
    call(config, &messages)
}

fn call(config: &LlmConfig, messages: &[Message]) -> Result<String, String> {
    let ollama = config.provider == "ollama";
    let (url, body) = build_body(config, messages);

    let mut req = ureq::post(&url).header("Content-Type", "application/json");
    if !ollama {
        if let Some(key) = &config.api_key {
            req = req.header("Authorization", &format!("Bearer {key}"));
        }
    }
    let mut resp = req
        .config()
        .http_status_as_error(false)
        .timeout_global(Some(REQUEST_TIMEOUT))
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
fn build_body(config: &LlmConfig, messages: &[Message]) -> (String, String) {
    let base = config.api_url.trim_end_matches('/');
    let msgs = messages_to_json(messages);
    if config.provider == "ollama" {
        (
            format!("{base}/api/chat"),
            format!(r#"{{"model":{},"messages":[{}],"stream":false}}"#, quote(&config.model), msgs),
        )
    } else {
        (
            format!("{base}/v1/chat/completions"),
            format!(r#"{{"model":{},"messages":[{}],"temperature":0.7}}"#, quote(&config.model), msgs),
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
            let (_, body) = build_body(&cfg(p), &msgs);
            let j = Json::parse(&body).unwrap_or_else(|| panic!("invalid json: {body}"));
            let c = j.get("messages").unwrap().idx(0).unwrap().get("content").unwrap().as_str().unwrap().to_string();
            assert!(!c.contains('\x1b') && !c.contains("abcdef123456"), "{c}");
            assert!(c.starts_with("ared") && c.contains("中😀"), "{c}");
            assert!(body.chars().all(|ch| (ch as u32) >= 0x20 && ch != '\u{2028}'));
        }
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
