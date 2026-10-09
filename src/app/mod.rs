mod ime;
pub mod keymap;
pub(crate) mod lifecycle;
pub mod mouse;
pub(crate) mod overlays;
pub mod panes;
pub mod shortcuts;
mod tabs;
pub mod windows;
pub mod window_ops;

use std::collections::HashMap;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::ai::chat::ChatUi;
use crate::ai::{Advisor, Autocomplete, LlmManager};
use crate::ai::observer::Observer;
use crate::config::Config;
use crate::network::{SshConnectRequest, SshDialog, SshPty, WebViewDialog, WebViewPane};
use crate::renderer::Renderer;
use crate::tools::audit::AuditLog;
use crate::tools::blocks::BlockManager;
use crate::tools::cicd::CicdPanel;
use crate::tools::command_palette::CommandPalette;
use crate::tools::compare::CompareView;
use crate::tools::docker_panel::DockerPanel;
use crate::tools::error_detect::{ErrorDetector, ErrorNotification};
use crate::tools::exec_preview::ExecPreview;
use crate::tools::file_manager::FileManager;
use crate::tools::git_panel::GitPanel;
use crate::tools::heatmap::Heatmap;
use crate::tools::history::HistorySearch;
use crate::tools::hud::Hud;
use crate::tools::network_monitor::NetworkMonitor;
use crate::tools::notify::Notifier;
use crate::tools::port_dashboard::PortDashboard;
use crate::tools::process_tree::ProcessTree;
use crate::tools::recording::Recorder;
use crate::tools::regex_playground::RegexPlayground;
use crate::tools::search::SearchOverlay;
use crate::tools::system_info::SystemInfo;
use crate::tools::secret_mask::SecretMasker;
use crate::tools::teaching::TeachingMode;
use crate::tools::timewarp::{TimeWarp, TimeWarpBrowser};
use crate::ui::{AppMenuBar, Preferences, Welcome};
use crate::terminal::MouseMode;
use crate::window::selection::{self, Selection};
use crate::window::WindowManager;
use windows::{WinId, WindowRegistry};

/// Everything that belongs to ONE window: the OS window and its surfaces,
/// the renderer, the tabs/panes ([`WindowManager`]), input state (pointer,
/// modifiers, IME, selection), and every overlay / dock / tool panel with its
/// visibility and layout.
///
/// `App` holds exactly one `WindowState` in `App::win` (the *active* window:
/// the one the code currently running is acting on) and parks the others in
/// `App::parked`. See [`App::activate`] for the swap protocol.
pub struct WindowState {
    /// Stable id (see `windows::WinId`); also the namespace of this window's pane ids.
    pub id: WinId,
    pub wm: WindowManager,
    pub renderer: Renderer,
    pub modifiers: ModifiersState,
    pub recorder: Option<Recorder>,
    pub prefs: Preferences,
    pub welcome: Welcome,
    pub ssh_dialog: SshDialog,
    pub autocomplete: Autocomplete,
    pub webview_dialog: WebViewDialog,
    /// The embedded browser is a child view of THIS window's OS window.
    pub webview: Option<WebViewPane>,
    pub webview_tab: Option<usize>,
    pub webview_pane: Option<usize>,
    pub webview_maximized: bool,
    /// Docked AI chat sidebar (see `ai::chat`).
    pub chat: ChatUi,
    pub advisor: Advisor,
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub ssh_connecting: Option<SshConnecting>,
    /// Confirmation modal queue: cloud-AI consent, multi-line paste, SSH host
    /// keys, closing a window with running processes.
    pub confirm: crate::ui::confirm::ConfirmModal,
    pub timewarp: TimeWarp,
    pub timewarp_browser: TimeWarpBrowser,
    pub hud: Hud,
    pub hud_visible: bool,
    pub broadcast: bool,
    pub compare_view: CompareView,
    pub search: SearchOverlay,
    pub command_palette: CommandPalette,
    pub selection: Selection,
    /// Scrollbar, context menu, tab-bar and click state (see `app::mouse`).
    pub mui: mouse::MouseUi,
    pub mouse_pressed: bool,
    pub window_focused: bool,
    pub hover_pane: Option<usize>,
    /// (pane, view row, col) under the pointer when it sits on an OSC 8 link.
    pub link_hover: Option<(usize, usize, usize)>,
    pub dragging_border: Option<usize>,
    /// Divider currently under the mouse (pre-order index), for highlight.
    pub hover_border: Option<usize>,
    /// Time + divider of the last divider click (double-click => equalize).
    pub last_border_click: Option<(std::time::Instant, usize)>,
    pub browser: crate::network::browser::BrowserUi,
    /// IME composition (preedit) text currently being edited; empty when idle.
    pub ime_preedit: String,
    /// Last IME cursor area sent to the OS (x, y, w, h in physical px).
    pub ime_area: Option<(i32, i32, u32, u32)>,

