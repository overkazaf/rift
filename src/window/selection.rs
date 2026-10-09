//! Text selection in absolute coordinates.
//!
//! Rows are *absolute* line indexes into `scrollback ++ grid` (see
//! `Terminal::abs_line`), so a selection stays attached to its text while the
//! view scrolls and can span scrollback.

use crate::terminal::grid::Cell;

/// Characters (besides alphanumerics) that belong to a "word" for double-click
/// selection: path/URL/identifier punctuation.
pub const WORD_CHARS: &str = "-_./~:@%+";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelMode {
    Char,
    Word,
    Line,
    /// Rectangular (Option/Alt + drag).
    Block,
}

type Pos = (usize, usize);

#[derive(Clone, Copy)]
pub struct Selection {
    /// (absolute row, col)
    pub start: Pos,
    pub end: Pos,
    pub active: bool,
    pub dragging: bool,
    pub mode: SelMode,
    /// Initial word / line range for Word and Line drags.
    anchor: (Pos, Pos),
}

impl Selection {
    pub fn new() -> Self {
        Self {
            start: (0, 0),
            end: (0, 0),
            active: false,
            dragging: false,
            mode: SelMode::Char,
            anchor: ((0, 0), (0, 0)),
        }
    }

    pub fn start_at(&mut self, row: usize, col: usize) {
        self.start = (row, col);
        self.end = (row, col);
        self.active = true;
        self.dragging = true;
        self.mode = SelMode::Char;
        self.anchor = ((row, col), (row, col));
    }

    /// Start a block (rectangular) selection.
    pub fn start_block(&mut self, row: usize, col: usize) {
        self.start_at(row, col);
        self.mode = SelMode::Block;
    }

    /// Start a word / line selection covering `range` (inclusive).
    pub fn start_range(&mut self, mode: SelMode, range: (Pos, Pos)) {
        self.start = range.0;
        self.end = range.1;
        self.active = true;
        self.dragging = true;
        self.mode = mode;
        self.anchor = range;
    }

    pub fn extend_to(&mut self, row: usize, col: usize) {
        if self.dragging {
            self.end = (row, col);
        }
    }

    /// Drag in Word / Line mode: union of the anchor range and the range
    /// under the pointer.
    pub fn extend_range(&mut self, cur: (Pos, Pos)) {
        if !self.dragging {
            return;
        }
        let (a0, a1) = self.anchor;
        if cur.0 < a0 {
            self.start = cur.0;
            self.end = a1;
        } else if cur.1 > a1 {
            self.start = a0;
            self.end = cur.1;
        } else {
            self.start = a0;
            self.end = a1;
        }
    }

    /// Shift+click: grow / shrink the selection towards (`row`, `col`),
    /// keeping the far end fixed, and continue as a drag.
    pub fn extend_from_click(&mut self, row: usize, col: usize) {
        let ((sr, sc), (er, ec)) = self.normalized();
        let click = (row, col);
        let anchor = if !self.active {
            self.end
        } else if click < (sr, sc) {
            (er, ec)
        } else {
            (sr, sc)
        };
        self.start = anchor;
        self.end = click;
        self.active = true;
        self.dragging = true;
        if self.mode != SelMode::Block {
            self.mode = SelMode::Char;
        }
        self.anchor = (anchor, anchor);
    }

    /// Select everything: `total_rows` lines of `cols` columns.
    pub fn select_all(&mut self, total_rows: usize, cols: usize) {
        self.start_at(0, 0);
        self.extend_to(total_rows.saturating_sub(1), cols.saturating_sub(1));
        self.finish();
    }

    pub fn finish(&mut self) {
        self.dragging = false;
        if self.mode == SelMode::Block {
            if self.start == self.end {
                self.active = false;
            }
            return;
        }
        let (sr, sc) = self.start;
        let (er, ec) = self.end;
        if sr > er || (sr == er && sc > ec) {
            self.start = (er, ec);
            self.end = (sr, sc);
        }
        // A plain click (no drag) selects nothing; word / line picks always do.
        if self.start == self.end && self.mode == SelMode::Char {
            self.active = false;
        }
    }

    pub fn clear(&mut self) {
        self.active = false;
        self.dragging = false;
    }

