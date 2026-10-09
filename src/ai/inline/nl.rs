//! `# natural language` at the shell prompt: detection, prompt and parsing.

use super::fix::{clean_explanation, sanitize_command};
use super::json;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NlCommand {
    pub command: String,
    pub explanation: String,
}

/// Minimum words after a plain `# ` for it to count as a request; shorter
/// ones are ordinary shell comments (`# TODO`, `# foo`).
pub const MIN_WORDS: usize = 3;

/// Opt-in alternative trigger, e.g. `RIFT_AI_NL_PREFIX="#? "` makes
/// `#? disk usage` a request with any number of words.
pub fn custom_prefix() -> Option<String> {
    std::env::var("RIFT_AI_NL_PREFIX").ok().filter(|p| p.trim().len() >= 2 && p.starts_with('#') && p.ends_with(' '))
}

/// If the text typed at the prompt is a `# request`, return the request.
/// Requires `#` immediately followed by a space and at least [`MIN_WORDS`]
/// words that do not start with a comment marker like `TODO`, so `#!`, `##`,
/// a bare `#` and everyday comments stay with the shell. A configured
/// custom prefix ([`custom_prefix`]) also triggers, with any non-empty body.
pub fn detect_query(typed: &str) -> Option<String> {
    detect_query_with(typed, custom_prefix().as_deref())
}

pub fn detect_query_with(typed: &str, prefix: Option<&str>) -> Option<String> {
    let t = typed.trim_start();
    if let Some(p) = prefix {
        if let Some(rest) = t.strip_prefix(p) {
            let q = rest.trim();
            return (!q.is_empty()).then(|| q.to_string());
        }
    }
    let rest = t.strip_prefix("# ")?;
    let q = rest.trim();
    let mut words = q.split_whitespace();
    let first = words.next()?;
    let marker = first.trim_end_matches(|c: char| c == ':' || c == '(' || c == ')').to_ascii_uppercase();
    if matches!(marker.as_str(), "TODO" | "FIXME" | "NOTE" | "XXX" | "HACK" | "BUG" | "WARNING" | "WARN" | "NB") {
        return None;
    }
    if 1 + words.count() < MIN_WORDS {
        return None;
    }
    Some(q.to_string())
}

pub fn nl_prompt(query: &str, previous: Option<&str>, cwd: &str, os: &str, shell: &str) -> String {
    use crate::ai::chat::guard::sanitize_line;
    // Everything here is user/terminal text: no ANSI, no secrets, bounded.
    let query = sanitize_line(query, 4096).0;
    let cwd = sanitize_line(cwd, 1024).0;
    let prev = previous
        .map(|p| format!("The previous attempt was: {}\nRefine it according to the request.\n", sanitize_line(p, 2048).0))
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
        assert_eq!(detect_query_with("# find the 10 largest files", None).as_deref(), Some("find the 10 largest files"));
        assert_eq!(detect_query_with("   # one  two three  ", None).as_deref(), Some("one  two three"));
        assert_eq!(detect_query_with("# find big files", None).as_deref(), Some("find big files"));
    }

    #[test]
    fn real_comments_stay_with_the_shell() {
        for c in ["# TODO", "# TODO fix the thing later", "# fixme: broken stuff here", "# note this one", "# two words", "# foo", "#\tdisk usage now please", "#  "] {
            assert_eq!(detect_query_with(c, None), None, "{c}");
        }
    }

    #[test]
    fn custom_prefix_triggers_with_short_bodies() {
        assert_eq!(detect_query_with("#? disk usage", Some("#? ")).as_deref(), Some("disk usage"));
        assert_eq!(detect_query_with("#? x", Some("#? ")).as_deref(), Some("x"));
        assert_eq!(detect_query_with("#?", Some("#? ")), None);
        // the plain form still needs 3 words
        assert_eq!(detect_query_with("# disk usage", Some("#? ")), None);
    }

    #[test]
    fn ignores_non_queries() {
        assert_eq!(detect_query_with("#", None), None);
        assert_eq!(detect_query_with("# ", None), None);
        assert_eq!(detect_query_with("#!/bin/sh", None), None);
        assert_eq!(detect_query_with("## heading is long enough", None), None);
        assert_eq!(detect_query_with("#comment is long enough", None), None);
        assert_eq!(detect_query_with("echo # not at start", None), None);
        assert_eq!(detect_query_with("", None), None);
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
        let p = nl_prompt("upload with token=abcdef123456", None, "/w", "linux", "sh");
        assert!(!p.contains("abcdef123456"));
    }
}
