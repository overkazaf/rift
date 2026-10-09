//! Hook events arriving from `rift agent-event` over the MCP socket.
//!
//! The socket server runs on its own threads; it drops events here and wakes the
//! UI thread, which drains them in `agents::runtime::poll`.

use std::collections::VecDeque;
use std::sync::Mutex;

use super::state::HookKind;
use super::AgentKind;
use crate::ai::chat::json::Json;

/// JSON-RPC method `rift agent-event` calls.
pub const METHOD: &str = "rift/agent_event";

const MAX_QUEUED: usize = 256;

/// One `rift agent-event` call.
#[derive(Clone, Debug, PartialEq)]
pub struct HookEvent {
    pub state: HookKind,
    /// `$RIFT_PANE_ID` of the calling shell, when it had one.
    pub pane_id: Option<usize>,
    pub agent: Option<AgentKind>,
    pub message: Option<String>,
    pub cwd: Option<String>,
}

static INBOX: Mutex<VecDeque<HookEvent>> = Mutex::new(VecDeque::new());

fn clip(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control() || *c == ' ').take(max).collect()
}

/// Parse the `params` of a `rift/agent_event` request.
pub fn parse_params(params: &Json) -> Result<HookEvent, String> {
    let state = params
        .get("state")
        .and_then(Json::as_str)
        .ok_or("agent_event needs params.state (working|waiting|done|idle|error)")?;
    let state = HookKind::parse(state).ok_or_else(|| format!("unknown state '{}'", clip(state, 32)))?;
    let pane_id = params
        .get("pane_id")
        .and_then(Json::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0)
        .map(|n| n as usize);
    let agent = params.get("agent").and_then(Json::as_str).and_then(AgentKind::parse);
    let message = params.get("message").and_then(Json::as_str).map(|m| clip(m, 200)).filter(|m| !m.is_empty());
    let cwd = params.get("cwd").and_then(Json::as_str).map(|c| clip(c, 1024)).filter(|c| !c.is_empty());
    Ok(HookEvent { state, pane_id, agent, message, cwd })
}

/// Queue an event and wake the UI thread.
pub fn submit(ev: HookEvent) {
    if let Ok(mut q) = INBOX.lock() {
        if q.len() >= MAX_QUEUED {
            q.pop_front();
        }
        q.push_back(ev);
    }
    crate::wake::wake();
}

/// Handle a JSON-RPC `rift/agent_event` call: validate, queue, answer.
pub fn handle_request(params: &Json) -> Result<(), String> {
    submit(parse_params(params)?);
    Ok(())
}

/// Take everything queued so far.
pub fn drain() -> Vec<HookEvent> {
    INBOX.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_params() {
        let j = Json::parse(r#"{"state":"waiting","pane_id":3,"agent":"claude","message":"needs\nyou","cwd":"/tmp/x"}"#).unwrap();
        let e = parse_params(&j).unwrap();
        assert_eq!(e.state, HookKind::Waiting);
        assert_eq!(e.pane_id, Some(3));
        assert_eq!(e.agent, Some(AgentKind::ClaudeCode));
        assert_eq!(e.message.as_deref(), Some("needsyou"));
        assert_eq!(e.cwd.as_deref(), Some("/tmp/x"));
    }

    #[test]
    fn rejects_bad_params() {
        assert!(parse_params(&Json::parse("{}").unwrap()).is_err());
        assert!(parse_params(&Json::parse(r#"{"state":"zzz"}"#).unwrap()).is_err());
        let e = parse_params(&Json::parse(r#"{"state":"done","pane_id":-1}"#).unwrap()).unwrap();
        assert_eq!(e.pane_id, None);
        let e = parse_params(&Json::parse(r#"{"state":"done","pane_id":1.5}"#).unwrap()).unwrap();
        assert_eq!(e.pane_id, None);
    }

    #[test]
    fn queue_is_bounded_and_drains() {
        // The queue is process-global and other tests may enqueue too: only look
        // at our own (very large) pane ids.
        const BASE: usize = 4_000_000;
        for i in 0..(MAX_QUEUED + 10) {
            submit(HookEvent { state: HookKind::Done, pane_id: Some(BASE + i), agent: None, message: None, cwd: None });
        }
        let mine: Vec<usize> = drain().into_iter().filter_map(|e| e.pane_id).filter(|p| *p >= BASE).collect();
        assert!(mine.len() <= MAX_QUEUED);
        assert_eq!(mine.last(), Some(&(BASE + MAX_QUEUED + 9)));
        assert!(mine.windows(2).all(|w| w[1] == w[0] + 1), "order preserved");
        assert!(drain().into_iter().all(|e| e.pane_id.map_or(true, |p| p < BASE)));
    }
}
