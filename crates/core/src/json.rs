//! Minimal JSON: parse / serialize without any dependency.
//!
//! Objects keep insertion order (we never rely on key order semantics,
//! but stable output makes tests and logs readable).

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

#[derive(Debug, Clone)]
pub struct JsonError {
    pub message: String,
    pub position: usize,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JSON error at {}: {}", self.position, self.message)
    }
}

impl Json {
    // ---- constructors ----
    pub fn str(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }

    pub fn num(n: f64) -> Json {
        Json::Num(n)
    }

    // ---- accessors ----
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_obj(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Obj(entries) => Some(entries),
            _ => None,
        }
    }

    /// Convenience: `j["a"]["b"].as_str()`.
    pub fn index(&self, key: &str) -> Option<&Json> {
        self.get(key)
    }

    // ---- parse ----
    pub fn parse(input: &str) -> Result<Json, JsonError> {
        let mut p = Parser {
            bytes: input.as_bytes(),
            pos: 0,
        };
        p.skip_ws();
        let value = p.parse_value()?;
        p.skip_ws();
        if p.pos != p.bytes.len() {
            return Err(p.err("trailing characters after JSON value"));
        }
        Ok(value)
    }

    // ---- serialize ----
    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Num(n) => {
                if !n.is_finite() {
                    // JSON has no NaN/Infinity - keep the output valid
                    out.push_str("null");
                } else if n.fract() == 0.0 && n.abs() < 1e15 {
                    out.push_str(&format!("{}", *n as i64));
                } else {
                    out.push_str(&format!("{n}"));
                }
            }
            Json::Str(s) => write_escaped(s, out),
            Json::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Obj(entries) => {
                out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_escaped(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

impl std::fmt::Display for Json {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = String::new();
        self.write(&mut out);
        f.write_str(&out)
    }
}

pub fn write_escaped(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn err(&self, msg: &str) -> JsonError {
        JsonError {
            message: msg.to_string(),
            position: self.pos,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(
            self.peek(),
            Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
        ) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, b: u8) -> Result<(), JsonError> {
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected '{}'", b as char)))
        }
    }

    fn parse_value(&mut self) -> Result<Json, JsonError> {
        match self.peek() {
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => Ok(Json::Str(self.parse_string()?)),
            Some(b't') => self.parse_literal("true", Json::Bool(true)),
            Some(b'f') => self.parse_literal("false", Json::Bool(false)),
            Some(b'n') => self.parse_literal("null", Json::Null),
            Some(_) => self.parse_number(),
            None => Err(self.err("unexpected end of input")),
        }
    }

    fn parse_literal(&mut self, word: &str, value: Json) -> Result<Json, JsonError> {
        if self.bytes[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(self.err("invalid literal"))
        }
    }

    fn parse_number(&mut self) -> Result<Json, JsonError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(
            self.peek(),
            Some(b'0'..=b'9') | Some(b'.') | Some(b'e') | Some(b'E') | Some(b'+') | Some(b'-')
        ) {
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| self.err("invalid number bytes"))?;
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| self.err("invalid number"))
    }

    fn parse_string(&mut self) -> Result<String, JsonError> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let esc = self.peek().ok_or_else(|| self.err("bad escape"))?;
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let code = self.parse_hex4()?;
                            if (0xD800..=0xDBFF).contains(&code) {
                                // surrogate pair
                                if self.peek() == Some(b'\\') {
                                    self.pos += 1;
                                    self.expect(b'u')?;
                                    let low = self.parse_hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&low) {
                                        return Err(self.err("invalid low surrogate"));
                                    }
                                    let c = 0x10000
                                        + ((code - 0xD800) as u32) * 0x400
                                        + (low - 0xDC00) as u32;
                                    out.push(
                                        char::from_u32(c)
                                            .ok_or_else(|| self.err("bad codepoint"))?,
                                    );
                                } else {
                                    return Err(self.err("lonely high surrogate"));
                                }
                            } else {
                                out.push(
                                    char::from_u32(code as u32)
                                        .ok_or_else(|| self.err("bad codepoint"))?,
                                );
                            }
                        }
                        _ => return Err(self.err("unknown escape")),
                    }
                }
                Some(_) => {
                    // copy one UTF-8 char directly
                    let start = self.pos;
                    self.pos += 1;
                    while self.peek().is_some_and(|b| (b & 0xC0) == 0x80) {
                        self.pos += 1;
                    }
                    out.push_str(
                        std::str::from_utf8(&self.bytes[start..self.pos])
                            .map_err(|_| self.err("invalid UTF-8 in string"))?,
                    );
                }
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u16, JsonError> {
        if self.pos + 4 > self.bytes.len() {
            return Err(self.err("bad \\u escape"));
        }
        let text = std::str::from_utf8(&self.bytes[self.pos..self.pos + 4])
            .map_err(|_| self.err("bad \\u escape"))?;
        let value = u16::from_str_radix(text, 16).map_err(|_| self.err("bad \\u escape"))?;
        self.pos += 4;
        Ok(value)
    }

    fn parse_array(&mut self) -> Result<Json, JsonError> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            self.skip_ws();
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Arr(items));
                }
                _ => return Err(self.err("expected ',' or ']'")),
            }
        }
    }

    fn parse_object(&mut self) -> Result<Json, JsonError> {
        self.expect(b'{')?;
        let mut entries = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Obj(entries));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            let value = self.parse_value()?;
            entries.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Obj(entries));
                }
                _ => return Err(self.err("expected ',' or '}'")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_basic() {
        let src =
            r#"{"name":"qwen2.5-coder:7b","size":123,"ok":true,"n":null,"xs":[1,2,{"a":"ب"}]}"#;
        let v = Json::parse(src).unwrap();
        assert_eq!(v.get("name").unwrap().as_str(), Some("qwen2.5-coder:7b"));
        assert_eq!(v.get("ok").unwrap().as_bool(), Some(true));
        assert_eq!(v.get("n").unwrap(), &Json::Null);
        let xs = v.get("xs").unwrap().as_arr().unwrap();
        assert_eq!(xs.len(), 3);
        assert_eq!(xs[2].get("a").unwrap().as_str(), Some("ب"));
        let out = v.to_string();
        assert_eq!(Json::parse(&out).unwrap(), v);
    }

    #[test]
    fn escapes() {
        let v = Json::parse(r#""line\nquote\"u:\u0041\u0633""#).unwrap();
        assert_eq!(v.as_str(), Some("line\nquote\"u:Aس"));
    }

    #[test]
    fn surrogate_pair() {
        let v = Json::parse(r#""\ud83d\ude00""#).unwrap();
        assert_eq!(v.as_str(), Some("😀"));
    }

    #[test]
    fn numbers() {
        assert_eq!(Json::parse("-2.5e3").unwrap(), Json::Num(-2500.0));
        assert_eq!(Json::parse("42").unwrap().as_f64(), Some(42.0));
    }

    #[test]
    fn rejects_trailing_garbage() {
        assert!(Json::parse("{} x").is_err());
        assert!(Json::parse("{").is_err());
        assert!(Json::parse("").is_err());
    }

    #[test]
    fn esc_roundtrip_control() {
        let s = "a\u{01}b\n\t\"";
        let mut out = String::new();
        write_escaped(s, &mut out);
        let v = Json::parse(&out).unwrap();
        assert_eq!(v.as_str(), Some(s));
    }
}
