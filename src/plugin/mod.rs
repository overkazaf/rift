#[cfg(feature = "plugins")]
mod host;

use std::path::PathBuf;

pub struct PluginMeta {
    pub name: String,
    pub version: String,
}

#[derive(Debug)]
pub enum PluginError {
    LoadFailed(String),
    InitFailed(String),
    RuntimeError(String),
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoadFailed(e) => write!(f, "plugin load failed: {e}"),
            Self::InitFailed(e) => write!(f, "plugin init failed: {e}"),
            Self::RuntimeError(e) => write!(f, "plugin runtime error: {e}"),
        }
    }
}

// ── Feature-gated implementation ──

#[cfg(feature = "plugins")]
pub(crate) struct PluginState {
    pub output_buffer: Vec<u8>,
    pub log_buffer: Vec<String>,
    pub term_cols: i32,
    pub term_rows: i32,
    pub cursor_row: i32,
    pub cursor_col: i32,
}

#[cfg(feature = "plugins")]
impl Default for PluginState {
    fn default() -> Self {
        Self {
            output_buffer: Vec::new(),
            log_buffer: Vec::new(),
            term_cols: 80,
            term_rows: 24,
            cursor_row: 0,
            cursor_col: 0,
        }
    }
}

#[cfg(feature = "plugins")]
struct LoadedPlugin {
    meta: PluginMeta,
    store: wasmtime::Store<PluginState>,
    instance: wasmtime::Instance,
}

#[cfg(not(feature = "plugins"))]
struct LoadedPlugin {
    meta: PluginMeta,
    #[allow(dead_code)]
    path: PathBuf,
}

pub struct PluginManager {
    #[cfg(feature = "plugins")]
    engine: wasmtime::Engine,
    plugins: Vec<LoadedPlugin>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "plugins")]
            engine: wasmtime::Engine::default(),
            plugins: Vec::new(),
        }
    }

    pub fn list(&self) -> Vec<&PluginMeta> {
        self.plugins.iter().map(|p| &p.meta).collect()
    }

    pub fn plugin_count(&self) -> usize {
        self.plugins.len()
    }

    #[cfg(not(feature = "plugins"))]
    pub fn load(&mut self, path: PathBuf) -> Result<(), PluginError> {
        let _ = path;
        Err(PluginError::LoadFailed(
            "plugin support not compiled (enable with --features plugins)".into(),
        ))
    }

    #[cfg(feature = "plugins")]
    pub fn load(&mut self, path: PathBuf) -> Result<(), PluginError> {
        use wasmtime::*;

        let module = Module::from_file(&self.engine, &path)
            .map_err(|e| PluginError::LoadFailed(format!("{}: {e}", path.display())))?;

        let mut store = Store::new(&self.engine, PluginState::default());
        let mut linker = Linker::new(&self.engine);

        host::register(&mut linker)
            .map_err(|e| PluginError::LoadFailed(format!("host registration: {e}")))?;

        let instance = linker.instantiate(&mut store, &module)
            .map_err(|e| PluginError::LoadFailed(format!("instantiation: {e}")))?;

        // Call init() if exported
        if let Ok(init) = instance.get_typed_func::<(), i32>(&mut store, "init") {
            let rc = init.call(&mut store, ())
                .map_err(|e| PluginError::InitFailed(e.to_string()))?;
            if rc != 0 {
                return Err(PluginError::InitFailed(format!("init() returned {rc}")));
            }
        }

        // Flush any logs from init
        for msg in store.data().log_buffer.iter() {
            log::info!("[plugin] {msg}");
        }
        store.data_mut().log_buffer.clear();

        // Read plugin name/version from exports (optional)
        let name = read_plugin_string(&instance, &mut store, "plugin_name")
            .unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().into()).unwrap_or_default());
        let version = read_plugin_string(&instance, &mut store, "plugin_version")
            .unwrap_or_else(|| "0.0.0".into());

        log::info!("Loaded plugin: {name} v{version} ({})", path.display());

        self.plugins.push(LoadedPlugin {
            meta: PluginMeta { name, version },
            store,
            instance,
        });

        Ok(())
    }

    pub fn update_term_state(&mut self, cols: usize, rows: usize, cursor_row: usize, cursor_col: usize) {
        #[cfg(feature = "plugins")]
        for plugin in &mut self.plugins {
            let state = plugin.store.data_mut();
            state.term_cols = cols as i32;
            state.term_rows = rows as i32;
            state.cursor_row = cursor_row as i32;
            state.cursor_col = cursor_col as i32;
        }
        #[cfg(not(feature = "plugins"))]
        { let _ = (cols, rows, cursor_row, cursor_col); }
    }

    pub fn on_output(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        #[allow(unused_mut)]
        let responses = Vec::new();
        #[cfg(feature = "plugins")]
        for plugin in &mut self.plugins {
            if let Some(pty_data) = call_data_hook(plugin, "on_output", data) {
                if !pty_data.is_empty() {
                    responses.push(pty_data);
                }
            }
        }
        #[cfg(not(feature = "plugins"))]
        { let _ = data; }
        responses
    }

    pub fn on_key(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        #[allow(unused_mut)]
        let responses = Vec::new();
        #[cfg(feature = "plugins")]
        for plugin in &mut self.plugins {
            if let Some(pty_data) = call_data_hook(plugin, "on_key", data) {
                if !pty_data.is_empty() {
                    responses.push(pty_data);
                }
            }
        }
        #[cfg(not(feature = "plugins"))]
        { let _ = data; }
        responses
    }
}

