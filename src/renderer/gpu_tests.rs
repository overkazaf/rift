//! GPU text renderer vs the CPU renderer: pixel comparison of offscreen
//! frames, plus incremental-update and ligature checks.
//!
//! Every test skips (passes with a note) when no wgpu adapter is available,
//! so headless CI without a GPU stays green.

use std::time::{Duration, Instant};

use super::gpu::OffscreenGpu;
use super::gpu_text::HlRect;
use super::*;
use crate::config::Config;
use crate::window::{PaneRect, WindowManager};

pub(crate) fn font_path() -> String {
    crate::config::resolve_font_path(&Config::default())
}

/// A JetBrains Mono (ligature font) if one is installed.
pub(crate) fn ligature_font() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    [
        format!("{home}/Library/Fonts/JetBrainsMonoNerdFontMono-Regular.ttf"),
        format!("{home}/Library/Fonts/JetBrainsMonoNerdFont-Regular.ttf"),
        "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf".to_string(),
    ]
    .into_iter()
    .find(|p| std::path::Path::new(p).exists())
}

pub(crate) struct Diff {
    pub max: u32,
    pub bad: usize,
    pub total: usize,
    pub first_bad: Option<(usize, usize)>,
}

pub(crate) fn diff(a: &[u32], b: &[u32], w: usize, tol: u32) -> Diff {
    let mut d = Diff { max: 0, bad: 0, total: a.len(), first_bad: None };
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        let m = (0..3)
            .map(|s| ((x >> (s * 8)) & 0xff).abs_diff((y >> (s * 8)) & 0xff))
            .max()
            .unwrap_or(0);
        d.max = d.max.max(m);
        if m > tol {
            d.bad += 1;
            d.first_bad.get_or_insert((i % w, i / w));
        }
    }
    d
}

pub(crate) fn save_png(name: &str, w: usize, h: usize, px: &[u32]) -> std::path::PathBuf {
    let p = crate::audit::scratch_dir("gpu").join(format!("{name}.png"));
    crate::audit::write_png(&p, w, h, px);
    p
}

/// Two renderers (CPU reference, GPU text) over the same window state.
pub(crate) struct Rig {
    pub cpu: Renderer,
    pub gpu: Renderer,
    pub dev: OffscreenGpu,
    pub wm: WindowManager,
    pub w: usize,
    pub h: usize,
    pub area: PaneRect,
    blocks: crate::tools::blocks::BlockManager,
}

impl Rig {
    pub fn new(cols: usize, rows: usize, font: &str, tab_bar: bool) -> Option<Self> {
        let dev = match OffscreenGpu::new() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("skipping GPU test: {e}");
                return None;
            }
        };
        let theme = Config::default().theme;
        let cpu = Renderer::new(font, 15.0, theme.clone());
        let mut gpu = Renderer::new(font, 15.0, theme);
        gpu.enable_gpu_text(true);
        gpu.set_ligatures(false);
        let (cw, ch) = (cpu.cell_width(), cpu.cell_height());
        let tbh = if tab_bar { ch + 16 } else { 0 };
        let (w, h) = (cols * cw, rows * ch + tbh);
        let wm = WindowManager::headless(cols, rows);
        let area = PaneRect { x: 0, y: tbh, width: w, height: rows * ch };
        Some(Self { cpu, gpu, dev, wm, w, h, area, blocks: crate::tools::blocks::BlockManager::new() })
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.wm.active_pane_mut().feed(bytes);
    }

    pub fn cpu_frame(&mut self) -> Vec<u32> {
        let mut buf = vec![0u32; self.w * self.h];
        self.cpu.start_time = Instant::now();
        self.cpu.render_tabbed(&self.wm, self.area, &mut buf, self.w as u32, self.h as u32, &self.blocks);
        buf
    }

    /// GPU frame: (pixels, stats).
    pub fn gpu_frame(&mut self) -> (Vec<u32>, super::gpu::FrameStats) {
        let mut ui = vec![0u32; self.w * self.h];
        self.gpu.start_time = Instant::now();
        self.gpu.render_tabbed(&self.wm, self.area, &mut ui, self.w as u32, self.h as u32, &self.blocks);
        let stats = self
            .dev
            .frame(self.gpu.gpu.as_deref_mut(), &ui, true, self.w as u32, self.h as u32, None, 0.0)
            .expect("gpu frame");
        (self.dev.read_pixels().expect("readback"), stats)
    }

    /// Compare one frame of both renderers; saves PNGs and panics on mismatch.
    pub fn assert_match(&mut self, name: &str, tol: u32, max_bad_frac: f64) -> Diff {
        let c = self.cpu_frame();
        let (g, _) = self.gpu_frame();
        let d = diff(&c, &g, self.w, tol);
        let frac = d.bad as f64 / d.total as f64;
        eprintln!(
            "[gpu-parity] {name}: {}x{} max channel diff {} , {} px over tol {tol} ({:.4}%)",
            self.w, self.h, d.max, d.bad, frac * 100.0
        );
        if frac > max_bad_frac {
            let pc = save_png(&format!("{name}_cpu"), self.w, self.h, &c);
            let pg = save_png(&format!("{name}_gpu"), self.w, self.h, &g);
            panic!(
                "{name}: GPU output differs from CPU: {} px over tolerance, first at {:?}; see {} and {}",
                d.bad, d.first_bad, pc.display(), pg.display()
            );
        }
        d
    }
}