    /// Whether the cell at (absolute row, col) is selected.
    pub fn contains(&self, row: usize, col: usize) -> bool {
        if !self.active {
            return false;
        }
        let ((sr, sc), (er, ec)) = self.normalized();
        if row < sr || row > er {
            return false;
        }
        if self.mode == SelMode::Block {
            let (c0, c1) = (self.start.1.min(self.end.1), self.start.1.max(self.end.1));
            return col >= c0 && col <= c1;
        }
        if sr == er {
            return col >= sc && col <= ec;
        }
        if row == sr {
            return col >= sc;
        }
        if row == er {
            return col <= ec;
        }
        true
    }

    /// Selected text. `line(abs_row)` returns that row's cells.
    pub fn extract_text<'a>(&self, line: impl Fn(usize) -> Option<&'a [Cell]>) -> String {
        if !self.active {
            return String::new();
        }
        let ((sr, sc), (er, ec)) = self.normalized();
        let (bc0, bc1) = (self.start.1.min(self.end.1), self.start.1.max(self.end.1));
        let mut text = String::new();
        for row in sr..=er {
            let Some(cells) = line(row) else { break };
            let (from, to) = if self.mode == SelMode::Block {
                (bc0, bc1 + 1)
            } else {
                (
                    if row == sr { sc } else { 0 },
                    if row == er { ec + 1 } else { cells.len() },
                )
            };
            let to = to.min(cells.len());
            let from = from.min(to);
            let mut seg = String::new();
            for cell in &cells[from..to] {
                // '\0' marks the right half of a wide character.
                // Clusters (combining marks, ZWJ sequences) are copied whole.
                cell.push_text(&mut seg);
            }
            // A soft-wrapped row continues on the next one: join without a newline
            // (and keep its trailing spaces, they are real content of the line).
            let joins_next = row < er && self.mode != SelMode::Block && cells.last().map_or(false, |c| c.wrap());
            if (row < er && !joins_next) || self.mode == SelMode::Block {
                seg.truncate(seg.trim_end_matches(' ').len());
            }
            text.push_str(&seg);
            if row < er && !joins_next {
                text.push('\n');
            }
        }
        text.trim_end().to_string()
    }

    pub fn normalized(&self) -> (Pos, Pos) {
        let (sr, sc) = self.start;
        let (er, ec) = self.end;
        if sr > er || (sr == er && sc > ec) {
            ((er, ec), (sr, sc))
        } else {
            ((sr, sc), (er, ec))
        }
    }
}

// ── Word / line boundaries ──────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Other,
}

fn classify(c: char) -> Class {
    if c.is_alphanumeric() || WORD_CHARS.contains(c) {
        Class::Word
    } else if c == ' ' || c == '\t' || c == '\0' {
        Class::Space
    } else {
        Class::Other
    }
}

/// Inclusive column range of the "word" under `col`.
///
/// URLs are selected whole; otherwise the run of same-class characters
/// (word chars / whitespace / other punctuation) around `col`. The right half
/// of a wide character (`'\0'`) belongs to the character on its left.
pub fn word_range(cells: &[Cell], col: usize) -> (usize, usize) {
    if cells.is_empty() {
        return (0, 0);
    }
    let col = col.min(cells.len() - 1);
    // Wide-char continuation cell: act on the glyph's first cell.
    let mut col = col;
    if cells[col].c == '\0' && col > 0 {
        col -= 1;
    }
    // '\0' (wide-glyph continuation) stays in the line: detect_urls keeps it inside a URL.
    let line: String = cells.iter().map(|c| c.c).collect();
    if let Some((s, e, _)) = crate::tools::url_detect::detect_urls(&line)
        .into_iter()
        .find(|(s, e, _)| col >= *s && col < *e)
    {
        return (s, e - 1);
    }
    // '\0' after a non-space glyph continues that glyph.
    let class_at = |i: usize| -> Class {
        if cells[i].c == '\0' && i > 0 {
            classify(cells[i - 1].c)
        } else {
            classify(cells[i].c)
        }
    };
    let cls = class_at(col);
    let mut s = col;
    while s > 0 && class_at(s - 1) == cls {
        s -= 1;
    }
    let mut e = col;
    while e + 1 < cells.len() && class_at(e + 1) == cls {
        e += 1;
    }
    (s, e)
}

