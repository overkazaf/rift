pub struct SecretMasker {
    pub enabled: bool,
    patterns: Vec<SecretPattern>,
}

struct SecretPattern {
    #[allow(dead_code)]
    name: &'static str,
    prefix: &'static str,
    min_len: usize,
}

impl SecretMasker {
    pub fn new() -> Self {
        Self {
            enabled: false,
            patterns: vec![
                SecretPattern { name: "openai", prefix: "sk-", min_len: 20 },
                SecretPattern { name: "github_pat", prefix: "ghp_", min_len: 20 },
                SecretPattern { name: "github_token", prefix: "gho_", min_len: 20 },
                SecretPattern { name: "github_app", prefix: "ghu_", min_len: 20 },
                SecretPattern { name: "github_fine", prefix: "github_pat_", min_len: 20 },
                SecretPattern { name: "aws_access", prefix: "AKIA", min_len: 16 },
                SecretPattern { name: "stripe_live", prefix: "sk_live_", min_len: 20 },
                SecretPattern { name: "stripe_test", prefix: "sk_test_", min_len: 20 },
                SecretPattern { name: "slack_bot", prefix: "xoxb-", min_len: 20 },
                SecretPattern { name: "slack_user", prefix: "xoxp-", min_len: 20 },
                SecretPattern { name: "npm_token", prefix: "npm_", min_len: 20 },
                SecretPattern { name: "pypi_token", prefix: "pypi-", min_len: 20 },
                SecretPattern { name: "deepseek", prefix: "sk-", min_len: 20 },
                SecretPattern { name: "anthropic", prefix: "sk-ant-", min_len: 20 },
                SecretPattern { name: "hf_token", prefix: "hf_", min_len: 20 },
            ],
        }
    }

    pub fn mask(&self, input: &str) -> Option<String> {
        if !self.enabled {
            return None;
        }

        let mut result = input.to_string();
        let mut masked = false;

        for pattern in &self.patterns {
            if let Some(pos) = result.find(pattern.prefix) {
                let start = pos;
                let mut end = pos + pattern.prefix.len();
                let chars: Vec<char> = result.chars().collect();
                while end < chars.len()
                    && (chars[end].is_alphanumeric() || chars[end] == '_' || chars[end] == '-')
                {
                    end += 1;
                }
                if end - start >= pattern.min_len {
                    let visible_len = pattern.prefix.len().min(4);
                    let visible_prefix = &result[start..start + visible_len];
                    let mask = format!("{visible_prefix}***");
                    result = format!("{}{}{}", &result[..start], mask, &result[end..]);
                    masked = true;
                }
            }
        }

        if let Some(r) = mask_key_value(&result, "password") {
            result = r;
            masked = true;
        }
        if let Some(r) = mask_key_value(&result, "secret") {
            result = r;
            masked = true;
        }
        if let Some(r) = mask_key_value(&result, "token") {
            result = r;
            masked = true;
        }
        if let Some(r) = mask_key_value(&result, "api_key") {
            result = r;
            masked = true;
        }

        if result.contains('.') {
            let snapshot = result.clone();
            let words: Vec<&str> = snapshot.split_whitespace().collect();
            for word in &words {
                if is_jwt(word) {
                    let jwt_masked = format!(
                        "{}...{}",
                        &word[..10.min(word.len())],
                        &word[word.len().saturating_sub(5)..]
                    );
                    result = result.replace(*word, &jwt_masked);
                    masked = true;
                }
            }
        }

        if masked { Some(result) } else { None }
    }

    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
        log::info!(
            "Secret masking: {}",
            if self.enabled { "ON" } else { "OFF" }
        );
    }
}

fn mask_key_value(input: &str, key: &str) -> Option<String> {
    let lower = input.to_lowercase();
    let key_lower = key.to_lowercase();
    for sep in &["=", ": ", "= "] {
        let pattern = format!("{key_lower}{sep}");
        if let Some(pos) = lower.find(&pattern) {
            let value_start = pos + key.len() + sep.len();
            let mut value_end = value_start;
            let chars: Vec<char> = input.chars().collect();
            while value_end < chars.len()
                && !chars[value_end].is_whitespace()
                && chars[value_end] != '"'
                && chars[value_end] != '\''
            {
                value_end += 1;
            }
            if value_end > value_start + 3 {
                let show = 3.min(value_end - value_start);
                let masked = format!(
                    "{}{}***{}",
                    &input[..value_start],
                    &input[value_start..value_start + show],
                    &input[value_end..]
                );
                return Some(masked);
            }
        }
    }
    None
}

fn is_jwt(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts.iter().all(|p| {
        p.len() > 10
            && p.chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '=')
    })
}

