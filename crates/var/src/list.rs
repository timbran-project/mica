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
use imbl::Vector;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Index;
use std::sync::OnceLock;

/// An immutable list with structurally shared appends, updates and slices.
pub struct ListValue {
    values: Vector<Value>,
    contiguous: OnceLock<Box<[Value]>>,
}

/// Builds a list from elements and structurally shared list splices.
#[derive(Default)]
pub struct ListBuilder {
    values: Vector<Value>,
}

impl ListBuilder {
    pub fn push(&mut self, value: Value) {
        self.values.push_back(value);
    }

    pub fn splice(&mut self, list: &ListValue) {
        if self.values.is_empty() {
            self.values = list.values.clone();
            return;
        }
        // Small suffixes fit in a vector leaf; pushing avoids rebalancing a tree join.
        if list.len() <= 64 {
            self.values.extend(list.iter().cloned());
            return;
        }
        self.values.append(list.values.clone());
    }

    pub fn finish(self) -> ListValue {
        ListValue::from_vector(self.values)
    }
}

impl ListValue {
    pub(crate) fn new(values: impl IntoIterator<Item = Value>) -> Self {
        Self::from_vector(values.into_iter().collect())
    }

    fn from_vector(values: Vector<Value>) -> Self {
        Self {
            values,
            contiguous: OnceLock::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Value> {
        self.values.get(index)
    }

    pub fn first(&self) -> Option<&Value> {
        self.values.front()
    }

    pub fn iter(&self) -> <&Vector<Value> as IntoIterator>::IntoIter {
        self.values.iter()
    }

    pub fn to_vec(&self) -> Vec<Value> {
        self.iter().cloned().collect()
    }

    /// Materializes a contiguous view once for consumers such as the JIT.
    /// Ordinary reads, iteration and updates use the vector directly.
    pub fn as_slice(&self) -> &[Value] {
        self.contiguous
            .get_or_init(|| self.iter().cloned().collect())
    }

    pub(crate) fn append(&self, value: Value) -> Self {
        let mut values = self.values.clone();
        values.push_back(value);
        Self::from_vector(values)
    }

    pub(crate) fn set(&self, index: usize, value: Value) -> Option<Self> {
        if index >= self.len() {
            return None;
        }
        let mut values = self.values.clone();
        values.set(index, value);
        Some(Self::from_vector(values))
    }

    pub(crate) fn slice(&self, start: usize, end: usize) -> Option<Self> {
        if start > end || end > self.len() {
            return None;
        }
        let mut values = self.values.clone();
        Some(Self::from_vector(values.slice(start..end)))
    }
}

impl<'a> IntoIterator for &'a ListValue {
    type Item = &'a Value;
    type IntoIter = <&'a Vector<Value> as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl Index<usize> for ListValue {
    type Output = Value;
    fn index(&self, index: usize) -> &Value {
        &self.values[index]
    }
}

impl PartialEq for ListValue {
    fn eq(&self, other: &Self) -> bool {
        self.values == other.values
    }
}

impl Eq for ListValue {}

impl PartialOrd for ListValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ListValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.iter().cmp(other.iter())
    }
}

impl Hash for ListValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.len().hash(state);
        for value in self {
            value.hash(state);
        }
    }
}

