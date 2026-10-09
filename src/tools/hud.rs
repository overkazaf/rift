use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::ui::kit::{Ctx, Rect, Tokens, Tone};

#[allow(dead_code)]
#[derive(Clone)]
pub struct HudData {
    pub user: String,
    pub host: String,
    pub shell: String,
    pub uptime: String,
    pub time: String,
    pub mem_pct: f32,
    pub mem_label: String,
    pub cpu_pct: f32,
    pub cpu_label: String,
    pub os: String,
    pub arch: String,
    pub rust_version: String,
    pub term: String,
    pub git_branch: String,
    pub cwd_short: String,
    pub pid: String,
    pub disk_label: String,
    pub disk_pct: f32,
    pub load_avg: String,
}

impl Default for HudData {
    fn default() -> Self {
        Self {
            user: String::new(), host: String::new(), shell: String::new(),
            uptime: String::new(), time: String::new(),
            mem_pct: 0.0, mem_label: String::new(),
            cpu_pct: 0.0, cpu_label: "N/A".into(),
            os: String::new(), arch: String::new(),
            rust_version: format!("rift v{}", crate::config::VERSION),
            term: String::new(), git_branch: "-".into(),
            cwd_short: String::new(), pid: String::new(),
            disk_label: String::new(), disk_pct: 0.0,
            load_avg: "N/A".into(),
        }
    }
}

pub struct Hud {
    last_update: Instant,
    update_interval: Duration,
    mem_used_mb: u64,
    mem_total_mb: u64,
    cpu_usage: f32,
    cached: HudData,
    static_inited: bool,
    cpu_hist: VecDeque<f32>,
    mem_hist: VecDeque<f32>,
    /// In-flight background refresh (system stats shell out to ps/df/git/...,
    /// which must never run on the UI thread).
    worker: Option<std::sync::mpsc::Receiver<Hud>>,
}

/// Samples kept for the sparklines (one per `update_interval`, ~80s).
const HIST_LEN: usize = 40;

impl Hud {
    pub fn new() -> Self {
        let mut hud = Self {
            last_update: Instant::now() - Duration::from_secs(10),
            update_interval: Duration::from_secs(2),
            mem_used_mb: 0,
            mem_total_mb: 0,
            cpu_usage: -1.0,
            cached: HudData::default(),
            static_inited: false,
            cpu_hist: VecDeque::with_capacity(HIST_LEN),
            mem_hist: VecDeque::with_capacity(HIST_LEN),
            worker: None,
        };
        // First sample is collected lazily on a worker thread (see `update`).
        hud.last_update = Instant::now() - Duration::from_secs(10);
        hud
    }

    /// A HUD showing fixed data and history instead of live system stats
    /// (headless screenshots). Never refreshes from the OS.
    pub fn with_data(data: HudData, cpu_hist: &[f32], mem_hist: &[f32]) -> Self {
        Self {
            last_update: Instant::now(),
            update_interval: Duration::from_secs(u64::MAX / 4),
            mem_used_mb: 0,
            mem_total_mb: 0,
            cpu_usage: data.cpu_pct,
            cached: data,
            static_inited: true,
            cpu_hist: cpu_hist.iter().copied().collect(),
            mem_hist: mem_hist.iter().copied().collect(),
            worker: None,
        }
    }

    pub fn needs_update(&self) -> bool {
        self.last_update.elapsed() >= self.update_interval
    }

    /// Start a background refresh (no-op while one is running). Results are
    /// picked up by [`Hud::poll`]; the UI thread never blocks on process forks.
    pub fn update(&mut self) {
        if self.worker.is_some() {
            return;
        }
        self.last_update = Instant::now();
        let mut snap = Hud {
            last_update: self.last_update,
            update_interval: self.update_interval,
            mem_used_mb: self.mem_used_mb,
            mem_total_mb: self.mem_total_mb,
            cpu_usage: self.cpu_usage,
            cached: self.cached.clone(),
            static_inited: self.static_inited,
            cpu_hist: self.cpu_hist.clone(),
            mem_hist: self.mem_hist.clone(),
            worker: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.worker = Some(rx);
        let spawned = std::thread::Builder::new().name("hud-sampler".into()).spawn(move || {
            snap.update_sync();
            let _ = tx.send(snap);
            crate::wake::wake();
        });
        if spawned.is_err() {
            self.worker = None;
        }
    }

    /// Adopt a finished background sample. Returns true when the data changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.worker else { return false };
        match rx.try_recv() {
            Ok(done) => {
                let last = self.last_update;
                *self = done;
                self.last_update = last;
                self.worker = None;
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.worker = None;
                false
            }
        }
    }

