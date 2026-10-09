use super::pane::Pane;

/// Direction of a split: `Horizontal` puts children side by side (left | right),
/// `Vertical` stacks them (top / bottom).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SplitDir {
    Horizontal,
    Vertical,
}

/// Screen direction used for focus / swap / keyboard resize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    /// Orientation of the divider that moves when resizing in this direction.
    pub fn axis(self) -> SplitDir {
        match self {
            Direction::Left | Direction::Right => SplitDir::Horizontal,
            Direction::Up | Direction::Down => SplitDir::Vertical,
        }
    }
}

/// Every pane-management action reachable from keyboard, menu and palette.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PaneCmd {
    SplitRight,
    SplitDown,
    ClosePane,
    Zoom,
    Equalize,
    Focus(Direction),
    Swap(Direction),
    Resize(Direction),
    FocusNext,
    FocusPrev,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneRect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl PaneRect {
    fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    pub fn right(&self) -> usize {
        self.x + self.width
    }

    pub fn bottom(&self) -> usize {
        self.y + self.height
    }
}

/// Smallest allowed pane, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MinSize {
    pub w: usize,
    pub h: usize,
}

/// A pane never gets narrower than this many columns...
pub const MIN_COLS: usize = 20;
/// ...or shorter than this many rows.
pub const MIN_ROWS: usize = 5;

impl MinSize {
    pub fn from_cells(cell_w: usize, cell_h: usize) -> Self {
        Self { w: MIN_COLS * cell_w.max(1), h: MIN_ROWS * cell_h.max(1) }
    }

    fn along(&self, dir: SplitDir) -> usize {
        match dir {
            SplitDir::Horizontal => self.w,
            SplitDir::Vertical => self.h,
        }
    }
}

pub const BORDER: usize = 1;
/// Absolute ratio limits (min-size limits are applied on top of these).
/// Panes in one row/column beyond which `Tab::split` re-balances to equal shares.
const MAX_HALVED_ROW: usize = 6;
const MIN_RATIO: f32 = 0.05;
const MAX_RATIO: f32 = 0.95;
/// Keyboard resize step as a fraction of the split's extent.
pub const RESIZE_STEP: f32 = 0.05;

/// Recursive split tree. Leaves hold panes (or any `T`, which keeps the
/// geometry unit-testable without PTYs); splits hold two children and a
/// draggable ratio. Leaf indices are assigned by in-order (left-to-right,
/// top-to-bottom) traversal.
pub enum PaneNode<T> {
    Leaf(T),
    Split {
        dir: SplitDir,
        ratio: f32,
        first: Box<PaneNode<T>>,
        second: Box<PaneNode<T>>,
    },
    /// Transient placeholder used only while restructuring the tree.
    Empty,
}

pub type PaneTree = PaneNode<Pane>;

fn extent(area: PaneRect, dir: SplitDir) -> usize {
    match dir {
        SplitDir::Horizontal => area.width,
        SplitDir::Vertical => area.height,
    }
}

/// Divide `area` into the two child rects of a split.
fn split_rects(area: PaneRect, dir: SplitDir, ratio: f32) -> (PaneRect, PaneRect) {
    match dir {
        // Both rects always stay inside `area` (first <= extent - BORDER, second
        // starts at most at the far edge), even for degenerate / tiny areas.
        SplitDir::Horizontal => {
            let w1 = (((area.width as f32) * ratio) as usize).min(area.width.saturating_sub(BORDER));
            let w2 = area.width.saturating_sub(w1 + BORDER);
            (
                PaneRect { x: area.x, y: area.y, width: w1, height: area.height },
                PaneRect { x: (area.x + w1 + BORDER).min(area.x + area.width), y: area.y, width: w2, height: area.height },
            )
        }
        SplitDir::Vertical => {
            let h1 = (((area.height as f32) * ratio) as usize).min(area.height.saturating_sub(BORDER));
            let h2 = area.height.saturating_sub(h1 + BORDER);
            (
                PaneRect { x: area.x, y: area.y, width: area.width, height: h1 },
                PaneRect { x: area.x, y: (area.y + h1 + BORDER).min(area.y + area.height), width: area.width, height: h2 },
            )
        }
    }
}

/// Clamp `ratio` so that the first child keeps at least `m1` and the second at
/// least `m2` pixels of a `total`-pixel extent. Falls back to a centred split
/// when both minimums cannot be honoured.
pub fn clamp_ratio(ratio: f32, total: usize, m1: usize, m2: usize) -> f32 {
    if total == 0 {
        return 0.5;
    }
    let t = total as f32;
    // +0.5 so float truncation in `split_rects` never lands one pixel short.
    let lo = ((m1 as f32 + 0.5) / t).max(MIN_RATIO);
    let hi = ((total.saturating_sub(BORDER + m2)) as f32 / t).min(MAX_RATIO);
    if lo > hi {
        return 0.5;
    }
    ratio.clamp(lo, hi)
}

