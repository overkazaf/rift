use std::sync::Arc;
use winit::window::Window;

#[allow(dead_code)]
pub struct WebViewPane {
    #[cfg(feature = "webview")]
    webview: wry::WebView,
    pub url: String,
    pub visible: bool,
}

impl WebViewPane {
    #[cfg(feature = "webview")]
    pub fn new(
        window: &Arc<Window>,
        url: &str,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
    ) -> Result<Self, String> {
        use wry::{Rect, WebViewBuilder};

        let webview = WebViewBuilder::new()
            .with_url(url)
            .with_bounds(Rect {
                position: wry::dpi::LogicalPosition::new(x as f64, y as f64).into(),
                size: wry::dpi::LogicalSize::new(w as f64, h as f64).into(),
            })
            .with_transparent(false)
            .with_devtools(true)
            .build_as_child(window.as_ref())
            .map_err(|e| format!("WebView creation failed: {e}"))?;

        log::info!("WebView created: {url} at ({x},{y} {w}x{h})");
        Ok(Self {
            webview,
            url: url.to_string(),
            visible: true,
        })
    }

    #[cfg(not(feature = "webview"))]
    pub fn new(
        _window: &Arc<Window>,
        url: &str,
        _x: i32,
        _y: i32,
        _w: u32,
        _h: u32,
    ) -> Result<Self, String> {
        let _ = url;
        Err("WebView requires --features webview".to_string())
    }

    pub fn navigate(&self, url: &str) {
        #[cfg(feature = "webview")]
        {
            let _ = self.webview.load_url(url);
        }
        let _ = url;
    }

    pub fn set_bounds(&self, x: i32, y: i32, w: u32, h: u32) {
        #[cfg(feature = "webview")]
        {
            use wry::Rect;
            let _ = self.webview.set_bounds(Rect {
                position: wry::dpi::LogicalPosition::new(x as f64, y as f64).into(),
                size: wry::dpi::LogicalSize::new(w as f64, h as f64).into(),
            });
        }
        let _ = (x, y, w, h);
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

    pub fn close_devtools(&self) {
        #[cfg(feature = "webview")]
        {
            if self.webview.is_devtools_open() {
                self.webview.close_devtools();
            }
        }
    }
}
