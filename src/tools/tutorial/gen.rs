//! Deterministic generator for the bundled demos (`assets/demos/*.cast`).
//!
//! Demos are *authored here*, not hand-typed escape codes: a small [`Tape`]
//! builder emits the exact byte stream an interactive zsh with Rift's shell
//! integration would produce (OSC 133 / OSC 7, SGR colours) on a fixed
//! millisecond clock, plus caption / key / UI markers.
//!
//! * `cargo test --bin rift tutorial::gen` checks the checked-in files are
//!   up to date (byte for byte).
//! * `RIFT_REGEN_DEMOS=1 cargo test --bin rift tutorial::gen` rewrites them.

use super::cast::{AiOp, BlocksOp, Cast, Event, Kind, Mark, PaneOp, Panel, Place, TabOp, UiOp};
use crate::screenshot::shell::paint;
use crate::tools::exec_preview::{ExecPreview, Severity};

const HOME: &str = "/Users/maya/dev";
pub const COLS: usize = 84;
pub const ROWS: usize = 20;

#[derive(Clone)]
struct Sh {
    dir: &'static str,
    cwd: String,
    branch: Option<&'static str>,
}

fn aurora() -> Sh {
    Sh { dir: "aurora", cwd: format!("{HOME}/aurora"), branch: Some("main") }
}

fn web() -> Sh {
    Sh { dir: "web", cwd: format!("{HOME}/aurora/web"), branch: Some("main") }
}

pub struct Tape {
    cast: Cast,
    ms: u64,
    last_exit: i32,
}

impl Tape {
    fn new(title: &str) -> Self {
        Self::sized(title, COLS, ROWS)
    }

    fn sized(title: &str, cols: usize, rows: usize) -> Self {
        Self { cast: Cast { cols, rows, title: title.into(), events: Vec::new() }, ms: 0, last_exit: 0 }
    }

    fn t(&self) -> f64 {
        self.ms as f64 / 1000.0
    }

    fn wait(&mut self, s: f64) -> &mut Self {
        self.ms += (s * 1000.0).round() as u64;
        self
    }

    fn out(&mut self, s: &str) -> &mut Self {
        let t = self.t();
        self.cast.events.push(Event { t, kind: Kind::Output(s.as_bytes().to_vec()) });
        self
    }

    fn mark(&mut self, m: Mark) -> &mut Self {
        let t = self.t();
        self.cast.events.push(Event { t, kind: Kind::Mark(m) });
        self
    }

    fn caption(&mut self, s: &str) -> &mut Self {
        self.mark(Mark::Caption(s.into()))
    }

    fn key(&mut self, r: &str, label: &str) -> &mut Self {
        self.mark(Mark::Key(r.into(), label.into()))
    }

    fn prompt(&mut self, sh: &Sh) -> &mut Self {
        let mut s = String::from("\x1b]133;A\x07");
        s.push_str(&format!("\x1b]7;file://mbp{}\x07", sh.cwd));
        s.push_str(&paint(&format!("{{bblue}}{}{{0}}", sh.dir)));
        if let Some(b) = sh.branch {
            s.push_str(&paint(&format!(" {{magenta}}git:({b}){{0}}")));
        }
        if self.last_exit != 0 {
            s.push_str(&paint(&format!(" {{bred}}[{}]{{0}}", self.last_exit)));
            s.push_str(&paint(" {bred}${0} "));
        } else {
            s.push_str(&paint(" {bgreen}${0} "));
        }
        s.push_str("\x1b]133;B\x07");
        self.out(&s)
    }

    /// Typed at the prompt, one character every 50 ms.
    fn typed(&mut self, cmd: &str) -> &mut Self {
        self.wait(0.35);
        for c in cmd.chars() {
            self.out(&c.to_string());
            self.wait(0.05);
        }
        self
    }

    fn enter(&mut self) -> &mut Self {
        self.out("\r\n\x1b]133;C\x07")
    }

