// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com> This program is free
// software: you can redistribute it and/or modify it under the terms of the GNU
// Affero General Public License as published by the Free Software Foundation,
// version 3.
//
// This program is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
// FOR A PARTICULAR PURPOSE. See the GNU Affero General Public License for more
// details.
//
// You should have received a copy of the GNU Affero General Public License along
// with this program. If not, see <https://www.gnu.org/licenses/>.

//! Shared JSON-to-Value and Value-to-JSON conversion.
//!
//! Both the runtime JSON builtins and the daemon HTTP conversion use these
//! functions so that number classification and float narrowing are consistent
//! across all entry points.

use mica_var::{Symbol, Value, ValueKind};

/// Mica integer range boundaries for JSON number classification.
const INT_MIN: i64 = -(1i64 << 55);
const INT_MAX: i64 = (1i64 << 55) - 1;

/// Error from JSON-to-Value conversion.
#[derive(Debug, Clone)]
pub struct JsonValueError(pub String);

impl std::fmt::Display for JsonValueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for JsonValueError {}

/// Converts a Mica `Value` to a `serde_json::Value`.
///
/// Float values are widened to `f64` at this boundary because JSON has no
/// binary32 type. This is a permanent boundary conversion, not an adapter.
pub fn json_from_value(value: &Value) -> Result<serde_json::Value, JsonValueError> {
    if is_json_null(value) {
        return Ok(serde_json::Value::Null);
    }
    match value.kind() {
        ValueKind::Bool => Ok(serde_json::Value::Bool(value.as_bool().unwrap())),
        ValueKind::Int => Ok(serde_json::Value::Number(value.as_int().unwrap().into())),
        ValueKind::Float => {
            let f = value.as_float().unwrap() as f64;
            serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .ok_or_else(|| JsonValueError("non-finite float cannot be encoded as JSON".into()))
        }
        ValueKind::String => Ok(serde_json::Value::String(
            value.with_str(str::to_owned).unwrap(),
        )),
        ValueKind::Symbol => {
            let name =
                value.as_symbol().unwrap().name().ok_or_else(|| {
                    JsonValueError("unnamed symbol cannot be encoded as JSON".into())
                })?;
            Ok(serde_json::Value::String(name.to_string()))
        }
        ValueKind::List => {
            let values = value
                .with_list(|values| {
                    values
                        .iter()
                        .map(json_from_value)
                        .collect::<Result<Vec<_>, _>>()
                })
                .ok_or_else(|| JsonValueError("expected list value".into()))??;
            Ok(serde_json::Value::Array(values))
        }
        ValueKind::Map => value
            .with_map(|entries| {
                let mut object = serde_json::Map::new();
                for (key, val) in entries {
                    let key_str = match key.kind() {
                        ValueKind::String => key.with_str(str::to_owned).unwrap(),
                        ValueKind::Symbol => {
                            let name = key.as_symbol().unwrap().name().ok_or_else(|| {
                                JsonValueError(
                                    "unnamed symbol key cannot be encoded as JSON".into(),
                                )
                            })?;
                            name.to_string()
                        }
                        _ => {
                            return Err(JsonValueError(format!(
                                "unsupported JSON key kind {:?}",
                                key.kind()
                            )));
                        }
                    };
                    object.insert(key_str, json_from_value(val)?);
                }
                Ok(serde_json::Value::Object(object))
            })
            .ok_or_else(|| JsonValueError("expected map value".into()))?,
        _ => Err(JsonValueError(format!(
            "cannot encode {:?} value as JSON",
            value.kind()
        ))),
    }
}

/// Converts JSON source text to a Mica `Value`.
///
/// Number classification uses the original JSON token. A token without a
/// decimal point or exponent is always an integer and is rejected outside
/// Mica's 56-bit integer range. A token containing a decimal point or exponent
/// is a float and must narrow to a finite binary32 value.
pub fn value_from_json_text(text: &str) -> Result<Value, JsonValueError> {
    let mut parser = JsonParser::new(text);
    let value = parser.parse_value()?;
    parser.skip_whitespace();
    if parser.peek().is_some() {
        return Err(parser.error("trailing data after JSON value"));
    }
    Ok(value)
}

