pub struct ProcessTree {
    pub visible: bool,
    entries: Vec<ProcessInfo>,
}

struct ProcessInfo {
    pid: u32,
    #[allow(dead_code)]
    ppid: u32,
    name: String,
    cpu: String,
    mem: String,
    depth: usize,
}

impl ProcessTree {
    pub fn new() -> Self { Self { visible: false, entries: Vec::new() } }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible { self.refresh(); }
    }

    pub fn refresh(&mut self) {
        self.entries.clear();
        let shell_pid = std::process::id();
        let output = std::process::Command::new("ps")
            .args(["-o", "pid,ppid,comm,%cpu,%mem", "-g", &shell_pid.to_string()])
            .output()
            .or_else(|_| {
                std::process::Command::new("ps")
                    .args(["--ppid", &shell_pid.to_string(), "-o", "pid,ppid,comm,%cpu,%mem", "--no-headers"])
                    .output()
            });

        if let Ok(out) = output {
            if let Ok(text) = std::str::from_utf8(&out.stdout) {
                for line in text.lines() {
                    let cols: Vec<&str> = line.split_whitespace().collect();
                    if cols.len() >= 3 {
                        if let Ok(pid) = cols[0].parse::<u32>() {
                            self.entries.push(ProcessInfo {
                                pid,
                                ppid: cols[1].parse().unwrap_or(0),
                                name: cols[2].to_string(),
                                cpu: cols.get(3).unwrap_or(&"0").to_string(),
                                mem: cols.get(4).unwrap_or(&"0").to_string(),
                                depth: 0,
                            });
                        }
                    }
                }
            }
        }
    }

    pub fn handle_key(&mut self, key: ProcTreeKey) {
        match key {
            ProcTreeKey::Escape => self.visible = false,
            ProcTreeKey::Char('r') => self.refresh(),
            _ => {}
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
        let pw = (45 * cw).min(width - 40);
        let ph = ((self.entries.len() + 4) * (ch + 4) + 40).min(height - 40).max(ch * 6);
        let px = (width - pw) / 2;
        let py = (height - ph) / 2;

        crate::ui::fill_rect(buffer, width, px, py, pw, ph, crate::ui::pack_rgb(crate::ui::lighten(theme.bg, 6)));
        crate::ui::draw_border(buffer, width, px, py, pw, ph, crate::ui::pack_rgb(crate::ui::dim(theme.cursor, 0.4)));

        let mut ty = py + 12;
        crate::ui::render_text(buffer, width, font, "Process Tree", px + 16, ty, theme.cursor);
        ty += ch + 8;

        for proc in &self.entries {
            if ty + ch >= py + ph - 20 { break; }
            let indent = "  ".repeat(proc.depth);
            let line = format!("{}{} [{}] cpu:{} mem:{}", indent, proc.name, proc.pid, proc.cpu, proc.mem);
            crate::ui::render_text(buffer, width, font, crate::ui::trunc(&line, (pw - 32) / cw), px + 16, ty, crate::ui::dim(theme.fg, 0.7));
            ty += ch + 4;
        }

        crate::ui::render_text(buffer, width, font, "r: refresh  Esc: close", px + 16, py + ph - ch - 10, crate::ui::dim(theme.fg, 0.3));
    }
}

pub enum ProcTreeKey { Escape, Char(char) }
