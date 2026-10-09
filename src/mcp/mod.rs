//! Model Context Protocol server built into Rift.
//!
//! External agents (Claude Code, Codex, Gemini CLI) can read terminal state
//! and, with the user's approval, run commands.
//!
//! ```text
//!  agent --stdio--> `rift mcp` (bridge) --unix socket--> Rift (server thread)
//!                                                          | Job + wake()
//!                                                          v
//!                                                      UI thread (host.rs)
//! ```
//!
//! * [`bridge`]   `rift mcp`: pipes stdin/stdout to the socket.
//! * [`server`]   Unix socket listener (0600, peer-UID checked), one thread per
//!                client, newline-delimited JSON-RPC 2.0, rate limited.
//! * [`protocol`] MCP framing/dispatch; pure, driven by a [`Backend`].
//! * [`tools`]    Tool/resource definitions and their read-only implementations.
//! * [`approval`] Race-free state machine behind `run_command` approvals.
//! * [`host`]     UI-thread glue: job queue, approval modal, `Backend` over a channel.
//! * [`overlay`]  "MCP Activity" overlay.
//!
//! Everything an agent sees is passed through `secret_mask::redact` and size
//! capped. Requests that need app state never block the UI thread: they are
//! queued, the event loop is woken, and the answer travels back on a channel.

pub mod approval;
pub mod bridge;
pub mod host;
pub mod overlay;
pub mod protocol;
pub mod server;
pub mod tools;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// `[mcp] allow_run`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AllowRun {
    /// Every `run_command` needs an explicit Run click.
    Ask,
    /// `run_command` is not offered at all.
    Never,
}

impl AllowRun {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ask" | "prompt" => Some(Self::Ask),
            "never" | "off" | "deny" | "no" => Some(Self::Never),
            _ => None,
        }
    }
}

/// `[mcp]` section of config.toml.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpConfig {
    /// Serve the MCP socket at all (read-only tools).
    pub enabled: bool,
    pub allow_run: AllowRun,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self { enabled: true, allow_run: AllowRun::Ask }
    }
}

/// Newest protocol revision we speak, then the older ones we can fall back to.
pub const LATEST_PROTOCOL: &str = "2025-06-18";
pub const SUPPORTED_PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Hard cap on one JSON-RPC line (request) from a client.
pub const MAX_LINE_BYTES: usize = 1 << 20;
/// Requests per second one client may make.
pub const RATE_PER_SEC: u32 = 20;
/// Concurrent clients served.
pub const MAX_CLIENTS: usize = 8;
/// `read_block` output cap.
pub const MAX_BLOCK_BYTES: usize = 64 * 1024;
/// `read_pane` / search output cap.
pub const MAX_TEXT_BYTES: usize = 256 * 1024;

/// Socket location: `$RIFT_MCP_SOCK`, else `~/.config/rift/mcp.sock`.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("RIFT_MCP_SOCK").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    match dirs::home_dir() {
        Some(h) => h.join(".config").join("rift").join("mcp.sock"),
        None => PathBuf::from("/tmp").join(format!("rift-mcp-{}.sock", current_uid())),
    }
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}
#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Ok,
    Error,
    /// `run_command` refused by the user (or never allowed).
    Denied,
    RateLimited,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Error => "error",
            Outcome::Denied => "denied",
            Outcome::RateLimited => "rate-limited",
        }
    }
}

/// One line of the "MCP Activity" log.
#[derive(Clone, Debug)]
pub struct Activity {
    pub at: Instant,
    pub client: u64,
    pub tool: String,
    /// Redacted, truncated argument summary.
    pub summary: String,
    pub outcome: Outcome,
    pub ms: u64,
}

const MAX_ACTIVITY: usize = 200;

/// State shared between the server threads and the UI.
pub struct Shared {
    pub allow_run: AllowRun,
    clients: AtomicUsize,
    next_client: AtomicU64,
    activity: Mutex<VecDeque<Activity>>,
}

impl Shared {
    pub fn new(allow_run: AllowRun) -> Self {
        Self {
            allow_run,
            clients: AtomicUsize::new(0),
            next_client: AtomicU64::new(1),
            activity: Mutex::new(VecDeque::new()),
        }
    }

    pub fn clients(&self) -> usize {
        self.clients.load(Ordering::Relaxed)
    }

    pub(crate) fn client_connected(&self) -> u64 {
        self.clients.fetch_add(1, Ordering::Relaxed);
        self.next_client.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn client_gone(&self) {
        let _ = self.clients.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| Some(n.saturating_sub(1)));
    }

    pub fn log(&self, a: Activity) {
        if let Ok(mut g) = self.activity.lock() {
            if g.len() >= MAX_ACTIVITY {
                g.pop_front();
            }
            g.push_back(a);
        }
    }

    /// Newest first.
    pub fn recent(&self, n: usize) -> Vec<Activity> {
        self.activity.lock().map(|g| g.iter().rev().take(n).cloned().collect()).unwrap_or_default()
    }

    pub fn activity_len(&self) -> usize {
        self.activity.lock().map(|g| g.len()).unwrap_or(0)
    }
}

/// What the protocol layer asks the application to do. Arguments are already
/// validated and clamped.
#[derive(Clone, Debug, PartialEq)]
pub enum AppRequest {
    ListPanes,
    ReadPane { pane_id: usize, lines: usize, include_scrollback: bool },
    ListBlocks { pane_id: usize, limit: usize },
    ReadBlock { pane_id: usize, block_index: usize },
    SearchScrollback { pane_id: usize, query: String, limit: usize, context: usize },
    RunCommand { pane_id: usize, command: String },
    ListResources,
    ReadResource { pane_id: usize, kind: tools::ResourceKind },
}

/// An answer from the application. For tools `is_error` becomes `isError`;
/// for resource reads it becomes a JSON-RPC error.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub text: String,
    pub is_error: bool,
}

impl Reply {
    pub fn ok(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: false }
    }
    pub fn err(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: true }
    }
}

/// The application side. Blocking; called from client threads, never the UI
/// thread. The real implementation is [`host::ChannelBackend`]; tests use fakes.
pub trait Backend: Send + Sync {
    fn call(&self, req: AppRequest) -> Reply;
}
