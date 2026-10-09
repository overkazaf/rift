//! UI-thread side of the MCP server.
//!
//! Client threads call [`ChannelBackend`], which queues a [`Job`], wakes the
//! event loop and waits (on the *client* thread) for the answer. The UI thread
//! drains the queue in `about_to_wait` via [`poll`]; read-only jobs are
//! answered immediately, `run_command` becomes a confirm modal whose answer
//! is delivered later by [`finish_run`].

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use super::approval::{self, ApprovalCell, Danger, PaneState, APPROVAL_TIMEOUT};
use super::tools;
use super::{AllowRun, AppRequest, Backend, McpConfig, Reply, Shared};
use crate::ai::chat::json::quote;
use crate::app::App;
use crate::ui::confirm::{ConfirmAction, ConfirmRequest};
use crate::ui::kit::Tone;
use crate::window::WindowManager;

/// How long a client waits for the UI thread to answer a read request.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Jobs handled per event-loop turn (keeps a flood from starving rendering).
const JOBS_PER_TURN: usize = 16;

/// A request travelling client thread -> UI thread.
pub struct Job {
    pub req: AppRequest,
    pub reply: Sender<Reply>,
    /// Present exactly for `run_command`.
    pub approval: Option<Arc<ApprovalCell>>,
}

/// [`Backend`] that forwards to the UI thread.
pub struct ChannelBackend {
    tx: Sender<Job>,
    read_timeout: Duration,
    approval_timeout: Duration,
}

impl ChannelBackend {
    pub fn new(tx: Sender<Job>) -> Self {
        Self { tx, read_timeout: READ_TIMEOUT, approval_timeout: APPROVAL_TIMEOUT }
    }

    #[cfg(test)]
    fn with_timeouts(tx: Sender<Job>, read: Duration, approval: Duration) -> Self {
        Self { tx, read_timeout: read, approval_timeout: approval }
    }
}

pub fn status_reply(status: &str, message: &str) -> Reply {
    Reply::ok(format!(r#"{{"status":{},"message":{}}}"#, quote(status), quote(message)))
}

impl Backend for ChannelBackend {
    fn call(&self, req: AppRequest) -> Reply {
        let approval = matches!(req, AppRequest::RunCommand { .. }).then(ApprovalCell::new);
        let (reply, rx) = channel();
        if self.tx.send(Job { req, reply, approval: approval.clone() }).is_err() {
            return Reply::err("Rift is shutting down");
        }
        crate::wake::wake();
        let timeout = if approval.is_some() { self.approval_timeout } else { self.read_timeout };
        match rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => match approval {
                // Still undecided: withdraw, so a late click cannot run it.
                Some(cell) if cell.abandon() => status_reply("expired", "The user did not answer in time; nothing was run."),
                // The user answered in the same instant; their reply is on its way.
                Some(_) => rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| Reply::err("Rift did not deliver the decision")),
                None => Reply::err("Rift's UI did not respond in time"),
            },
            Err(RecvTimeoutError::Disconnected) => Reply::err("Rift dropped the request"),
        }
    }
}

/// State owned by `App`.
pub struct UiState {
    pub shared: Option<Arc<Shared>>,
    rx: Option<Receiver<Job>>,
    #[cfg(unix)]
    _server: Option<super::server::ServerHandle>,
    pending: Arc<AtomicUsize>,
    pub overlay: super::overlay::Overlay,
    /// Why the server is not running (shown in the activity overlay).
    pub status: String,
}

impl UiState {
    pub fn off(status: impl Into<String>) -> Self {
        Self {
            shared: None,
            rx: None,
            #[cfg(unix)]
            _server: None,
            pending: Arc::new(AtomicUsize::new(0)),
            overlay: super::overlay::Overlay::default(),
            status: status.into(),
        }
    }

    /// Connected clients (for the tab-bar indicator).
    pub fn clients(&self) -> usize {
        self.shared.as_ref().map_or(0, |s| s.clients())
    }
}

