//! Item 2 (+ critic H7/H8/H9): feed real-world byte streams, render offscreen to PNG.
use super::{pane, scratch_dir, write_png, Soft};
use crate::renderer::Renderer;
use crate::window::{PaneRect, WindowManager};

pub fn make_renderer(size: f32) -> Renderer {
    let cfg = crate::config::toml::load_config();
    let font = crate::config::resolve_font_path(&cfg);
    Renderer::new(&font, size, cfg.theme.clone())
}

pub fn render_bytes(name: &str, cols: usize, rows: usize, bytes: &[u8], size: f32) -> (Vec<u32>, usize, usize) {
    let mut wm = WindowManager::headless(cols, rows);
    wm.active_pane_mut().feed(bytes);
    let mut r = make_renderer(size);
    let (cw, ch) = (r.cell_width(), r.cell_height());
    let (w, h) = (cols * cw, rows * ch);
    let mut buf = vec![0u32; w * h];
    let blocks = crate::tools::blocks::BlockManager::new();
    r.render_tabbed(&wm, PaneRect { x: 0, y: 0, width: w, height: h }, &mut buf, w as u32, h as u32, &blocks);
    if !name.is_empty() {
        let p = scratch_dir("png").join(format!("{name}.png"));
        write_png(&p, w, h, &buf);
        println!("AUDIT|render|{name}|INFO|png={} {w}x{h} cell={cw}x{ch}", p.display());
    }
    (buf, w, h)
}

fn crlf(s: &[u8]) -> Vec<u8> {
    let mut o = Vec::new();
    for &b in s {
        if b == b'\n' { o.extend_from_slice(b"\r\n"); } else { o.push(b); }
    }
    o
}

fn sh(cmd: &str) -> Vec<u8> {
    let out = std::process::Command::new("/bin/sh").arg("-c").arg(cmd).env("CLICOLOR_FORCE", "1").env("TERM", "xterm-256color").output().unwrap();
    crlf(&out.stdout)
}

#[test]
fn real_command_output_scenes() {
    let mut s = Soft::new("render");
    let mut b = Vec::new();
    b.extend(b"\x1b[1;32m$\x1b[0m ls -laG /usr/local/bin | head -12\r\n");
    b.extend(sh("ls -laG /usr/local/bin | head -12"));
    b.extend(b"\x1b[1;32m$\x1b[0m git log --graph --color=always --oneline -n 14\r\n");
    b.extend(sh("git -C /Users/nongjiawu/playground/research/cap log --graph --color=always --decorate --oneline -n 14 2>&1"));
    b.extend(b"\x1b[1;32m$\x1b[0m top -l 1 | head -n 14\r\n");
    b.extend(sh("top -l 1 | head -n 14"));
    let (buf, w, h) = render_bytes("01_ls_git_top", 130, 50, &b, 15.0);
    s.check("rendered_nonblank", buf.iter().any(|p| *p != buf[0]) && w > 0 && h > 0, "image has content");
    s.finish();
}

#[test]
fn vim_like_alt_screen() {
    let mut s = Soft::new("render");
    let mut p = pane(80, 24);
    p.feed(b"main screen line 1\r\nmain screen line 2\r\n$ vim x.c");
    let main_before = super::screen_text(&p.terminal);
    let mut v = Vec::new();
    v.extend(b"\x1b[?1049h\x1b[?1h\x1b=\x1b[2J\x1b[H");
    for i in 1..=22 { v.extend(format!("\x1b[{i};1H\x1b[34m~\x1b[0m").as_bytes()); }
    v.extend(b"\x1b[1;1H#include <stdio.h>\x1b[2;1Hint \x1b[1;33mmain\x1b[0m(void) {\x1b[3;1H    printf(\x1b[32m\"hi\"\x1b[0m);\x1b[4;1H}");
    v.extend(b"\x1b[24;1H\x1b[7m x.c                                              4,2          All \x1b[0m\x1b[4;2H");
    v.extend(b"\x1b[5;3r\x1b[5;10r\x1b[2J\x1b[H"); // odd scroll region settings (invalid then valid)
    p.feed(&v);
    s.check("alt_screen_active", p.terminal.is_alt_screen(), "");
    p.feed(b"\x1b[?1049l\x1b[?1l\x1b>");
    s.check("alt_screen_exit_restores_main", !p.terminal.is_alt_screen() && super::screen_text(&p.terminal) == main_before, format!("before={main_before:?}\nafter={:?}", super::screen_text(&p.terminal)));
    s.check("no_scrollback_from_alt", p.terminal.scrollback.len() == 0, format!("{}", p.terminal.scrollback.len()));
    // render the alt screen frame too
    let mut v2 = Vec::new();
    v2.extend(b"\x1b[?1049h\x1b[2J\x1b[H");
    for i in 1..=22 { v2.extend(format!("\x1b[{i};1H\x1b[34m~\x1b[0m").as_bytes()); }
    v2.extend(b"\x1b[1;1H#include <stdio.h>\x1b[2;1Hint \x1b[1;33mmain\x1b[0m(void) {\x1b[24;1H\x1b[7m x.c                                              4,2          All \x1b[0m");
    render_bytes("02_vim_alt", 80, 24, &v2, 16.0);
    s.finish();
}

