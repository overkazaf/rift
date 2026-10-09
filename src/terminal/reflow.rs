//! Resize and text reflow (child module of `terminal`).
//!
//! The primary screen and its scrollback are treated as one list of physical
//! rows. Rows whose last cell carries the `wrap` flag (set by auto-wrap) are
//! joined into logical lines, which are re-wrapped at the new width. The
//! cursor is tracked through the rewrap. Changing only the number of rows
//! pushes top lines into the scrollback (shrink) or pulls them back (grow),
//! like xterm / kitty, keeping the cursor row on screen.
//!
//! Absolute line numbers held by command blocks and semantic marks are
//! remapped to the new layout (`BlockManager::remap_lines`).

use super::{Cell, Color, Terminal};

/// Upper bounds on terminal dimensions (guards against absurd allocations).
pub const MAX_COLS: usize = 2000;
pub const MAX_ROWS: usize = 1000;

fn blank_row(cols: usize) -> Vec<Cell> {
    vec![Cell::default(); cols]
}

fn cell_is_blank(c: &Cell) -> bool {
    c.c == ' '
        && c.extra_id() == 0
        && c.bg == Color::Default
        && !c.reverse()
        && c.ul_style() == super::grid::UnderlineStyle::None
        && !c.strikethrough()
        && !c.overline()
}

fn simple_resize(grid: &mut Vec<Vec<Cell>>, cols: usize, rows: usize) {
    grid.truncate(rows);
    while grid.len() < rows {
        grid.push(blank_row(cols));
    }
    for r in grid.iter_mut() {
        r.truncate(cols);
        r.resize(cols, Cell::default());
        if let Some(l) = r.last_mut() {
            // A wide cluster cut in half at the edge.
            if l.c == '\0' && !l.is_spacer() {
                *l = Cell::default();
            }
        }
    }
}

/// Lay `units` (cell, width) out in rows of `cols` cells. Returns the rows
/// and the (row, col) of unit `cursor_unit` when requested.
fn wrap_units(
    units: &[(Cell, usize)],
    cols: usize,
    cursor_unit: Option<usize>,
) -> (Vec<Vec<Cell>>, Option<(usize, usize)>) {
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut cur: Vec<Cell> = Vec::with_capacity(cols);
    let mut cur_w = 0usize;
    let mut pos = None;
    for (idx, (u, w)) in units.iter().enumerate() {
        let w = (*w).min(cols).max(1);
        if cur_w + w > cols {
            if cur_w < cols {
                // A wide cluster did not fit: leave a spacer.
                let mut sp = Cell::default();
                sp.c = '\0';
                sp.set_spacer(true);
                cur.push(sp);
            }
            if let Some(l) = cur.last_mut() {
                l.set_wrap(true);
            }
            cur.resize(cols, Cell::default());
            rows.push(std::mem::replace(&mut cur, Vec::with_capacity(cols)));
            cur_w = 0;
        }
        if Some(idx) == cursor_unit {
            pos = Some((rows.len(), cur_w));
        }
        let mut cell = *u;
        cell.set_wrap(false);
        cur.push(cell);
        if w == 2 {
            let mut c2 = cell;
            c2.c = '\0';
            c2.clear_cluster();
            cur.push(c2);
        }
        cur_w += w;
    }
    cur.resize(cols, Cell::default());
    rows.push(cur);
    (rows, pos)
}

impl Terminal {
    /// Resize the terminal. Dimensions are clamped to `1..=MAX_COLS` x
    /// `1..=MAX_ROWS`. The primary screen reflows; the alternate screen is
    /// simply cropped / padded (full-screen apps repaint on SIGWINCH).
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.clamp(1, MAX_COLS);
        let rows = rows.clamp(1, MAX_ROWS);
        if cols == self.cols && rows == self.rows {
            return;
        }
        let old_cols = self.cols;

        if self.using_alt_screen {
            let main = std::mem::take(&mut self.alt_grid).into_rows();
            let (g, cur) = self.reflow_main(main, self.alt_cursor, false, cols, rows);
            self.alt_grid = g.into();
            self.alt_cursor = cur;
            simple_resize(&mut self.grid, cols, rows);
            self.cursor_row = self.cursor_row.min(rows - 1);
            self.cursor_col = self.cursor_col.min(cols - 1);
        } else {
            let main = std::mem::take(&mut self.grid).into_rows();
            let wrap_next = self.wrap_next;
            let (g, cur) = self.reflow_main(main, (self.cursor_row, self.cursor_col), wrap_next, cols, rows);
            self.grid = g.into();
            self.cursor_row = cur.0.min(rows - 1);
            self.cursor_col = cur.1.min(cols - 1);
            simple_resize(&mut self.alt_grid, cols, rows);
        }

