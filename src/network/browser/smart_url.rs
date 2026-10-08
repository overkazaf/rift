//! Classify address-bar input as a URL or a search query.

/// Search engine used for non-URL input. Single place to change (or later
/// make configurable); the query is appended percent-encoded.
pub const SEARCH_URL_PREFIX: &str = "https://www.google.com/search?q=";

/// Turn raw address-bar text into a URL to load. Returns `None` for blank input.
pub fn resolve(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if has_scheme(s) {
        return Some(s.to_string());
    }
    if s.chars().any(char::is_whitespace) {
        return Some(search_url(s));
    }
    match host_kind(host_of(s)) {
        HostKind::Local => Some(format!("http://{s}")),
        HostKind::Domain => Some(format!("https://{s}")),
        HostKind::None => Some(search_url(s)),
    }
}

pub fn search_url(query: &str) -> String {
    format!("{SEARCH_URL_PREFIX}{}", url_encode(query))
}

/// `scheme://...` or one of the opaque schemes we allow typing directly.
fn has_scheme(s: &str) -> bool {
    if let Some(i) = s.find("://") {
        let scheme = &s[..i];
        return !scheme.is_empty()
            && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c));
    }
    let lower = s.to_ascii_lowercase();
    ["about:", "data:", "file:", "blob:", "view-source:"]
        .iter()
        .any(|p| lower.starts_with(p))
}

/// `host[:port]` part of a scheme-less input (before any path/query/fragment).
fn host_of(s: &str) -> &str {
    let end = s.find(['/', '?', '#']).unwrap_or(s.len());
    &s[..end]
}

#[derive(PartialEq, Debug)]
enum HostKind {
    /// localhost / IP: use http.
    Local,
    /// Looks like a domain name: use https.
    Domain,
    None,
}

fn host_kind(hostport: &str) -> HostKind {
    // IPv6 literal: [::1] or [::1]:8080
    if let Some(rest) = hostport.strip_prefix('[') {
        return match rest.find(']') {
            Some(i) if rest[..i].contains(':') && valid_port(&rest[i + 1..]) => HostKind::Local,
            _ => HostKind::None,
        };
    }
    let (host, port_ok) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
        None => (hostport, true),
    };
    if !port_ok || host.is_empty() {
        return HostKind::None;
    }
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") {
        return HostKind::Local;
    }
    if is_ipv4(host) {
        return HostKind::Local;
    }
    if !host.contains('.') {
        return HostKind::None;
    }
    let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
    let label_ok = |l: &&str| {
        !l.is_empty()
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    };
    let tld_numeric = labels.last().is_some_and(|l| l.chars().all(|c| c.is_ascii_digit()));
    if labels.len() >= 2 && labels.iter().all(label_ok) && !tld_numeric {
        HostKind::Domain
    } else {
        HostKind::None
    }
}

/// `""` or `":8080"`.
fn valid_port(s: &str) -> bool {
    s.is_empty() || s.strip_prefix(':').is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

fn is_ipv4(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit()) && p.parse::<u16>().is_ok_and(|n| n <= 255))
}

/// Percent-encode for a query component; space becomes `+`.
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> String {
        resolve(s).unwrap()
    }

    #[test]
    fn blank_is_none() {
        assert_eq!(resolve(""), None);
        assert_eq!(resolve("   "), None);
    }

    #[test]
    fn explicit_scheme_kept() {
        assert_eq!(r("https://a.com/x"), "https://a.com/x");
        assert_eq!(r("http://localhost:3000"), "http://localhost:3000");
        assert_eq!(r("file:///tmp/a.html"), "file:///tmp/a.html");
        assert_eq!(r("about:blank"), "about:blank");
        assert_eq!(r("  https://a.com  "), "https://a.com");
    }

    #[test]
    fn domains_get_https() {
        assert_eq!(r("example.com"), "https://example.com");
        assert_eq!(r("docs.rs/wry/latest"), "https://docs.rs/wry/latest");
        assert_eq!(r("example.com:8443/path?q=1"), "https://example.com:8443/path?q=1");
        assert_eq!(r("www.example.co.uk"), "https://www.example.co.uk");
    }

    #[test]
    fn localhost_and_ips_get_http() {
        assert_eq!(r("localhost"), "http://localhost");
        assert_eq!(r("localhost:8080"), "http://localhost:8080");
        assert_eq!(r("localhost:8080/a/b"), "http://localhost:8080/a/b");
        assert_eq!(r("127.0.0.1"), "http://127.0.0.1");
        assert_eq!(r("192.168.1.10:3000/x"), "http://192.168.1.10:3000/x");
        assert_eq!(r("[::1]:8080"), "http://[::1]:8080");
        assert_eq!(r("app.localhost:3000"), "http://app.localhost:3000");
    }

    #[test]
    fn plain_words_are_searches() {
        assert_eq!(r("rust"), "https://www.google.com/search?q=rust");
        assert_eq!(r("how to rust"), "https://www.google.com/search?q=how+to+rust");
    }

    #[test]
    fn spaces_with_dots_are_searches() {
        assert_eq!(r("what is example.com"), "https://www.google.com/search?q=what+is+example.com");
    }

    #[test]
    fn numeric_and_bad_hosts_are_searches() {
        assert!(r("3.14").contains("search?q=3.14"));
        assert!(r("999.1.1.1").contains("search?q="));
        assert!(r("a..b").contains("search?q="));
        assert!(r("foo:bar").contains("search?q="));
        assert!(r(".com").contains("search?q="));
    }

    #[test]
    fn search_is_percent_encoded() {
        assert_eq!(r("c++ & rust?"), "https://www.google.com/search?q=c%2B%2B+%26+rust%3F");
        assert_eq!(url_encode("你"), "%E4%BD%A0");
    }
}
