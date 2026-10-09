//! What the compare view's buttons do: merge a candidate, discard the others,
//! ask the AI to judge. Every destructive step goes through `ui/confirm.rs`
//! first and the git work runs on background threads.

use std::time::Duration;

use super::bestof::{self, CandState, Cmd, Phase, TestStatus};
use super::gitops::{self, DiscardItem, MergePlan};
use super::ui::{CompareAct, CompareState};
use super::{BestRun, JobMsg, Run};
use crate::app::App;
use crate::ui::confirm::{ConfirmAction, ConfirmRequest};
use crate::ui::kit::Tone;

/// A confirmation owned by the workflows.
pub enum WfConfirm {
    Merge { run: u64, idx: usize, plan: MergePlan },
    Discard { run: u64, items: Vec<(usize, DiscardItem)> },
    /// Information only.
    Notice,
}

fn best_mut(app: &mut App, id: u64) -> Option<&mut BestRun> {
    match app.workflows.run_mut(id) {
        Some(Run::Best(b)) => Some(b),
        _ => None,
    }
}

fn say(app: &mut App, id: u64, tone: Tone, msg: impl Into<String>) {
    let msg = msg.into();
    if let Some(b) = best_mut(app, id) {
        b.message = Some((tone, msg.clone()));
    }
    app.blocks_ui.show_toast(msg);
    app.request_redraw();
}

fn notice(app: &mut App, title: &str, badge: &str, tone: Tone, text: &str) {
    app.confirm.push(ConfirmRequest {
        title: title.to_string(),
        badge: Some((badge.to_string(), tone)),
        lines: text.lines().map(str::to_string).collect(),
        buttons: vec!["OK".into()],
        default_sel: 0,
        esc_choice: Some(0),
        tone,
        action: ConfirmAction::Workflow(Box::new(WfConfirm::Notice)),
    });
}

// ───────────────────────────── background results ─────────────────────────────