fn overlap(a0: usize, a1: usize, b0: usize, b1: usize) -> usize {
    a1.min(b1).saturating_sub(a0.max(b0))
}

/// Nearest pane in `dir` from pane `from`, using real rects. Candidates must
/// lie fully beyond the edge and overlap on the perpendicular axis; the
/// smallest gap wins, then the largest overlap, then the closest centre line,
/// then the lowest index.
pub fn neighbor_in_direction(rects: &[(usize, PaneRect)], from: usize, dir: Direction) -> Option<usize> {
    let f = rects.iter().find(|(i, _)| *i == from)?.1;
    let f_center = match dir.axis() {
        SplitDir::Horizontal => f.y + f.height / 2,
        SplitDir::Vertical => f.x + f.width / 2,
    };
    rects
        .iter()
        .filter(|(i, _)| *i != from)
        .filter_map(|(i, c)| {
            let (gap, ov, center) = match dir {
                Direction::Right if c.x >= f.right() => {
                    (c.x - f.right(), overlap(f.y, f.bottom(), c.y, c.bottom()), c.y + c.height / 2)
                }
                Direction::Left if c.right() <= f.x => {
                    (f.x - c.right(), overlap(f.y, f.bottom(), c.y, c.bottom()), c.y + c.height / 2)
                }
                Direction::Down if c.y >= f.bottom() => {
                    (c.y - f.bottom(), overlap(f.x, f.right(), c.x, c.right()), c.x + c.width / 2)
                }
                Direction::Up if c.bottom() <= f.y => {
                    (f.y - c.bottom(), overlap(f.x, f.right(), c.x, c.right()), c.x + c.width / 2)
                }
                _ => return None,
            };
            if ov == 0 {
                return None;
            }
            Some(((gap, std::cmp::Reverse(ov), center.abs_diff(f_center), *i), *i))
        })
        .min_by_key(|(k, _)| *k)
        .map(|(_, i)| i)
}

/// Length of the border shared by two rects (0 when they do not touch).
fn shared_edge(a: &PaneRect, b: &PaneRect) -> usize {
    let touch_x = a.right() <= b.x && b.x - a.right() <= BORDER || b.right() <= a.x && a.x - b.right() <= BORDER;
    let touch_y = a.bottom() <= b.y && b.y - a.bottom() <= BORDER || b.bottom() <= a.y && a.y - b.bottom() <= BORDER;
    if touch_x {
        overlap(a.y, a.bottom(), b.y, b.bottom())
    } else if touch_y {
        overlap(a.x, a.right(), b.x, b.right())
    } else {
        0
    }
}

/// Which of `candidates` (pre-removal leaf indices) takes over the space of
/// `removed`: the one sharing the longest edge with it, then the closest
/// centre, then the last index.
pub fn absorb_target(rects: &[(usize, PaneRect)], removed: usize, candidates: &[usize]) -> Option<usize> {
    let r = rects.iter().find(|(i, _)| *i == removed)?.1;
    let (rcx, rcy) = (r.x + r.width / 2, r.y + r.height / 2);
    candidates
        .iter()
        .filter_map(|&c| rects.iter().find(|(i, _)| *i == c).map(|(i, rc)| (*i, *rc)))
        .max_by_key(|(i, rc)| {
            let dist = (rc.x + rc.width / 2).abs_diff(rcx) + (rc.y + rc.height / 2).abs_diff(rcy);
            (shared_edge(&r, rc), std::cmp::Reverse(dist), *i)
        })
        .map(|(i, _)| i)
}

impl<T> PaneNode<T> {
    fn count(&self) -> usize {
        match self {
            PaneNode::Leaf(_) => 1,
            PaneNode::Split { first, second, .. } => first.count() + second.count(),
            PaneNode::Empty => 0,
        }
    }