// ───────────────────────────────────────────────────────────────────────────
// Outbound redaction for the AI path.
//
// `SecretMasker` above masks *display/recorded* text and only handles the
// first hit of each pattern. Everything that leaves the machine for an LLM
// goes through `redact` instead: it replaces *every* occurrence, never
// slices inside a multi-byte char and is linear in the input size.
// ───────────────────────────────────────────────────────────────────────────

/// (prefix, minimum total length incl. prefix, label)
const TOKEN_PREFIXES: &[(&str, usize, &str)] = &[
    ("sk-ant-", 20, "anthropic key"),
    ("sk-", 20, "api key"),
    ("sk_live_", 20, "stripe key"),
    ("sk_test_", 20, "stripe key"),
    ("rk_live_", 20, "stripe key"),
    ("ghp_", 20, "github token"),
    ("gho_", 20, "github token"),
    ("ghu_", 20, "github token"),
    ("ghs_", 20, "github token"),
    ("ghr_", 20, "github token"),
    ("github_pat_", 30, "github token"),
    ("glpat-", 20, "gitlab token"),
    ("AKIA", 20, "aws key id"),
    ("ASIA", 20, "aws key id"),
    ("AIza", 39, "google key"),
    ("xoxa-", 15, "slack token"),
    ("xoxb-", 15, "slack token"),
    ("xoxp-", 15, "slack token"),
    ("xoxr-", 15, "slack token"),
    ("xoxs-", 15, "slack token"),
    ("npm_", 20, "npm token"),
    ("pypi-", 20, "pypi token"),
    ("hf_", 30, "hf token"),
    ("shpat_", 20, "shopify token"),
    ("dop_v1_", 20, "digitalocean token"),
];

/// Identifier fragments that make an `NAME=value` / `"name": "value"` pair sensitive.
const SENSITIVE_KEYS: &[&str] = &[
    "password", "passwd", "passphrase", "secret", "token", "api_key", "apikey", "api-key", "access_key",
    "accesskey", "private_key", "privatekey", "signing_key", "encryption_key", "credential",
];

const HEADER_KEYS: &[&str] = &["authorization:", "x-api-key:", "x-auth-token:", "api-key:", "apikey:", "cookie:", "set-cookie:"];

fn is_tok(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Replace every secret-looking substring of `input` with `[REDACTED...]`.
/// Returns the new text and the number of replacements.
pub fn redact(input: &str) -> (String, usize) {
    let mut n = 0;
    let mut s = redact_private_keys(input, &mut n);
    s = redact_url_creds(&s, &mut n);
    s = redact_headers(&s, &mut n);
    s = redact_bearer(&s, &mut n);
    s = redact_assignments(&s, &mut n);
    s = redact_prefixed(&s, &mut n);
    s = redact_jwt(&s, &mut n);
    (s, n)
}

fn redact_private_keys(s: &str, n: &mut usize) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(b) = rest.find("-----BEGIN ") {
        let after = &rest[b + 11..];
        let header_end = after.find("-----").unwrap_or(after.len());
        if !after[..header_end].contains("PRIVATE KEY") {
            out.push_str(&rest[..b + 11]);
            rest = after;
            continue;
        }
        out.push_str(&rest[..b]);
        out.push_str("[REDACTED:private key]");
        *n += 1;
        // Skip to the matching END marker (or to the end of input).
        let from = b + 11 + header_end;
        match rest[from..].find("-----END ") {
            Some(e) => {
                let tail = &rest[from + e + 9..];
                rest = match tail.find("-----") {
                    Some(t) => &tail[t + 5..],
                    None => "",
                };
            }
            None => rest = "",
        }
    }
    out.push_str(rest);
    out
}

fn redact_url_creds(s: &str, n: &mut usize) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find("://") {
        let auth_start = p + 3;
        out.push_str(&rest[..auth_start]);
        let tail = &rest[auth_start..];
        let auth_end = tail
            .find(|c: char| matches!(c, '/' | '?' | '#' | '"' | '\'' | '<' | '>') || c.is_whitespace())
            .unwrap_or(tail.len());
        let auth = &tail[..auth_end];
        match (auth.rfind('@'), auth.find(':')) {
            (Some(at), Some(colon)) if colon < at && at > colon + 1 && !auth[colon + 1..at].starts_with("[REDACTED") => {
                out.push_str(&auth[..colon + 1]);
                out.push_str("[REDACTED]");
                out.push_str(&auth[at..]);
                *n += 1;
            }
            _ => out.push_str(auth),
        }
        rest = &tail[auth_end..];
    }
    out.push_str(rest);
    out
}