    fn line(&mut self, l: &str) -> &mut Self {
        let p = paint(l);
        self.out(&format!("{p}\r\n"))
    }

    fn done(&mut self, code: i32) -> &mut Self {
        self.last_exit = code;
        self.out(&format!("\x1b]133;D;{code}\x07"))
    }

    /// Prompt, typed command, Enter, output spread over `secs`, finish.
    fn run(&mut self, sh: &Sh, cmd: &str, output: &str, secs: f64, exit: i32) -> &mut Self {
        self.prompt(sh).typed(cmd).wait(0.25).enter();
        let lines: Vec<&str> = if output.is_empty() { vec![] } else { output.split('\n').collect() };
        let per = if lines.is_empty() { 0.0 } else { secs / lines.len() as f64 };
        if lines.is_empty() {
            self.wait(secs);
        }
        for l in lines {
            self.wait(per);
            self.line(l);
        }
        self.done(exit)
    }

    fn panel(&mut self, place: Place, title: &str, badge: &str, lines: &[&str], hints: &[(&str, &str)]) -> &mut Self {
        self.mark(Mark::Ui(UiOp::Panel(Panel {
            place,
            title: title.into(),
            badge: badge.into(),
            lines: lines.iter().map(|s| s.to_string()).collect(),
            hints: hints.iter().map(|(k, l)| (k.to_string(), l.to_string())).collect(),
        })))
    }

    fn finish(&mut self) -> Cast {
        self.wait(2.0).caption("");
        self.cast.clone()
    }
}

// ───────────────────────────── content ─────────────────────────────

const LS_OUT: &str = "{bblue}build{0}  Cargo.lock  Cargo.toml  {bblue}docs{0}  {bblue}migrations{0}  README.md  {bblue}src{0}  {bblue}tests{0}";

const GIT_STATUS: &str = "## {bgreen}main{0}...{red}origin/main{0} [ahead 2]
{red} M{0} src/handlers/auth.rs
{red} M{0} src/router.rs
{red}??{0} src/middleware/rate_limit.rs";

const CARGO_TEST_Q: &str = "running 14 tests
{bgreen}..............{0}
test result: {bgreen}ok{0}. 14 passed; 0 failed; finished in 0.21s";

const NPM_INSTALL: &str = "added 1247 packages, and audited 1248 packages in 9s
214 packages are looking for funding
  run `npm fund` for details
found {bgreen}0{0} vulnerabilities";

const CARGO_BUILD_ERR: &str = "{bgreen}   Compiling{0} aurora v0.4.2 (/Users/maya/dev/aurora)
{bred}error[E0432]{0}{bold}: unresolved import `governor`{0}
{bblue} --> {0}src/middleware/rate_limit.rs:3:5
{bblue}  |{0}
{bblue}3 |{0} use governor::{Quota, RateLimiter};
{bblue}  |{0}     {bred}^^^^^^^^{0} {bred}use of unresolved module or unlinked crate `governor`{0}
{bblue}  |{0}
{bred}error{0}: could not compile `aurora` (bin \"aurora\") due to 1 previous error";

const CARGO_ADD: &str = "{bgreen}    Updating{0} crates.io index
{bgreen}      Adding{0} governor v0.6.3 to dependencies
             Features: + dashmap + std";

const ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/v1/orders?page=2", "14.9"),
    ("GET", "/healthz", "1.2"),
    ("POST", "/v1/session/refresh", "8.4"),
    ("GET", "/v1/orders/8812", "6.1"),
    ("GET", "/v1/users/me", "3.7"),
    ("POST", "/v1/orders", "21.5"),
    ("GET", "/v1/products?q=lamp", "11.0"),
];

