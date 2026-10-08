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
        use crate::ui::kit::{Column, Ctx, PanelSpec, TableRow, Tokens, Tone, Width};
        if !self.visible { return; }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(tk.backdrop);

        let want = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + (self.entries.len().max(3) + 1) * tk.row_h;
        let rect = cx.centered_cols(64, want.min(height * 3 / 4));
        let count = format!("{} processes", self.entries.len());
        let spec = PanelSpec::new("Process Tree")
            .badge(&count, Tone::Neutral)
            .hints(&[("r", "refresh"), ("Esc", "close")]);
        let body = cx.panel(rect, &spec);
        if self.entries.is_empty() {
            cx.empty_state(body, "No child processes", "");
            return;
        }
        let rows: Vec<TableRow> = self.entries.iter().map(|p| {
            TableRow::new(vec![
                format!("{}{}", "  ".repeat(p.depth), p.name),
                p.pid.to_string(), format!("{}%", p.cpu), format!("{}%", p.mem),
            ])
        }).collect();
        cx.table(body, &[
            Column::new("Process", Width::Flex(1)), Column::new("PID", Width::Cols(7)).right(),
            Column::new("CPU", Width::Cols(6)).right(), Column::new("Mem", Width::Cols(6)).right(),
        ], &rows, None, 0);
    }
}

pub enum ProcTreeKey { Escape, Char(char) }
