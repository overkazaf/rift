//! Starting workflows: the wizard, preparing worktrees on a background
//! thread, opening the agents' panes and handing out the prompts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::bestof::{self, BestOfN};
use super::deliver::{self, Delivery, PendingSend, SendTag};
use super::fixtests::{self, FixTests};
use super::review_loop::{self, WriteReview};
use super::template::{self, expand, Strategy, Template, Vars};
use super::ui::{Spec, Wizard};
use super::{BestRun, FixRun, JobMsg, Prep, ReviewRun, Run};
use crate::agents::launch;
use crate::agents::runtime::{self, Slot};
use crate::agents::{git, AgentKind};
use crate::app::App;

/// A start request waiting for its worktrees.
pub(crate) struct StartCtx {
    pub spec: Spec,
    pub cwd: PathBuf,
    /// "Creating 3 worktrees for \"add retry\"" for the progress toast.
    pub label: String,
}

/// Built-ins plus `workflows.toml`. Problems in the file show up once as a toast.
pub(crate) fn templates(app: &mut App) -> Vec<Template> {
    let loaded = template::load_user_templates();
    if loaded.warnings != app.workflows.warned {
        if let Some(first) = loaded.warnings.first() {
            log::warn!("workflows.toml: {}", loaded.warnings.join("; "));
            app.blocks_ui.show_toast(format!("workflows.toml: {first}{}", if loaded.warnings.len() > 1 { format!(" (+{} more)", loaded.warnings.len() - 1) } else { String::new() }));
        }
        app.workflows.warned = loaded.warnings.clone();
    }
    template::merge_templates(loaded.templates)
}

/// "Workflow: Best of N...": three agents to begin with, 2 to 4 and mixed kinds allowed.
pub fn open_best_of_n(app: &mut App) {
    let mut t = Template::new("Best of N", Strategy::BestOf);
    t.description = "Several agents, one task, compare and merge the best".into();
    open_with(app, t);
}

pub fn open_template(app: &mut App, name: &str) {
    let all = templates(app);
    match all.into_iter().find(|t| t.name.eq_ignore_ascii_case(name)) {
        Some(t) => open_with(app, t),
        None => {
            app.blocks_ui.show_toast(format!("No workflow called \"{name}\""));
            app.request_redraw();
        }
    }
}

fn open_with(app: &mut App, tpl: Template) {
    let installed = runtime::installed(app);
    if installed.is_empty() {
        app.blocks_ui.show_toast("No agent CLI found on PATH (claude, codex, gemini, opencode, aider, cursor-agent)");
        app.request_redraw();
        return;
    }
    let cwd = runtime::active_cwd(app);
    let info = git::repo_info(&cwd);
    if matches!(tpl.strategy, Strategy::BestOf | Strategy::WriteReview) && info.is_none() {
        app.blocks_ui.show_toast("This workflow needs a git repository: cd into one first");
        app.request_redraw();
        return;
    }
    let default_kind = runtime::default_agent(app).unwrap_or(installed[0]);
    let place = match &info {
        Some(i) => format!("{} \u{b7} {}", i.name, i.branch.clone().unwrap_or_else(|| "detached".into())),
        None => cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "~".into()),
    };
    let root = info.as_ref().map(|i| i.root.clone()).unwrap_or_else(|| cwd.clone());
    let test_default = template::detect_test_command(&root).unwrap_or_default();
    let mut tpl = tpl;
    if tpl.agents.is_empty() && tpl.strategy == Strategy::BestOf {
        tpl.agents = vec![default_kind; 3];
    }
    app.workflows.ui.close_all();
    app.workflows.ui.wizard = Some(Wizard::new(tpl, installed, default_kind, place, info.is_some(), &test_default));
    app.request_redraw();
}

// ───────────────────────────── starting ─────────────────────────────

pub(crate) fn start(app: &mut App, spec: Spec) {
    let id = app.workflows.fresh_id();
    let cwd = runtime::active_cwd(app);
    let n = spec.kinds.len();
    let label = match spec.tpl.strategy {
        Strategy::BestOf => format!("Creating {n} worktrees for \"{}\"", crate::ui::kit::ellipsize(spec.task.lines().next().unwrap_or(""), 32)),
        _ if spec.worktree => "Creating a worktree".to_string(),
        _ => "Starting".to_string(),
    };
    let known = app.workflows.delivery.clone();
    let tx = app.workflows.tx.clone();
    let (strategy, worktree, kinds, task, dir) = (spec.tpl.strategy, spec.worktree, spec.kinds.clone(), spec.task.clone(), cwd.clone());
    app.workflows.starting.insert(id, StartCtx { spec, cwd, label });
    std::thread::spawn(move || {
        let result = prepare(strategy, worktree, &kinds, &task, &dir, &known);
        let _ = tx.send(JobMsg::Prepared { id, result });
        crate::wake::wake();
    });
    app.request_redraw();
}

