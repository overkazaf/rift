//! Pure single-line text editing model for the browser address field.
//!
//! Indices are `char` positions (not bytes). Selection is the range between
//! `anchor` and `cursor`; there is no selection when `anchor` is `None`.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddrField {
    chars: Vec<char>,
    cursor: usize,
    anchor: Option<usize>,
    /// First visible char index (horizontal scroll), maintained by `visible_range`.
    scroll: usize,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl AddrField {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the content; cursor goes to the end, selection and scroll reset.
    pub fn set_text(&mut self, s: &str) {
        self.chars = s.chars().filter(|c| !c.is_control()).collect();
        self.cursor = self.chars.len();
        self.anchor = None;
        self.scroll = 0;
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn chars(&self) -> &[char] {
        &self.chars
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Ordered selection range `(start, end)`, `None` when empty.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        if a == self.cursor {
            None
        } else {
            Some((a.min(self.cursor), a.max(self.cursor)))
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection().map(|(s, e)| self.chars[s..e].iter().collect())
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.chars.len();
    }

    /// Delete the selection if any; returns whether something was removed.
    fn delete_selection(&mut self) -> bool {
        if let Some((s, e)) = self.selection() {
            self.chars.drain(s..e);
            self.cursor = s;
            self.anchor = None;
            true
        } else {
            self.anchor = None;
            false
        }
    }

    /// Insert text at the cursor, replacing the selection. Control chars
    /// (including newlines) are dropped, so pasted multi-line text is flattened.
    pub fn insert_str(&mut self, s: &str) {
        self.delete_selection();
        let clean: Vec<char> = s.chars().filter(|c| !c.is_control()).collect();
        let n = clean.len();
        self.chars.splice(self.cursor..self.cursor, clean);
        self.cursor += n;
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    /// Alt+Backspace: delete back to the start of the previous word.
    pub fn delete_word_back(&mut self) {
        if self.delete_selection() {
            return;
        }
        let to = self.word_left_pos(self.cursor);
        self.chars.drain(to..self.cursor);
        self.cursor = to;
    }

    /// Cmd+Backspace: delete everything before the cursor.
    pub fn delete_to_start(&mut self) {
        if self.delete_selection() {
            return;
        }
        self.chars.drain(0..self.cursor);
        self.cursor = 0;
    }

    /// Remove and return the selection (for Cmd+X).
    pub fn cut(&mut self) -> Option<String> {
        let t = self.selected_text();
        self.delete_selection();
        t
    }

    fn word_left_pos(&self, from: usize) -> usize {
        let mut i = from.min(self.chars.len());
        while i > 0 && !is_word(self.chars[i - 1]) {
            i -= 1;
        }
        while i > 0 && is_word(self.chars[i - 1]) {
            i -= 1;
        }
        i
    }

    fn word_right_pos(&self, from: usize) -> usize {
        let n = self.chars.len();
        let mut i = from.min(n);
        while i < n && !is_word(self.chars[i]) {
            i += 1;
        }
        while i < n && is_word(self.chars[i]) {
            i += 1;
        }
        i
    }

    /// Move the cursor to `pos`, extending the selection when `extend`.
    pub fn set_cursor(&mut self, pos: usize, extend: bool) {
        let pos = pos.min(self.chars.len());
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.cursor = pos;
        if self.anchor == Some(self.cursor) {
            self.anchor = None;
        }
    }

    pub fn move_left(&mut self, extend: bool) {
        if !extend {
            if let Some((s, _)) = self.selection() {
                self.set_cursor(s, false);
                return;
            }
        }
        self.set_cursor(self.cursor.saturating_sub(1), extend);
    }

    pub fn move_right(&mut self, extend: bool) {
        if !extend {
            if let Some((_, e)) = self.selection() {
                self.set_cursor(e, false);
                return;
            }
        }
        self.set_cursor(self.cursor + 1, extend);
    }

    pub fn move_word_left(&mut self, extend: bool) {
        self.set_cursor(self.word_left_pos(self.cursor), extend);
    }

    pub fn move_word_right(&mut self, extend: bool) {
        self.set_cursor(self.word_right_pos(self.cursor), extend);
    }

    pub fn move_home(&mut self, extend: bool) {
        self.set_cursor(0, extend);
    }

    pub fn move_end(&mut self, extend: bool) {
        self.set_cursor(self.chars.len(), extend);
    }

    /// Update the horizontal scroll so the cursor fits in `cols` columns and
    /// return the visible char range `(start, end)`.
    pub fn visible_range(&mut self, cols: usize) -> (usize, usize) {
        let n = self.chars.len();
        if cols == 0 {
            return (0, 0);
        }
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        }
        if self.cursor >= self.scroll + cols {
            self.scroll = self.cursor + 1 - cols;
        }
        // Don't leave blank space on the right when text could fill the field.
        let max_scroll = (n + 1).saturating_sub(cols);
        self.scroll = self.scroll.min(max_scroll);
        (self.scroll, (self.scroll + cols).min(n))
    }

    #[cfg(test)]
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Map a clicked column (relative to the field's text origin) to a char index.
    pub fn pos_from_col(&self, col: usize) -> usize {
        (self.scroll + col).min(self.chars.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(s: &str) -> AddrField {
        let mut a = AddrField::new();
        a.set_text(s);
        a
    }

    #[test]
    fn insert_at_cursor() {
        let mut a = f("helo");
        a.set_cursor(3, false);
        a.insert_str("l");
        assert_eq!(a.text(), "hello");
        assert_eq!(a.cursor(), 4);
    }

    #[test]
    fn insert_unicode_counts_chars() {
        let mut a = f("a");
        a.insert_str("你好");
        assert_eq!(a.cursor(), 3);
        a.move_left(false);
        a.backspace();
        assert_eq!(a.text(), "a好");
    }

    #[test]
    fn paste_strips_newlines() {
        let mut a = f("");
        a.insert_str("http://x.com/\r\n  a\nb\t");
        assert_eq!(a.text(), "http://x.com/  ab");
    }

    #[test]
    fn backspace_and_delete() {
        let mut a = f("abc");
        a.move_left(false);
        a.backspace();
        assert_eq!(a.text(), "ac");
        a.delete_forward();
        assert_eq!(a.text(), "a");
        a.delete_forward();
        assert_eq!(a.text(), "a");
        a.move_home(false);
        a.backspace();
        assert_eq!(a.text(), "a");
    }

    #[test]
    fn select_all_then_type_replaces() {
        let mut a = f("old text");
        a.select_all();
        assert_eq!(a.selected_text().as_deref(), Some("old text"));
        a.insert_str("new");
        assert_eq!(a.text(), "new");
        assert_eq!(a.selection(), None);
    }

    #[test]
    fn shift_arrows_select() {
        let mut a = f("hello");
        a.move_left(true);
        a.move_left(true);
        assert_eq!(a.selection(), Some((3, 5)));
        assert_eq!(a.selected_text().as_deref(), Some("lo"));
        a.move_right(true);
        assert_eq!(a.selection(), Some((4, 5)));
        a.move_right(true);
        assert_eq!(a.selection(), None);
    }

    #[test]
    fn plain_arrow_collapses_selection() {
        let mut a = f("hello");
        a.select_all();
        a.move_left(false);
        assert_eq!((a.cursor(), a.selection()), (0, None));
        a.select_all();
        a.move_right(false);
        assert_eq!((a.cursor(), a.selection()), (5, None));
    }

    #[test]
    fn home_end_with_shift() {
        let mut a = f("hello");
        a.set_cursor(2, false);
        a.move_end(true);
        assert_eq!(a.selection(), Some((2, 5)));
        a.move_home(true);
        assert_eq!(a.selection(), Some((0, 2)));
    }

    #[test]
    fn word_jumps() {
        let mut a = f("foo bar.baz");
        a.move_word_left(false);
        assert_eq!(a.cursor(), 8);
        a.move_word_left(false);
        assert_eq!(a.cursor(), 4);
        a.move_word_left(false);
        assert_eq!(a.cursor(), 0);
        a.move_word_right(false);
        assert_eq!(a.cursor(), 3);
        a.move_word_right(false);
        assert_eq!(a.cursor(), 7);
        a.move_word_right(false);
        assert_eq!(a.cursor(), 11);
        a.move_word_right(false);
        assert_eq!(a.cursor(), 11);
    }

    #[test]
    fn word_selection() {
        let mut a = f("foo bar");
        a.move_word_left(true);
        assert_eq!(a.selected_text().as_deref(), Some("bar"));
    }

    #[test]
    fn alt_backspace_deletes_word() {
        let mut a = f("foo bar");
        a.delete_word_back();
        assert_eq!(a.text(), "foo ");
        a.delete_word_back();
        assert_eq!(a.text(), "");
        a.delete_word_back();
        assert_eq!(a.text(), "");
    }

    #[test]
    fn delete_word_back_with_selection_deletes_selection_only() {
        let mut a = f("foo bar");
        a.move_left(true);
        a.delete_word_back();
        assert_eq!(a.text(), "foo ba");
    }

    #[test]
    fn cmd_backspace_deletes_to_start() {
        let mut a = f("foo bar");
        a.set_cursor(4, false);
        a.delete_to_start();
        assert_eq!(a.text(), "bar");
        assert_eq!(a.cursor(), 0);
    }

    #[test]
    fn cut_returns_selection() {
        let mut a = f("hello world");
        a.set_cursor(5, false);
        a.move_home(true);
        assert_eq!(a.cut().as_deref(), Some("hello"));
        assert_eq!(a.text(), " world");
        assert_eq!(a.cut(), None);
    }

    #[test]
    fn set_cursor_clamps() {
        let mut a = f("abc");
        a.set_cursor(99, false);
        assert_eq!(a.cursor(), 3);
        a.move_right(false);
        assert_eq!(a.cursor(), 3);
    }

    #[test]
    fn scroll_keeps_cursor_visible() {
        let mut a = f("0123456789");
        let (s, e) = a.visible_range(4);
        assert_eq!((s, e), (7, 10));
        assert!(a.cursor() - s < 4);
        a.move_home(false);
        assert_eq!(a.visible_range(4), (0, 4));
        a.set_cursor(6, false);
        let (s, _) = a.visible_range(4);
        assert!(a.cursor() >= s && a.cursor() < s + 4);
    }

    #[test]
    fn pos_from_col_uses_scroll() {
        let mut a = f("0123456789");
        a.visible_range(4);
        assert_eq!(a.pos_from_col(0), a.scroll());
        assert_eq!(a.pos_from_col(100), 10);
    }

    #[test]
    fn drag_select_by_extending() {
        let mut a = f("hello world");
        a.set_cursor(2, false);
        a.set_cursor(6, true);
        a.set_cursor(8, true);
        assert_eq!(a.selection(), Some((2, 8)));
        a.set_cursor(2, true);
        assert_eq!(a.selection(), None);
    }
}
