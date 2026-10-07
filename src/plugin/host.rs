use super::PluginState;
use wasmtime::{Caller, Linker};

pub fn register(linker: &mut Linker<PluginState>) -> wasmtime::Result<()> {
    linker.func_wrap("rift", "term_cols", |caller: Caller<'_, PluginState>| -> i32 {
        caller.data().term_cols
    })?;

    linker.func_wrap("rift", "term_rows", |caller: Caller<'_, PluginState>| -> i32 {
        caller.data().term_rows
    })?;

    linker.func_wrap("rift", "term_cursor_row", |caller: Caller<'_, PluginState>| -> i32 {
        caller.data().cursor_row
    })?;

    linker.func_wrap("rift", "term_cursor_col", |caller: Caller<'_, PluginState>| -> i32 {
        caller.data().cursor_col
    })?;

    linker.func_wrap("rift", "term_write", |mut caller: Caller<'_, PluginState>, ptr: i32, len: i32| {
        if let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) {
            let start = ptr as usize;
            let end = start + len as usize;
            let data = memory.data(&caller);
            if end <= data.len() {
                let bytes = data[start..end].to_vec();
                caller.data_mut().output_buffer.extend_from_slice(&bytes);
            }
        }
    })?;

    linker.func_wrap("rift", "term_log", |mut caller: Caller<'_, PluginState>, ptr: i32, len: i32| {
        if let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) {
            let start = ptr as usize;
            let end = start + len as usize;
            let data = memory.data(&caller);
            if end <= data.len() {
                let msg = String::from_utf8_lossy(&data[start..end]).into_owned();
                caller.data_mut().log_buffer.push(msg);
            }
        }
    })?;

    Ok(())
}