/// Blocking: probe the CLIs' `--help` and create the worktrees.
fn prepare(strategy: Strategy, worktree: bool, kinds: &[AgentKind], task: &str, cwd: &Path, known: &HashMap<AgentKind, Delivery>) -> Result<Prep, String> {
    let mut unknown: Vec<AgentKind> = Vec::new();
    for k in kinds {
        if !known.contains_key(k) && !unknown.contains(k) {
            unknown.push(*k);
        }
    }
    let probes: Vec<_> = unknown.into_iter().map(|k| std::thread::spawn(move || (k, deliver::probe(k, Duration::from_secs(6))))).collect();
    let info = git::repo_info(cwd);
    let mut prep = Prep { root: info.as_ref().map(|i| i.root.clone()), base_branch: None, base_commit: String::new(), slug: String::new(), plans: Vec::new(), deliveries: known.clone() };
    match strategy {
        Strategy::BestOf => {
            let info = info.ok_or("not inside a git repository")?;
            let (branch, commit) = super::gitops::head_info(&info.root)?;
            let root = info.root.clone();
            let taken = |p: &Path, b: &str| p.exists() || launch::branch_exists(&root, b);
            let (slug, plans) = launch::plan_bestof(&info.root, &info.name, &bestof::slugify(task, 24), kinds.len(), &taken);
            if plans.is_empty() {
                return Err("could not find free worktree names".into());
            }
            prep.plans = launch::run_plans(plans)?;
            prep.base_branch = branch;
            prep.base_commit = commit;
            prep.slug = slug;
        }
        _ if worktree => {
            let info = info.ok_or("not inside a git repository")?;
            prep.plans = launch::create_worktrees(&info.root, &info.name, kinds[0], 1)?;
            if let Ok((b, c)) = super::gitops::head_info(&info.root) {
                prep.base_branch = b;
                prep.base_commit = c;
            }
        }
        _ => {
            if let Some(i) = &info {
                if let Ok((b, c)) = super::gitops::head_info(&i.root) {
                    prep.base_branch = b;
                    prep.base_commit = c;
                }
            }
        }
    }
    for p in probes {
        if let Ok((k, d)) = p.join() {
            prep.deliveries.insert(k, d);
        }
    }
    Ok(prep)
}

/// The worktrees exist: open the panes and hand out the task.
pub(crate) fn finish_start(app: &mut App, id: u64, ctx: StartCtx, prep: Prep) {
    app.workflows.delivery.extend(prep.deliveries.iter().map(|(k, d)| (*k, *d)));
    match ctx.spec.tpl.strategy {
        Strategy::BestOf => finish_best_of(app, id, ctx, prep),
        Strategy::WriteReview => finish_write_review(app, id, ctx, prep),
        Strategy::FixTests => finish_fix_tests(app, id, ctx, prep),
        Strategy::Single => finish_single(app, ctx, prep),
    }
    app.request_redraw();
}

fn delivery_for(prep: &Prep, kind: AgentKind) -> Delivery {
    prep.deliveries.get(&kind).copied().unwrap_or(Delivery::Typed)
}