// ── Plugin calling helpers (feature-gated) ──

#[cfg(feature = "plugins")]
fn call_data_hook(plugin: &mut LoadedPlugin, func_name: &str, data: &[u8]) -> Option<Vec<u8>> {
    let hook = plugin.instance
        .get_typed_func::<(i32, i32), ()>(&mut plugin.store, func_name)
        .ok()?;

    // Allocate guest memory via exported malloc
    let malloc = plugin.instance
        .get_typed_func::<i32, i32>(&mut plugin.store, "malloc")
        .ok()?;

    let len = data.len() as i32;
    let ptr = malloc.call(&mut plugin.store, len).ok()?;
    if ptr == 0 { return None; }

    // Write data to guest memory
    let memory = plugin.instance.get_memory(&mut plugin.store, "memory")?;
    memory.data_mut(&mut plugin.store)[ptr as usize..ptr as usize + data.len()]
        .copy_from_slice(data);

    // Call the hook
    if let Err(e) = hook.call(&mut plugin.store, (ptr, len)) {
        log::warn!("[plugin] {func_name} error: {e}");
    }

    // Free guest memory
    if let Ok(free) = plugin.instance.get_typed_func::<(i32, i32), ()>(&mut plugin.store, "free") {
        let _ = free.call(&mut plugin.store, (ptr, len));
    }

    // Flush logs
    for msg in plugin.store.data().log_buffer.iter() {
        log::info!("[plugin] {msg}");
    }
    plugin.store.data_mut().log_buffer.clear();

    // Drain output buffer
    let output = std::mem::take(&mut plugin.store.data_mut().output_buffer);
    Some(output)
}

#[cfg(feature = "plugins")]
fn read_plugin_string(
    instance: &wasmtime::Instance,
    store: &mut wasmtime::Store<PluginState>,
    func_name: &str,
) -> Option<String> {
    let func = instance.get_typed_func::<(), i32>(&mut *store, func_name).ok()?;
    let ptr = func.call(&mut *store, ()).ok()?;
    if ptr == 0 { return None; }

    let memory = instance.get_memory(&mut *store, "memory")?;
    let data = memory.data(&*store);
    let start = ptr as usize;
    let end = data[start..].iter().position(|&b| b == 0).map(|i| start + i).unwrap_or(start);
    String::from_utf8(data[start..end].to_vec()).ok()
}
