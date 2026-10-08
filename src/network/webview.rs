use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::window::Window;

/// Events produced by wry's callbacks (which run on the main thread but
/// outside our event handling) and consumed by `WebViewPane::poll_events`.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum WvEvent {
    LoadStarted,
    LoadFinished,
    Title(String),
    /// A navigation (any frame) is about to happen; used as a hint to refresh the URL.
    Navigated,
    /// `target=_blank` / `window.open`: load in this webview instead.
    NewWindow(String),
    /// The page received a mouse/keyboard focus (injected script via IPC).
    Focused,
}

/// Result of draining the event queue.
#[derive(Default, Debug, Clone, Copy)]
pub struct PollOutcome {
    pub changed: bool,
    pub focus_clicked: bool,
}

/// A load that never reports `Finished` (e.g. a failed navigation) is treated
/// as done after this long, so the progress bar cannot spin forever.
const LOAD_TIMEOUT: Duration = Duration::from_secs(10);

/// Injected into every page so clicks/keys inside the native webview (which
/// winit never sees) can tell the app that the browser has keyboard focus.
#[cfg(feature = "webview")]
const FOCUS_SCRIPT: &str = "(function(){var p=function(){try{window.ipc.postMessage('focus')}catch(e){}};\
window.addEventListener('mousedown',p,true);window.addEventListener('keydown',p,true);})();";

#[allow(dead_code)]
pub struct WebViewPane {
    #[cfg(feature = "webview")]
    webview: wry::WebView,
    #[cfg(feature = "webview")]
    rx: std::sync::mpsc::Receiver<WvEvent>,
    /// Current main-frame URL (kept in sync with in-page navigation).
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub load_started: Option<Instant>,
    pub can_back: bool,
    pub can_forward: bool,
    pub visible: bool,
}

/// Logical (DPI-independent) bounds: x, y, w, h.
pub type LogicalBounds = (f64, f64, f64, f64);

impl WebViewPane {
    #[cfg(feature = "webview")]
    pub fn new(
        window: &Arc<Window>,
        url: &str,
        bounds: LogicalBounds,
        waker: Option<winit::event_loop::EventLoopProxy<()>>,
    ) -> Result<Self, String> {
        use std::sync::mpsc::channel;
        use wry::{NewWindowResponse, PageLoadEvent, WebViewBuilder};

        let (tx, rx) = channel::<WvEvent>();
        // Each handler gets its own sender; every send also wakes the event loop.
        let sender = move |tx: &std::sync::mpsc::Sender<WvEvent>, waker: &Option<winit::event_loop::EventLoopProxy<()>>, ev: WvEvent| {
            let _ = tx.send(ev);
            if let Some(w) = waker {
                let _ = w.send_event(());
            }
        };

        let (t1, w1) = (tx.clone(), waker.clone());
        let (t2, w2) = (tx.clone(), waker.clone());
        let (t3, w3) = (tx.clone(), waker.clone());
        let (t4, w4) = (tx.clone(), waker.clone());
        let (t5, w5) = (tx, waker);

        let webview = WebViewBuilder::new()
            .with_url(url)
            .with_bounds(logical_rect(bounds))
            .with_transparent(false)
            .with_devtools(true)
            .with_initialization_script(FOCUS_SCRIPT)
            .with_ipc_handler(move |_req| sender(&t5, &w5, WvEvent::Focused))
            .with_on_page_load_handler(move |ev, _url| {
                let e = match ev {
                    PageLoadEvent::Started => WvEvent::LoadStarted,
                    PageLoadEvent::Finished => WvEvent::LoadFinished,
                };
                sender(&t1, &w1, e);
            })
            .with_document_title_changed_handler(move |title| sender(&t2, &w2, WvEvent::Title(title)))
            .with_navigation_handler(move |_url| {
                sender(&t3, &w3, WvEvent::Navigated);
                true
            })
            .with_new_window_req_handler(move |url, _features| {
                sender(&t4, &w4, WvEvent::NewWindow(url));
                NewWindowResponse::Deny
            })
            .build_as_child(window.as_ref())
            .map_err(|e| format!("WebView creation failed: {e}"))?;

        log::info!("WebView created: {url} at {bounds:?}");
        Ok(Self {
            webview,
            rx,
            url: url.to_string(),
            title: String::new(),
            loading: true,
            load_started: Some(Instant::now()),
            can_back: false,
            can_forward: false,
            visible: true,
        })
    }

