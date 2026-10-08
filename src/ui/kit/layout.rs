//! Pure layout helpers (no drawing).

use super::Ctx;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Rect {
    pub const fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> usize { self.x + self.w }
    pub fn bottom(&self) -> usize { self.y + self.h }
    /// Shrink by `dx` horizontally and `dy` vertically on every side.
    pub fn inset(&self, dx: usize, dy: usize) -> Rect {
        let w = self.w.saturating_sub(2 * dx);
        let h = self.h.saturating_sub(2 * dy);
        Rect::new(self.x + dx.min(self.w / 2), self.y + dy.min(self.h / 2), w, h)
    }
    /// Take `h` pixels off the top; returns (top, rest).
    pub fn split_top(&self, h: usize) -> (Rect, Rect) {
        let h = h.min(self.h);
        (Rect::new(self.x, self.y, self.w, h), Rect::new(self.x, self.y + h, self.w, self.h - h))
    }
    /// Take `h` pixels off the bottom; returns (rest, bottom).
    pub fn split_bottom(&self, h: usize) -> (Rect, Rect) {
        let h = h.min(self.h);
        (Rect::new(self.x, self.y, self.w, self.h - h), Rect::new(self.x, self.y + self.h - h, self.w, h))
    }
    pub fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

/// Centered rectangle: `width_pct`/`height_pct` of the screen (0..=100),
/// capped at `max_w` pixels wide and always at least `margin` from the edges.
pub fn centered(sw: usize, sh: usize, width_pct: usize, max_w: usize, height_pct: usize, margin: usize) -> Rect {
    let w = (sw * width_pct.min(100) / 100).min(max_w);
    let h = sh * height_pct.min(100) / 100;
    centered_px(sw, sh, w, h, margin)
}

/// Centered rectangle of an explicit size, clamped to the screen minus `margin`.
pub fn centered_px(sw: usize, sh: usize, w: usize, h: usize, margin: usize) -> Rect {
    let w = w.min(sw.saturating_sub(2 * margin)).max(1.min(sw));
    let h = h.min(sh.saturating_sub(2 * margin)).max(1.min(sh));
    Rect::new((sw - w) / 2, (sh - h) / 2, w, h)
}

/// Full-width sheet anchored to the bottom edge.
pub fn bottom_sheet(sw: usize, sh: usize, h: usize) -> Rect {
    let h = h.min(sh);
    Rect::new(0, sh - h, sw, h)
}

/// Full-height panel docked to the left or right edge.
pub fn side_panel(sw: usize, sh: usize, side: Side, w: usize) -> Rect {
    let w = w.min(sw);
    match side {
        Side::Left => Rect::new(0, 0, w, sh),
        Side::Right => Rect::new(sw - w, 0, w, sh),
    }
}

impl<'a> Ctx<'a> {
    /// Centered rect; margin is the `xl` spacing token.
    pub fn centered(&self, width_pct: usize, max_w: usize, height_pct: usize) -> Rect {
        centered(self.w, self.h, width_pct, max_w, height_pct, self.tk.sp.xl)
    }
    /// Centered rect with a content-driven pixel size.
    pub fn centered_px(&self, w: usize, h: usize) -> Rect {
        centered_px(self.w, self.h, w, h, self.tk.sp.xl)
    }
    /// Centered rect `cols` characters wide and `h` pixels tall.
    pub fn centered_cols(&self, cols: usize, h: usize) -> Rect {
        self.centered_px(cols * self.tk.cw + 2 * self.tk.sp.lg, h)
    }
    pub fn bottom_sheet(&self, h: usize) -> Rect {
        bottom_sheet(self.w, self.h, h)
    }
    pub fn side_panel(&self, side: Side, w: usize) -> Rect {
        side_panel(self.w, self.h, side, w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_respects_pct_max_and_margin() {
        let r = centered(1000, 800, 60, 500, 50, 24);
        assert_eq!((r.w, r.h), (500, 400));
        assert_eq!((r.x, r.y), (250, 200));
        let r = centered(1000, 800, 100, 5000, 100, 24);
        assert_eq!((r.x, r.y, r.w, r.h), (24, 24, 952, 752));
    }

    #[test]
    fn centered_px_clamps_to_tiny_screens() {
        let r = centered_px(100, 60, 400, 400, 8);
        assert!(r.right() <= 100 && r.bottom() <= 60);
        assert_eq!((r.x, r.y), (8, 8));
    }

    #[test]
    fn sheets_and_side_panels_hug_edges() {
        let b = bottom_sheet(800, 600, 200);
        assert_eq!((b.x, b.y, b.w, b.h), (0, 400, 800, 200));
        let l = side_panel(800, 600, Side::Left, 300);
        let r = side_panel(800, 600, Side::Right, 300);
        assert_eq!((l.x, l.w, l.h), (0, 300, 600));
        assert_eq!((r.x, r.right()), (500, 800));
        assert_eq!(bottom_sheet(800, 600, 9999).h, 600);
    }

    #[test]
    fn rect_helpers() {
        let r = Rect::new(10, 10, 100, 50);
        let i = r.inset(8, 4);
        assert_eq!((i.x, i.y, i.w, i.h), (18, 14, 84, 42));
        let (t, rest) = r.split_top(20);
        assert_eq!((t.h, rest.y, rest.h), (20, 30, 30));
        let (rest, b) = r.split_bottom(20);
        assert_eq!((rest.h, b.y, b.h), (30, 40, 20));
        assert!(r.contains(10, 10) && !r.contains(110, 10));
        assert_eq!(r.inset(1000, 1000).w, 0);
    }
}
