//! Adversarial-review harness (test-only). Every test prints machine-greppable
//! lines `AUDIT|<id>|PASS|FAIL|INFO|<message>` (run with `--nocapture`).
//! Nothing here changes product code.
#![allow(dead_code)]

use crate::window::Pane;

mod shell_real;
mod render;
mod perf;
mod splits;
mod blocks;
mod textops;
mod ai_net;
mod safety;
mod escapes;
mod robust;
mod cfg;

pub struct Soft {
    pub area: &'static str,
    pub fails: Vec<String>,
}

impl Soft {
    pub fn new(area: &'static str) -> Self {
        Soft { area, fails: Vec::new() }
    }
    pub fn check(&mut self, id: &str, ok: bool, msg: impl AsRef<str>) {
        let msg = msg.as_ref().replace('\n', "\\n").replace('\x1b', "\\e");
        println!("AUDIT|{}|{}|{}|{}", self.area, id, if ok { "PASS" } else { "FAIL" }, msg);
        if !ok {
            self.fails.push(format!("{id}: {msg}"));
        }
    }
    pub fn info(&self, id: &str, msg: impl AsRef<str>) {
        let msg = msg.as_ref().replace('\n', "\\n").replace('\x1b', "\\e");
        println!("AUDIT|{}|{}|INFO|{}", self.area, id, msg);
    }
    /// Panics when any check failed (so `cargo test` reports the test as failed).
    pub fn finish(self) {
        assert!(self.fails.is_empty(), "{} failed checks:\n{}", self.fails.len(), self.fails.join("\n"));
    }
}

/// Scripted pane fed through the real VT parser (+ kitty APC scanner).
pub fn pane(cols: usize, rows: usize) -> Pane {
    Pane::scripted(0, cols, rows)
}

pub fn pane_id(id: usize, cols: usize, rows: usize) -> Pane {
    Pane::scripted(id, cols, rows)
}

/// HOME if it is a disposable sandbox (path contains "rift-audit"), else None.
/// Tests that write config/session files skip themselves outside a sandbox so a
/// plain `cargo test` never touches the user's real ~/.config.
/// Run them with: `HOME=$(mktemp -d -t rift-audit) cargo test --bin rift audit::`
pub fn sandbox_home(area: &str) -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    if home.contains("rift-audit") {
        Some(home)
    } else {
        println!("AUDIT|{area}|sandbox|INFO|skipped: HOME is not a rift-audit sandbox");
        None
    }
}

pub fn scratch_dir(name: &str) -> std::path::PathBuf {
    let base = std::env::var("AUDIT_OUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("rift-audit-out"));
    let d = base.join(name);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Plain text of one grid row (wide-char continuation cells skipped).
pub fn row_text(cells: &[crate::terminal::Cell]) -> String {
    crate::terminal::grid::cells_text(cells).trim_end().to_string()
}

pub fn screen_text(t: &crate::terminal::Terminal) -> String {
    t.grid.iter().map(|r| row_text(r)).collect::<Vec<_>>().join("\n")
}

/// Minimal PNG writer (stored via flate2) for ARGB-packed 0x00RRGGBB buffers.
pub fn write_png(path: &std::path::Path, w: usize, h: usize, buf: &[u32]) {
    use std::io::Write;
    let mut raw = Vec::with_capacity((w * 3 + 1) * h);
    for y in 0..h {
        raw.push(0u8);
        for x in 0..w {
            let p = buf[y * w + x];
            raw.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    z.write_all(&raw).unwrap();
    let idat = z.finish().unwrap();
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = ty.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc(&body).to_be_bytes());
    }
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    std::fs::write(path, out).unwrap();
}
