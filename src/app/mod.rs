mod lifecycle;
mod overlays;
pub mod shortcuts;

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::ai::{AiPanel, Advisor, Autocomplete, LlmManager};
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

pub struct App {
    pub config: Config,
    pub wm: WindowManager,
    pub renderer: Renderer,
    pub modifiers: ModifiersState,
    pub recorder: Option<Recorder>,
    pub prefs: Preferences,
    pub welcome: Welcome,
    pub ssh_dialog: SshDialog,
    pub autocomplete: Autocomplete,
    pub webview_dialog: WebViewDialog,
    pub webview: Option<WebViewPane>,
    pub webview_tab: Option<usize>,
    pub webview_pane: Option<usize>,
    pub webview_maximized: bool,
    pub llm: LlmManager,
    pub ai_panel: AiPanel,
    pub advisor: Advisor,
    pub menubar: AppMenuBar,
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub ssh_connecting: Option<SshConnecting>,
    pub timewarp: TimeWarp,
    pub timewarp_browser: TimeWarpBrowser,
    pub hud: Hud,
    pub hud_visible: bool,
    pub broadcast: bool,
    pub compare_view: CompareView,
    pub search: SearchOverlay,
    pub command_palette: CommandPalette,
    pub selection: Selection,
    pub mouse_pressed: bool,
    pub window_focused: bool,
    pub hover_pane: Option<usize>,
    pub dragging_border: Option<usize>,
    pub addr_bar_editing: bool,
    pub addr_bar_text: String,

    pub blocks: BlockManager,
    pub error_detector: ErrorDetector,
    pub error_notif: ErrorNotification,
    pub exec_preview: ExecPreview,
    pub secret_mask: SecretMasker,
    pub file_manager: FileManager,
    pub git_panel: GitPanel,
    pub cicd: CicdPanel,
    pub teaching: TeachingMode,
    pub heatmap: Heatmap,
    pub docker: DockerPanel,
    pub audit: AuditLog,
    pub notifier: Notifier,
    pub observer: Observer,
    pub observer_summary: Option<String>,
    pub network_monitor: NetworkMonitor,
    pub process_tree: ProcessTree,
    pub system_info: SystemInfo,
    pub port_dashboard: PortDashboard,
    pub regex_playground: RegexPlayground,
    pub history: HistorySearch,

    pub needs_render: bool,
    pub startup_time: std::time::Instant,

    #[cfg(feature = "gpu")]
    pub gpu_pipeline: Option<crate::renderer::gpu::GpuPipeline>,

    pub window: Option<Arc<Window>>,
    pub context: Option<softbuffer::Context<Arc<Window>>>,
    pub surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
}

pub struct SshConnecting {
    pub rx: std::sync::mpsc::Receiver<Result<SshPty, String>>,
    pub req: SshConnectRequest,
}

impl App {
    pub fn new(config: Config, renderer: Renderer, mut wm: WindowManager) -> Self {
        let prefs = Preferences::new(&config);
        let welcome = Welcome::new_auto();
        let menubar = AppMenuBar::new();
        let llm = LlmManager::new(config.llm.clone());

        // Restore the previous session (tabs, titles, working dirs,
        // scrollback) if one was saved on a prior exit.
        crate::tools::session::restore_session(&mut wm);

        Self {
            config,
            wm,
            renderer,
            modifiers: ModifiersState::empty(),
            recorder: None,
            prefs,
            welcome,
            ssh_dialog: SshDialog::new(),
            autocomplete: Autocomplete::new(),
            webview_dialog: WebViewDialog::new(),
            webview: None,
            webview_tab: None,
            webview_pane: None,
            webview_maximized: false,
            llm,
            ai_panel: AiPanel::new(),
            advisor: Advisor::new(),
            menubar,
            cursor_x: 0,
            cursor_y: 0,
            ssh_connecting: None,
            timewarp: TimeWarp::new(500),
            timewarp_browser: TimeWarpBrowser::new(),
            hud: Hud::new(),
            hud_visible: false,
            broadcast: false,
            compare_view: CompareView::new(),
            search: SearchOverlay::new(),
            command_palette: CommandPalette::new(),
            selection: Selection::new(),
            mouse_pressed: false,
            window_focused: true,
            hover_pane: None,
            dragging_border: None,
            addr_bar_editing: false,
            addr_bar_text: String::new(),

            blocks: BlockManager::new(),
            error_detector: ErrorDetector::new(),
            error_notif: ErrorNotification::new(),
            exec_preview: ExecPreview::hidden(),
            secret_mask: SecretMasker::new(),
            file_manager: FileManager::new(),
            git_panel: GitPanel::new(),
            cicd: CicdPanel::new(),
            teaching: TeachingMode::new(),
            heatmap: Heatmap::new(),
            docker: DockerPanel::new(),
            audit: AuditLog::new(),
            notifier: Notifier::new(),
            observer: Observer::new(),
            observer_summary: None,
            needs_render: true,
            network_monitor: NetworkMonitor::new(),
            process_tree: ProcessTree::new(),
            system_info: SystemInfo::new(),
            port_dashboard: PortDashboard::new(),
            regex_playground: RegexPlayground::new(),
            history: HistorySearch::new(),

            startup_time: std::time::Instant::now(),

            #[cfg(feature = "gpu")]
            gpu_pipeline: None,

            window: None,
            context: None,
            surface: None,
        }
    }