/// Did this row soft-wrap into the next one? (`Cell::wrap` on its last cell,
/// set by the terminal when auto-wrap moved the cursor to the next line.)
pub fn row_is_wrapped(cells: &[Cell]) -> bool {
    cells.last().map_or(false, |c| c.wrap())
}

/// First and last absolute row of the logical (soft-wrap joined) line that
/// contains `row`, among `n_rows` rows. `wrapped(r)` says whether row `r`
/// continues onto `r + 1`.
pub fn logical_line_span(n_rows: usize, wrapped: impl Fn(usize) -> bool, row: usize) -> (usize, usize) {
    if n_rows == 0 {
        return (0, 0);
    }
    let row = row.min(n_rows - 1);
    let mut first = row;
    while first > 0 && wrapped(first - 1) {
        first -= 1;
    }
    let mut last = row;
    while last + 1 < n_rows && wrapped(last) {
        last += 1;
    }
    (first, last)
}

/// Word range under (abs row, col) as selection endpoints.
pub fn word_at(term: &crate::terminal::Terminal, row: usize, col: usize) -> (Pos, Pos) {
    match term.abs_line(row) {
        Some(cells) => {
            // Scrollback rows are stored trimmed: pad so a click in the blank
            // tail selects the whitespace run, like on a live row.
            let padded;
            let cells = if cells.len() < term.cols {
                let mut v = cells.to_vec();
                v.resize(term.cols, Cell::default());
                padded = v;
                &padded[..]
            } else {
                cells
            };
            let (s, e) = word_range(cells, col);
            ((row, s), (row, e))
        }
        None => ((row, col), (row, col)),
    }
}

/// Logical line containing `row` as selection endpoints (full width).
pub fn line_at(term: &crate::terminal::Terminal, row: usize) -> (Pos, Pos) {
    let n = term.scrollback.len() + term.grid.len();
    let (first, last) = logical_line_span(
        n,
        |r| term.abs_line(r).map_or(false, row_is_wrapped),
        row,
    );
    ((first, 0), (last, term.cols.saturating_sub(1)))
}

pub fn copy_to_clipboard(text: &str) {
    if text.is_empty() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        if let Ok(mut child) = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
    }
    #[cfg(target_os = "linux")]
    {
        use std::io::Write;
        if let Ok(mut child) = std::process::Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
    }
    log::info!("Copied {} bytes to clipboard", text.len());
}

pub fn paste_from_clipboard() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("pbpaste").output().ok()?;
        if output.status.success() {
            return Some(String::from_utf8_lossy(&output.stdout).to_string());
        }
    }
    #[cfg(target_os = "linux")]
    {
        let output = std::process::Command::new("xclip")
            .args(["-selection", "clipboard", "-o"])
            .output()
            .ok()?;
        if output.status.success() {
            return Some(String::from_utf8_lossy(&output.stdout).to_string());
        }
    }
    None
}