/// Server log line `i` of the Time Warp demo; line 5 is the error that scrolls away.
fn log_line(i: usize) -> String {
    let secs = 1 + i * 13 / 10;
    let ts = format!("{{gray}}14:32:{:02}{{0}}", secs % 60);
    match i {
        0 => format!("{ts} {{bgreen}}INFO{{0}}  aurora::server listening on 0.0.0.0:4000"),
        5 => format!("{ts} {{bred}}ERROR{{0}} db pool timeout after 5000ms (orders::list)"),
        9 | 23 => format!("{ts} {{byellow}}WARN{{0}}  rate limit near threshold ip=10.0.4.{}", 10 + i),
        _ => {
            let (m, path, ms) = ROUTES[i % ROUTES.len()];
            format!("{ts} {{bgreen}}INFO{{0}}  {m} {path} {{bgreen}}200{{0}} {ms}ms")
        }
    }
}

// ───────────────────────────── demos ─────────────────────────────

fn blocks() -> Cast {
    let mut t = Tape::new("Command blocks");
    let sh = aurora();
    t.mark(Mark::Tab(TabOp::Title("aurora".into())));
    t.wait(0.4).caption("Every command you run becomes a block: the command, its output and how it ended.");
    t.run(&sh, "ls", LS_OUT, 0.1, 0);
    t.run(&sh, "git status -sb", GIT_STATUS, 0.2, 0);
    t.run(&sh, "npm install", NPM_INSTALL, 2.4, 0);
    t.run(&sh, "cargo test -q", CARGO_TEST_Q, 1.6, 0);
    t.run(&sh, "cargo build", CARGO_BUILD_ERR, 1.4, 101);
    t.prompt(&sh);
    t.wait(1.0).caption("The chip on each block shows its exit status and duration. A failed block is tinted red.");
    t.wait(4.0).caption("Hover a block for its toolbar: copy the command, copy the output, ask AI, rerun or collapse.");
    t.key("=Hover", "Block toolbar").mark(Mark::Blocks(BlocksOp::Hover(2, Some("collapse".into()))));
    t.wait(4.0).caption("Collapse output you are done with. A folded block takes one line; click it to open it again.");
    t.key("=Click", "Collapse").mark(Mark::Blocks(BlocksOp::Fold(2)));
    t.wait(4.0).mark(Mark::Blocks(BlocksOp::Clear));
    t.caption("{key:@block_jump} jumps from block to block, so the start of a long log is one key away.");
    t.wait(1.2).key("@block_jump", "Previous block").mark(Mark::Blocks(BlocksOp::Jump(3)));
    t.wait(1.4).key("@block_jump", "Previous block").mark(Mark::Blocks(BlocksOp::Jump(1)));
    t.wait(1.6).key("@block_jump", "Next block").mark(Mark::Blocks(BlocksOp::Bottom));
    t.wait(1.6).caption("Click a block's gutter to select it. {key:@block_copy} copies just its output, without the prompt.");
    t.key("=Click", "Select block").mark(Mark::Blocks(BlocksOp::Select(4)));
    t.wait(1.4).key("@block_copy", "Copy output");
    t.wait(3.0).mark(Mark::Blocks(BlocksOp::Clear));
    t.caption("Blocks come from shell integration (OSC 133), which Rift injects into zsh, bash and fish automatically.");
    t.wait(3.5);
    t.finish()
}

