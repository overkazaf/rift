//! Tiny, tolerant JSON-object extractor for LLM replies.
//!
//! Models wrap JSON in code fences or prose despite instructions, so this
//! scans for the first `{` that starts a well-formed object and parses only
//! that (flat string / null / bool / number values; nested values are skipped).
//! There is no serde dependency in this crate, and replies are tiny.

#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    /// Nested object / array (skipped, contents not retained).
    Other,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Object(pub Vec<(String, Val)>);

impl Object {
    pub fn get(&self, key: &str) -> Option<&Val> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// String value for `key`; `None` when missing, null or not a string.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Val::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// First well-formed JSON object found anywhere in `text`.
pub fn parse_first_object(text: &str) -> Option<Object> {
    let chars: Vec<char> = text.chars().collect();
    let mut start = 0;
    while start < chars.len() {
        if chars[start] == '{' {
            let mut p = Parser { c: &chars, i: start };
            if let Some(obj) = p.object() {
                return Some(obj);
            }
        }
        start += 1;
    }
    None
}

struct Parser<'a> {
    c: &'a [char],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.c.len() && self.c[self.i].is_whitespace() {
            self.i += 1;
        }
    }

    fn eat(&mut self, ch: char) -> bool {
        self.ws();
        if self.c.get(self.i) == Some(&ch) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn object(&mut self) -> Option<Object> {
        if !self.eat('{') {
            return None;
        }
        let mut out = Vec::new();
        if self.eat('}') {
            return Some(Object(out));
        }
        loop {
            self.ws();
            let key = self.string()?;
            if !self.eat(':') {
                return None;
            }
            let val = self.value()?;
            out.push((key, val));
            if self.eat(',') {
                continue;
            }
            if self.eat('}') {
                return Some(Object(out));
            }
            return None;
        }
    }

    fn value(&mut self) -> Option<Val> {
        self.ws();
        match *self.c.get(self.i)? {
            '"' => self.string().map(Val::Str),
            '{' => {
                self.object()?;
                Some(Val::Other)
            }
            '[' => self.skip_array().map(|_| Val::Other),
            _ => self.literal(),
        }
    }

    fn skip_array(&mut self) -> Option<()> {
        self.i += 1; // '['
        if self.eat(']') {
            return Some(());
        }
        loop {
            self.value()?;
            if self.eat(',') {
                continue;
            }
            if self.eat(']') {
                return Some(());
            }
            return None;
        }
    }

    fn literal(&mut self) -> Option<Val> {
        let start = self.i;
        while self.i < self.c.len() && !matches!(self.c[self.i], ',' | '}' | ']') && !self.c[self.i].is_whitespace() {
            self.i += 1;
        }
        let word: String = self.c[start..self.i].iter().collect();
        match word.as_str() {
            "null" => Some(Val::Null),
            "true" => Some(Val::Bool(true)),
            "false" => Some(Val::Bool(false)),
            w => w.parse::<f64>().ok().map(Val::Num),
        }
    }

    fn string(&mut self) -> Option<String> {
        if self.c.get(self.i) != Some(&'"') {
            return None;
        }
        self.i += 1;
        let mut s = String::new();
        loop {
            let ch = *self.c.get(self.i)?;
            self.i += 1;
            match ch {
                '"' => return Some(s),
                '\\' => {
                    let e = *self.c.get(self.i)?;
                    self.i += 1;
                    match e {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'r' => s.push('\r'),
                        'b' => s.push('\u{8}'),
                        'f' => s.push('\u{c}'),
                        '/' => s.push('/'),
                        '\\' => s.push('\\'),
                        '"' => s.push('"'),
                        'u' => {
                            let hi = self.hex4()?;
                            if (0xD800..0xDC00).contains(&hi)
                                && self.c.get(self.i) == Some(&'\\')
                                && self.c.get(self.i + 1) == Some(&'u')
                            {
                                self.i += 2;
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                                    s.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                                } else {
                                    // High surrogate not followed by a low one: replacement char, keep the second escape.
                                    s.push('\u{FFFD}');
                                    s.push(char::from_u32(lo).unwrap_or('\u{FFFD}'));
                                }
                            } else {
                                s.push(char::from_u32(hi).unwrap_or('\u{FFFD}'));
                            }
                        }
                        other => s.push(other),
                    }
                }
                c => s.push(c),
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..4 {
            let d = self.c.get(self.i)?.to_digit(16)?;
            v = v * 16 + d;
            self.i += 1;
        }
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_object() {
        let o = parse_first_object(r#"{"command": "ls -la", "explanation": "list"}"#).unwrap();
        assert_eq!(o.get_str("command"), Some("ls -la"));
        assert_eq!(o.get_str("explanation"), Some("list"));
    }

    #[test]
    fn tolerates_fences_and_prose() {
        let raw = "Sure! Here you go:\n```json\n{\"command\": \"npm install\", \"explanation\": \"missing deps\"}\n```\nHope it helps {really}.";
        let o = parse_first_object(raw).unwrap();
        assert_eq!(o.get_str("command"), Some("npm install"));
    }

    #[test]
    fn null_and_missing_are_not_strings() {
        let o = parse_first_object(r#"{"command": null}"#).unwrap();
        assert_eq!(o.get("command"), Some(&Val::Null));
        assert_eq!(o.get_str("command"), None);
        assert_eq!(o.get_str("nope"), None);
    }

    #[test]
    fn handles_escapes_unicode_and_braces_in_strings() {
        let o = parse_first_object(r#"{"command": "echo \"a}b\" é 😀\n", "n": 3}"#).unwrap();
        assert_eq!(o.get_str("command"), Some("echo \"a}b\" é 😀\n"));
        assert_eq!(o.get("n"), Some(&Val::Num(3.0)));
    }

    #[test]
    fn decodes_u_escapes_cjk_emoji_and_bad_surrogates() {
        let o = parse_first_object(r#"{"a": "\u4e2d\u6587 \ud83d\ude00 \u00e9", "b": "\ud83dA", "c": "\ud83d\u0041", "d": "\ude00x"}"#).unwrap();
        assert_eq!(o.get_str("a"), Some("中文 😀 é"));
        assert_eq!(o.get_str("b"), Some("\u{fffd}A"));
        assert_eq!(o.get_str("c"), Some("\u{fffd}A"));
        assert_eq!(o.get_str("d"), Some("\u{fffd}x"));
    }

    #[test]
    fn skips_nested_values() {
        let o = parse_first_object(r#"{"a": {"b": [1, 2, {"c": "}"}]}, "command": "x"}"#).unwrap();
        assert_eq!(o.get("a"), Some(&Val::Other));
        assert_eq!(o.get_str("command"), Some("x"));
    }

    #[test]
    fn skips_malformed_prefix_object() {
        let o = parse_first_object(r#"use {braces} then {"command": "pwd"}"#).unwrap();
        assert_eq!(o.get_str("command"), Some("pwd"));
    }

    #[test]
    fn garbage_is_none() {
        assert!(parse_first_object("no json here").is_none());
        assert!(parse_first_object("{\"command\": ").is_none());
        assert!(parse_first_object("").is_none());
    }
}