    pub blocks: BlockManager,
    /// Warp-style block chrome state (hover, selection, toast).
    pub blocks_ui: crate::blocks_ui::BlocksUi,
    /// Ambient AI: Cmd+K popover, proactive fixes, `#` natural language.
    pub inline_ai: crate::ai::inline::InlineAi,
    pub error_detector: ErrorDetector,
    pub error_notif: ErrorNotification,
    pub exec_preview: ExecPreview,
    pub file_manager: FileManager,
    pub git_panel: GitPanel,
    pub cicd: CicdPanel,
    pub teaching: TeachingMode,
    pub heatmap: Heatmap,
    pub docker: DockerPanel,
    pub network_monitor: NetworkMonitor,
    pub process_tree: ProcessTree,
    pub system_info: SystemInfo,
    pub port_dashboard: PortDashboard,
    pub regex_playground: RegexPlayground,
    pub history: HistorySearch,
    pub observer_summary: Option<String>,
    /// Mission Control dock of this window (selection, composer, policy log).
    pub agents_ui: crate::agents::ui::AgentsUi,

    pub needs_render: bool,
    /// Redraw pacing (PTY-burst coalescing, cursor blink) of this window.
    pub sched: lifecycle::Sched,

    // Drop order matters for the presentation objects: GPU pipeline and
    // softbuffer surface go before the context and the window.
    #[cfg(feature = "gpu")]
    pub gpu_pipeline: Option<crate::renderer::gpu::GpuPipeline>,
    pub surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    pub context: Option<softbuffer::Context<Arc<Window>>>,
    pub window: Option<Arc<Window>>,
}

impl WindowState {
    /// Fresh state for a window: no overlays open, no OS window yet.
    /// `first` is the app's initial window (the only one that may show the
    /// first-run welcome guide).
    pub fn new(id: WinId, config: &Config, renderer: Renderer, wm: WindowManager, first: bool) -> Self {
        Self {
            id,
            wm,
            renderer,
            modifiers: ModifiersState::empty(),
            recorder: None,
            prefs: Preferences::new(config),
            welcome: if first { Welcome::new_auto() } else { Welcome::hidden() },
            ssh_dialog: SshDialog::new(),
            autocomplete: Autocomplete::new(),
            webview_dialog: WebViewDialog::new(),
            webview: None,
            webview_tab: None,
            webview_pane: None,
            webview_maximized: false,
            chat: ChatUi::new(),
            advisor: Advisor::new(),
            cursor_x: 0,
            cursor_y: 0,
            ssh_connecting: None,
            confirm: crate::ui::confirm::ConfirmModal::new(),
            timewarp: TimeWarp::new(500),
            timewarp_browser: TimeWarpBrowser::new(),
            hud: Hud::new(),
            hud_visible: false,
            broadcast: false,
            compare_view: CompareView::new(),
            search: SearchOverlay::new(),
            command_palette: CommandPalette::new(),
            selection: Selection::new(),
            mui: mouse::MouseUi::default(),
            mouse_pressed: false,
            window_focused: true,
            hover_pane: None,
            link_hover: None,
            dragging_border: None,
            hover_border: None,
            last_border_click: None,
            browser: crate::network::browser::BrowserUi::new(),
            ime_preedit: String::new(),
            ime_area: None,
            blocks: BlockManager::new(),
            blocks_ui: crate::blocks_ui::BlocksUi::new(),
            inline_ai: crate::ai::inline::InlineAi::new(),
            error_detector: ErrorDetector::new(),
            error_notif: ErrorNotification::new(),
            exec_preview: ExecPreview::hidden(),
            file_manager: FileManager::new(),
            git_panel: GitPanel::new(),
            cicd: CicdPanel::new(),
            teaching: TeachingMode::new(),
            heatmap: Heatmap::new(),
            docker: DockerPanel::new(),
            network_monitor: NetworkMonitor::new(),
            process_tree: ProcessTree::new(),
            system_info: SystemInfo::new(),
            port_dashboard: PortDashboard::new(),
            regex_playground: RegexPlayground::new(),
            history: HistorySearch::new(),
            observer_summary: None,
            agents_ui: Default::default(),
            needs_render: true,
            sched: Default::default(),
            #[cfg(feature = "gpu")]
            gpu_pipeline: None,
            surface: None,
            context: None,
            window: None,
        }
    }
}