pub(crate) fn on_job(app: &mut App, msg: JobMsg) {
    match msg {
        JobMsg::Prepared { id, result } => super::start::on_prepared(app, id, result),
        JobMsg::ReviewDiff { run, result } => {
            let cmds = match app.workflows.run_mut(run) {
                Some(Run::Review(r)) => match result {
                    Ok(text) => r.sm.on_event(super::review_loop::Ev::DiffReady(Some(text))),
                    Err(e) => r.sm.on_event(super::review_loop::Ev::Fail(format!("could not read the diff: {e}"))),
                },
                _ => return,
            };
            super::start::run_review_cmds(app, run, cmds);
        }
        JobMsg::CandDiff { run, idx, result } => {
            let cmds = match app.workflows.run_mut(run) {
                Some(Run::Best(b)) => match result {
                    Ok((stat, parsed)) => {
                        if let Some(slot) = b.parsed.get_mut(idx) {
                            *slot = Some(parsed);
                        }
                        b.sm.on_event(bestof::Ev::DiffDone(idx, Ok(stat)))
                    }
                    Err(e) => {
                        if let Some(slot) = b.parsed.get_mut(idx) {
                            *slot = None;
                        }
                        b.sm.on_event(bestof::Ev::DiffDone(idx, Err(e)))
                    }
                },
                _ => return,
            };
            run_best_cmds(app, run, cmds);
        }
        JobMsg::CandTests { run, idx, status } => {
            let cmds = match app.workflows.run_mut(run) {
                Some(Run::Best(b)) => b.sm.on_event(bestof::Ev::TestsDone(idx, status)),
                _ => return,
            };
            run_best_cmds(app, run, cmds);
        }
        JobMsg::FixTests { run, result } => {
            let cmds = match app.workflows.run_mut(run) {
                Some(Run::Fix(f)) => f.sm.on_event(super::fixtests::Ev::TestsDone(result)),
                _ => return,
            };
            super::start::run_fix_cmds(app, run, cmds);
        }
        JobMsg::Preflight { run, idx, plan, result } => {
            if let Some(b) = best_mut(app, run) {
                b.busy = None;
            }
            match result {
                Err(e) => {
                    say(app, run, Tone::Warning, format!("Not merged: {e}"));
                    notice(app, "Not merged", "NOTHING CHANGED", Tone::Warning, &e);
                }
                Ok(pre) => {
                    let mut lines = vec![
                        format!("{} files, +{} -{} from branch {}.", pre.stat.files, pre.stat.added, pre.stat.removed, plan.cand_branch),
                        format!("Runs: git merge --no-ff {}   in {} ({}).", plan.cand_branch, plan.repo.display(), pre.target),
                    ];
                    if pre.uncommitted > 0 {
                        lines.push(format!("The candidate has {} uncommitted path{}: they are committed on its own branch first.", pre.uncommitted, if pre.uncommitted == 1 { "" } else { "s" }));
                    }
                    lines.push("Your main worktree is clean and stays untouched if the merge conflicts: it is aborted automatically.".to_string());
                    app.confirm.push(ConfirmRequest {
                        title: format!("Merge {} into {}?", plan.label, pre.target),
                        badge: Some(("git merge --no-ff".into(), Tone::Warning)),
                        lines,
                        buttons: vec!["Merge".into(), "Cancel".into()],
                        default_sel: 1,
                        esc_choice: Some(1),
                        tone: Tone::Warning,
                        action: ConfirmAction::Workflow(Box::new(WfConfirm::Merge { run, idx, plan })),
                    });
                }
            }
            app.request_redraw();
        }
        JobMsg::Merged { run, idx, result } => {
            let label = best_mut(app, run).map(|b| {
                b.busy = None;
                b.sm.cands.get(idx).map(|c| c.label.clone()).unwrap_or_default()
            });
            let Some(label) = label else { return };
            match result {
                Ok(done) => {
                    let target = best_mut(app, run).and_then(|b| b.sm.base_branch.clone()).unwrap_or_else(|| "HEAD".into());
                    if let Some(b) = best_mut(app, run) {
                        if let Some(c) = b.sm.cands.get_mut(idx) {
                            c.merged = true;
                        }
                    }
                    let short: String = done.sha.chars().take(7).collect();
                    say(app, run, Tone::Success, format!("Merged {label} into {target} ({short}). Press d to discard the other candidates."));
                }
                Err(e) => {
                    say(app, run, Tone::Danger, e.clone());
                    notice(app, "Merge failed", "ABORTED", Tone::Danger, &e);
                }
            }
        }
        JobMsg::Discarded { run, results } => {
            let mut ok = 0;
            let mut errors = Vec::new();
            if let Some(b) = best_mut(app, run) {
                b.busy = None;
                for (idx, label, r) in &results {
                    match r {
                        Ok(()) => {
                            ok += 1;
                            if let Some(c) = b.sm.cands.get_mut(*idx) {
                                c.removed = true;
                                c.pane = None;
                            }
                            if let Some(slot) = b.parsed.get_mut(*idx) {
                                *slot = None;
                            }
                        }
                        Err(e) => errors.push(format!("{label}: {e}")),
                    }
                }
            }
            if errors.is_empty() {
                say(app, run, Tone::Success, format!("Removed {ok} candidate{} (worktree and branch).", if ok == 1 { "" } else { "s" }));
            } else {
                say(app, run, Tone::Danger, format!("Removed {ok}; {} failed: {}", errors.len(), errors.join("; ")));
                notice(app, "Some candidates could not be removed", "PARTIAL", Tone::Warning, &errors.join("\n"));
            }
        }
    }
}

// ───────────────────────────── best-of-N commands ─────────────────────────────

pub(crate) fn run_best_cmds(app: &mut App, id: u64, cmds: Vec<Cmd>) {
    for c in cmds {
        match c {
            Cmd::Diff(i) => {
                let Some((dir, base)) = best_mut(app, id).and_then(|b| b.sm.cands.get(i).map(|c| (c.dir.clone(), b.sm.base_commit.clone()))) else { continue };
                let tx = app.workflows.tx.clone();
                std::thread::spawn(move || {
                    let result = gitops::candidate_diff(&dir, &base);
                    let _ = tx.send(JobMsg::CandDiff { run: id, idx: i, result });
                    crate::wake::wake();
                });
            }
            Cmd::Tests(i) => {
                let Some((dir, cmd)) = best_mut(app, id).and_then(|b| b.sm.cands.get(i).map(|c| (c.dir.clone(), b.sm.test_cmd.clone()))) else { continue };
                let tx = app.workflows.tx.clone();
                std::thread::spawn(move || {
                    let status = gitops::run_tests(&dir, &cmd, Duration::from_secs(20 * 60));
                    let _ = tx.send(JobMsg::CandTests { run: id, idx: i, status });
                    crate::wake::wake();
                });
            }
            Cmd::CompareReady => {
                app.workflows.open_compare = Some(id);
                app.blocks_ui.show_toast("Best of N: every candidate finished. Opening the comparison");
            }
            Cmd::Abort(msg) => app.blocks_ui.show_toast(format!("Best of N: {msg}")),
        }
    }
    app.request_redraw();
}

