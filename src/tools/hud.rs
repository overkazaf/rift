use std::time::{Duration, Instant};

#[allow(dead_code)]
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
            rust_version: "rift v0.3.0".into(),
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
}

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
        };
        hud.update();
        hud
    }

    pub fn needs_update(&self) -> bool {
        self.last_update.elapsed() >= self.update_interval
    }

    pub fn update(&mut self) {
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

    /// Returns cached data — no process forks, safe to call every frame.
    pub fn data(&self) -> &HudData {
        &self.cached
    }

    #[allow(dead_code)]
    pub fn render_pixels(&self, _buffer: &mut [u32], _buf_width: u32, _buf_height: u32, _cell_height: usize) {
        // Rendering is done in lifecycle.rs using data()
    }

    #[allow(dead_code)]
    pub fn status_segments(&self) -> Vec<(String, u32)> {
        let d = &self.cached;
        let accent = 0x5D_E4_A7u32;
        let blue = 0x89_B4_FAu32;
        let yellow = 0xF9_E2_AFu32;
        let cyan = 0x94_E2_D5u32;
        let pink = 0xF5_C2_E7u32;
        let dim = 0x6C_70_86u32;
        let mem_pct = d.mem_pct as u32;
        let mem_color = if mem_pct > 85 { 0xF3_8B_A8u32 } else if mem_pct > 60 { yellow } else { accent };
        let cpu_bar = progress_bar(d.cpu_pct as u32, 8);
        let mem_bar = progress_bar(mem_pct, 8);

        vec![
            (" \u{25C6} ".into(), accent),
            (format!("{}@{}", d.user, d.host), cyan),
            (" \u{2502} ".into(), dim),
            ("\u{25B2} ".into(), blue),
            (mem_bar, mem_color),
            (format!(" {}", d.mem_label), yellow),
            (" \u{2502} ".into(), dim),
            ("\u{25CF} ".into(), blue),
            (cpu_bar, if d.cpu_pct > 80.0 { 0xF3_8B_A8 } else { accent }),
            (format!(" {}", d.cpu_label), yellow),
            (" \u{2502} ".into(), dim),
            ("\u{25B7} ".into(), pink),
            (d.shell.clone(), pink),
            (" \u{2502} ".into(), dim),
            ("\u{21E1} ".into(), dim),
            (d.uptime.clone(), dim),
            (" \u{2502} ".into(), dim),
            (d.time.clone(), blue),
            (" \u{25C6}".into(), accent),
        ]
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
                let page_size: u64 = 16384;
                let mut active: u64 = 0;
                let mut wired: u64 = 0;
                let mut compressed: u64 = 0;
                for line in s.lines() {
                    if let Some(val) = extract_vm_stat_pages(line, "Pages active") { active = val; }
                    if let Some(val) = extract_vm_stat_pages(line, "Pages wired") { wired = val; }
                    if let Some(val) = extract_vm_stat_pages(line, "Pages occupied by compressor") { compressed = val; }
                }
                self.mem_used_mb = (active + wired + compressed) * page_size / (1024 * 1024);
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

#[allow(dead_code)]
fn progress_bar(pct: u32, width: usize) -> String {
    let filled = (pct as usize * width / 100).min(width);
    let empty = width - filled;
    let mut bar = String::with_capacity(width);
    for _ in 0..filled { bar.push('\u{2588}'); }
    for _ in 0..empty { bar.push('\u{2591}'); }
    bar
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
