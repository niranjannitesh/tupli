//! PHP's `serialize()`, read far enough to show what is in a cache entry.
//!
//! Laravel and Symfony both put serialized PHP in Redis by default, so a client
//! that cannot read it shows a wall of `a:3:{s:2:"id";i:7;…}` for a large share
//! of the keys in a real application. The format is simple enough — a type
//! letter, a colon, and a length-prefixed payload — that reading it is cheaper
//! than telling people it cannot be read.
//!
//! Reading only. Writing PHP back is not offered, because a serialized object
//! carries a class name that this crate has no way to validate, and a client
//! that lets you edit one is a client that lets you hand a PHP process an
//! object of a class it did not expect.

use serde_json::{Map, Value};

/// Parse a `serialize()` payload into the JSON model.
///
/// Objects become an object with their class name under `__class`, because the
/// class is the most useful thing in the value and JSON has nowhere else to put
/// it. References (`R` and `r`) become a marker rather than being followed: the
/// graph they describe can be cyclic, and a viewer that followed one could hang
/// on a value that a person only wanted to look at.
pub fn parse(bytes: &[u8]) -> Result<Value, String> {
    let mut parser = Parser { bytes, at: 0 };
    let value = parser.value()?;
    match parser.at == bytes.len() {
        true => Ok(value),
        false => Err(format!(
            "{} trailing bytes after the value",
            bytes.len() - parser.at
        )),
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Parser<'a> {
    fn value(&mut self) -> Result<Value, String> {
        match self.take()? {
            b'N' => {
                self.expect(b';')?;
                Ok(Value::Null)
            }
            b'b' => {
                self.expect(b':')?;
                let flag = self.take()?;
                self.expect(b';')?;
                Ok(Value::Bool(flag == b'1'))
            }
            b'i' => {
                self.expect(b':')?;
                let text = self.until(b';')?;
                text.parse::<i64>()
                    .map(Value::from)
                    .map_err(|_| format!("bad integer {text:?}"))
            }
            b'd' => {
                self.expect(b':')?;
                let text = self.until(b';')?;
                // PHP writes these three as words, and they have no JSON
                // spelling, so they stay words.
                if matches!(text.as_str(), "NAN" | "INF" | "-INF") {
                    return Ok(Value::String(text));
                }
                text.parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number)
                    .ok_or_else(|| format!("bad float {text:?}"))
            }
            b's' => {
                self.expect(b':')?;
                Ok(Value::String(self.string()?))
            }
            b'a' => {
                self.expect(b':')?;
                let count = self.count()?;
                self.expect(b':')?;
                self.expect(b'{')?;
                let mut fields = Map::new();
                for _ in 0..count {
                    let key = self.key()?;
                    let value = self.value()?;
                    fields.insert(key, value);
                }
                self.expect(b'}')?;
                // A PHP array is a map whose keys happen to be 0..n for a list,
                // and rendering that as a JSON array is what anybody reading it
                // expects to see.
                Ok(match is_list(&fields) {
                    true => Value::Array(fields.into_iter().map(|(_, v)| v).collect()),
                    false => Value::Object(fields),
                })
            }
            b'O' => {
                self.expect(b':')?;
                // The class name is length-prefixed like a string but is not
                // terminated: `O:4:"User":1:{…}` runs straight on into the
                // property count.
                let class = self.quoted()?;
                self.expect(b':')?;
                let count = self.count()?;
                self.expect(b':')?;
                self.expect(b'{')?;
                let mut fields = Map::new();
                fields.insert("__class".into(), Value::String(class));
                for _ in 0..count {
                    // Private and protected properties carry NUL-delimited
                    // scope markers that mean nothing outside PHP.
                    let key = self.key()?.replace('\0', "·");
                    let value = self.value()?;
                    fields.insert(key, value);
                }
                self.expect(b'}')?;
                Ok(Value::Object(fields))
            }
            // A back-reference into the graph already read. Not followed — see
            // the module note.
            tag @ (b'R' | b'r') => {
                self.expect(b':')?;
                let text = self.until(b';')?;
                Ok(Value::String(format!("<{} {text}>", tag as char)))
            }
            other => Err(format!("unknown type tag {:?}", other as char)),
        }
    }

    /// An array key, which PHP allows to be an integer or a string and JSON
    /// does not.
    fn key(&mut self) -> Result<String, String> {
        match self.value()? {
            Value::String(s) => Ok(s),
            Value::Number(n) => Ok(n.to_string()),
            other => Err(format!("{other} is not a key")),
        }
    }

    /// `5:"hello";` — a quoted run and the `;` that ends the value.
    fn string(&mut self) -> Result<String, String> {
        let text = self.quoted()?;
        self.expect(b';')?;
        Ok(text)
    }

    /// `5:"hello"` — length-prefixed *in bytes*, which is why this indexes
    /// rather than iterating characters: a multibyte string's length is not its
    /// character count, and neither is the byte count of a string that contains
    /// a quote.
    fn quoted(&mut self) -> Result<String, String> {
        let len = self.count()?;
        self.expect(b':')?;
        self.expect(b'"')?;
        let end = self
            .at
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| format!("string of {len} bytes runs off the end"))?;
        let text = String::from_utf8_lossy(&self.bytes[self.at..end]).into_owned();
        self.at = end;
        self.expect(b'"')?;
        Ok(text)
    }

    fn count(&mut self) -> Result<usize, String> {
        let text = self.peek_until(b':')?;
        let count: usize = text.parse().map_err(|_| format!("bad length {text:?}"))?;
        // A declared length larger than the whole payload cannot be honest, and
        // believing it would mean allocating on a hostile value's say-so.
        match count <= self.bytes.len() {
            true => Ok(count),
            false => Err(format!("length {count} is longer than the value")),
        }
    }

    fn take(&mut self) -> Result<u8, String> {
        let byte = *self.bytes.get(self.at).ok_or("value ends early")?;
        self.at += 1;
        Ok(byte)
    }

    fn expect(&mut self, want: u8) -> Result<(), String> {
        match self.take()? {
            got if got == want => Ok(()),
            got => Err(format!(
                "expected {:?} at byte {}, found {:?}",
                want as char,
                self.at - 1,
                got as char
            )),
        }
    }

    /// Everything up to `stop`, consuming it.
    fn until(&mut self, stop: u8) -> Result<String, String> {
        let text = self.peek_until(stop)?;
        self.at += 1;
        Ok(text)
    }

    /// Everything up to `stop`, leaving it in place.
    fn peek_until(&mut self, stop: u8) -> Result<String, String> {
        let start = self.at;
        while self.at < self.bytes.len() && self.bytes[self.at] != stop {
            self.at += 1;
        }
        if self.at >= self.bytes.len() {
            return Err(format!("no {:?} before the end", stop as char));
        }
        Ok(String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned())
    }
}

