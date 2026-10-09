//! Scenes for the workflows: the best-of-N compare view, the dock cards of a
//! write & review loop with task queues, the start wizard and the queue
//! editor. Everything is built from the real state types and drawn by the
//! real renderers (`workflow::ui`, `agents::dock`).

use super::scenes::Stage;
use crate::agents::AgentKind;
use crate::config::Theme;
use crate::renderer::font::FontManager;
use crate::ui::kit::Tone;
use crate::workflow::queue::TaskQueue;
use crate::workflow::sample;
use crate::workflow::template::{Strategy, Template};
use crate::workflow::ui::{CompareState, QueueEditor, Wizard};
use crate::workflow::{BestRun, CardInfo, Note};

/// What a scene draws on top.
pub enum Overlay {
    Compare(CompareState, Box<BestRun>),
    Wizard(Box<Wizard>),
    Queue(Box<QueueEditor>, TaskQueue),
}

impl Overlay {
    pub fn render(&mut self, buf: &mut [u32], w: usize, h: usize, font: &mut FontManager, theme: &Theme) {
        match self {
            Overlay::Compare(st, run) => st.render(buf, w, h, font, theme, run),
            Overlay::Wizard(wz) => wz.render(buf, w, h, font, theme),
            Overlay::Queue(ed, q) => ed.render(buf, w, h, font, theme, q),
        }
    }
}

/// The three candidates compared, with the agents' panes dimmed behind.
pub fn build_compare(s: &mut Stage) {
    super::mission::base(s);
    s.wm.active_tab_mut().title = "best of 3 \u{b7} add-rate-limiting".into();
    let run = sample::best_run(3);
    let uids: Vec<usize> = s.wm.active_tab().panes().iter().map(|p| p.id).collect();
    for uid in uids.iter().take(3) {
        if let Some(info) = s.agents_ui.info.get_mut(uid) {
            info.wf = CardInfo { label: Some("Best of 3 \u{b7} ready to compare".into()), ..Default::default() };
        }
    }
    let sel = run.sm.suggested();
    s.workflow = Some(Overlay::Compare(CompareState::new(run.sm.id, sel), Box::new(run)));
}

/// Dock cards of a write & review loop and two task queues.
pub fn build_dock(s: &mut Stage) {
    super::mission::base(s);
    s.window_title = "aurora \u{2014} rift".into();
    s.wm.active_tab_mut().title = "write & review \u{b7} feat/rate-limit".into();
    let uids: Vec<usize> = s.wm.active_tab().panes().iter().map(|p| p.id).collect();
    let note = Note {
        title: "Reviewer \u{b7} round 1 \u{b7} changes requested".into(),
        text: "src/middleware/rate_limit.rs:31 refills the bucket with Instant::now() twice, so a slow lock skews the rate.\nThe 429 path has no test.".into(),
        tone: Tone::Warning,
        forward: true,
    };
    // Codex is the writer here, Gemini the reviewer; Claude has a queue of its own.
    if let Some(i) = s.agents_ui.info.get_mut(&uids[1]) {
        i.wf = CardInfo { label: Some("Write & Review \u{b7} round 1/3 \u{b7} feedback ready".into()), note: Some(note.clone()), ..Default::default() };
    }
    if let Some(i) = s.agents_ui.info.get_mut(&uids[2]) {
        i.wf = CardInfo { label: Some("Reviewer \u{b7} round 1/3 \u{b7} feedback ready".into()), ..Default::default() };
    }
    if let Some(i) = s.agents_ui.info.get_mut(&uids[0]) {
        // Claude is between tasks here: no approval prompt on its card.
        i.prompt = None;
        i.risk = crate::agents::control::Risk::Safe;
        i.wf = CardInfo {
            queue: 3,
            next: Some("add a test for the 429 path (+1 line)".into()),
            countdown: Some(2),
            ..Default::default()
        };
    }
    s.agents_ui.selected = Some(uids[1]);
    s.agents_ui.compact = false;
    s.agents_ui.follow = true;
}

pub fn build_wizard(s: &mut Stage) {
    super::mission::base(s);
    let tpl = Template::new("Best of N", Strategy::BestOf);
    let installed = vec![AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Gemini];
    let mut w = Wizard::new(tpl, installed, AgentKind::ClaudeCode, "aurora \u{b7} main".into(), true, "cargo test");
    w.task.set_text("add rate limiting to /v1/orders: 60 requests per minute per API key,\nanswer 429 with a Retry-After header, and cover it with a test");
    w.kinds = vec![AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Gemini];
    w.field = 1;
    s.workflow = Some(Overlay::Wizard(Box::new(w)));
}

pub fn build_queue(s: &mut Stage) {
    super::mission::base(s);
    let uid = s.wm.active_tab().panes()[0].id;
    let mut q = TaskQueue::default();
    q.push("add a test for the 429 path");
    q.push("document the new limits in docs/api.md\nmention the Retry-After header");
    q.push("bump the version and update the changelog");
    let mut ed = QueueEditor::new(uid, "Claude Code \u{b7} aurora/feat/rate-limit".into());
    ed.sel = 1;
    s.workflow = Some(Overlay::Queue(Box::new(ed), q));
}