/// The application: process-global state plus the set of windows.
///
/// ## Windows
///
/// All windows share the global state below (config, LLM + consent, MCP
/// server, agent registry, workflow engine, review engine, notifier, menu
/// bar ...). Everything per-window lives in [`WindowState`].
///
/// To keep the ~1500 existing `app.<per-window thing>` call sites unchanged
/// (modulo the `win.` hop), the app uses a **swap-in** model:
///
/// * `app.win` is always the state of the *active* window - the window the
///   running code is acting on. `app.win.id` says which one.
/// * The other windows' states sit in `app.parked`.
/// * [`App::activate`] swaps a parked window in. Entry points call it first:
///   `window_event` activates the event's window; `about_to_wait` runs the
///   global work with the *focused* window active and then visits every window
///   in turn for its own tick; menu actions go to the focused window.
/// * Code that must touch panes of other windows (global registries keyed by
///   pane id) does not swap: it uses [`App::wm_of_pane`] & co., which look
///   into `parked` directly.
///
/// Invariant: `windows` (the registry) lists exactly `{win.id} U parked.keys()`.
pub struct App {
    // ── process-global ──
    pub config: Config,
    /// Rift's own keybindings (defaults + `[keybindings]`).
    pub keymap: keymap::Keymap,
    pub llm: LlmManager,
    pub menubar: AppMenuBar,
    /// The cloud-AI consent prompt has been considered this run.
    pub consent_checked: bool,
    /// OSC 52 "blocked" toasts already shown this run: (read, oversized write, write).
    pub osc52_warned: (bool, bool, bool),
    pub secret_mask: SecretMasker,
    pub audit: AuditLog,
    pub notifier: Notifier,
    pub observer: Observer,
    /// Built-in MCP server (socket, approval queue, activity overlay).
    pub mcp: crate::mcp::host::UiState,
    /// Change Review: per-pane checkpoints/turns, overlay, chips (see `crate::review`).
    pub review: crate::review::Review,
    /// Agent Mission Control: supervised AI coding agents (see `crate::agents`).
    pub agents: crate::agents::AgentRegistry,
    pub agents_rt: crate::agents::runtime::Runtime,
    /// Workflows: best-of-N, write & review, fix tests, task queues (see `crate::workflow`).
    pub workflows: crate::workflow::Workflows,
    pub startup_time: std::time::Instant,
    /// Set by any key press / click to cut the startup splash short.
    pub startup_skipped: bool,

    // ── windows ──
    /// The active window (see the type docs).
    pub win: WindowState,
    /// Every other window.
    pub parked: HashMap<WinId, WindowState>,
    /// Stable ids, OS handles and focus order of all windows.
    pub windows: WindowRegistry<WindowId>,
    /// Windows of the restored session that still have to be opened once the
    /// event loop is running (the first window is built in `App::new`).
    pub pending_windows: Vec<crate::tools::session::WindowSession>,
    /// Windows asked to close (stable id, save the session if it was the last):
    /// executed by `window_ops::flush_closes` once the current handler is done.
    pub close_queue: Vec<(WinId, bool)>,
    /// Index (creation order) of the window to focus once the session is restored.
    pub restore_focus: Option<usize>,
    /// Saved position / size of the first window (applied in `on_resumed`).
    pub first_geometry: Option<crate::tools::session::Geometry>,
}

pub struct SshConnecting {
    pub rx: std::sync::mpsc::Receiver<Result<SshPty, String>>,
    pub req: SshConnectRequest,
    /// Unknown-host-key questions from the connection thread.
    pub prompts: std::sync::mpsc::Receiver<crate::network::ssh::session::HostKeyPrompt>,
}

