//! Pure view-model helpers for command blocks: folding (collapsed output),
//! mouse-y -> row -> block mapping, row spans, text formatting.
//!
//! Everything here is free of rendering/IO so it can be unit tested.
//!
//! # View model
//!
//! A pane normally shows `rows` consecutive absolute lines ending at
//! `total - scroll_offset` (absolute line = index into scrollback ++ grid).
//! With collapsed blocks the view is built *backwards from the same end
//! line*: hidden lines are skipped and a collapsed block's output becomes a
//! single [`ViewRow::Folded`] placeholder row; extra older lines are pulled in
//! at the top to fill the pane. The bottom stays anchored, so the live cursor
//! row does not move.

use crate::terminal::{Attrs, Cell, Color, Terminal};
use crate::tools::blocks::BlockManager;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewRow {
    /// A real terminal line (absolute index).
    Line(usize),
    /// Placeholder for the hidden output lines `start..start + hidden`.
    Folded { block: usize, start: usize, hidden: usize },
}

impl ViewRow {
    /// First absolute line this row stands for.
    pub fn abs(&self) -> usize {
        match *self {
            ViewRow::Line(l) => l,
            ViewRow::Folded { start, .. } => start,
        }
    }
}

/// A collapsed block's hidden range (absolute lines, inclusive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fold {
    pub block: usize,
    pub start: usize,
    pub end: usize,
}

/// Hidden ranges of all collapsed, finished blocks, sorted and disjoint.
pub fn folds_of(mgr: &BlockManager) -> Vec<Fold> {
    let mut out: Vec<Fold> = Vec::new();
    for (i, b) in mgr.blocks().iter().enumerate() {
        if !b.collapsed || b.running || b.output_end < b.output_start {
            continue;
        }
        let f = Fold { block: i, start: b.output_start, end: b.output_end };
        match out.last() {
            Some(prev) if f.start <= prev.end => continue,
            _ => out.push(f),
        }
    }
    out
}

/// Build the rows shown for a pane. See the module docs.
pub fn build_view(total: usize, rows: usize, scroll_offset: usize, folds: &[Fold]) -> Vec<ViewRow> {
    let mut l = total.saturating_sub(scroll_offset); // exclusive upper bound
    let mut fi = folds.partition_point(|f| f.start < l);
    let mut out: Vec<ViewRow> = Vec::with_capacity(rows);
    while out.len() < rows && l > 0 {
        let line = l - 1;
        while fi > 0 && folds[fi - 1].start > line {
            fi -= 1;
        }
        match fi.checked_sub(1).map(|i| folds[i]) {
            Some(f) if f.end >= line => {
                out.push(ViewRow::Folded { block: f.block, start: f.start, hidden: f.end - f.start + 1 });
                l = f.start;
                fi -= 1;
            }
            _ => {
                out.push(ViewRow::Line(line));
                l = line;
            }
        }
    }
    out.reverse();
    out
}

/// `scroll_offset` that makes absolute line `target` the first row of the
/// view (clamped: near the bottom the view cannot start that late).
pub fn scroll_offset_for_top(total: usize, rows: usize, folds: &[Fold], target: usize) -> usize {
    // Walk forward `rows` view rows from `target`; the end of that walk is the
    // view's end line. Folds are atomic in both directions, so building the
    // view backwards from there yields exactly this window.
    let mut l = target;
    let mut fi = folds.partition_point(|f| f.end < l);
    let mut n = 0;
    while n < rows && l < total {
        match folds.get(fi) {
            Some(f) if f.start <= l && l <= f.end => {
                l = f.end + 1;
                fi += 1;
            }
            _ => l += 1,
        }
        n += 1;
    }
    total.saturating_sub(l.min(total))
}

/// Consecutive view rows that belong to one block (inclusive row range).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub block: usize,
    pub first: usize,
    pub last: usize,
}

/// Group view rows by block. `lookup` maps an absolute line to a block index.
pub fn spans_of(view: &[ViewRow], lookup: &dyn Fn(usize) -> Option<usize>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for (row, vr) in view.iter().enumerate() {
        let b = match *vr {
            ViewRow::Folded { block, .. } => Some(block),
            ViewRow::Line(l) => lookup(l),
        };
        match (b, out.last_mut()) {
            (Some(b), Some(s)) if s.block == b && s.last + 1 == row => s.last = row,
            (Some(b), _) => out.push(Span { block: b, first: row, last: row }),
            (None, _) => {}
        }
    }
    out
}