    fn collect<'a>(&'a self, out: &mut Vec<&'a T>) {
        match self {
            PaneNode::Leaf(p) => out.push(p),
            PaneNode::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
            PaneNode::Empty => {}
        }
    }

    fn collect_mut<'a>(&'a mut self, out: &mut Vec<&'a mut T>) {
        match self {
            PaneNode::Leaf(p) => out.push(p),
            PaneNode::Split { first, second, .. } => {
                first.collect_mut(out);
                second.collect_mut(out);
            }
            PaneNode::Empty => {}
        }
    }

    fn layouts(&self, area: PaneRect, idx: &mut usize, out: &mut Vec<(usize, PaneRect)>) {
        match self {
            PaneNode::Leaf(_) => {
                out.push((*idx, area));
                *idx += 1;
            }
            PaneNode::Split { dir, ratio, first, second } => {
                let (a, b) = split_rects(area, *dir, *ratio);
                first.layouts(a, idx, out);
                second.layouts(b, idx, out);
            }
            PaneNode::Empty => {}
        }
    }

    fn borders(&self, area: PaneRect, out: &mut Vec<SplitBorder>) {
        if let PaneNode::Split { dir, ratio, first, second } = self {
            let (a, b) = split_rects(area, *dir, *ratio);
            let pos = match dir {
                SplitDir::Horizontal => a.x + a.width,
                SplitDir::Vertical => a.y + a.height,
            };
            out.push(SplitBorder { dir: *dir, pos, area });
            first.borders(a, out);
            second.borders(b, out);
        }
    }

    /// Split the leaf with in-order index `target` (counting via `idx`).
    /// Returns true once the split was performed.
    fn split_leaf(&mut self, target: usize, idx: &mut usize, dir: SplitDir, new_leaf: &mut Option<T>) -> bool {
        match self {
            PaneNode::Leaf(_) => {
                if *idx != target {
                    *idx += 1;
                    return false;
                }
                let old = std::mem::replace(self, PaneNode::Empty);
                let fresh = new_leaf.take().expect("new pane consumed once");
                *self = PaneNode::Split {
                    dir,
                    ratio: 0.5,
                    first: Box::new(old),
                    second: Box::new(PaneNode::Leaf(fresh)),
                };
                true
            }
            PaneNode::Split { first, second, .. } => {
                first.split_leaf(target, idx, dir, new_leaf) || second.split_leaf(target, idx, dir, new_leaf)
            }
            PaneNode::Empty => false,
        }
    }

    /// Remove the leaf with in-order index `target`; its sibling takes the
    /// parent's place. Returns true once removed.
    fn remove_leaf(&mut self, target: usize, idx: &mut usize) -> bool {
        let PaneNode::Split { first, second, .. } = self else {
            return false;
        };
        let first_count = first.count();
        let promote = if *idx + first_count > target {
            // target lives in `first`
            if matches!(**first, PaneNode::Leaf(_)) {
                Some(std::mem::replace(&mut **second, PaneNode::Empty))
            } else {
                return first.remove_leaf(target, idx);
            }
        } else {
            *idx += first_count;
            if matches!(**second, PaneNode::Leaf(_)) && *idx == target {
                Some(std::mem::replace(&mut **first, PaneNode::Empty))
            } else {
                return second.remove_leaf(target, idx);
            }
        };
        if let Some(sibling) = promote {
            *self = sibling;
            return true;
        }
        false
    }

    /// Leaf indices (pre-removal) of the sibling subtree of leaf `target`,
    /// i.e. the panes that will absorb its space when it is closed.
    fn sibling_range(&self, target: usize, base: usize) -> Option<std::ops::Range<usize>> {
        let PaneNode::Split { first, second, .. } = self else {
            return None;
        };
        let fc = first.count();
        if matches!(**first, PaneNode::Leaf(_)) && base == target {
            return Some(base + 1..base + 1 + second.count());
        }
        if matches!(**second, PaneNode::Leaf(_)) && base + fc == target {
            return Some(base..base + fc);
        }
        if target < base + fc {
            first.sibling_range(target, base)
        } else {
            second.sibling_range(target, base + fc)
        }
    }

    /// Minimum extent this subtree needs along `dir`.
    fn min_extent(&self, dir: SplitDir, min: MinSize) -> usize {
        match self {
            PaneNode::Leaf(_) => min.along(dir),
            PaneNode::Split { dir: d, first, second, .. } => {
                let (a, b) = (first.min_extent(dir, min), second.min_extent(dir, min));
                if *d == dir { a + BORDER + b } else { a.max(b) }
            }
            PaneNode::Empty => 0,
        }
    }

    /// Number of cells this subtree occupies "in a row" along `dir`.
    fn row_weight(&self, dir: SplitDir) -> usize {
        match self {
            PaneNode::Split { dir: d, first, second, .. } if *d == dir => {
                first.row_weight(dir) + second.row_weight(dir)
            }
            PaneNode::Empty => 0,
            _ => 1,
        }
    }

    /// Balance every split; same-direction chains get equal shares.
    fn equalize(&mut self) {
        if let PaneNode::Split { dir, ratio, first, second } = self {
            let (a, b) = (first.row_weight(*dir), second.row_weight(*dir));
            if a + b > 0 {
                *ratio = a as f32 / (a + b) as f32;
            }
            first.equalize();
            second.equalize();
        }
    }

    /// Set the ratio of border `target` (pre-order) to `value`.
    fn set_ratio_at(&mut self, target: usize, idx: &mut usize, value: f32) -> bool {
        let PaneNode::Split { ratio, first, second, .. } = self else {
            return false;
        };
        if *idx == target {
            *ratio = value;
            return true;
        }
        *idx += 1;
        first.set_ratio_at(target, idx, value) || second.set_ratio_at(target, idx, value)
    }

    /// Move the divider of border `target` (pre-order) under the mouse,
    /// honouring the minimum pane size.
    fn set_ratio(&mut self, area: PaneRect, target: usize, idx: &mut usize, mx: usize, my: usize, min: MinSize) -> bool {
        let PaneNode::Split { dir, ratio, first, second } = self else {
            return false;
        };
        if *idx == target {
            let total = extent(area, *dir);
            let r = match dir {
                SplitDir::Horizontal => (mx.saturating_sub(area.x)) as f32 / total.max(1) as f32,
                SplitDir::Vertical => (my.saturating_sub(area.y)) as f32 / total.max(1) as f32,
            };
            let (m1, m2) = (first.min_extent(*dir, min), second.min_extent(*dir, min));
            *ratio = clamp_ratio(r, total, m1, m2);
            return true;
        }
        *idx += 1;
        let (a, b) = split_rects(area, *dir, *ratio);
        first.set_ratio(a, target, idx, mx, my, min) || second.set_ratio(b, target, idx, mx, my, min)
    }

    /// Nudge the nearest ancestor divider of orientation `want` that sits
    /// above leaf `target` by `delta` (positive = right/down).
    fn nudge_ancestor(&mut self, area: PaneRect, target: usize, base: usize, want: SplitDir, delta: f32, min: MinSize) -> bool {
        let PaneNode::Split { dir, ratio, first, second } = self else {
            return false;
        };
        let (a, b) = split_rects(area, *dir, *ratio);
        let fc = first.count();
        let handled = if target < base + fc {
            first.nudge_ancestor(a, target, base, want, delta, min)
        } else {
            second.nudge_ancestor(b, target, base + fc, want, delta, min)
        };
        if handled {
            return true;
        }
        if *dir != want {
            return false;
        }
        let total = extent(area, *dir);
        let (m1, m2) = (first.min_extent(*dir, min), second.min_extent(*dir, min));
        *ratio = clamp_ratio(*ratio + delta, total, m1, m2);
        true
    }

    /// Remove leaf `removed` and return the in-order index of the pane that
    /// should take focus: the sibling-subtree leaf that absorbs its space.
    fn close_with_focus(&mut self, area: PaneRect, removed: usize) -> usize {
        let mut rects = Vec::new();
        self.layouts(area, &mut 0, &mut rects);
        let candidates: Vec<usize> = self.sibling_range(removed, 0).map(|r| r.collect()).unwrap_or_default();
        self.remove_leaf(removed, &mut 0);
        let pre = absorb_target(&rects, removed, &candidates).unwrap_or(removed.saturating_sub(1));
        let post = if pre > removed { pre - 1 } else { pre };
        post.min(self.count().saturating_sub(1))
    }

    /// Exchange the values of leaves `a` and `b` (in-order indices).
    fn swap_leaves(&mut self, a: usize, b: usize) {
        if a == b {
            return;
        }
        let mut leaves = Vec::new();
        self.collect_mut(&mut leaves);
        if a.max(b) >= leaves.len() {
            return;
        }
        let (lo, hi) = (a.min(b), a.max(b));
        let (left, right) = leaves.split_at_mut(hi);
        std::mem::swap(&mut *left[lo], &mut *right[0]);
    }

    /// Same tree shape with every leaf transformed by `f` (in order).
    pub fn map_leaves<U>(&self, f: &mut impl FnMut(&T) -> U) -> PaneNode<U> {
        match self {
            PaneNode::Leaf(t) => PaneNode::Leaf(f(t)),
            PaneNode::Split { dir, ratio, first, second } => PaneNode::Split {
                dir: *dir,
                ratio: *ratio,
                first: Box::new(first.map_leaves(f)),
                second: Box::new(second.map_leaves(f)),
            },
            PaneNode::Empty => PaneNode::Empty,
        }
    }
}