/// Start the socket server if `[mcp] enabled`.
pub fn start(cfg: &McpConfig) -> UiState {
    if !cfg.enabled {
        return UiState::off("disabled ([mcp] enabled = false)");
    }
    #[cfg(unix)]
    {
        let path = super::socket_path();
        let shared = Arc::new(Shared::new(cfg.allow_run));
        let (tx, rx) = channel();
        let backend: Arc<dyn Backend> = Arc::new(ChannelBackend::new(tx));
        match super::server::start(&path, backend, shared.clone()) {
            Ok(handle) => {
                log::info!("MCP server listening on {}", path.display());
                let mut s = UiState::off(format!("listening on {}", path.display()));
                s.shared = Some(shared);
                s.rx = Some(rx);
                s._server = Some(handle);
                s
            }
            Err(e) => {
                log::warn!("MCP server not started: {e}");
                UiState::off(format!("not started: {e}"))
            }
        }
    }
    #[cfg(not(unix))]
    {
        UiState::off("unsupported on this platform")
    }
}

// ---- UI-thread job handling -------------------------------------------------------

/// Drain queued jobs. Called from `about_to_wait`; never blocks.
pub fn poll(app: &mut App) {
    let mut jobs = Vec::new();
    if let Some(rx) = &app.mcp.rx {
        while jobs.len() < JOBS_PER_TURN {
            match rx.try_recv() {
                Ok(j) => jobs.push(j),
                Err(_) => break,
            }
        }
    }
    let more = jobs.len() == JOBS_PER_TURN;
    for job in jobs {
        handle_job(app, job);
    }
    if more {
        crate::wake::wake();
    }
}

fn handle_job(app: &mut App, job: Job) {
    let Job { req, reply, approval } = job;
    if let AppRequest::RunCommand { pane_id, command } = req {
        let cell = approval.unwrap_or_else(ApprovalCell::new);
        start_run(app, reply, cell, pane_id, command);
        return;
    }
    let r = tools::answer_read(&app.wm, &req).unwrap_or_else(|| Reply::err("unsupported request"));
    let _ = reply.send(r);
}

/// What the UI thread currently knows about a pane, for vetting.
pub fn pane_state(wm: &WindowManager, pane_id: usize) -> Option<PaneState> {
    let p = tools::pane_ref(wm, pane_id)?;
    let t = &p.terminal;
    Some(PaneState {
        exited: p.exited.is_some(),
        alt_screen: t.is_alt_screen(),
        busy: t.blocks.is_running(),
        typing: t.pending_command_line().is_some(),
    })
}

/// A `run_command` waiting for the user.
pub struct PendingRun {
    pub cell: Arc<ApprovalCell>,
    pub reply: Sender<Reply>,
    pub pane_id: usize,
    pub command: String,
    /// Index of the "Run" button (0 normally; 1 for critical commands so a
    /// reflexive "1" or Enter on the default does not run them).
    pub run_index: usize,
    counter: Arc<AtomicUsize>,
}

