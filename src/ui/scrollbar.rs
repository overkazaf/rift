//! Overlay scrollbar: pure geometry, auto-hide state machine and drawing.
//!
//! The bar lives on the right edge of every pane. Geometry is expressed in
//! "track" coordinates (0 = top of the pane) so it can be unit-tested without
//! any window. The thumb maps `scroll_offset` (0 = bottom, `scrollback_len` =
//! top) onto the track.

use std::time::{Duration, Instant};

use crate::config::Rgb;
use crate::window::PaneRect;

/// Idle time after the last scroll before the bar starts fading out.
pub const HIDE_AFTER: Duration = Duration::from_millis(1200);
/// Length of the fade-out.
pub const FADE: Duration = Duration::from_millis(300);
/// Smallest thumb height in pixels.
pub const MIN_THUMB: usize = 24;
/// Frame pacing while the fade animates.
const ANIM_FRAME: Duration = Duration::from_millis(16);

/// Thumb placement inside a track (pixels from the track top).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thumb {
    pub y: usize,
    pub h: usize,
}

/// Thumb for a track of `track_h` px showing `rows` of `rows + sb_len` lines
/// scrolled back by `offset`. `None` when there is nothing to scroll.
pub fn thumb(track_h: usize, rows: usize, sb_len: usize, offset: usize) -> Option<Thumb> {
    if track_h == 0 || sb_len == 0 || rows == 0 {
        return None;
    }
    let total = sb_len + rows;
    let h = (track_h * rows / total).max(MIN_THUMB.min(track_h)).min(track_h);
    let range = track_h - h;
    let offset = offset.min(sb_len);
    // offset == sb_len -> top (y = 0); offset == 0 -> bottom (y = range).
    let y = range * (sb_len - offset) / sb_len;
    Some(Thumb { y, h })
}

/// Inverse of [`thumb`]: scroll offset that puts the thumb top at `top`.
pub fn offset_for_thumb_top(track_h: usize, rows: usize, sb_len: usize, top: usize) -> usize {
    let Some(t) = thumb(track_h, rows, sb_len, 0) else { return 0 };
    let range = track_h - t.h;
    if range == 0 {
        return 0;
    }
    let top = top.min(range);
    // Round to nearest line.
    let from_top = (top * sb_len + range / 2) / range;
    sb_len - from_top.min(sb_len)
}

/// What a press inside the track hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackHit {
    /// On the thumb; `grab` is the pixel offset inside the thumb.
    Thumb { grab: usize },
    /// Above the thumb: page towards older output.
    PageUp,
    /// Below the thumb: page towards the newest output.
    PageDown,
}

pub fn hit_track(track_h: usize, rows: usize, sb_len: usize, offset: usize, y: usize) -> Option<TrackHit> {
    let t = thumb(track_h, rows, sb_len, offset)?;
    if y < t.y {
        Some(TrackHit::PageUp)
    } else if y >= t.y + t.h {
        Some(TrackHit::PageDown)
    } else {
        Some(TrackHit::Thumb { grab: y - t.y })
    }
}

/// Pixel rect of the hit zone (wider than the drawn bar) for a pane.
pub fn hit_rect(pane: PaneRect, cell_w: usize) -> PaneRect {
    let w = (cell_w + 4).clamp(10, 24).min(pane.width);
    PaneRect { x: pane.x + pane.width - w, y: pane.y, width: w, height: pane.height }
}

/// Width of the drawn bar.
pub fn bar_width(cell_w: usize, emphasized: bool) -> usize {
    let base = (cell_w / 2).clamp(4, 8);
    if emphasized { base + 2 } else { base }
}

pub fn contains(r: PaneRect, x: usize, y: usize) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

/// Active thumb drag.
#[derive(Clone, Copy, Debug)]
pub struct Drag {
    pub pane: usize,
    pub grab: usize,
}

