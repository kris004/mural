use std::collections::BTreeMap;

use crate::ProtocolError;

const MAX_JSON_NESTING_DEPTH: usize = 128;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum JsonValue {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<JsonValue>),
    Object(BTreeMap<String, JsonValue>),
}

impl JsonValue {
    pub(crate) fn as_object(&self) -> Result<&BTreeMap<String, JsonValue>, ProtocolError> {
        match self {
            Self::Object(object) => Ok(object),
            _ => Err(ProtocolError::new("expected JSON object")),
        }
    }

    pub(crate) fn into_object(self) -> Result<BTreeMap<String, JsonValue>, ProtocolError> {
        match self {
            Self::Object(object) => Ok(object),
            _ => Err(ProtocolError::new("expected JSON object")),
        }
    }

    pub(crate) fn as_string(&self) -> Result<&str, ProtocolError> {
        match self {
            Self::String(string) => Ok(string),
            _ => Err(ProtocolError::new("expected JSON string")),
        }
    }

    pub(crate) fn into_string(self) -> Result<String, ProtocolError> {
        match self {
            Self::String(string) => Ok(string),
            _ => Err(ProtocolError::new("expected JSON string")),
        }
    }

    pub(crate) fn as_bool(&self) -> Result<bool, ProtocolError> {
        match self {
            Self::Bool(value) => Ok(*value),
            _ => Err(ProtocolError::new("expected JSON boolean")),
        }
    }
}

pub(crate) fn parse_json(input: &str) -> Result<JsonValue, ProtocolError> {
    let mut parser = JsonParser::new(input);
    let value = parser.parse_value(0)?;
    parser.skip_whitespace();
    if parser.is_done() {
        Ok(value)
    } else {
        Err(ProtocolError::new(
            "unexpected trailing data after JSON value",
        ))
    }
}

#[derive(Clone, Debug)]
struct JsonParser<'a> {
    input: &'a str,
    position: usize,
}

