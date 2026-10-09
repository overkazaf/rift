//! Item 3 (+ critic H1): throughput, render time, memory, flood behaviour.
//! Run in release:  cargo test --release --bin rift audit::perf -- --ignored --nocapture --test-threads=1
use super::render::make_renderer;
use super::{pane, Soft};
use crate::terminal::Terminal;
use crate::window::{PaneRect, WindowManager};
use std::time::Instant;

fn rss_kb() -> u64 {
    let out = std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0)
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn random_text(mb: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng(seed);
    let mut v = Vec::with_capacity(mb << 20);
    while v.len() < mb << 20 {
        let n = (r.next() % 100) as usize + 10;
        for _ in 0..n {
            let x = r.next();
            v.push(if x % 9 == 0 { b' ' } else { b'!' + (x >> 8) as u8 % 90 });
        }
        v.extend_from_slice(b"\r\n");
    }
    v
}

fn ansi_heavy(mb: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng(seed);
    let mut v = Vec::with_capacity(mb << 20);
    while v.len() < mb << 20 {
        for _ in 0..12 {
            let x = r.next();
            v.extend_from_slice(format!("\x1b[38;2;{};{};{}m\x1b[48;5;{}m", x & 255, (x >> 8) & 255, (x >> 16) & 255, (x >> 24) & 255).as_bytes());
            if x % 5 == 0 { v.extend_from_slice(b"\x1b[1m"); }
            for k in 0..8 { v.push(b'a' + ((x >> (k * 3)) as u8 % 26)); }
        }
        v.extend_from_slice(b"\x1b[0m\r\n");
    }
    v
}

fn time_feed(label: &str, s: &mut Soft, cols: usize, rows: usize, data: &[u8]) -> f64 {
    let mut p = pane(cols, rows);
    let t0 = Instant::now();
    p.feed(data);
    let dt = t0.elapsed().as_secs_f64();
    let mb = data.len() as f64 / (1 << 20) as f64;
    s.info(label, format!("{:.1} MB in {:.2}s = {:.1} MB/s ({:.1} ms/MB) grid={cols}x{rows} scrollback={}", mb, dt, mb / dt, dt * 1000.0 / mb, p.terminal.scrollback.len()));
    mb / dt
}

#[test]
#[ignore]
fn throughput() {
    let mut s = Soft::new("perf");
    s.info("build", if cfg!(debug_assertions) { "debug-assertions ON (not --release)" } else { "release" });
    let seq = std::process::Command::new("seq").args(["1", "500000"]).output().unwrap().stdout;
    let seq: Vec<u8> = seq.iter().flat_map(|&b| if b == b'\n' { vec![b'\r', b'\n'] } else { vec![b] }).collect();
    time_feed("seq_1_500000_80x24", &mut s, 80, 24, &seq);
    time_feed("seq_1_500000_200x60", &mut s, 200, 60, &seq);
    let rt = random_text(50, 1);
    let r1 = time_feed("random_text_50MB_80x24", &mut s, 80, 24, &rt);
    time_feed("random_text_50MB_200x60", &mut s, 200, 60, &rt);
    let an = ansi_heavy(30, 2);
    let r2 = time_feed("ansi_heavy_30MB_120x40", &mut s, 120, 40, &an);
    let mut r = Rng(3);
    let bin: Vec<u8> = (0..20 << 20).map(|_| r.next() as u8).collect();
    time_feed("random_binary_20MB_120x40", &mut s, 120, 40, &bin);
    // scroll-heavy cursor addressing (curses-like redraws)
    let mut redraw = Vec::new();
    for f in 0..2000 {
        redraw.extend_from_slice(b"\x1b[H");
        for row in 0..40 { redraw.extend_from_slice(format!("\x1b[{};1H\x1b[{}mline {} frame {f} ............................................\x1b[K", row + 1, 31 + row % 7, row).as_bytes()); }
    }
    time_feed("curses_redraw_2000_frames_120x40", &mut s, 120, 40, &redraw);
    s.info("reference", "UNCERTAIN public numbers (from memory, not re-measured here): Alacritty/kitty/Ghostty parse plain text at roughly 0.3-1+ GB/s; vte crate itself ~hundreds MB/s. A flood of `cat bigfile` on a 60 Hz display only needs >~100 MB/s to never be parser-bound.");
    s.check("plain_text_over_50MBps", r1 > 50.0, format!("{r1:.1} MB/s"));
    s.check("ansi_over_20MBps", r2 > 20.0, format!("{r2:.1} MB/s"));
    s.finish();
}

fn fill_colored(t: &mut Terminal, lines: usize) {
    let cols = t.cols;
    let mut p = pane(cols, t.rows);
    std::mem::swap(&mut p.terminal, t);
    let mut r = Rng(9);
    let mut buf = Vec::new();
    for i in 0..lines {
        buf.extend_from_slice(format!("\x1b[3{}m{i:06} ", i % 8).as_bytes());
        for _ in 0..cols - 8 { buf.push(b'a' + (r.next() % 26) as u8); }
        buf.extend_from_slice(b"\x1b[0m\r\n");
        if buf.len() > 1 << 20 { p.feed(&buf); buf.clear(); }
    }
    p.feed(&buf);
    std::mem::swap(&mut p.terminal, t);
}