    #[cfg(not(feature = "webview"))]
    pub fn new(
        _window: &Arc<Window>,
        url: &str,
        _bounds: LogicalBounds,
        _waker: Option<winit::event_loop::EventLoopProxy<()>>,
    ) -> Result<Self, String> {
        let _ = url;
        Err("WebView requires --features webview".to_string())
    }

    /// Drain pending wry events and refresh derived state (URL, history flags).
    pub fn poll_events(&mut self) -> PollOutcome {
        let mut out = PollOutcome::default();
        #[cfg(feature = "webview")]
        {
            while let Ok(ev) = self.rx.try_recv() {
                out.changed = true;
                match ev {
                    WvEvent::LoadStarted => {
                        self.loading = true;
                        self.load_started = Some(Instant::now());
                    }
                    WvEvent::LoadFinished => {
                        self.loading = false;
                        self.load_started = None;
                    }
                    WvEvent::Title(t) => self.title = t,
                    WvEvent::Navigated => {}
                    WvEvent::NewWindow(url) => self.navigate(&url),
                    WvEvent::Focused => out.focus_clicked = true,
                }
            }
            // Main-frame URL (also catches pushState navigation, which fires no handler).
            if let Ok(u) = self.webview.url() {
                if !u.is_empty() && u != self.url {
                    self.url = u;
                    out.changed = true;
                }
            }
            let back = self.webview.can_go_back().unwrap_or(false);
            let fwd = self.webview.can_go_forward().unwrap_or(false);
            if back != self.can_back || fwd != self.can_forward {
                self.can_back = back;
                self.can_forward = fwd;
                out.changed = true;
            }
        }
        if self.loading {
            if self.load_started.is_some_and(|t| t.elapsed() > LOAD_TIMEOUT) {
                self.loading = false;
                self.load_started = None;
            }
            // Keep the progress animation ticking.
            out.changed = true;
        }
        out
    }

    pub fn navigate(&mut self, url: &str) {
        #[cfg(feature = "webview")]
        {
            let _ = self.webview.load_url(url);
        }
        self.url = url.to_string();
        self.title.clear();
        self.loading = true;
        self.load_started = Some(Instant::now());
    }

    pub fn reload(&mut self) {
        #[cfg(feature = "webview")]
        {
            let _ = self.webview.reload();
        }
        self.loading = true;
        self.load_started = Some(Instant::now());
    }

    pub fn stop(&mut self) {
        #[cfg(feature = "webview")]
        {
            let _ = self.webview.evaluate_script("window.stop()");
        }
        self.loading = false;
        self.load_started = None;
    }

    pub fn go_back(&self) {
        #[cfg(feature = "webview")]
        {
            let _ = self.webview.go_back();
        }
    }

    pub fn go_forward(&self) {
        #[cfg(feature = "webview")]
        {
            let _ = self.webview.go_forward();
        }
    }

    /// Give the native webview keyboard focus.
    pub fn focus(&self) {
        #[cfg(feature = "webview")]
        {
            let _ = self.webview.focus();
        }
    }

    /// Hand keyboard focus back to the winit window's view.
    pub fn focus_parent(&self) {
        #[cfg(feature = "webview")]
        {
            let _ = self.webview.focus_parent();
        }
    }

    pub fn set_bounds(&self, bounds: LogicalBounds) {
        #[cfg(feature = "webview")]
        {
            let _ = self.webview.set_bounds(logical_rect(bounds));
        }
        let _ = bounds;
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        #[cfg(feature = "webview")]
        {
            if !visible {
                // Close DevTools before hiding to avoid orphaned inspector panels
                if self.webview.is_devtools_open() {
                    self.webview.close_devtools();
                }
            }
            let _ = self.webview.set_visible(visible);
        }
    }

    #[allow(dead_code)]
    pub fn close_devtools(&self) {
        #[cfg(feature = "webview")]
        {
            if self.webview.is_devtools_open() {
                self.webview.close_devtools();
            }
        }
    }
}

#[cfg(feature = "webview")]
fn logical_rect((x, y, w, h): LogicalBounds) -> wry::Rect {
    wry::Rect {
        position: wry::dpi::LogicalPosition::new(x, y).into(),
        size: wry::dpi::LogicalSize::new(w.max(1.0), h.max(1.0)).into(),
    }
}