    /// Synchronous collection; runs on the sampler thread.
    pub(crate) fn update_sync(&mut self) {
        self.last_update = Instant::now();
        self.update_memory();
        self.update_cpu();

        // Static data: only collect once (no process forks after first call)
        if !self.static_inited {
            self.cached.user = std::env::var("USER").unwrap_or_else(|_| "?".into());
            let shell_full = std::env::var("SHELL").unwrap_or_else(|_| "?".into());
            self.cached.shell = shell_full.rsplit('/').next().unwrap_or(&shell_full).to_string();
            self.cached.host = hostname();
            self.cached.os = std::env::consts::OS.to_string();
            self.cached.arch = std::env::consts::ARCH.to_string();
            self.cached.term = std::env::var("TERM").unwrap_or_else(|_| "?".into());
            self.cached.pid = format!("{}", std::process::id());
            self.static_inited = true;
        }

        // Dynamic data: refresh every update cycle
        self.cached.time = chrono_hms();

        self.cached.mem_pct = if self.mem_total_mb > 0 {
            self.mem_used_mb as f32 / self.mem_total_mb as f32 * 100.0
        } else { 0.0 };
        self.cached.mem_label = format!("{:.1}/{:.0}G {:.0}%",
            self.mem_used_mb as f64 / 1024.0,
            self.mem_total_mb as f64 / 1024.0,
            self.cached.mem_pct);

        self.cached.cpu_pct = self.cpu_usage.max(0.0);
        self.cached.cpu_label = if self.cpu_usage >= 0.0 {
            format!("{:.0}%", self.cpu_usage)
        } else { "N/A".into() };

        for (hist, v) in [(&mut self.cpu_hist, self.cached.cpu_pct), (&mut self.mem_hist, self.cached.mem_pct)] {
            if hist.len() == HIST_LEN {
                hist.pop_front();
            }
            hist.push_back(v);
        }

        self.cached.uptime = get_uptime_str();
        self.cached.git_branch = get_git_branch();

        let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_else(|_| "?".into());
        self.cached.cwd_short = if cwd.len() > 25 {
            let parts: Vec<&str> = cwd.rsplitn(3, '/').collect();
            if parts.len() >= 2 {
                format!(".../{}", parts[..2].iter().rev().cloned().collect::<Vec<_>>().join("/"))
            } else {
                cwd[cwd.len()-25..].to_string()
            }
        } else { cwd };

        let (disk_label, disk_pct) = get_disk_usage();
        self.cached.disk_label = disk_label;
        self.cached.disk_pct = disk_pct;
        self.cached.load_avg = get_load_avg();
    }

    /// (cpu, mem) history, oldest first, percent 0..=100.
    pub fn history(&self) -> (Vec<f32>, Vec<f32>) {
        (self.cpu_hist.iter().copied().collect(), self.mem_hist.iter().copied().collect())
    }

    /// Returns cached data — no process forks, safe to call every frame.
    pub fn data(&self) -> &HudData {
        &self.cached
    }

