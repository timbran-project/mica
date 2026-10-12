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

use crate::Value;
use imbl::OrdMap;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

/// An immutable map in canonical key order, with structurally shared updates.
pub struct MapValue {
    entries: OrdMap<Value, Value>,
    indexed: OnceLock<Box<[(Value, Value)]>>,
}

impl MapValue {
    pub(crate) fn new(entries: impl IntoIterator<Item = (Value, Value)>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
            indexed: OnceLock::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: &Value) -> Option<&Value> {
        self.entries.get(key)
    }

    pub fn iter(&self) -> <&OrdMap<Value, Value> as IntoIterator>::IntoIter {
        self.entries.iter()
    }

    pub(crate) fn update(&self, key: Value, value: Value) -> Self {
        let mut entries = self.entries.clone();
        entries.insert(key, value);
        Self {
            entries,
            indexed: OnceLock::new(),
        }
    }

    /// Materializes canonical entries once for ordinal VM traversal and JIT views.
    /// Key lookup, updates, encoding and ordinary iteration never need this array.
    /// The array belongs to this immutable version; updates start without a cache.
    pub fn indexed_entries(&self) -> &[(Value, Value)] {
        self.indexed
            .get_or_init(|| self.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }
}

impl<'a> IntoIterator for &'a MapValue {
    type Item = (&'a Value, &'a Value);
    type IntoIter = <&'a OrdMap<Value, Value> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl PartialEq for MapValue {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl Eq for MapValue {}

impl PartialOrd for MapValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MapValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.entries.cmp(&other.entries)
    }
}

impl Hash for MapValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Preserve the canonical slice hash, including its length prefix.
        self.len().hash(state);
        for entry in self {
            entry.hash(state);
        }
    }
}

impl fmt::Debug for MapValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.entries.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_value_exact, encode_value};
    use std::collections::hash_map::DefaultHasher;
    use std::thread;

    fn integer(n: i64) -> Value {
        Value::int(n).unwrap()
    }

    #[test]
    fn updates_share_storage_without_changing_retained_versions() {
        let base = Value::map((0..4096).map(|n| (integer(n), Value::list([integer(n)]))));
        let indexed = base.with_map(|m| m.indexed_entries().to_vec()).unwrap();
        let changed = base
            .map_set(integer(2048), Value::string("changed"))
            .unwrap();
        let branch = base.map_set(integer(4096), base.clone()).unwrap();
        assert_eq!(
            base.map_get(&integer(2048)),
            Some(Value::list([integer(2048)]))
        );
        assert_eq!(
            changed.map_get(&integer(2048)),
            Some(Value::string("changed"))
        );
        assert_eq!(branch.map_get(&integer(4096)), Some(base.clone()));
        assert_eq!(base.map_get(&integer(4096)), None);
        assert_eq!(changed.map_get(&integer(4096)), None);
        base.with_map(|m| assert_eq!(m.indexed_entries(), indexed));
        changed.with_map(|m| assert!(m.indexed.get().is_none()));
        branch.with_map(|m| assert!(m.indexed.get().is_none()));

        thread::scope(|scope| {
            for _ in 0..4 {
                let branch = &branch;
                scope.spawn(move || {
                    branch.with_map(|m| {
                        assert_eq!(m.indexed_entries().len(), 4097);
                        assert_eq!(m.indexed_entries()[4096].0, integer(4096));
                    });
                });
            }
        });
    }

    #[test]
    fn tree_order_hash_and_encoding_are_canonical_without_flattening() {
        let entries: Vec<_> = (0..512)
            .map(|n| (integer(n), Value::string(n.to_string())))
            .collect();
        let forward = Value::map(entries.clone());
        let reverse = Value::map(entries.iter().rev().cloned());
        assert_eq!(forward, reverse);
        assert_eq!(forward.cmp(&reverse), Ordering::Equal);
        let mut encoded = Vec::new();
        encode_value(&forward, &mut encoded).unwrap();
        let mut other_encoded = Vec::new();
        encode_value(&reverse, &mut other_encoded).unwrap();
        assert_eq!(encoded, other_encoded);
        assert_eq!(decode_value_exact(&encoded).unwrap(), forward);
        assert_eq!(forward.ordered_key_bytes(), reverse.ordered_key_bytes());
        forward.with_map(|m| {
            let mut expected = DefaultHasher::new();
            entries.as_slice().hash(&mut expected);
            let mut actual = DefaultHasher::new();
            m.hash(&mut actual);
            assert_eq!(actual.finish(), expected.finish());
            assert!(m.indexed.get().is_none());
        });
        reverse.with_map(|m| assert!(m.indexed.get().is_none()));
        // Map ordering is lexicographic, not length-first.
        assert!(
            Value::map([(integer(1), integer(0)), (integer(2), integer(0))])
                < Value::map([(integer(3), integer(0))])
        );
    }

    #[test]
    fn duplicate_updates_keep_type_sensitive_keys_and_last_value() {
        let int = integer(1);
        let float = Value::float(1.0).unwrap();
        let map = Value::map([
            (int.clone(), integer(10)),
            (float.clone(), integer(20)),
            (int.clone(), integer(30)),
        ]);
        assert_eq!(map.map_len(), Some(2));
        assert_eq!(map.map_get(&int), Some(integer(30)));
        assert_eq!(map.map_get(&float), Some(integer(20)));
        let updated = map.map_set(float.clone(), integer(40)).unwrap();
        assert_eq!(updated.map_get(&float), Some(integer(40)));
        assert_eq!(map.map_get(&float), Some(integer(20)));
    }
}
