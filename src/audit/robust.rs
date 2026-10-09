//! Item 10: resize extremes, tab churn, closing panes while output streams, PTY lifetime.
use super::{pane, pane_id, Soft};
use crate::terminal::Terminal;
use crate::window::tab::{MinSize, SplitDir};
use crate::window::{PaneRect, WindowManager};

fn catch<R>(f: impl FnOnce() -> R + std::panic::UnwindSafe) -> Result<R, String> {
    std::panic::catch_unwind(f).map_err(|e| e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "panic".into()))
}
fn rss_mb() -> u64 {
    let out = std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse::<u64>().unwrap_or(0) / 1024
}
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; self.0 }
    fn below(&mut self, n: usize) -> usize { (self.next() % n as u64) as usize }
}

#[test]
fn terminal_resize_extremes() {
    let _rss = super::rss_serial();
    let mut s = Soft::new("robust");
    for (c, r) in [(1usize, 1usize), (1, 100), (100, 1), (2, 2)] {
        let res = catch(move || {
            let mut t = Terminal::new(80, 24);
            let mut p = pane(80, 24);
            p.feed(b"hello wide \xe4\xb8\xad\xe6\x96\x87 world\r\n\x1b[5;5Hx\x1b[3;10r");
            p.terminal.resize(c, r);
            p.feed(b"after resize \xe4\xb8\xad\r\n\x1b[10;10H\x1b[2J\x1b[Hok\r\n\r\n\r\n");
            p.terminal.resize(80, 24);
            t.resize(c, r);
            (p.terminal.cursor_row, p.terminal.cursor_col)
        });
        s.check(&format!("resize_to_{c}x{r}_and_back"), res.is_ok(), format!("{res:?}"));
    }
    let res = catch(|| { let mut t = Terminal::new(80, 24); t.resize(0, 0); });
    s.check("resize_to_0x0_does_not_panic", res.is_ok(), format!("{res:?} (Terminal::resize uses rows - 1 / cols - 1; callers clamp to >=1 in WindowManager::resize_all but the API itself is unguarded)"));
    let res = catch(|| { let _ = Terminal::new(0, 24); });
    s.check("new_with_zero_cols_does_not_panic", res.is_ok(), format!("{res:?}"));
    let res = catch(|| { let mut p = pane(0, 24); p.feed(b"abc"); });
    s.check("feed_into_zero_col_terminal_does_not_panic", res.is_ok(), format!("{res:?} (config.toml `cols = 0` reaches WindowManager::new -> Terminal::new(0, ..))"));
    // huge
    let base = rss_mb();
    let res = catch(|| { let mut t = Terminal::new(80, 24); t.resize(3000, 1500); t.cursor_row = 1499; t.cursor_col = 2999; (t.grid.len(), t.grid[0].len()) });
    s.info("resize_3000x1500", format!("{res:?} RSS +{} MB (20 B/cell x 2 grids = {} MB; no upper bound on cols/rows from the window system or config)", rss_mb().saturating_sub(base), 3000 * 1500 * 20 * 2 / 1048576));
    let res = catch(|| { let mut t = Terminal::new(80, 24); t.resize(65535, 65535); });
    s.info("resize_65535x65535_would_allocate", format!("{} GB per grid (not executed)", 65535u64 * 65535 * 20 / (1 << 30)));
    let _ = res;
    s.finish();
}

#[test]
fn window_manager_zero_and_tiny_areas() {
    let mut s = Soft::new("robust");
    let res = catch(|| {
        let mut wm = WindowManager::headless(80, 24);
        let min = MinSize::from_cells(10, 20);
        let a = PaneRect { x: 0, y: 30, width: 2400, height: 1400 };
        for i in 0..6 {
            wm.split_active(if i % 2 == 0 { SplitDir::Horizontal } else { SplitDir::Vertical }, a, min);
            wm.resize_all(10, 20, 2400, 1430, 30);
        }
        for (cw, ch, w, h, tb) in [(10usize, 20usize, 0u32, 0u32, 30usize), (10, 20, 1, 1, 30), (0, 0, 800, 600, 30), (10, 20, 800, 10, 30), (10, 20, 5, 5000, 0), (10, 20, u32::MAX / 4, u32::MAX / 4, 30)] {
            wm.resize_all(cw, ch, w, h, tb);
            let sizes: Vec<_> = wm.active_tab().panes().iter().map(|p| (p.terminal.cols, p.terminal.rows)).collect();
            assert!(sizes.iter().all(|(c, r)| *c >= 1 && *r >= 1), "{sizes:?}");
            wm.active_tab().pane_layouts_check();
        }
    });
    s.check("resize_all_degenerate_areas", res.is_ok(), format!("{res:?}"));
    s.finish();
}

trait Check { fn pane_layouts_check(&self); }
impl Check for crate::window::tab::Tab {
    fn pane_layouts_check(&self) {
        let _ = self.tree_layouts(PaneRect { x: 0, y: 0, width: 0, height: 0 });
        let _ = self.tree_layouts(PaneRect { x: 0, y: 30, width: 3, height: 3 });
    }
}

