/// Characters that can never be part of a detected URL: whitespace, control
/// characters, quotes / angle brackets / backticks, and CJK / full-width
/// punctuation (so "详见https://a.com/x。后面" ends at the ideographic full stop
/// while CJK letters *inside* a path, as in `/中文`, are kept).
fn is_stop(c: char) -> bool {
    if c == '\0' {
        return false; // wide-glyph continuation cell: part of whatever precedes it
    }
    if c.is_whitespace() || c.is_control() {
        return true;
    }
    matches!(c, '"' | '\'' | '<' | '>' | '`')
        || matches!(c as u32,
            0x2018..=0x201F        // curly quotes
            | 0x3000..=0x303F      // CJK symbols & punctuation (。、「」『』【】 ...)
            | 0xFF00..=0xFF0F      // full-width ！＂＃…／
            | 0xFF1A..=0xFF20      // full-width ：；＜＝＞？＠
            | 0xFF3B..=0xFF40      // full-width ［＼］＾＿｀
            | 0xFF5B..=0xFF65)     // full-width ｛｜｝～ and half-width CJK punctuation
}

fn scheme_len(chars: &[char], i: usize) -> usize {
    let starts = |pat: &str| {
        let p: Vec<char> = pat.chars().collect();
        chars.len() >= i + p.len() && chars[i..i + p.len()] == p[..]
    };
    if starts("https://") {
        8
    } else if starts("http://") {
        7
    } else {
        0
    }
}

/// Detect URLs in a text line, returning `(start_col, end_col, url)` tuples
/// (`end_col` exclusive). Columns are `char` indices into `line`; a `'\0'`
/// (continuation cell of a wide glyph) counts as a column and stays inside the
/// URL but is dropped from the returned text.
///
/// Rules: only `http`/`https`; terminated by whitespace, quotes, `<>` and
/// CJK/full-width punctuation; `)` and `]` only belong to the URL when they
/// close a `(`/`[` opened inside it (Wikipedia links, IPv6 hosts, `q[]=1`);
/// trailing `.,;:!?` is sentence punctuation, not URL.
pub fn detect_urls(line: &str) -> Vec<(usize, usize, String)> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut results = Vec::new();
    let mut i = 0;

    while i < n {
        if chars[i] != 'h' {
            i += 1;
            continue;
        }
        let sl = scheme_len(&chars, i);
        if sl == 0 {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i + sl;
        let (mut paren, mut bracket) = (0i32, 0i32);
        while end < n {
            let c = chars[end];
            if is_stop(c) {
                break;
            }
            match c {
                '(' => paren += 1,
                ')' => {
                    if paren == 0 {
                        break;
                    }
                    paren -= 1;
                }
                '[' => bracket += 1,
                ']' => {
                    if bracket == 0 {
                        break;
                    }
                    bracket -= 1;
                }
                _ => {}
            }
            end += 1;
        }
        while end > start + sl && matches!(chars[end - 1], '.' | ',' | ';' | ':' | '!' | '?') {
            end -= 1;
        }
        // Needs a host: at least one real character after "://".
        if end > start + sl && chars[start + sl..end].iter().any(|c| *c != '\0') {
            let url: String = chars[start..end].iter().filter(|c| **c != '\0').collect();
            results.push((start, end, url));
            i = end;
        } else {
            i = start + sl;
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
