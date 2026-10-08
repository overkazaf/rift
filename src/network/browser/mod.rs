//! Built-in browser: toolbar chrome, address field, docking and navigation glue.
//!
//! The page itself is a native child view (see `network::webview`); everything
//! else here is drawn into the softbuffer surface. App integration is limited
//! to a handful of call sites into this module:
//! `poll` (about_to_wait), `on_cursor_moved` / `on_mouse_press` /
//! `on_mouse_release` (mouse events), `handle_key` (key interception while the
//! address field is focused), `render` (redraw) and `run_command` (menu).

pub mod chrome;
pub mod field;
pub mod input;
pub mod smart_url;

use std::time::Instant;

use crate::app::App;
use crate::window::PaneRect;

use chrome::{BrowserLayout, Hit};
use field::AddrField;

pub use input::{handle_key, on_cursor_moved, on_mouse_press, on_mouse_release};

/// Browser commands reachable from menu accelerators (which fire even while
/// the native webview owns the keyboard, unlike winit key events).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserCmd {
    Back,
    Forward,
    Reload,
    FocusAddress,
    Close,
}

/// UI-side browser state (the wry-side state lives in `WebViewPane`).
pub struct BrowserUi {
    pub field: AddrField,
    /// Address field has keyboard focus.
    pub editing: bool,
    /// Keyboard focus is in the browser (page or toolbar) rather than the terminal.
    pub focused: bool,
    /// Browser's share of the window width when docked.
    pub ratio: f32,
    pub hover: Option<Hit>,
    pub divider_hover: bool,
    pub divider_drag: bool,
    pub field_drag: bool,
    /// Was the page focused when the address field was entered (restored on Esc).
    pub return_to_page: bool,
    pub last_click: Option<(Instant, usize, usize)>,
    anim_start: Instant,
}

impl BrowserUi {
    pub fn new() -> Self {
        Self {
            field: AddrField::new(),
            editing: false,
            focused: false,
            ratio: chrome::DEFAULT_RATIO,
            hover: None,
            divider_hover: false,
            divider_drag: false,
            field_drag: false,
            return_to_page: true,
            last_click: None,
            anim_start: Instant::now(),
        }
    }

    pub fn anim_ms(&self) -> u128 {
        self.anim_start.elapsed().as_millis()
    }
}

impl Default for BrowserUi {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn browser_visible(&self) -> bool {
        self.webview.as_ref().is_some_and(|wv| wv.visible)
    }

    /// Geometry of the docked/maximized browser regardless of visibility.
    pub fn browser_geometry(&self) -> BrowserLayout {
        let (w, h, scale) = self
            .window
            .as_ref()
            .map_or((800, 600, 1.0), |win| {
                let s = win.inner_size();
                (s.width as usize, s.height as usize, win.scale_factor())
            });
        BrowserLayout::compute(
            w,
            h,
            self.tab_bar_height(),
            self.renderer.cell_width(),
            self.renderer.cell_height(),
            scale,
            self.browser.ratio,
            self.webview_maximized,
        )
    }

    /// Geometry when the browser is visible (single source of truth for docking).
    pub fn browser_layout(&self) -> Option<BrowserLayout> {
        self.browser_visible().then(|| self.browser_geometry())
    }

    /// Whole browser region (gutter + toolbar + page) when visible.
    #[allow(dead_code)]
    pub fn browser_rect(&self) -> Option<PaneRect> {
        self.browser_layout().map(|l| PaneRect {
            x: l.gutter.x,
            y: l.toolbar.y,
            width: l.web.w + l.gutter.w,
            height: l.gutter.h,
        })
    }

    /// Width available to terminal panes for a window `win_w` pixels wide.
    pub fn terminal_width(&self, win_w: usize) -> usize {
        match self.browser_layout() {
            Some(l) => l.terminal_w.min(win_w),
            None => win_w,
        }
    }
}

/// Push the layout's page bounds to the native webview.
pub fn apply_bounds(app: &App) {
    if let (Some(wv), Some(l)) = (&app.webview, app.browser_layout()) {
        wv.set_bounds(l.web_logical());
    }
}

/// Re-layout after any change of docking/visibility: terminal panes + webview.
pub fn relayout(app: &mut App) {
    crate::app::shortcuts::resize_from_window(app);
    apply_bounds(app);
    app.request_redraw();
}

/// Open `input` (URL or search text) in the built-in browser, creating it if needed.
pub fn open(app: &mut App, input: &str) {
    let Some(url) = smart_url::resolve(input) else { return };
    let Some(window) = app.window.clone() else { return };

    app.webview_tab = Some(app.wm.active_tab);
    app.webview_pane = Some(app.wm.active_tab().active);
    app.browser.editing = false;

    if let Some(wv) = &mut app.webview {
        wv.navigate(&url);
        wv.set_visible(true);
    } else {
        let bounds = app.browser_geometry().web_logical();
        let waker = Some(app.wm.get_proxy());
        match crate::network::WebViewPane::new(&window, &url, bounds, waker) {
            Ok(wv) => {
                log::info!("WebView opened: {url}");
                app.webview = Some(wv);
            }
            Err(e) => {
                log::error!("WebView failed: {e}");
                return;
            }
        }
    }
    relayout(app);
    focus_page(app);
}