/// Converts a programmatically constructed JSON tree to a Mica `Value`.
///
/// A `serde_json::Value` does not retain the original spelling of every
/// number, so raw HTTP and builtin JSON input must use [`value_from_json_text`]
/// instead. This helper serializes the tree and applies the same current JSON
/// syntax policy to that generated representation.
pub fn value_from_json(value: &serde_json::Value) -> Result<Value, JsonValueError> {
    let text = serde_json::to_string(value)
        .map_err(|error| JsonValueError(format!("failed to serialize JSON value: {error}")))?;
    value_from_json_text(&text)
}

struct JsonParser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            pos: 0,
        }
    }

    fn parse_value(&mut self) -> Result<Value, JsonValueError> {
        self.skip_whitespace();
        match self.peek() {
            Some(b'n') => {
                self.expect_bytes(b"null")?;
                Ok(json_null_value())
            }
            Some(b't') => {
                self.expect_bytes(b"true")?;
                Ok(Value::bool(true))
            }
            Some(b'f') => {
                self.expect_bytes(b"false")?;
                Ok(Value::bool(false))
            }
            Some(b'"') => self.parse_string().map(Value::string),
            Some(b'[') => self.parse_array(),
            Some(b'{') => self.parse_object(),
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            Some(_) => Err(self.error("expected JSON value")),
            None => Err(self.error("expected JSON value")),
        }
    }

    fn parse_array(&mut self) -> Result<Value, JsonValueError> {
        self.bump();
        self.skip_whitespace();
        let mut values = Vec::new();
        if self.consume(b']') {
            return Ok(Value::list(values));
        }
        loop {
            values.push(self.parse_value()?);
            self.skip_whitespace();
            if self.consume(b']') {
                return Ok(Value::list(values));
            }
            self.expect(b',')?;
            self.skip_whitespace();
        }
    }

    fn parse_object(&mut self) -> Result<Value, JsonValueError> {
        self.bump();
        self.skip_whitespace();
        let mut entries = Vec::new();
        if self.consume(b'}') {
            return Ok(Value::map(entries));
        }
        loop {
            if self.peek() != Some(b'"') {
                return Err(self.error("expected JSON object key"));
            }
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            let value = self.parse_value()?;
            entries.push((Value::symbol(Symbol::intern(&key)), value));
            self.skip_whitespace();
            if self.consume(b'}') {
                return Ok(Value::map(entries));
            }
            self.expect(b',')?;
            self.skip_whitespace();
        }
    }

    fn parse_string(&mut self) -> Result<String, JsonValueError> {
        let start = self.pos;
        self.expect(b'"')?;
        loop {
            match self.bump() {
                Some(b'"') => break,
                Some(b'\\') => {
                    if self.bump().is_none() {
                        return Err(self.error("unterminated JSON string escape"));
                    }
                }
                Some(0..=0x1f) => return Err(self.error("control character in JSON string")),
                Some(_) => {}
                None => return Err(self.error("unterminated JSON string")),
            }
        }
        serde_json::from_str(&self.text[start..self.pos])
            .map_err(|error| self.error(format!("invalid JSON string: {error}")))
    }

    fn parse_number(&mut self) -> Result<Value, JsonValueError> {
        let start = self.pos;
        self.consume(b'-');
        match self.bump() {
            Some(b'0') => {
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(self.error("leading zero in JSON number"));
                }
            }
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.bump();
                }
            }
            _ => return Err(self.error("invalid JSON number")),
        }

        let mut is_float = false;
        if self.consume(b'.') {
            is_float = true;
            self.consume_digits("expected digit after decimal point")?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.bump();
            if !self.consume(b'+') {
                let _ = self.consume(b'-');
            }
            self.consume_digits("expected exponent digits")?;
        }

        let token = &self.text[start..self.pos];
        if !is_float {
            let integer = token.parse::<i64>().map_err(|_| {
                self.error(format!(
                    "JSON integer {token} is outside the Mica integer range"
                ))
            })?;
            if !(INT_MIN..=INT_MAX).contains(&integer) {
                return Err(self.error(format!(
                    "JSON integer {token} is outside the Mica integer range"
                )));
            }
            return Value::int(integer)
                .map_err(|error| self.error(format!("invalid JSON integer {token}: {error:?}")));
        }

        let float = token
            .parse::<f64>()
            .map_err(|_| self.error(format!("invalid JSON float {token}")))?;
        if !float.is_finite() {
            return Err(self.error(format!("JSON float {token} is not finite")));
        }
        Value::float(float as f32)
            .map_err(|_| self.error(format!("JSON float {token} overflows binary32")))
    }

    fn consume_digits(&mut self, message: &str) -> Result<(), JsonValueError> {
        if !matches!(self.peek(), Some(b'0'..=b'9')) {
            return Err(self.error(message));
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.bump();
        }
        Ok(())
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.bump();
        }
    }

    fn expect_bytes(&mut self, expected: &[u8]) -> Result<(), JsonValueError> {
        for byte in expected {
            if self.bump() != Some(*byte) {
                return Err(self.error("invalid JSON literal"));
            }
        }
        Ok(())
    }

    fn expect(&mut self, expected: u8) -> Result<(), JsonValueError> {
        if self.consume(expected) {
            Ok(())
        } else {
            Err(self.error(format!("expected {:?}", expected as char)))
        }
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn error(&self, message: impl std::fmt::Display) -> JsonValueError {
        JsonValueError(format!("{message} at byte {}", self.pos))
    }
}

