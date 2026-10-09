//! Scene definitions: each scene builds a [`Stage`] (window manager with
//! scripted panes + every overlay's real state) and [`Stage::render`]
//! composes a frame in the same order as `app::lifecycle::redraw`.

use std::time::Instant;

use super::page;
use super::shell::{paint, Prompt, Script};
use crate::ai::advisor::{Advisor, AdvisorReview, RiskLevel};
use crate::ai::chat::session::Message;
use crate::ai::chat::ChatUi;
use crate::ai::hub::{AskRequest, ContextItem, Intent};
use crate::ai::inline::context::{BlockSnap, BlockSource, Resolved};
use crate::ai::inline::fix::FixSuggestion;
use crate::ai::inline::line_edit::LineEdit;
use crate::ai::inline::{ActiveFix, BlockSig, InlineAi, NlGhost, NlState, Popover, PopoverMode};
use crate::blocks_ui::view::Button;
use crate::blocks_ui::{BlocksUi, Hover};
use crate::config::Theme;
use crate::network::browser::chrome::{BrowserLayout, PageInfo};
use crate::network::browser::BrowserUi;
use crate::renderer::{Renderer, SplitUiState};
use crate::tools::blocks::BlockManager;
use crate::tools::command_palette::{CommandPalette, History, PaletteContext, PaletteKey, SshHostInfo};
use crate::tools::exec_preview::ExecPreview;
use crate::tools::hud::{Hud, HudData};
use crate::window::tab::{PaneNode, SplitDir, Tab};
use crate::window::{Pane, PaneRect, WindowManager};

pub const NAMES: [&str; 16] = [
    "hero",
    "blocks",
    "ai-chat",
    "cmdk",
    "fix-suggestion",
    "nl-command",
    "splits",
    "palette",
    "browser",
    "preview-accept",
    "effects-crt",
    "hud",
    "agents-dock",
    "mission-control",
    "mission-control-reply",
    "mission-control-menu",
];

pub struct SceneSpec {
    /// Multiplier on the base font size (dense scenes use a smaller font).
    pub font_mul: f32,
    pub build: fn(&mut Stage),
}

pub fn spec(name: &str) -> Option<SceneSpec> {
    // Font sizes are fractions of the 24px base: dense scenes (several panes,
    // docked sidebars) use 20px, single-pane scenes 21px.
    const DENSE: f32 = 20.0 / 24.0;
    const NORMAL: f32 = 21.0 / 24.0;
    let (font_mul, build): (f32, fn(&mut Stage)) = match name {
        "hero" => (DENSE, hero),
        "blocks" => (NORMAL, blocks),
        "ai-chat" => (DENSE, ai_chat),
        "cmdk" => (NORMAL, cmdk),
        "fix-suggestion" => (NORMAL, fix_suggestion),
        "nl-command" => (NORMAL, nl_command),
        "splits" => (DENSE, splits),
        "palette" => (NORMAL, palette),
        "browser" => (DENSE, browser),
        "preview-accept" => (NORMAL, preview_accept),
        "effects-crt" => (NORMAL, effects_crt),
        "hud" => (NORMAL, hud),
        "agents-dock" => (DENSE, agents_dock),
        "mission-control" => (18.0 / 24.0, mission_control),
        "mission-control-reply" => (18.0 / 24.0, |s| super::mission::build_reply(s)),
        "mission-control-menu" => (18.0 / 24.0, |s| super::mission::build_menu(s)),
        _ => return None,
    };
    Some(SceneSpec { font_mul, build })
}

// ─────────────────────────────────────────────────────────────────────────
// Stage
// ─────────────────────────────────────────────────────────────────────────

pub struct BrowserScene {
    pub ui: BrowserUi,
    pub url: String,
    pub title: String,
}

/// Everything a frame needs; mirrors the fields of `App` that `redraw` reads.
pub struct Stage {
    pub renderer: Renderer,
    pub wm: WindowManager,
    pub w: usize,
    pub h: usize,
    pub window_title: String,
    pub blocks: BlockManager,
    pub blocks_ui: BlocksUi,
    pub inline_ai: InlineAi,
    pub chat: ChatUi,
    pub advisor: Advisor,
    pub hud: Option<Hud>,
    pub palette: Option<CommandPalette>,
    pub exec_preview: Option<ExecPreview>,
    pub browser: Option<BrowserScene>,
    pub split_ui: SplitUiState,
    /// Mission Control: registry + dock state (same types the app owns).
    pub agents: crate::agents::AgentRegistry,
    pub agents_ui: crate::agents::ui::AgentsUi,
    /// Preferred dock width in columns (0 = default).
    pub dock_cols: usize,
    font_path: String,
    font_px: f32,
}

impl Stage {
    pub fn new(w: usize, h: usize, font_path: &str, font_px: f32, theme: Theme) -> Self {
        let mut renderer = Renderer::new(font_path, font_px, theme);
        renderer.opacity = 1.0;
        let (cw, ch) = (renderer.cell_width(), renderer.cell_height());
        let wm = WindowManager::headless((w / cw).max(20), (h / ch).max(5));
        Self {
            renderer,
            wm,
            w,
            h,
            window_title: "aurora \u{2014} rift".into(),
            blocks: BlockManager::new(),
            blocks_ui: BlocksUi::new(),
            inline_ai: InlineAi::new(),
            chat: ChatUi::new(),
            advisor: Advisor::new(),
            hud: None,
            palette: None,
            exec_preview: None,
            browser: None,
            split_ui: SplitUiState::default(),
            agents: crate::agents::AgentRegistry::new(),
            agents_ui: crate::agents::ui::AgentsUi::new(),
            dock_cols: 0,
            font_path: font_path.to_string(),
            font_px,
        }
    }

    pub fn cell(&self) -> (usize, usize) {
        (self.renderer.cell_width(), self.renderer.cell_height())
    }

    pub fn tab_bar_h(&self) -> usize {
        self.renderer.cell_height() + 16
    }

    pub fn hud_h(&self) -> usize {
        if self.hud.is_some() {
            self.renderer.cell_height() * 3 + 20
        } else {
            0
        }
    }

