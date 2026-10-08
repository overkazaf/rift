/// Port/Service Dashboard — lists every listening TCP/UDP port on the
/// machine together with its owning process, and lets the user kill it.
pub struct PortDashboard {
    pub visible: bool,
    entries: Vec<PortEntry>,
    selected: usize,
    sort_by: SortField,
}

struct PortEntry {
    port: u16,
    proto: String,
    pid: u32,
    process_name: String,
    /// LISTEN / ESTABLISHED / CLOSE_WAIT / TIME_WAIT / "" (UDP has no state).
    state: String,
    local_addr: String,
    remote_addr: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SortField {
    Port,
    Process,
    State,
}

// Column widths in character cells — kept fixed so every row lines up like
// `font-variant-numeric: tabular-nums` would for a proportional font.
const COL_PORT: usize = 6;
const COL_PROTO: usize = 5;
const COL_PID: usize = 7;
const COL_PROCESS: usize = 16;
const COL_STATE: usize = 11;
const COL_LOCAL: usize = 21;
const COL_REMOTE: usize = 21;

impl PortDashboard {
    pub fn new() -> Self {
        Self { visible: false, entries: Vec::new(), selected: 0, sort_by: SortField::Port }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible { self.refresh(); }
    }

    /// Re-run the platform port-listing command and re-sort.
    pub fn refresh(&mut self) {
        self.entries.clear();
        fetch_ports(&mut self.entries);
        self.sort_entries();
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
    }

    fn sort_entries(&mut self) {
        match self.sort_by {
            SortField::Port => self.entries.sort_by_key(|e| e.port),
            SortField::Process => self.entries.sort_by(|a, b| {
                a.process_name.to_lowercase().cmp(&b.process_name.to_lowercase())
            }),
            SortField::State => self.entries.sort_by(|a, b| a.state.cmp(&b.state)),
        }
    }

    pub fn handle_key(&mut self, key: PortDashboardKey) -> Option<PortAction> {
        match key {
            PortDashboardKey::Escape => { self.visible = false; None }
            PortDashboardKey::Up => { self.selected = self.selected.saturating_sub(1); None }
            PortDashboardKey::Down => {
                if self.selected + 1 < self.entries.len() { self.selected += 1; }
                None
            }
            PortDashboardKey::Enter => self.kill_selected(),
            PortDashboardKey::Char('k') => self.kill_selected(),
            PortDashboardKey::Char('r') => { self.refresh(); None }
            PortDashboardKey::Char('s') => {
                self.sort_by = match self.sort_by {
                    SortField::Port => SortField::Process,
                    SortField::Process => SortField::State,
                    SortField::State => SortField::Port,
                };
                self.sort_entries();
                None
            }
            PortDashboardKey::Char(_) => None,
        }
    }

    fn kill_selected(&self) -> Option<PortAction> {
        self.entries.get(self.selected).map(|e| PortAction::Kill(e.pid))
    }

