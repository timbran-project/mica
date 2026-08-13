#![no_main]

use libfuzzer_sys::fuzz_target;
use mica_var::{Identity, Symbol, Value};
use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

fuzz_target!(|data: &[u8]| {
    let mut cursor = Cursor::new(data);
    let mut values = Vec::new();

    while !cursor.is_empty() && values.len() < 64 {
        if let Some(value) = cursor.value(0) {
            values.push(value);
        } else {
            break;
        }
    }

    for value in &values {
        let cloned = value.clone();
        assert_eq!(value, &cloned);
        assert_eq!(value.cmp(&cloned), Ordering::Equal);
        assert_eq!(hash(value), hash(&cloned));
    }

    for pair in values.windows(2) {
        let left = &pair[0];
        let right = &pair[1];
        assert_eq!(left == right, left.cmp(right) == Ordering::Equal);
        if left == right {
            assert_eq!(hash(left), hash(right));
        }
        assert_eq!(
            left.ordered_key_bytes().cmp(&right.ordered_key_bytes()),
            left.cmp(right)
        );

        let _ = left.checked_add(right);
    }
});

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn is_empty(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn byte(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(byte)
    }

    fn bytes(&mut self, max_len: usize) -> Option<&'a [u8]> {
        let requested = self.byte()? as usize % (max_len + 1);
        let remaining = self.data.len().saturating_sub(self.pos);
        let len = requested.min(remaining);
        let start = self.pos;
        self.pos += len;
        Some(&self.data[start..start + len])
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        let tag = self.byte()? % if depth >= 4 { 8 } else { 11 };
        match tag {
            0 => Some(Value::empty_relation()),
            1 => Some(Value::bool(self.byte()? & 1 != 0)),
            2 => {
                let raw = self.u64()?;
                let value = ((raw & 0x00ff_ffff_ffff_ffff) as i64) << 8 >> 8;
                Value::int(value).ok()
            }
            3 => Value::float(f32::from_bits(self.u32()?)).ok(),
            4 => {
                let raw = self.u64()? & Identity::MAX;
                Some(Value::identity(Identity::new(raw).unwrap()))
            }
            5 => Some(Value::symbol(Symbol::from_id(self.u32()?))),
            6 => Some(Value::error_code(Symbol::from_id(self.u32()?))),
            7 => {
                let bytes = self.bytes(32)?;
                Some(Value::string(String::from_utf8_lossy(bytes)))
            }
            8 => {
                let len = self.byte()? as usize % 8;
                let mut values = Vec::with_capacity(len);
                for _ in 0..len {
                    values.push(self.value(depth + 1)?);
                }
                Some(Value::list(values))
            }
            9 => {
                let len = self.byte()? as usize % 8;
                let mut entries = Vec::with_capacity(len);
                for _ in 0..len {
                    entries.push((self.value(depth + 1)?, self.value(depth + 1)?));
                }
                Some(Value::map(entries))
            }
            10 => {
                let code = Symbol::from_id(self.u32()?);
                let message = match self.byte()? & 1 {
                    0 => None,
                    _ => {
                        let bytes = self.bytes(32)?;
                        Some(String::from_utf8_lossy(bytes).into_owned())
                    }
                };
                let value = match self.byte()? & 1 {
                    0 => None,
                    _ => Some(self.value(depth + 1)?),
                };
                Some(Value::error(code, message, value))
            }
            _ => unreachable!(),
        }
    }

    fn u32(&mut self) -> Option<u32> {
        let mut out = [0; 4];
        for byte in &mut out {
            *byte = self.byte()?;
        }
        Some(u32::from_le_bytes(out))
    }

    fn u64(&mut self) -> Option<u64> {
        let mut out = [0; 8];
        for byte in &mut out {
            *byte = self.byte()?;
        }
        Some(u64::from_le_bytes(out))
    }
}

fn hash(value: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}
