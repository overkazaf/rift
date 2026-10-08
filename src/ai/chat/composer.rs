//! Multi-line text editor behind the chat composer.
//!
//! The buffer is a `Vec<char>` with a char-index cursor. Visual layout (soft
//! wrapping at a column width, CJK-aware) is computed on demand by
//! [`Composer::visual_lines`]; vertical cursor movement uses it so Up/Down
//! follow what is on screen.

use super::markdown::cells;

#[derive(Default, Clone)]
pub struct Composer {
    chars: Vec<char>,
    cursor: usize,
    /// Preferred column (in cells) kept across vertical moves.
    goal_col: Option<usize>,
    /// Width in cells used by the last render (for vertical movement).
    pub cols: usize,
}

/// One visual row: char range `[start, end)` of the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VLine {
    pub start: usize,
    pub end: usize,
}

impl Composer {
    pub fn new() -> Self {
        Self { cols: 40, ..Default::default() }
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn chars(&self) -> &[char] {
        &self.chars
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set_text(&mut self, s: &str) {
        self.chars = s.chars().collect();
        self.cursor = self.chars.len();
        self.goal_col = None;
    }

    pub fn set_cursor(&mut self, pos: usize) {
        self.cursor = pos.min(self.chars.len());
        self.goal_col = None;
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
        self.goal_col = None;
    }

    /// Take the text and reset.
    pub fn take(&mut self) -> String {
        let s = self.text();
        self.clear();
        s
    }

    pub fn insert_str(&mut self, s: &str) {
        let ins: Vec<char> = s.replace("\r\n", "\n").replace('\r', "\n").chars().filter(|c| *c == '\n' || !c.is_control()).collect();
        let n = ins.len();
        self.chars.splice(self.cursor..self.cursor, ins);
        self.cursor += n;
        self.goal_col = None;
    }

    pub fn insert_newline(&mut self) {
        self.insert_str("\n");
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
        self.goal_col = None;
    }

    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
        self.goal_col = None;
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
        self.goal_col = None;
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.chars.len());
        self.goal_col = None;
    }

    fn is_word(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    /// Start of the word before the cursor (skips spaces, then a word run).
    fn word_left_pos(&self) -> usize {
        let mut i = self.cursor;
        while i > 0 && self.chars[i - 1].is_whitespace() && self.chars[i - 1] != '\n' {
            i -= 1;
        }
        if i > 0 && self.chars[i - 1] == '\n' {
            return i - 1;
        }
        if i > 0 && Self::is_word(self.chars[i - 1]) {
            while i > 0 && Self::is_word(self.chars[i - 1]) {
                i -= 1;
            }
        } else if i > 0 {
            i -= 1;
        }
        i
    }

    fn word_right_pos(&self) -> usize {
        let n = self.chars.len();
        let mut i = self.cursor;
        while i < n && self.chars[i].is_whitespace() && self.chars[i] != '\n' {
            i += 1;
        }
        if i < n && self.chars[i] == '\n' {
            return i + 1;
        }
        if i < n && Self::is_word(self.chars[i]) {
            while i < n && Self::is_word(self.chars[i]) {
                i += 1;
            }
        } else if i < n {
            i += 1;
        }
        i
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_left_pos();
        self.goal_col = None;
    }

    pub fn word_right(&mut self) {
        self.cursor = self.word_right_pos();
        self.goal_col = None;
    }

    /// Alt+Backspace.
    pub fn delete_word_back(&mut self) {
        let to = self.word_left_pos();
        self.chars.drain(to..self.cursor);
        self.cursor = to;
        self.goal_col = None;
    }

    /// Start of the logical line (after the previous `\n`).
    fn line_start(&self, pos: usize) -> usize {
        self.chars[..pos].iter().rposition(|c| *c == '\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, pos: usize) -> usize {
        self.chars[pos..].iter().position(|c| *c == '\n').map_or(self.chars.len(), |i| pos + i)
    }

    /// Cmd+Backspace / Ctrl+U.
    pub fn delete_to_line_start(&mut self) {
        let s = self.line_start(self.cursor);
        self.chars.drain(s..self.cursor);
        self.cursor = s;
        self.goal_col = None;
    }

    /// Ctrl+K.
    pub fn delete_to_line_end(&mut self) {
        let e = self.line_end(self.cursor);
        if e == self.cursor && e < self.chars.len() {
            self.chars.remove(e); // join with next line
        } else {
            self.chars.drain(self.cursor..e);
        }
        self.goal_col = None;
    }

    /// Home: start of the logical line.
    pub fn home(&mut self) {
        self.cursor = self.line_start(self.cursor);
        self.goal_col = None;
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end(self.cursor);
        self.goal_col = None;
    }

    pub fn buffer_start(&mut self) {
        self.cursor = 0;
        self.goal_col = None;
    }

    pub fn buffer_end(&mut self) {
        self.cursor = self.chars.len();
        self.goal_col = None;
    }

    /// Soft-wrapped rows for `cols` cells. Always at least one row.
    pub fn visual_lines(&self, cols: usize) -> Vec<VLine> {
        visual_lines(&self.chars, cols)
    }

    /// (row, column-in-cells) of `pos` in `lines`.
    pub fn locate(&self, lines: &[VLine], pos: usize) -> (usize, usize) {
        locate(&self.chars, lines, pos)
    }

    /// Move the cursor one visual row up/down. Returns false when already at
    /// the first/last row (caller can use the key for something else).
    pub fn move_vertical(&mut self, down: bool) -> bool {
        let lines = self.visual_lines(self.cols.max(1));
        let (row, col) = self.locate(&lines, self.cursor);
        let goal = *self.goal_col.get_or_insert(col);
        let target = if down {
            if row + 1 >= lines.len() {
                return false;
            }
            row + 1
        } else {
            if row == 0 {
                return false;
            }
            row - 1
        };
        let l = lines[target];
        let mut w = 0;
        let mut pos = l.start;
        while pos < l.end {
            let cw = cells(self.chars[pos]);
            if w + cw > goal {
                break;
            }
            w += cw;
            pos += 1;
        }
        self.cursor = pos;
        true
    }
}

pub fn visual_lines(chars: &[char], cols: usize) -> Vec<VLine> {
    let cols = cols.max(1);
    let mut out = Vec::new();
    let mut start = 0;
    let mut w = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            out.push(VLine { start, end: i });
            start = i + 1;
            w = 0;
        } else {
            let cw = cells(c);
            if w + cw > cols && w > 0 {
                out.push(VLine { start, end: i });
                start = i;
                w = 0;
            }
            w += cw;
        }
        i += 1;
    }
    out.push(VLine { start, end: chars.len() });
    out
}

