//! Unit tests for the Stage A terminal hardening (SGR, graphemes, reflow,
//! modes, kitty keyboard state, OSC handling).

use super::grid::{cells_text, UnderlineStyle};
use super::*;

fn feed(t: &mut Terminal, bytes: &[u8]) {
    let mut p = vte::Parser::new();
    let mut h = AnsiHandler::new(t);
    for &b in bytes {
        p.advance(&mut h, b);
    }
}

fn term(cols: usize, rows: usize, s: &str) -> Terminal {
    let mut t = Terminal::new(cols, rows);
    feed(&mut t, s.as_bytes());
    t
}

fn row(t: &Terminal, r: usize) -> String {
    cells_text(&t.grid[r]).trim_end().to_string()
}

fn reply(t: &mut Terminal) -> String {
    let v: Vec<u8> = t.response_queue.drain(..).flatten().collect();
    String::from_utf8_lossy(&v).into_owned()
}

// ── SGR ──

#[test]
fn sgr_colon_truecolor_with_and_without_colorspace() {
    let t = term(20, 2, "\x1b[38:2::255:100:0mA\x1b[38:2:1:2:3mB\x1b[48:2::9:8:7mC\x1b[38:5:200mD");
    assert!(t.grid[0][0].fg == Color::Rgb(255, 100, 0));
    assert!(t.grid[0][1].fg == Color::Rgb(1, 2, 3));
    assert!(t.grid[0][2].bg == Color::Rgb(9, 8, 7));
    assert!(t.grid[0][3].fg == Color::Indexed(200));
}

#[test]
fn sgr_semicolon_forms_still_work() {
    let t = term(20, 2, "\x1b[38;2;10;20;30;1mA\x1b[48;5;99;4mB");
    assert!(t.grid[0][0].fg == Color::Rgb(10, 20, 30));
    assert!(t.grid[0][0].bold());
    assert!(t.grid[0][1].bg == Color::Indexed(99));
    assert!(t.grid[0][1].underline());
}

#[test]
fn sgr_underline_styles_and_color() {
    let mut t = term(20, 2, "\x1b[4:3mA\x1b[4:0mB\x1b[4:2mC\x1b[4:4mD\x1b[4:5mE\x1b[0m\x1b[21mF");
    let ul = |t: &Terminal, c: usize| t.grid[0][c].ul_style();
    assert_eq!(ul(&t, 0), UnderlineStyle::Curly);
    assert_eq!(ul(&t, 1), UnderlineStyle::None);
    assert!(!t.grid[0][1].underline());
    assert_eq!(ul(&t, 2), UnderlineStyle::Double);
    assert_eq!(ul(&t, 3), UnderlineStyle::Dotted);
    assert_eq!(ul(&t, 4), UnderlineStyle::Dashed);
    assert_eq!(ul(&t, 5), UnderlineStyle::Double);
    feed(&mut t, b"\x1b[0m\x1b[4m\x1b[58:2::1:2:3mG\x1b[58;5;7mH\x1b[59mI");
    assert!(t.grid[0][6].underline_color() == Some(Color::Rgb(1, 2, 3)));
    assert!(t.grid[0][7].underline_color() == Some(Color::Indexed(7)));
    assert!(t.grid[0][8].underline_color().is_none());
}

#[test]
fn sgr_flags_pair_correctly() {
    let t = term(
        30,
        2,
        "\x1b[9mA\x1b[29mB\x1b[53mC\x1b[55mD\x1b[5mE\x1b[25mF\x1b[8mG\x1b[28mH\x1b[1;2mI\x1b[22mJ\x1b[1m\x1b[21mK",
    );
    let a = |c: usize| t.grid[0][c].attrs();
    assert!(a(0).strikethrough && !a(1).strikethrough);
    assert!(a(2).overline && !a(3).overline);
    assert!(a(4).blink && !a(5).blink);
    assert!(a(6).hidden && !a(7).hidden);
    assert!(a(8).bold && a(8).dim);
    assert!(!a(9).bold && !a(9).dim);
    // SGR 21 is double underline, not "bold off".
    assert!(a(10).bold && a(10).ul_style() == UnderlineStyle::Double);
}

#[test]
fn xtmodkeys_is_not_sgr() {
    let t = term(10, 2, "\x1b[>4;2mA");
    assert!(!t.grid[0][0].underline());
}

