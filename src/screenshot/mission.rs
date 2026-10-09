//! Scene "mission-control": the agent dock as a control console. Three
//! scripted agent panes (Claude Code waiting on a risky `git push`, Codex
//! working, Gemini done) and the dock beside them, fed through the real
//! parsers (`agents::prompt`, `agents::metrics`) exactly like the app does.

use std::time::{Duration, Instant};

use super::scenes::Stage;
use super::shell::{paint, Prompt, Script};
use crate::agents::control::{PaneInfo, Risk};
use crate::agents::registry::{PaneObs, Probe};
use crate::agents::{metrics, prompt, AgentKind};
use crate::review::{Summary, TurnDigest};
use crate::window::tab::{PaneNode, SplitDir};
use crate::window::Pane;

const HOME: &str = "/Users/maya/dev";

fn pad(s: &str, n: usize) -> String {
    let mut o: String = s.chars().take(n).collect();
    let len = o.chars().count();
    o.push_str(&" ".repeat(n.saturating_sub(len)));
    o
}

fn wrap(s: &str, n: usize) -> Vec<String> {
    crate::agents::ui::wrap(s, n)
}

/// Lines pinned to the bottom of a `rows`-high screen.
fn pin_bottom(rows: usize, block: &[String]) -> Vec<u8> {
    let start = (rows + 1).saturating_sub(block.len()).max(1);
    let mut s = format!("\x1b[{start};1H");
    // Erase each row first: the history above may reach down into the block.
    s.push_str(&block.iter().map(|l| format!("\x1b[2K{}", paint(l))).collect::<Vec<_>>().join("\r\n"));
    s.into_bytes()
}

/// What Claude Code asks for in the scene.
pub(super) struct Ask {
    pub command: &'static str,
    pub description: &'static str,
    pub always: &'static str,
    pub risk: Option<&'static str>,
}

pub(super) const FORCE_PUSH: Ask = Ask {
    command: "git push --force origin main",
    description: "Publish the rate limiter",
    always: "2. Yes, and don't ask again for git push commands in /Users/maya/dev/aurora",
    risk: Some("Force-push rewrites history on origin/main"),
};

fn claude(s: &mut Stage, idx: usize, ask: &Ask) {
    let (cols, rows) = (s.pane(idx).terminal.cols, s.pane(idx).terminal.rows);
    let mut sc = Script::new();
    sc.out("{bold}>{0} add rate limiting to /v1/orders");
    sc.out("");
    sc.out("{bwhite}\u{25cf}{0} I'll add a token-bucket middleware.");
    sc.out("");
    sc.out("{bgreen}\u{25cf}{0} {bold}Read{0}(src/router.rs)");
    sc.out("  {dim}\u{23bf}  Read 142 lines{0}");
    sc.out("");
    sc.out("{bgreen}\u{25cf}{0} {bold}Write{0}(src/middleware/rate_limit.rs)");
    sc.out("  {dim}\u{23bf}  Wrote 61 lines{0}");
    sc.out("");
    sc.out("{bgreen}\u{25cf}{0} {bold}Update{0}(src/router.rs)");
    sc.out("  {dim}\u{23bf}  Updated with 6 additions{0}");
    s.feed(idx, &sc);

    let w = cols.saturating_sub(2).min(74);
    let inner = w.saturating_sub(4);
    let row = |t: &str| format!("{{byellow}}\u{2502}{{0}} {} {{byellow}}\u{2502}{{0}}", pad(t, inner));
    let mut block = vec![format!("{{byellow}}\u{256d}{}\u{256e}{{0}}", "\u{2500}".repeat(w - 2))];
    block.push(row("Bash command"));
    block.push(row(""));
    block.push(row(&format!("  {}", ask.command)));
    block.push(row(&format!("  {}", ask.description)));
    block.push(row(""));
    block.push(row("Do you want to proceed?"));
    block.push(row("\u{276f} 1. Yes"));
    for (i, l) in wrap(ask.always, inner.saturating_sub(2)).iter().enumerate() {
        block.push(row(&format!("  {}{l}", if i == 0 { "" } else { "   " })));
    }
    block.push(row("  3. No, and tell Claude what to do differently (esc)"));
    block.push(format!("{{byellow}}\u{2570}{}\u{256f}{{0}}", "\u{2500}".repeat(w - 2)));
    block.push("{dim}  Opus 5.5 \u{b7} 57929 tokens \u{b7} $0.42 spent{0}".into());
    block.push("{dim}  4h 37m until reset \u{b7} 59% context left{0}".into());
    s.pane(idx).feed(&pin_bottom(rows, &block));
}

