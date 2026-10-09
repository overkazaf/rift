//! Outbound-bytes ledger: the "zero bytes left this machine" indicator.
//!
//! Every AI request is recorded with its payload size and whether the endpoint
//! is loopback. Bytes to non-loopback endpoints add up in a per-day total that
//! is persisted to `~/.config/rift/ai_usage` (never any request content). The
//! last requests (time, feature, provider, bytes, redaction count) are kept in
//! memory for the Privacy Report.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;

use super::Feature;
use crate::ai::LlmConfig;

const KEEP_DAYS: usize = 30;
const KEEP_ENTRIES: usize = 200;

/// Is `url` served from this machine (`localhost`, `127.0.0.0/8`, `::1`)?
pub fn is_loopback_url(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit('@').next().unwrap_or(auth);
    let host = if let Some(v6) = auth.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        auth.split(':').next().unwrap_or("")
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host == "::1" {
        return true;
    }
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4 && parts[0] == "127" && parts.iter().all(|p| p.parse::<u8>().is_ok())
}

/// Host part of `url` for display.
pub fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#']).next().unwrap_or("").rsplit('@').next().unwrap_or("").to_string()
}

/// Number of secrets masked in outbound text (`[REDACTED...]` markers).
pub fn count_redactions(text: &str) -> usize {
    text.matches("[REDACTED").count()
}

pub fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Unix seconds.
    pub ts: u64,
    pub feature: Feature,
    pub provider: String,
    pub model: String,
    pub host: String,
    pub bytes: u64,
    pub redactions: usize,
    /// Left the machine (non-loopback endpoint).
    pub cloud: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Day {
    pub cloud_bytes: u64,
    pub cloud_reqs: u32,
    pub local_bytes: u64,
    pub local_reqs: u32,
}

/// Local-time day number for `ts` (days since the epoch, shifted by the
/// current UTC offset so "today" matches the user's wall clock).
pub fn day_of(ts: u64) -> u64 {
    (ts as i64 + utc_offset_secs(ts)).max(0) as u64 / 86_400
}

#[cfg(unix)]
fn utc_offset_secs(ts: u64) -> i64 {
    // SAFETY: localtime_r only writes into the zeroed tm we pass.
    unsafe {
        let t = ts as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i64
    }
}
#[cfg(not(unix))]
fn utc_offset_secs(_ts: u64) -> i64 {
    0
}

/// `HH:MM:SS` local wall clock.
pub fn clock(ts: u64) -> String {
    let s = (ts as i64 + utc_offset_secs(ts)).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Default)]
pub struct Ledger {
    entries: VecDeque<Entry>,
    days: BTreeMap<u64, Day>,
}

impl Ledger {
    pub fn record(&mut self, e: Entry) {
        let d = self.days.entry(day_of(e.ts)).or_default();
        if e.cloud {
            d.cloud_bytes += e.bytes;
            d.cloud_reqs += 1;
        } else {
            d.local_bytes += e.bytes;
            d.local_reqs += 1;
        }
        self.entries.push_back(e);
        while self.entries.len() > KEEP_ENTRIES {
            self.entries.pop_front();
        }
        while self.days.len() > KEEP_DAYS {
            let first = *self.days.keys().next().unwrap();
            self.days.remove(&first);
        }
    }

    pub fn today(&self, now: u64) -> Day {
        self.days.get(&day_of(now)).copied().unwrap_or_default()
    }

    /// Newest first.
    pub fn recent(&self, n: usize) -> Vec<Entry> {
        self.entries.iter().rev().take(n).cloned().collect()
    }

    /// `day<TAB>cloud_bytes<TAB>cloud_reqs<TAB>local_bytes<TAB>local_reqs` per line.
    pub fn serialize(&self) -> String {
        self.days
            .iter()
            .map(|(k, d)| format!("{k}\t{}\t{}\t{}\t{}\n", d.cloud_bytes, d.cloud_reqs, d.local_bytes, d.local_reqs))
            .collect()
    }

    pub fn parse(s: &str) -> Self {
        let mut days = BTreeMap::new();
        for line in s.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() != 5 {
                continue;
            }
            let (Ok(k), Ok(cb), Ok(cr), Ok(lb), Ok(lr)) =
                (f[0].parse::<u64>(), f[1].parse::<u64>(), f[2].parse::<u32>(), f[3].parse::<u64>(), f[4].parse::<u32>())
            else {
                continue;
            };
            days.insert(k, Day { cloud_bytes: cb, cloud_reqs: cr, local_bytes: lb, local_reqs: lr });
        }
        Self { entries: VecDeque::new(), days }
    }
}

// ── Global ledger ──

static LEDGER: Mutex<Option<Ledger>> = Mutex::new(None);

pub fn usage_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RIFT_AI_USAGE_FILE") {
        return Some(PathBuf::from(p));
    }
    Some(dirs::home_dir()?.join(".config").join("rift").join("ai_usage"))
}

fn persist_enabled() -> bool {
    // Unit tests must never touch the user's real file.
    !cfg!(test) || std::env::var_os("RIFT_AI_USAGE_FILE").is_some()
}

fn with_ledger<R>(f: impl FnOnce(&mut Ledger) -> R) -> Option<R> {
    let mut g = LEDGER.lock().ok()?;
    let l = g.get_or_insert_with(|| {
        if persist_enabled() {
            usage_path().and_then(|p| std::fs::read_to_string(p).ok()).map(|s| Ledger::parse(&s)).unwrap_or_default()
        } else {
            Ledger::default()
        }
    });
    Some(f(l))
}

