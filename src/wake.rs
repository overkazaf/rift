//! Process-wide event-loop waker. Background threads (AI requests, menu
//! callbacks, ...) call [`wake`] after publishing a result so the UI thread can
//! sleep indefinitely while idle instead of polling every few milliseconds.

use std::sync::Mutex;
use winit::event_loop::EventLoopProxy;

static PROXY: Mutex<Option<EventLoopProxy<()>>> = Mutex::new(None);

/// Register the event loop's proxy (call once from `main`).
pub fn init(proxy: EventLoopProxy<()>) {
    if let Ok(mut g) = PROXY.lock() {
        *g = Some(proxy);
    }
}

/// Wake the event loop (no-op before [`init`] / in headless mode).
pub fn wake() {
    if let Ok(g) = PROXY.lock() {
        if let Some(p) = g.as_ref() {
            let _ = p.send_event(());
        }
    }
}