fn splits() -> Cast {
    let mut t = Tape::new("Splits & panes");
    let sh = aurora();
    t.mark(Mark::Tab(TabOp::Title("aurora".into())));
    t.run(&sh, "cargo test -q", "running 14 tests\n{bgreen}..............{0}\ntest result: {bgreen}ok{0}. 14 passed", 1.0, 0);
    t.prompt(&sh);
    t.wait(0.6).caption("{key:split_right} splits the focused pane side by side. The new pane starts in the same directory.");
    t.wait(1.8).key("split_right", "Split right").mark(Mark::Pane(PaneOp::SplitRight));
    t.wait(0.4).run(&web(), "npm run dev", "{bgreen}VITE v5.4.2{0} ready in 312 ms\n  -> Local: http://localhost:5173/", 0.8, 0);
    t.wait(1.5).caption("{key:split_down} stacks a new pane below the focused one.");
    t.wait(1.6).key("split_down", "Split down").mark(Mark::Pane(PaneOp::SplitDown));
    t.wait(0.4).run(&sh, "git log --oneline -3", "{byellow}3f9a1c2{0} wip: rate limiting\n{byellow}b81d07e{0} feat(auth): rotate tokens\n{byellow}5ac2e90{0} fix: trim trailing slash", 0.3, 0);
    t.prompt(&sh);
    t.wait(1.2).caption("Move focus with {key:@pane_focus}, or cycle through the panes with {key:@pane_next}.");
    t.wait(1.6).key("@pane_focus", "Focus left").mark(Mark::Pane(PaneOp::Focus(0)));
    t.wait(1.6).key("@pane_focus", "Focus right").mark(Mark::Pane(PaneOp::Focus(1)));
    t.wait(1.6).key("@pane_next", "Next pane").mark(Mark::Pane(PaneOp::Focus(2)));
    t.wait(1.6).caption("{key:@pane_zoom} zooms the focused pane to the whole window. Press it again to bring the layout back.");
    t.wait(1.4).key("@pane_zoom", "Zoom").mark(Mark::Pane(PaneOp::Zoom));
    t.wait(2.6).key("@pane_zoom", "Unzoom").mark(Mark::Pane(PaneOp::Zoom));
    t.wait(1.4).caption("Drag a divider to resize. {key:@pane_close} closes the focused pane.");
    t.wait(3.5);
    t.finish()
}

fn ai() -> Cast {
    let mut t = Tape::new("AI: fix, ask, # commands");
    let sh = aurora();
    t.mark(Mark::Tab(TabOp::Title("aurora".into())));
    t.wait(0.3).caption("AI in Rift works where you already are. All answers here are samples; nothing is sent anywhere.");
    t.run(&sh, "cargo build", CARGO_BUILD_ERR, 1.4, 101);
    t.prompt(&sh);
    t.wait(0.8).mark(Mark::Ai(AiOp::Fix("cargo add governor".into(), "adds the missing dependency".into())));
    t.caption("When a command fails, a corrected command appears at the prompt. {key:=Tab} puts it there, {key:=Esc} dismisses it.");
    t.wait(4.5).key("=Tab", "Accept fix").mark(Mark::Ai(AiOp::Clear));
    t.out("cargo add governor");
    t.wait(0.3).caption("It is only typed, never run: you press {key:=Enter} when it looks right.");
    t.wait(3.2).key("=Enter", "Run").enter();
    for l in CARGO_ADD.split('\n') {
        t.wait(0.3).line(l);
    }
    t.done(0);
    t.prompt(&sh);
    t.wait(1.0).caption("{key:@ask_inline} asks about this: your selection, the hovered block, the last block or the screen.");
    t.wait(1.2).key("@ask_inline", "Ask AI").mark(Mark::Ai(AiOp::Ask("what changed in Cargo.toml?".into())));
    t.wait(3.0).key("=Enter", "Ask").mark(Mark::Ai(AiOp::Clear));
    t.panel(
        Place::Right,
        "AI Assistant",
        "SAMPLE",
        &[
            "> what changed in Cargo.toml?",
            "",
            "cargo add put one line under",
            "[dependencies]:",
            "",
            "~   governor = \"0.6.3\"",
            "",
            "Cargo.lock now pins governor and",
            "its dependencies. Run cargo build",
            "again to check the import resolves.",
        ],
        &[],
    );
    t.caption("Answers stream into the docked chat ({key:ai_assistant}). Commands it suggests never run without you.");
    t.wait(5.0).mark(Mark::Ui(UiOp::Clear));
    t.caption("Start a line with # and say what you want. Enter writes the command into your prompt for review.");
    t.wait(0.6).typed("# find the 10 largest files");
    t.wait(0.8).key("=Enter", "Generate");
    let cmd = "du -ah . | sort -rh | head -n 10";
    t.wait(0.9).out("\r\x1b[2K");
    t.prompt(&sh).out(cmd);
    t.mark(Mark::Ai(AiOp::Nl("find the 10 largest files".into(), cmd.into())));
    t.wait(1.0).caption("Nothing runs until you press {key:=Enter} yourself; {key:=Esc} clears it.");
    t.wait(4.0).mark(Mark::Ai(AiOp::Clear));
    t.caption("To use AI for real, connect an OpenAI-compatible API or a local model: see AI setup in the README.");
    t.wait(3.5);
    t.finish()
}