/// Pane-relative pixel y -> view row.
pub fn row_at_y(y: usize, top: usize, cell_h: usize, nrows: usize) -> Option<usize> {
    if y < top || cell_h == 0 {
        return None;
    }
    let row = (y - top) / cell_h;
    (row < nrows).then_some(row)
}

pub fn span_at_row(spans: &[Span], row: usize) -> Option<&Span> {
    spans.iter().find(|s| row >= s.first && row <= s.last)
}

/// Mouse y -> the span under it (convenience over `row_at_y` + `span_at_row`).
pub fn span_at_y(spans: &[Span], y: usize, top: usize, cell_h: usize, nrows: usize) -> Option<(usize, Span)> {
    let row = row_at_y(y, top, cell_h, nrows)?;
    span_at_row(spans, row).map(|s| (row, *s))
}

/// Index of the view row showing absolute `line` (folded rows match any of
/// their hidden lines).
pub fn row_of_line(view: &[ViewRow], line: usize) -> Option<usize> {
    view.iter().position(|r| match *r {
        ViewRow::Line(l) => l == line,
        ViewRow::Folded { start, hidden, .. } => line >= start && line < start + hidden,
    })
}

/// Text of the collapsed-output placeholder row.
pub fn placeholder_text(hidden: usize) -> String {
    format!("\u{25B8} {} line{} hidden", hidden, if hidden == 1 { "" } else { "s" })
}

fn placeholder_cells(text: &str, cols: usize) -> Vec<Cell> {
    let mut cells = vec![Cell::default(); cols];
    let attrs = Attrs { dim: true, italic: true, ..Attrs::default() };
    for (i, c) in text.chars().take(cols).enumerate() {
        cells[i] = Cell::with_pen(c, Color::Default, Color::Default, attrs.bits(), 0);
    }
    cells
}

/// Header chip parts: (badge, duration) e.g. ("✓", "1.2s"), ("✗ 127", "340ms"),
/// ("●", "3.4s") while running.
pub fn chip_parts(running: bool, exit_code: Option<i32>, duration_ms: u64) -> (String, String) {
    let d = crate::tools::blocks::format_duration(duration_ms);
    if running {
        return ("\u{25CF}".into(), d);
    }
    match exit_code {
        Some(0) | None => ("\u{2713}".into(), d),
        Some(c) => (format!("\u{2717} {c}"), d),
    }
}

#[cfg(test)]
/// Full chip text: "✓ 1.2s", "✗ 127 · 340ms", "● 3.4s" (running).
pub fn chip_text(running: bool, exit_code: Option<i32>, duration_ms: u64) -> String {
    let (badge, dur) = chip_parts(running, exit_code, duration_ms);
    format!("{badge}{}{dur}", chip_sep(running, exit_code))
}

pub fn chip_sep(running: bool, exit_code: Option<i32>) -> &'static str {
    if !running && exit_code.map_or(false, |c| c != 0) { " \u{00B7} " } else { " " }
}

/// A pane view that differs from the plain screen because of collapsed
/// blocks, with owned placeholder rows.
pub struct FoldedView {
    pub rows: Vec<ViewRow>,
    placeholders: Vec<Vec<Cell>>,
}

/// `Some` only when at least one collapsed block intersects the view.
pub fn folded_view(t: &Terminal) -> Option<FoldedView> {
    if t.is_alt_screen() || !t.blocks.osc_seen() {
        return None;
    }
    let folds = folds_of(&t.blocks);
    if folds.is_empty() {
        return None;
    }
    let total = t.scrollback.len() + t.grid.len();
    let rows = build_view(total, t.rows.min(t.grid.len()), t.scroll_offset, &folds);
    if !rows.iter().any(|r| matches!(r, ViewRow::Folded { .. })) {
        return None;
    }
    let placeholders = rows
        .iter()
        .filter_map(|r| match *r {
            ViewRow::Folded { hidden, .. } => Some(placeholder_cells(&placeholder_text(hidden), t.cols)),
            _ => None,
        })
        .collect();
    Some(FoldedView { rows, placeholders })
}

impl FoldedView {
    /// Cell rows in view order (borrowing the terminal and the placeholders).
    pub fn cell_rows<'a>(&'a self, t: &'a Terminal) -> Vec<&'a Vec<Cell>> {
        let sb = t.scrollback.len();
        let mut ph = self.placeholders.iter();
        self.rows
            .iter()
            .filter_map(|r| match *r {
                ViewRow::Line(l) if l < sb => t.scrollback.get(l),
                ViewRow::Line(l) => t.grid.get(l - sb),
                ViewRow::Folded { .. } => ph.next(),
            })
            .collect()
    }
}

