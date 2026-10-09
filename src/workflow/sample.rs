//! Synthetic best-of-N results for the screenshot scene and the tests: three
//! candidates for "add rate limiting to /v1/orders" with real unified diffs
//! (parsed by the same engine Change Review uses).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::bestof::{BestOfN, CandState, DiffStat, Phase, TestStatus};
use super::BestRun;
use crate::agents::AgentKind;
use crate::review::diff;

fn new_file(path: &str, body: &[&str]) -> String {
    let mut s = format!("diff --git a/{path} b/{path}\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@\n", body.len());
    for l in body {
        s.push('+');
        s.push_str(l);
        s.push('\n');
    }
    s
}

fn edit(path: &str, start: usize, before: &[&str], removed: &[&str], added: &[&str], after: &[&str]) -> String {
    let old_len = before.len() + removed.len() + after.len();
    let new_len = before.len() + added.len() + after.len();
    let mut s = format!("diff --git a/{path} b/{path}\nindex 2222222..3333333 100644\n--- a/{path}\n+++ b/{path}\n@@ -{start},{old_len} +{start},{new_len} @@ fn router()\n");
    for l in before {
        s.push_str(&format!(" {l}\n"));
    }
    for l in removed {
        s.push_str(&format!("-{l}\n"));
    }
    for l in added {
        s.push_str(&format!("+{l}\n"));
    }
    for l in after {
        s.push_str(&format!(" {l}\n"));
    }
    s
}

fn claude_diff() -> String {
    let limiter = [
        "use std::collections::HashMap;",
        "use std::sync::Mutex;",
        "use std::time::{Duration, Instant};",
        "",
        "/// Token bucket per client key.",
        "pub struct RateLimiter {",
        "    capacity: f64,",
        "    refill_per_sec: f64,",
        "    buckets: Mutex<HashMap<String, (f64, Instant)>>,",
        "}",
        "",
        "impl RateLimiter {",
        "    pub fn new(capacity: u32, per: Duration) -> Self {",
        "        Self { capacity: capacity as f64, refill_per_sec: capacity as f64 / per.as_secs_f64(), buckets: Mutex::default() }",
        "    }",
        "",
        "    /// Returns how long the caller must wait, or `None` when the request may pass.",
        "    pub fn check(&self, key: &str) -> Option<Duration> {",
        "        let mut map = self.buckets.lock().unwrap();",
        "        let (tokens, last) = map.entry(key.to_string()).or_insert((self.capacity, Instant::now()));",
        "        *tokens = (*tokens + last.elapsed().as_secs_f64() * self.refill_per_sec).min(self.capacity);",
        "        *last = Instant::now();",
        "        if *tokens >= 1.0 {",
        "            *tokens -= 1.0;",
        "            None",
        "        } else {",
        "            Some(Duration::from_secs_f64((1.0 - *tokens) / self.refill_per_sec))",
        "        }",
        "    }",
        "}",
    ];
    let test = [
        "use aurora::middleware::rate_limit::RateLimiter;",
        "use std::time::Duration;",
        "",
        "#[test]",
        "fn burst_then_block() {",
        "    let rl = RateLimiter::new(3, Duration::from_secs(60));",
        "    assert!((0..3).all(|_| rl.check(\"a\").is_none()));",
        "    assert!(rl.check(\"a\").is_some());",
        "    assert!(rl.check(\"b\").is_none(), \"other clients are independent\");",
        "}",
    ];
    let router = edit(
        "src/router.rs",
        41,
        &["    Router::new()", "        .route(\"/v1/health\", get(health))"],
        &["        .route(\"/v1/orders\", post(create_order))"],
        &["        .route(\"/v1/orders\", post(create_order).layer(rate_limit(60)))", "        .layer(Extension(Arc::new(RateLimiter::new(60, Duration::from_secs(60)))))"],
        &["        .layer(TraceLayer::new_for_http())"],
    );
    format!("{}{}{}", new_file("src/middleware/rate_limit.rs", &limiter), router, new_file("tests/rate_limit.rs", &test))
}