    pub fn tab_bar_height(&self) -> usize {
        self.renderer.cell_height() + 16
    }

    pub fn update_title(&self) {
        if let Some(w) = &self.window {
            let rec = if self.recorder.is_some() { " [REC]" } else { "" };
            let bcast = if self.broadcast { " [BROADCAST]" } else { "" };
            let tab_count = self.wm.tab_count();
            let tab_idx = self.wm.active_tab + 1;
            let pane_title = self.wm.active_tab().display_title();
            if tab_count > 1 {
                w.set_title(&format!("rift{rec}{bcast} [{tab_idx}/{tab_count}] {pane_title}"));
            } else {
                w.set_title(&format!("rift{rec}{bcast} — {pane_title}"));
            }
        }
    }

    pub fn toggle_recording(&mut self) {
        if self.recorder.is_some() {
            let rec = self.recorder.take().unwrap();
            rec.finish();
            log::info!("Recording stopped");
        } else {
            let pane = self.wm.active_pane();
            let now = chrono_timestamp();
            let path = std::path::PathBuf::from(format!("recordings/rift-{now}.cast"));
            match Recorder::start(&path, pane.terminal.cols, pane.terminal.rows) {
                Ok(rec) => {
                    log::info!("Recording started: {}", path.display());
                    self.recorder = Some(rec);
                }
                Err(e) => log::error!("Failed to start recording: {e}"),
            }
        }
        self.update_title();
    }

