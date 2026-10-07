use super::context::TermContext;
use super::{LlmConfig, Message};

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

    match config.provider.as_str() {
        "ollama" => call_ollama(config, &messages),
        _ => call_openai_compat(config, &messages),
    }
}

fn call_ollama(config: &LlmConfig, messages: &[Message]) -> Result<String, String> {
    let url = format!("{}/api/chat", config.api_url);
    let body = format!(
        r#"{{"model":"{}","messages":[{}],"stream":false}}"#,
        escape_json(&config.model),
        messages_to_json(messages)
    );

    let resp = ureq::post(&url)
        .header("Content-Type", "application/json")
        .send(body.as_bytes())
        .map_err(|e| format!("Ollama request failed: {e}"))?;

    let text = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Read body failed: {e}"))?;

    extract_json_string(&text, "content").ok_or_else(|| format!("Parse failed: {}", trunc(&text, 200)))
}

fn call_openai_compat(config: &LlmConfig, messages: &[Message]) -> Result<String, String> {
    let url = format!("{}/v1/chat/completions", config.api_url);
    let body = format!(
        r#"{{"model":"{}","messages":[{}],"temperature":0.7}}"#,
        escape_json(&config.model),
        messages_to_json(messages)
    );

    let mut req = ureq::post(&url).header("Content-Type", "application/json");

    if let Some(key) = &config.api_key {
        req = req.header("Authorization", &format!("Bearer {key}"));
    }

    let resp = req
        .send(body.as_bytes())
        .map_err(|e| format!("API request failed: {e}"))?;

    let text = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Read body failed: {e}"))?;

    // OpenAI format: {"choices":[{"message":{"content":"..."}}]}
    extract_json_string(&text, "content").ok_or_else(|| format!("Parse failed: {}", trunc(&text, 200)))
}

fn messages_to_json(messages: &[Message]) -> String {
    messages
        .iter()
        .map(|m| {
            format!(
                r#"{{"role":"{}","content":"{}"}}"#,
                m.role,
                escape_json(&m.content)
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn build_system_prompt(ctx: &TermContext, profile: &str) -> String {
    let mut prompt = String::from(
        "You are rift AI, a terminal command expert. \
         Output the shell command on the FIRST line. \
         Brief explanation on following lines. \
         If dangerous (rm -rf, DROP, etc), add a WARNING line. \
         Be concise.\n\n",
    );
    prompt.push_str(&format!("OS: {}, Shell: {}\n", ctx.os, ctx.shell));
    prompt.push_str(&format!("CWD: {}\n", ctx.cwd));
    if let Some(ref branch) = ctx.git_branch {
        prompt.push_str(&format!("Git branch: {branch}\n"));
    }
    if let Some(ref ptype) = ctx.project_type {
        prompt.push_str(&format!("Project: {ptype}\n"));
    }
    if !ctx.recent_commands.is_empty() {
        prompt.push_str(&format!(
            "Recent commands: {}\n",
            ctx.recent_commands.join("; ")
        ));
    }
    if !profile.is_empty() {
        prompt.push_str(&format!("User preferences: {profile}\n"));
    }
    prompt
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let pos = json.find(&pattern)?;
    let after_key = &json[pos + pattern.len()..];
    // skip whitespace and colon
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let after_colon = after_colon.trim_start();
    if !after_colon.starts_with('"') {
        return None;
    }
    let content = &after_colon[1..];
    let mut result = String::new();
    let mut chars = content.chars();
    loop {
        match chars.next()? {
            '\\' => match chars.next()? {
                'n' => result.push('\n'),
                'r' => result.push('\r'),
                't' => result.push('\t'),
                '"' => result.push('"'),
                '\\' => result.push('\\'),
                '/' => result.push('/'),
                other => {
                    result.push('\\');
                    result.push(other);
                }
            },
            '"' => break,
            c => result.push(c),
        }
    }
    Some(result)
}

use crate::ui::trunc;