    fn browser_layout(&self) -> Option<BrowserLayout> {
        let b = self.browser.as_ref()?;
        let (cw, ch) = self.cell();
        let avail = self.w - self.chat.dock_w(self.w);
        Some(BrowserLayout::compute(avail, self.h, self.tab_bar_h(), cw, ch, 2.0, b.ui.ratio, false))
    }

    /// Mission Control dock rectangle (None while hidden / too narrow / browser open).
    pub fn agents_dock(&self) -> Option<crate::ui::kit::Rect> {
        if !self.agents_ui.visible || self.browser.is_some() {
            return None;
        }
        crate::agents::ui::dock_rect_pref(self.w, self.h, self.tab_bar_h(), self.hud_h(), self.cell().0, self.dock_cols)
    }

    /// Same rule as `App::content_area_for`.
    pub fn content_area(&self) -> PaneRect {
        let avail = self.w - self.chat.dock_w(self.w);
        let dock = self.agents_dock().map_or(0, |r| r.w);
        let width = self.browser_layout().map_or(avail, |l| l.terminal_w.min(avail)).saturating_sub(dock);
        let tbh = self.tab_bar_h();
        PaneRect { x: dock, y: tbh, width, height: self.h.saturating_sub(tbh + self.hud_h()) }
    }

    /// Size every pane for the current dock/HUD/browser configuration.
    /// Call before feeding content (terminals reflow poorly after output).
    pub fn layout(&mut self) {
        let (cw, ch) = self.cell();
        let area = self.content_area();
        self.wm.resize_to(cw, ch, area);
        self.renderer.invalidate_all();
    }

    /// Resize the window (frame size) and re-lay out, like `handle_resize`.
    #[allow(dead_code)] // used by scenes that simulate a window resize
    pub fn resize_window(&mut self, w: usize, h: usize) {
        self.w = w;
        self.h = h;
        self.layout();
    }

    /// Feed the registry like `agents::runtime::poll`: every pane that runs
    /// `claude`, plus its current screen.
    pub fn observe_agents(&mut self, now: Instant) {
        for (ti, tab) in self.wm.tabs.iter().enumerate() {
            for pane in tab.panes() {
                let obs = crate::agents::registry::PaneObs {
                    uid: pane.id,
                    tab_index: ti,
                    block_running: true,
                    running_cmd: Some("claude".into()),
                    osc_seen: true,
                    cwd: Some(format!("{HOME}/aurora")),
                    bytes: pane.act.bytes,
                    ..Default::default()
                };
                self.agents.observe_pane(&obs, &mut || crate::agents::registry::Probe::Unknown, now);
                let lines = crate::agents::runtime::screen_lines(&pane.terminal);
                self.agents.observe_screen(pane.id, &lines, pane.act.bytes.max(1), now);
            }
        }
    }

    pub fn set_tabs(&mut self, titles: &[&str]) {
        for (i, t) in titles.iter().enumerate() {
            if i >= self.wm.tabs.len() {
                let id = self.wm.alloc_pane_id();
                let (cols, rows) = (self.wm.tabs[0].active_pane().terminal.cols, self.wm.tabs[0].active_pane().terminal.rows);
                let mut tab = Tab::new(Pane::scripted(id, cols, rows));
                tab.title = (*t).into();
                tab.custom_title = true;
                self.wm.tabs.push(tab);
            } else {
                self.wm.tabs[i].title = (*t).into();
                self.wm.tabs[i].custom_title = true;
            }
        }
        self.wm.active_tab = 0;
    }

    pub fn pane(&mut self, idx: usize) -> &mut Pane {
        self.wm.active_tab_mut().pane_mut(idx).expect("pane index")
    }

    pub fn feed(&mut self, idx: usize, script: &Script) {
        self.pane(idx).feed(&script.bytes);
    }

    /// Set the measured duration of the first N finished blocks.
    pub fn retime(&mut self, idx: usize, durations_ms: &[u64]) {
        let blocks = self.pane(idx).terminal.blocks.blocks_mut();
        for (b, d) in blocks.iter_mut().zip(durations_ms) {
            b.duration_ms = *d;
        }
    }

    /// Leave `n` blank rows under the cursor (for bars drawn below the prompt).
    pub fn pad_below(&mut self, idx: usize, n: usize) {
        let col = self.pane(idx).terminal.cursor_col;
        let s = format!("{}\x1b[{}A\x1b[{}G", "\r\n".repeat(n), n, col + 1);
        self.pane(idx).feed(s.as_bytes());
    }

    /// Compose a full frame; returns the app buffer (`w * h`, 0x00RRGGBB).
    pub fn render(&mut self) -> Vec<u32> {
        let (w, h) = (self.w, self.h);
        let (cw, ch) = self.cell();
        let tbh = self.tab_bar_h();
        let hud_h = self.hud_h();
        let area = self.content_area();
        let blayout = self.browser_layout();
        let mut buf = vec![0u32; w * h];

        self.renderer.render_tabbed_with_cmd(
            &self.wm, area, &mut buf, w as u32, h as u32, false, &self.blocks, self.split_ui,
        );
        crate::blocks_ui::draw::draw(&self.wm, &mut self.renderer, &self.blocks_ui, &mut buf, w as u32, h as u32, area);
        crate::ai::inline::draw(&self.wm, &mut self.renderer, &mut self.inline_ai, &mut buf, w, h, area, "");

        if let Some(hud) = &self.hud {
            let tk = crate::ui::kit::Tokens::new(&self.renderer.theme, cw, ch);
            let bar_h = ch * 3 + 20;
            let mut cx = crate::ui::kit::Ctx::new(&mut buf, w, h, &mut self.renderer.font, &tk);
            crate::tools::hud::draw(&mut cx, hud, h - bar_h, bar_h);
        }

        if self.agents_ui.visible || !self.agents.sessions().is_empty() {
            let dock = self.agents_dock();
            crate::agents::runtime::draw(
                &self.agents, &mut self.agents_ui, &self.wm, &mut self.renderer, &mut buf, w, h, area, dock, Instant::now(), "",
            );
        }

        if let Some(rect) = self.chat.dock_rect(w, h, tbh, hud_h) {
            self.chat.render(&mut buf, w, h, &mut self.renderer.font, &self.renderer.theme, rect, &self.advisor, "");
        }

        if let (Some(b), Some(l)) = (&mut self.browser, blayout) {
            let mut code_font = crate::renderer::font::FontManager::new(&self.font_path, self.font_px * 0.8);
            page::draw_docs_page(&mut buf, w, h, &l, &mut code_font, &self.renderer.theme);
            let info = PageInfo { url: &b.url, title: &b.title, loading: false, can_back: true, can_forward: false };
            crate::network::browser::chrome::render_page(&mut buf, w, h, &l, &mut b.ui, &info, false, &mut self.renderer.font, &self.renderer.theme);
        }

        if let Some(p) = &self.palette {
            p.render(&mut buf, w, h, &mut self.renderer.font, &self.renderer.theme);
        }
        if let Some(e) = &self.exec_preview {
            e.render(&mut buf, w, h, &mut self.renderer.font, &self.renderer.theme);
        }
        buf
    }
}

