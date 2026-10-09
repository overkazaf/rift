//! Writer + Reviewer: agent A writes, agent B reviews each finished turn and
//! its feedback goes back to A. Pure state machine (events in, commands out),
//! plus the prompts and the reply extraction, all tested here.
//!
//! Round `k`: writer turn finishes with changes -> its diff goes to the
//! reviewer -> the reviewer's reply is shown as a note on the writer's card ->
//! `f` (or `auto_forward`) sends it to the writer, which starts round `k+1`.
//! The loop ends on APPROVED, after `max_rounds` reviews, on Stop, or when a
//! turn leaves no changes to review.

/// What the reviewer said, read from its closing "VERDICT:" line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Approved,
    ChangesRequested,
    /// No recognisable verdict line: the user decides.
    Unclear,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Approved,
    MaxRounds,
    Stopped,
    /// The writer's turn changed nothing, so there is nothing to review.
    NoChanges,
    /// An agent pane closed or the agent quit.
    Gone(String),
}

impl Outcome {
    pub fn label(&self) -> String {
        match self {
            Outcome::Approved => "approved by the reviewer".into(),
            Outcome::MaxRounds => "stopped at the round limit".into(),
            Outcome::Stopped => "stopped".into(),
            Outcome::NoChanges => "no changes to review".into(),
            Outcome::Gone(w) => format!("ended: {w}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Waiting for the writer to finish a turn.
    Writing,
    /// The turn is over; its diff is being computed.
    AwaitDiff,
    /// The reviewer has the diff.
    Reviewing,
    /// Feedback is ready; waiting for `f` (or auto-forward).
    Feedback,
    Done,
}

/// Feedback from one review round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Feedback {
    pub round: u32,
    pub text: String,
    pub verdict: Verdict,
}

pub enum Ev {
    /// The writer got its (first or follow-up) prompt.
    WriterSent,
    /// The writer finished a turn.
    WriterFinished,
    /// The writer's turn diff: `None` or empty = no changes.
    DiffReady(Option<String>),
    /// The reviewer got the review prompt.
    ReviewerSent,
    /// The reviewer finished; its extracted reply.
    ReviewerFinished(String),
    Forward,
    Stop,
    /// Pane `uid` closed or its agent exited.
    PaneGone(usize),
    /// Something outside the loop failed (the turn's diff could not be read).
    Fail(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cmd {
    ComputeDiff,
    SendReviewer(String),
    SendWriter(String),
    Finish(Outcome),
}

#[derive(Clone, Debug)]
pub struct WriteReview {
    pub id: u64,
    pub task: String,
    pub writer: usize,
    pub reviewer: usize,
    pub max_rounds: u32,
    pub auto_forward: bool,
    /// Current round, 1-based.
    pub round: u32,
    pub phase: Phase,
    pub feedback: Option<Feedback>,
    pub outcome: Option<Outcome>,
    writer_sent: bool,
    reviewer_sent: bool,
}

impl WriteReview {
    pub fn new(id: u64, task: &str, writer: usize, reviewer: usize, max_rounds: u32, auto_forward: bool) -> WriteReview {
        WriteReview {
            id,
            task: task.to_string(),
            writer,
            reviewer,
            max_rounds: max_rounds.max(1),
            auto_forward,
            round: 1,
            phase: Phase::Writing,
            feedback: None,
            outcome: None,
            writer_sent: false,
            reviewer_sent: false,
        }
    }

    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    pub fn panes(&self) -> [usize; 2] {
        [self.writer, self.reviewer]
    }

    fn finish(&mut self, o: Outcome) -> Vec<Cmd> {
        if self.phase == Phase::Done {
            return Vec::new();
        }
        self.phase = Phase::Done;
        self.outcome = Some(o.clone());
        vec![Cmd::Finish(o)]
    }

    pub fn on_event(&mut self, ev: Ev) -> Vec<Cmd> {
        if self.phase == Phase::Done {
            return Vec::new();
        }
        match ev {
            Ev::Stop => self.finish(Outcome::Stopped),
            Ev::Fail(msg) => self.finish(Outcome::Gone(msg)),
            Ev::PaneGone(uid) => {
                let who = if uid == self.writer { "the writer" } else if uid == self.reviewer { "the reviewer" } else { return Vec::new() };
                self.finish(Outcome::Gone(format!("{who} is gone")))
            }
            Ev::WriterSent => {
                self.writer_sent = true;
                Vec::new()
            }
            Ev::ReviewerSent => {
                self.reviewer_sent = true;
                Vec::new()
            }
            Ev::WriterFinished => {
                if self.phase != Phase::Writing || !self.writer_sent {
                    return Vec::new();
                }
                self.writer_sent = false;
                self.phase = Phase::AwaitDiff;
                vec![Cmd::ComputeDiff]
            }
            Ev::DiffReady(diff) => {
                if self.phase != Phase::AwaitDiff {
                    return Vec::new();
                }
                match diff.filter(|d| !d.trim().is_empty()) {
                    None => self.finish(Outcome::NoChanges),
                    Some(d) => {
                        self.phase = Phase::Reviewing;
                        vec![Cmd::SendReviewer(review_prompt(&self.task, self.round, self.max_rounds, &d))]
                    }
                }
            }
            Ev::ReviewerFinished(reply) => {
                if self.phase != Phase::Reviewing || !self.reviewer_sent {
                    return Vec::new();
                }
                self.reviewer_sent = false;
                let verdict = parse_verdict(&reply);
                self.feedback = Some(Feedback { round: self.round, text: strip_verdict(&reply), verdict });
                if verdict == Verdict::Approved {
                    return self.finish(Outcome::Approved);
                }
                if self.round >= self.max_rounds {
                    return self.finish(Outcome::MaxRounds);
                }
                self.phase = Phase::Feedback;
                if self.auto_forward {
                    return self.on_event(Ev::Forward);
                }
                Vec::new()
            }
            Ev::Forward => {
                if self.phase != Phase::Feedback {
                    return Vec::new();
                }
                let Some(fb) = self.feedback.clone() else { return Vec::new() };
                self.round += 1;
                self.phase = Phase::Writing;
                vec![Cmd::SendWriter(feedback_prompt(&fb.text, fb.round, self.max_rounds))]
            }
        }
    }
}

// ───────────────────────────── prompts ─────────────────────────────

/// Largest diff pasted into the reviewer prompt.
pub const MAX_DIFF_BYTES: usize = 16 * 1024;

/// Marker that opens a review prompt: the reply is read from below its last occurrence.
pub fn marker(round: u32) -> String {
    format!("[rift-review r{round}]")
}

/// Cut `s` to at most `max` bytes on a line boundary.
pub fn cap_text(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let cut = s[..end].rfind('\n').unwrap_or(end);
    (s[..cut].to_string(), true)
}

pub fn review_prompt(task: &str, round: u32, max_rounds: u32, diff: &str) -> String {
    let (diff, cut) = cap_text(diff.trim_end(), MAX_DIFF_BYTES);
    let mut p = format!(
        "{} You are reviewing another coding agent's work. Do not modify any file and do not run commands that change the repository.\n\n",
        marker(round)
    );
    p.push_str(&format!("The task it was given:\n{}\n\n", task.trim()));
    p.push_str(&format!("Its latest changes (review round {round} of {max_rounds}), as a unified diff:\n```diff\n{diff}\n```\n"));
    if cut {
        p.push_str("(The diff was truncated; read the files for the rest.)\n");
    }
    p.push_str(
        "\nList concrete problems: bugs, missed requirements, missing tests, risky changes. Give the file and line, most important first, briefly. \
         End with exactly one line: \"VERDICT: APPROVED\" if nothing needs fixing, otherwise \"VERDICT: CHANGES REQUESTED\".",
    );
    p
}

pub fn feedback_prompt(feedback: &str, round: u32, max_rounds: u32) -> String {
    let (fb, _) = cap_text(feedback.trim(), 8000);
    format!(
        "A reviewer looked at your last turn (round {round} of {max_rounds}) and reported:\n\n{fb}\n\nFix the points that are valid and say briefly if you disagree with any. Do not redo work that is already fine."
    )
}

/// Read "VERDICT: ..." from the last line that has one.
pub fn parse_verdict(reply: &str) -> Verdict {
    for line in reply.lines().rev() {
        let l = line.trim().trim_matches(|c: char| c == '*' || c == '`' || c == '_' || c == '#' || c == '>').trim();
        let up = l.to_ascii_uppercase();
        if let Some(rest) = up.strip_prefix("VERDICT") {
            let rest = rest.trim_start_matches(|c: char| c == ':' || c == '-' || c == '*' || c.is_whitespace());
            // The prompt itself spells out both options: an echo of it has "APPROVED" *if* ... skip those.
            if rest.starts_with("APPROVED") || rest.starts_with("LGTM") {
                return if rest.contains("OTHERWISE") { continue } else { Verdict::Approved };
            }
            if rest.starts_with("CHANGES") || rest.starts_with("REJECTED") || rest.starts_with("NEEDS") {
                return Verdict::ChangesRequested;
            }
        }
    }
    Verdict::Unclear
}

/// The reply without its VERDICT line(s) and trailing blanks.
pub fn strip_verdict(reply: &str) -> String {
    let kept: Vec<&str> = reply.lines().filter(|l| !l.trim().trim_matches(|c: char| c == '*' || c == '`' || c == '_').to_ascii_uppercase().starts_with("VERDICT")).collect();
    kept.join("\n").trim().to_string()
}

// ───────────────────────────── reading the reply off the screen ─────────────────────────────

fn is_box_line(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.chars().all(|c| matches!(c, '\u{2500}'..='\u{257f}' | ' ' | '-' | '=' | '_'))
}

/// Strip the border an agent's UI draws around a line (`│ text │`) and its bullet.
fn unframe(s: &str) -> String {
    let mut t = s.trim_end().to_string();
    let trimmed = t.trim_start();
    if trimmed.starts_with('\u{2502}') || trimmed.starts_with('\u{2503}') {
        t = trimmed.trim_start_matches(['\u{2502}', '\u{2503}']).to_string();
        t = t.trim_end().trim_end_matches(['\u{2502}', '\u{2503}']).to_string();
    }
    let t2 = t.trim_start();
    for bullet in ["\u{25cf} ", "\u{23fa} ", "\u{2022} ", "\u{2726} ", "\u{25e6} "] {
        if let Some(rest) = t2.strip_prefix(bullet) {
            return rest.trim_end().to_string();
        }
    }
    t.trim_end().to_string()
}

/// The reviewer's answer: the screen text below the last `marker` (the prompt's
/// first line), without borders, status lines and the input box at the bottom.
/// Falls back to the screen's last lines when the marker scrolled away.
pub fn extract_reply(lines: &[String], marker: &str) -> String {
    let start = lines.iter().rposition(|l| l.contains(marker));
    let from = match start {
        // Skip the prompt itself: it runs until the "VERDICT" instruction (or a few lines).
        Some(i) => {
            let end = (i..lines.len()).find(|&j| lines[j].contains("VERDICT: CHANGES REQUESTED")).map_or(i + 1, |j| j + 1);
            end.min(lines.len())
        }
        None => lines.len().saturating_sub(60),
    };
    let mut out: Vec<String> = Vec::new();
    for raw in &lines[from..] {
        let line = unframe(raw);
        if line.trim().is_empty() {
            if !out.last().is_some_and(|l| l.is_empty()) && !out.is_empty() {
                out.push(String::new());
            }
            continue;
        }
        if is_box_line(&line) || crate::agents::registry::is_chrome_line(&line) {
            continue;
        }
        let t = line.trim_start();
        // The agent's input box ("> ", "\u{203a} ", "? for shortcuts") and spinners are not part of the answer.
        if t == ">" || t.starts_with("> ") && t.len() < 4 || t == "\u{203a}" || t.starts_with("\u{203a} ") && out.is_empty() {
            continue;
        }
        if crate::agents::state::is_spinner_line(&t.to_lowercase()) {
            continue;
        }
        out.push(line);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    let text = out.join("\n");
    let (text, _) = cap_text(&text, 12_000);
    text
}

/// A few lines for the card note: the first non-empty lines, ellipsised.
pub fn note_lines(text: &str, max_lines: usize, cols: usize) -> Vec<String> {
    let mut out = Vec::new();
    for l in text.lines().filter(|l| !l.trim().is_empty()) {
        for piece in crate::agents::ui::wrap(l.trim(), cols.max(8)) {
            if out.len() >= max_lines {
                if let Some(last) = out.last_mut() {
                    *last = crate::ui::kit::ellipsize(&format!("{last}\u{2026}"), cols.max(8));
                }
                return out;
            }
            out.push(piece);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rv() -> WriteReview {
        WriteReview::new(1, "add a retry", 10, 11, 3, false)
    }

    fn start(r: &mut WriteReview) {
        r.on_event(Ev::WriterSent);
        r.on_event(Ev::ReviewerSent);
    }

    const DIFF: &str = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";

    #[test]
    fn a_full_loop_until_approved() {
        let mut r = rv();
        start(&mut r);
        assert_eq!(r.on_event(Ev::WriterFinished), vec![Cmd::ComputeDiff]);
        assert_eq!(r.phase, Phase::AwaitDiff);
        let cmds = r.on_event(Ev::DiffReady(Some(DIFF.into())));
        assert!(matches!(cmds.as_slice(), [Cmd::SendReviewer(p)] if p.contains("add a retry") && p.contains("+b") && p.starts_with("[rift-review r1]")));
        assert_eq!(r.phase, Phase::Reviewing);
        r.on_event(Ev::ReviewerSent);
        // Feedback waits for the user.
        assert!(r.on_event(Ev::ReviewerFinished("x.rs:4 off by one\nVERDICT: CHANGES REQUESTED".into())).is_empty());
        assert_eq!(r.phase, Phase::Feedback);
        let fb = r.feedback.clone().unwrap();
        assert_eq!((fb.round, fb.verdict, fb.text.as_str()), (1, Verdict::ChangesRequested, "x.rs:4 off by one"));
        // [f] forwards: round 2.
        let cmds = r.on_event(Ev::Forward);
        assert!(matches!(cmds.as_slice(), [Cmd::SendWriter(p)] if p.contains("off by one") && p.contains("round 1 of 3")));
        assert_eq!((r.round, r.phase), (2, Phase::Writing));
        r.on_event(Ev::WriterSent);
        assert_eq!(r.on_event(Ev::WriterFinished), vec![Cmd::ComputeDiff]);
        r.on_event(Ev::DiffReady(Some(DIFF.into())));
        r.on_event(Ev::ReviewerSent);
        let cmds = r.on_event(Ev::ReviewerFinished("Looks good.\nVERDICT: APPROVED".into()));
        assert_eq!(cmds, vec![Cmd::Finish(Outcome::Approved)]);
        assert!(r.is_done());
        assert!(r.on_event(Ev::Forward).is_empty(), "nothing moves a finished loop");
    }

    #[test]
    fn the_round_limit_ends_the_loop() {
        let mut r = WriteReview::new(1, "t", 1, 2, 2, true);
        start(&mut r);
        let mut sent_writer = 0;
        let mut last = Vec::new();
        for _ in 0..5 {
            if r.is_done() {
                break;
            }
            r.on_event(Ev::WriterFinished);
            r.on_event(Ev::DiffReady(Some(DIFF.into())));
            r.on_event(Ev::ReviewerSent);
            last = r.on_event(Ev::ReviewerFinished("bug\nVERDICT: CHANGES REQUESTED".into()));
            if matches!(last.as_slice(), [Cmd::SendWriter(_)]) {
                sent_writer += 1;
                r.on_event(Ev::WriterSent);
            }
        }
        assert_eq!(sent_writer, 1, "auto-forward after round 1 only; round 2 is the limit");
        assert_eq!(last, vec![Cmd::Finish(Outcome::MaxRounds)]);
        assert_eq!(r.round, 2);
        assert!(r.feedback.is_some(), "the last feedback stays visible");
    }

    #[test]
    fn auto_forward_sends_immediately() {
        let mut r = WriteReview::new(1, "t", 1, 2, 3, true);
        start(&mut r);
        r.on_event(Ev::WriterFinished);
        r.on_event(Ev::DiffReady(Some(DIFF.into())));
        r.on_event(Ev::ReviewerSent);
        let cmds = r.on_event(Ev::ReviewerFinished("fix y".into()));
        assert!(matches!(cmds.as_slice(), [Cmd::SendWriter(_)]), "unclear verdict still forwards: {cmds:?}");
        assert_eq!(r.round, 2);
    }

    #[test]
    fn turns_before_the_prompt_arrived_are_ignored() {
        let mut r = rv();
        assert!(r.on_event(Ev::WriterFinished).is_empty(), "startup noise");
        assert_eq!(r.phase, Phase::Writing);
        r.on_event(Ev::WriterSent);
        assert!(!r.on_event(Ev::WriterFinished).is_empty());
        r.on_event(Ev::DiffReady(Some(DIFF.into())));
        assert!(r.on_event(Ev::ReviewerFinished("early".into())).is_empty(), "the reviewer has not been sent the prompt yet");
        assert_eq!(r.phase, Phase::Reviewing);
    }

    #[test]
    fn no_changes_means_nothing_to_review() {
        let mut r = rv();
        start(&mut r);
        r.on_event(Ev::WriterFinished);
        assert_eq!(r.on_event(Ev::DiffReady(Some("  \n".into()))), vec![Cmd::Finish(Outcome::NoChanges)]);
        let mut r = rv();
        start(&mut r);
        r.on_event(Ev::WriterFinished);
        assert_eq!(r.on_event(Ev::DiffReady(None)), vec![Cmd::Finish(Outcome::NoChanges)]);
    }

    #[test]
    fn stop_and_gone() {
        let mut r = rv();
        start(&mut r);
        assert_eq!(r.on_event(Ev::Stop), vec![Cmd::Finish(Outcome::Stopped)]);
        assert!(r.on_event(Ev::Stop).is_empty());
        let mut r = rv();
        assert!(r.on_event(Ev::PaneGone(99)).is_empty(), "unrelated panes do not end it");
        assert_eq!(r.on_event(Ev::PaneGone(11)), vec![Cmd::Finish(Outcome::Gone("the reviewer is gone".into()))]);
        assert_eq!(Outcome::Gone("x".into()).label(), "ended: x");
    }

    #[test]
    fn verdicts() {
        assert_eq!(parse_verdict("ok\nVERDICT: APPROVED"), Verdict::Approved);
        assert_eq!(parse_verdict("**VERDICT: CHANGES REQUESTED**"), Verdict::ChangesRequested);
        assert_eq!(parse_verdict("verdict - approved."), Verdict::Approved);
        assert_eq!(parse_verdict("looks fine to me"), Verdict::Unclear);
        // The last verdict wins.
        assert_eq!(parse_verdict("VERDICT: APPROVED\nactually no\nVERDICT: CHANGES REQUESTED"), Verdict::ChangesRequested);
        assert_eq!(strip_verdict("a\nb\nVERDICT: APPROVED\n"), "a\nb");
        assert_eq!(strip_verdict("**VERDICT: CHANGES REQUESTED**"), "");
    }

    #[test]
    fn prompts_are_capped_and_marked() {
        let big = format!("+{}\n", "x".repeat(40_000)).repeat(3);
        let p = review_prompt("task", 2, 3, &big);
        assert!(p.starts_with("[rift-review r2] "));
        assert!(p.contains("truncated"));
        assert!(p.len() < MAX_DIFF_BYTES + 2000);
        assert!(p.contains("round 2 of 3") && p.contains("VERDICT: APPROVED"));
        let (c, cut) = cap_text("a\nb\ncc\n", 4);
        assert_eq!((c.as_str(), cut), ("a\nb", true));
        assert_eq!(cap_text("short", 100), ("short".to_string(), false));
        // Multi-byte text is cut on a character boundary.
        let (c, cut) = cap_text("\u{4e2d}\u{6587}\u{4e2d}\u{6587}", 7);
        assert!(cut && c.chars().all(|ch| ch == '\u{4e2d}' || ch == '\u{6587}'));
    }

    fn screen(s: &str) -> Vec<String> {
        s.lines().map(str::to_string).collect()
    }

    #[test]
    fn reply_is_read_from_below_the_prompt() {
        let prompt = review_prompt("add a retry", 1, 3, DIFF);
        let mut lines: Vec<String> = vec!["earlier chatter".into()];
        lines.extend(prompt.lines().map(|l| format!("> {l}")));
        lines.extend(screen(
            "\n\
             \u{25cf} Two problems:\n\
             \u{25cf} 1. src/retry.rs:12 sleeps without a cap\n\
             2. the new path has no test\n\
             \n\
             VERDICT: CHANGES REQUESTED\n\
             \u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}\n\
             \u{2502} >            \u{2502}\n\
             \u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}\n\
             ? for shortcuts",
        ));
        let reply = extract_reply(&lines, &marker(1));
        assert!(reply.contains("src/retry.rs:12 sleeps without a cap"), "{reply}");
        assert!(reply.contains("no test"));
        assert!(!reply.contains("shortcuts") && !reply.contains('\u{256d}') && !reply.contains("rift-review"));
        assert_eq!(parse_verdict(&reply), Verdict::ChangesRequested);
        assert!(!reply.starts_with('\u{25cf}'), "bullets are stripped: {reply}");
    }

    #[test]
    fn reply_falls_back_to_the_screen_tail() {
        let lines = screen("a\nb\nreal answer\nVERDICT: APPROVED");
        let r = extract_reply(&lines, "[rift-review r9]");
        assert!(r.contains("real answer"));
        assert_eq!(extract_reply(&[], "x"), "");
    }

    #[test]
    fn note_lines_wrap_and_cut() {
        let t = "first point is quite long and needs wrapping here\nsecond\nthird\nfourth";
        let v = note_lines(t, 3, 20);
        assert_eq!(v.len(), 3);
        assert!(v.last().unwrap().ends_with('\u{2026}'), "{v:?}");
        assert!(v.iter().all(|l| l.chars().count() <= 20));
        assert_eq!(note_lines("one\n\ntwo", 5, 20), ["one", "two"]);
        assert!(note_lines("", 3, 20).is_empty());
    }
}
