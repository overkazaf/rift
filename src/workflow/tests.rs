//! Glue-level tests that need no `App`: dock card data, the judge request and
//! queue bookkeeping on `Workflows`.

use std::time::{Duration, Instant};

use super::*;
use crate::ui::kit::Tone;

fn review_run(id: u64) -> ReviewRun {
    ReviewRun { sm: WriteReview::new(id, "add a retry", 10, 11, 3, false), waiting_range: None }
}

#[test]
fn queue_label_and_countdown_on_the_card() {
    let mut wf = Workflows::new();
    let now = Instant::now();
    assert_eq!(wf.card_info(10, now), CardInfo::default());
    wf.queue_mut(10).push("write the migration\nand the rollback");
    wf.queue_mut(10).push("update the docs");
    let ci = wf.card_info(10, now);
    assert_eq!((ci.queue, ci.paused), (2, false));
    assert_eq!(ci.next.as_deref(), Some("write the migration (+1 line)"));
    wf.countdown = Some(Countdown::start(10, "write the migration", now));
    let ci = wf.card_info(10, now + Duration::from_millis(500));
    assert_eq!(ci.countdown, Some(3));
    assert_eq!(wf.card_info(11, now).countdown, None, "other cards do not count down");
    let lines = card_lines(&ci, 60, 3);
    assert_eq!(lines[0], ("queue 2".to_string(), Tone::Accent));
    assert!(lines[1].0.starts_with("next in 3s: write the migration (+1 line)") && lines[1].0.contains("Esc pauses"), "{lines:?}");
    wf.queue_mut(10).paused = true;
    let lines = card_lines(&wf.card_info(10, now), 60, 3);
    assert_eq!(lines[0], ("queue 2 paused".to_string(), Tone::Warning));
    wf.queue_mut(10).clear();
    wf.drop_empty_queue(10);
    assert!(wf.queues.contains_key(&10), "a paused queue is kept");
    wf.queue_mut(10).paused = false;
    wf.drop_empty_queue(10);
    assert!(!wf.queues.contains_key(&10));
}

#[test]
fn reviewer_feedback_becomes_a_card_note_with_a_forward_hint() {
    let mut wf = Workflows::new();
    let mut r = review_run(5);
    r.sm.on_event(review_loop::Ev::WriterSent);
    r.sm.on_event(review_loop::Ev::WriterFinished);
    r.sm.on_event(review_loop::Ev::DiffReady(Some("diff --git a/x b/x\n+y\n".into())));
    r.sm.on_event(review_loop::Ev::ReviewerSent);
    r.sm.on_event(review_loop::Ev::ReviewerFinished("src/retry.rs:12 sleeps without a cap\nVERDICT: CHANGES REQUESTED".into()));
    wf.runs.push(Run::Review(r));
    let now = Instant::now();
    let writer = wf.card_info(10, now);
    assert_eq!(writer.label.as_deref(), Some("Write & Review \u{b7} round 1/3 \u{b7} feedback ready"));
    let note = writer.note.clone().unwrap();
    assert!(note.forward && note.text.contains("sleeps without a cap") && !note.text.contains("VERDICT"));
    assert_eq!(note.tone, Tone::Warning);
    let reviewer = wf.card_info(11, now);
    assert_eq!(reviewer.label.as_deref(), Some("Reviewer \u{b7} round 1/3 \u{b7} feedback ready"));
    assert!(!reviewer.note.unwrap().forward, "only the writer's card forwards");
    let lines = card_lines(&writer, 50, 3);
    assert!(lines.iter().any(|(t, _)| t.starts_with("Reviewer \u{b7} round 1 \u{b7} changes requested")));
    assert!(lines[1].0.contains("forward") && lines[1].1 == Tone::Success, "the action is the second line: {lines:?}");
    // Both panes belong to the loop, so their queues must not fire.
    assert!(wf.managed(10) && wf.managed(11) && !wf.managed(12));
    if let Some(Run::Review(r)) = wf.run_mut(5) {
        r.sm.on_event(review_loop::Ev::Stop);
    }
    assert!(!wf.managed(10), "a finished loop releases its panes");
    assert!(wf.card_info(10, now).label.unwrap().ends_with("stopped"));
}