#[test]
fn unicode_scene_and_cell_math() {
    let mut s = Soft::new("render");
    let mut t = String::new();
    t += "ASCII  : Hello World 0123456789\r\n";
    t += "CJK    : 你好，世界 こんにちは 한국어 [end]\r\n";
    t += "Emoji  : 😀🎉🚀 🇨🇳🇺🇸 ❤️ 👍🏽 [end]\r\n";
    t += "ZWJ    : 👩‍💻 👨‍👩‍👧 [end]\r\n";
    t += "Combine: e\u{301} a\u{308} n\u{303} \u{5d0}\u{5b8} [end]\r\n";
    t += "Nerd   : \u{e0b0} \u{e0b2} \u{f015} \u{e725} \u{f7a1} \u{f121} [end]\r\n";
    t += "Box    : ┌─┬─┐│ ││└─┴─┘ ▀▄█▓▒░ ⣿⣷ [end]\r\n";
    t += "Full   : ＡＢＣ１２３ [end]\r\n";
    t += "Tabs   : a\tb\tc\td [end]\r\n";
    t += "RTL    : שלום עולם مرحبا [end]\r\n";
    render_bytes("03_unicode", 60, 14, t.as_bytes(), 18.0);
    // cell math
    let cursor_after = |text: &str| {
        let mut p = pane(40, 3);
        p.feed(text.as_bytes());
        (p.terminal.cursor_col, p.terminal.cursor_row)
    };
    let (c, _) = cursor_after("e\u{301}x");
    s.check("combining_mark_takes_no_cell", c == 2, format!("'e'+U+0301+'x' leaves cursor at col {c} (expect 2: e(0) x(1))"));
    let (c, _) = cursor_after("👩\u{200d}💻");
    s.check("zwj_sequence_is_one_wide_cluster", c == 2, format!("woman+ZWJ+laptop leaves cursor at col {c} (expect 2 for one emoji cluster)"));
    let (c, _) = cursor_after("❤\u{fe0f}");
    s.check("vs16_emoji_two_cells", c == 2 || c == 1, format!("heart+VS16 -> col {c}"));
    let mut p = pane(40, 3);
    p.feed("e\u{301}x".as_bytes());
    s.info("combining_grid", format!("cells: {:?}", p.terminal.grid[0].iter().take(4).map(|c| c.c).collect::<Vec<_>>()));
    let (c, _) = cursor_after("你好");
    s.check("cjk_two_cells_each", c == 4, format!("{c}"));
    let (c, r) = cursor_after(&format!("{}你", "a".repeat(39)));
    s.check("wide_char_at_last_col_wraps", c == 2 && r == 1, format!("col={c} row={r} (wide char needing cols 39-40 must wrap to next row)"));
    // 'é 👩‍💻' from the critic (precomposed + ZWJ)
    let (c, _) = cursor_after("é 👩\u{200d}💻\n");
    s.info("critic_h8_cursor", format!("{c}"));
    s.finish();
}