impl App {
    /// No wgpu pipeline is driving the window (CPU renderer / softbuffer present).
    #[allow(dead_code)]
    pub fn gpu_pipeline_absent(&self) -> bool {
        #[cfg(feature = "gpu")]
        {
            self.win.gpu_pipeline.is_none()
        }
        #[cfg(not(feature = "gpu"))]
        {
            true
        }
    }

    pub fn new(config: Config, renderer: Renderer, mut wm: WindowManager) -> Self {
        let menubar = AppMenuBar::new();
        menubar.set_ai_checks(config.ai_auto_fix, config.ai_nl_hash);
        let llm = LlmManager::new(config.llm.clone());

        // Restore the previous session (windows, tabs, titles, working dirs,
        // scrollback). The first saved window goes into the window we are
        // about to create; the others are opened on `resumed`.
        let file = crate::tools::session::load_session_file();
        let restore_focus = file.as_ref().map(|f| f.focused).filter(|f| *f > 0);
        let mut saved = file.map(|f| f.windows).unwrap_or_default();
        let mut pending_windows = Vec::new();
        let mut restored_queues = Vec::new();
        let mut first_geometry = None;
        if !saved.is_empty() {
            let first = saved.remove(0);
            restored_queues = crate::tools::session::restore_window(&mut wm, &first);
            first_geometry = first.geometry;
            pending_windows = saved;
        }

        let keymap = keymap::Keymap::build(&config.input.keybindings);
        let notify_after_secs = config.notify_after_secs;
        let config_mcp = config.mcp.clone();
        let mut agents_registry = crate::agents::AgentRegistry::new();
        agents_registry.configure(&config.agents);

        let mut windows = WindowRegistry::default();
        let first_id = windows.reserve();
        debug_assert_eq!(first_id, 0);
        let win = WindowState::new(first_id, &config, renderer, wm, true);
        Self {
            config,
            keymap,
            llm,
            menubar,
            consent_checked: false,
            osc52_warned: (false, false, false),
            secret_mask: SecretMasker::new(),
            audit: AuditLog::new(),
            notifier: Notifier::new(notify_after_secs),
            observer: Observer::new(),
            mcp: crate::mcp::host::start(&config_mcp),
            review: Default::default(),
            agents: agents_registry,
            agents_rt: Default::default(),
            workflows: {
                let mut w = crate::workflow::Workflows::new();
                w.queues.extend(restored_queues);
                w
            },
            startup_time: std::time::Instant::now(),
            startup_skipped: false,
            win,
            parked: HashMap::new(),
            windows,
            pending_windows,
            close_queue: Vec::new(),
            first_geometry,
            restore_focus,
        }
    }

    pub fn tab_bar_height(&self) -> usize {
        self.win.renderer.cell_height() + 16
    }

    pub fn update_title(&self) {
        if let Some(w) = &self.win.window {
            let rec = if self.win.recorder.is_some() { " [REC]" } else { "" };
            let bcast = if self.win.broadcast { " [BROADCAST]" } else { "" };
            let tab_count = self.win.wm.tab_count();
            let tab_idx = self.win.wm.active_tab + 1;
            let pane_title = self.win.wm.active_tab().display_title();
            if tab_count > 1 {
                w.set_title(&format!("rift{rec}{bcast} [{tab_idx}/{tab_count}] {pane_title}"));
            } else {
                w.set_title(&format!("rift{rec}{bcast} — {pane_title}"));
            }
        }
    }

    pub fn toggle_recording(&mut self) {
        if self.win.recorder.is_some() {
            let rec = self.win.recorder.take().unwrap();
            rec.finish();
            log::info!("Recording stopped");
        } else {
            let pane = self.win.wm.active_pane();
            let now = chrono_timestamp();
            let path = std::path::PathBuf::from(format!("recordings/rift-{now}.cast"));
            match Recorder::start(&path, pane.terminal.cols, pane.terminal.rows) {
                Ok(rec) => {
                    log::info!("Recording started: {}", path.display());
                    self.win.recorder = Some(rec);
                }
                Err(e) => log::error!("Failed to start recording: {e}"),
            }
        }
        self.update_title();
    }