fn codex(s: &mut Stage, idx: usize) {
    let (cols, rows) = (s.pane(idx).terminal.cols, s.pane(idx).terminal.rows);
    let mut sc = Script::new();
    sc.out("{bold}>_ OpenAI Codex{0} {dim}(v0.46.0){0}");
    sc.out("");
    sc.out("{dim}model:{0}     gpt-5-codex medium");
    sc.out("{dim}directory:{0} ~/dev/aurora/web");
    sc.out("");
    sc.out("{bold}\u{203a}{0} migrate the settings page to the new form components");
    sc.out("");
    sc.out("{bold}\u{2022} Exploring{0}");
    sc.out("  {dim}\u{2514}{0} Read SettingsForm.tsx");
    sc.out("{bold}\u{2022} Updating{0} SettingsForm.tsx {bgreen}(+42{0} {bred}-17){0}");
    s.feed(idx, &sc);
    let w = cols.saturating_sub(1);
    let footer = {
        let left = "  \u{23ce} send   \u{2303}C quit";
        let right = "74% context left";
        let gap = w.saturating_sub(left.chars().count() + right.chars().count());
        format!("{{dim}}{left}{}{right}{{0}}", " ".repeat(gap))
    };
    let block = vec![
        "{byellow}\u{25e6} Working{0} {dim}(1m 12s \u{2022} esc to interrupt){0}".to_string(),
        String::new(),
        "{bold}\u{203a}{0} {dim}Ask Codex to do anything{0}".to_string(),
        String::new(),
        footer,
    ];
    s.pane(idx).feed(&pin_bottom(rows, &block));
}

fn gemini(s: &mut Stage, idx: usize) {
    let (cols, rows) = (s.pane(idx).terminal.cols, s.pane(idx).terminal.rows);
    let mut sc = Script::new();
    sc.out("{bblue}>{0} write the changelog entry for 0.4.2");
    sc.out("");
    sc.out("{bmagenta}\u{2726}{0} Added a 0.4.2 section to CHANGELOG.md:");
    sc.out("  {dim}- rate limiting on /v1/orders{0}");
    sc.out("  {dim}- fixed refresh-token rotation{0}");
    s.feed(idx, &sc);
    let left = "~/aurora/docs (main*)";
    let right = "gemini-2.5-pro (93% context left)";
    let gap = cols.saturating_sub(left.chars().count() + right.chars().count() + 2);
    let mut block = vec![format!("{{dim}}{left}{}{right}{{0}}", " ".repeat(gap))];
    block.insert(0, String::new());
    // The CLI has quit: the shell prompt follows.
    let at = rows.saturating_sub(2).max(1);
    let mut b = pin_bottom(at, &block);
    b.extend(b"\r\n");
    s.pane(idx).feed(&b);
    let p = Prompt::new("docs", &format!("{HOME}/aurora/docs")).git("main", "");
    let mut sc = Script::new();
    sc.prompt(&p);
    s.feed(idx, &sc);
}

fn digest(id: u64, secs: Option<u64>, files: Option<(usize, usize, usize)>) -> TurnDigest {
    TurnDigest {
        id,
        label: format!("Turn {id}"),
        running: secs.is_none(),
        duration: secs.map(Duration::from_secs),
        summary: files.map(|(f, a, r)| Summary { files: f, added: a, removed: r }),
        failed: false,
    }
}

fn ago(now: Instant, secs: u64) -> Instant {
    now.checked_sub(Duration::from_secs(secs)).unwrap_or(now)
}

/// The default scene: Claude Code waits on a risky command, Codex works, Gemini is done.
pub fn build(s: &mut Stage) {
    base(s);
}

/// Reply composer open on the Codex card (marked for broadcast).
pub fn build_reply(s: &mut Stage) {
    base(s);
    let codex = s.wm.active_tab().panes()[1].id;
    s.agents_ui.selected = Some(codex);
    s.agents_ui.ctl.marked = vec![codex];
    s.agents_ui.ctl.start_compose(false);
    s.agents_ui.ctl.composer.insert_str("also cover the retry path with a test");
}

