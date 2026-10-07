/// Detect URLs in a text line, returning (start_col, end_col, url) tuples.
pub fn detect_urls(line: &str) -> Vec<(usize, usize, String)> {
    let mut results = Vec::new();
    let mut i = 0;
    let chars: Vec<char> = line.chars().collect();

    while i < chars.len() {
        let remaining: String = chars[i..].iter().collect();
        if remaining.starts_with("http://") || remaining.starts_with("https://") {
            let start = i;
            let mut end = i;
            while end < chars.len()
                && !matches!(chars[end], ' ' | '"' | '\'' | ')' | ']' | '>' | '`' | '\t')
            {
                end += 1;
            }
            while end > start && matches!(chars[end - 1], '.' | ',' | ';' | ':' | '!') {
                end -= 1;
            }
            let url: String = chars[start..end].iter().collect();
            if url.len() > 8 {
                results.push((start, end, url));
            }
            i = end;
        } else {
            i += 1;
        }
    }
    results
}

/// Open a URL using the system browser.
pub fn open_url(url: &str) {
    log::info!("Opening URL: {}", url);
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", url])
            .spawn();
    }
}

/// Check if a cell position (col) falls within any URL on the given line.
pub fn url_at_col(line: &str, col: usize) -> Option<String> {
    for (start, end, url) in detect_urls(line) {
        if col >= start && col < end {
            return Some(url);
        }
    }
    None
}