const TOL: u32 = 3;
const FRAC: f64 = 0.0005;

#[test]
fn ascii_and_colors() {
    let Some(mut r) = Rig::new(60, 12, &font_path(), false) else { return };
    r.feed(b"Hello, World! 0123456789 {}[]()<> !@#$%^&*\r\n");
    r.feed(b"\x1b[31mred\x1b[32m green\x1b[33m yellow\x1b[34m blue\x1b[35m magenta\x1b[36m cyan\x1b[0m\r\n");
    r.feed(b"\x1b[38;2;255;128;0mtruecolor fg\x1b[0m \x1b[48;2;20;60;120m truecolor bg \x1b[0m\r\n");
    r.feed(b"\x1b[38;5;196m256-color\x1b[0m \x1b[48;5;22m\x1b[38;5;231m on green \x1b[0m\r\n");
    r.feed(b"fn main() { println!(\"quick brown fox\"); } // jumps over the lazy dog\r\n");
    r.assert_match("ascii_colors", TOL, FRAC);
}

#[test]
fn text_styles_and_decorations() {
    let Some(mut r) = Rig::new(70, 14, &font_path(), false) else { return };
    r.feed(b"\x1b[1mbold\x1b[0m \x1b[3mitalic\x1b[0m \x1b[1;3mbold-italic\x1b[0m \x1b[2mdim\x1b[0m \x1b[7mreverse\x1b[0m \x1b[8mhidden\x1b[0m|\r\n");
    r.feed(b"\x1b[4munderline\x1b[0m \x1b[4:2mdouble\x1b[0m \x1b[4:3mcurly\x1b[0m \x1b[4:4mdotted\x1b[0m \x1b[4:5mdashed\x1b[0m\r\n");
    r.feed(b"\x1b[9mstrike\x1b[0m \x1b[53moverline\x1b[0m \x1b[4;58;2;255;0;0mred underline\x1b[0m \x1b[4;9;53mall three\x1b[0m\r\n");
    r.feed(b"\x1b[41;37m red bg \x1b[0m\x1b[42;30m green bg \x1b[0m\x1b[44;97m blue bg \x1b[0m\x1b[100m gray bg\x1b[0m\r\n");
    r.feed(b"\x1b[1;31mbold red\x1b[0m \x1b[1;4;32mbold underline green\x1b[0m\r\n");
    r.feed("box: \u{250c}\u{2500}\u{252c}\u{2500}\u{2510} \u{2502} \u{2502} \u{2514}\u{2500}\u{2534}\u{2500}\u{2518} \u{2588}\u{2593}\u{2592}\u{2591} \u{e0b0}\u{e0b2}\r\n".as_bytes());
    r.assert_match("styles_decorations", TOL, FRAC);
}

