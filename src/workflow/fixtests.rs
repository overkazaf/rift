//! "Fix failing tests": run the test command, send the failures to the agent,
//! re-run after each of its turns, until the tests are green or the attempt
//! limit is used up. Pure state machine; the glue runs the tests in a
//! background thread.

use super::review_loop::cap_text;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Green after this many fix turns (0 = they passed from the start).
    Green(u32),
    GaveUp(u32),
    Stopped,
    Gone,
}

impl Outcome {
    pub fn label(&self) -> String {
        match self {
            Outcome::Green(0) => "tests already pass".into(),
            Outcome::Green(n) => format!("tests green after {n} fix{}", if *n == 1 { "" } else { "es" }),
            Outcome::GaveUp(n) => format!("still red after {n} attempt{}", if *n == 1 { "" } else { "s" }),
            Outcome::Stopped => "stopped".into(),
            Outcome::Gone => "the agent is gone".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The test command is running.
    Testing,
    /// Failures were handed to the agent; it is working on them.
    Fixing,
    Done,
}

pub enum Ev {
    /// The test run finished: `Ok(())` = green, `Err(output)` = failures.
    TestsDone(Result<(), String>),
    /// The agent received the failure prompt.
    Sent,
    /// The agent finished a turn.
    AgentFinished,
    Stop,
    AgentGone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cmd {
    RunTests,
    /// Send this prompt to the agent.
    Send(String),
    Finish(Outcome),
}

#[derive(Clone, Debug)]
pub struct FixTests {
    pub id: u64,
    pub pane: usize,
    pub task: String,
    pub test_cmd: String,
    pub max_attempts: u32,
    /// Fix prompts sent so far.
    pub attempts: u32,
    pub phase: Phase,
    pub outcome: Option<Outcome>,
    /// Output of the latest failing run (trimmed), for the card.
    pub last_failure: Option<String>,
    sent: bool,
}

/// Lines of test output kept for the agent.
pub const MAX_OUTPUT_LINES: usize = 120;

impl FixTests {
    pub fn new(id: u64, pane: usize, task: &str, test_cmd: &str, max_attempts: u32) -> FixTests {
        FixTests {
            id,
            pane,
            task: task.to_string(),
            test_cmd: test_cmd.trim().to_string(),
            max_attempts: max_attempts.max(1),
            attempts: 0,
            phase: Phase::Testing,
            outcome: None,
            last_failure: None,
            sent: false,
        }
    }

    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
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
            Ev::AgentGone => self.finish(Outcome::Gone),
            Ev::Sent => {
                self.sent = true;
                Vec::new()
            }
            Ev::TestsDone(Ok(())) if self.phase == Phase::Testing => {
                self.last_failure = None;
                self.finish(Outcome::Green(self.attempts))
            }
            Ev::TestsDone(Err(out)) if self.phase == Phase::Testing => {
                self.last_failure = Some(tail_lines(&out, 12));
                if self.attempts >= self.max_attempts {
                    return self.finish(Outcome::GaveUp(self.attempts));
                }
                self.attempts += 1;
                self.phase = Phase::Fixing;
                self.sent = false;
                vec![Cmd::Send(fix_prompt(&self.task, &self.test_cmd, &out, self.attempts, self.max_attempts))]
            }
            Ev::TestsDone(_) => Vec::new(),
            Ev::AgentFinished => {
                if self.phase != Phase::Fixing || !self.sent {
                    return Vec::new();
                }
                self.phase = Phase::Testing;
                vec![Cmd::RunTests]
            }
        }
    }
}

/// The last `n` lines of `s`.
pub fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

pub fn fix_prompt(task: &str, cmd: &str, output: &str, attempt: u32, max: u32) -> String {
    let tail = tail_lines(output.trim_end(), MAX_OUTPUT_LINES);
    let (tail, _) = cap_text(&tail, 12_000);
    let mut p = String::new();
    if !task.trim().is_empty() && attempt == 1 {
        p.push_str(task.trim());
        p.push_str("\n\n");
    }
    p.push_str(&format!(
        "Running `{cmd}` fails (attempt {attempt} of {max}). Last output:\n```\n{tail}\n```\nFix the code (or the tests, if they are wrong) so that `{cmd}` passes. Run it yourself to check, then stop."
    ));
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(max: u32) -> FixTests {
        FixTests::new(1, 10, "make the parser tests pass", "cargo test", max)
    }

    #[test]
    fn already_green() {
        let mut f = fx(3);
        assert_eq!(f.on_event(Ev::TestsDone(Ok(()))), vec![Cmd::Finish(Outcome::Green(0))]);
        assert!(f.is_done());
        assert_eq!(Outcome::Green(0).label(), "tests already pass");
    }

    #[test]
    fn red_then_fixed_in_two_attempts() {
        let mut f = fx(3);
        let cmds = f.on_event(Ev::TestsDone(Err("test a ... FAILED\nassertion failed".into())));
        match cmds.as_slice() {
            [Cmd::Send(p)] => {
                assert!(p.starts_with("make the parser tests pass"), "the first prompt carries the task");
                assert!(p.contains("cargo test") && p.contains("assertion failed") && p.contains("attempt 1 of 3"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(f.phase, Phase::Fixing);
        // The agent's start-up turn before the prompt arrived does not count.
        assert!(f.on_event(Ev::AgentFinished).is_empty());
        f.on_event(Ev::Sent);
        assert_eq!(f.on_event(Ev::AgentFinished), vec![Cmd::RunTests]);
        assert_eq!(f.phase, Phase::Testing);
        let cmds = f.on_event(Ev::TestsDone(Err("still failing".into())));
        assert!(matches!(cmds.as_slice(), [Cmd::Send(p)] if !p.starts_with("make the parser") && p.contains("attempt 2 of 3")));
        f.on_event(Ev::Sent);
        f.on_event(Ev::AgentFinished);
        assert_eq!(f.on_event(Ev::TestsDone(Ok(()))), vec![Cmd::Finish(Outcome::Green(2))]);
        assert_eq!(Outcome::Green(2).label(), "tests green after 2 fixes");
        assert!(f.last_failure.is_none());
    }

    #[test]
    fn gives_up_after_the_attempt_limit() {
        let mut f = fx(2);
        for _ in 0..2 {
            assert!(matches!(f.on_event(Ev::TestsDone(Err("red".into()))).as_slice(), [Cmd::Send(_)]));
            f.on_event(Ev::Sent);
            assert_eq!(f.on_event(Ev::AgentFinished), vec![Cmd::RunTests]);
        }
        assert_eq!(f.on_event(Ev::TestsDone(Err("red".into()))), vec![Cmd::Finish(Outcome::GaveUp(2))]);
        assert_eq!(f.attempts, 2);
        assert_eq!(f.last_failure.as_deref(), Some("red"));
        assert!(Outcome::GaveUp(2).label().contains("2 attempts"));
    }

    #[test]
    fn stop_and_exit() {
        let mut f = fx(2);
        assert_eq!(f.on_event(Ev::Stop), vec![Cmd::Finish(Outcome::Stopped)]);
        assert!(f.on_event(Ev::TestsDone(Ok(()))).is_empty());
        let mut f = fx(2);
        assert_eq!(f.on_event(Ev::AgentGone), vec![Cmd::Finish(Outcome::Gone)]);
    }

    #[test]
    fn stray_results_are_ignored() {
        let mut f = fx(2);
        f.on_event(Ev::TestsDone(Err("red".into())));
        // A second result while the agent is fixing does nothing.
        assert!(f.on_event(Ev::TestsDone(Err("late".into()))).is_empty());
        assert_eq!(f.attempts, 1);
    }

    #[test]
    fn output_is_trimmed_for_the_agent() {
        let out: String = (0..500).map(|i| format!("line {i}\n")).collect();
        let p = fix_prompt("", "npm test", &out, 1, 3);
        assert!(p.contains("line 499") && !p.contains("line 100\n"));
        assert!(!p.starts_with('\n'));
        assert_eq!(tail_lines("a\nb\nc", 2), "b\nc");
    }
}