fn palette() -> Cast {
    let mut t = Tape::new("Command palette");
    let sh = aurora();
    t.mark(Mark::Tab(TabOp::Title("aurora".into())));
    t.mark(Mark::Tab(TabOp::New("web".into())));
    t.mark(Mark::Tab(TabOp::Select(0)));
    t.run(&sh, "git status -sb", GIT_STATUS, 0.2, 0);
    t.prompt(&sh);
    t.wait(0.6).caption("{key:command_palette} opens the command palette: every Rift action in one fuzzy search.");
    t.wait(1.6).key("command_palette", "Command palette").mark(Mark::Ui(UiOp::Palette(String::new())));
    t.wait(2.6).caption("Type a few letters. Each result shows its shortcut, so the palette teaches you the keys as you go.");
    for q in ["s", "sp", "spl"] {
        t.wait(0.35).mark(Mark::Ui(UiOp::Palette(q.into())));
    }
    t.wait(3.5).caption("Some entries take a parameter: theme, font, open, ssh, cd, > for a shell command and ? to ask AI.");
    for q in ["t", "th", "the", "them", "theme", "theme ", "theme n", "theme no"] {
        t.wait(0.18).mark(Mark::Ui(UiOp::Palette(q.into())));
    }
    t.wait(3.5).caption("{key:=Enter} runs the highlighted entry and {key:=Esc} closes. These tutorials are in the palette too.");
    for q in ["t", "tu", "tut", "tuto"] {
        t.wait(0.25).mark(Mark::Ui(UiOp::Palette(q.into())));
    }
    t.wait(3.5).key("=Esc", "Close").mark(Mark::Ui(UiOp::Clear));
    t.wait(1.0);
    t.finish()
}

pub const PREVIEW_ROUTINE: &str = "rm -rf ./build";
pub const PREVIEW_WARN: &str = "git reset --hard origin/main";
pub const PREVIEW_CRIT: &str = "curl -fsSL https://get.example.dev/install.sh | sh";

