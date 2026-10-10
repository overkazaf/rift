//! GitHub Releases: fetch + parse the release list and pick the asset for
//! this platform. Parsing and selection are pure (unit-tested offline).

use std::time::Duration;

use super::version::Version;
use crate::ai::chat::json::Json;

pub const RELEASES_URL: &str = "https://api.github.com/repos/overkazaf/rift/releases?per_page=50";
pub const RELEASES_PAGE: &str = "https://github.com/overkazaf/rift/releases";
const API_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub tag: String,
    pub name: String,
    /// ISO-8601 (`2026-10-01T12:00:00Z`); empty when unpublished.
    pub published_at: String,
    pub prerelease: bool,
    pub draft: bool,
    pub body: String,
    pub assets: Vec<Asset>,
    /// Parsed from `tag` (None for tags that are not versions).
    pub version: Option<Version>,
}

impl Release {
    /// `2026-10-01` (or "" when unknown).
    pub fn date(&self) -> &str {
        self.published_at.get(..10).unwrap_or(&self.published_at)
    }

    /// Version string without the `v` prefix (falls back to the raw tag).
    pub fn version_str(&self) -> String {
        self.version.as_ref().map_or_else(|| self.tag.clone(), Version::to_string)
    }
}

/// Parse the `/releases` JSON array. Drafts and entries without a tag are
/// dropped; the result is sorted newest version first.
pub fn parse_releases(body: &str) -> Result<Vec<Release>, String> {
    let json = Json::parse(body.trim()).ok_or("GitHub returned invalid JSON")?;
    let arr = match json.as_arr() {
        Some(a) => a,
        None => {
            let msg = json.get("message").and_then(Json::as_str).unwrap_or("unexpected response");
            return Err(format!("GitHub API: {msg}"));
        }
    };
    let s = |j: &Json, k: &str| j.get(k).and_then(Json::as_str).unwrap_or("").to_string();
    let mut out: Vec<Release> = arr
        .iter()
        .filter_map(|r| {
            let tag = s(r, "tag_name");
            if tag.is_empty() {
                return None;
            }
            let assets = r
                .get("assets")
                .and_then(Json::as_arr)
                .unwrap_or(&[])
                .iter()
                .filter_map(|a| {
                    let name = s(a, "name");
                    let url = s(a, "browser_download_url");
                    (!name.is_empty() && !url.is_empty()).then(|| Asset {
                        name,
                        url,
                        size: a.get("size").and_then(Json::as_f64).unwrap_or(0.0).max(0.0) as u64,
                    })
                })
                .collect();
            Some(Release {
                version: Version::parse(&tag),
                name: s(r, "name"),
                published_at: s(r, "published_at"),
                prerelease: r.get("prerelease").and_then(Json::as_bool).unwrap_or(false),
                draft: r.get("draft").and_then(Json::as_bool).unwrap_or(false),
                body: s(r, "body"),
                assets,
                tag,
            })
        })
        .filter(|r| !r.draft)
        .collect();
    out.sort_by(|a, b| match (&a.version, &b.version) {
        (Some(x), Some(y)) => y.cmp(x),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => b.published_at.cmp(&a.published_at),
    });
    Ok(out)
}

/// Newest stable release (or newest including pre-releases with `pre`).
pub fn latest(releases: &[Release], pre: bool) -> Option<&Release> {
    releases
        .iter()
        .filter(|r| r.version.is_some() && (pre || !r.prerelease) && (pre || !r.version.as_ref().is_some_and(Version::is_prerelease)))
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// Find a release by `0.4.1` / `v0.4.1` / exact tag.
pub fn find<'a>(releases: &'a [Release], wanted: &str) -> Option<&'a Release> {
    let w = wanted.trim();
    let wv = Version::parse(w);
    releases.iter().find(|r| r.tag == w || (wv.is_some() && r.version == wv))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// Universal `.dmg` with `Rift.app`.
    MacOs,
    /// `rift-<v>-linux-x86_64.tar.gz` with the bare binary.
    LinuxX86_64,
    Unsupported,
}