impl fmt::Debug for ListValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Symbol, decode_value_exact, encode_value};
    use std::collections::hash_map::DefaultHasher;
    use std::sync::Barrier;
    use std::thread;

    fn integer(n: usize) -> Value {
        Value::int(n as i64).unwrap()
    }

    #[test]
    fn splices_share_large_inputs_and_release_nested_values() {
        let marker = Value::string("shared leaf");
        {
            let left_len = 65537;
            let right_len = 65539;
            let left = Value::list((0..left_len).map(|_| marker.clone()));
            let right = Value::list((0..right_len).map(|_| marker.clone()));
            let references = marker.heap_strong_count().unwrap();
            let mut builder = ListBuilder::default();
            builder.push(integer(1));
            left.with_list(|list| builder.splice(list)).unwrap();
            builder.push(left.clone());
            right.with_list(|list| builder.splice(list)).unwrap();
            left.with_list(|list| builder.splice(list)).unwrap();
            builder.push(integer(2));
            let joined = Value::from(builder.finish());

            let joined_len = left_len * 2 + right_len + 3;
            assert_eq!(joined.list_len(), Some(joined_len));
            assert_eq!(joined.list_get(0), Some(integer(1)));
            assert_eq!(joined.list_get(left_len + 1), Some(left.clone()));
            assert_eq!(joined.list_get(joined_len - 1), Some(integer(2)));
            // Concatenation copies boundary nodes, not every element of each splice.
            let retained = marker.heap_strong_count().unwrap() - references;
            assert!(
                retained < 16384,
                "splice retained {retained} element references"
            );
            for list in [&left, &right, &joined] {
                list.with_list(|list| assert!(list.contiguous.get().is_none()));
            }
            let branch = joined.list_set(left_len + 2, integer(3)).unwrap();
            assert_eq!(branch.list_get(left_len + 2), Some(integer(3)));
            assert_eq!(joined.list_get(left_len + 2), Some(marker.clone()));
            assert_eq!(right.list_get(0), Some(marker.clone()));
        }
        assert_eq!(marker.heap_strong_count(), Some(1));
    }

    #[test]
    fn splices_match_flat_lists_across_vector_boundaries() {
        for size in [0, 1, 3, 31, 32, 63, 64, 65, 127, 128, 129, 4096] {
            let prefix = Value::list((0..size).map(integer));
            let mut builder = ListBuilder::default();
            prefix.with_list(|list| builder.splice(list)).unwrap();
            builder.push(prefix.clone());
            prefix.with_list(|list| builder.splice(list)).unwrap();
            let joined = Value::from(builder.finish());
            let expected = Value::list(
                (0..size)
                    .map(integer)
                    .chain([prefix.clone()])
                    .chain((0..size).map(integer)),
            );
            assert_eq!(joined, expected);
            assert_eq!(joined.ordered_key_bytes(), expected.ordered_key_bytes());
            let mut encoded = Vec::new();
            encode_value(&joined, &mut encoded).unwrap();
            assert_eq!(decode_value_exact(&encoded).unwrap(), expected);
        }
    }

    #[test]
    fn updates_and_slices_preserve_nested_snapshots_without_flattening() {
        let mut list = Value::list([]);
        let mut snapshots = Vec::new();
        for index in 0..5000 {
            if index % 64 == 0 {
                snapshots.push((index, list.clone()));
            }
            list = list.list_append(Value::list([integer(index)])).unwrap();
        }
        for (len, snapshot) in snapshots {
            assert_eq!(snapshot.list_len(), Some(len));
            assert_eq!(snapshot.list_get(len), None);
            if len != 0 {
                assert_eq!(
                    snapshot.list_get(len - 1),
                    Some(Value::list([integer(len - 1)]))
                );
            }
            snapshot.with_list(|list| assert!(list.contiguous.get().is_none()));
        }
        let branch = list.list_set(4096, Value::string("changed")).unwrap();
        assert_eq!(branch.list_get(4096), Some(Value::string("changed")));
        assert_eq!(list.list_get(4096), Some(Value::list([integer(4096)])));
        let slice = list.list_slice(63, 4097).unwrap();
        let expected = Value::list((63..4097).map(|n| Value::list([integer(n)])));
        assert_eq!(slice, expected);
        assert_eq!(slice.cmp(&expected), Ordering::Equal);
        assert_eq!(slice.ordered_key_bytes(), expected.ordered_key_bytes());
        let mut encoded = Vec::new();
        encode_value(&slice, &mut encoded).unwrap();
        assert_eq!(decode_value_exact(&encoded).unwrap(), slice);
        slice.with_list(|list| {
            let mut expected = DefaultHasher::new();
            list.to_vec().hash(&mut expected);
            let mut actual = DefaultHasher::new();
            list.hash(&mut actual);
            assert_eq!(actual.finish(), expected.finish());
            assert!(list.contiguous.get().is_none());
        });
        list.with_list(|list| assert!(list.contiguous.get().is_none()));
        assert_eq!(list.list_slice(5000, 5000), Some(Value::list([])));
        assert!(list.list_slice(1, 0).is_none());
        assert!(list.list_slice(0, 5001).is_none());
        assert!(list.list_set(5000, integer(0)).is_none());
    }

    #[test]
    fn concurrent_branches_preserve_borrowed_contiguous_views() {
        let seed = ListValue::new((0..4096).map(integer));
        let borrowed = seed.as_slice();
        let barrier = Barrier::new(4);
        thread::scope(|scope| {
            for index in 0..4 {
                let seed = &seed;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    let item = Value::list([integer(index)]);
                    let mut branch = seed.append(item.clone());
                    for _ in 0..128 {
                        branch = branch.append(item.clone());
                    }
                    assert_eq!(branch.len(), 4225);
                    assert_eq!(branch.get(4096), Some(&item));
                    assert!(branch.iter().skip(4096).all(|v| *v == item));
                    assert_eq!(seed.as_slice().as_ptr(), borrowed.as_ptr());
                    assert!(branch.contiguous.get().is_none());
                });
            }
        });
        assert_eq!(borrowed.len(), 4096);
        assert_eq!(borrowed[4095], integer(4095));
    }

    #[test]
    fn self_append_and_cross_append_release_all_nested_values() {
        let marker = Value::string("owned leaf");
        {
            let left = Value::list((0..128).map(|_| marker.clone()));
            let right = Value::list([marker.clone()]);
            let self_append = left.list_append(left.clone()).unwrap();
            assert_eq!(self_append.list_get(128), Some(left.clone()));
            self_append.with_list(|list| {
                list.as_slice();
            });
            let wrap = |v| Value::map([(Value::symbol(Symbol::intern("nested")), v)]);
            let barrier = Barrier::new(2);
            thread::scope(|scope| {
                let a = scope.spawn(|| {
                    barrier.wait();
                    left.list_append(wrap(right.clone())).unwrap()
                });
                let b = scope.spawn(|| {
                    barrier.wait();
                    right.list_append(wrap(left.clone())).unwrap()
                });
                drop(a.join().unwrap());
                drop(b.join().unwrap());
            });
        }
        assert_eq!(marker.heap_strong_count(), Some(1));
    }
}