pub(crate) fn open_compare(app: &mut App, id: u64) {
    let Some(sel) = app.workflows.best_run(id).map(|b| b.sm.suggested()) else { return };
    app.workflows.ui.close_all();
    app.workflows.ui.compare = Some(CompareState::new(id, sel));
    app.request_redraw();
}

// ───────────────────────────── compare actions ─────────────────────────────

pub(crate) fn compare_act(app: &mut App, id: u64, act: CompareAct) {
    match act {
        CompareAct::None | CompareAct::Close => {}
        CompareAct::Merge(i) => begin_merge(app, id, i),
        CompareAct::DiscardOthers(i) => begin_discard(app, id, i, false),
        CompareAct::DiscardThis(i) => begin_discard(app, id, i, true),
        CompareAct::Judge => judge(app, id),
    }
}

fn begin_merge(app: &mut App, id: u64, idx: usize) {
    let Some(b) = best_mut(app, id) else { return };
    if b.busy.is_some() {
        return;
    }
    let Some(c) = b.sm.cands.get(idx) else { return };
    let why = if c.removed {
        Some("That candidate was removed")
    } else if c.merged {
        Some("That candidate is already merged")
    } else if b.sm.phase != Phase::Ready {
        Some("Wait until every candidate has finished and the results are collected")
    } else {
        None
    };
    if let Some(w) = why {
        app.blocks_ui.show_toast(w);
        app.request_redraw();
        return;
    }
    let plan = MergePlan {
        repo: b.sm.repo.clone(),
        base_branch: b.sm.base_branch.clone(),
        cand_dir: c.dir.clone(),
        cand_branch: c.branch.clone(),
        label: c.label.clone(),
        task: b.sm.task.clone(),
    };
    let base = b.sm.base_commit.clone();
    b.busy = Some("Checking the main worktree".into());
    b.message = None;
    let tx = app.workflows.tx.clone();
    std::thread::spawn(move || {
        let result = gitops::preflight(&plan, &base);
        let _ = tx.send(JobMsg::Preflight { run: id, idx, plan, result });
        crate::wake::wake();
    });
    app.request_redraw();
}

/// Ask before removing `keep`'s siblings (or, with `only_this`, the merged candidate itself).
fn begin_discard(app: &mut App, id: u64, keep: usize, only_this: bool) {
    let Some(b) = best_mut(app, id) else { return };
    if b.busy.is_some() {
        return;
    }
    let picks: Vec<usize> = if only_this { vec![keep] } else { (0..b.sm.cands.len()).filter(|i| *i != keep).collect() };
    if only_this && !b.sm.cands.get(keep).is_some_and(|c| c.merged) {
        app.blocks_ui.show_toast("Only a merged candidate can be removed this way: press d to discard the others");
        app.request_redraw();
        return;
    }
    let items: Vec<(usize, DiscardItem)> = picks
        .into_iter()
        .filter_map(|i| {
            let c = b.sm.cands.get(i)?;
            (!c.removed).then(|| (i, DiscardItem { dir: c.dir.clone(), branch: c.branch.clone(), label: c.label.clone() }))
        })
        .collect();
    if items.is_empty() {
        app.blocks_ui.show_toast("Nothing left to discard");
        app.request_redraw();
        return;
    }
    let mut lines = Vec::new();
    for (i, it) in &items {
        let c = &b.sm.cands[*i];
        let work = match (c.merged, c.diff) {
            (true, _) => "merged".to_string(),
            (false, Some(d)) if d.files > 0 => format!("{} files +{} -{} NOT merged", d.files, d.added, d.removed),
            _ => "no changes".to_string(),
        };
        let running = if matches!(c.state, CandState::Working) { ", agent still working" } else { "" };
        lines.push(format!("{}: {}  ({work}{running})", it.label, it.dir.display()));
    }
    lines.push("Runs for each: git worktree remove --force <dir> and git branch -D <branch>. Their panes are closed.".to_string());
    lines.push("Uncommitted work in those worktrees is lost.".to_string());
    let n = items.len();
    app.confirm.push(ConfirmRequest {
        title: format!("Remove {n} candidate{}?", if n == 1 { "" } else { "s" }),
        badge: Some(("DELETES WORK".into(), Tone::Danger)),
        lines,
        buttons: vec!["Remove".into(), "Cancel".into()],
        default_sel: 1,
        esc_choice: Some(1),
        tone: Tone::Danger,
        action: ConfirmAction::Workflow(Box::new(WfConfirm::Discard { run: id, items })),
    });
    app.request_redraw();
}

