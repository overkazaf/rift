//! Minimal single-line text editor for the inline prompt popover.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LineEdit {
    text: Vec<char>,
    cursor: usize,
}

impl LineEdit {
    pub fn with_text(s: &str) -> Self {
        let mut e = Self::default();
        e.insert_str(s);
        e
    }

    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Insert at the cursor; control characters are dropped, newlines become spaces.
    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            let c = if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c };
            if c.is_control() {
                continue;
            }
            self.text.insert(self.cursor, c);
            self.cursor += 1;
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.text.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.len());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Ctrl+U: delete everything before the cursor.
    pub fn kill_to_start(&mut self) {
        self.text.drain(..self.cursor);
        self.cursor = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_at_cursor() {
        let mut e = LineEdit::with_text("helo");
        e.left();
        e.insert_str("l");
        assert_eq!(e.text(), "hello");
        e.home();
        e.delete();
        assert_eq!(e.text(), "ello");
        e.end();
        e.backspace();
        assert_eq!(e.text(), "ell");
        e.kill_to_start();
        assert!(e.is_empty());
    }

    #[test]
    fn sanitizes_input_and_handles_wide_chars() {
        let mut e = LineEdit::default();
        e.insert_str("a\nb\u{7}c\t");
        assert_eq!(e.text(), "a bc ");
        let mut e = LineEdit::with_text("你好");
        e.left();
        e.backspace();
        assert_eq!(e.text(), "好");
        assert_eq!(e.cursor(), 0);
    }
}