fn redact_headers(s: &str, n: &mut usize) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    loop {
        // Earliest header key at/after pos.
        let next = HEADER_KEYS.iter().filter_map(|k| lower[pos..].find(k).map(|i| (pos + i, k.len()))).min();
        let Some((at, klen)) = next else { break };
        let vstart = at + klen;
        out.push_str(&s[pos..vstart]);
        let line_end = s[vstart..]
            .find(|c: char| matches!(c, '\n' | '\r' | '"' | '\''))
            .map_or(s.len(), |i| vstart + i);
        let value = &s[vstart..line_end];
        if value.trim().is_empty() || value.trim_start().starts_with("[REDACTED") {
            out.push_str(value);
        } else {
            out.push_str(" [REDACTED]");
            *n += 1;
        }
        pos = line_end;
    }
    out.push_str(&s[pos..]);
    out
}

fn redact_bearer(s: &str, n: &mut usize) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    while let Some(i) = lower[pos..].find("bearer ") {
        let at = pos + i;
        let vstart = at + 7;
        let vend = s[vstart..]
            .bytes()
            .position(|b| !(is_tok(b) || matches!(b, b'.' | b'~' | b'+' | b'/' | b'=')))
            .map_or(s.len(), |e| vstart + e);
        out.push_str(&s[pos..vstart]);
        if vend - vstart >= 8 {
            out.push_str("[REDACTED]");
            *n += 1;
        } else {
            out.push_str(&s[vstart..vend]);
        }
        pos = vend;
    }
    out.push_str(&s[pos..]);
    out
}

fn benign_value(v: &str) -> bool {
    let l = v.to_ascii_lowercase();
    l.is_empty()
        || l.starts_with("[redacted")
        || l.starts_with('$')
        || l.starts_with('<')
        || l.starts_with("{{")
        || l.starts_with("***")
        || matches!(l.as_str(), "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "none" | "nil" | "0" | "1" | "undefined")
}

fn sensitive_name(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    SENSITIVE_KEYS.iter().any(|k| l.contains(k))
}

/// Byte range of the value starting at `i` (after the separator): optional
/// quote, then up to whitespace / quote / `&`. Returns (start, end).
fn value_span(s: &str, mut i: usize) -> (usize, usize) {
    let b = s.as_bytes();
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    let quote = match b.get(i) {
        Some(&q @ (b'"' | b'\'')) => {
            i += 1;
            Some(q)
        }
        _ => None,
    };
    let start = i;
    let end = s[start..]
        .find(|c: char| match quote {
            Some(q) => c == q as char || c == '\n',
            None => c.is_whitespace() || matches!(c, '"' | '\'' | '&' | ';'),
        })
        .map_or(s.len(), |e| start + e);
    (start, end)
}

fn redact_assignments(s: &str, n: &mut usize) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut pos = 0; // copied up to here
    let mut i = 0;
    while i < b.len() {
        if !(is_tok(b[i]) || b[i] == b'.') {
            i += 1;
            continue;
        }
        let run_start = i;
        while i < b.len() && (is_tok(b[i]) || b[i] == b'.') {
            i += 1;
        }
        let name = &s[run_start..i];
        if !sensitive_name(name) {
            continue;
        }
        let mut j = i;
        if matches!(b.get(j), Some(b'"' | b'\'')) {
            j += 1;
        }
        let mut k = j;
        while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
            k += 1;
        }
        let colon = match b.get(k) {
            Some(b'=') if b.get(k + 1) != Some(&b'=') => false,
            Some(b':') => true,
            // `--password hunter2` style flags.
            Some(_) if k > j && name.starts_with("--") && b.get(k) != Some(&b'-') => {
                let (vs, ve) = value_span(s, k);
                let v = &s[vs..ve];
                if !benign_value(v) && v.len() >= 3 {
                    out.push_str(&s[pos..vs]);
                    out.push_str("[REDACTED]");
                    *n += 1;
                    pos = ve;
                    i = ve;
                }
                continue;
            }
            _ => continue,
        };
        let (vs, ve) = value_span(s, k + 1);
        let v = &s[vs..ve];
        if benign_value(v) || (colon && v.len() < 6) || v.is_empty() {
            continue;
        }
        // `token: expired` is prose; `"token": "x"` / `token: abc123` are data.
        let quoted = j > i || (vs > 0 && matches!(b[vs - 1], b'"' | b'\''));
        if colon && !quoted && v.chars().all(char::is_alphabetic) {
            continue;
        }
        out.push_str(&s[pos..vs]);
        out.push_str("[REDACTED]");
        *n += 1;
        pos = ve;
        i = ve;
    }
    out.push_str(&s[pos..]);
    out
}

