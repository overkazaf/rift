//! Minimal Markdown for assistant answers plus CJK-aware word wrapping.
//!
//! Supported: ATX headings (`#`..`###`), fenced code blocks (``` / ~~~ with a
//! language label; an unterminated fence is "open" while streaming),
//! bullet (`-`, `*`, `+`) and numbered lists, paragraphs, `**bold**` and
//! `` `inline code` ``. Everything else is plain text.

use crate::renderer::font::is_wide;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Normal,
    Bold,
    Code,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

impl Span {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self { text: text.into(), style }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading { level: u8, spans: Vec<Span> },
    Paragraph(Vec<Span>),
    /// `marker` is "•" for bullets or "1." for numbered items; `indent` is
    /// the nesting level (2 source spaces per level).
    ListItem { marker: String, indent: usize, spans: Vec<Span> },
    Code { lang: String, code: String, closed: bool },
    Blank,
}

/// Terminal cell width of one char (0 for combining / control, 2 for wide).
pub fn cells(c: char) -> usize {
    if is_wide(c) {
        2
    } else {
        c.width().unwrap_or(0)
    }
}

pub fn str_cells(s: &str) -> usize {
    s.chars().map(cells).sum()
}

// ── Block parser ──

pub fn parse_blocks(src: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut fence: Option<(char, usize, String, Vec<&str>)> = None; // (ch, len, lang, lines)

    fn flush(out: &mut Vec<Block>, para: &mut Vec<&str>) {
        if !para.is_empty() {
            out.push(Block::Paragraph(parse_inline(&para.join(" "))));
            para.clear();
        }
    }

    for line in src.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some((ch, len, lang, lines)) = fence.as_mut() {
            let t = line.trim();
            let closes = t.len() >= *len && t.chars().all(|c| c == *ch);
            if closes {
                let (lang, code) = (std::mem::take(lang), lines.join("\n"));
                out.push(Block::Code { lang, code, closed: true });
                fence = None;
            } else {
                lines.push(line);
            }
            continue;
        }
        let trimmed = line.trim_start();
        // Opening fence.
        if let Some(fc) = trimmed.chars().next().filter(|c| *c == '`' || *c == '~') {
            let n = trimmed.chars().take_while(|c| *c == fc).count();
            if n >= 3 {
                let info = trimmed[n..].trim();
                // A backtick fence's info string can't contain backticks.
                if !(fc == '`' && info.contains('`')) {
                    flush(&mut out, &mut para);
                    let lang = info.split_whitespace().next().unwrap_or("").to_string();
                    fence = Some((fc, n, lang, Vec::new()));
                    continue;
                }
            }
        }
        if trimmed.is_empty() {
            flush(&mut out, &mut para);
            if !matches!(out.last(), Some(Block::Blank) | None) {
                out.push(Block::Blank);
            }
            continue;
        }
        // Heading.
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            flush(&mut out, &mut para);
            out.push(Block::Heading { level: hashes as u8, spans: parse_inline(trimmed[hashes..].trim()) });
            continue;
        }
        // List item.
        if let Some((marker, rest)) = list_marker(trimmed) {
            flush(&mut out, &mut para);
            let indent = (line.len() - trimmed.len()) / 2;
            out.push(Block::ListItem { marker, indent: indent.min(4), spans: parse_inline(rest) });
            continue;
        }
        para.push(trimmed);
    }
    flush(&mut out, &mut para);
    if let Some((_, _, lang, lines)) = fence {
        out.push(Block::Code { lang, code: lines.join("\n"), closed: false });
    }
    while matches!(out.last(), Some(Block::Blank)) {
        out.pop();
    }
    out
}

fn list_marker(t: &str) -> Option<(String, &str)> {
    let mut it = t.chars();
    let c = it.next()?;
    if matches!(c, '-' | '*' | '+') && t[1..].starts_with(' ') && !t.starts_with("**") {
        return Some(("\u{2022}".to_string(), t[1..].trim_start()));
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=3).contains(&digits) {
        let rest = &t[digits..];
        if (rest.starts_with(". ") || rest.starts_with(") ")) && rest.len() > 2 {
            return Some((format!("{}.", &t[..digits]), rest[2..].trim_start()));
        }
    }
    None
}

// ── Inline parser ──