    /// Redraw the ACTIVE window.
    pub fn request_redraw(&mut self) {
        self.win.needs_render = true;
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Rect occupied by the active tab's panes: THE single source of truth
    /// for terminal geometry. Rendering, PTY resizing, mouse hit-testing,
    /// IME, selection and screenshots all go through this (via
    /// [`App::content_area_for`]).
    pub fn content_area(&self) -> crate::window::PaneRect {
        let (w, h) = self.win.window.as_ref()
            .map_or((800, 600), |w| { let s = w.inner_size(); (s.width as usize, s.height as usize) });
        self.content_area_for(w, h)
    }

    /// Content area for a window of `w` x `h` physical pixels: what is left
    /// after the tab bar, the HUD strip, the agents dock (left), the chat dock
    /// (right) and the docked browser. `x` is the left edge of the first pane.
    pub fn content_area_for(&self, w: usize, h: usize) -> crate::window::PaneRect {
        let tbh = self.tab_bar_height();
        let ch = self.win.renderer.cell_height();
        let hud_h = if self.win.hud_visible { ch * 3 + 20 } else { 0 };
        let dock = crate::agents::runtime::dock_w(self, w);
        let avail = w - self.win.chat.dock_w(w).min(w);
        let width = match self.browser_visible().then(|| self.browser_geometry_for(w, h)) {
            Some(l) => l.terminal_w.min(avail),
            None => avail,
        }
        .saturating_sub(dock);
        crate::window::PaneRect { x: dock, y: tbh, width, height: h.saturating_sub(tbh + hud_h) }
    }

    /// Cell under the pointer when it lies on an OSC 8 hyperlink.
    pub fn link_cell_under_pointer(&self) -> Option<(usize, usize, usize)> {
        let pane = self.win.hover_pane?;
        let (_, rect, _) = self.win.wm.pane_layouts(self.content_area()).into_iter().find(|(i, _, _)| *i == pane)?;
        let (cw, ch) = (self.win.renderer.cell_width().max(1), self.win.renderer.cell_height().max(1));
        if self.win.cursor_x < rect.x || self.win.cursor_y < rect.y { return None; }
        let (col, row) = ((self.win.cursor_x - rect.x) / cw, (self.win.cursor_y - rect.y) / ch);
        let t = &self.win.wm.active_tab().pane(pane)?.terminal;
        t.hyperlink_at(row, col)?;
        Some((pane, row, col))
    }

    /// Map a window pixel to (row, col) inside the active pane.
    pub fn pixel_to_cell(&self, px: usize, py: usize) -> (usize, usize) {
        let cw = self.win.renderer.cell_width().max(1);
        let ch = self.win.renderer.cell_height().max(1);
        let area = self.content_area();
        let rect = self.win.wm.pane_layouts(area)
            .into_iter()
            .find(|(_, _, active)| *active)
            .map(|(_, r, _)| r)
            .unwrap_or(area);
        let col = px.saturating_sub(rect.x) / cw;
        let row = py.saturating_sub(rect.y) / ch;
        (row, col)
    }

    fn update_resize_cursor(&self) {
        use winit::window::CursorIcon;
        let Some(w) = &self.win.window else { return };
        let area = self.content_area();
        let icon = if self.win.dragging_border.is_some() {
            None
        } else {
            match self.win.wm.active_tab().border_at(area, self.win.cursor_x, self.win.cursor_y, 4) {
                Some((_, crate::window::tab::SplitDir::Horizontal)) => Some(CursorIcon::ColResize),
                Some((_, crate::window::tab::SplitDir::Vertical)) => Some(CursorIcon::RowResize),
                None => Some(CursorIcon::Text),
            }
        };
        if let Some(icon) = icon {
            w.set_cursor(icon);
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        lifecycle::on_resumed(self, event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.handle_window_event(event_loop, id, event);
        // Windows closed by this event (close button, answered confirmation ...).
        window_ops::flush_closes(self, event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        lifecycle::about_to_wait(self, event_loop);
    }
}

impl App {
    fn handle_window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // Route by OS window id: from here on `self.win` is the event's window.
        let Some(win_id) = self.windows.id_of(&id) else { return };
        self.activate(win_id);
        // Any key press or click skips the startup splash (the event still goes through).
        if lifecycle::startup_active(self) {
            let pressed = match &event {
                WindowEvent::KeyboardInput { event: k, .. } => k.state.is_pressed(),
                WindowEvent::MouseInput { state, .. } => state.is_pressed(),
                _ => false,
            };
            if pressed {
                self.startup_skipped = true;
                self.request_redraw();
            }
        }
        match event {
            // The close button closes only this window (the last one quits the app).
            WindowEvent::CloseRequested => window_ops::request_close_active(self, event_loop),
            WindowEvent::RedrawRequested => lifecycle::redraw(self),
            WindowEvent::Resized(size) => lifecycle::handle_resize(self, size.width, size.height),
            WindowEvent::ModifiersChanged(mods) => {
                crate::input::note_modifiers(&mods);
                let was_super = self.win.modifiers.super_key();
                self.win.modifiers = mods.state();
                if was_super != self.win.modifiers.super_key() {
                    self.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.win.cursor_x = position.x as usize;
                self.win.cursor_y = position.y as usize;

                // Command palette: hover highlights rows; swallow the move.
                if self.win.command_palette.visible {
                    overlays::palette_cursor_moved(self);
                    return;
                }

                // Docked AI chat: divider drag, hover, resize cursor.
                if crate::ai::chat::on_cursor_moved(self) {
                    return;
                }

                if crate::agents::runtime::on_cursor_moved(self) {
                    return;
                }

                if crate::network::browser::on_cursor_moved(self) {
                    return;
                }

                // Context menu hover, scrollbar / tab drags, tab + scrollbar hover.
                if mouse::on_cursor_moved(self) {
                    return;
                }

                // Divider drag: resize splits live
                if let Some(border) = self.win.dragging_border {
                    let area = self.content_area();
                    let (x, y) = (self.win.cursor_x, self.win.cursor_y);
                    let min = panes::min_size(self);
                    if self.win.wm.active_tab_mut().drag_border(area, border, x, y, min) {
                        if let Some(win) = &self.win.window {
                            let s = win.inner_size();
                            lifecycle::handle_resize(self, s.width, s.height);
                        }
                        self.request_redraw();
                    }
                    return;
                }

                // Hover highlight + resize cursor over dividers
                if self.win.cursor_y >= self.tab_bar_height() {
                    let area = self.content_area();
                    let old_hover = self.win.hover_pane;
                    self.win.hover_pane = self.win.wm.active_tab().pane_at(area, self.win.cursor_x, self.win.cursor_y);
                    let old_border = self.win.hover_border;
                    self.win.hover_border = self.win.wm.active_tab().border_at(area, self.win.cursor_x, self.win.cursor_y, 4).map(|(b, _)| b);
                    if (old_hover != self.win.hover_pane || old_border != self.win.hover_border)
                        && self.win.wm.active_tab().pane_count() > 1
                    {
                        self.request_redraw();
                    }
                    self.update_resize_cursor();
                    let link = self.link_cell_under_pointer();
                    if link != self.win.link_hover {
                        self.win.link_hover = link;
                        self.request_redraw();
                    }
                    // Command blocks: hover toolbar / gutter (sets pointer cursor)
                    if !self.win.mouse_pressed && crate::blocks_ui::on_mouse_move(self) {
                        return;
                    }
                }

                if self.win.mouse_pressed && self.win.selection.dragging {
                    mouse::drag_selection(self);
                } else if self.win.mouse_pressed {
                    let mouse_mode = self.win.wm.active_pane().terminal.mouse_mode;
                    if mouse_mode == MouseMode::AnyEvent && !self.win.modifiers.shift_key() {
                        let (row, col) = self.pixel_to_cell(self.win.cursor_x, self.win.cursor_y);
                        let seq = format!("\x1b[<35;{};{}M", col + 1, row + 1);
                        self.win.wm.active_pane_mut().write(seq.as_bytes());
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if self.win.mui.tabs.hover.take().is_some() || self.win.mui.scrollbar.hover.take().is_some() {
                    self.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                // Confirmation modal is modal for the mouse too.
                if self.win.confirm.visible() {
                    if state == ElementState::Pressed && button == MouseButton::Left {
                        if let Some((req, choice)) = self.win.confirm.click(self.win.cursor_x, self.win.cursor_y) {
                            crate::ui::confirm::resolve(self, req, choice);
                        }
                    }
                    self.request_redraw();
                    return;
                }
                // Workflow overlays (compare buttons, candidate cards) own the mouse.
                if self.workflows.overlay_visible() {
                    if state == ElementState::Pressed && button == MouseButton::Left {
                        crate::workflow::on_click(self);
                    }
                    return;
                }
                // Command palette is modal for the mouse: click runs/closes.
                if self.win.command_palette.visible {
                    if state == ElementState::Pressed && button == MouseButton::Left {
                        overlays::palette_click(self, event_loop);
                    }
                    return;
                }
                let mouse_mode = self.win.wm.active_pane().terminal.mouse_mode;

                // Other buttons over the chat dock are swallowed.
                if state == ElementState::Pressed
                    && button != MouseButton::Left
                    && crate::ai::chat::contains(self, self.win.cursor_x, self.win.cursor_y)
                {
                    return;
                }

                match (state, button) {
                    (ElementState::Pressed, MouseButton::Left) => {
                        self.win.mouse_pressed = true;
                        // Docked AI chat: focus, buttons, divider grab.
                        if crate::ai::chat::on_mouse_press(self) {
                            self.win.mouse_pressed = false;
                            return;
                        }
                        // Mission Control dock: select / jump to an agent.
                        if crate::agents::runtime::on_mouse_press(self) {
                            self.win.mouse_pressed = false;
                            return;
                        }
                        // Browser toolbar / divider / focus handoff
                        if crate::network::browser::on_mouse_press(self) {
                            return;
                        }
                        // Context menu, tab rename commit, scrollbar.
                        if mouse::on_left_press(self) {
                            self.win.mouse_pressed = false;
                            return;
                        }
                        // Inline AI: suggestion bar buttons / popover
                        if crate::ai::inline::on_click(self) {
                            self.win.mouse_pressed = false;
                            return;
                        }
                        let tbh = self.tab_bar_height();
                        if self.win.cursor_y < tbh {
                            shortcuts::handle_click(self, event_loop);
                        } else {
                            let area = self.content_area();
                            // Grab a split divider?
                            if let Some((border, _)) = self.win.wm.active_tab().border_at(area, self.win.cursor_x, self.win.cursor_y, 4) {
                                // Double-click a divider: reset that split to 50/50.
                                panes::register_border_click(self, border);
                                self.win.dragging_border = Some(border);
                                return;
                            }
                            // Click in content area: focus the pane under the cursor
                            if let Some(idx) = self.win.wm.active_tab().pane_at(area, self.win.cursor_x, self.win.cursor_y) {
                                if idx != self.win.wm.active_tab().active {
                                    panes::focus_pane_idx(self, idx);
                                    self.request_redraw();
                                    // Cmd+click on another pane only focuses it;
                                    // the click is not passed on to the app.
                                    if self.win.modifiers.super_key() {
                                        self.win.mouse_pressed = false;
                                        return;
                                    }
                                }
                            }
                            // Command blocks: toolbar buttons / gutter select / fold toggle
                            if crate::blocks_ui::on_click(self) {
                                self.win.mouse_pressed = false;
                                return;
                            }
                            // Then handle Cmd+Click URL / mouse mode / selection
                            if self.win.modifiers.super_key() {
                                if let Some(url) = mouse::url_at_cursor(self) {
                                    // Cmd+Click opens in Rift's browser, Cmd+Shift+Click externally.
                                    if self.win.modifiers.shift_key() || !cfg!(feature = "webview") {
                                        crate::tools::url_detect::open_url(&url);
                                    } else {
                                        crate::network::browser::open(self, &url);
                                    }
                                }
                            } else if mouse_mode != MouseMode::None && !self.win.modifiers.shift_key() {
                                let (row, col) = self.pixel_to_cell(self.win.cursor_x, self.win.cursor_y);
                                let seq = format!("\x1b[<0;{};{}M", col + 1, row + 1);
                                self.win.wm.active_pane_mut().write(seq.as_bytes());
                            } else {
                                mouse::begin_selection(self);
                            }
                        }
                    }
                    (ElementState::Released, MouseButton::Left) => {
                        let was_pressed = std::mem::replace(&mut self.win.mouse_pressed, false);
                        if crate::agents::runtime::on_mouse_release(self) {
                            return;
                        }
                        if crate::ai::chat::on_mouse_release(self) {
                            return;
                        }
                        if crate::network::browser::on_mouse_release(self) {
                            return;
                        }
                        if mouse::on_left_release(self) {
                            return;
                        }
                        if self.win.dragging_border.take().is_some() {
                            self.update_resize_cursor();
                            return;
                        }
                        if self.win.selection.dragging {
                            mouse::finish_selection(self);
                        } else if was_pressed && mouse_mode != MouseMode::None && !self.win.modifiers.shift_key() {
                            let (row, col) = self.pixel_to_cell(self.win.cursor_x, self.win.cursor_y);
                            let seq = format!("\x1b[<0;{};{}m", col + 1, row + 1);
                            self.win.wm.active_pane_mut().write(seq.as_bytes());
                        }
                    }
                    (ElementState::Pressed, MouseButton::Right) => {
                        // Mission Control dock: card context menu.
                        if crate::agents::runtime::on_right_press(self) {
                            return;
                        }
                        mouse::dismiss_menu(self);
                        if self.win.cursor_y < self.tab_bar_height() {
                            // Nothing on the tab bar yet.
                        } else if mouse_mode != MouseMode::None && !self.win.modifiers.shift_key() {
                            let (row, col) = self.pixel_to_cell(self.win.cursor_x, self.win.cursor_y);
                            let seq = format!("\x1b[<2;{};{}M", col + 1, row + 1);
                            self.win.wm.active_pane_mut().write(seq.as_bytes());
                        } else {
                            // Paste moved into the context menu.
                            mouse::open_context_menu(self);
                        }
                    }
                    (ElementState::Pressed, MouseButton::Middle) => {
                        if mouse::dismiss_menu(self) {
                            return;
                        }
                        if self.win.cursor_y < self.tab_bar_height() {
                            tabs::middle_click(self);
                        } else if mouse_mode != MouseMode::None && !self.win.modifiers.shift_key() {
                            let (row, col) = self.pixel_to_cell(self.win.cursor_x, self.win.cursor_y);
                            let seq = format!("\x1b[<1;{};{}M", col + 1, row + 1);
                            self.win.wm.active_pane_mut().write(seq.as_bytes());
                        } else if let Some(text) = selection::paste_from_clipboard() {
                            mouse::paste_text(self, &text);
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.win.command_palette.visible {
                    let lines = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y as i32,
                        MouseScrollDelta::PixelDelta(pos) => {
                            let ch = self.win.renderer.cell_height().max(1) as f64;
                            (pos.y / ch) as i32
                        }
                    };
                    overlays::palette_wheel(self, lines);
                    return;
                }
                if crate::agents::runtime::on_wheel(self, delta) {
                    return;
                }
                if crate::ai::chat::on_wheel(self, delta) {
                    return;
                }
                // Notches = 3 lines, trackpad pixels accumulate; mouse
                // reporting / alt-screen aware.
                mouse::handle_wheel(self, delta);
            }
            WindowEvent::Ime(ime) => ime::handle_ime(self, ime),
            WindowEvent::Focused(focused) => {
                self.win.window_focused = focused;
                if focused {
                    window_ops::note_focused(self, win_id);
                }
                if !focused && !self.win.ime_preedit.is_empty() {
                    self.win.ime_preedit.clear();
                    self.request_redraw();
                }
                // Send focus in/out sequences if terminal requested focus reporting
                let pane = self.win.wm.active_pane_mut();
                if pane.terminal.focus_reporting {
                    let seq = if focused { b"\x1b[I" } else { b"\x1b[O" };
                    pane.write(seq);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed || event.repeat {
                    // Cmd+Q — quit (macOS convention)
                    if self.win.modifiers.super_key() && !self.win.modifiers.shift_key() {
                        if let winit::keyboard::Key::Character(ref s) = event.logical_key {
                            if s.eq_ignore_ascii_case("q") {
                                window_ops::quit(self, event_loop);
                                return;
                            }
                        }
                    }
                    shortcuts::handle_key(self, &event, event_loop);
                } else {
                    shortcuts::handle_key_release(self, &event);
                }
            }
            _ => {}
        }
    }
}

fn chrono_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    let (y, mo, d) = days_to_ymd(days);
    format!("{y:04}{mo:02}{d:02}-{h:02}{m:02}{s:02}")
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    days += 719468;
    let era = days / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