// ── Graphemes ──

#[test]
fn combining_mark_takes_no_cell() {
    let t = term(20, 2, "e\u{301}x");
    assert_eq!(t.cursor_col, 2);
    assert_eq!(t.grid[0][0].c, 'e');
    assert_eq!(t.grid[0][0].text(), "e\u{301}");
    assert_eq!(t.grid[0][1].c, 'x');
}

#[test]
fn zwj_sequence_is_one_wide_cluster() {
    let t = term(20, 2, "👩\u{200d}💻x");
    assert_eq!(t.cursor_col, 3);
    assert_eq!(t.grid[0][0].text(), "👩\u{200d}💻");
    assert_eq!(t.grid[0][1].c, '\0');
    assert_eq!(t.grid[0][2].c, 'x');
}

#[test]
fn vs16_widens_and_skin_tone_and_flags_attach() {
    let t = term(20, 2, "❤\u{fe0f}x");
    assert_eq!(t.cursor_col, 3);
    assert_eq!(t.grid[0][0].text(), "❤\u{fe0f}");
    assert_eq!(t.grid[0][1].c, '\0');

    let t = term(20, 2, "👍\u{1F3FD}x");
    assert_eq!(t.cursor_col, 3);
    assert_eq!(t.grid[0][0].text(), "👍\u{1F3FD}");

    let t = term(20, 2, "🇺🇸🇯🇵");
    assert_eq!(t.grid[0][0].text(), "🇺🇸");
    assert_eq!(t.grid[0][2].text(), "🇯🇵");
    assert_eq!(t.cursor_col, 4);
}

#[test]
fn cluster_text_helpers() {
    let t = term(20, 2, "ae\u{301}b");
    assert_eq!(cells_text(&t.grid[0]).trim_end(), "ae\u{301}b");
}

#[test]
fn wide_char_at_last_column_wraps_with_spacer() {
    let t = term(4, 3, "abc你");
    assert_eq!(t.cursor_row, 1);
    assert_eq!(t.cursor_col, 2);
    assert!(t.grid[0][3].wrap());
    assert_eq!(t.grid[1][0].c, '你');
}

// ── Reflow ──

#[test]
fn reflow_narrow_then_wide_roundtrips_and_tracks_cursor() {
    let mut t = term(40, 6, "0123456789 abcdefghij ABCDEFGHIJ klmnopqrst\r\nsecond\r\n$ ");
    let before: Vec<String> = (0..6).map(|r| row(&t, r)).collect();
    t.resize(15, 6);
    assert_eq!(row(&t, 0), "0123456789 abcd");
    assert!(t.grid[0][14].wrap());
    // cursor stays right after the prompt
    let (cr, cc) = (t.cursor_row, t.cursor_col);
    assert_eq!(row(&t, cr).trim_end(), "$");
    assert_eq!(cc, 2);
    t.resize(40, 6);
    let after: Vec<String> = (0..6).map(|r| row(&t, r)).collect();
    assert_eq!(before, after);
    assert_eq!((t.cursor_row, t.cursor_col), (3, 2));
}