/// Make text safe to write into a PTY as user input.
///
/// * removes the bracketed-paste markers `ESC[200~` / `ESC[201~` (a pasted
///   `ESC[201~` would end the paste early and run the rest as typed input),
/// * removes every remaining ESC, DEL, C0 control (except tab, LF, CR) and C1
///   control (U+0080..=U+009F, which include the 8-bit CSI),
/// * turns CRLF into CR (what Enter sends), in both paste modes.
pub fn sanitize_paste(text: &str, bracketed: bool) -> String {
    let _ = bracketed; // same rules in both modes; the caller adds the markers.
    let text = text.replace("\x1b[200~", "").replace("\x1b[201~", "");
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\r');
            }
            '\n' | '\t' => out.push(c),
            c if (c as u32) < 0x20 || c == '\x7f' || ('\u{80}'..='\u{9f}').contains(&c) => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(s: &str) -> Vec<Cell> {
        use unicode_width::UnicodeWidthChar;
        let mut v = Vec::new();
        for c in s.chars() {
            let mut cell = Cell::default();
            cell.c = c;
            v.push(cell);
            if c.width() == Some(2) {
                let mut p = Cell::default();
                p.c = '\0';
                v.push(p);
            }
        }
        v
    }

    fn word(s: &str, col: usize) -> String {
        let c = cells(s);
        let (a, b) = word_range(&c, col);
        c[a..=b].iter().filter(|c| c.c != '\0').map(|c| c.c).collect()
    }

    #[test]
    fn words_include_path_punctuation() {
        let line = "cd ~/src/rift-term/main.rs now";
        assert_eq!(word(line, 4), "~/src/rift-term/main.rs");
        assert_eq!(word(line, 0), "cd");
        assert_eq!(word(line, 27), "now");
        assert_eq!(word("user@host:22", 6), "user@host:22");
    }

    #[test]
    fn whitespace_and_punctuation_runs() {
        assert_eq!(word("a   b", 2), "   ");
        assert_eq!(word("foo(bar)", 3), "(");
        assert_eq!(word("foo(bar)", 5), "bar");
        assert_eq!(word("x => y", 3), "=>");
    }

    #[test]
    fn urls_are_selected_whole() {
        let line = "see https://example.com/a?b=1&c=2#frag, ok";
        let w = word(line, 20);
        assert_eq!(w, "https://example.com/a?b=1&c=2#frag");
        // Clicking on the scheme part works too.
        assert_eq!(word(line, 5), "https://example.com/a?b=1&c=2#frag");
        // Trailing comma is not part of it.
        assert_eq!(word(line, 40), "ok");
    }

    #[test]
    fn cjk_words_and_wide_continuation() {
        let line = "echo 终端 test";
        // cells: e c h o _ 终 \0 端 \0 _ t e s t
        assert_eq!(word(line, 5), "终端");
        assert_eq!(word(line, 6), "终端", "continuation cell resolves to its glyph");
        assert_eq!(word(line, 12), "test");
    }

    #[test]
    fn word_range_clamps() {
        let c = cells("abc");
        assert_eq!(word_range(&c, 99), (0, 2));
        assert_eq!(word_range(&[], 3), (0, 0));
    }

    #[test]
    fn logical_line_follows_soft_wraps() {
        // rows 1,2 are soft-wrapped into row 3; row 0 and row 4 stand alone.
        let wrapped = |r: usize| matches!(r, 1 | 2);
        assert_eq!(logical_line_span(5, wrapped, 0), (0, 0));
        assert_eq!(logical_line_span(5, wrapped, 1), (1, 3));
        assert_eq!(logical_line_span(5, wrapped, 2), (1, 3));
        assert_eq!(logical_line_span(5, wrapped, 3), (1, 3));
        assert_eq!(logical_line_span(5, wrapped, 4), (4, 4));
        // The last row never claims a continuation.
        assert_eq!(logical_line_span(3, |_| true, 2), (0, 2));
        assert_eq!(logical_line_span(0, |_| true, 2), (0, 0));
    }

    #[test]
    fn wrapped_row_heuristic() {
        // Only the terminal's soft-wrap flag on the last cell counts - a full row
        // that ended in a real newline is not wrapped.
        let mut full = cells("abcd");
        assert!(!row_is_wrapped(&full));
        full.last_mut().unwrap().set_wrap(true);
        assert!(row_is_wrapped(&full));
        assert!(!row_is_wrapped(&cells("abc ")));
        assert!(!row_is_wrapped(&[]));
    }

    fn sel(a: Pos, b: Pos, mode: SelMode) -> Selection {
        let mut s = Selection::new();
        s.start_at(a.0, a.1);
        s.mode = mode;
        s.extend_to(b.0, b.1);
        s.finish();
        s
    }

    #[test]
    fn contains_stream_and_block() {
        let s = sel((2, 5), (4, 3), SelMode::Char);
        assert!(s.contains(2, 5) && s.contains(2, 99));
        assert!(!s.contains(2, 4));
        assert!(s.contains(3, 0) && s.contains(3, 99));
        assert!(s.contains(4, 3) && !s.contains(4, 4));
        assert!(!s.contains(1, 5) && !s.contains(5, 0));

        let b = sel((2, 8), (4, 3), SelMode::Block);
        assert!(b.contains(3, 3) && b.contains(3, 8) && b.contains(2, 5));
        assert!(!b.contains(3, 2) && !b.contains(3, 9));
    }

    #[test]
    fn click_without_drag_selects_nothing_but_word_does() {
        let s = sel((3, 3), (3, 3), SelMode::Char);
        assert!(!s.active);
        let mut w = Selection::new();
        w.start_range(SelMode::Word, ((3, 3), (3, 3))); // one-letter word
        w.finish();
        assert!(w.active);
    }

    #[test]
    fn word_drag_unions_with_anchor() {
        let mut s = Selection::new();
        s.start_range(SelMode::Word, ((5, 10), (5, 14)));
        s.extend_range(((5, 20), (5, 25)));
        assert_eq!((s.start, s.end), ((5, 10), (5, 25)));
        s.extend_range(((5, 0), (5, 3)));
        assert_eq!((s.start, s.end), ((5, 0), (5, 14)));
        s.extend_range(((5, 11), (5, 12)));
        assert_eq!((s.start, s.end), ((5, 10), (5, 14)));
    }

    #[test]
    fn shift_click_extends_from_far_end() {
        let mut s = sel((2, 2), (2, 8), SelMode::Char);
        s.extend_from_click(6, 1);
        assert_eq!((s.start, s.end), ((2, 2), (6, 1)));
        s.finish();
        // Click before the start keeps the far end fixed.
        s.extend_from_click(1, 4);
        s.finish();
        assert_eq!(s.normalized(), ((1, 4), (6, 1)));
        // With no selection the last click position is the anchor.
        let mut n = sel((4, 4), (4, 4), SelMode::Char);
        assert!(!n.active);
        n.extend_from_click(4, 9);
        n.finish();
        assert_eq!(n.normalized(), ((4, 4), (4, 9)));
    }

    fn grid(rows: &[&str]) -> Vec<Vec<Cell>> {
        rows.iter().map(|r| cells(r)).collect()
    }

    fn text(s: &Selection, g: &[Vec<Cell>]) -> String {
        s.extract_text(|r| g.get(r).map(|v| v.as_slice()))
    }

    #[test]
    fn extract_across_rows_and_blocks() {
        let g = grid(&["hello   ", "world   ", "again   "]);
        let s = sel((0, 3), (2, 1), SelMode::Char);
        assert_eq!(text(&s, &g), "lo\nworld\nag");
        let b = sel((0, 1), (2, 3), SelMode::Block);
        assert_eq!(text(&b, &g), "ell\norl\ngai");
    }

    #[test]
    fn extract_skips_wide_continuations() {
        let g = grid(&["a终端b"]);
        let s = sel((0, 0), (0, 5), SelMode::Char);
        assert_eq!(text(&s, &g), "a终端b");
    }

    #[test]
    fn select_all_covers_scrollback_rows() {
        let g = grid(&["one", "two", "six"]);
        let mut s = Selection::new();
        s.select_all(3, 3);
        assert!(s.active);
        assert_eq!(text(&s, &g), "one\ntwo\nsix");
    }

    #[test]
    fn sanitize_paste_strips_escapes_and_paste_markers() {
        assert_eq!(sanitize_paste("ls\x1b[201~; rm -rf ~\r", true), "ls; rm -rf ~\r");
        assert_eq!(sanitize_paste("a\x1b[200~b", false), "ab");
        assert_eq!(sanitize_paste("\x1b]0;title\x07x\x1b[31mred", true), "]0;titlex[31mred");
        assert_eq!(sanitize_paste("c\u{9b}201~d\u{85}e", true), "c201~de");
        assert_eq!(sanitize_paste("a\x03b\x04c\x7fd\x00e", false), "abcde");
    }

    #[test]
    fn sanitize_paste_keeps_text_and_normalizes_newlines() {
        assert_eq!(sanitize_paste("a\tb \u{4e2d}\u{6587} \u{1f600}", true), "a\tb \u{4e2d}\u{6587} \u{1f600}");
        assert_eq!(sanitize_paste("l1\r\nl2\r\n", true), "l1\rl2\r");
        assert_eq!(sanitize_paste("l1\nl2\r", false), "l1\nl2\r");
        assert_eq!(sanitize_paste("", true), "");
    }
}