#[test]
fn rapid_tab_churn_and_close_during_output() {
    let mut s = Soft::new("robust");
    let res = catch(|| {
        let mut wm = WindowManager::headless(80, 24);
        let mut rng = Rng(77);
        let a = PaneRect { x: 0, y: 30, width: 1600, height: 900 };
        let min = MinSize::from_cells(10, 20);
        for step in 0..20_000 {
            match rng.below(8) {
                0 | 1 => wm.new_tab(80, 24),
                2 => { let n = wm.tab_count(); wm.close_tab_at(rng.below(n + 1)); }
                3 => { wm.close_current(); if wm.tabs.is_empty() { wm.new_tab(80, 24); } }
                4 => { let n = wm.tab_count(); wm.move_tab(rng.below(n), rng.below(n)); }
                5 => { wm.split_active(SplitDir::Horizontal, a, min); wm.resize_all(10, 20, 1600, 930, 30); }
                6 => { wm.next_tab(); wm.prev_tab(); wm.switch_tab(rng.below(30)); }
                _ => {
                    // output streaming into every pane of every tab while the layout changes
                    for t in &mut wm.tabs { for p in t.panes_mut() { p.feed(format!("stream {step}\r\n").repeat(5).as_bytes()); } }
                    wm.take_tab_events();
                }
            }
            assert!(wm.tabs.is_empty() || wm.active_tab < wm.tabs.len(), "active_tab {} of {} at step {step}", wm.active_tab, wm.tabs.len());
            if wm.tabs.len() > 60 { wm.close_tab_at(0); }
        }
        wm.tab_count()
    });
    s.check("20000_random_tab_pane_ops", res.is_ok(), format!("{res:?}"));
    s.finish();
}

/// Spawns a real shell through the production [`crate::pty::Pty`] (same reader/reaper/hangup
/// machinery as a pane); dropping the guard is "closing the pane".
struct RiftPty {
    _pty: crate::pty::Pty,
    pid: u32,
}
fn spawn_like_rift(cmdline: Option<&str>) -> RiftPty {
    let mut cmd = match cmdline {
        Some(c) => { let mut b = portable_pty::CommandBuilder::new("/bin/sh"); b.arg("-c"); b.arg(c); b }
        None => { let mut b = portable_pty::CommandBuilder::new("/bin/sh"); b.arg("-i"); b }
    };
    cmd.env("PS1", "$ ");
    let pty = crate::pty::Pty::spawn_cmd(80, 24, cmd, std::sync::Arc::new(|| {})).unwrap();
    let pid = pty.pid().unwrap();
    RiftPty { _pty: pty, pid }
}
fn proc_state(pid: u32) -> String {
    let o = std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn closing_panes_leaks_shells_and_zombies() {
    let mut s = Soft::new("robust");
    // idle interactive shell: close the pane
    let mut idle = Vec::new();
    for _ in 0..10 { idle.push(spawn_like_rift(None)); }
    std::thread::sleep(std::time::Duration::from_millis(800));
    let pids: Vec<u32> = idle.iter().map(|p| p.pid).collect();
    drop(idle); // == closing 10 panes
    std::thread::sleep(std::time::Duration::from_secs(3));
    let states: Vec<String> = pids.iter().map(|p| proc_state(*p)).collect();
    let alive = states.iter().filter(|s| !s.is_empty() && !s.starts_with('Z')).count();
    let zombies = states.iter().filter(|s| s.starts_with('Z')).count();
    s.check("closed_idle_shells_terminate", alive == 0, format!("10 panes closed (Pty dropped); 3 s later: {alive} shells still running, {zombies} zombies (defunct), states={states:?}"));
    s.check("no_zombies_left_after_close", zombies == 0, format!("{zombies} zombie children: the Child handle from spawn_command is dropped without wait()"));
    // streaming shell: close while `yes` floods
    let mut busy = Vec::new();
    for _ in 0..5 { busy.push(spawn_like_rift(Some("yes"))); }
    std::thread::sleep(std::time::Duration::from_millis(800));
    let pids: Vec<u32> = busy.iter().map(|p| p.pid).collect();
    drop(busy);
    std::thread::sleep(std::time::Duration::from_secs(3));
    let states: Vec<String> = pids.iter().map(|p| proc_state(*p)).collect();
    let alive = states.iter().filter(|s| !s.is_empty() && !s.starts_with('Z')).count();
    s.check("closing_pane_while_output_streams_stops_producer", alive == 0, format!("5 panes running `yes` closed; 3 s later {alive} still running; states={states:?}"));
    // cleanup anything left so the machine stays clean
    for p in pids.iter() { let _ = std::process::Command::new("kill").args(["-9", &p.to_string()]).output(); }
    s.finish();
}

#[test]
fn fd_and_thread_growth_over_open_close_cycles() {
    let mut s = Soft::new("robust");
    let count_fds = || {
        let o = std::process::Command::new("lsof").args(["-p", &std::process::id().to_string()]).output().unwrap();
        String::from_utf8_lossy(&o.stdout).lines().count()
    };
    let before = count_fds();
    let mut pids = Vec::new();
    for _ in 0..40 {
        let p = spawn_like_rift(None);
        pids.push(p.pid);
        drop(p);
    }
    std::thread::sleep(std::time::Duration::from_secs(3));
    let after = count_fds();
    s.check("fd_count_returns_after_40_open_close_cycles", after < before + 20, format!("lsof lines before={before} after={after}"));
    for p in pids { let _ = std::process::Command::new("kill").args(["-9", &p.to_string()]).output(); }
    s.finish();
}