#[test]
fn reflow_pushes_overflow_into_scrollback_and_keeps_cursor_visible() {
    let mut t = term(20, 4, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\nprompt$ ");
    // 40 a's = 2 rows + prompt row = 3 rows; narrow to 10 => 4 + 1 rows
    t.resize(10, 4);
    assert!(t.scrollback.len() >= 1);
    assert_eq!(row(&t, t.cursor_row), "prompt$");
    assert_eq!(t.cursor_col, 8);
}

#[test]
fn wrap_next_cursor_survives_reflow() {
    let mut t = term(10, 3, "0123456789");
    assert!(t.wrap_next);
    t.resize(20, 3);
    feed(&mut t, b"X");
    assert_eq!(row(&t, 0), "0123456789X");
}

#[test]
fn shrink_rows_pushes_top_lines_and_grow_pulls_back() {
    let mut t = Terminal::new(10, 6);
    for i in 0..6 {
        feed(&mut t, format!("row{i}\r\n").as_bytes());
    }
    let sb0 = t.scrollback.len();
    t.resize(10, 3);
    assert!(t.scrollback.len() > sb0);
    assert_eq!(t.cursor_row, 2);
    let sb1 = t.scrollback.len();
    t.resize(10, 6);
    assert!(t.scrollback.len() < sb1);
    // everything is still there in order
    let all: Vec<String> = (0..t.scrollback.len() + t.rows)
        .map(|i| cells_text(t.abs_line(i).unwrap()).trim_end().to_string())
        .collect();
    let joined = all.join("|");
    assert!(joined.contains("row0|row1|row2|row3|row4|row5"), "{joined}");
}

#[test]
fn reflow_remaps_marks() {
    let mut t = Terminal::new(10, 8);
    feed(&mut t, b"aaaaaaaaaaaaaaaaaaaa\r\n");
    feed(&mut t, b"\x1b]133;A\x07$ ");
    let line_before = t.marks[0].line;
    assert_eq!(line_before, 2);
    t.resize(5, 8);
    // 20 a's now take 4 rows, so the prompt moved down by 2
    assert_eq!(t.marks[0].line, 4);
    assert_eq!(t.abs_cursor_line(), 4);
}

#[test]
fn resize_extremes_never_panic() {
    let mut t = Terminal::new(0, 0);
    assert_eq!((t.cols, t.rows), (1, 1));
    t.resize(0, 0);
    feed(&mut t, "wide 你好 text\r\n\r\n".as_bytes());
    t.resize(1, 100);
    t.resize(100, 1);
    t.resize(usize::MAX, usize::MAX);
    assert_eq!((t.cols, t.rows), (MAX_COLS, MAX_ROWS));
}

// ── Modes ──

#[test]
fn sync_output_2026() {
    let mut t = term(10, 2, "\x1b[?2026h");
    assert!(t.sync_pending());
    feed(&mut t, b"\x1b[?2026$p");
    assert_eq!(reply(&mut t), "\x1b[?2026;1$y");
    std::thread::sleep(std::time::Duration::from_millis(170));
    assert!(!t.sync_pending(), "safety timeout");
    feed(&mut t, b"\x1b[?2026l");
    assert!(!t.sync_pending());
    feed(&mut t, b"\x1b[?2026$p");
    assert_eq!(reply(&mut t), "\x1b[?2026;2$y");
}

#[test]
fn origin_mode_is_relative_to_scroll_region() {
    let mut t = term(10, 10, "\x1b[3;6r\x1b[?6h\x1b[1;1HX");
    assert_eq!(t.cursor_row, 2 + 0);
    assert_eq!(t.grid[2][0].c, 'X');
    feed(&mut t, b"\x1b[99;1H");
    assert_eq!(t.cursor_row, 5);
    feed(&mut t, b"\x1b[6n");
    assert_eq!(reply(&mut t), "\x1b[4;1R");
    feed(&mut t, b"\x1b[?6l\x1b[1;1H");
    assert_eq!(t.cursor_row, 0);
}

#[test]
fn autowrap_off_overwrites_last_column() {
    let mut t = term(5, 2, "\x1b[?7labcdefgh");
    assert_eq!(row(&t, 0), "abcdh");
    assert_eq!(t.cursor_row, 0);
    feed(&mut t, b"\x1b[?7h\x1b[1;1Habcdefg");
    assert_eq!(row(&t, 0), "abcde");
    assert_eq!(row(&t, 1), "fg");
}

#[test]
fn decsc_decrc_saves_attrs_charset_origin() {
    let mut t = term(10, 5, "\x1b[?6h\x1b[3;3H\x1b[1;31m\x1b(0\x1b7");
    feed(&mut t, b"\x1b[0m\x1b(B\x1b[?6l\x1b[1;1H\x1b8x");
    assert!(t.attrs.bold);
    assert!(t.fg == Color::Indexed(1));
    assert!(t.g0_charset == Charset::LineDrawing);
    assert!(t.origin_mode);
    assert_eq!(t.grid[2][2].c, '│'); // 'x' through the restored line-drawing charset
}

#[test]
fn mode_1048_saves_and_restores_cursor() {
    let t = term(10, 5, "\x1b[3;4H\x1b[?1048h\x1b[1;1H\x1b[?1048l");
    assert_eq!((t.cursor_row, t.cursor_col), (2, 3));
}

#[test]
fn decscnm_reverse_screen() {
    let mut t = term(10, 2, "\x1b[?5h");
    assert!(t.reverse_screen);
    feed(&mut t, b"\x1b[?5l");
    assert!(!t.reverse_screen);
}

#[test]
fn ed3_clears_scrollback_only() {
    let mut t = Terminal::new(10, 3);
    for i in 0..10 {
        feed(&mut t, format!("l{i}\r\n").as_bytes());
    }
    assert!(t.scrollback.len() > 0);
    feed(&mut t, b"visible\x1b[3J");
    assert_eq!(t.scrollback.len(), 0);
    assert!(row(&t, t.cursor_row).starts_with("visible"));
    // ED 2 leaves the scrollback alone
    for i in 0..10 {
        feed(&mut t, format!("m{i}\r\n").as_bytes());
    }
    let n = t.scrollback.len();
    feed(&mut t, b"\x1b[2J");
    assert_eq!(t.scrollback.len(), n);
}

#[test]
fn tab_stops_hts_tbc_cht_cbt() {
    let mut t = term(40, 2, "\x1b[3g\x1b[1;5H\x1bH\x1b[1;12H\x1bH\x1b[1;1H");
    feed(&mut t, b"\t");
    assert_eq!(t.cursor_col, 4);
    feed(&mut t, b"\t");
    assert_eq!(t.cursor_col, 11);
    feed(&mut t, b"\x1b[Z");
    assert_eq!(t.cursor_col, 4);
    feed(&mut t, b"\x1b[2I");
    assert_eq!(t.cursor_col, 39);
    feed(&mut t, b"\x1b[2Z");
    assert_eq!(t.cursor_col, 4);
    // TBC 0 clears the stop under the cursor
    feed(&mut t, b"\x1b[0g\x1b[1;1H\t");
    assert_eq!(t.cursor_col, 11);
}

#[test]
fn rep_and_hpa_vpr_hpr() {
    let mut t = term(20, 5, "ab\x1b[3b");
    assert_eq!(row(&t, 0), "abbbb");
    feed(&mut t, b"\x1b[1;1H\x1b[5`");
    assert_eq!(t.cursor_col, 4);
    feed(&mut t, b"\x1b[2a");
    assert_eq!(t.cursor_col, 6);
    feed(&mut t, b"\x1b[2e");
    assert_eq!(t.cursor_row, 2);
}

#[test]
fn decstr_soft_reset() {
    let mut t = term(10, 5, "\x1b[1;31m\x1b[?7l\x1b[?6h\x1b[2;4r\x1b[4h\x1b[?25l");
    feed(&mut t, b"\x1b[!p");
    assert!(t.autowrap && !t.origin_mode && !t.insert_mode && t.cursor_visible);
    assert!(!t.attrs.bold && t.fg == Color::Default);
    assert_eq!((t.scroll_top, t.scroll_bottom), (0, 4));
}

#[test]
fn title_stack_and_xtversion_and_da() {
    let mut t = term(10, 2, "\x1b]2;one\x07\x1b[22;0t\x1b]2;two\x07");
    assert_eq!(t.title.as_deref(), Some("two"));
    feed(&mut t, b"\x1b[23;0t");
    assert_eq!(t.title.as_deref(), Some("one"));
    feed(&mut t, b"\x1b[>q");
    let r = reply(&mut t);
    assert!(r.starts_with("\x1bP>|Rift ") && r.ends_with("\x1b\\"), "{r:?}");
    feed(&mut t, b"\x1b[c\x1b[>c");
    let r = reply(&mut t);
    assert!(r.contains("\x1b[?62;") && r.contains("\x1b[>"), "{r:?}");
}

#[test]
fn decrqm_reports_known_and_unknown_modes() {
    let mut t = term(10, 2, "\x1b[?7$p\x1b[?25l\x1b[?25$p\x1b[?9999$p\x1b[4$p");
    assert_eq!(reply(&mut t), "\x1b[?7;1$y\x1b[?25;2$y\x1b[?9999;0$y\x1b[4;2$y");
}

#[test]
fn mouse_modes_track_independently() {
    let mut t = term(10, 2, "\x1b[?1000h\x1b[?1002h");
    assert!(t.mouse_mode == MouseMode::ButtonTrack);
    feed(&mut t, b"\x1b[?1002l");
    assert!(t.mouse_mode == MouseMode::Press, "disabling 1002 must not kill 1000");
    feed(&mut t, b"\x1b[?1003h");
    assert!(t.mouse_mode == MouseMode::AnyEvent);
    feed(&mut t, b"\x1b[?1003l\x1b[?1000l");
    assert!(t.mouse_mode == MouseMode::None);
}

// ── Kitty keyboard ──

#[test]
fn kitty_keyboard_stack() {
    let mut t = term(10, 2, "");
    assert_eq!(t.kitty_keyboard_flags(), 0);
    feed(&mut t, b"\x1b[>1u");
    assert_eq!(t.kitty_keyboard_flags(), 1);
    feed(&mut t, b"\x1b[>11u");
    assert_eq!(t.kitty_keyboard_flags(), 11);
    assert_eq!(t.kitty_keyboard_stack_depth(), 2);
    feed(&mut t, b"\x1b[?u");
    assert_eq!(reply(&mut t), "\x1b[?11u");
    feed(&mut t, b"\x1b[=4;2u");
    assert_eq!(t.kitty_keyboard_flags(), 15);
    feed(&mut t, b"\x1b[=1;3u");
    assert_eq!(t.kitty_keyboard_flags(), 14);
    feed(&mut t, b"\x1b[=5;1u");
    assert_eq!(t.kitty_keyboard_flags(), 5);
    feed(&mut t, b"\x1b[<u");
    assert_eq!(t.kitty_keyboard_flags(), 1);
    feed(&mut t, b"\x1b[<5u");
    assert_eq!(t.kitty_keyboard_flags(), 0);
    assert_eq!(t.kitty_keyboard_stack_depth(), 0);
}

#[test]
fn kitty_keyboard_stacks_are_per_screen() {
    let mut t = term(10, 2, "\x1b[>3u");
    feed(&mut t, b"\x1b[?1049h");
    assert_eq!(t.kitty_keyboard_flags(), 0);
    feed(&mut t, b"\x1b[>8u");
    assert_eq!(t.kitty_keyboard_flags(), 8);
    feed(&mut t, b"\x1b[?1049l");
    assert_eq!(t.kitty_keyboard_flags(), 3);
}

// ── OSC ──

#[test]
fn osc8_hyperlinks() {
    let mut t = term(30, 3, "a\x1b]8;;https://x.test/p?q=1;2\x1b\\link\x1b]8;;\x1b\\z");
    assert_eq!(t.hyperlink_at(0, 0), None);
    assert_eq!(t.hyperlink_at(0, 1), Some("https://x.test/p?q=1;2"));
    assert_eq!(t.hyperlink_at(0, 3), Some("https://x.test/p?q=1;2"));
    assert_eq!(t.hyperlink_at(0, 4), Some("https://x.test/p?q=1;2"));
    assert_eq!(t.hyperlink_at(0, 5), None);
    assert_eq!(row(&t, 0), "alinkz");
    // ids dedupe and survive reflow/scroll
    feed(&mut t, b"\r\n\x1b]8;id=k;https://y.test\x07w\x1b]8;;\x07");
    assert_eq!(t.hyperlink_at(1, 0), Some("https://y.test"));
}

#[test]
fn osc9_and_osc777_notifications() {
    let mut t = term(10, 2, "\x1b]9;build done\x07\x1b]777;notify;Title;Body;more\x1b\\\x1b]9;4;1;50\x07");
    let n = t.take_notifications();
    assert_eq!(n.len(), 2);
    assert_eq!(n[0], Notification { title: String::new(), body: "build done".into() });
    assert_eq!(n[1], Notification { title: "Title".into(), body: "Body;more".into() });
    assert!(t.take_notifications().is_empty());
}

#[test]
fn osc_color_queries() {
    let mut t = term(10, 2, "");
    t.set_reported_colors((0xff, 0x80, 0x00), (0x01, 0x02, 0x03), (0xaa, 0xbb, 0xcc));
    feed(&mut t, b"\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]12;?\x07");
    assert_eq!(
        reply(&mut t),
        "\x1b]10;rgb:ffff/8080/0000\x07\x1b]11;rgb:0101/0202/0303\x1b\\\x1b]12;rgb:aaaa/bbbb/cccc\x07"
    );
}

#[test]
fn osc4_set_query_reset() {
    let mut t = term(10, 2, "");
    t.set_reported_palette(&[(1, 2, 3)]);
    feed(&mut t, b"\x1b]4;0;?\x07");
    assert_eq!(reply(&mut t), "\x1b]4;0;rgb:0101/0202/0303\x07");
    let g0 = t.palette_gen;
    feed(&mut t, b"\x1b]4;1;rgb:ff/00/80;2;#00ff00\x07");
    assert_eq!(t.palette_override(1), Some((255, 0, 128)));
    assert_eq!(t.palette_override(2), Some((0, 255, 0)));
    assert!(t.palette_gen != g0);
    feed(&mut t, b"\x1b]4;1;?\x07");
    assert_eq!(reply(&mut t), "\x1b]4;1;rgb:ffff/0000/8080\x07");
    feed(&mut t, b"\x1b]104;1\x07");
    assert_eq!(t.palette_override(1), None);
    assert_eq!(t.palette_override(2), Some((0, 255, 0)));
    feed(&mut t, b"\x1b]104\x07");
    assert_eq!(t.palette_override(2), None);
}

#[test]
fn osc1_icon_title() {
    let t = term(10, 2, "\x1b]1;icon\x07\x1b]2;win\x07");
    assert_eq!(t.icon_title.as_deref(), Some("icon"));
    assert_eq!(t.title.as_deref(), Some("win"));
}

// ── Parameter caps ──

#[test]
fn huge_repeat_counts_are_cheap() {
    let mut t = Terminal::new(200, 50);
    let start = std::time::Instant::now();
    for seq in ["\x1b[65535S", "\x1b[65535T", "\x1b[65535L", "\x1b[65535M", "\x1b[65535@", "\x1b[65535P", "\x1b[65535X", "a\x1b[65535b"] {
        for _ in 0..20 {
            feed(&mut t, seq.as_bytes());
        }
    }
    assert!(start.elapsed() < std::time::Duration::from_millis(200), "{:?}", start.elapsed());
    assert!(t.scrollback.len() <= 2100, "{}", t.scrollback.len());
}

// ── Compact scrollback / extent hints ──

/// Padded text of scrollback row `i` (rows are stored without their blank tail).
fn sb_text(t: &Terminal, i: usize) -> String {
    let mut r = t.scrollback[i].clone();
    r.resize(t.cols, Cell::default());
    cells_text(&r).trim_end().to_string()
}

#[test]
fn scrollback_rows_are_stored_trimmed_with_content_intact() {
    let mut t = Terminal::new(40, 3);
    for i in 0..10 {
        feed(&mut t, format!("line {i}\r\n").as_bytes());
    }
    assert_eq!(t.scrollback.len(), 8);
    for i in 0..8 {
        assert_eq!(sb_text(&t, i), format!("line {i}"));
        assert_eq!(t.scrollback[i].len(), format!("line {i}").len(), "row {i} keeps only its used cells");
    }
    // An empty line is an empty row.
    feed(&mut t, b"\r\n\r\n\r\n\r\n\r\n");
    assert!(t.scrollback.iter().rev().take(2).all(|r| r.is_empty()));
    // The recycled screen rows are fully blank again (no stale text from hints).
    feed(&mut t, b"\r\n\r\n\r\n");
    assert!(t.grid.iter().all(|r| r.iter().all(|c| c.is_default_blank())));
}

#[test]
fn scrollback_keeps_styled_blanks_wraps_and_wide_cells() {
    let mut t = Terminal::new(10, 2);
    // Colored blank tail (BCE-style) must survive trimming; trailing default blanks must not.
    feed(&mut t, b"\x1b[44mab\x1b[K\x1b[0m\r\n");
    // Soft-wrapped line: the wrap flag lives on the (full-width) row's last cell.
    feed(&mut t, b"0123456789wrapped\r\n");
    // Wide char + combining mark + link-ish extras.
    feed(&mut t, "\u{4e2d}e\u{301}\r\n".as_bytes());
    feed(&mut t, b"\r\n\r\n");
    let r0 = &t.scrollback[0];
    assert_eq!(r0.len(), 10, "colored blanks are content");
    assert!(r0[5].bg == Color::Indexed(4) && r0[9].bg == Color::Indexed(4));
    let r1 = &t.scrollback[1];
    assert_eq!(r1.len(), 10);
    assert!(r1.last().unwrap().wrap());
    assert_eq!(sb_text(&t, 2), "wrapped");
    assert_eq!(sb_text(&t, 3), "\u{4e2d}e\u{301}");
    assert_eq!(t.scrollback[3][0].c, '\u{4e2d}');
    assert_eq!(t.scrollback[3][1].c, '\0');
    assert_eq!(t.scrollback[3][2].extra(), "\u{301}");
}

#[test]
fn scroll_region_and_alt_screen_keep_hints_consistent() {
    let mut t = Terminal::new(12, 5);
    feed(&mut t, b"top\r\nmid\r\nbot\x1b[2;4r");
    // Scroll inside a region (rows 2..4): content leaving the region is not lost or duplicated.
    feed(&mut t, b"\x1b[4;1Hx\n\ny\n");
    let rows: Vec<String> = (0..5).map(|r| row(&t, r)).collect();
    assert_eq!(rows[0], "top");
    // Alt screen: scrolling there never feeds scrollback and leaves no residue.
    let before = t.scrollback.len();
    feed(&mut t, b"\x1b[?1049h\x1b[2J\x1b[Halt\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n");
    assert_eq!(t.scrollback.len(), before);
    feed(&mut t, b"\x1b[?1049l");
    assert_eq!(row(&t, 0), "top");
    feed(&mut t, b"\x1b[?1049h");
    assert!(t.grid.iter().all(|r| r.iter().all(|c| c.is_default_blank())), "alt screen re-entered clean");
}

#[test]
fn direct_grid_writes_are_never_lost_by_extent_hints() {
    // Code that pokes `grid` directly (tests, tools) bypasses the tracked fast paths;
    // the hints must notice and the cell must still reach scrollback.
    let mut t = Terminal::new(20, 3);
    t.grid[0][17].c = 'Z';
    t.grid[0][17].fg = Color::Indexed(3);
    feed(&mut t, b"\x1b[3;1H\n");
    assert_eq!(t.scrollback.len(), 1);
    assert_eq!(t.scrollback[0].len(), 18);
    assert_eq!(t.scrollback[0][17].c, 'Z');
    // And again after the hints were rebuilt and tracked writes happened in between.
    feed(&mut t, b"abc");
    t.grid[0][15].c = 'Q';
    feed(&mut t, b"\n");
    assert_eq!(t.scrollback[1].len(), 16);
    assert_eq!(t.scrollback[1][15].c, 'Q');
}

#[test]
fn readers_handle_trimmed_scrollback_rows() {
    use crate::tools::search::SearchOverlay;
    use crate::window::Selection;
    let mut t = Terminal::new(30, 3);
    feed(&mut t, b"alpha beta  \r\nsecond line\r\nthird\r\n\r\nfifth\r\nsixth\r\n");
    assert!(t.scrollback.len() >= 4);
    assert!(t.scrollback.iter().all(|r| r.len() < 30));
    // Selection across trimmed rows (including a click far past the end of text).
    let mut sel = Selection::new();
    sel.start_at(0, 6);
    sel.extend_to(1, 29);
    assert_eq!(sel.extract_text(|r| t.abs_line(r)), "beta\nsecond line");
    let (a, b) = crate::window::selection::word_at(&t, 0, 25);
    assert_eq!((a.0, b.0), (0, 0));
    assert_eq!((a.1, b.1), (10, 29), "blank tail selects the whitespace run to the right edge");
    let (la, lb) = crate::window::selection::line_at(&t, 1);
    assert_eq!((la, lb), ((1, 0), (1, 29)));
    // Search sees trailing blanks of a trimmed row, and text on both kinds of rows.
    let mut so = SearchOverlay::new();
    so.query = "beta  ".to_string();
    so.search(&t.scrollback, &t.grid);
    assert_eq!(so.matches.len(), 1);
    so.query = "sixth".to_string();
    so.search(&t.scrollback, &t.grid);
    assert_eq!(so.matches.len(), 1);
    // Output text (copy output) and session scrollback extraction.
    let out = crate::blocks_ui::output_text(&t, 0, 2);
    assert_eq!(out, "alpha beta\nsecond line\nthird");
    // Hyperlink / cwd lookups on a short row do not index out of range.
    assert!(t.hyperlink_at_abs(0, 28).is_none());
}