/// A draggable divider between two children of a split.
#[derive(Clone, Copy, Debug)]
pub struct SplitBorder {
    pub dir: SplitDir,
    /// x of the divider column (Horizontal) or y of the divider row (Vertical)
    pub pos: usize,
    /// Area of the split that owns this divider
    pub area: PaneRect,
}

impl SplitBorder {
    fn hit(&self, x: usize, y: usize, tolerance: usize) -> bool {
        match self.dir {
            SplitDir::Horizontal => {
                x.abs_diff(self.pos) <= tolerance && y >= self.area.y && y < self.area.y + self.area.height
            }
            SplitDir::Vertical => {
                y.abs_diff(self.pos) <= tolerance && x >= self.area.x && x < self.area.x + self.area.width
            }
        }
    }
}

/// Pre-order index of the divider under (x, y).
pub fn border_hit(borders: &[SplitBorder], x: usize, y: usize, tolerance: usize) -> Option<(usize, SplitDir)> {
    borders.iter().enumerate().find(|(_, b)| b.hit(x, y, tolerance)).map(|(i, b)| (i, b.dir))
}

/// Can `rect` be split in half along `dir` and leave both halves >= `min`?
pub fn can_split_rect(rect: PaneRect, dir: SplitDir, min: MinSize) -> bool {
    let (a, b) = split_rects(rect, dir, 0.5);
    match dir {
        SplitDir::Horizontal => a.width >= min.w && b.width >= min.w,
        SplitDir::Vertical => a.height >= min.h && b.height >= min.h,
    }
}