fn codex_diff() -> String {
    let mw = [
        "use axum::{http::StatusCode, middleware::Next, response::Response};",
        "",
        "static HITS: AtomicU64 = AtomicU64::new(0);",
        "",
        "pub async fn limit<B>(req: Request<B>, next: Next<B>) -> Result<Response, StatusCode> {",
        "    // Naive global counter: resets every minute.",
        "    if HITS.fetch_add(1, Ordering::Relaxed) > 100 {",
        "        return Err(StatusCode::TOO_MANY_REQUESTS);",
        "    }",
        "    Ok(next.run(req).await)",
        "}",
    ];
    let router = edit(
        "src/router.rs",
        41,
        &["    Router::new()"],
        &[],
        &["        .layer(axum::middleware::from_fn(limit))"],
        &["        .route(\"/v1/health\", get(health))", "        .route(\"/v1/orders\", post(create_order))"],
    );
    format!("{}{}", new_file("src/limit.rs", &mw), router)
}

fn gemini_diff() -> String {
    let cfg = [
        "[rate_limit]",
        "requests_per_minute = 60",
        "burst = 10",
        "key = \"api_key\"",
    ];
    let mw = [
        "use std::time::Duration;",
        "",
        "pub struct Limiter { window: Duration, max: u32 }",
        "",
        "impl Limiter {",
        "    pub fn from_config(c: &Config) -> Self {",
        "        Self { window: Duration::from_secs(60), max: c.rate_limit.requests_per_minute }",
        "    }",
        "}",
    ];
    let router = edit(
        "src/router.rs",
        38,
        &["    let state = AppState::new(cfg);"],
        &["    Router::new().with_state(state)"],
        &["    let limiter = Limiter::from_config(&cfg);", "    Router::new().layer(Extension(limiter)).with_state(state)"],
        &[],
    );
    let doc = edit("docs/api.md", 12, &["## Orders"], &["Orders are created with POST /v1/orders."], &["Orders are created with POST /v1/orders.", "Requests are limited to 60 per minute per API key (HTTP 429 beyond)."], &[]);
    format!("{}{}{}{}", new_file("config/rate_limit.toml", &cfg), new_file("src/middleware/limiter.rs", &mw), router, doc)
}

/// A finished run with `n` (2..=4) candidates; the first has passing tests.
pub fn best_run(n: usize) -> BestRun {
    let n = n.clamp(2, 4);
    let kinds = [AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Gemini, AgentKind::ClaudeCode];
    let task = "add rate limiting to /v1/orders";
    let mut sm = BestOfN::new(
        1,
        task,
        PathBuf::from("/Users/maya/dev/aurora"),
        Some("main".into()),
        "9f3c2a1d84be0c7e5a6b13d2c4f8e9017a5b6c3d".into(),
        &kinds[..n],
        "cargo test",
    );
    let diffs = [claude_diff(), codex_diff(), gemini_diff(), claude_diff()];
    let secs = [252u64, 363, 220, 300];
    let costs = [Some(0.42), Some(0.31), None, Some(0.55)];
    let tests = [TestStatus::Passed, TestStatus::Failed("test rate_limit::burst ... FAILED\nassertion failed: limiter allowed 101 requests".into()), TestStatus::Passed, TestStatus::Passed];
    let t0 = Instant::now();
    let slug = sm.slug.clone();
    let mut parsed = Vec::new();
    for i in 0..n {
        let p = diff::parse_unified(&diffs[i], false);
        let (files, added, removed) = p.totals();
        let c = &mut sm.cands[i];
        c.dir = PathBuf::from(format!("/Users/maya/dev/aurora-bestof-{slug}-{}", i + 1));
        c.pane = Some(100 + i);
        c.state = CandState::Finished;
        c.delivered = true;
        c.started = Some(t0);
        c.finished = Some(t0 + Duration::from_secs(secs[i]));
        c.cost = costs[i];
        c.diff = Some(DiffStat { files, added, removed });
        c.tests = tests[i].clone();
        parsed.push(Some(p));
    }
    sm.phase = Phase::Ready;
    BestRun {
        sm,
        parsed,
        busy: None,
        message: None,
    }
}