fn preview() -> Cast {
    // The story depends on the real classifier: fail generation if it changes.
    assert!(ExecPreview::check_for_enter(PREVIEW_ROUTINE, Some("/nonexistent")).is_none(), "routine delete must not prompt");
    assert_eq!(ExecPreview::check_static(PREVIEW_WARN).map(|p| p.severity), Some(Severity::Warning));
    assert_eq!(ExecPreview::check_static(PREVIEW_CRIT).map(|p| p.severity), Some(Severity::Critical));

    let mut t = Tape::new("Preview-Then-Accept");
    let sh = aurora();
    t.mark(Mark::Tab(TabOp::Title("aurora".into())));
    t.run(&sh, "ls", LS_OUT, 0.1, 0);
    t.wait(0.4).caption("Rift looks at a command when you press Enter. Routine ones, like deleting a build directory, just run.");
    t.prompt(&sh).typed(PREVIEW_ROUTINE).wait(0.4).key("=Enter", "Run").enter().wait(0.2).done(0);
    t.prompt(&sh);
    t.wait(1.6).caption("Dangerous ones stop first, with what they would do. Nothing reaches the shell until you decide.");
    t.typed(PREVIEW_WARN).wait(0.4).key("=Enter", "Run");
    t.mark(Mark::Ui(UiOp::Preview(PREVIEW_WARN.into(), String::new())));
    t.wait(4.0).caption("Run it with {key:=Y} or {key:=Enter}; {key:=N} or {key:=Esc} cancels and leaves the line as it was.");
    t.wait(3.0).key("=Esc", "Cancel").mark(Mark::Ui(UiOp::Clear));
    // The line is left as it was; the user abandons it with Ctrl+C (zsh prints ^C).
    t.wait(1.2).out("^C\r\n");
    t.prompt(&sh);
    t.wait(0.6).caption("Critical commands, such as piping a download into a shell, need the word yes typed out.");
    t.typed(PREVIEW_CRIT).wait(0.4).key("=Enter", "Run");
    t.mark(Mark::Ui(UiOp::Preview(PREVIEW_CRIT.into(), String::new())));
    t.wait(2.4).key("=Type", "yes");
    for typed in ["y", "ye", "yes"] {
        t.wait(0.3).mark(Mark::Ui(UiOp::Preview(PREVIEW_CRIT.into(), typed.into())));
    }
    t.wait(2.2).caption("Changed your mind? {key:=Esc} still cancels. Here we do: never pipe scripts you have not read.");
    t.wait(2.4).key("=Esc", "Cancel").mark(Mark::Ui(UiOp::Clear));
    t.wait(1.0).out("^C\r\n");
    t.prompt(&sh);
    t.wait(0.6).caption("The same rules mark AI suggestions and agent approval prompts as RISKY, wherever a command comes from.");
    t.wait(4.0);
    t.finish()
}

const CLAUDE_ASK: &[&str] = &[
    "{bold}> add rate limiting to orders{0}",
    "",
    "{bmagenta}*{0} {bold}Update{0}(rate_limit.rs) {gray}+42{0}",
    "{bmagenta}*{0} {bold}Bash{0}(cargo test rate_limit)",
    "",
    "Do you want to proceed?",
    "{bcyan}> 1. Yes{0}",
    "  2. Yes, and don't ask again",
    "  3. No, tell Claude what to do",
];

fn agents() -> Cast {
    let mut t = Tape::new("Agent Mission Control");
    t.mark(Mark::Tab(TabOp::Title("claude \u{00b7} main".into())));
    t.wait(0.3).caption("Run AI coding agents (Claude Code, Codex, Gemini CLI, Aider...) in any pane. Rift tracks what each one is doing.");
    t.prompt(&aurora()).typed("claude").wait(0.3).enter();
    for l in CLAUDE_ASK {
        t.wait(0.12).line(l);
    }
    t.wait(1.2).caption("{key:agent_mission_control} opens Mission Control: one card per agent across all tabs, with its state.");
    t.wait(1.6).key("agent_mission_control", "Mission Control");
    let waiting: &[&str] = &[
        "## 3 agents \u{00b7} 1 needs you",
        "! Claude Code \u{00b7} main",
        "~   needs you \u{00b7} cargo test",
        "    [1 Approve] [2 Always]",
        "    [3 Deny]",
        "> Codex \u{00b7} agent/codex-1",
        "~   working \u{00b7} 2m 03s",
        "+ Gemini \u{00b7} web/main",
        "~   done \u{00b7} 6 files",
    ];
    let hints: &[(&str, &str)] = &[("1-3", "answer"), ("Enter", "jump"), ("r", "reply"), ("v", "review")];
    t.panel(Place::Left, "Mission Control", "SAMPLE", waiting, hints);
    t.wait(4.0).caption("Answer an approval prompt right from the card with {key:=1-3}, jump to the agent with {key:=Enter}, reply with {key:=r}.");
    t.wait(3.5).key("=1", "Approve");
    let working: &[&str] = &[
        "## 3 agents",
        "> Claude Code \u{00b7} main",
        "~   working \u{00b7} 4m 20s",
        "",
        "",
        "> Codex \u{00b7} agent/codex-1",
        "~   working \u{00b7} 2m 11s",
        "+ Gemini \u{00b7} web/main",
        "~   done \u{00b7} 6 files",
    ];
    t.panel(Place::Left, "Mission Control", "SAMPLE", working, hints);
    t.wait(0.4).line("").line("{bmagenta}*{0} Running cargo test ...").wait(1.2).line("  {bgreen}ok{0}: 6 passed; 0 failed");
    t.wait(1.6).caption("{key:agent_next_attention} jumps to the next agent waiting for you, in any tab.");
    t.wait(1.4).key("agent_next_attention", "Next waiting agent");
    t.wait(2.6).caption("Risky commands on a card get a RISKY badge. Autopilot ({key:=p}) answers routine prompts by policy; it is off by default.");
    t.wait(4.5).caption("Card states come from the screen and from agent hooks: see Mission control in the README.");
    t.wait(3.0).mark(Mark::Ui(UiOp::Clear));
    t.finish()
}