pub fn json_null_value() -> Value {
    Value::map([(
        Value::symbol(Symbol::intern("json")),
        Value::symbol(Symbol::intern("null")),
    )])
}

pub fn is_json_null(value: &Value) -> bool {
    value
        .with_map(|entries| {
            entries
                == [(
                    Value::symbol(Symbol::intern("json")),
                    Value::symbol(Symbol::intern("null")),
                )]
        })
        .unwrap_or(false)
}

/// Renders a finite binary32 float as a source literal that round-trips to the
/// same binary32 bits.
pub fn float_to_literal(value: f32) -> String {
    if value == 0.0 {
        return "0.0".to_string();
    }
    let rendered = format!("{:e}", value);
    if rendered.contains('.') || rendered.contains('e') {
        rendered
    } else {
        format!("{rendered}.0")
    }
}

#[cfg(test)]
mod tests {
    use super::{json_null_value, value_from_json_text};
    use mica_var::{Symbol, Value};

    #[test]
    fn preserves_json_number_token_kinds() {
        assert_eq!(value_from_json_text("1").unwrap(), Value::int(1).unwrap());
        assert_eq!(
            value_from_json_text("1.0").unwrap(),
            Value::float(1.0).unwrap()
        );
        assert_eq!(
            value_from_json_text("1e0").unwrap(),
            Value::float(1.0).unwrap()
        );
        assert_eq!(value_from_json_text("-0").unwrap(), Value::int(0).unwrap());
        assert_eq!(
            value_from_json_text("-0.0").unwrap(),
            Value::float(0.0).unwrap()
        );
    }

    #[test]
    fn rejects_oversized_lexical_json_integers() {
        for input in [
            "36028797018963968",
            "18446744073709551616",
            "-36028797018963969",
        ] {
            assert!(value_from_json_text(input).is_err(), "{input}");
        }
    }

    #[test]
    fn converts_nested_json_values() {
        assert_eq!(
            value_from_json_text(r#"{"value":[1,1.0,true,null]}"#).unwrap(),
            Value::map([(
                Value::symbol(Symbol::intern("value")),
                Value::list([
                    Value::int(1).unwrap(),
                    Value::float(1.0).unwrap(),
                    Value::bool(true),
                    json_null_value(),
                ]),
            )])
        );
    }
}