pub struct Tab {
    pub title: String,
    pub custom_title: bool,
    pub root: PaneTree,
    pub active: usize,
    /// The active pane fills the whole content area; other panes keep running.
    pub zoomed: bool,
}

impl Tab {
    pub fn new(pane: Pane) -> Self {
        Self {
            title: "Tab 1".to_string(),
            custom_title: false,
            root: PaneNode::Leaf(pane),
            active: 0,
            zoomed: false,
        }
    }

    pub fn pane_count(&self) -> usize {
        self.root.count()
    }

    /// All panes in index order.
    pub fn panes(&self) -> Vec<&Pane> {
        let mut out = Vec::new();
        self.root.collect(&mut out);
        out
    }

    pub fn panes_mut(&mut self) -> Vec<&mut Pane> {
        let mut out = Vec::new();
        self.root.collect_mut(&mut out);
        out
    }

    pub fn pane(&self, idx: usize) -> Option<&Pane> {
        self.panes().into_iter().nth(idx)
    }

    pub fn pane_mut(&mut self, idx: usize) -> Option<&mut Pane> {
        self.panes_mut().into_iter().nth(idx)
    }

    pub fn active_pane(&self) -> &Pane {
        self.pane(self.active).expect("active pane index in range")
    }

    pub fn active_pane_mut(&mut self) -> &mut Pane {
        let active = self.active;
        self.pane_mut(active).expect("active pane index in range")
    }

    /// Rects of every pane as laid out by the split tree (ignores zoom).
    pub fn tree_layouts(&self, area: PaneRect) -> Vec<(usize, PaneRect)> {
        let mut out = Vec::new();
        self.root.layouts(area, &mut 0, &mut out);
        out
    }

    /// Visible panes. When zoomed only the active pane, filling `area`.
    pub fn layouts(&self, area: PaneRect) -> Vec<(usize, PaneRect, bool)> {
        if self.zoomed && self.pane_count() > 1 {
            return vec![(self.active, area, true)];
        }
        self.tree_layouts(area).into_iter().map(|(i, r)| (i, r, i == self.active)).collect()
    }

    /// Dividers in pre-order (empty while zoomed).
    pub fn split_borders(&self, area: PaneRect) -> Vec<SplitBorder> {
        let mut out = Vec::new();
        if !(self.zoomed && self.pane_count() > 1) {
            self.root.borders(area, &mut out);
        }
        out
    }

    pub fn is_zoomed(&self) -> bool {
        self.zoomed && self.pane_count() > 1
    }

    pub fn unzoom(&mut self) -> bool {
        std::mem::replace(&mut self.zoomed, false)
    }

    /// Toggle zoom of the active pane. Returns the new state.
    pub fn toggle_zoom(&mut self) -> bool {
        if self.zoomed {
            self.zoomed = false;
        } else if self.pane_count() > 1 {
            self.zoomed = true;
        }
        self.zoomed
    }

    /// Would splitting the active pane along `dir` keep both halves above the minimum?
    pub fn can_split(&self, area: PaneRect, dir: SplitDir, min: MinSize) -> bool {
        self.tree_layouts(area)
            .into_iter()
            .find(|(i, _)| *i == self.active)
            .map_or(false, |(_, r)| can_split_rect(r, dir, min))
    }

    /// Split the active pane; the new pane takes focus. Unzooms first.
    pub fn split(&mut self, dir: SplitDir, new_pane: Pane) {
        self.zoomed = false;
        let target = self.active;
        let mut slot = Some(new_pane);
        if self.root.split_leaf(target, &mut 0, dir, &mut slot) {
            // The new leaf sits right after the split one in in-order traversal.
            self.active = target + 1;
            // Unguarded callers can nest splits arbitrarily deep; halving per level
            // would drive panes to zero width. Past a handful of panes in one row
            // or column, fall back to equal shares so every pane keeps real size.
            let crowd = self.root.row_weight(SplitDir::Horizontal).max(self.root.row_weight(SplitDir::Vertical));
            if crowd > MAX_HALVED_ROW {
                self.root.equalize();
            }
        }
    }

    /// Close the active pane. Returns true when it was the last pane (close tab).
    /// Focus moves to the pane that absorbs the freed space.
    pub fn close_pane(&mut self, area: PaneRect) -> bool {
        if self.pane_count() <= 1 {
            return true;
        }
        self.zoomed = false;
        let removed = self.active;
        self.active = self.root.close_with_focus(area, removed);
        false
    }

    pub fn focus_pane(&mut self, idx: usize) {
        if idx < self.pane_count() {
            self.active = idx;
        }
    }

    pub fn focus_next(&mut self) {
        let n = self.pane_count();
        if n > 0 {
            self.zoomed = false;
            self.active = (self.active + 1) % n;
        }
    }

    pub fn focus_prev(&mut self) {
        let n = self.pane_count();
        if n > 0 {
            self.zoomed = false;
            self.active = self.active.checked_sub(1).unwrap_or(n - 1);
        }
    }