impl Platform {
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Platform::LinuxX86_64
        } else {
            Platform::Unsupported
        }
    }

    fn expected_name(self, version: &str) -> Option<String> {
        match self {
            Platform::MacOs => Some(format!("Rift-{version}-macos.dmg")),
            Platform::LinuxX86_64 => Some(format!("rift-{version}-linux-x86_64.tar.gz")),
            Platform::Unsupported => None,
        }
    }

    fn suffix(self) -> Option<&'static str> {
        match self {
            Platform::MacOs => Some("-macos.dmg"),
            Platform::LinuxX86_64 => Some("-linux-x86_64.tar.gz"),
            Platform::Unsupported => None,
        }
    }
}

/// The package asset for `platform` and its `.sha256` companion (if published).
pub fn select_asset(release: &Release, platform: Platform) -> Option<(&Asset, Option<&Asset>)> {
    let want = platform.expected_name(&release.version_str())?;
    let suffix = platform.suffix()?;
    let pkg = release
        .assets
        .iter()
        .find(|a| a.name == want)
        .or_else(|| release.assets.iter().find(|a| a.name.to_ascii_lowercase().ends_with(suffix)))?;
    let sum_name = format!("{}.sha256", pkg.name);
    let sum = release.assets.iter().find(|a| a.name == sum_name);
    Some((pkg, sum))
}

/// First few meaningful lines of the release notes, markdown stripped.
pub fn notes_excerpt(body: &str, max_lines: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("<!--") || t.starts_with("**Full Changelog**") {
            continue;
        }
        let t = t.trim_start_matches('#').trim();
        let t = if let Some(rest) = t.strip_prefix("* ").or_else(|| t.strip_prefix("- ")) { format!("\u{2022} {rest}") } else { t.to_string() };
        let t = t.replace("**", "").replace('`', "");
        if t.is_empty() {
            continue;
        }
        out.push(if t.chars().count() > 110 { format!("{}\u{2026}", t.chars().take(109).collect::<String>()) } else { t });
        if out.len() >= max_lines {
            break;
        }
    }
    out
}

// ── Network ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// DNS / connect / TLS failure: probably offline.
    Offline(String),
    /// 403/429 with the rate limit used up; `reset` is a unix timestamp.
    RateLimited { reset: Option<u64> },
    Http(u16, String),
    Parse(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Offline(e) => write!(f, "could not reach GitHub (offline?): {e}"),
            FetchError::RateLimited { reset } => {
                write!(f, "GitHub API rate limit reached")?;
                if let Some(r) = reset {
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                    let mins = r.saturating_sub(now).div_ceil(60);
                    write!(f, "; try again in ~{mins} min")?;
                }
                write!(f, " (set GITHUB_TOKEN to raise the limit)")
            }
            FetchError::Http(code, msg) => write!(f, "GitHub API returned HTTP {code}{}{msg}", if msg.is_empty() { "" } else { ": " }),
            FetchError::Parse(e) => write!(f, "{e}"),
        }
    }
}

pub fn user_agent() -> String {
    format!("rift/{} (+https://github.com/overkazaf/rift)", env!("CARGO_PKG_VERSION"))
}