fn redact_prefixed(s: &str, n: &mut usize) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    let mut i = 0;
    while i < b.len() {
        // Only token starts: previous byte must not be part of a word.
        if i > 0 && is_tok(b[i - 1]) {
            i += 1;
            continue;
        }
        let hit = TOKEN_PREFIXES.iter().find(|(p, ..)| b[i..].starts_with(p.as_bytes()));
        let Some(&(prefix, min, label)) = hit else {
            i += 1;
            continue;
        };
        let mut e = i + prefix.len();
        while e < b.len() && is_tok(b[e]) {
            e += 1;
        }
        if e - i >= min {
            out.push_str(&s[pos..i]);
            out.push_str(&format!("[REDACTED:{label}]"));
            *n += 1;
            pos = e;
            i = e;
        } else {
            i += prefix.len();
        }
    }
    out.push_str(&s[pos..]);
    out
}

fn redact_jwt(s: &str, n: &mut usize) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    let mut from = 0;
    while let Some(p) = s[from..].find("eyJ") {
        let i = from + p;
        from = i + 3;
        if i > 0 && is_tok(b[i - 1]) {
            continue;
        }
        let mut e = i;
        let mut segs = 0;
        loop {
            let st = e;
            while e < b.len() && (is_tok(b[e]) || b[e] == b'=') {
                e += 1;
            }
            if e - st < 5 && !(segs == 2 && e > st) {
                break;
            }
            segs += 1;
            if segs == 3 {
                break;
            }
            if b.get(e) == Some(&b'.') {
                e += 1;
            } else {
                break;
            }
        }
        if segs == 3 {
            out.push_str(&s[pos..i]);
            out.push_str("[REDACTED:jwt]");
            *n += 1;
            pos = e;
            from = e;
        }
    }
    out.push_str(&s[pos..]);
    out
}

#[cfg(test)]
mod redact_tests {
    use super::redact;

    #[test]
    fn audit_sample() {
        let src = "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\nAuthorization: Bearer sk-live-abcdef1234567890\npassword=hunter2\n";
        let (o, n) = redact(src);
        assert!(!o.contains("wJalr") && !o.contains("sk-live") && !o.contains("hunter2"), "{o}");
        assert!(n >= 3);
    }

    #[test]
    fn token_formats() {
        for t in [
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "github_pat_11ABCDEFG0abcdefghijklmnopqrstuvwxyz",
            "sk-proj-abcdefghijklmnopqrstu",
            "sk-ant-api03-abcdefghijklmnop",
            "AKIAIOSFODNN7EXAMPLE",
            "xoxb-123456789012-abcdefghij",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
        ] {
            let (o, n) = redact(&format!("x {t} y"));
            assert_eq!(n, 1, "{t} -> {o}");
            assert!(o.starts_with("x [REDACTED") && o.ends_with("] y"), "{o}");
        }
    }

    #[test]
    fn private_key_urls_and_env() {
        let (o, n) = redact("a\n-----BEGIN RSA PRIVATE KEY-----\nMIIE\nabc\n-----END RSA PRIVATE KEY-----\nb");
        assert_eq!(o, "a\n[REDACTED:private key]\nb");
        assert_eq!(n, 1);
        let (o, _) = redact("git clone https://bob:s3cr3tpass@github.com/x/y.git");
        assert_eq!(o, "git clone https://bob:[REDACTED]@github.com/x/y.git");
        let (o, _) = redact("DATABASE_PASSWORD='pa ss' API_TOKEN=abc123 --password hunter2 {\"secret\": \"zzzzzzzz\"}");
        assert!(!o.contains("pa ss") && !o.contains("abc123") && !o.contains("hunter2") && !o.contains("zzzzzzzz"), "{o}");
        let (o, _) = redact("curl -H 'Authorization: Bearer sk-live-abc' x");
        assert_eq!(o, "curl -H 'Authorization: [REDACTED]' x");
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        for t in [
            "cargo build --release",
            "PWD=/home/me TOKENIZERS_PARALLELISM=false",
            "invalid token: expired",
            "echo $TOKEN password=$PW",
            "task-runner sk-short 中文 ünïcode",
            "ssh git@github.com",
            "https://example.com/a@b",
        ] {
            let (o, n) = redact(t);
            assert_eq!((o.as_str(), n), (t, 0), "{t}");
        }
    }

    #[test]
    fn multibyte_safe_and_linear() {
        let (o, _) = redact("密码 password=中文密码abc 完");
        assert!(o.contains("[REDACTED]") && o.ends_with("完"));
        let big = "A".repeat(5 << 20);
        let t = std::time::Instant::now();
        let _ = redact(&big);
        assert!(t.elapsed().as_secs() < 5);
    }
}