    /// Pane adjacent to the active one in `dir`, if any (ignores zoom).
    pub fn neighbor(&self, area: PaneRect, dir: Direction) -> Option<usize> {
        neighbor_in_direction(&self.tree_layouts(area), self.active, dir)
    }

    /// Move focus geometrically. Returns true when focus moved.
    pub fn focus_dir(&mut self, area: PaneRect, dir: Direction) -> bool {
        match self.neighbor(area, dir) {
            Some(n) => {
                self.zoomed = false;
                self.active = n;
                true
            }
            None => false,
        }
    }

    /// Swap the active pane with its neighbour in `dir`; focus follows the pane.
    pub fn swap_dir(&mut self, area: PaneRect, dir: Direction) -> bool {
        let Some(n) = self.neighbor(area, dir) else { return false };
        self.zoomed = false;
        let a = self.active;
        self.root.swap_leaves(a, n);
        self.active = n;
        true
    }

    /// Grow/shrink the active pane by moving the nearest ancestor divider of
    /// the matching orientation one step toward `dir`.
    pub fn resize_dir(&mut self, area: PaneRect, dir: Direction, min: MinSize) -> bool {
        self.zoomed = false;
        let delta = match dir {
            Direction::Right | Direction::Down => RESIZE_STEP,
            Direction::Left | Direction::Up => -RESIZE_STEP,
        };
        let active = self.active;
        self.root.nudge_ancestor(area, active, 0, dir.axis(), delta, min)
    }

    pub fn equalize(&mut self) {
        self.root.equalize();
    }

    /// Reset one divider (pre-order index) to 0.5.
    pub fn equalize_border(&mut self, border_idx: usize) -> bool {
        self.root.set_ratio_at(border_idx, &mut 0, 0.5)
    }

    pub fn display_title(&self) -> &str {
        &self.title
    }

    /// Index of the divider under (x, y), if any.
    pub fn border_at(&self, area: PaneRect, x: usize, y: usize, tolerance: usize) -> Option<(usize, SplitDir)> {
        border_hit(&self.split_borders(area), x, y, tolerance)
    }

    /// Move divider `border_idx` so it sits under the mouse.
    pub fn drag_border(&mut self, area: PaneRect, border_idx: usize, x: usize, y: usize, min: MinSize) -> bool {
        self.root.set_ratio(area, border_idx, &mut 0, x, y, min)
    }