/// What the periodic tick wants from the event loop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tick {
    pub redraw: bool,
    pub next: Option<Instant>,
}

/// Auto-hide / hover / drag state shared by all panes.
#[derive(Default)]
pub struct ScrollbarState {
    /// Pane that scrolled last and when.
    activity: Option<(usize, Instant)>,
    /// Pane whose right edge the mouse is over.
    pub hover: Option<usize>,
    pub drag: Option<Drag>,
    /// Last observed (pane, offset) so scrolling by any means shows the bar.
    seen: Option<(usize, usize)>,
    /// The final "fully hidden" frame has been requested.
    settled: bool,
}

impl ScrollbarState {
    pub fn note_activity(&mut self, pane: usize, now: Instant) {
        self.activity = Some((pane, now));
        self.settled = false;
    }

    /// Call once per frame with the active pane's offset; any change counts as
    /// scrolling activity (keyboard paging, search jumps, ...).
    pub fn observe(&mut self, pane: usize, offset: usize, now: Instant) -> bool {
        let mut noted = false;
        if self.seen != Some((pane, offset)) {
            if matches!(self.seen, Some((p, _)) if p == pane) {
                self.note_activity(pane, now);
                noted = true;
            }
            self.seen = Some((pane, offset));
        }
        noted
    }

    /// Forget all activity (e.g. after switching tabs).
    pub fn reset(&mut self) {
        self.activity = None;
        self.seen = None;
        self.hover = None;
        self.settled = true;
    }

    /// The bar to draw this frame, if any.
    pub fn bar(&self, now: Instant) -> Option<BarDraw> {
        let pane = self
            .drag
            .map(|d| d.pane)
            .or(self.hover)
            .or(self.activity.map(|(p, _)| p))?;
        let alpha = self.alpha(pane, now);
        (alpha > 0).then(|| BarDraw { pane, alpha, emphasized: self.emphasized(pane) })
    }

    /// Bar opacity (0..=255) for `pane` at `now`.
    pub fn alpha(&self, pane: usize, now: Instant) -> u8 {
        if self.drag.map_or(false, |d| d.pane == pane) || self.hover == Some(pane) {
            return 255;
        }
        match self.activity {
            Some((p, t)) if p == pane => fade_alpha(now.saturating_duration_since(t)),
            _ => 0,
        }
    }

    /// Whether the bar should be drawn emphasized (wider) for `pane`.
    pub fn emphasized(&self, pane: usize) -> bool {
        self.drag.map_or(false, |d| d.pane == pane) || self.hover == Some(pane)
    }

    /// Advance timers; says whether to redraw now and when to wake next.
    pub fn tick(&mut self, now: Instant) -> Tick {
        if self.drag.is_some() || self.hover.is_some() {
            return Tick::default();
        }
        let Some((_, t)) = self.activity else { return Tick::default() };
        let el = now.saturating_duration_since(t);
        if el < HIDE_AFTER {
            Tick { redraw: false, next: Some(t + HIDE_AFTER) }
        } else if el < HIDE_AFTER + FADE {
            Tick { redraw: true, next: Some(now + ANIM_FRAME) }
        } else if !self.settled {
            self.settled = true;
            Tick { redraw: true, next: None }
        } else {
            Tick::default()
        }
    }
}

fn fade_alpha(idle: Duration) -> u8 {
    if idle < HIDE_AFTER {
        255
    } else if idle < HIDE_AFTER + FADE {
        let left = (HIDE_AFTER + FADE - idle).as_millis() as f32 / FADE.as_millis() as f32;
        (left * 255.0) as u8
    } else {
        0
    }
}

/// Per-frame description handed to the renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BarDraw {
    pub pane: usize,
    pub alpha: u8,
    pub emphasized: bool,
}