fn tabs() -> Cast {
    let mut t = Tape::new("Tabs & windows");
    let sh = aurora();
    t.mark(Mark::Tab(TabOp::Title("aurora".into())));
    t.run(&sh, "git status -sb", GIT_STATUS, 0.2, 0);
    t.prompt(&sh);
    t.wait(0.6).caption("{key:new_tab} opens a tab. Every tab keeps its own panes and splits.");
    t.wait(1.6).key("new_tab", "New tab").mark(Mark::Tab(TabOp::New("web".into())));
    t.last_exit = 0;
    t.wait(0.3).run(&web(), "npm test", " {bgreen}OK{0} src/lib/format.test.ts {gray}(9 tests){0}\n {gray}Tests{0}  {bgreen}9 passed{0} (9)", 0.6, 0);
    t.prompt(&web());
    t.wait(1.2).caption("Switch tabs with {key:prev_tab} and {key:next_tab}, or pick one by name in the command palette.");
    t.wait(1.6).key("prev_tab", "Previous tab").mark(Mark::Tab(TabOp::Select(0)));
    t.wait(1.6).key("next_tab", "Next tab").mark(Mark::Tab(TabOp::Select(1)));
    t.wait(1.6).caption("You can rename and reorder tabs. {key:close_tab} closes the current pane or tab.");
    t.wait(3.5).caption("{key:new_window} opens another window in the same directory, with tabs of its own.");
    t.wait(1.4).key("new_window", "New window");
    t.wait(2.6).caption("{key:close_window} closes a window; closing the last one quits. Session restore brings tabs and splits back.");
    t.wait(4.0);
    t.finish()
}