#[test]
fn wide_cjk_marks_and_emoji() {
    let Some(mut r) = Rig::new(50, 8, &font_path(), false) else { return };
    r.feed("CJK: \u{4f60}\u{597d}\u{ff0c}\u{4e16}\u{754c} \u{3053}\u{3093}\u{306b}\u{3061}\u{306f} \u{d55c}\u{ad6d}\u{c5b4} [end]\r\n".as_bytes());
    r.feed("Marks: e\u{301} a\u{308} n\u{303} o\u{302}x [end]\r\n".as_bytes());
    r.feed("Emoji: \u{1f600}\u{1f389}\u{1f680} \u{2764}\u{fe0f} [end]\r\n".as_bytes());
    r.feed("\x1b[44m\u{4e2d}\u{6587}\x1b[0m \x1b[7m\u{4e2d}\x1b[0m \x1b[4m\u{4e2d}\u{6587}\x1b[0m\r\n".as_bytes());
    r.feed("Full: \u{ff21}\u{ff22}\u{ff23}\u{ff11}\u{ff12}\u{ff13} [end]\r\n".as_bytes());
    r.assert_match("wide_marks_emoji", TOL, FRAC);
}

#[test]
fn cursor_styles() {
    for (name, seq) in [("block", "\x1b[2 q"), ("bar", "\x1b[6 q"), ("underline", "\x1b[4 q")] {
        let Some(mut r) = Rig::new(40, 6, &font_path(), false) else { return };
        r.feed(b"prompt$ echo hi\r\nhi\r\n");
        r.feed(seq.as_bytes());
        r.feed(b"\x1b[2;1Hcursor here");
        r.feed(b"\x1b[2;3H");
        r.assert_match(&format!("cursor_{name}"), TOL, FRAC);
        // Cursor on a wide glyph.
        r.feed("\x1b[4;1H\u{4e2d}\u{6587}\x1b[4;1H".as_bytes());
        r.assert_match(&format!("cursor_{name}_wide"), TOL, FRAC);
    }
}

#[test]
fn selection_and_search_highlights() {
    let Some(mut r) = Rig::new(50, 8, &font_path(), false) else { return };
    r.feed(b"first line of text\r\nsecond line of text\r\nthird line of text\r\n");
    r.feed(b"\x1b[31mcolored\x1b[0m \x1b[44mbackground\x1b[0m line\r\n");
    // Selection from (row 1, col 7) to (row 3, col 12).
    let mut sel = crate::window::Selection::new();
    sel.start_at(1, 7);
    sel.extend_to(3, 12);
    let rows = {
        let t = &r.wm.active_pane().terminal;
        crate::blocks_ui::view::view_abs_rows(t)
    };
    // CPU: blend over the finished frame.
    let mut c = r.cpu_frame();
    r.cpu.render_selection(&sel, &rows, &mut c, r.w, r.h, r.area);
    // GPU: highlight quads.
    let mut rects = Vec::new();
    for (row, abs) in rows.iter().enumerate() {
        let Some(abs) = abs else { continue };
        let mut start = None;
        for col in 0..=r.w / r.cpu.cell_width() {
            let on = col < r.w / r.cpu.cell_width() && sel.contains(*abs, col);
            match (on, start) {
                (true, None) => start = Some(col),
                (false, Some(s)) => {
                    rects.push(HlRect { row, c0: s, c1: col, rgba: [80, 120, 200, 105] });
                    start = None;
                }
                _ => {}
            }
        }
    }
    assert!(!rects.is_empty());
    r.gpu.set_highlights(0, rects);
    let (g, _) = r.gpu_frame();
    let d = diff(&c, &g, r.w, TOL);
    eprintln!("[gpu-parity] selection: max {} bad {}", d.max, d.bad);
    assert!(d.bad as f64 / d.total as f64 <= FRAC, "selection differs: {} px, first {:?}", d.bad, d.first_bad);
    // Clearing the highlights repaints the rows.
    r.gpu.set_highlights(0, Vec::new());
    r.assert_match("selection_cleared", TOL, FRAC);
}