#[inline]
fn blend_px(base: u32, c: Rgb, a: u32) -> u32 {
    let inv = 255 - a;
    let r = (((base >> 16) & 0xff) * inv + c.0 as u32 * a) / 255;
    let g = (((base >> 8) & 0xff) * inv + c.1 as u32 * a) / 255;
    let b = ((base & 0xff) * inv + c.2 as u32 * a) / 255;
    (r << 16) | (g << 8) | b
}

/// Draw the bar for `pane` (rect) onto `buffer`. `a` is the overall opacity.
pub fn draw(
    buffer: &mut [u32],
    buf_w: usize,
    pane: PaneRect,
    cell_w: usize,
    th: Thumb,
    a: u8,
    emphasized: bool,
    color: Rgb,
) {
    if a == 0 || pane.width < 12 {
        return;
    }
    let bw = bar_width(cell_w, emphasized);
    let x0 = pane.x + pane.width - bw - 2;
    let rows_end = pane.y + pane.height;

    // Faint track while emphasized.
    if emphasized {
        let ta = (a as u32 * 28) / 255;
        for y in pane.y..rows_end {
            for x in x0..x0 + bw {
                let i = y * buf_w + x;
                if i < buffer.len() {
                    buffer[i] = blend_px(buffer[i], color, ta);
                }
            }
        }
    }

    let alpha = (a as u32 * if emphasized { 190 } else { 130 }) / 255;
    let r = (bw / 2) as i32;
    for dy in 0..th.h {
        // Rounded caps: inset the first/last rows.
        let from_edge = dy.min(th.h - 1 - dy) as i32;
        let inset = if from_edge < r { ((r - from_edge) as usize + 1) / 2 } else { 0 };
        let y = pane.y + th.y + dy;
        for x in (x0 + inset)..(x0 + bw).saturating_sub(inset) {
            let i = y * buf_w + x;
            if i < buffer.len() {
                buffer[i] = blend_px(buffer[i], color, alpha);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_thumb_without_scrollback() {
        assert_eq!(thumb(400, 24, 0, 0), None);
        assert_eq!(thumb(0, 24, 100, 0), None);
    }

    #[test]
    fn thumb_at_bottom_and_top() {
        // 24 rows visible of 124 total over a 600px track.
        let bottom = thumb(600, 24, 100, 0).unwrap();
        assert_eq!(bottom.h, 600 * 24 / 124);
        assert_eq!(bottom.y + bottom.h, 600);
        let top = thumb(600, 24, 100, 100).unwrap();
        assert_eq!(top.y, 0);
        assert_eq!(top.h, bottom.h);
    }

    #[test]
    fn thumb_has_minimum_size_and_stays_in_track() {
        let t = thumb(300, 24, 100_000, 50_000).unwrap();
        assert_eq!(t.h, MIN_THUMB);
        assert!(t.y + t.h <= 300);
        // Offset beyond scrollback is clamped.
        let t = thumb(300, 24, 100, 9999).unwrap();
        assert_eq!(t.y, 0);
        // Tiny track never exceeds itself.
        let t = thumb(10, 24, 100, 0).unwrap();
        assert_eq!((t.y, t.h), (0, 10));
    }

    #[test]
    fn thumb_moves_monotonically() {
        let mut last = usize::MAX;
        for off in 0..=100 {
            let t = thumb(500, 30, 100, off).unwrap();
            assert!(t.y <= last);
            last = t.y;
        }
    }

    #[test]
    fn offset_roundtrip() {
        for off in [0usize, 1, 17, 50, 99, 100] {
            let t = thumb(600, 24, 100, off).unwrap();
            let back = offset_for_thumb_top(600, 24, 100, t.y);
            assert!((back as i64 - off as i64).abs() <= 1, "{off} -> {back}");
        }
        assert_eq!(offset_for_thumb_top(600, 24, 100, 0), 100);
        assert_eq!(offset_for_thumb_top(600, 24, 100, 10_000), 0);
        assert_eq!(offset_for_thumb_top(600, 24, 0, 10), 0);
    }

    #[test]
    fn track_hits() {
        // Thumb at the bottom: everything above is "page up".
        let t = thumb(600, 24, 100, 0).unwrap();
        assert_eq!(hit_track(600, 24, 100, 0, 5), Some(TrackHit::PageUp));
        assert_eq!(hit_track(600, 24, 100, 0, t.y + 3), Some(TrackHit::Thumb { grab: 3 }));
        // Thumb at the top: everything below is "page down".
        assert_eq!(hit_track(600, 24, 100, 100, 599), Some(TrackHit::PageDown));
        assert_eq!(hit_track(600, 24, 0, 0, 5), None);
    }

    #[test]
    fn hit_zone_is_on_the_right_edge() {
        let pane = PaneRect { x: 100, y: 30, width: 500, height: 300 };
        let z = hit_rect(pane, 9);
        assert_eq!(z.x + z.width, 600);
        assert_eq!((z.y, z.height), (30, 300));
        assert!(contains(z, 599, 30));
        assert!(!contains(z, 100, 30));
    }

    #[test]
    fn autohide_timeline() {
        let t0 = Instant::now();
        let mut s = ScrollbarState::default();
        assert_eq!(s.alpha(0, t0), 0);
        s.note_activity(0, t0);
        assert_eq!(s.alpha(0, t0 + Duration::from_millis(1000)), 255);
        let mid = s.alpha(0, t0 + HIDE_AFTER + FADE / 2);
        assert!(mid > 0 && mid < 255);
        assert_eq!(s.alpha(0, t0 + HIDE_AFTER + FADE), 0);
        // Another pane never shows.
        assert_eq!(s.alpha(1, t0), 0);
        // Hover pins it.
        s.hover = Some(0);
        assert_eq!(s.alpha(0, t0 + Duration::from_secs(10)), 255);
    }

    #[test]
    fn tick_schedules_then_settles() {
        let t0 = Instant::now();
        let mut s = ScrollbarState::default();
        assert_eq!(s.tick(t0), Tick::default());
        s.note_activity(0, t0);
        let k = s.tick(t0 + Duration::from_millis(100));
        assert!(!k.redraw);
        assert_eq!(k.next, Some(t0 + HIDE_AFTER));
        let k = s.tick(t0 + HIDE_AFTER + Duration::from_millis(10));
        assert!(k.redraw && k.next.is_some());
        let k = s.tick(t0 + HIDE_AFTER + FADE + Duration::from_millis(1));
        assert!(k.redraw && k.next.is_none());
        assert_eq!(s.tick(t0 + Duration::from_secs(9)), Tick::default());
    }

    #[test]
    fn observe_detects_scrolling_but_not_pane_switch() {
        let t0 = Instant::now();
        let mut s = ScrollbarState::default();
        s.observe(0, 0, t0);
        assert_eq!(s.alpha(0, t0), 0);
        s.observe(0, 5, t0);
        assert_eq!(s.alpha(0, t0), 255);
        let mut s = ScrollbarState::default();
        s.observe(0, 3, t0);
        s.observe(1, 0, t0);
        assert_eq!(s.alpha(1, t0), 0);
    }

    #[test]
    fn draw_stays_inside_buffer() {
        let (w, h) = (80usize, 60usize);
        let mut buf = vec![0u32; w * h];
        let pane = PaneRect { x: 0, y: 0, width: w, height: h };
        let th = thumb(h, 10, 40, 0).unwrap();
        draw(&mut buf, w, pane, 8, th, 255, true, (255, 255, 255));
        assert!(buf.iter().any(|&p| p != 0));
        // Nothing drawn left of the bar.
        for y in 0..h {
            assert_eq!(buf[y * w], 0);
        }
        let mut blank = vec![0u32; w * h];
        draw(&mut blank, w, pane, 8, th, 0, false, (255, 255, 255));
        assert!(blank.iter().all(|&p| p == 0));
    }
}