/// Cmd+Shift+B / menu: show or hide the browser without discarding the page.
pub fn toggle(app: &mut App) {
    let Some(wv) = &mut app.webview else {
        app.webview_dialog.toggle();
        return;
    };
    if wv.visible {
        wv.set_visible(false);
        wv.focus_parent();
        app.browser.editing = false;
        app.browser.focused = false;
        focus_terminal_window(app);
    } else {
        wv.set_visible(true);
        app.webview_tab = Some(app.wm.active_tab);
    }
    relayout(app);
    if app.browser_visible() {
        focus_page(app);
    }
}

/// Discard the webview entirely (toolbar close button / Cmd+W).
pub fn close(app: &mut App) {
    if let Some(wv) = &app.webview {
        wv.focus_parent();
    }
    app.webview = None;
    app.webview_tab = None;
    app.webview_pane = None;
    app.webview_maximized = false;
    app.browser.editing = false;
    app.browser.focused = false;
    app.browser.hover = None;
    app.browser.divider_hover = false;
    app.browser.divider_drag = false;
    focus_terminal_window(app);
    relayout(app);
}

pub fn toggle_maximize(app: &mut App) {
    app.webview_maximized = !app.webview_maximized;
    relayout(app);
}

/// Give the page keyboard focus.
pub fn focus_page(app: &mut App) {
    if let Some(wv) = &app.webview {
        if wv.visible {
            wv.focus();
            app.browser.focused = true;
        }
    }
}

/// Return keyboard focus to the terminal (winit view).
pub fn focus_terminal(app: &mut App) {
    if let Some(wv) = &app.webview {
        wv.focus_parent();
    }
    app.browser.focused = false;
    focus_terminal_window(app);
}

fn focus_terminal_window(app: &App) {
    if let Some(w) = &app.window {
        w.focus_window();
    }
}

/// Focus the address field and select its content (Cmd+L).
pub fn begin_edit(app: &mut App, select_all: bool) {
    let Some(wv) = &app.webview else { return };
    if !wv.visible {
        return;
    }
    if !app.browser.editing {
        app.browser.return_to_page = app.browser.focused;
        let url = wv.url.clone();
        app.browser.field.set_text(&url);
        app.browser.editing = true;
    }
    // The native webview may be first responder; key events must reach winit.
    wv.focus_parent();
    focus_terminal_window(app);
    app.browser.focused = true;
    if select_all {
        app.browser.field.select_all();
    }
    app.request_redraw();
}

/// Leave the address field. `commit` navigates to its content.
pub fn end_edit(app: &mut App, commit: bool) {
    if !app.browser.editing {
        return;
    }
    app.browser.editing = false;
    app.browser.field_drag = false;
    let text = app.browser.field.text();
    let mut go_page = app.browser.return_to_page;
    if commit {
        if let Some(url) = smart_url::resolve(&text) {
            if let Some(wv) = &mut app.webview {
                wv.navigate(&url);
            }
            go_page = true;
        }
    }
    if let Some(wv) = &app.webview {
        app.browser.field.set_text(&wv.url);
    }
    if go_page {
        focus_page(app);
    } else {
        focus_terminal(app);
    }
    app.request_redraw();
}

/// Execute a toolbar/menu command.
pub fn run_command(app: &mut App, cmd: BrowserCmd) {
    if !app.browser_visible() {
        return;
    }
    match cmd {
        BrowserCmd::FocusAddress => begin_edit(app, true),
        // Page-level commands only apply while the browser owns the keyboard,
        // so the same accelerators stay free for the terminal otherwise.
        _ if !app.browser.focused => {}
        BrowserCmd::Back => back(app),
        BrowserCmd::Forward => forward(app),
        BrowserCmd::Reload => reload_or_stop(app, false),
        BrowserCmd::Close => close(app),
    }
    app.request_redraw();
}

fn back(app: &App) {
    if let Some(wv) = &app.webview {
        wv.go_back();
    }
}

fn forward(app: &App) {
    if let Some(wv) = &app.webview {
        wv.go_forward();
    }
}

/// Toolbar reload button toggles to stop while loading; Cmd+R always reloads.
fn reload_or_stop(app: &mut App, stop_if_loading: bool) {
    if let Some(wv) = &mut app.webview {
        if stop_if_loading && wv.loading {
            wv.stop();
        } else {
            wv.reload();
        }
    }
}

pub(crate) fn toolbar_click(app: &mut App, hit: Hit) {
    match hit {
        Hit::Back => back(app),
        Hit::Forward => forward(app),
        Hit::Reload => reload_or_stop(app, true),
        Hit::Maximize => toggle_maximize(app),
        Hit::Close => close(app),
        _ => {}
    }
}

/// Drain webview events; call from `about_to_wait`.
pub fn poll(app: &mut App) {
    let Some(wv) = &mut app.webview else { return };
    let out = wv.poll_events();
    let visible = wv.visible;
    if !visible {
        return;
    }
    let mut redraw = out.changed;
    if out.focus_clicked {
        if app.browser.editing {
            // The user clicked into the page: drop the edit without navigating.
            app.browser.editing = false;
            if let Some(wv) = &app.webview {
                app.browser.field.set_text(&wv.url);
            }
        }
        if !app.browser.focused {
            app.browser.focused = true;
            redraw = true;
        }
    }
    if redraw {
        app.request_redraw();
    }
}

pub use chrome::render;