/// Whether a PHP array's keys are exactly `0..n`, which is how PHP spells a
/// list.
fn is_list(fields: &Map<String, Value>) -> bool {
    fields
        .keys()
        .enumerate()
        .all(|(index, key)| key.parse::<usize>() == Ok(index))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scalar_types_come_through_as_themselves() {
        assert_eq!(parse(b"N;").unwrap(), Value::Null);
        assert_eq!(parse(b"b:1;").unwrap(), Value::Bool(true));
        assert_eq!(parse(b"b:0;").unwrap(), Value::Bool(false));
        assert_eq!(parse(b"i:-42;").unwrap(), Value::from(-42));
        assert_eq!(parse(b"d:1.5;").unwrap(), Value::from(1.5));
        assert_eq!(parse(br#"s:5:"hello";"#).unwrap(), Value::from("hello"));
    }

    #[test]
    fn a_php_array_with_sequential_keys_is_a_json_array() {
        let value = parse(br#"a:2:{i:0;s:1:"a";i:1;s:1:"b";}"#).unwrap();
        assert_eq!(value, serde_json::json!(["a", "b"]));
    }

    #[test]
    fn a_php_array_with_string_keys_is_a_json_object() {
        let value = parse(br#"a:2:{s:2:"id";i:7;s:4:"name";s:3:"ada";}"#).unwrap();
        assert_eq!(value, serde_json::json!({"id": 7, "name": "ada"}));
    }

    #[test]
    fn an_object_keeps_the_class_name_because_it_is_the_useful_part() {
        let value = parse(br#"O:4:"User":1:{s:2:"id";i:1;}"#).unwrap();
        assert_eq!(value, serde_json::json!({"__class": "User", "id": 1}));
    }

    #[test]
    fn a_string_length_is_bytes_and_not_characters() {
        // `é` is two bytes, so the declared length is 3 for two characters.
        let value = parse("s:3:\"aé\";".as_bytes()).unwrap();
        assert_eq!(value, Value::from("aé"));
    }

    #[test]
    fn a_string_containing_the_delimiters_is_still_read_by_its_length() {
        // Four bytes, `a";b`, two of which would have ended the value if this
        // read delimiters instead of counting.
        let value = parse(br#"s:4:"a";b";"#).unwrap();
        assert_eq!(value, Value::from(r#"a";b"#));
    }

    #[test]
    fn nesting_works_to_whatever_depth_the_value_has() {
        let value = parse(br#"a:1:{s:1:"a";a:1:{s:1:"b";a:0:{}}}"#).unwrap();
        assert_eq!(value, serde_json::json!({"a": {"b": []}}));
    }

    #[test]
    fn a_reference_is_named_rather_than_followed() {
        let value = parse(br#"a:1:{i:0;R:1;}"#).unwrap();
        assert_eq!(value, serde_json::json!(["<R 1>"]));
    }

    #[test]
    fn a_truncated_or_lying_value_is_an_error_and_not_a_panic() {
        for payload in [
            &b"s:5:\"ab\";"[..],
            b"a:2:{i:0;i:1;}",
            b"i:notanumber;",
            b"z:1;",
            b"",
            b"s:99999999:\"x\";",
            b"a:1:{",
        ] {
            assert!(
                parse(payload).is_err(),
                "{:?} should not parse",
                String::from_utf8_lossy(payload)
            );
        }
    }

    #[test]
    fn trailing_bytes_after_a_complete_value_are_refused() {
        assert!(parse(b"N;N;").is_err());
    }
}