fn finish_best_of(app: &mut App, id: u64, ctx: StartCtx, prep: Prep) {
    let spec = ctx.spec;
    let Some(root) = prep.root.clone() else { return };
    let mut sm = BestOfN::new(id, &spec.task, root, prep.base_branch.clone(), prep.base_commit.clone(), &spec.kinds, &spec.test);
    sm.slug = prep.slug.clone();
    sm.on_event(bestof::Ev::Created(Ok(prep.plans.iter().map(|p| (p.path.clone(), p.branch.clone())).collect())));
    let mut slots = Vec::new();
    let mut prompts = Vec::new();
    let mut as_arg = Vec::new();
    for (p, kind) in prep.plans.iter().zip(&spec.kinds) {
        let vars = Vars { task: spec.task.clone(), branch: p.branch.clone(), cwd: p.path.display().to_string() };
        let prompt = expand(&spec.tpl.prompt, &vars);
        let (cmd, arg) = deliver::launch_with_prompt(*kind, delivery_for(&prep, *kind), &prompt);
        slots.push(Slot { dir: p.path.clone(), label: p.branch.clone(), kind: *kind, cmd });
        prompts.push(prompt);
        as_arg.push(arg);
    }
    let n = slots.len();
    let (cols, rows) = bestof::grid_for(n);
    let title = format!("best of {n} \u{b7} {}", prep.slug);
    let (uids, refused) = runtime::open_grid(app, &title, cols, rows, &slots);
    if refused {
        app.blocks_ui.show_toast("Window too small for the whole grid: extra candidates opened in tabs");
    }
    sm.on_event(bestof::Ev::Opened(uids.clone()));
    for ((uid, arg), prompt) in uids.iter().zip(&as_arg).zip(&prompts) {
        if *arg {
            sm.on_event(bestof::Ev::Delivered(*uid));
        } else {
            app.workflows.pending.push(PendingSend { uid: *uid, text: prompt.clone(), since: Instant::now(), tag: SendTag::Candidate { run: id } });
        }
    }
    app.workflows.runs.push(Run::Best(BestRun { parsed: (0..n).map(|_| None).collect(), sm, busy: None, message: None }));
    let arg_n = as_arg.iter().filter(|a| **a).count();
    app.blocks_ui.show_toast(format!(
        "Best of {n}: {} worktree{} created{}",
        n,
        if n == 1 { "" } else { "s" },
        if arg_n == n { "" } else { "; prompts are typed once each agent is ready" }
    ));
}

fn finish_write_review(app: &mut App, id: u64, ctx: StartCtx, prep: Prep) {
    let spec = ctx.spec;
    let dir = prep.plans.first().map(|p| p.path.clone()).unwrap_or_else(|| ctx.cwd.clone());
    let branch = prep.plans.first().map(|p| p.branch.clone()).or(prep.base_branch.clone()).unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    let vars = Vars { task: spec.task.clone(), branch: branch.clone(), cwd: dir.display().to_string() };
    let prompt = expand(&spec.tpl.prompt, &vars);
    let (cmd, arg) = deliver::launch_with_prompt(spec.kinds[0], delivery_for(&prep, spec.kinds[0]), &prompt);
    let slots = vec![
        Slot { dir: dir.clone(), label: branch.clone(), kind: spec.kinds[0], cmd },
        Slot::plain(dir.clone(), branch.clone(), spec.kinds[1]),
    ];
    let title = format!("write & review \u{b7} {branch}");
    let (uids, _) = runtime::open_grid(app, &title, 2, 1, &slots);
    if uids.len() < 2 {
        app.blocks_ui.show_toast("Could not open both panes");
        return;
    }
    let mut sm = WriteReview::new(id, &spec.task, uids[0], uids[1], spec.rounds, spec.tpl.auto_forward);
    if arg {
        sm.on_event(review_loop::Ev::WriterSent);
    } else {
        app.workflows.pending.push(PendingSend { uid: uids[0], text: prompt, since: Instant::now(), tag: SendTag::Writer { run: id } });
    }
    app.workflows.runs.push(Run::Review(ReviewRun { sm, waiting_range: None }));
    app.blocks_ui.show_toast(format!("Write & Review started: {} writes, {} reviews (up to {} rounds)", spec.kinds[0].name(), spec.kinds[1].name(), spec.rounds));
}

fn finish_fix_tests(app: &mut App, id: u64, ctx: StartCtx, prep: Prep) {
    let spec = ctx.spec;
    let dir = prep.plans.first().map(|p| p.path.clone()).unwrap_or_else(|| ctx.cwd.clone());
    let label = prep.plans.first().map(|p| p.branch.clone()).unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    let slot = Slot::plain(dir.clone(), label.clone(), spec.kinds[0]);
    let uid = runtime::open_slot_tab(app, &slot);
    let vars = Vars { task: spec.task.clone(), branch: label.clone(), cwd: dir.display().to_string() };
    let task = if spec.task.trim().is_empty() { String::new() } else { expand(&spec.tpl.prompt, &vars) };
    let sm = FixTests::new(id, uid, &task, &spec.test, spec.rounds);
    app.workflows.runs.push(Run::Fix(FixRun { sm, dir: dir.clone() }));
    spawn_tests(app, id);
    app.blocks_ui.show_toast(format!("Running `{}` first\u{2026}", spec.test));
}