    pub fn render(
        &self, buffer: &mut [u32], width: usize, height: usize,
        font: &mut crate::renderer::font::FontManager, theme: &crate::config::Theme,
    ) {
        if !self.visible { return; }
        crate::ui::dim_backdrop(buffer, 3);

        let cw = font.cell_width.max(1);
        let ch = font.cell_height;

        let content_chars = COL_PORT + COL_PROTO + COL_PID + COL_PROCESS + COL_STATE + COL_LOCAL + COL_REMOTE + 6;
        let pw = ((content_chars + 4) * cw).min(width.saturating_sub(40)).max(cw * 24);

        let title_h = ch + 10;
        let footer_h = ch + 14;
        let row_h = ch + 2;
        let fixed_h = title_h + ch + 16 + footer_h;
        let max_rows = ((height.saturating_sub(40).saturating_sub(fixed_h)) / row_h.max(1)).max(1);
        let visible_rows = self.entries.len().clamp(1, max_rows);
        let ph = (fixed_h + visible_rows * row_h).min(height.saturating_sub(40));

        let px = width.saturating_sub(pw) / 2;
        let py = height.saturating_sub(ph) / 2;
        let right_edge = px + pw.saturating_sub(10);

        let bg = crate::ui::lighten(theme.bg, 6);
        crate::ui::fill_rect(buffer, width, px, py, pw, ph, crate::ui::pack_rgb(bg));
        crate::ui::draw_border(buffer, width, px, py, pw, ph, crate::ui::pack_rgb(crate::ui::dim(theme.cursor, 0.4)));

        let mut ty = py + 10;

        // Title + live count.
        let title = format!("PORT DASHBOARD  ({} entries)", self.entries.len());
        crate::ui::render_text(buffer, width, font, &title, px + 16, ty, theme.cursor);
        ty += title_h;

        // Column headers — green accent, same fixed widths as the data rows.
        let header_color = crate::ui::dim(theme.cursor, 0.85);
        let mut rx = px + 16;
        draw_col(buffer, width, font, &mut rx, right_edge, ty, "PORT", COL_PORT, header_color);
        draw_col(buffer, width, font, &mut rx, right_edge, ty, "PROTO", COL_PROTO, header_color);
        draw_col(buffer, width, font, &mut rx, right_edge, ty, "PID", COL_PID, header_color);
        draw_col(buffer, width, font, &mut rx, right_edge, ty, "PROCESS", COL_PROCESS, header_color);
        draw_col(buffer, width, font, &mut rx, right_edge, ty, "STATE", COL_STATE, header_color);
        draw_col(buffer, width, font, &mut rx, right_edge, ty, "LOCAL", COL_LOCAL, header_color);
        draw_col(buffer, width, font, &mut rx, right_edge, ty, "REMOTE", COL_REMOTE, header_color);

        // Divider under the header row.
        let sep_y = ty + ch + 2;
        {
            let sep_px = crate::ui::pack_rgb(crate::ui::dim(theme.fg, 0.15));
            let off = sep_y * width + px + 8;
            let end = (off + pw.saturating_sub(16)).min(buffer.len());
            if off < buffer.len() { buffer[off..end].fill(sep_px); }
        }
        ty = sep_y + 6;

        if self.entries.is_empty() {
            crate::ui::render_text(buffer, width, font, "No listening ports found", px + 16, ty,
                crate::ui::dim(theme.fg, 0.4));
        } else {
            let total = self.entries.len();
            let start = if total <= visible_rows { 0 } else {
                self.selected.saturating_sub(visible_rows / 2).min(total - visible_rows)
            };
            let end = (start + visible_rows).min(total);

            for i in start..end {
                if ty + ch >= py + ph.saturating_sub(footer_h) { break; }
                let e = &self.entries[i];
                let sel = i == self.selected;

                if sel {
                    crate::ui::fill_rect(
                        buffer, width, px + 4, ty.saturating_sub(1), pw.saturating_sub(8), row_h,
                        crate::ui::pack_rgb(crate::ui::lighten(bg, 14)),
                    );
                }

                let dim_fg = if sel { theme.fg } else { crate::ui::dim(theme.fg, 0.65) };
                let proc_color = if sel { theme.fg } else { crate::ui::dim(theme.fg, 0.85) };
                let addr_color = crate::ui::dim(theme.fg, if sel { 0.6 } else { 0.4 });
                let st_color = state_color(&e.state, theme.fg);
                let state_disp = if e.state.is_empty() { "-" } else { &e.state };

                let mut rx = px + 16;
                draw_col(buffer, width, font, &mut rx, right_edge, ty, &e.port.to_string(), COL_PORT, dim_fg);
                draw_col(buffer, width, font, &mut rx, right_edge, ty, &e.proto, COL_PROTO, dim_fg);
                draw_col(buffer, width, font, &mut rx, right_edge, ty, &e.pid.to_string(), COL_PID, dim_fg);
                draw_col(buffer, width, font, &mut rx, right_edge, ty,
                    crate::ui::trunc(&e.process_name, COL_PROCESS), COL_PROCESS, proc_color);
                draw_col(buffer, width, font, &mut rx, right_edge, ty, state_disp, COL_STATE, st_color);
                draw_col(buffer, width, font, &mut rx, right_edge, ty,
                    crate::ui::trunc(&e.local_addr, COL_LOCAL), COL_LOCAL, addr_color);
                draw_col(buffer, width, font, &mut rx, right_edge, ty,
                    crate::ui::trunc(&e.remote_addr, COL_REMOTE), COL_REMOTE, addr_color);

                ty += row_h;
            }
        }

        let sort_label = match self.sort_by {
            SortField::Port => "PORT",
            SortField::Process => "PROCESS",
            SortField::State => "STATE",
        };
        let footer = format!("[K] Kill  [R] Refresh  [S] Sort  [Esc] Close   (sort: {sort_label})");
        let max_chars = pw.saturating_sub(32) / cw;
        crate::ui::render_text(
            buffer, width, font, crate::ui::trunc(&footer, max_chars),
            px + 16, py + ph.saturating_sub(ch + 10), crate::ui::dim(theme.fg, 0.3),
        );
    }
}

pub enum PortDashboardKey { Escape, Up, Down, Enter, Char(char) }
pub enum PortAction { Kill(u32) }

/// Render one fixed-width, left-padded column, skipping it entirely once it
/// would spill past the panel's right edge (keeps truncation column-aligned
/// rather than mid-glyph).
fn draw_col(
    buffer: &mut [u32], buf_w: usize, font: &mut crate::renderer::font::FontManager,
    rx: &mut usize, right_edge: usize, y: usize, text: &str, w: usize, color: crate::config::Rgb,
) {
    let cw = font.cell_width.max(1);
    if *rx + w * cw > right_edge { return; }
    crate::ui::render_text(buffer, buf_w, font, &pad(text, w), *rx, y, color);
    *rx += (w + 1) * cw;
}

fn pad(s: &str, w: usize) -> String {
    format!("{:<1$}", s, w)
}

fn state_color(state: &str, fg: crate::config::Rgb) -> crate::config::Rgb {
    match state {
        "LISTEN" => (166, 227, 161),      // green
        "ESTABLISHED" => (148, 226, 213), // cyan
        "CLOSE_WAIT" => (249, 226, 175),  // yellow
        "TIME_WAIT" => crate::ui::dim(fg, 0.35),
        "" => crate::ui::dim(fg, 0.3),
        _ => crate::ui::dim(fg, 0.55),
    }
}

/// Kill a process by PID with SIGTERM (`kill -15 <pid>`).
pub fn kill_pid(pid: u32) {
    match std::process::Command::new("kill").args(["-15", &pid.to_string()]).output() {
        Ok(out) if !out.status.success() => {
            log::warn!(
                "kill -15 {pid} exited with {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim(),
            );
        }
        Err(e) => log::warn!("failed to run `kill -15 {pid}`: {e}"),
        _ => {}
    }
}

// ── Port listing (platform-specific) ──

#[cfg(target_os = "macos")]
fn fetch_ports(entries: &mut Vec<PortEntry>) {
    if let Ok(out) = std::process::Command::new("lsof")
        .args(["-iTCP", "-iUDP", "-sTCP:LISTEN", "-P", "-n"])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if let Some(e) = parse_lsof_line(line) {
                entries.push(e);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn fetch_ports(entries: &mut Vec<PortEntry>) {
    if let Ok(out) = std::process::Command::new("ss").args(["-tulnp"]).output() {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines().skip(1) {
            if let Some(e) = parse_ss_line(line) {
                entries.push(e);
            }
        }
    }
}

/// Parse one data line of `lsof -iTCP -iUDP -sTCP:LISTEN -P -n`, e.g.:
///   `redis-ser   855 user    7u  IPv6 0xf15c4caae39323a4      0t0  TCP [::1]:6379 (LISTEN)`
///   `identitys   585 user   10u  IPv4 0xe69503d374e6ddd3      0t0  UDP *:*`
/// Walked from the right so a COMMAND containing spaces can't shift the
/// fixed-position columns (USER/FD/TYPE/DEVICE/SIZE-OFF/NODE/NAME/[STATE]).
#[cfg(target_os = "macos")]
fn parse_lsof_line(line: &str) -> Option<PortEntry> {
    let mut toks: Vec<&str> = line.split_whitespace().collect();
    if toks.len() < 8 || toks[0] == "COMMAND" {
        return None;
    }

    let mut state = String::new();
    if let Some(last) = toks.last() {
        if last.starts_with('(') && last.ends_with(')') {
            state = last.trim_matches(|c: char| c == '(' || c == ')').to_string();
            toks.pop();
        }
    }

    let name = toks.pop()?.to_string();
    let proto = toks.pop()?.to_string();
    if toks.len() < 6 {
        return None;
    }
    // Discard USER, FD, TYPE, DEVICE, SIZE/OFF — unused by the dashboard.
    for _ in 0..5 {
        toks.pop();
    }
    let pid: u32 = toks.pop()?.parse().ok()?;
    let process_name = if toks.is_empty() { "?".to_string() } else { toks.join(" ") };

    let (local_addr, remote_addr) = match name.find("->") {
        Some(idx) => (name[..idx].to_string(), name[idx + 2..].to_string()),
        None => (name.clone(), String::new()),
    };
    let port: u16 = local_addr.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or(0);

    Some(PortEntry {
        port,
        proto: proto.to_uppercase(),
        pid,
        process_name,
        state,
        local_addr,
        remote_addr,
    })
}

/// Parse one data line of `ss -tulnp` on Linux, e.g.:
///   `tcp   LISTEN 0  128  0.0.0.0:22   0.0.0.0:*  users:(("sshd",pid=1234,fd=3))`
#[cfg(not(target_os = "macos"))]
fn parse_ss_line(line: &str) -> Option<PortEntry> {
    let cols: Vec<&str> = line.split_whitespace().collect();
    if cols.len() < 5 {
        return None;
    }
    let proto = cols[0].to_uppercase();
    if proto != "TCP" && proto != "UDP" {
        return None;
    }
    let state = cols[1].to_string();
    let local = cols[4];
    let remote = cols.get(5).copied().unwrap_or("*:*");
    let port: u16 = local.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or(0);

    let mut pid = 0u32;
    let mut process_name = "-".to_string();
    if cols.len() > 6 {
        let proc_col = cols[6..].join(" ");
        if let Some(start) = proc_col.find('"') {
            if let Some(end) = proc_col[start + 1..].find('"') {
                process_name = proc_col[start + 1..start + 1 + end].to_string();
            }
        }
        if let Some(idx) = proc_col.find("pid=") {
            let digits: String = proc_col[idx + 4..].chars().take_while(|c| c.is_ascii_digit()).collect();
            pid = digits.parse().unwrap_or(0);
        }
    }

    let is_wildcard_remote = remote.ends_with(":*")
        && (remote.starts_with("0.0.0.0") || remote.starts_with('*') || remote.starts_with("[::]"));
    let remote_addr = if is_wildcard_remote { String::new() } else { remote.to_string() };

    Some(PortEntry {
        port,
        proto,
        pid,
        process_name,
        state,
        local_addr: local.to_string(),
        remote_addr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_tcp_listen_line() {
        let line = "redis-ser   855 user    7u  IPv6 0xf15c4caae39323a4      0t0  TCP [::1]:6379 (LISTEN)";
        let e = parse_lsof_line(line).expect("should parse");
        assert_eq!(e.pid, 855);
        assert_eq!(e.proto, "TCP");
        assert_eq!(e.state, "LISTEN");
        assert_eq!(e.port, 6379);
        assert_eq!(e.local_addr, "[::1]:6379");
        assert_eq!(e.process_name, "redis-ser");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_udp_line_with_no_state() {
        let line = "identitys   585 user   10u  IPv4 0xe69503d374e6ddd3      0t0  UDP *:*";
        let e = parse_lsof_line(line).expect("should parse");
        assert_eq!(e.pid, 585);
        assert_eq!(e.proto, "UDP");
        assert_eq!(e.state, "");
        assert_eq!(e.process_name, "identitys");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_established_with_remote_addr() {
        let line = "Google    1234 user   10u  IPv4 0xabc      0t0  TCP 192.168.1.5:54321->17.248.128.1:443 (ESTABLISHED)";
        let e = parse_lsof_line(line).expect("should parse");
        assert_eq!(e.local_addr, "192.168.1.5:54321");
        assert_eq!(e.remote_addr, "17.248.128.1:443");
        assert_eq!(e.state, "ESTABLISHED");
        assert_eq!(e.port, 54321);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn skips_header_line() {
        let line = "COMMAND     PID   USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME";
        assert!(parse_lsof_line(line).is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn handles_multi_word_command_name() {
        // Some processes (e.g. "Google Chrome H") report COMMAND fields with
        // embedded spaces; parsing from the right must still find the PID.
        let line = "Google Chrome H 4242 user   10u  IPv4 0xabc      0t0  TCP *:8080 (LISTEN)";
        let e = parse_lsof_line(line).expect("should parse");
        assert_eq!(e.pid, 4242);
        assert_eq!(e.process_name, "Google Chrome H");
        assert_eq!(e.port, 8080);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn parses_ss_listen_line() {
        let line = r#"tcp   LISTEN 0      128    0.0.0.0:22       0.0.0.0:*     users:(("sshd",pid=1234,fd=3))"#;
        let e = parse_ss_line(line).expect("should parse");
        assert_eq!(e.pid, 1234);
        assert_eq!(e.process_name, "sshd");
        assert_eq!(e.port, 22);
        assert_eq!(e.state, "LISTEN");
    }

    #[test]
    fn sort_cycle_covers_all_fields() {
        let mut d = PortDashboard::new();
        assert_eq!(d.sort_by, SortField::Port);
        d.handle_key(PortDashboardKey::Char('s'));
        assert_eq!(d.sort_by, SortField::Process);
        d.handle_key(PortDashboardKey::Char('s'));
        assert_eq!(d.sort_by, SortField::State);
        d.handle_key(PortDashboardKey::Char('s'));
        assert_eq!(d.sort_by, SortField::Port);
    }
}
