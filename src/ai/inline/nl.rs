//! `# natural language` at the shell prompt: detection, prompt and parsing.

use super::fix::{clean_explanation, sanitize_command};
use super::json;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NlCommand {
    pub command: String,
    pub explanation: String,
}

/// If the text typed at the prompt is a `# request`, return the request.
/// Requires `#` followed by whitespace and a non-empty body, so `#!`, `##`
/// and a bare `#` are left alone for the shell.
pub fn detect_query(typed: &str) -> Option<String> {
    let t = typed.trim_start();
    let rest = t.strip_prefix('#')?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let q = rest.trim();
    if q.is_empty() {
        None
    } else {
        Some(q.to_string())
    }
}

pub fn nl_prompt(query: &str, previous: Option<&str>, cwd: &str, os: &str, shell: &str) -> String {
    let prev = previous
        .map(|p| format!("The previous attempt was: {p}\nRefine it according to the request.\n"))
        .unwrap_or_default();
    format!(
        "Translate the request into ONE shell command for the environment below.\n\
         Reply with ONLY a JSON object and nothing else (no markdown, no prose):\n\
         {{\"command\": \"<single-line shell command>\", \"explanation\": \"<under 12 words>\"}}\n\
         If it cannot be done with one command, reply exactly: {{\"command\": null}}\n\
         Prefer safe, non-destructive commands.\n\n\
         OS: {os}\nShell: {shell}\nWorking directory: {cwd}\n{prev}Request: {query}\n"
    )
}

/// Parse a reply: JSON first, then (only if the reply has no JSON at all)
/// a single bare / fenced command line.
pub fn parse_nl_response(raw: &str) -> Option<NlCommand> {
    if let Some(obj) = json::parse_first_object(raw) {
        let command = sanitize_command(obj.get_str("command")?)?;
        let explanation = obj.get_str("explanation").map(clean_explanation).unwrap_or_default();
        return Some(NlCommand { command, explanation });
    }
    if raw.contains('{') {
        return None;
    }
    let lines: Vec<&str> = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("```"))
        .collect();
    if lines.len() != 1 || lines[0].chars().count() > 300 {
        return None;
    }
    Some(NlCommand { command: sanitize_command(lines[0])?, explanation: String::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_hash_space_queries() {
        assert_eq!(detect_query("# find the 10 largest files").as_deref(), Some("find the 10 largest files"));
        assert_eq!(detect_query("   #   spaced  ").as_deref(), Some("spaced"));
        assert_eq!(detect_query("#\tdisk usage").as_deref(), Some("disk usage"));
    }

    #[test]
    fn ignores_non_queries() {
        assert_eq!(detect_query("#"), None);
        assert_eq!(detect_query("# "), None);
        assert_eq!(detect_query("#!/bin/sh"), None);
        assert_eq!(detect_query("## heading"), None);
        assert_eq!(detect_query("#comment"), None);
        assert_eq!(detect_query("echo # not at start"), None);
        assert_eq!(detect_query(""), None);
    }

    #[test]
    fn parses_json_and_fallback() {
        let c = parse_nl_response("```json\n{\"command\": \"du -ah . | sort -rh | head -10\", \"explanation\": \"top 10\"}\n```").unwrap();
        assert_eq!(c.command, "du -ah . | sort -rh | head -10");
        assert_eq!(c.explanation, "top 10");

        let c = parse_nl_response("```sh\nls -la\n```").unwrap();
        assert_eq!(c.command, "ls -la");

        assert!(parse_nl_response(r#"{"command": null}"#).is_none());
        assert!(parse_nl_response("First do this\nthen that").is_none());
        assert!(parse_nl_response("{ broken json").is_none());
    }

    #[test]
    fn prompt_carries_environment() {
        let p = nl_prompt("list files", Some("ls"), "/work", "macos", "/bin/zsh");
        assert!(p.contains("Request: list files") && p.contains("/work") && p.contains("/bin/zsh"));
        assert!(p.contains("previous attempt was: ls"));
        assert!(!nl_prompt("x", None, "/", "linux", "sh").contains("previous attempt"));
    }
}