pub fn parse_inline(s: &str) -> Vec<Span> {
    let chars: Vec<char> = s.chars().collect();
    let mut spans: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    fn push(spans: &mut Vec<Span>, buf: &mut String, style: Style) {
        if !buf.is_empty() {
            match spans.last_mut() {
                Some(l) if l.style == style => l.text.push_str(buf),
                _ => spans.push(Span::new(std::mem::take(buf), style)),
            }
            buf.clear();
        }
    }

    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            // Run of backticks closes with an equal run.
            let n = chars[i..].iter().take_while(|c| **c == '`').count();
            if let Some(end) = find_run(&chars, i + n, '`', n) {
                push(&mut spans, &mut buf, Style::Normal);
                let inner: String = chars[i + n..end].iter().collect();
                spans.push(Span::new(inner.trim_matches(' ').to_string(), Style::Code));
                i = end + n;
                continue;
            }
        } else if c == '*' && chars.get(i + 1) == Some(&'*') && chars.get(i + 2).is_some_and(|c| !c.is_whitespace()) {
            if let Some(end) = find_run(&chars, i + 2, '*', 2) {
                if end > i + 2 && !chars[end - 1].is_whitespace() {
                    push(&mut spans, &mut buf, Style::Normal);
                    let inner: String = chars[i + 2..end].iter().collect();
                    spans.push(Span::new(inner, Style::Bold));
                    i = end + 2;
                    continue;
                }
            }
        }
        buf.push(c);
        i += 1;
    }
    push(&mut spans, &mut buf, Style::Normal);
    spans
}

