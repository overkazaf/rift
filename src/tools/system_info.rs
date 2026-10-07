pub struct SystemInfo {
    pub visible: bool,
    info: Vec<(String, String)>,
}

impl SystemInfo {
    pub fn new() -> Self { Self { visible: false, info: Vec::new() } }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible { self.collect(); }
    }

    fn collect(&mut self) {
        self.info.clear();
        self.info.push(("OS".into(), format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)));
        self.info.push(("Host".into(), run_cmd("hostname", &["-s"]).unwrap_or("?".into())));
        self.info.push(("User".into(), std::env::var("USER").unwrap_or("?".into())));
        self.info.push(("Shell".into(), std::env::var("SHELL").unwrap_or("?".into())));
        self.info.push(("Terminal".into(), format!("rift v{}", crate::config::VERSION)));

        #[cfg(target_os = "macos")]
        {
            if let Some(cpu) = run_cmd("sysctl", &["-n", "machdep.cpu.brand_string"]) {
                self.info.push(("CPU".into(), cpu.trim().to_string()));
            }
            if let Some(mem) = run_cmd("sysctl", &["-n", "hw.memsize"]) {
                if let Ok(bytes) = mem.trim().parse::<u64>() {
                    self.info.push(("Memory".into(), format!("{:.0} GB", bytes as f64 / 1073741824.0)));
                }
            }
        }

        #[cfg(target_os = "linux")]
        {
            if let Some(cpu) = run_cmd("grep", &["-m1", "model name", "/proc/cpuinfo"]) {
                if let Some(name) = cpu.split(':').nth(1) {
                    self.info.push(("CPU".into(), name.trim().to_string()));
                }
            }
            if let Ok(mem) = std::fs::read_to_string("/proc/meminfo") {
                if let Some(line) = mem.lines().next() {
                    if let Some(kb_str) = line.split_whitespace().nth(1) {
                        if let Ok(kb) = kb_str.parse::<u64>() {
                            self.info.push(("Memory".into(), format!("{:.0} GB", kb as f64 / 1048576.0)));
                        }
                    }
                }
            }
        }

        self.info.push(("Uptime".into(), crate::tools::hud::get_uptime_str()));

        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
        self.info.push(("Cores".into(), format!("{}", cores)));

        if let Some(home) = dirs::home_dir() {
            self.info.push(("Home".into(), home.display().to_string()));
        }

        if let Ok(cwd) = std::env::current_dir() {
            self.info.push(("CWD".into(), cwd.display().to_string()));
        }

        self.info.push(("TERM".into(), std::env::var("TERM").unwrap_or("?".into())));
    }

    pub fn handle_key(&mut self, key: SysInfoKey) {
        match key {
            SysInfoKey::Escape => self.visible = false,
        }
    }

    pub fn render(
        &self, buffer: &mut [u32], width: usize, height: usize,
        font: &mut crate::renderer::font::FontManager, theme: &crate::config::Theme,
    ) {
        if !self.visible { return; }
        crate::ui::dim_backdrop(buffer, 3);
        let cw = font.cell_width;
        let ch = font.cell_height;
        let pw = (40 * cw).min(width - 40);
        let ph = ((self.info.len() + 4) * (ch + 4) + 40).min(height - 40);
        let px = (width - pw) / 2;
        let py = (height - ph) / 2;

        crate::ui::fill_rect(buffer, width, px, py, pw, ph, crate::ui::pack_rgb(crate::ui::lighten(theme.bg, 6)));
        crate::ui::draw_border(buffer, width, px, py, pw, ph, crate::ui::pack_rgb(crate::ui::dim(theme.cursor, 0.4)));

        let mut ty = py + 12;
        crate::ui::render_text(buffer, width, font, "System Info", px + 16, ty, theme.cursor);
        ty += ch + 8;

        let label_w = 12 * cw;
        for (key, val) in &self.info {
            if ty + ch >= py + ph - 20 { break; }
            crate::ui::render_text(buffer, width, font, key, px + 16, ty, (137, 180, 250));
            crate::ui::render_text(buffer, width, font, crate::ui::trunc(val, (pw - label_w - 32) / cw), px + 16 + label_w, ty, crate::ui::dim(theme.fg, 0.7));
            ty += ch + 4;
        }

        crate::ui::render_text(buffer, width, font, "Esc: close", px + 16, py + ph - ch - 10, crate::ui::dim(theme.fg, 0.3));
    }
}

pub enum SysInfoKey { Escape }

fn run_cmd(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    if out.status.success() { Some(String::from_utf8_lossy(&out.stdout).trim().to_string()) }
    else { None }
}