impl Drop for PendingRun {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Everything the confirm modal shows (pure, so it can be tested).
#[derive(Debug, PartialEq)]
pub struct ModalSpec {
    pub title: String,
    pub badge: Option<(String, Tone)>,
    pub lines: Vec<String>,
    pub buttons: Vec<String>,
    pub default_sel: usize,
    pub run_index: usize,
    pub tone: Tone,
}

pub fn modal_spec(pane_id: usize, pane_title: Option<&str>, cwd: Option<&str>, command: &str, danger: Option<&Danger>) -> ModalSpec {
    let pane = match pane_title.filter(|t| !t.is_empty()) {
        Some(t) => format!("pane {pane_id} ({t})"),
        None => format!("pane {pane_id}"),
    };
    let mut lines = vec![format!("Agent wants to run `{command}` in {pane}")];
    if let Some(c) = cwd {
        lines.push(format!("Directory: {c}"));
    }
    let (title, badge, tone, buttons, run_index, default_sel) = match danger {
        None => ("Run command from MCP client?", Some(("MCP".to_string(), Tone::Accent)), Tone::Warning, vec!["Run", "Deny"], 0, 1),
        Some(d) if !d.critical => {
            lines.push("This command looks risky:".into());
            lines.extend(d.impacts.iter().map(|i| format!("  - {i}")));
            ("Run risky command from MCP client?", Some(("RISKY".to_string(), Tone::Warning)), Tone::Warning, vec!["Run anyway", "Deny"], 0, 1)
        }
        Some(d) => {
            lines.push("CRITICAL: this command can cause serious or irreversible damage:".into());
            lines.extend(d.impacts.iter().map(|i| format!("  - {i}")));
            // Deny first: pressing 1 / Enter on the default never runs it.
            ("Run CRITICAL command from MCP client?", Some(("CRITICAL".to_string(), Tone::Danger)), Tone::Danger, vec!["Deny", "Run anyway"], 1, 0)
        }
    };
    lines.push("Nothing runs unless you choose Run. The agent sees the command's output (secrets redacted).".into());
    ModalSpec {
        title: title.into(),
        badge,
        lines,
        buttons: buttons.into_iter().map(String::from).collect(),
        default_sel,
        run_index,
        tone,
    }
}

fn start_run(app: &mut App, reply: Sender<Reply>, cell: Arc<ApprovalCell>, pane_id: usize, command: String) {
    // Tolerate surrounding whitespace/newline; what is reviewed is what is typed.
    let command = command.trim().to_string();
    let refuse = |cell: &ApprovalCell, reply: &Sender<Reply>, msg: &str| {
        cell.decide(false);
        let _ = reply.send(Reply::err(msg));
    };
    if app.config.mcp.allow_run == AllowRun::Never {
        return refuse(&cell, &reply, "run_command is disabled ([mcp] allow_run = \"never\")");
    }
    let pending = app.mcp.pending.load(Ordering::SeqCst);
    if let Err(msg) = approval::vet(&command, pane_state(&app.wm, pane_id), pending) {
        return refuse(&cell, &reply, &msg);
    }
    let (title, cwd) = match tools::pane_ref(&app.wm, pane_id) {
        Some(p) => (p.title().map(str::to_string), p.terminal.cwd.clone()),
        None => return refuse(&cell, &reply, "no such pane"),
    };
    let danger = approval::assess(&command, cwd.as_deref());
    let spec = modal_spec(pane_id, title.as_deref(), cwd.as_deref(), &command, danger.as_ref());
    app.mcp.pending.fetch_add(1, Ordering::SeqCst);
    let run = PendingRun { cell, reply, pane_id, command, run_index: spec.run_index, counter: app.mcp.pending.clone() };
    let deny_index = 1 - spec.run_index;
    app.confirm.push(ConfirmRequest {
        title: spec.title,
        badge: spec.badge,
        lines: spec.lines,
        buttons: spec.buttons,
        default_sel: spec.default_sel,
        esc_choice: Some(deny_index),
        tone: spec.tone,
        action: ConfirmAction::McpRun(Box::new(run)),
    });
    if let Some(w) = &app.window {
        let _ = w.request_user_attention(Some(winit::window::UserAttentionType::Informational));
    }
    app.request_redraw();
}

/// Type `command` + Enter into the pane. Returns the index the command's
/// block will have (when shell integration lets us know).
pub fn execute_run(wm: &mut WindowManager, pane_id: usize, command: &str) -> Result<Option<usize>, String> {
    let (ti, pi) = tools::locate(wm, pane_id).ok_or("the pane no longer exists")?;
    let pane = wm.tabs[ti].pane_mut(pi).ok_or("the pane no longer exists")?;
    let index = pane.terminal.blocks.osc_seen().then(|| pane.terminal.blocks.block_count());
    pane.terminal.scroll_to_bottom();
    pane.write(format!("{command}\r").as_bytes());
    Ok(index)
}

/// Apply the user's answer: the state machine decides, then (and only then) the
/// command is typed. Returns the reply for the waiting client.
pub fn resolve_run(wm: &mut WindowManager, run: &PendingRun, choice: Option<usize>) -> Reply {
    let approve = choice == Some(run.run_index);
    if !run.cell.decide(approve) {
        // The client already gave up (or this was answered before): run nothing.
        return status_reply("expired", "The request was withdrawn before the decision; nothing was run.");
    }
    if !approve {
        return status_reply("denied", "The user denied this command; nothing was run.");
    }
    // Time has passed: re-check the pane before typing into it.
    if let Err(msg) = approval::vet(&run.command, pane_state(wm, run.pane_id), 0) {
        return Reply::err(format!("The user approved, but the command was not run: {msg}"));
    }
    match execute_run(wm, run.pane_id, &run.command) {
        Ok(index) => Reply::ok(format!(
            r#"{{"status":"approved","pane_id":{},"command":{},"block_index":{},"message":{}}}"#,
            run.pane_id,
            quote(&run.command),
            index.map_or("null".to_string(), |i| i.to_string()),
            quote(if index.is_some() {
                "Approved and sent. Poll list_blocks / read_block with block_index until running is false."
            } else {
                "Approved and sent. Shell integration (OSC 133) is not active in this pane, so there is no block to poll; use read_pane."
            }),
        )),
        Err(e) => Reply::err(format!("The user approved, but the command was not run: {e}")),
    }
}

/// Called by `ui::confirm::resolve` when the modal is answered.
pub fn finish_run(app: &mut App, run: Box<PendingRun>, choice: Option<usize>) {
    let reply = resolve_run(&mut app.wm, &run, choice);
    let approved = reply.text.contains(r#""status":"approved""#);
    let _ = run.reply.send(reply);
    if approved {
        app.blocks_ui.show_toast("Ran command from MCP client");
    }
    app.request_redraw();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::json::Json;

    fn wm() -> WindowManager {
        WindowManager::headless(80, 10)
    }

    fn pending(command: &str, run_index: usize) -> (PendingRun, Receiver<Reply>, Arc<AtomicUsize>) {
        let (tx, rx) = channel();
        let counter = Arc::new(AtomicUsize::new(1));
        let run = PendingRun {
            cell: ApprovalCell::new(),
            reply: tx,
            pane_id: 0,
            command: command.to_string(),
            run_index,
            counter: counter.clone(),
        };
        (run, rx, counter)
    }

    fn status(r: &Reply) -> String {
        Json::parse(&r.text).and_then(|j| j.get("status").and_then(Json::as_str).map(String::from)).unwrap_or_default()
    }

    #[test]
    fn modal_for_a_routine_command_defaults_to_deny() {
        let m = modal_spec(3, Some("zsh"), Some("/tmp"), "ls -la", None);
        assert_eq!(m.buttons, ["Run", "Deny"]);
        assert_eq!(m.default_sel, 1);
        assert_eq!(m.run_index, 0);
        assert!(m.lines[0].contains("`ls -la`") && m.lines[0].contains("pane 3"));
        assert_eq!(m.tone, Tone::Warning);
    }

    #[test]
    fn modal_for_a_critical_command_puts_deny_first_and_lists_impacts() {
        let d = approval::assess("rm -rf /", None).expect("critical");
        let m = modal_spec(1, None, None, "rm -rf /", Some(&d));
        assert_eq!(m.buttons, ["Deny", "Run anyway"]);
        assert_eq!((m.default_sel, m.run_index), (0, 1));
        assert_eq!(m.tone, Tone::Danger);
        assert!(m.lines.iter().any(|l| l.contains("CRITICAL")));
        assert!(m.lines.len() >= 4);
    }

    #[test]
    fn approved_command_is_typed_once_and_reports_the_block_to_poll() {
        let mut wm = wm();
        wm.tabs[0].pane_mut(0).unwrap().feed(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        let (run, _rx, _c) = pending("echo hi", 0);
        let r = resolve_run(&mut wm, &run, Some(0));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(status(&r), "approved");
        let j = Json::parse(&r.text).unwrap();
        assert_eq!(j.get("block_index").and_then(Json::as_f64), Some(0.0));
        assert_eq!(j.get("command").and_then(Json::as_str), Some("echo hi"));
        // Answering again does nothing (the decision was already made).
        assert_eq!(status(&resolve_run(&mut wm, &run, Some(0))), "expired");
    }

    #[test]
    fn denial_dismissal_and_wrong_button_never_run() {
        let mut wm = wm();
        for choice in [Some(1), None] {
            let (run, _rx, _c) = pending("echo hi", 0);
            assert_eq!(status(&resolve_run(&mut wm, &run, choice)), "denied");
            assert_eq!(run.cell.phase(), approval::Phase::Denied);
        }
        // Critical layout: button 0 is Deny, so choosing 0 is a denial.
        let (run, _rx, _c) = pending("rm -rf /", 1);
        assert_eq!(status(&resolve_run(&mut wm, &run, Some(0))), "denied");
        let (run, _rx, _c) = pending("rm -rf /", 1);
        assert_eq!(status(&resolve_run(&mut wm, &run, Some(1))), "approved");
    }

    #[test]
    fn withdrawn_request_cannot_be_approved_later() {
        let mut wm = wm();
        let (run, _rx, _c) = pending("echo hi", 0);
        assert!(run.cell.abandon(), "client timed out first");
        let r = resolve_run(&mut wm, &run, Some(0));
        assert_eq!(status(&r), "expired");
    }

    #[test]
    fn approval_rechecks_the_pane_before_typing() {
        let mut wm = wm();
        let (run, _rx, _c) = pending("echo hi", 0);
        // Between request and click a long command started in the pane.
        wm.tabs[0].pane_mut(0).unwrap().feed(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 9\r\n\x1b]133;C\x07");
        let r = resolve_run(&mut wm, &run, Some(0));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("still running"), "{}", r.text);
    }

    #[test]
    fn pending_counter_is_released_when_the_request_is_dropped() {
        let (run, _rx, counter) = pending("ls", 0);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        drop(run);
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    /// A stand-in UI thread: answers read jobs from a headless WindowManager.
    fn fake_ui(rx: Receiver<Job>, mut wm: WindowManager) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut unanswered = Vec::new();
            while let Ok(job) = rx.recv_timeout(Duration::from_millis(500)) {
                if let AppRequest::RunCommand { .. } = job.req {
                    unanswered.push(job); // the human never answers (reply sender stays alive)
                    continue;
                }
                let r = tools::answer_read(&wm, &job.req).unwrap();
                let _ = job.reply.send(r);
                let _ = &mut wm;
            }
        })
    }

    #[test]
    fn channel_backend_round_trips_reads_and_expires_unanswered_approvals() {
        let (tx, rx) = channel();
        let mut w = wm();
        w.tabs[0].pane_mut(0).unwrap().feed(b"hello");
        let ui = fake_ui(rx, w);
        let b = ChannelBackend::with_timeouts(tx, Duration::from_secs(5), Duration::from_millis(100));
        let r = b.call(AppRequest::ReadPane { pane_id: 0, lines: 10, include_scrollback: false });
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("hello"));
        let r = b.call(AppRequest::RunCommand { pane_id: 0, command: "ls".into() });
        assert_eq!(status(&r), "expired");
        drop(b);
        ui.join().unwrap();
    }

    #[test]
    fn channel_backend_reports_a_dead_ui() {
        let (tx, rx) = channel();
        drop(rx);
        let b = ChannelBackend::new(tx);
        assert!(b.call(AppRequest::ListPanes).is_error);
    }
}