#[test]
fn best_of_cards_show_progress() {
    let mut wf = Workflows::new();
    let mut run = sample::best_run(3);
    run.sm.phase = bestof::Phase::Running;
    run.sm.cands[1].state = bestof::CandState::Working;
    wf.runs.push(Run::Best(run));
    let ci = wf.card_info(100, Instant::now());
    assert_eq!(ci.label.as_deref(), Some("Best of 3 \u{b7} 2/3 finished"));
    assert!(wf.latest_ready_best().is_none());
    if let Some(Run::Best(b)) = wf.run_mut(1) {
        b.sm.phase = bestof::Phase::Ready;
    }
    assert_eq!(wf.latest_ready_best(), Some(1));
    assert!(wf.card_info(100, Instant::now()).note.unwrap().text.contains("Compare Candidates"));
}

#[test]
fn fix_loop_card_shows_the_failure_tail() {
    let mut wf = Workflows::new();
    let mut f = FixRun { sm: FixTests::new(9, 20, "", "cargo test", 3), dir: "/w".into() };
    f.sm.on_event(fixtests::Ev::TestsDone(Err("a\nb\nthe real error".into())));
    wf.runs.push(Run::Fix(f));
    let ci = wf.card_info(20, Instant::now());
    assert_eq!(ci.label.as_deref(), Some("Fix tests \u{b7} attempt 1/3 \u{b7} agent fixing"));
    assert!(ci.note.unwrap().text.ends_with("the real error"));
}

#[test]
fn judge_request_carries_every_candidate_and_redacts_secrets() {
    let mut run = sample::best_run(3);
    run.sm.cands[1].removed = true;
    let req = actions::judge_request(&run);
    assert!(req.question.contains("add rate limiting to /v1/orders") && req.question.contains("Pick the one to merge"));
    assert_eq!(req.context.len(), 2, "removed candidates are skipped");
    let built = req.build_prompt();
    assert!(built.text.contains("Candidate 1 of 3: Claude Code") && built.text.contains("Candidate 3 of 3: Gemini CLI"));
    assert!(built.text.contains("tests passed") && built.text.contains("RateLimiter"));
    assert!(built.text.contains("<terminal_output") || built.text.contains("terminal_output"), "diffs are fenced as untrusted text");
    // A key in a diff never leaves the machine unredacted.
    let mut run = sample::best_run(2);
    let secret = "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789AbCdEfGhIjKl";
    run.parsed[0] = Some(crate::review::diff::parse_unified(&format!("diff --git a/.env b/.env\nnew file mode 100644\n--- /dev/null\n+++ b/.env\n@@ -0,0 +1,1 @@\n+API_KEY={secret}\n"), false));
    let built = actions::judge_request(&run).build_prompt();
    assert!(!built.text.contains(secret), "secret leaked into the prompt");
    assert!(built.redacted >= 1);
}

#[test]
fn queue_persistence_for_the_session_is_per_leaf() {
    let mut wf = Workflows::new();
    wf.queue_mut(1).push("a");
    wf.queue_mut(1).push("b");
    wf.queue_mut(3).push("c");
    wf.queue_mut(4).paused = true; // empty queues are not saved
    let wm = crate::window::WindowManager::headless(80, 24);
    let ids: Vec<usize> = wm.tabs.iter().flat_map(|t| t.panes()).map(|p| p.id).collect();
    assert!(!ids.is_empty());
    let per_tab = wf.queues_for_session(&wm);
    assert_eq!(per_tab.len(), wm.tabs.len());
    assert!(per_tab[0].iter().all(|(leaf, q)| *leaf == 0 && !q.is_empty()) || per_tab[0].is_empty());
}