    #[cfg(target_os = "macos")]
    fn update_memory(&mut self) {
        use std::process::Command;
        if let Ok(out) = Command::new("sysctl").arg("-n").arg("hw.memsize").output() {
            if let Ok(s) = std::str::from_utf8(&out.stdout) {
                if let Ok(bytes) = s.trim().parse::<u64>() {
                    self.mem_total_mb = bytes / (1024 * 1024);
                }
            }
        }
        if let Ok(out) = Command::new("vm_stat").output() {
            if let Ok(s) = std::str::from_utf8(&out.stdout) {
                let page_size: u64 = s.lines().next()
                    .and_then(|l| l.split("page size of ").nth(1))
                    .and_then(|r| r.split_whitespace().next())
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(16384);
                let mut active: u64 = 0;
                let mut wired: u64 = 0;
                let mut compressed: u64 = 0;
                for line in s.lines() {
                    if let Some(val) = extract_vm_stat_pages(line, "Pages active") { active = val; }
                    if let Some(val) = extract_vm_stat_pages(line, "Pages wired") { wired = val; }
                    if let Some(val) = extract_vm_stat_pages(line, "Pages occupied by compressor") { compressed = val; }
                }
                self.mem_used_mb = ((active + wired + compressed) * page_size / (1024 * 1024)).min(self.mem_total_mb);
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn update_memory(&mut self) {
        if let Ok(contents) = std::fs::read_to_string("/proc/meminfo") {
            let mut total: u64 = 0;
            let mut available: u64 = 0;
            for line in contents.lines() {
                if let Some(val) = extract_meminfo_kb(line, "MemTotal") { total = val; }
                if let Some(val) = extract_meminfo_kb(line, "MemAvailable") { available = val; }
            }
            self.mem_total_mb = total / 1024;
            self.mem_used_mb = (total - available) / 1024;
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn update_memory(&mut self) {
        self.mem_total_mb = 0;
        self.mem_used_mb = 0;
    }

    fn update_cpu(&mut self) {
        #[cfg(target_os = "macos")]
        {
            if let Ok(out) = std::process::Command::new("ps").args(["-A", "-o", "%cpu"]).output() {
                if let Ok(s) = std::str::from_utf8(&out.stdout) {
                    let total: f32 = s.lines().skip(1)
                        .filter_map(|l| l.trim().parse::<f32>().ok())
                        .sum();
                    let cores = std::thread::available_parallelism()
                        .map(|n| n.get() as f32).unwrap_or(4.0);
                    self.cpu_usage = (total / cores).min(100.0);
                }
            }
        }
        #[cfg(target_os = "linux")]
        {
            if let Ok(s) = std::fs::read_to_string("/proc/loadavg") {
                if let Some(load) = s.split_whitespace().next() {
                    if let Ok(l) = load.parse::<f32>() {
                        let cores = std::thread::available_parallelism()
                            .map(|n| n.get() as f32).unwrap_or(4.0);
                        self.cpu_usage = (l / cores * 100.0).min(100.0);
                    }
                }
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        { self.cpu_usage = -1.0; }
    }
}

pub fn get_uptime_str() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("sysctl").arg("-n").arg("kern.boottime").output() {
            if let Ok(s) = std::str::from_utf8(&out.stdout) {
                if let Some(sec_start) = s.find("sec = ") {
                    let rest = &s[sec_start + 6..];
                    if let Some(end) = rest.find(',') {
                        if let Ok(boot) = rest[..end].trim().parse::<u64>() {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default().as_secs();
                            let up = now.saturating_sub(boot);
                            let days = up / 86400;
                            let hours = (up % 86400) / 3600;
                            if days > 0 { return format!("{days}d {hours}h"); }
                            else { let mins = (up % 3600) / 60; return format!("{hours}h {mins}m"); }
                        }
                    }
                }
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/uptime") {
            if let Some(secs_str) = s.split_whitespace().next() {
                if let Ok(secs) = secs_str.parse::<f64>() {
                    let up = secs as u64;
                    let days = up / 86400;
                    let hours = (up % 86400) / 3600;
                    if days > 0 { return format!("{days}d {hours}h"); }
                    else { let mins = (up % 3600) / 60; return format!("{hours}h {mins}m"); }
                }
            }
        }
    }
    "N/A".into()
}

fn hostname() -> String {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if let Ok(out) = std::process::Command::new("hostname").arg("-s").output() {
            if out.status.success() {
                return String::from_utf8_lossy(&out.stdout).trim().to_string();
            }
        }
    }
    std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".into())
}

fn get_git_branch() -> String {
    std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .output().ok()
        .and_then(|o| if o.status.success() {
            Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else { None })
        .unwrap_or_else(|| "-".into())
}

fn get_disk_usage() -> (String, f32) {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if let Ok(out) = std::process::Command::new("df").arg("-h").arg("/").output() {
            if let Ok(s) = std::str::from_utf8(&out.stdout) {
                if let Some(line) = s.lines().nth(1) {
                    let cols: Vec<&str> = line.split_whitespace().collect();
                    if cols.len() >= 5 {
                        let pct: f32 = cols[4].trim_end_matches('%').parse().unwrap_or(0.0);
                        return (format!("{}/{}", cols[2], cols[1]), pct);
                    }
                }
            }
        }
    }
    ("?".into(), 0.0)
}

fn get_load_avg() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("sysctl").arg("-n").arg("vm.loadavg").output() {
            if let Ok(s) = std::str::from_utf8(&out.stdout) {
                let nums: Vec<&str> = s.trim().trim_matches(|c| c == '{' || c == '}')
                    .split_whitespace().take(3).collect();
                if !nums.is_empty() { return nums.join(" "); }
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/loadavg") {
            let parts: Vec<&str> = s.split_whitespace().take(3).collect();
            if !parts.is_empty() { return parts.join(" "); }
        }
    }
    "N/A".into()
}

fn chrono_hms() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

#[cfg(target_os = "macos")]
fn extract_vm_stat_pages(line: &str, prefix: &str) -> Option<u64> {
    if !line.starts_with(prefix) { return None; }
    let colon = line.find(':')?;
    line[colon + 1..].trim().trim_end_matches('.').parse().ok()
}

#[cfg(target_os = "linux")]
fn extract_meminfo_kb(line: &str, prefix: &str) -> Option<u64> {
    if !line.starts_with(prefix) { return None; }
    let colon = line.find(':')?;
    let rest = line[colon + 1..].trim();
    let num_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..num_end].parse().ok()
}

/// Tone for a utilisation percentage.
fn load_tone(pct: f32) -> Tone {
    if pct >= 85.0 { Tone::Danger } else if pct >= 60.0 { Tone::Warning } else { Tone::Accent }
}

/// Draw the HUD into the band `[y0, y0 + height)` of the buffer behind `cx`.
/// Everything comes from the UI kit tokens: thin accent lines, monospace
/// labels and sparklines for CPU / MEM history.
pub fn draw(cx: &mut Ctx, hud: &Hud, y0: usize, height: usize) {
    let tk: Tokens = *cx.tk;
    let (w, ch, cw) = (cx.w, tk.ch, tk.cw);
    let data = hud.data();
    let (cpu_hist, mem_hist) = hud.history();

    // Translucent panel + thin accent line with a faint glow below it.
    cx.fill(Rect::new(0, y0, w, height), tk.bg);
    cx.hline(0, y0, w, tk.accent);
    cx.fill_a(Rect::new(0, y0 + 1, w, 1), tk.accent, 70);

    let pad = tk.sp.md;
    let row1 = y0 + 5;
    let row2 = row1 + ch + 4;
    let row3 = row2 + ch + 4;
    let right = w.saturating_sub(pad);

    // A separator: thin vertical rule, returns the x after it.
    let sep = |cx: &mut Ctx, x: usize, y: usize| -> usize {
        cx.vline(x + cw, y + 2, ch.saturating_sub(4), tk.border_strong);
        x + 2 * cw
    };

    // ── Row 1: brand, identity, clock ──
    let mut x = pad;
    cx.text(x, row1, "\u{25C6} RIFT", tk.accent);
    x += 7 * cw;
    x = sep(cx, x, row1);
    let ident = format!("{}@{}", data.user, data.host);
    cx.text(x, row1, &ident, tk.text);
    x += (ident.chars().count()) * cw;
    x = sep(cx, x, row1);
    cx.text(x, row1, &data.shell, tk.text_muted);
    x += (data.shell.chars().count()) * cw;
    x = sep(cx, x, row1);
    cx.text(x, row1, &format!("UP {}", data.uptime), tk.text_faint);
    cx.text_right(right, row1, &data.time, tk.accent);

    // ── Row 2: CPU / MEM sparklines ──
    let spark_w = (14 * cw).min(w / 4);
    let spark_h = ch.saturating_sub(2);
    let mut x = pad;
    for (label, hist, pct, value) in [
        ("CPU", &cpu_hist, data.cpu_pct, data.cpu_label.as_str()),
        ("MEM", &mem_hist, data.mem_pct, data.mem_label.as_str()),
    ] {
        cx.text(x, row2, label, tk.text_muted);
        x += 4 * cw;
        let tone = load_tone(pct);
        cx.sparkline(Rect::new(x, row2 + 1, spark_w, spark_h), hist, 100.0, tk.tone(tone));
        x += spark_w + cw;
        cx.text(x, row2, value, tk.tone(tone));
        x += (value.chars().count()) * cw;
        x = sep(cx, x, row2);
    }

    // ── Row 3: environment ──
    let mut x = pad;
    let os = format!("{}/{}", data.os, data.arch);
    cx.text(x, row3, &os, tk.text_faint);
    x += (os.chars().count()) * cw;
    x = sep(cx, x, row3);
    let git = format!("\u{2387} {}", data.git_branch);
    cx.text(x, row3, &git, if data.git_branch == "-" { tk.text_faint } else { tk.success });
    x += (git.chars().count()) * cw;
    x = sep(cx, x, row3);
    cx.text(x, row3, &data.cwd_short, tk.text);
    x += (data.cwd_short.chars().count()) * cw;
    x = sep(cx, x, row3);
    cx.text(x, row3, "DSK", tk.text_muted);
    x += 4 * cw;
    let bar_w = 8 * cw;
    cx.progress(Rect::new(x, row3, bar_w, ch), (data.disk_pct / 100.0).clamp(0.0, 1.0), load_tone(data.disk_pct));
    x += bar_w + cw;
    cx.text(x, row3, &data.disk_label, tk.text);
    x += (data.disk_label.chars().count()) * cw;
    x = sep(cx, x, row3);
    let load = format!("LOAD {}", data.load_avg);
    cx.text(x, row3, &load, tk.text_faint);
    x += load.chars().count() * cw;
    // The right-aligned PID / version tag is dropped when the left side
    // already reaches it (narrow windows or large fonts).
    let tag = format!("PID {} | {}", data.pid, data.rust_version);
    if x + 2 * cw <= right.saturating_sub(tag.chars().count() * cw) {
        cx.text_right(right, row3, &tag, tk.text_faint);
    }
}

#[cfg(test)]
mod qa_tests {
    use super::*;
    use crate::ui::kit::gallery::qa::each_theme;

    #[test]
    fn hud_renders_in_every_theme_without_hardcoded_palette() {
        let mut hud = Hud::new();
        for _ in 0..5 {
            hud.update();
        }
        each_theme("hud", |b, w, h, f, t| {
            let tk = Tokens::new(t, f.cell_width, f.cell_height);
            let bar_h = tk.ch * 3 + 20;
            let mut cx = Ctx::new(b, w, h, f, &tk);
            draw(&mut cx, &hud, h - bar_h, bar_h);
            // the accent rule sits on the first row of the band
            assert_eq!(cx.buf[(h - bar_h) * w + 3], pack_tk(tk.accent));
        });
    }

    fn pack_tk(c: crate::config::Rgb) -> u32 {
        ((c.0 as u32) << 16) | ((c.1 as u32) << 8) | c.2 as u32
    }

    #[test]
    fn history_is_bounded() {
        let mut hud = Hud::new();
        for _ in 0..(HIST_LEN + 10) {
            hud.update_sync(); // `update()` samples on a worker thread; the test wants determinism
        }
        let (c, m) = hud.history();
        assert_eq!((c.len(), m.len()), (HIST_LEN, HIST_LEN));
    }
}
