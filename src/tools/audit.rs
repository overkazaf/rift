#[allow(dead_code)]

use std::io::Write;
use std::path::PathBuf;

pub struct AuditLog {
    pub enabled: bool,
    file: Option<std::fs::File>,
    path: PathBuf,
}

impl AuditLog {
    pub fn new() -> Self {
        let path = dirs::home_dir()
            .unwrap_or_default()
            .join(".config")
            .join("rift")
            .join("audit.log");
        Self { enabled: false, file: None, path }
    }

    pub fn enable(&mut self) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(f) => {
                self.file = Some(f);
                self.enabled = true;
                log::info!("Audit log enabled: {}", self.path.display());
            }
            Err(e) => log::error!("Audit log failed: {e}"),
        }
    }

    pub fn disable(&mut self) {
        self.enabled = false;
        self.file = None;
    }

    pub fn toggle(&mut self) {
        if self.enabled { self.disable(); } else { self.enable(); }
    }

    pub fn log_command(&mut self, command: &str, pane_id: usize) {
        if !self.enabled { return; }
        if let Some(ref mut f) = self.file {
            let ts = unix_ts();
            let user = std::env::var("USER").unwrap_or_default();
            let entry = format!("[{}] user={} pane={} cmd={}\n", ts, user, pane_id, command.trim());
            let _ = f.write_all(entry.as_bytes());
            let _ = f.flush();
        }
    }

    pub fn log_output(&mut self, output: &[u8], pane_id: usize) {
        if !self.enabled { return; }
        if let Some(ref mut f) = self.file {
            let ts = unix_ts();
            let text = String::from_utf8_lossy(output);
            let first_line = text.lines().next().unwrap_or("");
            if !first_line.trim().is_empty() {
                let entry = format!("[{}] pane={} out={}\n", ts, pane_id, first_line.trim());
                let _ = f.write_all(entry.as_bytes());
            }
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

fn unix_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