#[test]
fn sgr_scene() {
    let mut s = Soft::new("render");
    let mut t = String::new();
    t += "\x1b[1mbold\x1b[0m \x1b[2mdim\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munderline\x1b[0m \x1b[7mreverse\x1b[0m \x1b[8mhidden\x1b[0m \x1b[9mstrike\x1b[0m \x1b[1;3;4;31mall\x1b[0m\r\n";
    t += "16 fg: ";
    for i in 30..38 { t += &format!("\x1b[{i}m#{i}\x1b[0m "); }
    for i in 90..98 { t += &format!("\x1b[{i}m#{i}\x1b[0m "); }
    t += "\r\n16 bg: ";
    for i in 40..48 { t += &format!("\x1b[{i}m {i} \x1b[0m"); }
    for i in 100..108 { t += &format!("\x1b[{i}m {i}\x1b[0m"); }
    t += "\r\n256   : ";
    for i in 16..52 { t += &format!("\x1b[48;5;{i}m \x1b[0m"); }
    t += "\r\n256 gr: ";
    for i in 232..256 { t += &format!("\x1b[48;5;{i}m \x1b[0m"); }
    t += "\r\ntruecol: ";
    for i in 0..64 { t += &format!("\x1b[48;2;{};{};{}m \x1b[0m", i * 4, 255 - i * 4, 128); }
    t += "\r\ncolon  : \x1b[38:2::255:100:0mcolon-truecolor\x1b[0m \x1b[38;2;255;100;0mseminc-truecolor\x1b[0m\r\n";
    t += "rev+bg : \x1b[41;37mred-bg \x1b[7mreversed\x1b[27m back\x1b[0m\r\n";
    t += "bold+col: \x1b[1;31mbold-red\x1b[0m \x1b[1;30mbold-black(bright?)\x1b[0m \x1b[2;32mdim-green\x1b[0m\r\n";
    t += "bg-erase: \x1b[44mblue bg then erase line:\x1b[K\x1b[0m|\r\n";
    t += &format!("long   : {}\r\n", "0123456789".repeat(14));
    t += "tab    : a\tb\tc\td\te\r\n";
    render_bytes("04_sgr", 100, 16, t.as_bytes(), 16.0);
    // semantic: colon-form truecolor parsed?
    let mut p = pane(40, 2);
    p.feed(b"\x1b[38:2::255:100:0mX");
    let c = p.terminal.grid[0][0].fg;
    let ok = matches!(c, crate::terminal::Color::Rgb(255, 100, 0));
    s.check("colon_separated_truecolor_parsed", ok, format!("fg after CSI 38:2::255:100:0 m = {}", match c { crate::terminal::Color::Default => "Default".into(), crate::terminal::Color::Indexed(i) => format!("Indexed({i})"), crate::terminal::Color::Rgb(r, g, b) => format!("Rgb({r},{g},{b})") }));
    let mut p = pane(40, 2);
    p.feed(b"\x1b[9mX\x1b[53mY");
    s.info("strike_attr", "SGR 9 (strikethrough) has no Attrs field: parser ignores it (grid.rs Attrs: bold,dim,italic,underline,reverse,hidden)");
    s.finish();
}

/// H9: do bold / italic / underline change any pixels vs plain text?
#[test]
fn h9_bold_italic_underline_pixels() {
    let mut s = Soft::new("render");
    let text = "Hello, Wxyz gjpqy 0123";
    let variants = [("plain", ""), ("bold", "\x1b[1m"), ("italic", "\x1b[3m"), ("underline", "\x1b[4m"), ("dim", "\x1b[2m"), ("reverse", "\x1b[7m"), ("bold_italic_ul", "\x1b[1;3;4m")];
    let (plain, _, _) = render_bytes("", 30, 2, format!("{text}").as_bytes(), 18.0);
    for (name, sgr) in variants.iter().skip(1) {
        let (buf, w, h) = render_bytes("", 30, 2, format!("{sgr}{text}").as_bytes(), 18.0);
        let diff = buf.iter().zip(&plain).filter(|(a, b)| a != b).count();
        s.check(&format!("sgr_{name}_changes_pixels"), diff > 0, format!("{diff} of {} pixels differ from plain", w * h));
    }
    // visual strip: 5 rows of each style for human viewing
    let mut t = String::new();
    for (name, sgr) in &variants { t += &format!("{sgr}{text} ({name})\x1b[0m\r\n"); }
    render_bytes("05_styles", 40, 8, t.as_bytes(), 22.0);
    // bold vs bold-through-cell-size: bold must not change layout
    s.finish();
}

/// H7: shrinking then growing the window must not destroy line content.
#[test]
fn h7_resize_narrow_then_wide() {
    let mut s = Soft::new("resize");
    let mut p = pane(80, 10);
    p.feed(b"0123456789 abcdefghij ABCDEFGHIJ klmnopqrst KLMNOPQRST end-of-line\r\nsecond line stays\r\n$ ");
    let before = super::screen_text(&p.terminal);
    p.terminal.resize(30, 10);
    p.terminal.resize(80, 10);
    let after = super::screen_text(&p.terminal);
    s.check("narrow_then_wide_preserves_text", before == after, format!("before={before:?}\nafter={after:?}"));
    // shrink rows: where do the lines that no longer fit go?
    let mut p = pane(80, 10);
    for i in 0..10 { p.feed(format!("row{i}\r\n").as_bytes()); }
    let sb0 = p.terminal.scrollback.len();
    p.terminal.resize(80, 5);
    let sb1 = p.terminal.scrollback.len();
    let screen = super::screen_text(&p.terminal);
    s.check("shrink_rows_pushes_top_lines_to_scrollback", sb1 > sb0, format!("scrollback {sb0}->{sb1}; screen after shrink to 5 rows = {screen:?} (content cut off at bottom instead of scrolling up)"));
    s.finish();
}
