//! Throughput helpers for the hot output path (child module of `terminal`, so it
//! can touch `Terminal`'s private state without widening its public surface).

use super::{Charset, Terminal};

impl Terminal {
    /// Is the APC scanner mid-sequence? (When idle it only cares about ESC.)
    #[inline]
    pub fn apc_active(&self) -> bool {
        self.apc_scan != super::ApcScan::Idle
    }

    /// Can printable ASCII be written straight into the row (no per-char
    /// charset mapping / insert-mode shifting)?
    #[inline]
    pub fn ascii_fast_ok(&self) -> bool {
        if self.cols == 0 || self.insert_mode {
            return false;
        }
        let cs = if self.active_charset == 0 { self.g0_charset } else { self.g1_charset };
        cs != Charset::LineDrawing
    }

    /// Bulk-print a run of printable ASCII (`0x20..=0x7e`). Equivalent to calling
    /// [`Terminal::put_char`] per byte, but the interior of each row stretch is
    /// filled by copying the cell `put_char` just produced for the stretch's first
    /// byte (so current colors / attrs / hyperlink / future per-cell state follow
    /// automatically), and the stretch's last byte also goes through `put_char`
    /// (so wrap bookkeeping stays in exactly one place).
    pub fn print_ascii_run(&mut self, mut bytes: &[u8]) {
        if !self.ascii_fast_ok() {
            for &b in bytes {
                self.put_char(b as char);
            }
            return;
        }
        while let Some((&first, rest)) = bytes.split_first() {
            self.put_char(first as char);
            bytes = rest;
            if self.wrap_next || bytes.len() < 2 {
                continue;
            }
            // Cell just written by `put_char` (width 1, cursor already advanced).
            let template = self.grid[self.cursor_row][self.cursor_col - 1];
            let room = self.cols - self.cursor_col; // >= 1 since !wrap_next
            let n = room.min(bytes.len());
            let row = &mut self.grid[self.cursor_row];
            for (slot, &b) in row[self.cursor_col..self.cursor_col + n - 1].iter_mut().zip(&bytes[..n - 1]) {
                *slot = template;
                slot.c = b as char;
            }
            self.cursor_col += n - 1;
            self.put_char(bytes[n - 1] as char);
            bytes = &bytes[n..];
        }
    }

    /// Set the scrollback capacity (lines). Existing excess lines are dropped
    /// from the oldest end.
    pub fn set_max_scrollback(&mut self, lines: usize) {
        self.max_scrollback = lines;
        let excess = self.scrollback.len().saturating_sub(lines);
        if excess > 0 {
            self.scrollback.drain(..excess);
            self.blocks.shift_lines(excess);
        }
        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
    }
}
