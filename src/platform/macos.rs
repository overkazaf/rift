#[allow(deprecated)]
use cocoa::appkit::NSWindow;
#[allow(deprecated)]
use cocoa::base::{id, nil};
use winit::raw_window_handle::HasWindowHandle;

/// Apply native macOS window transparency.
#[allow(deprecated)]
pub fn apply_transparency(window: &winit::window::Window, opacity: f64) {
    let Ok(handle) = window.window_handle() else { return };
    let winit::raw_window_handle::RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return };

    unsafe {
        let ns_view: id = appkit.ns_view.as_ptr() as id;
        #[allow(clippy::let_unit_value)]
        let ns_window: id = objc::msg_send![ns_view, window];
        if ns_window.is_null() { return; }

        NSWindow::setAlphaValue_(ns_window, opacity);
        NSWindow::setOpaque_(ns_window, cocoa::base::NO);

        let clear: id = cocoa::appkit::NSColor::colorWithSRGBRed_green_blue_alpha_(nil, 0.0, 0.0, 0.0, 0.0);
        NSWindow::setBackgroundColor_(ns_window, clear);

        log::info!("macOS: window transparency set to {:.0}%", opacity * 100.0);
    }
}

/// Set (or clear, with 0) the number shown on the Dock icon.
#[allow(deprecated)]
pub fn set_dock_badge(count: usize) {
    use cocoa::appkit::NSApp;
    use cocoa::foundation::NSString;
    unsafe {
        let app: id = NSApp();
        if app.is_null() {
            return;
        }
        let tile: id = objc::msg_send![app, dockTile];
        if tile.is_null() {
            return;
        }
        let label: id = if count == 0 {
            nil
        } else {
            NSString::alloc(nil).init_str(&count.to_string())
        };
        let _: () = objc::msg_send![tile, setBadgeLabel: label];
    }
}