/// GET the release list. Blocking; call from a background thread in the UI.
pub fn fetch_releases() -> Result<Vec<Release>, FetchError> {
    let mut req = ureq::get(RELEASES_URL)
        .header("User-Agent", &user_agent())
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Ok(tok) = std::env::var("GITHUB_TOKEN") {
        if !tok.trim().is_empty() {
            req = req.header("Authorization", &format!("Bearer {}", tok.trim()));
        }
    }
    let mut resp = req
        .config()
        .http_status_as_error(false)
        .timeout_global(Some(API_TIMEOUT))
        .build()
        .call()
        .map_err(|e| FetchError::Offline(e.to_string()))?;
    let status = resp.status().as_u16();
    let header = |k: &str| resp.headers().get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
    let remaining = header("x-ratelimit-remaining");
    let reset = header("x-ratelimit-reset").and_then(|v| v.parse().ok());
    let body = resp
        .body_mut()
        .with_config()
        .limit(8 << 20)
        .read_to_string()
        .map_err(|e| FetchError::Offline(format!("reading response: {e}")))?;
    if (status == 403 || status == 429) && (remaining.as_deref() == Some("0") || body.contains("rate limit")) {
        return Err(FetchError::RateLimited { reset });
    }
    if !(200..300).contains(&status) {
        let msg = Json::parse(body.trim()).and_then(|j| j.get("message").and_then(Json::as_str).map(str::to_string)).unwrap_or_default();
        return Err(FetchError::Http(status, msg));
    }
    parse_releases(&body).map_err(FetchError::Parse)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const SAMPLE: &str = r###"[
      {"tag_name":"v0.5.0-rc.1","name":"Rift 0.5.0 RC 1","published_at":"2026-10-05T09:00:00Z","prerelease":true,"draft":false,
       "body":"Release candidate","assets":[]},
      {"tag_name":"v0.4.1","name":"Rift 0.4.1","published_at":"2026-10-01T12:34:56Z","prerelease":false,"draft":false,
       "body":"## What's Changed\r\n* Fix tab close by @a in #12\r\n* **Faster** `blocks`\r\n\r\n**Full Changelog**: https://x",
       "assets":[
         {"name":"Rift-0.4.1-macos.dmg","size":31457280,"browser_download_url":"https://github.com/overkazaf/rift/releases/download/v0.4.1/Rift-0.4.1-macos.dmg"},
         {"name":"Rift-0.4.1-macos.dmg.sha256","size":87,"browser_download_url":"https://github.com/overkazaf/rift/releases/download/v0.4.1/Rift-0.4.1-macos.dmg.sha256"},
         {"name":"rift-0.4.1-linux-x86_64.tar.gz","size":9000000,"browser_download_url":"https://github.com/overkazaf/rift/releases/download/v0.4.1/rift-0.4.1-linux-x86_64.tar.gz"},
         {"name":"rift-0.4.1-linux-x86_64.tar.gz.sha256","size":97,"browser_download_url":"https://github.com/overkazaf/rift/releases/download/v0.4.1/rift-0.4.1-linux-x86_64.tar.gz.sha256"}
       ]},
      {"tag_name":"v0.6.0","name":"draft","published_at":null,"prerelease":false,"draft":true,"body":null,"assets":[]},
      {"tag_name":"v0.3.0","name":"","published_at":"2026-06-01T00:00:00Z","prerelease":false,"draft":false,"body":null,
       "assets":[{"name":"Rift-0.3.0-macos.dmg","size":1,"browser_download_url":"https://e/x.dmg"}]},
      {"tag_name":"v0.4.0","name":"Rift 0.4.0","published_at":"2026-09-01T00:00:00Z","prerelease":false,"draft":false,"body":"","assets":[]},
      {"tag_name":"nightly","name":"Nightly","published_at":"2026-10-09T00:00:00Z","prerelease":true,"draft":false,"body":"","assets":[]}
    ]"###;

    #[test]
    fn parses_releases_sorted_newest_first_without_drafts() {
        let r = parse_releases(SAMPLE).unwrap();
        let tags: Vec<&str> = r.iter().map(|r| r.tag.as_str()).collect();
        assert_eq!(tags, ["v0.5.0-rc.1", "v0.4.1", "v0.4.0", "v0.3.0", "nightly"]);
        let r41 = &r[1];
        assert_eq!(r41.name, "Rift 0.4.1");
        assert_eq!(r41.date(), "2026-10-01");
        assert!(!r41.prerelease && r[0].prerelease);
        assert_eq!(r41.assets.len(), 4);
        assert_eq!(r41.assets[0].size, 31_457_280);
        assert_eq!(r41.version_str(), "0.4.1");
        assert!(r[3].body.is_empty(), "null body -> empty");
        assert_eq!(r[4].version, None);
    }

    #[test]
    fn api_errors_are_reported() {
        assert_eq!(parse_releases(r#"{"message":"Not Found"}"#).unwrap_err(), "GitHub API: Not Found");
        assert!(parse_releases("<html>").is_err());
        assert_eq!(parse_releases("[]").unwrap(), vec![]);
    }

    #[test]
    fn latest_and_find() {
        let r = parse_releases(SAMPLE).unwrap();
        assert_eq!(latest(&r, false).unwrap().tag, "v0.4.1");
        assert_eq!(latest(&r, true).unwrap().tag, "v0.5.0-rc.1");
        assert_eq!(find(&r, "0.3.0").unwrap().tag, "v0.3.0");
        assert_eq!(find(&r, "v0.4.1").unwrap().tag, "v0.4.1");
        assert_eq!(find(&r, "nightly").unwrap().tag, "nightly");
        assert!(find(&r, "9.9.9").is_none());
        assert!(latest(&[], false).is_none());
    }

    #[test]
    fn selects_platform_asset_and_checksum() {
        let r = parse_releases(SAMPLE).unwrap();
        let r41 = find(&r, "0.4.1").unwrap();
        let (pkg, sum) = select_asset(r41, Platform::MacOs).unwrap();
        assert_eq!(pkg.name, "Rift-0.4.1-macos.dmg");
        assert_eq!(sum.unwrap().name, "Rift-0.4.1-macos.dmg.sha256");
        let (pkg, sum) = select_asset(r41, Platform::LinuxX86_64).unwrap();
        assert_eq!(pkg.name, "rift-0.4.1-linux-x86_64.tar.gz");
        assert_eq!(sum.unwrap().name, "rift-0.4.1-linux-x86_64.tar.gz.sha256");
        assert!(select_asset(r41, Platform::Unsupported).is_none());
        // Package without a checksum file.
        let (pkg, sum) = select_asset(find(&r, "0.3.0").unwrap(), Platform::MacOs).unwrap();
        assert_eq!(pkg.name, "Rift-0.3.0-macos.dmg");
        assert!(sum.is_none());
        // No assets at all.
        assert!(select_asset(find(&r, "0.4.0").unwrap(), Platform::MacOs).is_none());
        assert!(select_asset(find(&r, "0.3.0").unwrap(), Platform::LinuxX86_64).is_none());
    }

    #[test]
    fn suffix_fallback_when_name_differs() {
        let rel = Release {
            tag: "v1.0.0".into(),
            name: String::new(),
            published_at: String::new(),
            prerelease: false,
            draft: false,
            body: String::new(),
            assets: vec![
                Asset { name: "Rift-1.0-macos.dmg".into(), url: "u".into(), size: 0 },
                Asset { name: "Rift-1.0-macos.dmg.sha256".into(), url: "u2".into(), size: 0 },
            ],
            version: Version::parse("v1.0.0"),
        };
        let (pkg, sum) = select_asset(&rel, Platform::MacOs).unwrap();
        assert_eq!(pkg.name, "Rift-1.0-macos.dmg");
        assert!(sum.is_some());
    }

    #[test]
    fn notes_excerpt_strips_markdown() {
        let r = parse_releases(SAMPLE).unwrap();
        let lines = notes_excerpt(&r[1].body, 5);
        assert_eq!(lines, ["What's Changed", "\u{2022} Fix tab close by @a in #12", "\u{2022} Faster blocks"]);
        assert_eq!(notes_excerpt(&r[1].body, 1).len(), 1);
        assert!(notes_excerpt("", 3).is_empty());
    }

    #[test]
    fn rate_limit_message_mentions_token() {
        let m = FetchError::RateLimited { reset: None }.to_string();
        assert!(m.contains("rate limit") && m.contains("GITHUB_TOKEN"), "{m}");
    }
}