#[test]
fn tab_bar_and_scrollback_indicator() {
    let Some(mut r) = Rig::new(50, 10, &font_path(), true) else { return };
    r.wm.new_tab(50, 10);
    r.wm.switch_tab(0);
    for i in 0..40 {
        r.feed(format!("line {i:02} of scrollback content\r\n").as_bytes());
    }
    r.assert_match("tabbar_bottom", TOL, FRAC);
    r.wm.active_pane_mut().terminal.scroll_view_up(7);
    r.assert_match("tabbar_scrolled_back", TOL, FRAC);
}

#[test]
fn split_panes_dim_unfocused() {
    use crate::window::tab::SplitDir;
    let Some(mut r) = Rig::new(80, 10, &font_path(), false) else { return };
    r.feed(b"left pane text\r\nmore left\r\n");
    let min = crate::window::tab::MinSize { w: 40, h: 40 };
    assert!(r.wm.split_active(SplitDir::Horizontal, r.area, min));
    r.feed(b"right pane active\r\n\x1b[32mgreen on right\x1b[0m\r\n");
    r.assert_match("split_panes", TOL, FRAC);
}

#[test]
fn incremental_updates_match_full_repaint() {
    let Some(mut r) = Rig::new(60, 12, &font_path(), false) else { return };
    r.feed(b"initial content\r\nsecond row\r\n");
    r.assert_match("incr_0", TOL, FRAC);
    let (_, s0) = {
        // Nothing changed: no row is rebuilt and no terminal pass is needed.
        let (px, s) = r.gpu_frame();
        (px, s)
    };
    assert_eq!(s0.rows_uploaded, 0, "idle frame must not upload rows: {s0:?}");
    assert_eq!(s0.atlas_uploads, 0);
    assert!(!s0.term_pass, "idle frame must reuse the terminal layer");
    assert_eq!(s0.ui_rects, 0, "idle frame must not upload the UI layer: {s0:?}");

    // Type a character: exactly one row changes.
    r.feed(b"x");
    let (_, s1) = r.gpu_frame();
    assert_eq!(s1.rows_uploaded, 1, "one changed row: {s1:?}");
    r.assert_match("incr_1", TOL, FRAC);

    // Scroll the whole screen.
    for i in 0..30 {
        r.feed(format!("scroll {i}\r\n").as_bytes());
    }
    r.assert_match("incr_scroll", TOL, FRAC);
    // Clear screen, then repaint with colors.
    r.feed(b"\x1b[2J\x1b[H\x1b[1;34mafter clear\x1b[0m\r\n");
    r.assert_match("incr_clear", TOL, FRAC);
    // Shrink content: rows that disappear must be blanked.
    r.feed(b"\x1b[H\x1b[J");
    r.assert_match("incr_blank", TOL, FRAC);
}

#[test]
fn legacy_opaque_frame_and_mode_switch() {
    // A suspended GPU renderer (CPU text, opaque UI layer) matches the CPU path
    // exactly, and returning to GPU text afterwards repaints correctly.
    let Some(mut r) = Rig::new(40, 6, &font_path(), false) else { return };
    r.feed(b"suspended mode test \x1b[1mbold\x1b[0m\r\n");
    r.assert_match("mode_gpu", TOL, FRAC);
    r.gpu.set_gpu_suspended(true);
    let c = r.cpu_frame();
    let mut ui = vec![0u32; r.w * r.h];
    r.gpu.render_tabbed(&r.wm, r.area, &mut ui, r.w as u32, r.h as u32, &r.blocks);
    r.dev.frame(None, &ui, false, r.w as u32, r.h as u32, None, 0.0).unwrap();
    let g = r.dev.read_pixels().unwrap();
    let d = diff(&c, &g, r.w, 1);
    assert!(d.bad == 0, "opaque legacy frame must equal the CPU frame ({} px, first {:?})", d.bad, d.first_bad);
    r.gpu.set_gpu_suspended(false);
    r.assert_match("mode_back_to_gpu", TOL, FRAC);
}

