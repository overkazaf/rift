//! Example rterm WASM plugin.
//!
//! Build with:
//!   rustup target add wasm32-unknown-unknown  # one-time setup
//!   cd examples/hello_plugin
//!   cargo build --target wasm32-unknown-unknown --release
//!
//! The .wasm file will be at:
//!   target/wasm32-unknown-unknown/release/hello_plugin.wasm

// Host functions provided by rterm (imported from "rterm" module)
extern "C" {
    fn term_cols() -> i32;
    fn term_rows() -> i32;
    fn term_cursor_row() -> i32;
    fn term_cursor_col() -> i32;
    fn term_write(ptr: *const u8, len: i32);
    fn term_log(ptr: *const u8, len: i32);
}

fn log(msg: &str) {
    unsafe { term_log(msg.as_ptr(), msg.len() as i32); }
}

fn write_pty(data: &[u8]) {
    unsafe { term_write(data.as_ptr(), data.len() as i32); }
}

/// Called once when the plugin is loaded. Return 0 for success.
#[no_mangle]
pub extern "C" fn init() -> i32 {
    let cols = unsafe { term_cols() };
    let rows = unsafe { term_rows() };
    log(&format!("Hello from WASM plugin! Terminal is {cols}x{rows}"));
    0
}

/// Called when the terminal receives output from the PTY.
/// `ptr` and `len` describe a byte slice in WASM linear memory
/// (allocated by the host via our exported `malloc`).
#[no_mangle]
pub extern "C" fn on_output(ptr: *const u8, len: i32) {
    let data = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    // Example: detect "error" in output and log it
    if let Ok(text) = std::str::from_utf8(data) {
        if text.to_lowercase().contains("error") {
            log(&format!("Detected error in output: {}", text.trim()));
        }
    }
}

/// Called when the user presses a key (before it's sent to PTY).
#[no_mangle]
pub extern "C" fn on_key(ptr: *const u8, len: i32) {
    let _data = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    // Example: could intercept specific key sequences here
}

/// Returns a pointer to a null-terminated plugin name string.
#[no_mangle]
pub extern "C" fn plugin_name() -> *const u8 {
    b"hello-plugin\0".as_ptr()
}

/// Returns a pointer to a null-terminated version string.
#[no_mangle]
pub extern "C" fn plugin_version() -> *const u8 {
    b"0.1.0\0".as_ptr()
}

// ── Memory allocator exports (required by host to pass data) ──

#[no_mangle]
pub extern "C" fn malloc(size: i32) -> *mut u8 {
    let layout = std::alloc::Layout::from_size_align(size as usize, 1).unwrap();
    unsafe { std::alloc::alloc(layout) }
}

#[no_mangle]
pub extern "C" fn free(ptr: *mut u8, size: i32) {
    let layout = std::alloc::Layout::from_size_align(size as usize, 1).unwrap();
    unsafe { std::alloc::dealloc(ptr, layout) }
}