/// Index of the first run of exactly `n` `ch`s at or after `from`.
fn find_run(chars: &[char], from: usize, ch: char, n: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == ch {
            let run = chars[i..].iter().take_while(|c| **c == ch).count();
            if run == n {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

// ── Wrapping ──

/// One wrapped output line: styled segments left to right.
pub type Line = Vec<Span>;

fn line_push(line: &mut Line, text: &str, style: Style) {
    match line.last_mut() {
        Some(l) if l.style == style => l.text.push_str(text),
        _ => line.push(Span::new(text, style)),
    }
}

/// Greedy word wrap of styled spans to `width` cells. Latin words stay whole,
/// every wide (CJK) char is its own break opportunity, runs of spaces
/// collapse to one and never start a line. Words longer than `width` are
/// split. Returns at least one (possibly empty) line.
pub fn wrap_spans(spans: &[Span], width: usize) -> Vec<Line> {
    let width = width.max(1);
    // Atoms: (text, style, cells, is_space)
    let mut atoms: Vec<(String, Style, usize, bool)> = Vec::new();
    for sp in spans {
        let mut word = String::new();
        let mut wcells = 0;
        let flush_word = |atoms: &mut Vec<(String, Style, usize, bool)>, word: &mut String, wcells: &mut usize| {
            if !word.is_empty() {
                atoms.push((std::mem::take(word), sp.style, *wcells, false));
                *wcells = 0;
            }
        };
        for c in sp.text.chars() {
            if c == '\t' || c == ' ' || c == '\n' {
                flush_word(&mut atoms, &mut word, &mut wcells);
                if !matches!(atoms.last(), Some((_, _, _, true))) || sp.style == Style::Code {
                    atoms.push((" ".into(), sp.style, 1, true));
                }
            } else if is_wide(c) {
                flush_word(&mut atoms, &mut word, &mut wcells);
                atoms.push((c.to_string(), sp.style, 2, false));
            } else if cells(c) > 0 || !word.is_empty() {
                wcells += cells(c);
                word.push(c);
            }
        }
        flush_word(&mut atoms, &mut word, &mut wcells);
    }

    let mut lines: Vec<Line> = Vec::new();
    let mut cur: Line = Vec::new();
    let mut cur_w = 0usize;
    let mut pending_space: Option<Style> = None;

    for (text, style, w, is_space) in atoms {
        if is_space {
            if cur_w > 0 {
                pending_space = Some(style);
            }
            continue;
        }
        let sp_w = usize::from(pending_space.is_some());
        if cur_w + sp_w + w <= width {
            if let Some(s) = pending_space.take() {
                line_push(&mut cur, " ", s);
                cur_w += 1;
            }
            line_push(&mut cur, &text, style);
            cur_w += w;
            continue;
        }
        pending_space = None;
        if cur_w > 0 {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        if w <= width {
            line_push(&mut cur, &text, style);
            cur_w = w;
        } else {
            // Over-long word: hard split by cells.
            for c in text.chars() {
                let cw = cells(c);
                if cur_w + cw > width && cur_w > 0 {
                    lines.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
                line_push(&mut cur, c.encode_utf8(&mut [0; 4]), style);
                cur_w += cw;
            }
        }
    }
    lines.push(cur);
    lines
}

/// Wrap plain text (user bubbles): honours `\n`, otherwise like [`wrap_spans`].
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        let lines = wrap_spans(&[Span::new(para.trim_end_matches('\r'), Style::Normal)], width);
        for l in lines {
            out.push(l.into_iter().map(|s| s.text).collect());
        }
    }
    out
}

/// Hard wrap (no word logic, whitespace preserved) for code lines.
pub fn wrap_code_line(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut w = 0;
    for c in line.chars() {
        let c = if c == '\t' { ' ' } else { c };
        let cw = cells(c);
        if w + cw > width && w > 0 {
            out.push(std::mem::take(&mut cur));
            w = 0;
        }
        cur.push(c);
        w += cw;
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.iter().map(|s| s.text.as_str()).collect()).collect()
    }

    #[test]
    fn parses_headings_lists_and_paragraphs() {
        let b = parse_blocks("# Title\n\nSome **bold** text\nsecond line\n\n- one\n  - nested\n2. two\n");
        assert_eq!(b[0], Block::Heading { level: 1, spans: vec![Span::new("Title", Style::Normal)] });
        assert_eq!(b[1], Block::Blank);
        match &b[2] {
            Block::Paragraph(s) => {
                assert_eq!(s[0], Span::new("Some ", Style::Normal));
                assert_eq!(s[1], Span::new("bold", Style::Bold));
                assert_eq!(s[2], Span::new(" text second line", Style::Normal));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&b[4], Block::ListItem { marker, indent: 0, .. } if marker == "\u{2022}"));
        assert!(matches!(&b[5], Block::ListItem { indent: 1, .. }));
        assert!(matches!(&b[6], Block::ListItem { marker, .. } if marker == "2."));
    }

    #[test]
    fn parses_fenced_code_closed_and_open() {
        let b = parse_blocks("Run:\n```bash\nls -la\necho \"a\"\n```\nDone");
        assert_eq!(b.len(), 3);
        assert_eq!(b[1], Block::Code { lang: "bash".into(), code: "ls -la\necho \"a\"".into(), closed: true });
        assert!(matches!(&b[2], Block::Paragraph(_)));

        // Streaming: no closing fence yet.
        let b = parse_blocks("```sh\ncargo bu");
        assert_eq!(b, vec![Block::Code { lang: "sh".into(), code: "cargo bu".into(), closed: false }]);

        // Longer fences contain shorter ones; ~~~ works; no language is fine.
        let b = parse_blocks("````\n```\ninner\n```\n````\n~~~\nx\n~~~");
        assert_eq!(b[0], Block::Code { lang: "".into(), code: "```\ninner\n```".into(), closed: true });
        assert_eq!(b[1], Block::Code { lang: "".into(), code: "x".into(), closed: true });
    }

    #[test]
    fn inline_code_bold_and_unclosed_markers() {
        let s = parse_inline("use `cargo build` and **fast**!");
        assert_eq!(
            s,
            vec![
                Span::new("use ", Style::Normal),
                Span::new("cargo build", Style::Code),
                Span::new(" and ", Style::Normal),
                Span::new("fast", Style::Bold),
                Span::new("!", Style::Normal),
            ]
        );
        assert_eq!(parse_inline("a `` b`c `` d")[1], Span::new("b`c", Style::Code));
        // Unclosed markers stay literal (mid-stream).
        assert_eq!(parse_inline("**bol"), vec![Span::new("**bol", Style::Normal)]);
        assert_eq!(parse_inline("`co"), vec![Span::new("`co", Style::Normal)]);
        assert_eq!(parse_inline("2 * 3 ** 4"), vec![Span::new("2 * 3 ** 4", Style::Normal)]);
    }

    #[test]
    fn wraps_latin_on_word_boundaries() {
        let w = wrap_spans(&parse_inline("the quick brown fox"), 9);
        assert_eq!(text(&w), vec!["the quick", "brown fox"]);
        let w = wrap_spans(&parse_inline("a    b"), 10);
        assert_eq!(text(&w), vec!["a b"]);
        // Over-long word is split.
        let w = wrap_spans(&parse_inline("abcdefghij"), 4);
        assert_eq!(text(&w), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn wraps_cjk_by_cell_width() {
        // 8 wide chars = 16 cells; width 7 fits 3 chars (6 cells) per line.
        let w = wrap_spans(&[Span::new("你好世界中文测试", Style::Normal)], 7);
        let t = text(&w);
        assert_eq!(t, vec!["你好世", "界中文", "测试"]);
        assert!(t.iter().all(|l| str_cells(l) <= 7));
        // Mixed: Latin word is kept whole next to CJK.
        let w = wrap_spans(&[Span::new("运行 cargo build 即可", Style::Normal)], 12);
        let t = text(&w);
        assert!(t.iter().all(|l| str_cells(l) <= 12), "{t:?}");
        assert_eq!(t.join("|"), "运行 cargo|build 即可");
    }

    #[test]
    fn wrap_keeps_styles_and_handles_empty() {
        let w = wrap_spans(&parse_inline("ab `cd ef` gh"), 5);
        assert_eq!(text(&w), vec!["ab cd", "ef gh"]);
        assert_eq!(w[0][1].style, Style::Code);
        assert_eq!(wrap_spans(&[], 10), vec![Vec::<Span>::new()]);
        assert_eq!(wrap_text("a\n\nb", 10), vec!["a", "", "b"]);
    }

    #[test]
    fn code_lines_hard_wrap_with_wide_chars() {
        assert_eq!(wrap_code_line("abcdef", 4), vec!["abcd", "ef"]);
        assert_eq!(wrap_code_line("中中中", 5), vec!["中中", "中"]);
        assert_eq!(wrap_code_line("", 4), vec![""]);
    }
}