fn persist(l: &Ledger) {
    if !persist_enabled() {
        return;
    }
    let Some(path) = usage_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, l.serialize()).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Record one request about to be sent to `config`'s endpoint. `payload` is
/// the exact request body (only its size and the redaction markers are kept).
pub fn record(feature: Feature, config: &LlmConfig, payload: &str) {
    record_entry(Entry {
        ts: now(),
        feature,
        provider: config.provider.clone(),
        model: config.model.clone(),
        host: host_of(&config.api_url),
        bytes: payload.len() as u64,
        redactions: count_redactions(payload),
        cloud: !is_loopback_url(&config.api_url),
    });
}

pub fn record_entry(e: Entry) {
    let cloud = e.cloud;
    with_ledger(|l| {
        l.record(e);
        // Local-only traffic is flushed together with the next cloud write.
        if cloud {
            persist(l);
        }
    });
}

pub fn today() -> Day {
    with_ledger(|l| l.today(now())).unwrap_or_default()
}

pub fn recent(n: usize) -> Vec<Entry> {
    with_ledger(|l| l.recent(n)).unwrap_or_default()
}

/// `Today: 0 B sent to cloud` / `Today: 3.4 KB sent to cloud (2 requests)`.
pub fn footer_text(d: Day) -> String {
    if d.cloud_reqs == 0 {
        "Today: 0 B sent to cloud".to_string()
    } else {
        format!(
            "Today: {} sent to cloud ({} request{})",
            format_bytes(d.cloud_bytes),
            d.cloud_reqs,
            if d.cloud_reqs == 1 { "" } else { "s" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ts: u64, url: &str, bytes: u64) -> Entry {
        Entry {
            ts,
            feature: Feature::Fix,
            provider: "p".into(),
            model: "m".into(),
            host: host_of(url),
            bytes,
            redactions: 0,
            cloud: !is_loopback_url(url),
        }
    }

    #[test]
    fn loopback_detection() {
        for u in [
            "http://localhost:11434",
            "http://127.0.0.1:1234/v1",
            "http://127.1.2.3",
            "https://LOCALHOST/x",
            "http://[::1]:8080",
            "http://user:pw@127.0.0.1:80/",
            "localhost:8000",
        ] {
            assert!(is_loopback_url(u), "{u}");
        }
        for u in [
            "https://api.openai.com",
            "http://192.168.1.5:11434",
            "http://0.0.0.0:8000",
            "http://127.0.0.1.evil.com",
            "http://localhost.evil.com",
            "http://evil.com/?u=127.0.0.1",
            "http://127.0.0.1@evil.com/",
            "http://10.0.0.2",
        ] {
            assert!(!is_loopback_url(u), "{u}");
        }
    }

    #[test]
    fn ledger_separates_cloud_and_local() {
        let mut l = Ledger::default();
        let t = 1_800_000_000;
        l.record(entry(t, "http://127.0.0.1:11434", 500));
        assert_eq!(l.today(t), Day { local_bytes: 500, local_reqs: 1, ..Day::default() });
        assert_eq!(footer_text(l.today(t)), "Today: 0 B sent to cloud");
        l.record(entry(t + 5, "https://api.openai.com", 2048));
        l.record(entry(t + 9, "https://api.deepseek.com", 1024));
        let d = l.today(t + 9);
        assert_eq!((d.cloud_bytes, d.cloud_reqs, d.local_reqs), (3072, 2, 1));
        assert_eq!(footer_text(d), "Today: 3.0 KB sent to cloud (2 requests)");
        let r = l.recent(2);
        assert_eq!(r[0].host, "api.deepseek.com");
        assert_eq!(r.len(), 2);
        // A different day starts from zero.
        assert_eq!(l.today(t + 3 * 86_400), Day::default());
    }

    #[test]
    fn ledger_round_trips_and_ignores_junk() {
        let mut l = Ledger::default();
        l.record(entry(1_800_000_000, "https://api.openai.com", 10));
        l.record(entry(1_800_000_000, "http://localhost:1", 7));
        let s = l.serialize();
        let back = Ledger::parse(&format!("{s}garbage\n1\t2\n"));
        assert_eq!(back.today(1_800_000_000), l.today(1_800_000_000));
        assert!(!s.contains("openai"), "only totals are persisted, no hosts or content");
    }

    #[test]
    fn ledger_is_bounded() {
        let mut l = Ledger::default();
        for i in 0..(KEEP_ENTRIES as u64 + 50) {
            l.record(entry(1_800_000_000 + i, "http://127.0.0.1", 1));
        }
        assert_eq!(l.entries.len(), KEEP_ENTRIES);
        for d in 0..(KEEP_DAYS as u64 + 5) {
            l.record(entry(1_700_000_000 + d * 86_400, "http://127.0.0.1", 1));
        }
        assert!(l.days.len() <= KEEP_DAYS);
    }

    #[test]
    fn record_counts_redactions_and_size() {
        let cfg = LlmConfig { provider: "openai".into(), model: "m".into(), api_url: "https://api.openai.com".into(), api_key: None, enabled: true };
        let before = today().cloud_bytes;
        let body = r#"{"messages":[{"content":"token=[REDACTED] and [REDACTED:private key]"}]}"#;
        record(Feature::Chat, &cfg, body);
        assert_eq!(today().cloud_bytes - before, body.len() as u64);
        let last = recent(KEEP_ENTRIES).into_iter().find(|e| e.bytes == body.len() as u64 && e.host == "api.openai.com").expect("recorded");
        assert!(last.cloud && last.redactions == 2);
    }

    #[test]
    fn byte_formatting() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(5 << 20), "5.0 MB");
    }
}
