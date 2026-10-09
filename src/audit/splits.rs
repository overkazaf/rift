//! Item 4: split trees, focus, resize clamping, zoom, equalize, close, swap, session round trip.
use super::{pane_id as pane, Soft};
use crate::window::tab::{Direction, MinSize, PaneNode, SplitDir, Tab};
use crate::window::{PaneRect, WindowManager};

fn area() -> PaneRect {
    PaneRect { x: 0, y: 30, width: 2400, height: 1400 }
}
fn min() -> MinSize {
    MinSize::from_cells(10, 20)
}
fn overlap(a: &PaneRect, b: &PaneRect) -> bool {
    a.width > 0 && b.width > 0 && a.height > 0 && b.height > 0 && a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}
fn check_layout(tab: &Tab, a: PaneRect) -> Result<(), String> {
    let l = tab.tree_layouts(a);
    if l.len() != tab.pane_count() {
        return Err(format!("layout count {} != pane_count {}", l.len(), tab.pane_count()));
    }
    if tab.active >= tab.pane_count() {
        return Err(format!("active {} >= count {}", tab.active, tab.pane_count()));
    }
    for (i, r) in &l {
        if r.x < a.x || r.y < a.y || r.right() > a.right() || r.bottom() > a.bottom() {
            return Err(format!("pane {i} rect {r:?} escapes area {a:?}"));
        }
    }
    for i in 0..l.len() {
        for j in i + 1..l.len() {
            if overlap(&l[i].1, &l[j].1) {
                return Err(format!("panes {} and {} overlap: {:?} {:?}", l[i].0, l[j].0, l[i].1, l[j].1));
            }
        }
    }
    let ids: Vec<usize> = tab.panes().iter().map(|p| p.id).collect();
    let mut u = ids.clone();
    u.sort();
    u.dedup();
    if u.len() != ids.len() {
        return Err(format!("duplicate pane ids {ids:?}"));
    }
    Ok(())
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn split_30_panes_with_min_size_guard() {
    let mut s = Soft::new("splits");
    let mut wm = WindowManager::headless(80, 24);
    let mut ok = 0;
    let mut refused = 0;
    for i in 0..29 {
        let dir = if i % 2 == 0 { SplitDir::Horizontal } else { SplitDir::Vertical };
        if wm.split_active(dir, area(), min()) {
            ok += 1;
            wm.resize_all(10, 20, 2400, 1430, 30);
            if let Err(e) = check_layout(wm.active_tab(), area()) {
                s.check("layout_invariants", false, e);
            }
        } else {
            refused += 1;
            // try another focus to continue growing
            wm.focus_next_pane();
        }
    }
    s.info("alternating_splits", format!("{ok} accepted, {refused} refused by min-size guard, panes={}", wm.pane_count()));
    s.check("some_splits_work", ok >= 8, format!("{ok} accepted"));
    // every pane must be >= MIN cols/rows after resize_all when guarded
    let small: Vec<_> = wm.active_tab().panes().iter().filter(|p| p.terminal.cols < 20 || p.terminal.rows < 5).map(|p| (p.id, p.terminal.cols, p.terminal.rows)).collect();
    s.check("guarded_panes_respect_min_cells", small.is_empty(), format!("panes below 20x5: {small:?}"));
    s.finish();
}

#[test]
fn deep_nesting_unguarded_30_and_resize_all() {
    let mut s = Soft::new("splits");
    let mut wm = WindowManager::headless(80, 24);
    for i in 1..=30usize {
        let p = pane(i, 40, 12);
        wm.active_tab_mut().split(SplitDir::Horizontal, p);
    }
    s.check("count_31", wm.pane_count() == 31, format!("{}", wm.pane_count()));
    wm.resize_all(10, 20, 2400, 1430, 30);
    let widths: Vec<usize> = wm.active_tab().tree_layouts(area()).iter().map(|(_, r)| r.width).collect();
    s.info("widths_px", format!("{widths:?}"));
    let zero = widths.iter().filter(|w| **w == 0).count();
    s.check("no_zero_width_panes_when_unguarded", zero == 0, format!("{zero} panes have width 0 px (tab.split has no min-size check; guard lives only in WindowManager::split_active)"));
    s.check("layout_ok", check_layout(wm.active_tab(), area()).is_ok(), format!("{:?}", check_layout(wm.active_tab(), area())));
    let cols: Vec<usize> = wm.active_tab().panes().iter().map(|p| p.terminal.cols).collect();
    s.check("terminals_clamped_to_1_col", cols.iter().all(|c| *c >= 1), format!("{cols:?}"));
    s.finish();
}

#[test]
fn focus_swap_resize_zoom_equalize_close() {
    let mut s = Soft::new("splits");
    let a = area();
    // 2x2 grid: split H, then split each side V.
    let mut wm = WindowManager::headless(80, 24);
    assert!(wm.split_active(SplitDir::Horizontal, a, min()));
    wm.resize_all(10, 20, 2400, 1430, 30);
    assert!(wm.split_active(SplitDir::Vertical, a, min())); // right column: active = idx 2
    wm.resize_all(10, 20, 2400, 1430, 30);
    wm.active_tab_mut().focus_pane(0);
    assert!(wm.split_active(SplitDir::Vertical, a, min()));
    wm.resize_all(10, 20, 2400, 1430, 30);
    let tab = wm.active_tab_mut();
    let ids: Vec<usize> = tab.panes().iter().map(|p| p.id).collect();
    s.info("ids_in_order", format!("{ids:?} rects={:?}", tab.tree_layouts(a).iter().map(|(i, r)| (*i, r.x, r.y, r.width, r.height)).collect::<Vec<_>>()));
    // From top-left (idx 0) go Right -> should land on a right-column pane; Down -> bottom-left
    tab.focus_pane(0);
    let moved = tab.focus_dir(a, Direction::Right);
    let r = tab.tree_layouts(a)[tab.active].1;
    s.check("focus_right_goes_to_right_column", moved && r.x > a.width / 2, format!("moved={moved} active={} rect={r:?}", tab.active));
    let back = tab.focus_dir(a, Direction::Left);
    s.check("focus_left_returns_to_left_column", back && tab.tree_layouts(a)[tab.active].1.x == 0, format!("active={}", tab.active));
    tab.focus_pane(0);
    s.check("focus_up_at_edge_is_noop", !tab.focus_dir(a, Direction::Up) && tab.active == 0, format!("active={}", tab.active));
    s.check("focus_left_at_edge_is_noop", !tab.focus_dir(a, Direction::Left) && tab.active == 0, "");
    let down = tab.focus_dir(a, Direction::Down);
    s.check("focus_down", down && tab.active == 1, format!("active={}", tab.active));

    // swap: focus follows the pane
    tab.focus_pane(0);
    let id0 = tab.pane(0).unwrap().id;
    let swapped = tab.swap_dir(a, Direction::Right);
    let now_active_id = tab.active_pane().id;
    s.check("swap_focus_follows_pane", swapped && now_active_id == id0, format!("swapped={swapped} active_id={now_active_id} expected {id0}"));
    let l = tab.tree_layouts(a);
    s.check("swap_moved_pane_to_right", l[tab.active].1.x > a.width / 2, format!("{:?}", l[tab.active].1));

    // resize clamping: push the divider 200 times to the right; neighbor must keep min width
    let mut wm2 = WindowManager::headless(80, 24);
    wm2.split_active(SplitDir::Horizontal, a, min());
    wm2.resize_all(10, 20, 2400, 1430, 30);
    wm2.active_tab_mut().focus_pane(0);
    for _ in 0..200 {
        wm2.active_tab_mut().resize_dir(a, Direction::Right, min());
    }
    let l = wm2.active_tab().tree_layouts(a);
    s.check("resize_right_clamped_keeps_min_width", l[1].1.width >= min().w, format!("right pane width {} px (min {})", l[1].1.width, min().w));
    for _ in 0..400 {
        wm2.active_tab_mut().resize_dir(a, Direction::Left, min());
    }
    let l = wm2.active_tab().tree_layouts(a);
    s.check("resize_left_clamped_keeps_min_width", l[0].1.width >= min().w, format!("left pane width {} px", l[0].1.width));
    s.check("resize_all_consistent", check_layout(wm2.active_tab(), a).is_ok(), format!("{:?}", check_layout(wm2.active_tab(), a)));

    // zoom
    let mut wm3 = WindowManager::headless(80, 24);
    for _ in 0..3 {
        wm3.split_active(SplitDir::Horizontal, a, min());
        wm3.resize_all(10, 20, 2400, 1430, 30);
    }
    let z = wm3.active_tab_mut().toggle_zoom();
    wm3.resize_all(10, 20, 2400, 1430, 30);
    let lay = wm3.pane_layouts(a);
    s.check("zoom_shows_only_active_full_area", z && lay.len() == 1 && lay[0].1 == a, format!("{lay:?}"));
    let zc = wm3.active_pane().terminal.cols;
    s.check("zoomed_pane_terminal_resized", zc == 240, format!("cols={zc} (expect 2400/10)"));
    let before = wm3.pane_count();
    wm3.close_current();
    s.check("close_while_zoomed_unzooms", !wm3.active_tab().is_zoomed() && wm3.pane_count() == before - 1, format!("zoomed={} count={}", wm3.active_tab().is_zoomed(), wm3.pane_count()));
    wm3.resize_all(10, 20, 2400, 1430, 30);
    // after unzoom, other panes must be re-laid out (tree sizes restored)
    let colsv: Vec<usize> = wm3.active_tab().panes().iter().map(|p| p.terminal.cols).collect();
    s.check("unzoom_restores_pane_sizes", colsv.iter().all(|c| *c < 240), format!("{colsv:?}"));

    // equalize a 5-chain
    let mut wm4 = WindowManager::headless(80, 24);
    for _ in 0..4 {
        wm4.split_active(SplitDir::Horizontal, a, min());
        wm4.resize_all(10, 20, 2400, 1430, 30);
    }
    wm4.active_tab_mut().equalize();
    let w: Vec<usize> = wm4.active_tab().tree_layouts(a).iter().map(|(_, r)| r.width).collect();
    let (mn, mx) = (*w.iter().min().unwrap(), *w.iter().max().unwrap());
    s.check("equalize_chain_equal_widths", mx - mn <= 6, format!("{w:?}"));

    // close every pane in turn, always valid focus
    let mut wm5 = WindowManager::headless(80, 24);
    for i in 0..9 {
        wm5.split_active(if i % 2 == 0 { SplitDir::Horizontal } else { SplitDir::Vertical }, a, min());
        wm5.resize_all(10, 20, 2400, 1430, 30);
    }
    let mut rng = Rng(0x1234_5678_9abc_def1);
    while wm5.pane_count() > 1 {
        let n = wm5.pane_count();
        wm5.active_tab_mut().focus_pane(rng.below(n));
        let last = wm5.close_current();
        if let Err(e) = check_layout(wm5.active_tab(), a) {
            s.check("close_sequence_invariants", false, e);
            break;
        }
        if last {
            break;
        }
    }
    s.check("close_down_to_one", wm5.pane_count() == 1, format!("{}", wm5.pane_count()));
    s.finish();
}

#[test]
fn fuzz_20000_random_ops_keep_invariants() {
    let mut s = Soft::new("splits");
    let a = area();
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    let mut tab = Tab::new(pane(0, 80, 24));
    let mut next_id = 1usize;
    let mut failure = None;
    for step in 0..20_000 {
        let op = rng.below(13);
        let n = tab.pane_count();
        match op {
            0 | 1 if n < 40 => {
                let d = if rng.below(2) == 0 { SplitDir::Horizontal } else { SplitDir::Vertical };
                if tab.can_split(a, d, min()) {
                    tab.split(d, pane(next_id, 10, 10));
                    next_id += 1;
                }
            }
            2 => {
                tab.close_pane(a);
            }
            3 => { tab.focus_dir(a, [Direction::Left, Direction::Right, Direction::Up, Direction::Down][rng.below(4)]); }
            4 => { tab.swap_dir(a, [Direction::Left, Direction::Right, Direction::Up, Direction::Down][rng.below(4)]); }
            5 => { tab.resize_dir(a, [Direction::Left, Direction::Right, Direction::Up, Direction::Down][rng.below(4)], min()); }
            6 => { tab.toggle_zoom(); }
            7 => tab.equalize(),
            8 => tab.focus_next(),
            9 => tab.focus_prev(),
            10 => {
                let b = tab.split_borders(a);
                if !b.is_empty() {
                    let i = rng.below(b.len());
                    tab.drag_border(a, i, rng.below(2400), 30 + rng.below(1400), min());
                }
            }
            11 => { tab.equalize_border(rng.below(8)); }
            _ => { tab.pane_at(a, rng.below(2500), rng.below(1500)); }
        }
        if let Err(e) = check_layout(&tab, a) {
            failure = Some((step, op, e));
            break;
        }
    }
    s.check("invariants_hold_20000_ops", failure.is_none(), format!("{failure:?}"));
    s.info("final", format!("panes={}", tab.pane_count()));
    s.finish();
}

fn shape(n: &PaneNode<crate::window::Pane>) -> String {
    match n {
        PaneNode::Leaf(_) => "L".into(),
        PaneNode::Split { dir, ratio, first, second } => format!("{}{:.3}({},{})", if *dir == SplitDir::Horizontal { "h" } else { "v" }, ratio, shape(first), shape(second)),
        PaneNode::Empty => "E".into(),
    }
}
fn shape_state(n: &PaneNode<Option<String>>) -> String {
    match n {
        PaneNode::Leaf(_) => "L".into(),
        PaneNode::Split { dir, ratio, first, second } => format!("{}{:.3}({},{})", if *dir == SplitDir::Horizontal { "h" } else { "v" }, ratio, shape_state(first), shape_state(second)),
        PaneNode::Empty => "E".into(),
    }
}

/// Requires a sandbox HOME: `HOME=$(mktemp -d -t rift-audit) cargo test ... audit::splits::session`.
#[test]
fn session_save_restore_roundtrip() {
    let Some(home) = super::sandbox_home("session") else { return };
    let mut s = Soft::new("session");
    let a = area();
    let mut wm = WindowManager::headless(80, 24);
    for i in 0..5 {
        wm.split_active(if i % 2 == 0 { SplitDir::Horizontal } else { SplitDir::Vertical }, a, min());
        wm.resize_all(10, 20, 2400, 1430, 30);
    }
    wm.active_tab_mut().drag_border(a, 0, 700, 400, min());
    wm.active_tab_mut().focus_pane(2);
    wm.rename_tab(0, "he said \"hi\" \\ back\nline2\t\u{1}末尾 🙂");
    wm.new_tab(80, 24);
    wm.split_active(SplitDir::Vertical, a, min());
    let before: Vec<String> = wm.tabs.iter().map(|t| shape(&t.root)).collect();
    crate::tools::session::save_session(&wm).expect("save");
    let raw = std::fs::read_to_string(format!("{home}/.config/rift/session.json")).unwrap();
    s.info("session_json_bytes", format!("{}", raw.len()));
    let st = crate::tools::session::load_session();
    s.check("load_ok", st.is_some(), "session parsed");
    if let Some(st) = st {
        s.check("tab_count", st.tabs.len() == 2, format!("{}", st.tabs.len()));
        s.check("title_roundtrip", st.tabs[0].title == wm.tabs[0].title, format!("{:?} vs {:?}", st.tabs[0].title, wm.tabs[0].title));
        let after: Vec<String> = st.tabs.iter().map(|t| t.layout.as_ref().map_or("L".into(), shape_state)).collect();
        s.check("layout_shapes_equal", before == after, format!("before={before:?} after={after:?}"));
        s.check("active_pane_saved", st.tabs[0].active_pane == 2, format!("{}", st.tabs[0].active_pane));
        // restore into a new headless wm
        let mut wm2 = WindowManager::headless(80, 24);
        crate::tools::session::restore_session(&mut wm2);
        let restored: Vec<String> = wm2.tabs.iter().map(|t| shape(&t.root)).collect();
        s.check("restore_layout_equal", restored == before, format!("restored={restored:?}"));
        s.check("restore_active_pane", wm2.tabs[0].active == 2, format!("{}", wm2.tabs[0].active));
    }
    // layout deeper than MAX_LAYOUT_DEPTH (32) -> silently dropped?
    let mut wm3 = WindowManager::headless(80, 24);
    for i in 1..=40usize {
        wm3.active_tab_mut().split(SplitDir::Horizontal, pane(i, 10, 10));
    }
    crate::tools::session::save_session(&wm3).unwrap();
    let st = crate::tools::session::load_session().unwrap();
    s.check("deep_40_chain_layout_survives", st.tabs[0].layout.is_some(), format!("layout present={} (41 panes saved)", st.tabs[0].layout.is_some()));
    let _ = std::fs::remove_file(format!("{home}/.config/rift/session.json"));
    s.finish();
}