impl Stage {
    /// Final frame. With `--features gpu` and an active effect, the effect
    /// runs through the real wgpu shader offscreen (curvature, chromatic
    /// aberration, ...), exactly like the app's GPU path. Without the
    /// feature, or when no adapter works, the CPU fallback inside
    /// `render_tabbed_with_cmd` produces the effect.
    pub fn render_final(&mut self) -> Vec<u32> {
        #[cfg(feature = "gpu")]
        if self.renderer.shader.kind().is_some() {
            self.renderer.shader.set_gpu_active(true); // CPU path stays out of the way
            let frame = self.render();
            if let Some(fx) = self.renderer.active_effect() {
                match crate::renderer::gpu::render_offscreen(&frame, self.w as u32, self.h as u32, Some(fx), 1.0) {
                    Ok(px) => return px,
                    Err(e) => log::warn!("GPU effect unavailable ({e}); using the CPU fallback"),
                }
            }
            self.renderer.shader.set_gpu_active(false);
            self.renderer.invalidate();
        }
        self.render()
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Content
// ─────────────────────────────────────────────────────────────────────────

const HOME: &str = "/Users/maya/dev";

fn aurora() -> Prompt {
    Prompt::new("aurora", &format!("{HOME}/aurora")).git("main", "!2 ?1")
}

const LS_OUT: &str = "{bblue}build{0}  Cargo.lock  Cargo.toml  {bblue}docs{0}  {bblue}migrations{0}  README.md  {bblue}src{0}  {bblue}target{0}  {bblue}tests{0}";

const GIT_STATUS: &str = "## {bgreen}main{0}...{red}origin/main{0} [ahead 2]
{red} M{0} src/handlers/auth.rs
{red} M{0} src/router.rs
{red}??{0} src/middleware/rate_limit.rs";

const NPM_INSTALL: &str = "
added 1247 packages, and audited 1248 packages in 9s

214 packages are looking for funding
  run `npm fund` for details

found {bgreen}0{0} vulnerabilities";

const CARGO_TEST_Q: &str = "
running 14 tests
{bgreen}..............{0}
test result: {bgreen}ok{0}. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.21s
";

const CARGO_BUILD_ERR: &str = "{bgreen}   Compiling{0} aurora v0.4.2 (/Users/maya/dev/aurora)
{bred}error[E0432]{0}{bold}: unresolved import `governor`{0}
{bblue} --> {0}src/middleware/rate_limit.rs:3:5
{bblue}  |{0}
{bblue}3 |{0} use governor::{Quota, RateLimiter};
{bblue}  |{0}     {bred}^^^^^^^^{0} {bred}use of unresolved module or unlinked crate `governor`{0}
{bblue}  |{0}
{bblue}  = {0}{bold}help{0}: if you wanted to use a crate named `governor`, use `cargo add governor` to add it to your `Cargo.toml`

{bold}For more information about this error, try `rustc --explain E0432`.{0}
{bred}error{0}: could not compile `aurora` (bin \"aurora\") due to 1 previous error";

const CARGO_TEST_FULL: &str = "{bgreen}   Compiling{0} aurora v0.4.2 (/Users/maya/dev/aurora)
{bgreen}    Finished{0} `test` profile [unoptimized + debuginfo] target(s) in 3.82s
{bgreen}     Running{0} unittests src/main.rs (target/debug/deps/aurora-3f2a91c7d0be)

running 14 tests
test config::tests::loads_defaults ... {bgreen}ok{0}
test handlers::auth::tests::rejects_expired_token ... {bgreen}ok{0}
test handlers::auth::tests::refreshes_valid_session ... {bgreen}ok{0}
test handlers::orders::tests::paginates_results ... {bgreen}ok{0}
test router::tests::matches_nested_routes ... {bgreen}ok{0}
test router::tests::trims_trailing_slash ... {bgreen}ok{0}

test result: {bgreen}ok{0}. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.21s";

const CARGO_TEST_SHORT: &str = "{bgreen}   Compiling{0} aurora v0.4.2
{bgreen}    Finished{0} `test` profile in 3.82s
{bgreen}     Running{0} unittests src/main.rs

running 14 tests
test config::tests::loads_defaults ... {bgreen}ok{0}
test handlers::auth::tests::rejects_expired_token ... {bgreen}ok{0}
test router::tests::matches_nested_routes ... {bgreen}ok{0}
{gray}... 11 more{0}
test result: {bgreen}ok{0}. 14 passed; 0 failed; finished in 0.21s";

const GIT_LOG: &str = "{red}*{0} {byellow}3f9a1c2{0} {bcyan}({bgreen}HEAD -> main{bcyan}){0} wip: rate limiting
{red}*{0} {byellow}b81d07e{0} feat(auth): rotate refresh tokens
{red}*{0} {byellow}5ac2e90{0} {bcyan}({bred}origin/main{bcyan}){0} fix: trim trailing slash
{red}*{0} {byellow}91e4b3d{0} chore: bump tokio to 1.41
{red}*{0} {byellow}e02f6a8{0} feat: /healthz endpoint
{red}*{0} {byellow}7c3d5b1{0} refactor: split handlers module
{red}*{0} {byellow}2d6c8f4{0} test: cover refresh rotation
{red}*{0} {byellow}6a1b3c7{0} ci: cache cargo registry
{red}*{0} {byellow}d94e7f0{0} init: aurora skeleton";

const NPM_TEST: &str = " {black}{bgcyan} RUN {0} {bcyan}v2.1.4{0} {gray}~/dev/aurora/web{0}

 {bgreen}\u{2713}{0} src/components/Button.test.tsx {gray}(4 tests){0} {byellow}12ms{0}
 {bgreen}\u{2713}{0} src/hooks/useSession.test.ts {gray}(6 tests){0} {byellow}31ms{0}
 {bgreen}\u{2713}{0} src/lib/format.test.ts {gray}(9 tests){0} {byellow}5ms{0}

 {gray}Test Files{0}  {bgreen}3 passed{0} (3)
      {gray}Tests{0}  {bgreen}19 passed{0} (19)
   {gray}Duration{0}  642ms";

const DIFF_STAT: &str = " src/handlers/auth.rs | 14 {bgreen}++++++++{0}{bred}------{0}
 src/router.rs        |  6 {bgreen}+++++{0}{bred}-{0}
 2 files changed, 13 insertions(+), 7 deletions(-)";

const CURL_429: &str = "{bred}HTTP/1.1 429 Too Many Requests{0}
{gray}Content-Length:{0} 38
{gray}Content-Type:{0} application/json
{gray}Retry-After:{0} 1
{gray}X-RateLimit-Limit:{0} 50
{gray}X-RateLimit-Remaining:{0} 0

{ \"error\": \"rate limit exceeded\" }";

const DOCKER_PS: &str = "CONTAINER ID   IMAGE            STATUS         PORTS
{bcyan}a41f9c02d7be{0}   postgres:16      Up 3 hours     5432/tcp
{bcyan}c9d03e1b88aa{0}   redis:7-alpine   Up 3 hours     6379/tcp";

const LOG_TAIL: &str = "{gray}14:32:01{0} {bgreen}INFO{0}  aurora::server listening on 0.0.0.0:4000
{gray}14:32:04{0} {bgreen}INFO{0}  GET /healthz {bgreen}200{0} 1.2ms
{gray}14:32:09{0} {bgreen}INFO{0}  POST /v1/session/refresh {bgreen}200{0} 8.4ms
{gray}14:32:12{0} {byellow}WARN{0}  rate limit near threshold ip=10.0.4.17
{gray}14:32:15{0} {bgreen}INFO{0}  GET /v1/orders?page=2 {bgreen}200{0} 14.9ms
{gray}14:32:21{0} {bred}ERROR{0} db pool timeout after 5000ms
{gray}14:32:22{0} {bgreen}INFO{0}  GET /v1/orders?page=2 {bgreen}200{0} 11.3ms";

const FIX_CMD: &str = "cargo add governor";
const FIX_EXPL: &str = "adds the missing dependency";
const FIX_EXPL_SHORT: &str = "adds crate";

/// Failed `cargo build` session used by several scenes. `lead` selects how
/// much history precedes it: 0 = tests only, 1 = + `git status`, 2 = + `ls`.
/// Leaves the shell at a fresh prompt (previous exit 101).
fn build_session(lead: u8) -> Script {
    let p = aurora();
    let mut sc = Script::new();
    if lead >= 2 {
        sc.run(&p, "ls", LS_OUT, 0);
    }
    if lead >= 1 {
        sc.run(&p, "git status -sb", GIT_STATUS, 0);
    }
    sc.run(&p, "cargo test -q", CARGO_TEST_Q.trim_start_matches('\n').trim_end_matches('\n'), 0);
    sc.run(&p, "cargo build", CARGO_BUILD_ERR, 101);
    sc.prompt(&p.exit(101));
    sc
}

fn failing_snap(s: &mut Stage, idx: usize, block: usize) -> BlockSnap {
    let t = &s.pane(idx).terminal;
    let b = &t.blocks.blocks()[block];
    BlockSnap {
        command: b.command.clone(),
        exit_code: b.exit_code,
        output: paint(CARGO_BUILD_ERR).replace('\x1b', "").replace("[0m", ""),
        cwd: t.cwd.clone(),
        running: false,
        line: b.command_line,
    }
}

fn set_fix_bar(s: &mut Stage, idx: usize, block: usize, expl: &str) {
    let snap = failing_snap(s, idx, block);
    let pane = s.pane(idx);
    let sig = BlockSig::of(pane.id, &pane.terminal);
    s.inline_ai.fix.current = Some(ActiveFix {
        suggestion: FixSuggestion { command: FIX_CMD.into(), explanation: expl.into() },
        dangerous: false,
        sig,
        snap,
    });
}

// Chat conversation about the failed build.
const CHAT_ANSWER: &str = "`src/middleware/rate_limit.rs` imports **governor**, but the crate is missing from your `Cargo.toml`. Add it:

```sh
cargo add governor
```

Then re-run `cargo build`. `Quota` and `RateLimiter` are exported by that crate, so the import resolves once it is added.";

fn fill_chat(s: &mut Stage, streaming: bool) {
    let chat = &mut s.chat;
    chat.visible = true;
    chat.focused = true;
    chat.model = "deepseek-chat".into();
    let req = AskRequest::new("Why did this fail?", Intent::Fix).with(ContextItem::Block {
        command: "cargo build".into(),
        exit_code: Some(101),
        output: String::new(),
        cwd: None,
        running: false,
    });
    chat.session.push_user(&req);
    // While streaming, the answer stops mid-sentence at the cursor.
    let text = if streaming { &CHAT_ANSWER[..CHAT_ANSWER.find("exported by that crate, so the").map_or(CHAT_ANSWER.len(), |i| i + "exported by that crate, so the".len())] } else { CHAT_ANSWER };
    let mut m = Message::assistant(text);
    m.refresh_actions();
    chat.session.messages.push(m);
    s.advisor.enabled = true;
    chat.advised = Some(FIX_CMD.into());
    s.advisor.review = Some(AdvisorReview {
        safe: true,
        notes: vec!["Only edits Cargo.toml and Cargo.lock".into()],
        suggestion: None,
        risk_level: RiskLevel::Safe,
    });
    if streaming {
        chat.show_streaming();
    }
    chat.started = Instant::now();
}

// ─────────────────────────────────────────────────────────────────────────
// Scenes
// ─────────────────────────────────────────────────────────────────────────

fn std_tabs(s: &mut Stage) {
    s.window_title = "aurora \u{2014} zsh".into();
    s.set_tabs(&["aurora", "web", "infra"]);
}

fn blocks(s: &mut Stage) {
    std_tabs(s);
    s.layout();
    let p = aurora();
    let mut sc = Script::new();
    sc.run(&p, "ls", LS_OUT, 0);
    sc.run(&p, "git status -sb", GIT_STATUS, 0);
    sc.run(&p, "npm install", NPM_INSTALL, 0);
    sc.run(&p, "cargo test -q", CARGO_TEST_Q.trim_start_matches('\n').trim_end_matches('\n'), 0);
    sc.run(&p, "cargo build", CARGO_BUILD_ERR, 101);
    sc.prompt(&p.exit(101));
    s.feed(0, &sc);
    s.retime(0, &[12, 58, 9_400, 4_200, 1_800]);
    s.pane(0).terminal.blocks.toggle_collapse(2);
    s.blocks_ui.hover = Some(Hover { tab: 0, pane: 0, block: 3, button: Some(Button::AskAi) });
}

fn fix_suggestion(s: &mut Stage) {
    std_tabs(s);
    s.layout();
    s.feed(0, &build_session(2));
    s.pad_below(0, 2);
    s.retime(0, &[12, 58, 4_200, 1_800]);
    set_fix_bar(s, 0, 3, FIX_EXPL);
}

fn nl_command(s: &mut Stage) {
    std_tabs(s);
    s.layout();
    let p = aurora();
    let mut sc = Script::new();
    sc.run(&p, "ls", LS_OUT, 0);
    sc.run(&p, "git status -sb", GIT_STATUS, 0);
    sc.run(&p, "docker ps", DOCKER_PS, 0);
    sc.run(&p, "git log --oneline -4", &GIT_LOG.lines().take(4).collect::<Vec<_>>().join("\n"), 0);
    sc.run(&p, "cargo test -q", CARGO_TEST_Q.trim_end_matches('\n'), 0);
    sc.prompt(&p).typed("du -ah . | sort -rh | head -n 10");
    s.feed(0, &sc);
    s.pad_below(0, 2);
    s.retime(0, &[12, 58, 120, 41, 4_200]);
    let id = s.pane(0).id;
    s.inline_ai.nl = NlState::Ghost(NlGhost {
        query: "find the 10 largest files".into(),
        command: "du -ah . | sort -rh | head -n 10".into(),
        dangerous: false,
        pane_id: id,
    });
}

fn cmdk(s: &mut Stage) {
    std_tabs(s);
    s.layout();
    s.feed(0, &build_session(2));
    s.retime(0, &[12, 58, 4_200, 1_800]);
    let snap = failing_snap(s, 0, 3);
    s.blocks_ui.selected = Some((0, 0, 3));
    s.inline_ai.popover = Some(Popover {
        edit: LineEdit::with_text("why did this fail?"),
        resolved: Resolved::Block { snap, source: BlockSource::Selected },
        mode: PopoverMode::Ask,
    });
}

fn ai_chat(s: &mut Stage) {
    std_tabs(s);
    s.chat.ratio = 0.38;
    fill_chat(s, true);
    s.layout();
    s.feed(0, &build_session(1));
    s.retime(0, &[58, 4_200, 1_800]);
}

/// Lay out `tab` as a 2x2 grid; pane order is [top-left, bottom-left,
/// top-right, bottom-right]. Left column width `ratio`.
fn grid_2x2(s: &mut Stage, ratio: f32) {
    let t = s.wm.active_tab_mut();
    let (cols, rows) = {
        let p = t.active_pane();
        (p.terminal.cols, p.terminal.rows)
    };
    t.split(SplitDir::Horizontal, Pane::scripted(101, cols / 2, rows));
    t.focus_pane(0);
    t.split(SplitDir::Vertical, Pane::scripted(102, cols / 2, rows / 2));
    t.focus_pane(2);
    t.split(SplitDir::Vertical, Pane::scripted(103, cols / 2, rows / 2));
    if let PaneNode::Split { ratio: r, .. } = &mut t.root {
        *r = ratio;
    }
    t.focus_pane(0);
}

fn hero(s: &mut Stage) {
    s.window_title = "aurora \u{2014} rift".into();
    s.set_tabs(&["aurora", "web", "infra"]);
    s.chat.ratio = 0.38;
    fill_chat(s, false);
    // Two stacked panes: the failing build on top, the web tests below.
    {
        let t = s.wm.active_tab_mut();
        let (cols, rows) = {
            let p = t.active_pane();
            (p.terminal.cols, p.terminal.rows)
        };
        t.split(SplitDir::Vertical, Pane::scripted(1, cols, rows / 2));
        if let PaneNode::Split { ratio, .. } = &mut t.root {
            *ratio = 0.68;
        }
        t.focus_pane(0);
    }
    s.layout();
    let p = aurora();
    let mut sc = Script::new();
    sc.run(&p, "git log --oneline -1", &GIT_LOG.lines().next().unwrap_or(""), 0);
    sc.run(&p, "cargo build", CARGO_BUILD_ERR, 101);
    sc.prompt(&p.exit(101));
    s.feed(0, &sc);
    s.retime(0, &[41, 1_800]);
    s.pad_below(0, 3);
    set_fix_bar(s, 0, 1, FIX_EXPL_SHORT);

    let mut sc = Script::new();
    let web = Prompt::new("web", &format!("{HOME}/aurora/web")).git("main", "");
    sc.run(&web, "npm test", NPM_TEST, 0);
    sc.prompt(&web);
    s.feed(1, &sc);
    s.retime(1, &[642]);
}

fn splits(s: &mut Stage) {
    s.window_title = "aurora \u{2014} rift".into();
    s.set_tabs(&["aurora", "web", "infra"]);
    grid_2x2(s, 0.5);
    s.layout();
    let p = aurora();
    let mut sc = Script::new();
    sc.run(&p, "cargo test", CARGO_TEST_SHORT, 0);
    sc.prompt(&p);
    s.feed(0, &sc);
    s.retime(0, &[3_900]);

    let infra = Prompt::new("infra", &format!("{HOME}/aurora/infra"));
    let mut sc = Script::new();
    sc.run(&infra, "docker logs --tail 7 aurora", LOG_TAIL, 0);
    sc.prompt(&infra);
    s.feed(1, &sc);
    s.retime(1, &[310]);

    let mut sc = Script::new();
    sc.run(&p, "git log --oneline --graph", GIT_LOG, 0);
    sc.prompt(&p);
    s.feed(2, &sc);
    s.retime(2, &[41]);

    let web = Prompt::new("web", &format!("{HOME}/aurora/web")).git("main", "");
    let mut sc = Script::new();
    sc.run(&web, "npm test", NPM_TEST, 0);
    sc.prompt(&web);
    s.feed(3, &sc);
    s.retime(3, &[642]);
    s.wm.active_tab_mut().focus_pane(0);
}

fn palette(s: &mut Stage) {
    std_tabs(s);
    s.layout();
    s.feed(0, &build_session(2));
    s.retime(0, &[12, 58, 4_200, 1_800]);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut hist = History::default();
    for id in ["Split Right", "Toggle HUD", "Theme: Tokyo Night", "Split Right", "New Tab"] {
        hist.record(id, now);
    }
    let mut pal = CommandPalette::with_parts(hist, None);
    pal.open(PaletteContext {
        tabs: vec!["aurora".into(), "web".into(), "infra".into()],
        active_tab: 0,
        ssh_hosts: vec![
            SshHostInfo { alias: "prod-api".into(), detail: "deploy@10.0.4.17:22".into() },
            SshHostInfo { alias: "staging".into(), detail: "maya@stg.aurora.dev:22".into() },
        ],
        font_size: 12.0,
        ..Default::default()
    });
    for c in "nt".chars() {
        pal.handle_key(PaletteKey::Char(c));
    }
    s.palette = Some(pal);
}

fn browser(s: &mut Stage) {
    s.window_title = "aurora \u{2014} rift".into();
    s.set_tabs(&["aurora", "web", "infra"]);
    let mut ui = BrowserUi::new();
    ui.ratio = 0.5;
    s.browser = Some(BrowserScene {
        ui,
        url: "http://localhost:4000/docs/rate-limiting".into(),
        title: "Rate limiting \u{00b7} Aurora docs".into(),
    });
    s.layout();
    let p = aurora();
    let mut sc = Script::new();
    sc.run(&p, "git status -sb", GIT_STATUS, 0);
    sc.run(&p, "cargo test -q", CARGO_TEST_Q.trim_start_matches('\n').trim_end_matches('\n'), 0);
    sc.run(&p, "cargo build --release", "{bgreen}    Finished{0} `release` profile [optimized] in 41.2s", 0);
    sc.run(&p, "git diff --stat", DIFF_STAT, 0);
    sc.run(&p, "http :4000/v1/orders", CURL_429, 0);
    sc.prompt(&p);
    s.feed(0, &sc);
    s.retime(0, &[58, 4_200, 41_200, 96, 214]);
}

fn preview_accept(s: &mut Stage) {
    std_tabs(s);
    s.layout();
    let p = aurora();
    let mut sc = Script::new();
    sc.run(&p, "ls", LS_OUT, 0);
    sc.run(&p, "git status -sb", GIT_STATUS, 0);
    sc.run(&p, "cargo --version", "cargo 1.82.0 (8f40fc59f 2024-08-21)", 0);
    // Piping a download into a shell is a genuinely critical command (a
    // routine `rm -rf ./build` is only Info and shows no preview).
    let cmd = "curl -fsSL https://get.example.dev/install.sh | sh";
    sc.prompt(&p).typed(cmd);
    s.feed(0, &sc);
    s.retime(0, &[12, 58, 240]);

    // Real classification of the real command (nothing is executed).
    let mut prev = ExecPreview::check_command_in(cmd, Some(&format!("{HOME}/aurora")));
    if let Some(p) = prev.as_mut() {
        p.visible = true;
        assert!(p.severity == crate::tools::exec_preview::Severity::Critical, "scene must show a critical preview");
    }
    s.exec_preview = prev;
}

fn effects_crt(s: &mut Stage) {
    std_tabs(s);
    s.renderer.shader.set_effect(Some(crate::effects::EffectKind::Crt));
    // The GPU shader (curvature, aberration) reads well at the default
    // strength; the CPU fallback is only scanlines + vignette, so push it.
    s.renderer.shader.set_intensity(if cfg!(feature = "gpu") { crate::effects::DEFAULT_INTENSITY } else { 1.0 });
    s.layout();
    let p = aurora();
    let mut sc = Script::new();
    sc.run(&p, "ls", LS_OUT, 0);
    sc.run(&p, "git log --oneline --graph", GIT_LOG, 0);
    sc.run(&p, "cargo test", CARGO_TEST_FULL, 0);
    sc.prompt(&p);
    s.feed(0, &sc);
    s.retime(0, &[12, 41, 3_900]);
}

fn hud(s: &mut Stage) {
    std_tabs(s);
    let data = HudData {
        user: "maya".into(),
        host: "mbp".into(),
        shell: "zsh".into(),
        uptime: "3d 4h 12m".into(),
        time: "14:32:07".into(),
        mem_pct: 66.0,
        mem_label: "10.6/16G 66%".into(),
        cpu_pct: 41.0,
        cpu_label: "41%".into(),
        os: "macos".into(),
        arch: "aarch64".into(),
        rust_version: format!("rift v{}", crate::config::VERSION),
        term: "xterm-256color".into(),
        git_branch: "main".into(),
        cwd_short: ".../dev/aurora".into(),
        pid: "48213".into(),
        disk_label: "412G/926G 45%".into(),
        disk_pct: 45.0,
        load_avg: "2.31 1.98 1.77".into(),
    };
    // Plausible history: CPU wanders with a build spike; memory climbs with
    // sawtooth drops where the OS reclaims cache.
    let mut cpu = Vec::new();
    let mut mem = Vec::new();
    let mut seed = 7u32;
    let mut rnd = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 16) as f32 / 65_535.0
    };
    for i in 0..40 {
        let base = 22.0 + 9.0 * (i as f32 * 0.35).sin();
        let spike = if (22..31).contains(&i) { 42.0 * (1.0 - ((i as f32 - 26.0).abs() / 5.0)).max(0.0) } else { 0.0 };
        cpu.push((base + spike + rnd() * 8.0).clamp(4.0, 96.0));
        let saw = (i % 14) as f32 * 2.1;
        mem.push(44.0 + saw + i as f32 * 0.35 + rnd() * 1.8);
    }
    cpu[39] = 41.0;
    mem[39] = 66.0;
    s.hud = Some(Hud::with_data(data, &cpu, &mem));
    s.layout();
    s.feed(0, &build_session(2));
    s.retime(0, &[12, 58, 4_200, 1_800]);
}

// ─────────────────────────────────────────────────────────────────────────
// Agents dock: two Claude-Code-like (ink) panes, dock opened after the
// split was already on screen.
// ─────────────────────────────────────────────────────────────────────────

/// Pad `s` (visible chars) to `n` columns.
fn pad_to(s: &str, n: usize) -> String {
    let mut o: String = s.chars().take(n).collect();
    let len = o.chars().count();
    o.push_str(&" ".repeat(n.saturating_sub(len)));
    o
}

/// The live (repainted) part of a Claude Code screen: spinner / status line,
/// prompt box and the footer with a right-aligned segment placed by CHA.
pub(crate) fn claude_frame(cols: usize, working: bool, tick: usize, right: &str) -> Vec<String> {
    let inner = cols.saturating_sub(2);
    let status = if working {
        paint(&format!("{{byellow}}\u{273b} Meandering\u{2026}{{0}} {{dim}}({tick}s \u{b7} \u{2193} {} tokens \u{b7} esc to interrupt){{0}}", 300 + tick * 7))
    } else {
        paint("{dim}\u{273b} Cooked for 3s{0}")
    };
    let top = format!("\u{256d}{}\u{256e}", "\u{2500}".repeat(inner));
    let mid = format!("\u{2502}{}\u{2502}", pad_to(" > ", inner));
    let bot = format!("\u{2570}{}\u{256f}", "\u{2500}".repeat(inner));
    let left = "  -- INSERT --";
    let col = cols.saturating_sub(right.chars().count() + 1);
    let footer = format!("{}\x1b[{}G{}", paint(&format!("{{dim}}{left}{{0}}")), col.max(left.len() + 2) + 1, paint(&format!("{{dim}}{right}{{0}}")));
    vec![status, String::new(), top, mid, bot, footer]
}

/// ink's log-update: sync begin, cursor up over the previous frame, erase
/// below, draw, sync end.
pub(crate) fn ink_repaint(prev_lines: usize, frame: &[String]) -> Vec<u8> {
    let mut s = String::from("\x1b[?2026h");
    if prev_lines > 0 {
        s.push_str(&format!("\x1b[{prev_lines}A\x1b[G\x1b[J"));
    }
    s.push_str(&frame.join("\r\n"));
    s.push_str("\r\n\x1b[?2026l");
    s.into_bytes()
}

fn claude_history(question: &str, answer: &[&str], tools: bool) -> Script {
    let mut sc = Script::new();
    let inner = 38;
    let rule = "\u{2500}".repeat(inner);
    sc.out(&format!("{{byellow}}\u{256d}{rule}\u{256e}{{0}}"));
    sc.out(&format!("{{byellow}}\u{2502}{{0}}{}{{byellow}}\u{2502}{{0}}", pad_to(" \u{273b} Welcome to Claude Code!", inner)));
    sc.out(&format!("{{byellow}}\u{2502}{{0}}{{dim}}{}{{0}}{{byellow}}\u{2502}{{0}}", pad_to("   cwd: /Users/maya/dev/aurora", inner)));
    sc.out(&format!("{{byellow}}\u{2570}{rule}\u{256f}{{0}}"));
    sc.out("");
    sc.out(&format!("{{bold}}>{{0}} {question}"));
    sc.out("");
    for a in answer {
        sc.out(&format!("{{bwhite}}\u{25cf}{{0}} {a}"));
        sc.out("");
    }
    if tools {
        sc.out("{bgreen}\u{25cf}{0} {bold}Read{0}(src/middleware/rate_limit.rs)");
        sc.out("  {dim}\u{23bf}  Read 84 lines{0}");
        sc.out("");
    }
    sc
}

fn mission_control(s: &mut Stage) {
    super::mission::build(s);
}

fn agents_dock(s: &mut Stage) {
    s.window_title = "aurora \u{2014} rift".into();
    s.set_tabs(&["aurora"]);
    s.chat.ratio = 0.38;
    // Two side-by-side panes (ids 0 and 101).
    {
        let t = s.wm.active_tab_mut();
        let (cols, rows) = {
            let p = t.active_pane();
            (p.terminal.cols, p.terminal.rows)
        };
        t.split(SplitDir::Horizontal, Pane::scripted(101, cols / 2, rows));
        t.focus_pane(0);
    }
    s.layout();

    // Both agents are running; the dock is still closed.
    let right_a = "Image in clipboard \u{b7} ctrl+v to paste";
    let right_b = "57929 tokens";
    let (cols_a, cols_b) = {
        let t = s.wm.active_tab();
        (t.pane(0).map_or(40, |p| p.terminal.cols), t.pane(1).map_or(40, |p| p.terminal.cols))
    };
    s.feed(0, &claude_history("refactor the rate limiter to use governor", &["I'll start by reading the current middleware."], true));
    s.pane(0).feed(&ink_repaint(0, &claude_frame(cols_a, true, 1, right_a)));
    s.pane(1).feed(&claude_history("who are you", &["I'm Claude, an AI assistant made by Anthropic, running in your terminal. I can read and edit code, run commands, and help you ship."], false).bytes);
    s.pane(1).feed(&ink_repaint(0, &claude_frame(cols_b, false, 0, right_b)));
    let _ = s.render(); // frame 1: damage tracker now holds the dock-less layout

    // Open Mission Control (Cmd+Shift+;): content area shrinks, panes reflow,
    // and both apps repaint for the new width (SIGWINCH).
    s.agents_ui.visible = true;
    s.agents_ui.selected = None;
    s.layout();
    let (cols_a, cols_b) = {
        let t = s.wm.active_tab();
        (t.pane(0).map_or(40, |p| p.terminal.cols), t.pane(1).map_or(40, |p| p.terminal.cols))
    };
    s.pane(0).feed(&ink_repaint(6, &claude_frame(cols_a, true, 2, right_a)));
    s.pane(1).feed(&ink_repaint(6, &claude_frame(cols_b, false, 0, right_b)));
    let _ = s.render();

    // Drag the divider a bit, as in the report.
    if let PaneNode::Split { ratio, .. } = &mut s.wm.active_tab_mut().root {
        *ratio = 0.46;
    }
    s.layout();
    let (cols_a, cols_b) = {
        let t = s.wm.active_tab();
        (t.pane(0).map_or(40, |p| p.terminal.cols), t.pane(1).map_or(40, |p| p.terminal.cols))
    };
    s.pane(0).feed(&ink_repaint(6, &claude_frame(cols_a, true, 3, right_a)));
    s.pane(1).feed(&ink_repaint(6, &claude_frame(cols_b, false, 0, right_b)));
    s.observe_agents(Instant::now());
    s.wm.active_tab_mut().focus_pane(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(w: usize, h: usize) -> Option<Stage> {
        let font = super::super::font_path();
        if !std::path::Path::new(&font).exists() {
            return None; // no monospace font on this machine
        }
        Some(Stage::new(w, h, &font, 18.0, crate::config::Theme::rift_neon()))
    }

    fn max_diff(a: u32, b: u32) -> u32 {
        (0..3).map(|i| (((a >> (8 * i)) & 0xff) as i32 - ((b >> (8 * i)) & 0xff) as i32).unsigned_abs()).max().unwrap()
    }

    /// Hovering / selecting a command block only tints it and shows the
    /// toolbar; the text stays, and leaving restores the exact frame
    /// (nothing is persisted into the damage-tracked back buffer).
    #[test]
    fn block_hover_and_selection_are_translucent_and_reversible() {
        for effect in [None, Some(crate::effects::EffectKind::Matrix), Some(crate::effects::EffectKind::Crt)] {
            let Some(mut s) = stage(1000, 640) else { return };
            s.renderer.shader.set_effect(effect);
            blocks(&mut s);
            let ch = s.cell().1;
            s.blocks_ui.hover = None;
            let base = s.render();
            for sel in [false, true] {
                s.blocks_ui.hover = Some(Hover { tab: 0, pane: 0, block: 3, button: None });
                s.blocks_ui.selected = sel.then_some((0, 0, 3));
                let hov = s.render();
                // Only the toolbar (top-right of the block, one text row) may change a lot.
                let mut strong = Vec::new();
                let mut changed = 0usize;
                for (i, (a, b)) in base.iter().zip(&hov).enumerate() {
                    let d = max_diff(*a, *b);
                    if d > 0 {
                        changed += 1;
                    }
                    let limit = if sel { 40 } else { 14 };
                    if d > limit {
                        strong.push((i % s.w, i / s.w));
                    }
                }
                assert!(changed > 0, "hover must be visible (effect {effect:?})");
                if let (Some(y0), Some(y1)) = (strong.iter().map(|p| p.1).min(), strong.iter().map(|p| p.1).max()) {
                    // The gutter bar (left edge) and the toolbar (top row) are opaque; nothing else.
                    let gutter = |x: usize| x < 8;
                    let rest: Vec<_> = strong.iter().filter(|p| !gutter(p.0)).collect();
                    if let (Some(a), Some(b)) = (rest.iter().map(|p| p.1).min(), rest.iter().map(|p| p.1).max()) {
                        assert!(b - a <= ch + 2, "opaque pixels outside the toolbar row: rows {a}..{b} (sel {sel}, effect {effect:?}, y {y0}..{y1})");
                        assert!(rest.iter().all(|p| p.0 > s.w / 3), "text hidden left of the toolbar (sel {sel}, effect {effect:?})");
                    }
                }
            }
            // Hover ends: exactly the original frame again.
            s.blocks_ui.hover = None;
            s.blocks_ui.selected = None;
            let after = s.render();
            assert!(base == after, "frame not restored after hover ended (effect {effect:?})");
        }
    }

    /// The Mission Control dock changes the content area: the damage-tracked
    /// frame after opening it must equal a fresh render of the same state.
    #[test]
    fn opening_the_agents_dock_repaints_everything() {
        let Some(mut tracked) = stage(1200, 700) else { return };
        agents_dock(&mut tracked);
        let a = tracked.render();
        let Some(mut fresh) = stage(1200, 700) else { return };
        agents_dock(&mut fresh);
        fresh.renderer.invalidate_all();
        let b = fresh.render();
        // The dock itself animates with wall-clock time; the panes must be identical.
        let area = tracked.content_area();
        assert!(area.x > 0 && area.x + area.width <= tracked.w, "{area:?}");
        let w = tracked.w;
        let bad = (area.y..area.y + area.height)
            .flat_map(|y| (area.x..area.x + area.width).map(move |x| (x, y)))
            .find(|&(x, y)| a[y * w + x] != b[y * w + x]);
        assert!(bad.is_none(), "pane area differs between tracked and fresh renders at {bad:?}");
        // The dock strip belongs to the dock alone: no pane pixels left of the content area.
        let dock = tracked.agents_dock().expect("dock open");
        assert_eq!(dock.w, area.x);
    }
}