    pub fn request_redraw(&mut self) {
        self.needs_render = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Rect occupied by the active tab's panes (mirrors lifecycle::redraw).
    pub fn content_area(&self) -> crate::window::PaneRect {
        let (w, h) = self.window.as_ref()
            .map_or((800, 600), |w| { let s = w.inner_size(); (s.width as usize, s.height as usize) });
        let tbh = self.tab_bar_height();
        let ch = self.renderer.cell_height();
        let hud_h = if self.hud_visible { ch * 3 + 20 } else { 0 };
        let wv_visible = self.webview.as_ref().map_or(false, |wv| wv.visible);
        let width = if wv_visible && self.webview_maximized { 0 } else if wv_visible { w / 2 } else { w };
        crate::window::PaneRect { x: 0, y: tbh, width, height: h.saturating_sub(tbh + hud_h) }
    }

    /// Map a window pixel to (row, col) inside the active pane.
    pub fn pixel_to_cell(&self, px: usize, py: usize) -> (usize, usize) {
        let cw = self.renderer.cell_width().max(1);
        let ch = self.renderer.cell_height().max(1);
        let area = self.content_area();
        let rect = self.wm.pane_layouts(area)
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
        let Some(w) = &self.window else { return };
        let area = self.content_area();
        let icon = if self.dragging_border.is_some() {
            None
        } else {
            match self.wm.active_tab().border_at(area, self.cursor_x, self.cursor_y, 4) {
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

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                // Auto-save config and terminal session on exit
                crate::config::toml::save_config(&self.config);
                if let Err(e) = crate::tools::session::save_session(&self.wm) {
                    log::warn!("Failed to save session: {e}");
                }
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => lifecycle::redraw(self),
            WindowEvent::Resized(size) => lifecycle::handle_resize(self, size.width, size.height),
            WindowEvent::ModifiersChanged(mods) => {
                let was_super = self.modifiers.super_key();
                self.modifiers = mods.state();
                if was_super != self.modifiers.super_key() {
                    self.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_x = position.x as usize;
                self.cursor_y = position.y as usize;

                // Divider drag: resize splits live
                if let Some(border) = self.dragging_border {
                    let area = self.content_area();
                    let (x, y) = (self.cursor_x, self.cursor_y);
                    if self.wm.active_tab_mut().drag_border(area, border, x, y) {
                        if let Some(win) = &self.window {
                            let s = win.inner_size();
                            lifecycle::handle_resize(self, s.width, s.height);
                        }
                        self.request_redraw();
                    }
                    return;
                }

                // Hover highlight + resize cursor over dividers
                if self.cursor_y >= self.tab_bar_height() {
                    let area = self.content_area();
                    let old_hover = self.hover_pane;
                    self.hover_pane = self.wm.active_tab().pane_at(area, self.cursor_x, self.cursor_y);
                    if old_hover != self.hover_pane && self.wm.active_tab().pane_count() > 1 {
                        self.request_redraw();
                    }
                    self.update_resize_cursor();
                }

                if self.mouse_pressed && self.selection.dragging {
                    let (row, col) = self.pixel_to_cell(self.cursor_x, self.cursor_y);
                    self.selection.extend_to(row, col);
                    self.request_redraw();
                } else if self.mouse_pressed {
                    let mouse_mode = self.wm.active_pane().terminal.mouse_mode;
                    if mouse_mode == MouseMode::AnyEvent && !self.modifiers.shift_key() {
                        let (row, col) = self.pixel_to_cell(self.cursor_x, self.cursor_y);
                        let seq = format!("\x1b[<35;{};{}M", col + 1, row + 1);
                        self.wm.active_pane_mut().write(seq.as_bytes());
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let tbh = self.tab_bar_height();
                let mouse_mode = self.wm.active_pane().terminal.mouse_mode;

                match (state, button) {
                    (ElementState::Pressed, MouseButton::Left) => {
                        self.mouse_pressed = true;
                        // Click on address bar area?
                        let wv_visible = self.webview.as_ref().map_or(false, |wv| wv.visible);
                        if wv_visible {
                            let w = self.window.as_ref().map_or(800, |w| w.inner_size().width as usize);
                            let bar_x = if self.webview_maximized { 0 } else { w / 2 };
                            let bar_w = if self.webview_maximized { w } else { w / 2 };
                            let addr_bar_bottom = tbh + self.renderer.cell_height() + 12;
                            if self.cursor_x >= bar_x && self.cursor_y >= tbh && self.cursor_y < addr_bar_bottom {
                                // Check if clicking the maximize button (right 4 chars)
                                let cw = self.renderer.cell_width();
                                let btn_right = bar_x + bar_w;
                                let btn_left = btn_right.saturating_sub(5 * cw);
                                if self.cursor_x >= btn_left {
                                    shortcuts::toggle_webview_maximize(self);
                                    return;
                                }
                                self.addr_bar_editing = true;
                                self.addr_bar_text = self.webview.as_ref().map_or(String::new(), |wv| wv.url.clone());
                                self.request_redraw();
                                return;
                            } else {
                                self.addr_bar_editing = false;
                            }
                        }
                        if self.cursor_y < tbh {
                            shortcuts::handle_click(self, event_loop);
                        } else {
                            let area = self.content_area();
                            // Grab a split divider?
                            if let Some((border, _)) = self.wm.active_tab().border_at(area, self.cursor_x, self.cursor_y, 4) {
                                self.dragging_border = Some(border);
                                return;
                            }
                            // Click in content area: focus the pane under the cursor
                            if let Some(idx) = self.wm.active_tab().pane_at(area, self.cursor_x, self.cursor_y) {
                                if idx != self.wm.active_tab().active {
                                    self.wm.active_tab_mut().focus_pane(idx);
                                    self.selection.clear();
                                    self.request_redraw();
                                }
                            }
                            // Then handle Cmd+Click URL / mouse mode / selection
                            if self.modifiers.super_key() {
                                let (row, col) = self.pixel_to_cell(self.cursor_x, self.cursor_y);
                                let terminal = &self.wm.active_pane().terminal;
                                if row < terminal.rows {
                                    let line: String = terminal.grid[row].iter().map(|c| c.c).collect();
                                    if let Some(url) = crate::tools::url_detect::url_at_col(&line, col) {
                                        crate::tools::url_detect::open_url(&url);
                                    }
                                }
                            } else if mouse_mode != MouseMode::None && !self.modifiers.shift_key() {
                                let (row, col) = self.pixel_to_cell(self.cursor_x, self.cursor_y);
                                let seq = format!("\x1b[<0;{};{}M", col + 1, row + 1);
                                self.wm.active_pane_mut().write(seq.as_bytes());
                            } else {
                                self.selection.clear();
                                let (row, col) = self.pixel_to_cell(self.cursor_x, self.cursor_y);
                                self.selection.start_at(row, col);
                                self.request_redraw();
                            }
                        }
                    }
                    (ElementState::Released, MouseButton::Left) => {
                        self.mouse_pressed = false;
                        if self.dragging_border.take().is_some() {
                            self.update_resize_cursor();
                            return;
                        }
                        if self.selection.dragging {
                            self.selection.finish();
                            if self.selection.active {
                                let text = self.selection.extract_text(
                                    &self.wm.active_pane().terminal.grid,
                                );
                                if !text.is_empty() {
                                    selection::copy_to_clipboard(&text);
                                }
                            }
                            self.request_redraw();
                        } else if mouse_mode != MouseMode::None && !self.modifiers.shift_key() {
                            let (row, col) = self.pixel_to_cell(self.cursor_x, self.cursor_y);
                            let seq = format!("\x1b[<0;{};{}m", col + 1, row + 1);
                            self.wm.active_pane_mut().write(seq.as_bytes());
                        }
                    }
                    (ElementState::Pressed, MouseButton::Right) => {
                        if mouse_mode != MouseMode::None && !self.modifiers.shift_key() {
                            let (row, col) = self.pixel_to_cell(self.cursor_x, self.cursor_y);
                            let seq = format!("\x1b[<2;{};{}M", col + 1, row + 1);
                            self.wm.active_pane_mut().write(seq.as_bytes());
                        } else if let Some(text) = selection::paste_from_clipboard() {
                            let pane = self.wm.active_pane_mut();
                            if pane.terminal.bracketed_paste {
                                pane.write(b"\x1b[200~");
                                pane.write(text.as_bytes());
                                pane.write(b"\x1b[201~");
                            } else {
                                pane.write(text.as_bytes());
                            }
                        }
                    }
                    (ElementState::Pressed, MouseButton::Middle) => {
                        if mouse_mode != MouseMode::None && !self.modifiers.shift_key() {
                            let (row, col) = self.pixel_to_cell(self.cursor_x, self.cursor_y);
                            let seq = format!("\x1b[<1;{};{}M", col + 1, row + 1);
                            self.wm.active_pane_mut().write(seq.as_bytes());
                        } else if let Some(text) = selection::paste_from_clipboard() {
                            let pane = self.wm.active_pane_mut();
                            if pane.terminal.bracketed_paste {
                                pane.write(b"\x1b[200~");
                                pane.write(text.as_bytes());
                                pane.write(b"\x1b[201~");
                            } else {
                                pane.write(text.as_bytes());
                            }
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as i32,
                    MouseScrollDelta::PixelDelta(pos) => {
                        let ch = self.renderer.cell_height().max(1) as f64;
                        (pos.y / ch) as i32
                    }
                };
                if lines > 0 {
                    self.wm.active_pane_mut().terminal.scroll_view_up(lines as usize);
                } else if lines < 0 {
                    self.wm.active_pane_mut().terminal.scroll_view_down((-lines) as usize);
                }
                self.request_redraw();
            }
            WindowEvent::Focused(focused) => {
                self.window_focused = focused;
                // Send focus in/out sequences if terminal requested focus reporting
                let pane = self.wm.active_pane_mut();
                if pane.terminal.focus_reporting {
                    let seq = if focused { b"\x1b[I" } else { b"\x1b[O" };
                    pane.write(seq);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed || event.repeat {
                    // Cmd+Q — quit (macOS convention)
                    if self.modifiers.super_key() && !self.modifiers.shift_key() {
                        if let winit::keyboard::Key::Character(ref s) = event.logical_key {
                            if s.eq_ignore_ascii_case("q") {
                                crate::config::toml::save_config(&self.config);
                                if let Err(e) = crate::tools::session::save_session(&self.wm) {
                                    log::warn!("Failed to save session: {e}");
                                }
                                event_loop.exit();
                                return;
                            }
                        }
                    }
                    shortcuts::handle_key(self, &event, event_loop);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        lifecycle::about_to_wait(self, event_loop);
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