impl<'a> JsonParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, position: 0 }
    }

    fn parse_value(&mut self, depth: usize) -> Result<JsonValue, ProtocolError> {
        self.skip_whitespace();
        match self.peek() {
            Some(b'n') => self.parse_literal("null", JsonValue::Null),
            Some(b't') => self.parse_literal("true", JsonValue::Bool(true)),
            Some(b'f') => self.parse_literal("false", JsonValue::Bool(false)),
            Some(b'\"') => self.parse_string().map(JsonValue::String),
            Some(b'{') => {
                Self::check_nesting_depth(depth)?;
                self.parse_object(depth + 1).map(JsonValue::Object)
            }
            Some(b'[') => {
                Self::check_nesting_depth(depth)?;
                self.parse_array(depth + 1).map(JsonValue::Array)
            }
            Some(b'-' | b'0'..=b'9') => self.parse_number().map(JsonValue::Number),
            Some(byte) => Err(ProtocolError::new(format!(
                "unexpected byte in JSON value: 0x{byte:02x}"
            ))),
            None => Err(ProtocolError::new("unexpected end of JSON input")),
        }
    }

    fn check_nesting_depth(depth: usize) -> Result<(), ProtocolError> {
        if depth >= MAX_JSON_NESTING_DEPTH {
            return Err(ProtocolError::new(format!(
                "JSON nesting exceeds the maximum depth of {MAX_JSON_NESTING_DEPTH}"
            )));
        }
        Ok(())
    }

    fn parse_literal(
        &mut self,
        literal: &str,
        value: JsonValue,
    ) -> Result<JsonValue, ProtocolError> {
        if self.input[self.position..].starts_with(literal) {
            self.position += literal.len();
            Ok(value)
        } else {
            Err(ProtocolError::new(format!("expected literal {literal}")))
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<BTreeMap<String, JsonValue>, ProtocolError> {
        self.expect(b'{')?;
        self.skip_whitespace();

        let mut object = BTreeMap::new();
        if self.consume_if(b'}') {
            return Ok(object);
        }

        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            let value = self.parse_value(depth)?;
            object.insert(key, value);
            self.skip_whitespace();

            if self.consume_if(b'}') {
                return Ok(object);
            }
            self.expect(b',')?;
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<Vec<JsonValue>, ProtocolError> {
        self.expect(b'[')?;
        self.skip_whitespace();

        let mut array = Vec::new();
        if self.consume_if(b']') {
            return Ok(array);
        }

        loop {
            array.push(self.parse_value(depth)?);
            self.skip_whitespace();
            if self.consume_if(b']') {
                return Ok(array);
            }
            self.expect(b',')?;
        }
    }

    fn parse_string(&mut self) -> Result<String, ProtocolError> {
        self.expect(b'\"')?;
        let mut output = String::new();

        while let Some(byte) = self.next() {
            match byte {
                b'\"' => return Ok(output),
                b'\\' => output.push(self.parse_escape()?),
                0x00..=0x1f => {
                    return Err(ProtocolError::new("unescaped control byte in JSON string"));
                }
                _ => {
                    let character = self.decode_utf8_at(byte)?;
                    output.push(character);
                }
            }
        }

        Err(ProtocolError::new("unterminated JSON string"))
    }

    fn parse_escape(&mut self) -> Result<char, ProtocolError> {
        match self.next() {
            Some(b'\"') => Ok('\"'),
            Some(b'\\') => Ok('\\'),
            Some(b'/') => Ok('/'),
            Some(b'b') => Ok('\u{08}'),
            Some(b'f') => Ok('\u{0c}'),
            Some(b'n') => Ok('\n'),
            Some(b'r') => Ok('\r'),
            Some(b't') => Ok('\t'),
            Some(b'u') => self.parse_unicode_escape(),
            Some(byte) => Err(ProtocolError::new(format!(
                "invalid JSON string escape: 0x{byte:02x}"
            ))),
            None => Err(ProtocolError::new("unterminated JSON string escape")),
        }
    }

    fn parse_unicode_escape(&mut self) -> Result<char, ProtocolError> {
        let first = self.parse_unicode_code_unit()?;
        let value = match first {
            0xd800..=0xdbff => {
                if self.next() != Some(b'\\') || self.next() != Some(b'u') {
                    return Err(ProtocolError::new(
                        "high surrogate must be followed by a unicode low surrogate",
                    ));
                }
                let second = self.parse_unicode_code_unit()?;
                if !(0xdc00..=0xdfff).contains(&second) {
                    return Err(ProtocolError::new(
                        "high surrogate must be followed by a unicode low surrogate",
                    ));
                }
                0x1_0000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00)
            }
            0xdc00..=0xdfff => {
                return Err(ProtocolError::new(
                    "unicode low surrogate is missing a leading high surrogate",
                ));
            }
            _ => u32::from(first),
        };

        char::from_u32(value).ok_or_else(|| ProtocolError::new("invalid unicode scalar value"))
    }

    fn parse_unicode_code_unit(&mut self) -> Result<u16, ProtocolError> {
        let mut value = 0_u16;
        for _ in 0..4 {
            let byte = self
                .next()
                .ok_or_else(|| ProtocolError::new("unterminated unicode escape"))?;
            value = (value << 4)
                | match byte {
                    b'0'..=b'9' => u16::from(byte - b'0'),
                    b'a'..=b'f' => u16::from(byte - b'a' + 10),
                    b'A'..=b'F' => u16::from(byte - b'A' + 10),
                    _ => return Err(ProtocolError::new("invalid unicode escape digit")),
                };
        }
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<String, ProtocolError> {
        let start = self.position;
        if self.consume_if(b'-') && !matches!(self.peek(), Some(b'0'..=b'9')) {
            return Err(ProtocolError::new("expected digit after minus sign"));
        }

        match self.peek() {
            Some(b'0') => {
                self.position += 1;
            }
            Some(b'1'..=b'9') => {
                self.position += 1;
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.position += 1;
                }
            }
            _ => return Err(ProtocolError::new("expected JSON number")),
        }

        if self.consume_if(b'.') {
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(ProtocolError::new("expected digit after decimal point"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.position += 1;
            }
        }

        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(ProtocolError::new("expected digit in exponent"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.position += 1;
            }
        }

        Ok(self.input[start..self.position].to_owned())
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.position += 1;
        }
    }

    fn expect(&mut self, expected: u8) -> Result<(), ProtocolError> {
        match self.next() {
            Some(actual) if actual == expected => Ok(()),
            Some(actual) => Err(ProtocolError::new(format!(
                "expected byte 0x{expected:02x}, got 0x{actual:02x}"
            ))),
            None => Err(ProtocolError::new(format!(
                "expected byte 0x{expected:02x}, got end of input"
            ))),
        }
    }

    fn consume_if(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.as_bytes().get(self.position).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.position += 1;
        Some(byte)
    }

    fn is_done(&self) -> bool {
        self.position == self.input.len()
    }

    fn decode_utf8_at(&mut self, first_byte: u8) -> Result<char, ProtocolError> {
        let width = utf8_width(first_byte)
            .ok_or_else(|| ProtocolError::new("invalid UTF-8 byte in JSON string"))?;
        if width == 1 {
            return Ok(char::from(first_byte));
        }

        let start = self.position - 1;
        let end = start + width;
        if end > self.input.len() {
            return Err(ProtocolError::new(
                "truncated UTF-8 sequence in JSON string",
            ));
        }

        self.position = end;
        self.input[start..end]
            .chars()
            .next()
            .ok_or_else(|| ProtocolError::new("invalid UTF-8 sequence in JSON string"))
    }
}

fn utf8_width(first_byte: u8) -> Option<usize> {
    match first_byte {
        0x00..=0x7f => Some(1),
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_JSON_NESTING_DEPTH, parse_json};

    fn nested_array(depth: usize) -> String {
        format!("{}null{}", "[".repeat(depth), "]".repeat(depth))
    }

    #[test]
    fn accepts_json_at_maximum_nesting_depth() {
        assert!(parse_json(&nested_array(MAX_JSON_NESTING_DEPTH)).is_ok());
    }

    #[test]
    fn rejects_json_beyond_maximum_nesting_depth() {
        let error = parse_json(&nested_array(MAX_JSON_NESTING_DEPTH + 1)).unwrap_err();

        assert_eq!(
            error.to_string(),
            format!("JSON nesting exceeds the maximum depth of {MAX_JSON_NESTING_DEPTH}")
        );
    }
}