/// The view for any pane (folded or plain), as absolute-line rows. Used by
/// the overlay/hit-testing so they agree with what the renderer drew.
pub fn pane_view(t: &Terminal) -> Vec<ViewRow> {
    let total = t.scrollback.len() + t.grid.len();
    let rows = t.rows.min(t.grid.len());
    let folds = if t.is_alt_screen() { Vec::new() } else { folds_of(&t.blocks) };
    build_view(total, rows, t.scroll_offset, &folds)
}

/// Absolute line for each visible row (None for fold placeholders), in the
/// same order the renderer draws. Selection/search/hit-testing use this so
/// they stay aligned when collapsed blocks are on screen.
pub fn view_abs_rows(t: &Terminal) -> Vec<Option<usize>> {
    pane_view(t)
        .into_iter()
        .map(|r| match r {
            ViewRow::Line(l) => Some(l),
            ViewRow::Folded { .. } => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[ViewRow]) -> Vec<usize> {
        v.iter().map(|r| r.abs()).collect()
    }

    #[test]
    fn plain_view_matches_window() {
        let v = build_view(100, 10, 0, &[]);
        assert_eq!(lines(&v), (90..100).collect::<Vec<_>>());
        let v = build_view(100, 10, 25, &[]);
        assert_eq!(lines(&v), (65..75).collect::<Vec<_>>());
        // short history
        let v = build_view(4, 10, 0, &[]);
        assert_eq!(lines(&v), vec![0, 1, 2, 3]);
    }

    #[test]
    fn fold_collapses_to_one_row_and_pulls_older_lines() {
        // lines 92..=96 hidden (5 lines)
        let f = [Fold { block: 3, start: 92, end: 96 }];
        let v = build_view(100, 10, 0, &f);
        assert_eq!(v.len(), 10);
        assert_eq!(v[9], ViewRow::Line(99));
        assert_eq!(v[7], ViewRow::Line(97));
        assert_eq!(v[6], ViewRow::Folded { block: 3, start: 92, hidden: 5 });
        // 6 older lines pulled in to fill the pane: 86..=91
        assert_eq!(v[0], ViewRow::Line(86));
        assert_eq!(v[5], ViewRow::Line(91));
    }

    #[test]
    fn fold_straddling_view_end_is_atomic() {
        let f = [Fold { block: 0, start: 40, end: 60 }];
        // end = 50 falls inside the fold: the whole fold shows as one row
        let v = build_view(100, 5, 50, &f);
        assert_eq!(*v.last().unwrap(), ViewRow::Folded { block: 0, start: 40, hidden: 21 });
        assert_eq!(v[0], ViewRow::Line(36));
    }

    #[test]
    fn two_folds() {
        let f = [Fold { block: 0, start: 2, end: 4 }, Fold { block: 1, start: 7, end: 8 }];
        let v = build_view(12, 20, 0, &f);
        assert_eq!(
            v,
            vec![
                ViewRow::Line(0), ViewRow::Line(1),
                ViewRow::Folded { block: 0, start: 2, hidden: 3 },
                ViewRow::Line(5), ViewRow::Line(6),
                ViewRow::Folded { block: 1, start: 7, hidden: 2 },
                ViewRow::Line(9), ViewRow::Line(10), ViewRow::Line(11),
            ]
        );
    }

    #[test]
    fn scroll_offset_for_top_is_exact() {
        let f = [Fold { block: 0, start: 20, end: 29 }, Fold { block: 1, start: 50, end: 59 }];
        let total = 100;
        let rows = 12;
        for target in [0usize, 5, 19, 20, 30, 45, 60, 70, 85] {
            let off = scroll_offset_for_top(total, rows, &f, target);
            let v = build_view(total, rows, off, &f);
            assert_eq!(v[0].abs(), target, "target {target} off {off}");
        }
        // too close to the bottom: clamps to live view
        assert_eq!(scroll_offset_for_top(total, rows, &f, 99), 0);
    }

    #[test]
    fn spans_group_by_block() {
        let view = vec![
            ViewRow::Line(0), ViewRow::Line(1), ViewRow::Line(2),
            ViewRow::Folded { block: 4, start: 3, hidden: 7 },
            ViewRow::Line(10), ViewRow::Line(11),
        ];
        // lines 0..=2 -> block 3, 10..=11 -> block 5, line 1 gap in none
        let lookup = |l: usize| match l {
            0..=2 => Some(3),
            10..=11 => Some(5),
            _ => None,
        };
        let spans = spans_of(&view, &lookup);
        assert_eq!(
            spans,
            vec![
                Span { block: 3, first: 0, last: 2 },
                Span { block: 4, first: 3, last: 3 },
                Span { block: 5, first: 4, last: 5 },
            ]
        );
    }

    #[test]
    fn mouse_y_maps_to_block() {
        let spans = [Span { block: 1, first: 2, last: 4 }, Span { block: 2, first: 5, last: 7 }];
        let (top, ch, n) = (30, 20, 10);
        // above the pane
        assert_eq!(span_at_y(&spans, 10, top, ch, n), None);
        // row 0/1: no block
        assert_eq!(span_at_y(&spans, 30, top, ch, n), None);
        assert_eq!(span_at_y(&spans, 69, top, ch, n), None);
        // row 2 begins at y = 70
        assert_eq!(span_at_y(&spans, 70, top, ch, n).map(|(r, s)| (r, s.block)), Some((2, 1)));
        assert_eq!(span_at_y(&spans, 129, top, ch, n).map(|(r, s)| (r, s.block)), Some((4, 1)));
        assert_eq!(span_at_y(&spans, 130, top, ch, n).map(|(r, s)| (r, s.block)), Some((5, 2)));
        // below the last row
        assert_eq!(span_at_y(&spans, 30 + 10 * 20, top, ch, n), None);
        assert_eq!(row_at_y(5, 0, 0, 3), None);
    }

    #[test]
    fn row_of_line_handles_folds() {
        let view = vec![
            ViewRow::Line(5),
            ViewRow::Folded { block: 0, start: 6, hidden: 4 },
            ViewRow::Line(10),
        ];
        assert_eq!(row_of_line(&view, 5), Some(0));
        assert_eq!(row_of_line(&view, 8), Some(1));
        assert_eq!(row_of_line(&view, 10), Some(2));
        assert_eq!(row_of_line(&view, 11), None);
    }

    #[test]
    fn chip_and_duration_formatting() {
        assert_eq!(chip_text(false, Some(0), 340), "\u{2713} 340ms");
        assert_eq!(chip_text(false, None, 1234), "\u{2713} 1.2s");
        assert_eq!(chip_text(false, Some(127), 340), "\u{2717} 127 \u{00B7} 340ms");
        assert_eq!(chip_text(true, None, 61_000), "\u{25CF} 1m 1s");
        assert_eq!(crate::tools::blocks::format_duration(999), "999ms");
        assert_eq!(crate::tools::blocks::format_duration(1000), "1.0s");
        assert_eq!(crate::tools::blocks::format_duration(3_723_000), "1h 2m");
    }

    #[test]
    fn placeholder_wording() {
        assert_eq!(placeholder_text(120), "\u{25B8} 120 lines hidden");
        assert_eq!(placeholder_text(1), "\u{25B8} 1 line hidden");
    }

    #[test]
    fn folded_terminal_view_uses_placeholder_rows() {
        let mut t = Terminal::new(20, 6);
        // 10 scrollback lines + 6 grid lines; label each by first char
        for i in 0..10 {
            let mut r = vec![Cell::default(); 20];
            r[0].c = char::from(b'a' + i as u8);
            t.scrollback.push_back(r);
        }
        for (i, row) in t.grid.iter_mut().enumerate() {
            row[0].c = char::from(b'k' + i as u8);
        }
        t.blocks.on_prompt_start(7);
        t.blocks.on_command_start(7, 0);
        t.blocks.on_command_output(8, "x".into());
        t.blocks.on_command_finished(11, Some(0)); // output 8..=11 (4 lines)
        assert!(folded_view(&t).is_none()); // not collapsed
        t.blocks.toggle_collapse(0);
        let fv = folded_view(&t).expect("collapsed block is in view");
        assert_eq!(fv.rows.len(), 6);
        let cells = fv.cell_rows(&t);
        assert_eq!(cells.len(), 6);
        let txt: Vec<String> = cells.iter().map(|r| r.iter().map(|c| c.c).collect::<String>().trim_end().to_string()).collect();
        // grid rows k..p are abs 10..15; view ends at 16 total lines
        assert_eq!(txt[5], "p");
        assert!(txt.iter().any(|s| s == "\u{25B8} 4 lines hidden"), "{txt:?}");
        // an older line (abs 7 = 'h') is pulled in above the fold
        assert_eq!(txt[0], "h");
    }
}

// ── Hover toolbar geometry ──

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    CopyCmd,
    CopyOutput,
    AskAi,
    Rerun,
    Collapse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolbarButton {
    pub button: Button,
    pub label: &'static str,
    pub x0: usize,
    pub x1: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toolbar {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub buttons: Vec<ToolbarButton>,
}

impl Toolbar {
    pub fn contains(&self, px: usize, py: usize) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    pub fn button_at(&self, px: usize, py: usize) -> Option<Button> {
        if !self.contains(px, py) {
            return None;
        }
        self.buttons.iter().find(|b| px >= b.x0 && px < b.x1).map(|b| b.button)
    }
}

/// Right margin kept free at the pane edge (scrollbar / chip alignment).
pub const EDGE_MARGIN: usize = 12;

/// Toolbar for a block whose first visible row starts at pixel `row_y`,
/// right-aligned inside `pane_x..pane_x + pane_w`. Finished blocks get all five
/// buttons; running ones only the copy/AI actions. Labels switch to a compact
/// form when the pane is narrow.
pub fn toolbar_layout(
    pane_x: usize,
    pane_w: usize,
    row_y: usize,
    cw: usize,
    ch: usize,
    running: bool,
    collapsed: bool,
) -> Toolbar {
    let build = |labels: &[(Button, &'static str)]| -> (Vec<ToolbarButton>, usize) {
        let mut x = 0;
        let mut v = Vec::new();
        for &(button, label) in labels {
            let w = (label.chars().count() + 2) * cw;
            v.push(ToolbarButton { button, label, x0: x, x1: x + w });
            x += w;
        }
        (v, x)
    };
    let fold_full = if collapsed { "Expand" } else { "Collapse" };
    let fold_short = if collapsed { "open" } else { "fold" };
    let mut full = vec![
        (Button::CopyCmd, "Copy cmd"),
        (Button::CopyOutput, "Copy output"),
        (Button::AskAi, "Ask AI"),
    ];
    let mut short = vec![(Button::CopyCmd, "cmd"), (Button::CopyOutput, "out"), (Button::AskAi, "AI")];
    if !running {
        full.push((Button::Rerun, "Rerun"));
        full.push((Button::Collapse, fold_full));
        short.push((Button::Rerun, "run"));
        short.push((Button::Collapse, fold_short));
    }
    let avail = pane_w.saturating_sub(EDGE_MARGIN);
    let (mut buttons, mut total) = build(&full);
    if total > avail {
        (buttons, total) = build(&short);
    }
    let x = (pane_x + avail).saturating_sub(total).max(pane_x);
    for b in &mut buttons {
        b.x0 += x;
        b.x1 += x;
    }
    Toolbar { x, y: row_y, w: total, h: ch, buttons }
}

#[cfg(test)]
mod toolbar_tests {
    use super::*;

    #[test]
    fn toolbar_is_right_aligned_and_hit_testable() {
        let tb = toolbar_layout(0, 1200, 100, 10, 20, false, false);
        assert_eq!(tb.buttons.len(), 5);
        assert_eq!(tb.x + tb.w, 1200 - EDGE_MARGIN);
        let first = &tb.buttons[0];
        assert_eq!(tb.button_at(first.x0, 105), Some(Button::CopyCmd));
        let last = tb.buttons.last().unwrap();
        assert_eq!(tb.button_at(last.x1 - 1, 119), Some(Button::Collapse));
        assert_eq!(tb.button_at(last.x1 - 1, 120), None); // below
        assert_eq!(tb.button_at(tb.x - 1, 105), None); // left of toolbar
    }

    #[test]
    fn narrow_pane_uses_compact_labels_and_running_has_no_rerun() {
        let wide = toolbar_layout(0, 1200, 0, 10, 20, false, false);
        let narrow = toolbar_layout(0, 300, 0, 10, 20, false, true);
        assert!(narrow.w < wide.w);
        assert_eq!(narrow.buttons.last().unwrap().label, "open");
        let running = toolbar_layout(0, 1200, 0, 10, 20, true, false);
        assert!(running.buttons.iter().all(|b| b.button != Button::Rerun && b.button != Button::Collapse));
    }
}