#[test]
fn atlas_eviction_keeps_output_correct() {
    // A tiny atlas (two 128x128 pages, ~100 CJK glyphs) is cycled through
    // three disjoint glyph sets, which forces LRU page eviction.
    let Some(mut r) = Rig::new(60, 10, &font_path(), false) else { return };
    r.gpu.gpu = Some(Box::new(super::gpu_text::GpuText::with_atlas(128, 2)));
    r.gpu.invalidate();
    let set = |base: u32| {
        let mut text = String::from("\x1b[2J\x1b[H");
        for row in 0..4u32 {
            for col in 0..10u32 {
                text.push(char::from_u32(base + row * 10 + col).unwrap());
            }
            text.push_str("\r\n");
        }
        text
    };
    for (i, base) in [0x4e00u32, 0x4f00, 0x5000, 0x4e00, 0x5000].into_iter().enumerate() {
        r.feed(set(base).as_bytes());
        // An eviction mid-frame asks for an immediate second frame; the pair converges.
        let _ = r.gpu_frame();
        let st = r.gpu.gpu.as_ref().map(|g| (g.atlas.evictions, g.atlas.glyph_count(), g.atlas.page_count())).unwrap();
        let c = r.cpu_frame();
        let (g, _) = r.gpu_frame();
        let d = diff(&c, &g, r.w, TOL);
        eprintln!("[gpu-parity] eviction set {i}: {} bad px (after frame 1: evictions/glyphs/pages {:?}, redraw {})", d.bad, st, r.gpu.redraw_requested);
        assert!(d.bad as f64 / d.total as f64 <= FRAC, "set {i}: {} px differ after eviction", d.bad);
    }
    let ev = r.gpu.gpu.as_ref().map_or(0, |g| g.atlas.evictions);
    eprintln!("[gpu-parity] atlas evictions: {ev}");
    assert!(ev > 0, "the test atlas should have evicted pages");
}

// ── Ligatures ──