    /// Index of the pane under (x, y), if any.
    pub fn pane_at(&self, area: PaneRect, x: usize, y: usize) -> Option<usize> {
        self.layouts(area).into_iter().find(|(_, r, _)| r.contains(x, y)).map(|(i, _, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type T = PaneNode<u32>;

    fn leaf(n: u32) -> Box<T> {
        Box::new(PaneNode::Leaf(n))
    }
    fn h(ratio: f32, a: Box<T>, b: Box<T>) -> Box<T> {
        Box::new(PaneNode::Split { dir: SplitDir::Horizontal, ratio, first: a, second: b })
    }
    fn v(ratio: f32, a: Box<T>, b: Box<T>) -> Box<T> {
        Box::new(PaneNode::Split { dir: SplitDir::Vertical, ratio, first: a, second: b })
    }
    fn area(w: usize, hh: usize) -> PaneRect {
        PaneRect { x: 0, y: 0, width: w, height: hh }
    }
    fn rects(t: &T, a: PaneRect) -> Vec<(usize, PaneRect)> {
        let mut out = Vec::new();
        t.layouts(a, &mut 0, &mut out);
        out
    }
    fn ratio_of(t: &T) -> f32 {
        match t {
            PaneNode::Split { ratio, .. } => *ratio,
            _ => panic!("not a split"),
        }
    }
    const MIN: MinSize = MinSize { w: 20, h: 5 };

    /// Left pane full height | right column (top 70% / bottom 30%).
    fn sample() -> Box<T> {
        h(0.5, leaf(0), v(0.7, leaf(1), leaf(2)))
    }

    #[test]
    fn neighbor_picks_adjacent_pane_by_geometry() {
        let r = rects(&sample(), area(101, 50));
        assert_eq!(neighbor_in_direction(&r, 0, Direction::Right), Some(1)); // bigger overlap
        assert_eq!(neighbor_in_direction(&r, 2, Direction::Left), Some(0));
        assert_eq!(neighbor_in_direction(&r, 1, Direction::Down), Some(2));
        assert_eq!(neighbor_in_direction(&r, 2, Direction::Up), Some(1));
        assert_eq!(neighbor_in_direction(&r, 0, Direction::Left), None);
        assert_eq!(neighbor_in_direction(&r, 1, Direction::Up), None);
        assert_eq!(neighbor_in_direction(&r, 1, Direction::Right), None);
    }

    #[test]
    fn neighbor_ignores_diagonal_and_prefers_closest() {
        // 2x2 grid: (0|1) over (2|3)
        let t = v(0.5, h(0.5, leaf(0), leaf(1)), h(0.5, leaf(2), leaf(3)));
        let r = rects(&t, area(101, 51));
        assert_eq!(neighbor_in_direction(&r, 0, Direction::Right), Some(1));
        assert_eq!(neighbor_in_direction(&r, 0, Direction::Down), Some(2));
        assert_eq!(neighbor_in_direction(&r, 3, Direction::Left), Some(2));
        assert_eq!(neighbor_in_direction(&r, 3, Direction::Up), Some(1));
        assert_eq!(neighbor_in_direction(&r, 3, Direction::Right), None);
    }

    #[test]
    fn equalize_weights_same_direction_chains() {
        // ((0|1)|2) -> root 2/3, inner 1/2
        let mut t = h(0.9, h(0.2, leaf(0), leaf(1)), leaf(2));
        t.equalize();
        assert!((ratio_of(&t) - 2.0 / 3.0).abs() < 1e-6);
        let r = rects(&t, area(302, 10));
        let widths: Vec<usize> = r.iter().map(|(_, r)| r.width).collect();
        let (min, max) = (*widths.iter().min().unwrap(), *widths.iter().max().unwrap());
        assert!(max - min <= 2, "widths {widths:?}");
    }

    #[test]
    fn equalize_right_nested_row_of_four() {
        let mut t = h(0.1, leaf(0), h(0.9, leaf(1), h(0.3, leaf(2), leaf(3))));
        t.equalize();
        assert!((ratio_of(&t) - 0.25).abs() < 1e-6);
        let r = rects(&t, area(403, 10));
        let widths: Vec<usize> = r.iter().map(|(_, r)| r.width).collect();
        assert!(widths.iter().max().unwrap() - widths.iter().min().unwrap() <= 2, "{widths:?}");
    }

    #[test]
    fn equalize_mixed_directions_count_as_one() {
        // 0 | (1 over (2|3)): the right side is a single cell along x
        let mut t = h(0.8, leaf(0), v(0.2, leaf(1), h(0.9, leaf(2), leaf(3))));
        t.equalize();
        assert!((ratio_of(&t) - 0.5).abs() < 1e-6);
        if let PaneNode::Split { second, .. } = &*t {
            assert!((ratio_of(second) - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn clamp_ratio_respects_minimums() {
        let lo = clamp_ratio(0.0, 200, 20, 20);
        let hi = clamp_ratio(1.0, 200, 20, 20);
        assert!(lo * 200.0 >= 20.0);
        assert!(200.0 - hi * 200.0 - 1.0 >= 20.0 - 1e-3);
        assert_eq!(clamp_ratio(0.5, 200, 20, 20), 0.5);
        // impossible: falls back to centre
        assert_eq!(clamp_ratio(0.9, 30, 20, 20), 0.5);
    }

    #[test]
    fn drag_clamps_to_min_size() {
        let a = area(201, 40);
        let mut t = h(0.5, leaf(0), leaf(1));
        assert!(t.set_ratio(a, 0, &mut 0, 2, 0, MIN));
        let r = rects(&t, a);
        assert!(r[0].1.width >= MIN.w, "left {}", r[0].1.width);
        assert!(t.set_ratio(a, 0, &mut 0, 199, 0, MIN));
        let r = rects(&t, a);
        assert!(r[1].1.width >= MIN.w, "right {}", r[1].1.width);
    }

    #[test]
    fn drag_clamps_with_nested_subtree_minimums() {
        // Right side holds two panes side by side: needs 20+1+20 px.
        let a = area(201, 40);
        let mut t = h(0.5, leaf(0), h(0.5, leaf(1), leaf(2)));
        assert_eq!(t.min_extent(SplitDir::Horizontal, MIN), 20 + 1 + 20 + 1 + 20);
        t.set_ratio(a, 0, &mut 0, 190, 0, MIN);
        let r = rects(&t, a);
        assert!(r[1].1.width >= MIN.w && r[2].1.width >= MIN.w, "{r:?}");
    }

    #[test]
    fn keyboard_resize_moves_nearest_matching_divider() {
        let a = area(201, 101);
        // active = 2 (bottom right): Down/Up move the inner V divider.
        let mut t = sample();
        let before_v = match &*t { PaneNode::Split { second, .. } => ratio_of(second), _ => unreachable!() };
        assert!(t.nudge_ancestor(a, 2, 0, SplitDir::Vertical, -RESIZE_STEP, MIN));
        let after_v = match &*t { PaneNode::Split { second, .. } => ratio_of(second), _ => unreachable!() };
        assert!(after_v < before_v);
        assert!((ratio_of(&t) - 0.5).abs() < 1e-6);
        // Horizontal: skips the V split and reaches the root.
        assert!(t.nudge_ancestor(a, 2, 0, SplitDir::Horizontal, RESIZE_STEP, MIN));
        assert!((ratio_of(&t) - 0.55).abs() < 1e-5);
        // Pane 0 has no vertical ancestor.
        assert!(!t.nudge_ancestor(a, 0, 0, SplitDir::Vertical, RESIZE_STEP, MIN));
    }

    #[test]
    fn keyboard_resize_stops_at_minimum() {
        let a = area(101, 40);
        let mut t = h(0.5, leaf(0), leaf(1));
        for _ in 0..40 {
            t.nudge_ancestor(a, 0, 0, SplitDir::Horizontal, -RESIZE_STEP, MIN);
        }
        let r = rects(&t, a);
        assert!(r[0].1.width >= MIN.w, "{r:?}");
    }

    #[test]
    fn border_hit_indexing_is_preorder() {
        let a = area(101, 51);
        let t = h(0.5, v(0.5, leaf(0), leaf(1)), leaf(2));
        let mut b = Vec::new();
        t.borders(a, &mut b);
        assert_eq!(b.len(), 2);
        // root divider at x=50 spans the full height
        assert_eq!(border_hit(&b, 50, 10, 2), Some((0, SplitDir::Horizontal)));
        assert_eq!(border_hit(&b, 52, 40, 2), Some((0, SplitDir::Horizontal)));
        // inner divider at y=25, only across the left half
        assert_eq!(border_hit(&b, 10, 25, 2), Some((1, SplitDir::Vertical)));
        assert_eq!(border_hit(&b, 80, 25, 1), None);
        assert_eq!(border_hit(&b, 20, 10, 1), None);
    }

    #[test]
    fn equalize_single_border_sets_half() {
        let mut t = h(0.2, v(0.8, leaf(0), leaf(1)), leaf(2));
        assert!(t.set_ratio_at(1, &mut 0, 0.5));
        if let PaneNode::Split { first, .. } = &*t {
            assert_eq!(ratio_of(first), 0.5);
        }
        assert_eq!(ratio_of(&t), 0.2);
        assert!(!t.set_ratio_at(5, &mut 0, 0.5));
    }

    #[test]
    fn close_focuses_the_pane_that_absorbs_space() {
        let a = area(101, 50);
        // close left pane: right column's top (bigger shared edge) takes over
        let mut t = sample();
        assert_eq!(t.close_with_focus(a, 0), 0);
        assert_eq!(t.count(), 2);
        // close bottom-right: sibling (top-right) absorbs; now index 1
        let mut t = sample();
        assert_eq!(t.close_with_focus(a, 2), 1);
        // close top-right: bottom-right absorbs and moves to index 1
        let mut t = sample();
        assert_eq!(t.close_with_focus(a, 1), 1);
        // close right pane of (0|1): left absorbs
        let mut t = h(0.5, leaf(0), leaf(1));
        assert_eq!(t.close_with_focus(a, 1), 0);
        let mut t = h(0.5, leaf(0), leaf(1));
        assert_eq!(t.close_with_focus(a, 0), 0);
    }

    #[test]
    fn close_prefers_longest_shared_edge_in_nested_sibling() {
        let a = area(101, 50);
        // (0 | 1) over 2 ... close 2: sibling subtree is the whole top row.
        let mut t = v(0.5, h(0.8, leaf(0), leaf(1)), leaf(2));
        // leaf 0 is much wider -> longest shared edge with the bottom pane
        assert_eq!(t.close_with_focus(a, 2), 0);
    }

    #[test]
    fn sibling_range_covers_sibling_leaves() {
        let t = sample();
        assert_eq!(t.sibling_range(0, 0), Some(1..3));
        assert_eq!(t.sibling_range(1, 0), Some(2..3));
        assert_eq!(t.sibling_range(2, 0), Some(1..2));
    }

    #[test]
    fn swap_exchanges_leaf_values() {
        let mut t = sample();
        t.swap_leaves(0, 2);
        let mut v = Vec::new();
        t.collect(&mut v);
        assert_eq!(v.into_iter().copied().collect::<Vec<_>>(), vec![2, 1, 0]);
        t.swap_leaves(1, 1);
        t.swap_leaves(0, 9);
    }

    #[test]
    fn can_split_enforces_min_size() {
        // 41 cols wide: halves are 20 and 20 -> ok; 40 wide: 20 and 19 -> refuse
        assert!(can_split_rect(area(41, 40), SplitDir::Horizontal, MIN));
        assert!(!can_split_rect(area(40, 40), SplitDir::Horizontal, MIN));
        assert!(can_split_rect(area(100, 11), SplitDir::Vertical, MIN));
        assert!(!can_split_rect(area(100, 10), SplitDir::Vertical, MIN));
    }

    #[test]
    fn map_leaves_preserves_shape() {
        let t = sample();
        let m = t.map_leaves(&mut |n| *n * 10);
        let mut out = Vec::new();
        m.collect(&mut out);
        assert_eq!(out.into_iter().copied().collect::<Vec<_>>(), vec![0u32, 10, 20]);
        assert_eq!(ratio_of(&m), 0.5);
    }
}
