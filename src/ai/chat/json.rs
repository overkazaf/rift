//! Minimal JSON reader/writer for the chat module (the project has no serde).
//!
//! Enough for streaming chunks (`choices[0].delta.content`, Ollama
//! `message.content`) and for round-tripping chat history files.

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn parse(src: &str) -> Option<Json> {
        let mut p = Parser { s: src.as_bytes(), pos: 0 };
        let v = p.value(0)?;
        p.ws();
        (p.pos == p.s.len()).then_some(v)
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(o) => o.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn idx(&self, i: usize) -> Option<&Json> {
        match self {
            Json::Arr(a) => a.get(i),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }
}

/// JSON string body escape (without the surrounding quotes).
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out
}

/// Quoted, escaped JSON string.
pub fn quote(s: &str) -> String {
    format!("\"{}\"", escape(s))
}

const MAX_DEPTH: usize = 64;

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while matches!(self.s.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, b: u8) -> Option<()> {
        (self.s.get(self.pos) == Some(&b)).then(|| self.pos += 1)
    }

    fn lit(&mut self, word: &str, v: Json) -> Option<Json> {
        if self.s[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Some(v)
        } else {
            None
        }
    }

    fn value(&mut self, depth: usize) -> Option<Json> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.ws();
        match *self.s.get(self.pos)? {
            b'{' => {
                self.pos += 1;
                let mut o = Vec::new();
                self.ws();
                if self.eat(b'}').is_some() {
                    return Some(Json::Obj(o));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    self.eat(b':')?;
                    o.push((k, self.value(depth + 1)?));
                    self.ws();
                    match self.s.get(self.pos)? {
                        b',' => self.pos += 1,
                        b'}' => {
                            self.pos += 1;
                            return Some(Json::Obj(o));
                        }
                        _ => return None,
                    }
                }
            }
            b'[' => {
                self.pos += 1;
                let mut a = Vec::new();
                self.ws();
                if self.eat(b']').is_some() {
                    return Some(Json::Arr(a));
                }
                loop {
                    a.push(self.value(depth + 1)?);
                    self.ws();
                    match self.s.get(self.pos)? {
                        b',' => self.pos += 1,
                        b']' => {
                            self.pos += 1;
                            return Some(Json::Arr(a));
                        }
                        _ => return None,
                    }
                }
            }
            b'"' => self.string().map(Json::Str),
            b't' => self.lit("true", Json::Bool(true)),
            b'f' => self.lit("false", Json::Bool(false)),
            b'n' => self.lit("null", Json::Null),
            _ => self.number(),
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = self.s.get(self.pos..self.pos + 4)?;
        let h = std::str::from_utf8(h).ok()?;
        self.pos += 4;
        u32::from_str_radix(h, 16).ok()
    }

    fn string(&mut self) -> Option<String> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let b = *self.s.get(self.pos)?;
            self.pos += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let e = *self.s.get(self.pos)?;
                    self.pos += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi) {
                                // Surrogate pair: expect \uXXXX low half.
                                if self.s.get(self.pos) == Some(&b'\\') && self.s.get(self.pos + 1) == Some(&b'u') {
                                    self.pos += 2;
                                    let lo = self.hex4()?;
                                    if (0xDC00..0xE000).contains(&lo) {
                                        0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                    } else {
                                        0xFFFD
                                    }
                                } else {
                                    0xFFFD
                                }
                            } else {
                                hi
                            };
                            char::from_u32(code).unwrap_or('\u{FFFD}')
                        }
                        _ => return None,
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                b => out.push(b),
            }
        }
        String::from_utf8(out).ok()
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.pos;
        while matches!(self.s.get(self.pos), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
            self.pos += 1;
        }
        if start == self.pos {
            return None;
        }
        std::str::from_utf8(&self.s[start..self.pos]).ok()?.parse().ok().map(Json::Num)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_and_unicode_escapes() {
        let j = Json::parse(r#"{"a":[1,{"b":"x中😀\n"}],"c":null,"d":true}"#).unwrap();
        assert_eq!(j.get("a").unwrap().idx(1).unwrap().get("b").unwrap().as_str(), Some("x中😀\n"));
        assert_eq!(j.get("c"), Some(&Json::Null));
        assert_eq!(j.get("d").unwrap().as_bool(), Some(true));
    }

    #[test]
    fn rejects_garbage_and_trailing() {
        assert!(Json::parse("{\"a\":").is_none());
        assert!(Json::parse("{} x").is_none());
        assert!(Json::parse("").is_none());
    }

    #[test]
    fn escape_round_trips() {
        let s = "a\"b\\c\n\t\u{1}中";
        assert_eq!(Json::parse(&quote(s)).unwrap().as_str(), Some(s));
    }

    #[test]
    fn escape_covers_every_control_char() {
        let all: String = (0u32..0x20).filter_map(char::from_u32).chain(['\u{7f}', '\u{2028}', '\u{2029}']).collect();
        let q = quote(&all);
        assert!(q.chars().all(|c| (c as u32) >= 0x20 && c != '\u{7f}' && c != '\u{2028}' && c != '\u{2029}'), "{q}");
        assert_eq!(Json::parse(&q).unwrap().as_str(), Some(all.as_str()));
    }

    #[test]
    fn cjk_emoji_and_surrogates_decode() {
        let j = Json::parse(r#"{"a":"\u4e2d\u6587 \ud83d\ude00 \u00e9","b":"\ud83d x","c":"direct 中😀"}"#).unwrap();
        assert_eq!(j.get("a").unwrap().as_str(), Some("中文 😀 é"));
        assert_eq!(j.get("b").unwrap().as_str(), Some("\u{fffd} x")); // lone surrogate
        assert_eq!(j.get("c").unwrap().as_str(), Some("direct 中😀"));
        assert_eq!(Json::parse(&quote("中文😀")).unwrap().as_str(), Some("中文😀"));
    }
}