fn count_ink(px: &[u32], w: usize, x0: usize, x1: usize, y0: usize, y1: usize, bg: u32) -> usize {
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            if px[y * w + x] != bg {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn ligatures_change_the_rendering_of_arrows() {
    let Some(font) = ligature_font() else {
        eprintln!("skipping: no JetBrains Mono installed");
        return;
    };
    let Some(mut r) = Rig::new(40, 6, &font, false) else { return };
    r.feed(b"a -> b => c != d == e >= f\r\n");
    r.feed(b"fn x() -> Result<()> { a |> b }\r\n");
    let (plain, _) = r.gpu_frame();
    r.gpu.set_ligatures(true);
    let (lig, _) = r.gpu_frame();
    let d = diff(&plain, &lig, r.w, 0);
    eprintln!("[ligatures] {} pixels change when ligatures are enabled", d.bad);
    assert!(d.bad > 50, "ligatures should visibly change the arrows/operators");
    save_png("ligatures_off", r.w, r.h, &plain);
    save_png("ligatures_on", r.w, r.h, &lig);

    // Text without any ligature sequence is pixel-identical either way.
    let Some(mut r2) = Rig::new(40, 4, &font, false) else { return };
    r2.feed(b"plain words and (parentheses) only\r\n");
    let (a, _) = r2.gpu_frame();
    r2.gpu.set_ligatures(true);
    let (b, _) = r2.gpu_frame();
    assert_eq!(diff(&a, &b, r2.w, 0).bad, 0, "no ligature sequences: identical output");
}

#[test]
fn ligatures_are_broken_at_the_cursor_cell() {
    let Some(font) = ligature_font() else { return };
    let Some(mut r) = Rig::new(30, 3, &font, false) else { return };
    r.gpu.set_ligatures(true);
    // Cursor parked on the '>' of "->": the pair must render as separate glyphs.
    r.feed(b"x -> y\x1b[1;4H");
    let cw = r.cpu.cell_width();
    let (with_cursor, _) = r.gpu_frame();
    // Move the cursor away: ligature appears.
    r.feed(b"\x1b[3;1H");
    let (away, _) = r.gpu_frame();
    let ch = r.cpu.cell_height();
    let bg = away[(ch + 2) * r.w + 2];
    let ink_a = count_ink(&with_cursor, r.w, 2 * cw, 4 * cw, 0, ch, bg);
    let ink_b = count_ink(&away, r.w, 2 * cw, 4 * cw, 0, ch, bg);
    assert!(ink_a > 0 && ink_b > 0);
    assert_ne!(
        with_cursor[..r.w * ch],
        away[..r.w * ch],
        "ligature must change when the cursor leaves the pair"
    );
}

#[test]
fn ligatures_split_at_selection_boundary() {
    let Some(font) = ligature_font() else { return };
    let Some(mut r) = Rig::new(30, 3, &font, false) else { return };
    r.gpu.set_ligatures(true);
    r.feed(b"x -> y\r\n");
    // Highlight only the '-' (col 2): the ligature pair is cut there.
    r.gpu.set_highlights(0, vec![HlRect { row: 0, c0: 2, c1: 3, rgba: [80, 120, 200, 105] }]);
    let (sel, _) = r.gpu_frame();
    r.gpu.set_highlights(0, vec![]);
    let (none, _) = r.gpu_frame();
    let cw = r.cpu.cell_width();
    let ch = r.cpu.cell_height();
    // The cell right of the selection edge (the '>') differs: separate '>' glyph vs ligature tail.
    let a: Vec<u32> = (0..ch).flat_map(|y| (3 * cw..4 * cw).map(move |x| (x, y))).map(|(x, y)| sel[y * r.w + x]).collect();
    let b: Vec<u32> = (0..ch).flat_map(|y| (3 * cw..4 * cw).map(move |x| (x, y))).map(|(x, y)| none[y * r.w + x]).collect();
    assert_ne!(a, b, "selection edge must break the ligature");
}

// ── Frame cost benchmark (ignored by default) ──
//
//   cargo test --bin rift --features gpu gpu_bench -- --ignored --nocapture --test-threads=1
//
// "GPU text" is the new pipeline (instances + diff-uploaded UI layer + composite).
// "before" is the previous GPU pipeline: CPU glyph blits into the frame buffer,
// XRGB->RGBA conversion of the whole frame and a full-frame texture upload
// (emulated with the legacy opaque path and a forced full UI upload).

#[derive(Default, Clone, Copy)]
struct Avg {
    render: f64,
    inst: f64,
    ui: f64,
    submit: f64,
    wait: f64,
    total: f64,
}

impl Avg {
    fn line(&self) -> String {
        format!(
            "total {:6.2} ms  (render_tabbed {:5.2} | atlas+instances {:5.2} | ui diff/upload {:5.2} | encode+submit {:5.2} | gpu wait {:5.2})",
            self.total, self.render, self.inst, self.ui, self.submit, self.wait
        )
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn unique_lines(cols: usize, n: usize, seed: usize) -> String {
    let mut s = String::new();
    for k in 0..n {
        for c in 0..cols.saturating_sub(1) {
            let ch = (b'!' + ((seed * 31 + k * 17 + c * 7 + c * c) % 90) as u8) as char;
            if c % 13 == 0 {
                s.push_str(&format!("\x1b[3{}m", 1 + (c / 13 + k) % 6));
            }
            s.push(ch);
        }
        s.push_str("\x1b[0m\r\n");
    }
    s
}

fn fill_lines(cols: usize, rows: usize) -> String {
    unique_lines(cols, rows, 0)
}

struct Bench {
    r: Rig,
    ui: Vec<u32>,
    cpu_buf: Vec<u32>,
    blocks: crate::tools::blocks::BlockManager,
}

impl Bench {
    /// One new-pipeline frame; returns (render_tabbed time, stats).
    fn gpu_once(&mut self) -> (Duration, super::gpu::FrameStats) {
        let (w, h) = (self.r.w as u32, self.r.h as u32);
        let t = Instant::now();
        self.r.gpu.start_time = Instant::now();
        self.r.gpu.render_tabbed(&self.r.wm, self.r.area, &mut self.ui, w, h, &self.blocks);
        let render = t.elapsed();
        let st = self.r.dev.frame(self.r.gpu.gpu.as_deref_mut(), &self.ui, true, w, h, None, 0.0).unwrap();
        (render, st)
    }

    /// One frame of the previous pipeline.
    fn old_once(&mut self) -> (Duration, super::gpu::FrameStats) {
        let (w, h) = (self.r.w as u32, self.r.h as u32);
        let t = Instant::now();
        self.r.cpu.render_tabbed(&self.r.wm, self.r.area, &mut self.cpu_buf, w, h, &self.blocks);
        let render = t.elapsed();
        self.r.dev.invalidate_ui();
        let st = self.r.dev.frame(None, &self.cpu_buf, false, w, h, None, 0.0).unwrap();
        (render, st)
    }

    fn run(&mut self, n: usize, mut step: impl FnMut(&mut Rig, usize), old: bool) -> Avg {
        let mut a = Avg::default();
        // Settle after a pipeline switch (first frame re-uploads the UI layer).
        for i in 0..2 {
            step(&mut self.r, 1000 + i);
            if old { self.old_once(); } else { self.gpu_once(); }
        }
        for i in 0..n {
            step(&mut self.r, i);
            let t = Instant::now();
            let (render, st) = if old { self.old_once() } else { self.gpu_once() };
            a.total += ms(t.elapsed());
            a.render += ms(render);
            a.inst += ms(st.t_inst);
            a.ui += ms(st.t_ui);
            a.submit += ms(st.t_submit);
            a.wait += ms(st.t_wait);
        }
        let n = n as f64;
        Avg { render: a.render / n, inst: a.inst / n, ui: a.ui / n, submit: a.submit / n, wait: a.wait / n, total: a.total / n }
    }
}

fn bench_size(cols: usize, rows: usize, font: &str) -> Option<()> {
    let mut r = Rig::new(cols, rows, font, false)?;
    r.gpu.set_ligatures(true);
    r.cpu.set_ligatures(false);
    r.wm.active_pane_mut().terminal.set_max_scrollback(100);
    r.feed(fill_lines(cols, rows).as_bytes());
    let (w, h) = (r.w, r.h);
    let mut b = Bench { ui: vec![0; w * h], cpu_buf: vec![0; w * h], blocks: crate::tools::blocks::BlockManager::new(), r };
    let n = 20;
    println!("[gpu-bench] {cols}x{rows} cells = {w}x{h} px");
    // warm up glyph atlas, shader pipelines, font caches (both paths)
    for _ in 0..3 {
        b.gpu_once();
        b.old_once();
    }

    let type_step = |r: &mut Rig, i: usize| r.feed(format!("\x1b[3;1Hx{}", i % 10).as_bytes());
    let scroll_step = |r: &mut Rig, i: usize| {
        let t = unique_lines(cols, 3, i + 1);
        r.feed(t.as_bytes());
    };
    let rows_of = |b: &mut Bench| b.r.gpu.gpu.as_ref().map_or(0, |g| g.rows_built);

    for (name, step) in [
        ("idle           ", None::<&dyn Fn(&mut Rig, usize)>),
        ("typing 1 char  ", Some(&type_step as &dyn Fn(&mut Rig, usize))),
        ("scroll 3 lines ", Some(&scroll_step as &dyn Fn(&mut Rig, usize))),
    ] {
        let noop = |_: &mut Rig, _: usize| {};
        let before_rows = rows_of(&mut b);
        let new = match step {
            Some(f) => b.run(n, |r, i| f(r, i), false),
            None => b.run(n, noop, false),
        };
        let rows_per_frame = (rows_of(&mut b) - before_rows) as f64 / n as f64;
        let old = match step {
            Some(f) => b.run(n, |r, i| f(r, i), true),
            None => b.run(n, noop, true),
        };
        println!("  {name} before: {}", old.line());
        println!("  {name} after : {}   [{rows_per_frame:.0} rows rebuilt/frame]", new.line());
    }
    // Full repaint (resize, theme change).
    let full_new = b.run(n, |r, _| r.gpu.invalidate(), false);
    let full_old = b.run(n, |r, _| r.cpu.invalidate(), true);
    println!("  full repaint    before: {}", full_old.line());
    println!("  full repaint    after : {}", full_new.line());
    Some(())
}

#[test]
#[ignore]
fn gpu_bench() {
    let font = ligature_font().unwrap_or_else(font_path);
    println!("font: {font}");
    for (c, r) in [(200, 60), (400, 100), (320, 90)] {
        if bench_size(c, r, &font).is_none() {
            println!("no GPU adapter: skipping");
            return;
        }
    }
}
