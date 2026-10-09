//! Item 6: URL detection, search, selection on tricky content.
use super::{pane, Soft};
use crate::tools::search::SearchOverlay;
use crate::tools::url_detect::{detect_urls, url_at_col};
use crate::window::selection::{word_range, Selection};

#[test]
fn url_detection_table() {
    let mut s = Soft::new("url");
    let cases: &[(&str, &str, Option<&str>)] = &[
        ("plain", "see https://example.com/a now", Some("https://example.com/a")),
        ("trailing_dot", "go to https://example.com/a.", Some("https://example.com/a")),
        ("trailing_comma", "https://example.com/a, then", Some("https://example.com/a")),
        ("trailing_question", "is it https://example.com/a?", Some("https://example.com/a")),
        ("trailing_paren_wrapping", "(https://example.com/x)", Some("https://example.com/x")),
        ("markdown_link", "[l](https://example.com/p)", Some("https://example.com/p")),
        ("wikipedia_parens", "https://en.wikipedia.org/wiki/Rust_(programming_language)", Some("https://en.wikipedia.org/wiki/Rust_(programming_language)")),
        ("wiki_parens_then_dot", "see https://en.wikipedia.org/wiki/Rust_(language).", Some("https://en.wikipedia.org/wiki/Rust_(language)")),
        ("angle", "<https://a.com/x>", Some("https://a.com/x")),
        ("dq", "\"https://a.com/x\"", Some("https://a.com/x")),
        ("sq", "'https://a.com/x'", Some("https://a.com/x")),
        ("query_frag", "https://a.com/x?y=1&z=2#f", Some("https://a.com/x?y=1&z=2#f")),
        ("port_localhost", "http://localhost:3000/", Some("http://localhost:3000/")),
        ("ipv6", "http://[::1]:8080/x", Some("http://[::1]:8080/x")),
        ("userinfo", "https://user:pw@host.io/p", Some("https://user:pw@host.io/p")),
        ("cjk_path", "https://a.com/中文", Some("https://a.com/中文")),
        ("cjk_period", "详见https://a.com/x。后面", Some("https://a.com/x")),
        ("brackets_in_query", "https://a.com/?q[]=1", Some("https://a.com/?q[]=1")),
        ("json_embedded", "{\"url\":\"https://a.com/x\",\"b\":1}", Some("https://a.com/x")),
        ("short_host", "http://a/b", Some("http://a/b")),
        ("scheme_only", "https://", None),
        ("www_no_scheme", "www.example.com", None),
        ("ftp", "ftp://files.example.com/x", None),
        ("file_scheme", "file:///etc/hosts", None),
        ("ssh_scheme", "ssh://git@github.com/x/y.git", None),
        ("mailto", "mailto:a@b.co", None),
    ];
    for (id, line, want) in cases {
        let got = detect_urls(line).into_iter().next().map(|u| u.2);
        let ok = got.as_deref() == *want;
        s.check(id, ok, format!("{line:?} -> {got:?} (want {want:?})"));
    }
    // Wide-char row as the mouse/selection paths build it (one char per cell, '\0' for the right half
    // of a wide glyph): "https://a.com/" is cols 0-13, 中 = 14-15, 文 = 16-17. A click on the last
    // cell of the URL (col 17) must return the whole URL. (The original probe used col 20, which
    // is inside " end" and cannot belong to the URL under any column mapping.)
    let line_cells = "https://a.com/中\0文\0 end";
    let hit = url_at_col(line_cells, 17);
    s.check("click_on_url_with_wide_chars_returns_whole_url", hit.as_deref() == Some("https://a.com/中文"), format!("clicked col 17 -> {hit:?}; renderer underlines the whole thing and click must open the same"));
    s.finish();
}

#[test]
fn word_range_paths_and_urls() {
    let mut s = Soft::new("selection");
    let mut p = pane(80, 5);
    p.feed(b"error at src/main.rs:42:5 (see https://a.com/x_(y)) ok\r\n");
    let row = p.terminal.grid[0].clone();
    let (a, b) = word_range(&row, 15);
    let w: String = row[a..=b].iter().map(|c| c.c).collect();
    s.check("path_line_col_selected_whole", w == "src/main.rs:42:5", format!("double-click in path selects {w:?}"));
    let col = 38;
    let (a, b) = word_range(&row, col);
    let w: String = row[a..=b].iter().map(|c| c.c).collect();
    s.check("url_with_parens_selected_whole", w == "https://a.com/x_(y)", format!("{w:?}"));
    let mut p = pane(80, 5);
    p.feed("中文路径/文件.txt 和 x".as_bytes());
    let row = p.terminal.grid[0].clone();
    let (a, b) = word_range(&row, 1);
    let w: String = row[a..=b].iter().filter(|c| c.c != '\0').map(|c| c.c).collect();
    s.check("cjk_word_selection", w == "中文路径/文件.txt", format!("{w:?}"));
    s.finish();
}