/// The user answered a workflow confirmation.
pub fn resolve_confirm(app: &mut App, wf: WfConfirm, choice: Option<usize>) {
    match wf {
        WfConfirm::Notice => {}
        WfConfirm::Merge { run, idx, plan } => {
            if choice != Some(0) {
                say(app, run, Tone::Neutral, "Merge cancelled");
                return;
            }
            if let Some(b) = best_mut(app, run) {
                b.busy = Some(format!("Merging {}", plan.label));
            }
            let tx = app.workflows.tx.clone();
            std::thread::spawn(move || {
                let result = gitops::execute_merge(&plan);
                let _ = tx.send(JobMsg::Merged { run, idx, result });
                crate::wake::wake();
            });
        }
        WfConfirm::Discard { run, items } => {
            if choice != Some(0) {
                say(app, run, Tone::Neutral, "Nothing was removed");
                return;
            }
            let Some((repo, panes)) = best_mut(app, run).map(|b| {
                b.busy = Some("Removing candidates".into());
                let panes: Vec<usize> = items.iter().filter_map(|(i, _)| b.sm.cands.get(*i).and_then(|c| c.pane)).collect();
                (b.sm.repo.clone(), panes)
            }) else {
                return;
            };
            // The agents hold their worktrees: close the panes first.
            for uid in panes {
                if app.wm.locate_pane(uid).is_some() {
                    crate::agents::console::close_pane(app, uid);
                }
            }
            let tx = app.workflows.tx.clone();
            std::thread::spawn(move || {
                let list: Vec<DiscardItem> = items.iter().map(|(_, it)| it.clone()).collect();
                let outcome = gitops::execute_discard(&repo, &list);
                let results = items.iter().zip(outcome).map(|((i, _), (label, r))| (*i, label, r)).collect();
                let _ = tx.send(JobMsg::Discarded { run, results });
                crate::wake::wake();
            });
        }
    }
    app.request_redraw();
}

// ───────────────────────────── AI judge ─────────────────────────────

/// The AI request that compares the candidates. Diffs pass through the chat's
/// outbound hygiene (secret redaction, size caps) and its consent check.
pub fn judge_request(run: &BestRun) -> crate::ai::hub::AskRequest {
    use crate::ai::hub::{AskRequest, ContextItem, Intent};
    let n = run.sm.cands.len();
    let question = format!(
        "Task given to every candidate:\n{}\n\nThe {n} items above are candidate implementations of that task: unified diffs against the same base commit, with test results. \
         Pick the one to merge: correctness first, then tests, scope and simplicity. Say which candidate wins, the two or three decisive reasons, and any bug or risk you see in the winner or the others. Be concise.",
        run.sm.task.trim()
    );
    let mut req = AskRequest::new(question, Intent::Explain).display(format!("Judge {n} best-of-N candidates"));
    for (i, c) in run.sm.cands.iter().enumerate() {
        if c.removed {
            continue;
        }
        let tests = match &c.tests {
            TestStatus::None => "no test command".to_string(),
            TestStatus::Passed => "tests passed".to_string(),
            TestStatus::Failed(_) => "tests FAILED".to_string(),
            TestStatus::Pending | TestStatus::Running => "tests not finished".to_string(),
        };
        let stat = c.diff.map(|d| format!("{} files +{} -{}", d.files, d.added, d.removed)).unwrap_or_else(|| "diff unavailable".into());
        let patch = run.parsed.get(i).and_then(|p| p.as_ref()).map(|p| p.full_patch()).unwrap_or_default();
        let (patch, cut) = super::review_loop::cap_text(&patch, 14_000);
        let mut text = format!("Candidate {} of {n}: {} (branch {}). {stat}; {tests}.\n\n{patch}", i + 1, c.label, c.branch);
        if cut {
            text.push_str("\n(diff truncated)");
        }
        req = req.with(ContextItem::Selection(text));
    }
    req
}

fn judge(app: &mut App, id: u64) {
    let Some(req) = app.workflows.best_run(id).map(judge_request) else { return };
    // The chat sidebar answers; leave the overlay so it is visible.
    app.workflows.ui.compare = None;
    crate::ai::hub::ask(app, req);
    app.blocks_ui.show_toast("Asking the AI to judge. Reopen the comparison: palette > Workflow: Compare Candidates");
    app.request_redraw();
}