pub fn locate(chars: &[char], lines: &[VLine], pos: usize) -> (usize, usize) {
    for (r, l) in lines.iter().enumerate() {
        // The cursor at a soft-wrap boundary belongs to the next row; at the
        // end of a logical line it belongs to that row.
        let last = r + 1 == lines.len();
        let next_starts_here = lines.get(r + 1).is_some_and(|n| n.start == pos && n.start == l.end);
        if pos >= l.start && (pos < l.end || (pos == l.end && !next_starts_here) || last) {
            let col = chars[l.start..pos.min(l.end)].iter().map(|c| cells(*c)).sum();
            return (r, col);
        }
    }
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comp(s: &str) -> Composer {
        let mut c = Composer::new();
        c.insert_str(s);
        c
    }

    #[test]
    fn insert_and_delete_with_cursor() {
        let mut c = comp("hello world");
        c.left();
        c.left();
        c.insert_str("X");
        assert_eq!(c.text(), "hello worXld");
        c.backspace();
        c.delete();
        assert_eq!(c.text(), "hello word");
        c.insert_str("a\r\nb\u{7}");
        assert_eq!(c.text(), "hello wora\nbd");
    }

    #[test]
    fn word_motion_and_alt_backspace() {
        let mut c = comp("git commit -m foo");
        c.delete_word_back();
        assert_eq!(c.text(), "git commit -m ");
        c.delete_word_back();
        assert_eq!(c.text(), "git commit -");
        let mut c = comp("ab cd");
        c.word_left();
        assert_eq!(c.cursor(), 3);
        c.word_left();
        assert_eq!(c.cursor(), 0);
        c.word_right();
        assert_eq!(c.cursor(), 2);
    }

    #[test]
    fn home_end_and_line_kills() {
        let mut c = comp("one\ntwo three");
        c.home();
        assert_eq!(c.cursor(), 4);
        c.end();
        assert_eq!(c.cursor(), 13);
        c.delete_to_line_start();
        assert_eq!(c.text(), "one\n");
        let mut c = comp("ab\ncd");
        c.buffer_start();
        c.delete_to_line_end();
        assert_eq!(c.text(), "\ncd");
        c.delete_to_line_end();
        assert_eq!(c.text(), "cd");
    }

    #[test]
    fn visual_lines_wrap_and_newlines() {
        let c = comp("abcdef\nxy");
        let l = c.visual_lines(4);
        assert_eq!(l, vec![VLine { start: 0, end: 4 }, VLine { start: 4, end: 6 }, VLine { start: 7, end: 9 }]);
        let l = visual_lines(&"中中中".chars().collect::<Vec<_>>(), 5);
        assert_eq!(l, vec![VLine { start: 0, end: 2 }, VLine { start: 2, end: 3 }]);
        assert_eq!(visual_lines(&[], 5), vec![VLine { start: 0, end: 0 }]);
    }

    #[test]
    fn vertical_movement_keeps_goal_column() {
        let mut c = comp("abcdef\nxy\nlonger line");
        c.cols = 40;
        c.buffer_start();
        for _ in 0..5 {
            c.right();
        }
        assert!(c.move_vertical(true)); // "xy": clamps to end
        assert_eq!(c.cursor(), 9);
        assert!(c.move_vertical(true)); // goal col 5 restored
        assert_eq!(c.cursor(), 10 + 5);
        assert!(!c.move_vertical(true));
        assert!(c.move_vertical(false));
        assert!(c.move_vertical(false));
        assert!(!c.move_vertical(false));
    }

    #[test]
    fn locate_cursor_on_wrapped_rows() {
        let c = comp("abcdefgh");
        let l = c.visual_lines(4);
        assert_eq!(c.locate(&l, 0), (0, 0));
        assert_eq!(c.locate(&l, 4), (1, 0)); // boundary belongs to next row
        assert_eq!(c.locate(&l, 8), (1, 4));
        let c = comp("ab\n");
        let l = c.visual_lines(4);
        assert_eq!(c.locate(&l, 3), (1, 0));
        assert_eq!(c.locate(&l, 2), (0, 2));
    }
}
