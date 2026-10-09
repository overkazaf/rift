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
            let r = self.cursor_row;
            let row = &mut self.grid.rows[r];
            for (slot, &b) in row[self.cursor_col..self.cursor_col + n - 1].iter_mut().zip(&bytes[..n - 1]) {
                *slot = template;
                slot.c = b as char;
            }
            // The cell the stretch's last byte goes into may be the right half of
            // a wide glyph whose left half we just overwrote: orphan it exactly as
            // per-character printing would have (`put_char`'s "after" fix-up).
            let last = self.cursor_col + n - 1;
            if row[last].c == '\0' && !row[last].is_spacer() {
                row[last].c = ' ';
            }
            self.grid.raise(r, last);
            self.cursor_col += n - 1;
            self.put_char(bytes[n - 1] as char);
            bytes = &bytes[n..];
        }
    }

    /// Parse and apply a plain SGR sequence (`ESC [ <digits ; :>* m`) at the start
    /// of `data` without going through the VT state machine. Only valid while the
    /// parser is known to be in its ground state. Returns the bytes consumed, or
    /// `None` (nothing applied) when the bytes are anything else, so the caller
    /// falls back to the parser, which then handles every odd case identically.
    pub fn try_fast_sgr(&mut self, data: &[u8]) -> Option<usize> {
        if data.len() < 3 || data[0] != 0x1b || data[1] != b'[' {
            return None;
        }
        // Mirrors vte's `Params`: at most 32 values; `;` closes a group, `:`
        // continues it with a sub-value.
        let mut vals = [0u16; 32];
        let mut gstart = [0u8; 33];
        let (mut nv, mut ng) = (0usize, 0usize);
        let mut cur = 0u16;
        let mut digits = 0;
        let mut j = 2;
        loop {
            let b = *data.get(j)?;
            j += 1;
            match b {
                b'0'..=b'9' => {
                    digits += 1;
                    if digits > 4 {
                        return None; // let the parser deal with huge values
                    }
                    cur = cur * 10 + (b - b'0') as u16;
                }
                b';' | b':' | b'm' => {
                    if nv == 32 {
                        return None; // too many params: parser sets `ignore`
                    }
                    vals[nv] = cur;
                    nv += 1;
                    cur = 0;
                    digits = 0;
                    if b != b':' {
                        ng += 1;
                        gstart[ng] = nv as u8;
                        if b == b'm' {
                            break;
                        }
                    }
                }
                _ => return None,
            }
        }
        let mut groups: [&[u16]; 32] = [&[]; 32];
        for g in 0..ng {
            groups[g] = &vals[gstart[g] as usize..gstart[g + 1] as usize];
        }
        super::ansi::apply_sgr_groups(self, &groups[..ng]);
        Some(j)
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