        if cols != old_cols {
            self.wrap_next = false;
        }
        self.cols = cols;
        self.rows = rows;
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.saved.row = self.saved.row.min(rows - 1);
        self.saved.col = self.saved.col.min(cols - 1);
        self.alt_cursor = (self.alt_cursor.0.min(rows - 1), self.alt_cursor.1.min(cols - 1));
        self.tab_stops.resize(cols, false);
        for i in old_cols.min(cols)..cols {
            self.tab_stops[i] = i % 8 == 0;
        }
        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
    }

    fn push_scrollback(&mut self, mut row: Vec<Cell>) {
        super::grid::trim_row(&mut row);
        self.scrollback.push_back(row);
        if self.scrollback.len() > self.max_scrollback {
            self.scrollback.pop_front();
            self.blocks.shift_lines(1);
        }
    }

    /// Reflow the primary screen `main` together with `self.scrollback`.
    /// `self.cols` still holds the OLD width. Returns the new grid and cursor.
    fn reflow_main(
        &mut self,
        mut main: Vec<Vec<Cell>>,
        cursor: (usize, usize),
        wrap_next: bool,
        new_cols: usize,
        new_rows: usize,
    ) -> (Vec<Vec<Cell>>, (usize, usize)) {
        if main.is_empty() {
            return ((0..new_rows).map(|_| blank_row(new_cols)).collect(), (0, 0));
        }
        let old_cols = self.cols;
        // Rows below the cursor that are blank are dropped and re-added as
        // padding, so the content (not the empty tail) drives scrolling.
        let mut keep_end = cursor.0.min(main.len() - 1);
        if let Some(last) = main.iter().rposition(|r| r.iter().any(|c| !cell_is_blank(c))) {
            keep_end = keep_end.max(last);
        }

        if new_cols == old_cols {
            // Row-count change only: move whole lines between grid and scrollback.
            main.truncate(keep_end + 1);
            let mut cur_row = cursor.0.min(main.len() - 1);
            let content = main.len();
            if content > new_rows {
                let shift = (content - new_rows).min(cur_row);
                let top: Vec<Vec<Cell>> = main.drain(..shift).collect();
                for row in top {
                    self.push_scrollback(row);
                }
                cur_row -= shift;
                main.truncate(new_rows);
            } else if content < new_rows && !self.scrollback.is_empty() {
                let pull = (new_rows - content).min(self.scrollback.len());
                let at = self.scrollback.len() - pull;
                let pulled: Vec<Vec<Cell>> = self.scrollback.drain(at..).collect();
                main.splice(0..0, pulled);
                cur_row += pull;
            }
            while main.len() < new_rows {
                main.push(blank_row(new_cols));
            }
            for r in main.iter_mut() {
                r.resize(new_cols, Cell::default());
            }
            return (main, (cur_row, cursor.1.min(new_cols - 1)));
        }

        // ── Column change: rewrap logical lines ──
        let nsb = self.scrollback.len();
        let trailing = main.len() - (keep_end + 1);
        let mut all: Vec<Vec<Cell>> = Vec::with_capacity(nsb + keep_end + 1);
        all.extend(self.scrollback.drain(..));
        all.extend(main.drain(..=keep_end));
        let keep_abs = nsb + keep_end;
        let cursor_abs = nsb + cursor.0.min(keep_end);
        let cursor_col = cursor.1;

        let mut out: Vec<Vec<Cell>> = Vec::with_capacity(all.len());
        let mut remap: Vec<usize> = Vec::with_capacity(all.len());
        let mut cur_out = (0usize, 0usize);
        let mut i = 0;
        while i < all.len() {
            let start = i;
            let mut end = i;
            while end + 1 < all.len() && all[end].last().map_or(false, |c| c.wrap()) {
                end += 1;
            }
            let has_cursor = cursor_abs >= start && cursor_abs <= end;
            let mut units: Vec<(Cell, usize)> = Vec::new();
            let mut cursor_unit = 0usize;
            for r in start..=end {
                let row = &all[r];
                if has_cursor && r == cursor_abs {
                    let limit = if wrap_next { row.len() } else { cursor_col.min(row.len()) };
                    cursor_unit = units.len() + row[..limit].iter().filter(|c| !c.is_continuation()).count();
                }
                let take = if r == end {
                    row.iter().rposition(|c| !cell_is_blank(c)).map_or(0, |p| p + 1)
                } else {
                    row.len()
                };
                for j in 0..take {
                    let c = row[j];
                    if c.is_continuation() {
                        continue;
                    }
                    let wide = j + 1 < row.len() && row[j + 1].c == '\0' && !row[j + 1].is_spacer();
                    let w = if wide {
                        2
                    } else {
                        1
                    };
                    let mut u = c;
                    u.set_wrap(false);
                    units.push((u, w));
                }
            }
            if has_cursor {
                while units.len() <= cursor_unit {
                    units.push((Cell::default(), 1));
                }
            }
            let (lines, pos) = wrap_units(&units, new_cols, has_cursor.then_some(cursor_unit));
            let base = out.len();
            if let Some((rr, cc)) = pos {
                cur_out = (base + rr, cc);
            }
            let n = lines.len();
            for r in start..=end {
                remap.push(base + (r - start).min(n - 1));
            }
            out.extend(lines);
            i = end + 1;
        }

        let total = out.len();
        let (cr, cc) = cur_out;
        let grid_start = total.saturating_sub(new_rows).min(cr);
        let mut grid: Vec<Vec<Cell>> = out.split_off(grid_start);
        grid.truncate(new_rows);
        while grid.len() < new_rows {
            grid.push(blank_row(new_cols));
        }
        let mut dropped = 0;
        let mut sb: std::collections::VecDeque<Vec<Cell>> = out.into();
        if sb.len() > self.max_scrollback {
            dropped = sb.len() - self.max_scrollback;
            sb.drain(..dropped);
        }
        sb.iter_mut().for_each(super::grid::trim_row);
        self.scrollback = sb;

        let map_line = move |l: usize| -> usize {
            let n = if l <= keep_abs { remap[l] } else { total + (l - keep_abs - 1) };
            n.saturating_sub(dropped)
        };
        let _ = trailing;
        self.blocks.remap_lines(&map_line);
        for m in &mut self.marks {
            m.line = map_line(m.line);
        }

        (grid, (cr - grid_start, cc.min(new_cols - 1)))
    }
}
