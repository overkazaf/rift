use super::pane::Pane;

/// Direction of a split: `Horizontal` puts children side by side (left | right),
/// `Vertical` stacks them (top / bottom).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SplitDir {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug)]
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
}

const BORDER: usize = 1;
const MIN_RATIO: f32 = 0.1;
const MAX_RATIO: f32 = 0.9;

/// Recursive split tree. Leaves hold panes; splits hold two children and a
/// draggable ratio. Pane indices are assigned by in-order (left-to-right,
/// top-to-bottom) traversal.
pub enum PaneNode {
    Leaf(Pane),
    Split {
        dir: SplitDir,
        ratio: f32,
        first: Box<PaneNode>,
        second: Box<PaneNode>,
    },
    /// Transient placeholder used only while restructuring the tree.
    Empty,
}

/// Divide `area` into the two child rects of a split.
fn split_rects(area: PaneRect, dir: SplitDir, ratio: f32) -> (PaneRect, PaneRect) {
    match dir {
        SplitDir::Horizontal => {
            let w1 = ((area.width as f32) * ratio) as usize;
            let w2 = area.width.saturating_sub(w1 + BORDER);
            (
                PaneRect { x: area.x, y: area.y, width: w1, height: area.height },
                PaneRect { x: area.x + w1 + BORDER, y: area.y, width: w2, height: area.height },
            )
        }
        SplitDir::Vertical => {
            let h1 = ((area.height as f32) * ratio) as usize;
            let h2 = area.height.saturating_sub(h1 + BORDER);
            (
                PaneRect { x: area.x, y: area.y, width: area.width, height: h1 },
                PaneRect { x: area.x, y: area.y + h1 + BORDER, width: area.width, height: h2 },
            )
        }
    }
}

impl PaneNode {
    fn count(&self) -> usize {
        match self {
            PaneNode::Leaf(_) => 1,
            PaneNode::Split { first, second, .. } => first.count() + second.count(),
            PaneNode::Empty => 0,
        }
    }

    fn collect<'a>(&'a self, out: &mut Vec<&'a Pane>) {
        match self {
            PaneNode::Leaf(p) => out.push(p),
            PaneNode::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
            PaneNode::Empty => {}
        }
    }

    fn collect_mut<'a>(&'a mut self, out: &mut Vec<&'a mut Pane>) {
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
    fn split_leaf(&mut self, target: usize, idx: &mut usize, dir: SplitDir, new_pane: &mut Option<Pane>) -> bool {
        match self {
            PaneNode::Leaf(_) => {
                if *idx != target {
                    *idx += 1;
                    return false;
                }
                let old = std::mem::replace(self, PaneNode::Empty);
                let fresh = new_pane.take().expect("new pane consumed once");
                *self = PaneNode::Split {
                    dir,
                    ratio: 0.5,
                    first: Box::new(old),
                    second: Box::new(PaneNode::Leaf(fresh)),
                };
                true
            }
            PaneNode::Split { first, second, .. } => {
                first.split_leaf(target, idx, dir, new_pane) || second.split_leaf(target, idx, dir, new_pane)
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

    /// Set the ratio of the split identified by its border index (pre-order).
    fn set_ratio(&mut self, area: PaneRect, target: usize, idx: &mut usize, mx: usize, my: usize) -> bool {
        let PaneNode::Split { dir, ratio, first, second } = self else {
            return false;
        };
        if *idx == target {
            let r = match dir {
                SplitDir::Horizontal => (mx.saturating_sub(area.x)) as f32 / area.width.max(1) as f32,
                SplitDir::Vertical => (my.saturating_sub(area.y)) as f32 / area.height.max(1) as f32,
            };
            *ratio = r.clamp(MIN_RATIO, MAX_RATIO);
            return true;
        }
        *idx += 1;
        let (a, b) = split_rects(area, *dir, *ratio);
        first.set_ratio(a, target, idx, mx, my) || second.set_ratio(b, target, idx, mx, my)
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

pub struct Tab {
    pub title: String,
    pub custom_title: bool,
    pub root: PaneNode,
    pub active: usize,
}

impl Tab {
    pub fn new(pane: Pane) -> Self {
        Self {
            title: "Tab 1".to_string(),
            custom_title: false,
            root: PaneNode::Leaf(pane),
            active: 0,
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

    pub fn layouts(&self, area: PaneRect) -> Vec<(usize, PaneRect, bool)> {
        let mut out = Vec::new();
        self.root.layouts(area, &mut 0, &mut out);
        out.into_iter().map(|(i, r)| (i, r, i == self.active)).collect()
    }

    /// Split the active pane side by side (new pane on the right).
    pub fn split_h(&mut self, new_pane: Pane) {
        self.split_active(SplitDir::Horizontal, new_pane);
    }

    /// Split the active pane top/bottom (new pane below).
    pub fn split_v(&mut self, new_pane: Pane) {
        self.split_active(SplitDir::Vertical, new_pane);
    }

    fn split_active(&mut self, dir: SplitDir, new_pane: Pane) {
        let target = self.active;
        let mut slot = Some(new_pane);
        if self.root.split_leaf(target, &mut 0, dir, &mut slot) {
            // The new leaf sits right after the split one in in-order traversal.
            self.active = target + 1;
        }
    }

    /// Close the active pane. Returns true when it was the last pane (close tab).
    pub fn close_pane(&mut self) -> bool {
        if self.pane_count() <= 1 {
            return true;
        }
        self.root.remove_leaf(self.active, &mut 0);
        self.active = self.active.min(self.pane_count().saturating_sub(1));
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
            self.active = (self.active + 1) % n;
        }
    }

    pub fn focus_prev(&mut self) {
        let n = self.pane_count();
        if n > 0 {
            self.active = self.active.checked_sub(1).unwrap_or(n - 1);
        }
    }

    pub fn display_title(&self) -> &str {
        &self.title
    }

    /// Index of the divider under (x, y), if any.
    pub fn border_at(&self, area: PaneRect, x: usize, y: usize, tolerance: usize) -> Option<(usize, SplitDir)> {
        let mut borders = Vec::new();
        self.root.borders(area, &mut borders);
        borders
            .iter()
            .enumerate()
            .find(|(_, b)| b.hit(x, y, tolerance))
            .map(|(i, b)| (i, b.dir))
    }

    /// Move divider `border_idx` so it sits under the mouse.
    pub fn drag_border(&mut self, area: PaneRect, border_idx: usize, x: usize, y: usize) -> bool {
        self.root.set_ratio(area, border_idx, &mut 0, x, y)
    }

    /// Index of the pane under (x, y), if any.
    pub fn pane_at(&self, area: PaneRect, x: usize, y: usize) -> Option<usize> {
        self.layouts(area).into_iter().find(|(_, r, _)| r.contains(x, y)).map(|(i, _, _)| i)
    }
}