/// Compact density with a card's context menu open.
pub fn build_menu(s: &mut Stage) {
    base(s);
    let claude = s.wm.active_tab().panes()[0].id;
    s.agents_ui.compact = true;
    s.agents_ui.ctl.open_menu(claude);
}

pub(super) fn base(s: &mut Stage) {
    base_with(s, &FORCE_PUSH);
}

pub(super) fn base_with(s: &mut Stage, ask: &Ask) {
    s.window_title = "aurora \u{2014} rift".into();
    s.set_tabs(&["aurora"]);
    s.dock_cols = 42;
    s.agents_ui.visible = true;
    s.agents_ui.focused = true;
    // Left: Claude Code. Right: Codex over Gemini.
    {
        let t = s.wm.active_tab_mut();
        let (cols, rows) = {
            let p = t.active_pane();
            (p.terminal.cols, p.terminal.rows)
        };
        t.split(SplitDir::Horizontal, Pane::scripted(101, cols / 2, rows));
        t.focus_pane(1);
        t.split(SplitDir::Vertical, Pane::scripted(102, cols / 2, rows / 2));
        if let PaneNode::Split { ratio, .. } = &mut t.root {
            *ratio = 0.54;
        }
        t.focus_pane(0);
    }
    s.layout();
    claude(s, 0, ask);
    codex(s, 1);
    gemini(s, 2);

    // Registry: three agents with different histories.
    let now = Instant::now();
    let uids: Vec<usize> = s.wm.active_tab().panes().iter().map(|p| p.id).collect();
    let cmds = [(uids[0], "claude --resume", AgentKind::ClaudeCode), (uids[1], "codex", AgentKind::Codex), (uids[2], "gemini", AgentKind::Gemini)];
    let started = [ago(now, 52 * 60), ago(now, 14 * 60), ago(now, 41 * 60)];
    for (i, (uid, cmd, _)) in cmds.iter().enumerate() {
        let obs = PaneObs { uid: *uid, tab_index: 0, osc_seen: true, block_running: true, running_cmd: Some((*cmd).into()), ..Default::default() };
        s.agents.observe_pane(&obs, &mut || Probe::Unknown, started[i]);
    }
    let screens: Vec<Vec<String>> = (0..3).map(|i| crate::agents::runtime::screen_lines(&s.pane(i).terminal)).collect();
    // Codex: spinner on screen -> working since 7 minutes.
    s.agents.observe_screen(uids[1], &screens[1], 1, ago(now, 7 * 60));
    // Claude: asked for permission 3 minutes ago.
    s.agents.observe_notification(uids[0], "Claude Code", "needs your permission to use Bash", ago(now, 3 * 60));
    // Gemini: quit 12 minutes ago.
    let done = PaneObs { uid: uids[2], tab_index: 0, osc_seen: true, block_running: false, last_exit: Some(0), ..Default::default() };
    s.agents.observe_pane(&done, &mut || Probe::Unknown, ago(now, 12 * 60));
    // Claude and Codex share a checkout; Gemini has its own worktree.
    s.agents.pin_place(uids[0], &format!("{HOME}/aurora"), "aurora", "feat/rate-limit");
    s.agents.pin_place(uids[1], &format!("{HOME}/aurora"), "aurora", "feat/rate-limit");
    s.agents.pin_place(uids[2], &format!("{HOME}/aurora-gemini-1"), "aurora", "agent/gemini-1");

    // Per-agent view, produced by the same parsers the app runs.
    for (i, (uid, _, kind)) in cmds.iter().enumerate() {
        let lines = &screens[i];
        let mut info = PaneInfo { metrics: metrics::parse_screen(Some(*kind), lines), ..Default::default() };
        if i == 0 {
            info.prompt = prompt::parse(Some(*kind), lines);
            info.risk = ask.risk.map_or(Risk::Safe, |r| Risk::Risky(vec![r.into()]));
            info.settled = true;
            info.turns = vec![digest(1, Some(161), Some((3, 45, 12))), digest(2, Some(58), Some((0, 0, 0))), digest(3, None, None)];
            info.files = (Some(3), 5);
        } else if i == 1 {
            info.turns = vec![digest(1, Some(412), Some((6, 210, 88))), digest(2, None, None)];
            info.files = (Some(6), 6);
        } else {
            info.turns = vec![digest(1, Some(34), Some((1, 18, 0)))];
            info.files = (Some(1), 1);
        }
        s.agents_ui.info.insert(*uid, info);
    }
    s.agents_ui.selected = Some(uids[0]);
}