#[test]
#[ignore]
fn memory_scrollback() {
    let mut s = Soft::new("perf");
    s.info("sizes", format!("size_of::<Cell>={} bytes", std::mem::size_of::<crate::terminal::Cell>()));
    for (cols, rows) in [(120usize, 40usize), (200, 60), (400, 100)] {
        let base = rss_kb();
        let mut panes: Vec<Terminal> = Vec::new();
        for _ in 0..10 {
            let mut t = Terminal::new(cols, rows);
            fill_colored(&mut t, 12_000);
            panes.push(t);
        }
        let used = rss_kb().saturating_sub(base);
        s.info(&format!("10_panes_{cols}x{rows}_10k_scrollback"), format!("RSS +{} MB ({} MB/pane); scrollback lens {:?}", used / 1024, used / 1024 / 10, panes.iter().map(|t| t.scrollback.len()).take(2).collect::<Vec<_>>()));
        s.check(&format!("mem_{cols}x{rows}_under_500MB"), used / 1024 < 500, format!("{} MB for 10 panes", used / 1024));
        drop(panes);
    }
    s.finish();
}

#[test]
#[ignore]
fn render_frame_times() {
    let mut s = Soft::new("perf");
    for (cols, rows) in [(120usize, 40usize), (200, 60), (400, 100)] {
        let mut wm = WindowManager::headless(cols, rows);
        fill_colored(&mut wm.active_pane_mut().terminal, 200);
        let mut r = make_renderer(15.0);
        let (cw, ch) = (r.cell_width(), r.cell_height());
        let (w, h) = (cols * cw, rows * ch);
        let mut buf = vec![0u32; w * h];
        let blocks = crate::tools::blocks::BlockManager::new();
        let area = PaneRect { x: 0, y: 0, width: w, height: h };
        // warm glyph cache
        r.render_tabbed(&wm, area, &mut buf, w as u32, h as u32, &blocks);
        let n = 20;
        let t0 = Instant::now();
        for _ in 0..n {
            r.invalidate();
            r.render_tabbed(&wm, area, &mut buf, w as u32, h as u32, &blocks);
        }
        let full = t0.elapsed().as_secs_f64() * 1000.0 / n as f64;
        let t0 = Instant::now();
        for i in 0..n {
            wm.active_pane_mut().feed(format!("x{}", i % 10).as_bytes());
            r.render_tabbed(&wm, area, &mut buf, w as u32, h as u32, &blocks);
        }
        let typing = t0.elapsed().as_secs_f64() * 1000.0 / n as f64;
        // scrolling frame: 3 new lines per frame (like `cat`)
        let t0 = Instant::now();
        for i in 0..n {
            wm.active_pane_mut().feed(format!("{}\r\n{}\r\n{}\r\n", "s".repeat(cols - 1), "t".repeat(cols - 1), i).as_bytes());
            r.render_tabbed(&wm, area, &mut buf, w as u32, h as u32, &blocks);
        }
        let scroll = t0.elapsed().as_secs_f64() * 1000.0 / n as f64;
        s.info(&format!("render_{cols}x{rows}"), format!("buffer {w}x{h} px: full repaint {full:.1} ms, keystroke {typing:.1} ms, scroll-3-lines {scroll:.1} ms (CPU software path, memcpy included; 16.7ms = 60 fps)"));
        s.check(&format!("full_repaint_{cols}x{rows}_under_50ms"), full < 50.0, format!("{full:.1} ms"));
    }
    s.finish();
}

/// H1: `yes` flood through a real PTY with the same unbounded-channel architecture as src/pty.rs.
#[test]
#[ignore]
fn h1_flood_yes_unbounded_queue_and_drain() {
    use super::shell_real::{tmp_zdotdir, Sh, ENV_LOCK};
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let zd = tmp_zdotdir("flood", "PS1='%# '\n");
    std::env::set_var("ZDOTDIR", &zd);
    let mut s = Soft::new("flood");
    let mut sh = Sh::spawn(Some("/bin/zsh"), false, "/tmp", 120, 40);
    assert!(sh.prompt_ready(30));
    let base = rss_kb();
    sh.send("yes\r");
    // Phase A: UI thread "busy": nothing drains rx for 6 s.
    let mut samples = Vec::new();
    for i in 1..=6 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        samples.push((i, rss_kb().saturating_sub(base) / 1024));
    }
    s.info("rss_growth_MB_per_second_undrained", format!("{samples:?}"));
    let growth = samples.last().unwrap().1;
    s.check("memory_bounded_when_ui_stalls_6s", growth < 200, format!("RSS grew {growth} MB in 6 s with the consumer stalled (bounded channel in src/pty.rs)"));
    // Phase B: one production drain pass (Pane::process_output: the time-budgeted loop about_to_wait runs).
    let t0 = Instant::now();
    let more = sh.budgeted_pass();
    let dt = t0.elapsed().as_secs_f64();
    s.info("drain", format!("one budgeted pass took {:.1} ms; backlog_remaining={more}", dt * 1000.0));
    s.check("drain_loop_returns_within_one_frame_budget_100ms", dt < 0.1, format!("one budgeted drain pass ran {dt:.3}s (backlog_remaining={more}); the UI thread cannot redraw/handle input during this time"));
    sh.send("\x03");
    sh.pump(1500);
    let after = rss_kb().saturating_sub(base) / 1024;
    s.info("rss_after_ctrl_c_MB", format!("{after}"));
    sh.kill();
    std::env::remove_var("ZDOTDIR");
    s.finish();
}