fn time_warp() -> Cast {
    let mut t = Tape::new("Time Warp & recording");
    let sh = aurora();
    let rows = ROWS;
    t.mark(Mark::Tab(TabOp::Title("aurora".into())));
    t.wait(0.3).caption("Something scrolled past too fast? Time Warp rewinds the screen of the active pane.");
    t.prompt(&sh).typed("tail -f log/dev.log").wait(0.2).enter();
    // Screen history so the generator can draw earlier frames exactly.
    let mut screen: Vec<String> = vec![
        paint("{bblue}aurora{0} {magenta}git:(main){0} {bgreen}${0} tail -f log/dev.log"),
    ];
    let total = 34;
    for i in 0..total {
        t.wait(if i == 5 { 0.5 } else { 0.22 });
        let l = log_line(i);
        t.line(&l);
        screen.push(paint(&l));
    }
    t.wait(0.6).caption("The ERROR line is gone. {key:time_warp} opens Time Warp on this pane.");
    t.wait(1.6).key("time_warp", "Time Warp");
    // Frames: snapshot k shows the screen as it was when line `upto` arrived.
    let frame = |upto: usize, n: usize, of: usize, ago: f64| -> String {
        let start = (upto + 1).saturating_sub(rows - 1);
        let mut s = String::from("\x1b[?25l\x1b[H\x1b[2J");
        for (i, l) in screen[start..=upto].iter().enumerate() {
            if i > 0 {
                s.push_str("\r\n");
            }
            s.push_str(l);
        }
        s.push_str(&format!(
            "\x1b[{rows};1H\x1b[48;2;40;20;60m\x1b[38;2;200;160;255m [TIME WARP]  Frame {n}/{of} | {ago:.1}s ago | \u{2190}\u{2192} navigate | Shift+Arrow x10 | Esc exit \x1b[K\x1b[0m"
        ));
        s
    };
    let of = 96;
    // Enter Time Warp on the alternate screen, so leaving restores the live view.
    t.out("\x1b[?1049h").out(&frame(total, of, of, 0.1));
    t.wait(1.2).caption("{key:=Left} and {key:=Right} step through snapshots; {key:=Shift+Left} jumps ten at a time.");
    let mut upto = total;
    let mut n = of;
    for step in 0..6 {
        t.wait(0.7);
        let big = step < 2;
        t.key(if big { "=Shift+Left" } else { "=Left" }, if big { "Back 10" } else { "Back" });
        let back = if big { 10 } else { 1 };
        n -= back;
        upto = upto.saturating_sub(if big { 8 } else { 1 });
        let ago = (of - n) as f64 * 0.1 + 0.1;
        t.out(&frame(upto.max(6), n, of, ago));
        if upto <= 8 {
            break;
        }
    }
    t.wait(0.8).caption("There it is. Rift keeps the last 500 snapshots, up to ten a second. It only shows them: nothing runs again.");
    t.wait(4.0).caption("{key:=Esc} goes back to the live screen.");
    t.wait(1.6).key("=Esc", "Exit Time Warp").out("\x1b[?1049l\x1b[?25h");
    t.wait(1.4).caption("{key:recording} records the session as an asciinema v2 .cast file; press it again to stop.");
    t.wait(1.4).key("recording", "Record");
    t.wait(3.4).caption("These tutorials are casts in the same format, played back inside Rift. Play one of yours: rift --demo file.cast");
    t.wait(4.0);
    t.finish()
}

/// (file id, cast) for every bundled demo, in `DEMOS` order.
pub fn all() -> Vec<(&'static str, Cast)> {
    vec![
        ("blocks", blocks()),
        ("splits", splits()),
        ("ai", ai()),
        ("palette", palette()),
        ("preview", preview()),
        ("agents", agents()),
        ("tabs", tabs()),
        ("time-warp", time_warp()),
    ]
}

#[test]
fn bundled_demos_match_the_generator() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/demos");
    let regen = std::env::var_os("RIFT_REGEN_DEMOS").is_some();
    let mut stale = Vec::new();
    for (id, cast) in all() {
        let text = cast.to_asciicast();
        let path = dir.join(format!("{id}.cast"));
        if regen {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&path, &text).unwrap();
        } else if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            stale.push(id);
        }
        // What is embedded is what the generator makes.
        if !regen {
            let embedded = super::find(id).expect("every generated demo is registered").source;
            if embedded != text {
                stale.push(id);
            }
        }
    }
    assert!(stale.is_empty(), "demos out of date: {stale:?}; run RIFT_REGEN_DEMOS=1 cargo test --bin rift tutorial::gen");
}

#[test]
fn generator_is_deterministic() {
    let a: Vec<String> = all().into_iter().map(|(_, c)| c.to_asciicast()).collect();
    let b: Vec<String> = all().into_iter().map(|(_, c)| c.to_asciicast()).collect();
    assert_eq!(a, b);
}