#[test]
fn search_unicode_and_panics() {
    let mut s = Soft::new("search");
    let run = |text: &str, q: &str| -> Result<Vec<(usize, usize, usize)>, String> {
        let text = text.to_string();
        let q = q.to_string();
        std::panic::catch_unwind(move || {
            let mut p = pane(40, 4);
            p.feed(text.as_bytes());
            let mut so = SearchOverlay::new();
            so.query = q;
            so.search(&p.terminal.scrollback, &p.terminal.grid);
            so.matches.iter().map(|m| (m.row, m.col_start, m.col_end)).collect::<Vec<_>>()
        })
        .map_err(|e| e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default())
    };
    let r = run("ééé", "é");
    s.check("repeat_multibyte_match_no_panic", r.is_ok(), format!("search 'é' in 'ééé' -> {r:?}"));
    let r = run("日本語 日本語", "日本");
    s.check("cjk_no_panic", r.is_ok(), format!("{r:?}"));
    if let Ok(m) = &r {
        // 日(0-1)本(2-3)語(4-5) space(6) 日(7-8) -> second match should start at cell col 7
        s.check("cjk_match_column_is_cell_column", m.iter().any(|x| x.1 == 7), format!("matches (row,col_start,col_end)={m:?}; expected a match starting at cell col 7"));
    }
    let r = run("naïve café", "café");
    if let Ok(m) = &r {
        s.check("accent_match_col", m.first().map(|x| x.1) == Some(6), format!("{m:?} (expect col 6; byte offset would be 7)"));
    } else {
        s.check("accent_no_panic", false, format!("{r:?}"));
    }
    let r = run("ASCII Hello hello HELLO", "hello");
    s.check("ascii_case_insensitive_3", r.as_ref().map(|m| m.len()) == Ok(3), format!("{r:?}"));
    let r = run("İstanbul", "i");
    s.check("turkish_dotted_I_no_panic", r.is_ok(), format!("{r:?}"));
    // wrapped match across rows
    let r = run(&format!("{}needle", "x".repeat(37)), "needle");
    s.check("match_across_soft_wrap_found", r.as_ref().map_or(false, |m| !m.is_empty()), format!("'needle' split across a wrapped row -> {r:?}"));
    // perf: 10k scrollback lines
    let mut p = pane(120, 30);
    let mut buf = String::new();
    for i in 0..12_000 { buf += &format!("line {i} lorem ipsum dolor sit amet consectetur adipiscing elit\r\n"); }
    p.feed(buf.as_bytes());
    let mut so = SearchOverlay::new();
    so.query = "ipsum".into();
    let t0 = std::time::Instant::now();
    so.search(&p.terminal.scrollback, &p.terminal.grid);
    let ms = t0.elapsed().as_millis();
    s.check("search_10k_lines_fast", ms < 200 && so.matches.len() >= 10_000, format!("{} matches in {ms}ms", so.matches.len()));
    s.finish();
}

#[test]
fn selection_copy_of_wrapped_and_wide() {
    let mut s = Soft::new("selection");
    let mut p = pane(40, 6);
    let url = "https://example.com/a/very/long/path/that/definitely/wraps/around";
    p.feed(format!("{url}\r\n中文字符测试\r\n").as_bytes());
    let t = &p.terminal;
    let mut sel = Selection::new();
    sel.start_at(0, 0);
    sel.extend_to(1, 39);
    sel.finish();
    let txt = sel.extract_text(|r| t.abs_line(r));
    s.check("copy_selection_over_soft_wrap_has_no_newline_inside_url", txt.lines().next() == Some(url), format!("copied first line = {:?}", txt.lines().next()));
    let mut sel = Selection::new();
    sel.start_at(2, 0);
    sel.extend_to(2, 11);
    sel.finish();
    let txt = sel.extract_text(|r| t.abs_line(r));
    s.check("copy_wide_chars", txt == "中文字符测试", format!("{txt:?}"));
    // select starting on the right half of a wide char
    let mut sel = Selection::new();
    sel.start_at(2, 1);
    sel.extend_to(2, 4);
    sel.finish();
    let txt = sel.extract_text(|r| t.abs_line(r));
    s.info("start_on_right_half", format!("{txt:?}"));
    s.finish();
}
