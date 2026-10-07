mod lifecycle;
mod overlays;
pub mod shortcuts;

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::ai::{AiPanel, Autocomplete, LlmManager};
use crate::config::Config;
use crate::network::{SshConnectRequest, SshDialog, SshPty, WebViewDialog, WebViewPane};
use crate::renderer::Renderer;
use crate::tools::audit::AuditLog;
use crate::tools::blocks::BlockManager;
use crate::tools::cicd::CicdPanel;
use crate::tools::compare::CompareView;
use crate::tools::docker_panel::DockerPanel;
use crate::tools::error_detect::{ErrorDetector, ErrorNotification};
use crate::tools::file_manager::FileManager;
use crate::tools::git_panel::GitPanel;
use crate::tools::heatmap::Heatmap;
use crate::tools::hud::Hud;
use crate::tools::notify::Notifier;
use crate::tools::recording::Recorder;
use crate::tools::search::SearchOverlay;
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
    pub llm: LlmManager,
    pub ai_panel: AiPanel,
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
    pub selection: Selection,
    pub mouse_pressed: bool,
    pub window_focused: bool,

    // Integrated tools (read via PTY hooks, not direct field access)
    #[allow(dead_code)]
    pub blocks: BlockManager,
    #[allow(dead_code)]
    pub error_detector: ErrorDetector,
    pub error_notif: ErrorNotification,
    pub secret_mask: SecretMasker,
    pub file_manager: FileManager,
    pub git_panel: GitPanel,
    pub cicd: CicdPanel,
    pub teaching: TeachingMode,
    pub heatmap: Heatmap,
    pub docker: DockerPanel,
    pub audit: AuditLog,
    pub notifier: Notifier,

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
    pub fn new(config: Config, renderer: Renderer, wm: WindowManager) -> Self {
        let prefs = Preferences::new(&config);
        let welcome = Welcome::new_auto();
        let menubar = AppMenuBar::new();
        let llm = LlmManager::new(config.llm.clone());
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
            llm,
            ai_panel: AiPanel::new(),
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
            selection: Selection::new(),
            mouse_pressed: false,
            window_focused: true,

            blocks: BlockManager::new(),
            error_detector: ErrorDetector::new(),
            error_notif: ErrorNotification::new(),
            secret_mask: SecretMasker::new(),
            file_manager: FileManager::new(),
            git_panel: GitPanel::new(),
            cicd: CicdPanel::new(),
            teaching: TeachingMode::new(),
            heatmap: Heatmap::new(),
            docker: DockerPanel::new(),
            audit: AuditLog::new(),
            notifier: Notifier::new(),

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

    pub fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    pub fn pixel_to_cell(&self, px: usize, py: usize) -> (usize, usize) {
        let cw = self.renderer.cell_width();
        let ch = self.renderer.cell_height();
        let tbh = self.tab_bar_height();
        let col = px / cw.max(1);
        let row = py.saturating_sub(tbh) / ch.max(1);
        (row, col)
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        lifecycle::on_resumed(self, event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                // Auto-save config on exit
                crate::config::toml::save_config(&self.config);
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
                        if self.cursor_y < tbh {
                            shortcuts::handle_click(self, event_loop);
                        } else if self.modifiers.super_key() {
                            // Cmd+Click: open URL at cursor position
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
                    (ElementState::Released, MouseButton::Left) => {
                        self.mouse_pressed = false;
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