fn finish_single(app: &mut App, ctx: StartCtx, prep: Prep) {
    let spec = ctx.spec;
    let dir = prep.plans.first().map(|p| p.path.clone()).unwrap_or_else(|| ctx.cwd.clone());
    let label = prep.plans.first().map(|p| p.branch.clone()).unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    let vars = Vars { task: spec.task.clone(), branch: label.clone(), cwd: dir.display().to_string() };
    let prompt = expand(&spec.tpl.prompt, &vars);
    let (cmd, arg) = deliver::launch_with_prompt(spec.kinds[0], delivery_for(&prep, spec.kinds[0]), &prompt);
    let uid = runtime::open_slot_tab(app, &Slot { dir, label, kind: spec.kinds[0], cmd });
    if !arg {
        app.workflows.pending.push(PendingSend { uid, text: prompt, since: Instant::now(), tag: SendTag::Queue });
    }
}

// ───────────────────────────── commands from the state machines ─────────────────────────────

pub(crate) fn run_review_cmds(app: &mut App, id: u64, cmds: Vec<review_loop::Cmd>) {
    for c in cmds {
        let (writer, reviewer) = match app.workflows.runs.iter().find(|r| r.id() == id) {
            Some(Run::Review(r)) => (r.sm.writer, r.sm.reviewer),
            _ => return,
        };
        match c {
            review_loop::Cmd::ComputeDiff => {} // the caller set `waiting_range`; `poll_ranges` does the rest
            review_loop::Cmd::SendReviewer(text) => app.workflows.pending.push(PendingSend { uid: reviewer, text, since: Instant::now(), tag: SendTag::Reviewer { run: id } }),
            review_loop::Cmd::SendWriter(text) => app.workflows.pending.push(PendingSend { uid: writer, text, since: Instant::now(), tag: SendTag::Writer { run: id } }),
            review_loop::Cmd::Finish(o) => {
                app.workflows.pending.retain(|p| p.uid != writer && p.uid != reviewer);
                app.blocks_ui.show_toast(format!("Write & Review {}", o.label()));
            }
        }
    }
    app.request_redraw();
}

pub(crate) fn run_fix_cmds(app: &mut App, id: u64, cmds: Vec<fixtests::Cmd>) {
    for c in cmds {
        let pane = match app.workflows.runs.iter().find(|r| r.id() == id) {
            Some(Run::Fix(f)) => f.sm.pane,
            _ => return,
        };
        match c {
            fixtests::Cmd::RunTests => spawn_tests(app, id),
            fixtests::Cmd::Send(text) => app.workflows.pending.push(PendingSend { uid: pane, text, since: Instant::now(), tag: SendTag::Fixer { run: id } }),
            fixtests::Cmd::Finish(o) => {
                app.workflows.pending.retain(|p| p.uid != pane);
                app.blocks_ui.show_toast(format!("Fix tests: {}", o.label()));
            }
        }
    }
    app.request_redraw();
}

/// Run the fix loop's test command on a worker thread.
fn spawn_tests(app: &mut App, id: u64) {
    let Some((dir, cmd)) = app.workflows.runs.iter().find_map(|r| match r {
        Run::Fix(f) if f.sm.id == id => Some((f.dir.clone(), f.sm.test_cmd.clone())),
        _ => None,
    }) else {
        return;
    };
    let tx = app.workflows.tx.clone();
    std::thread::spawn(move || {
        let result = match super::gitops::run_tests(&dir, &cmd, Duration::from_secs(20 * 60)) {
            super::bestof::TestStatus::Passed => Ok(()),
            super::bestof::TestStatus::Failed(out) => Err(out),
            _ => Err(String::new()),
        };
        let _ = tx.send(JobMsg::FixTests { run: id, result });
        crate::wake::wake();
    });
}

pub(crate) fn on_prepared(app: &mut App, id: u64, result: Result<Prep, String>) {
    let Some(ctx) = app.workflows.starting.remove(&id) else { return };
    match result {
        Ok(prep) => finish_start(app, id, ctx, prep),
        Err(e) => {
            log::error!("workflow start: {e}");
            app.blocks_ui.show_toast(format!("Workflow failed to start: {e}"));
            app.request_redraw();
        }
    }
}
