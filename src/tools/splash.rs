pub fn generate_splash(_cols: usize, _rows: usize) -> String {
    let green = "\x1b[32m";
    let cyan = "\x1b[36m";
    let yellow = "\x1b[33m";
    let dim = "\x1b[90m";
    let bold = "\x1b[1m";
    let reset = "\x1b[0m";

    let logo = [
        r"  ██████╗ ████████╗███████╗██████╗ ███╗   ███╗",
        r"  ██╔══██╗╚══██╔══╝██╔════╝██╔══██╗████╗ ████║",
        r"  ██████╔╝   ██║   █████╗  ██████╔╝██╔████╔██║",
        r"  ██╔══██╗   ██║   ██╔══╝  ██╔══██╗██║╚██╔╝██║",
        r"  ██║  ██║   ██║   ███████╗██║  ██║██║ ╚═╝ ██║",
        r"  ╚═╝  ╚═╝   ╚═╝   ╚══════╝╚═╝  ╚═╝╚═╝     ╚═╝",
    ];

    let os_name = format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH);
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "unknown".into());
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "unknown".into());
    let uptime = get_uptime().unwrap_or_else(|| "N/A".into());

    let mut out = String::with_capacity(2048);

    out.push_str("\r\n");
    out.push_str(&format!("{dim}  ┌──────────────────────────────────────────────────┐{reset}\r\n"));
    out.push_str(&format!("{dim}  │{reset}                                                  {dim}│{reset}\r\n"));

    for line in &logo {
        out.push_str(&format!("{dim}  │{reset} {bold}{green}{line}{reset}"));
        let pad = 48_usize.saturating_sub(visible_len(line));
        for _ in 0..pad {
            out.push(' ');
        }
        out.push_str(&format!(" {dim}│{reset}\r\n"));
    }

    out.push_str(&format!("{dim}  │{reset}                                                  {dim}│{reset}\r\n"));

    let version_line = "v0.3.0 · Rust Terminal Emulator";
    out.push_str(&format!(
        "{dim}  │{reset}  {bold}{green}{version_line}{reset}{}  {dim}│{reset}\r\n",
        " ".repeat(48 - 2 - version_line.len())
    ));

    out.push_str(&format!("{dim}  │{reset}                                                  {dim}│{reset}\r\n"));

    let info_lines = [
        ("OS", os_name),
        ("User", user),
        ("Shell", shell),
        ("Term", "rift (softbuffer)".into()),
        ("Uptime", uptime),
    ];
    for (label, value) in &info_lines {
        let content = format!("{label}: {value}");
        let pad = 48 - 2 - content.len().min(46);
        out.push_str(&format!(
            "{dim}  │{reset}  {cyan}{content}{reset}{}{dim}│{reset}\r\n",
            " ".repeat(pad)
        ));
    }

    out.push_str(&format!("{dim}  │{reset}                                                  {dim}│{reset}\r\n"));

    let shortcuts = [
        ("Ctrl+Shift+R", "Toggle Recording"),
        ("Ctrl+Shift+1", "CRT Effect"),
        ("Ctrl+Shift+E", "Codec Toolkit"),
        ("Ctrl+Shift+H", "Hex Viewer"),
    ];
    for (key, desc) in &shortcuts {
        let content = format!("{key}  {desc}");
        let pad = 48 - 2 - content.len().min(46);
        out.push_str(&format!(
            "{dim}  │{reset}  {yellow}{key}{reset}  {dim}{desc}{reset}{}{dim}│{reset}\r\n",
            " ".repeat(pad)
        ));
    }

    out.push_str(&format!("{dim}  │{reset}                                                  {dim}│{reset}\r\n"));
    out.push_str(&format!("{dim}  └──────────────────────────────────────────────────┘{reset}\r\n"));
    out.push_str("\r\n");

    out
}

fn visible_len(s: &str) -> usize {
    s.chars().count()
}

fn get_uptime() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(content) = std::fs::read_to_string("/proc/uptime") {
            if let Some(secs_str) = content.split_whitespace().next() {
                if let Ok(secs) = secs_str.parse::<f64>() {
                    return Some(format_duration(secs as u64));
                }
            }
        }
        None
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let output = Command::new("sysctl")
            .args(["-n", "kern.boottime"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let sec_start = text.find("sec = ")? + 6;
        let sec_end = sec_start + text[sec_start..].find(',')?;
        let boot_sec: u64 = text[sec_start..sec_end].trim().parse().ok()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        Some(format_duration(now.saturating_sub(boot_sec)))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

fn format_duration(total_secs: u64) -> String {
    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let mins = (total_secs % 3600) / 60;
    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}
