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

use crate::{Value, ValueKind, ValueRef, ValueVisitor, VisitDecision};
use std::cell::UnsafeCell;
use std::mem::{ManuallyDrop, MaybeUninit};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct ListStorage {
    values: Box<[UnsafeCell<MaybeUninit<Value>>]>,
    claimed: AtomicUsize,
}

// SAFETY: published prefixes are immutable. A writer claims one uninitialized
// tail cell, writes it without a fallible operation, then publishes its view.
// The allocation never moves, and Value supports concurrent shared ownership.
unsafe impl Sync for ListStorage {}

impl ListStorage {
    fn new(values: Vec<Value>) -> Self {
        let mut values = ManuallyDrop::new(values);
        let len = values.len();
        let capacity = values.capacity();
        // SAFETY: UnsafeCell and MaybeUninit preserve Value's size and alignment.
        // This transfers the entire Vec allocation, including its uninitialized
        // spare capacity. Drop below destroys exactly the initialized prefix;
        // dropping the box then frees the allocation without dropping values twice.
        let cells = unsafe {
            Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                values.as_mut_ptr().cast::<UnsafeCell<MaybeUninit<Value>>>(),
                capacity,
            ))
        };
        Self {
            values: cells,
            claimed: AtomicUsize::new(len),
        }
    }

    fn try_append(&self, len: usize, value: Value) -> Result<(), Value> {
        if len == self.values.len()
            || self
                .claimed
                .compare_exchange(len, len + 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return Err(value);
        }
        // SAFETY: the successful claim grants this writer the exclusive tail cell.
        // Earlier views cannot read it. Moving a Value into it cannot panic.
        unsafe {
            self.values[len].get().write(MaybeUninit::new(value));
        }
        Ok(())
    }
}

impl Drop for ListStorage {
    fn drop(&mut self) {
        let len = *self.claimed.get_mut();
        for cell in &mut self.values[..len] {
            // SAFETY: exclusive drop occurs after every writer finishes, so every
            // claimed cell contains one initialized Value and is dropped once.
            unsafe {
                cell.get_mut().assume_init_drop();
            }
        }
    }
}

pub(crate) struct HeapList {
    data: ListData,
}

enum ListData {
    Exact(Box<[Value]>),
    Prefix {
        storage: Arc<ListStorage>,
        len: usize,
    },
}

impl HeapList {
    pub(crate) fn new(values: Vec<Value>) -> Self {
        Self {
            data: ListData::Exact(values.into_boxed_slice()),
        }
    }

    pub(crate) fn as_slice(&self) -> &[Value] {
        match &self.data {
            ListData::Exact(values) => values,
            ListData::Prefix { storage, len } => {
                // SAFETY: this view owns a fully initialized, immutable prefix.
                // Appends initialize cells after it, and storage keeps it alive.
                unsafe { std::slice::from_raw_parts(storage.values.as_ptr().cast::<Value>(), *len) }
            }
        }
    }

    pub(crate) fn append(&self, value: Value) -> Self {
        // A list reference in the appended value could form an ownership cycle
        // through invisible tails, including tails appended on another thread.
        // Only values proven free of list references may enter shared storage.
        let value = if let ListData::Prefix { storage, len } = &self.data
            && contains_no_lists(&value)
        {
            match storage.try_append(*len, value) {
                Ok(()) => {
                    return Self {
                        data: ListData::Prefix {
                            storage: Arc::clone(storage),
                            len: len + 1,
                        },
                    };
                }
                Err(value) => value,
            }
        } else {
            value
        };
        let prefix = self.as_slice();
        let len = prefix.len() + 1;
        let capacity = prefix.len().saturating_mul(2).max(len).max(4);
        let mut values = Vec::with_capacity(capacity);
        values.extend(prefix.iter().cloned());
        values.push(value);
        Self {
            data: ListData::Prefix {
                storage: Arc::new(ListStorage::new(values)),
                len,
            },
        }
    }
}

fn contains_no_lists(value: &Value) -> bool {
    if value.is_immediate() || matches!(value.kind(), ValueKind::String | ValueKind::Bytes) {
        return true;
    }
    // Exhaustion conservatively selects copying. Bound both traversal work and
    // stack depth for shared or deeply nested container graphs.
    value
        .walk(&mut ListReferenceCheck {
            remaining: 256,
            depth: 0,
        })
        .is_ok()
}

struct ListReferenceCheck {
    remaining: usize,
    depth: usize,
}

impl ValueVisitor for ListReferenceCheck {
    type Error = ();

    fn visit_value(&mut self, _value: &Value, value: ValueRef<'_>) -> Result<VisitDecision, ()> {
        if self.remaining == 0 || self.depth == 32 || matches!(value, ValueRef::List(_)) {
            return Err(());
        }
        self.remaining -= 1;
        self.depth += 1;
        Ok(VisitDecision::Descend)
    }

    fn leave_value(&mut self, _value: &Value, _view: ValueRef<'_>) -> Result<(), ()> {
        self.depth -= 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Symbol;
    use crate::heap::HeapValue;

    #[test]
    fn concurrent_appends_preserve_borrowed_prefixes() {
        let mut values = Vec::with_capacity(16);
        values.push(Value::int(1).unwrap());
        let seed = HeapList::new(values).append(Value::int(2).unwrap());
        let borrowed = seed.as_slice();
        std::thread::scope(|scope| {
            let threads = (0..4)
                .map(|index| {
                    let seed = &seed;
                    scope.spawn(move || {
                        let item = Value::int(index).unwrap();
                        let mut list = seed.append(item.clone());
                        for _ in 0..32 {
                            list = list.append(item.clone());
                        }
                        assert_eq!(list.as_slice().len(), 35);
                        assert_eq!(list.as_slice()[0], Value::int(1).unwrap());
                        assert!(list.as_slice()[2..].iter().all(|value| *value == item));
                    })
                })
                .collect::<Vec<_>>();
            for thread in threads {
                thread.join().unwrap();
            }
        });
        assert_eq!(borrowed, [Value::int(1).unwrap(), Value::int(2).unwrap()]);
    }

    fn seed() -> Value {
        Value::list([]).list_append(Value::int(1).unwrap()).unwrap()
    }

    fn storage(value: &Value) -> &Arc<ListStorage> {
        let Some(HeapValue::List(HeapList {
            data: ListData::Prefix { storage, .. },
        })) = value.heap_ref()
        else {
            panic!("expected appended list");
        };
        storage
    }

    #[test]
    fn self_append_and_cross_append_do_not_leak_ownership_cycles() {
        let left = seed();
        let right = seed();
        assert!(storage(&left).values.len() > 1);
        let weak_left = Arc::downgrade(storage(&left));
        let weak_right = Arc::downgrade(storage(&right));
        let self_append = left.list_append(left.clone()).unwrap();
        assert_eq!(self_append.list_get(1), Some(left.clone()));
        let wrap = |value: Value| Value::map([(Value::symbol(Symbol::intern("nested")), value)]);
        std::thread::scope(|scope| {
            let a = scope.spawn(|| left.list_append(wrap(right.clone())).unwrap());
            let b = scope.spawn(|| right.list_append(wrap(left.clone())).unwrap());
            drop(a.join().unwrap());
            drop(b.join().unwrap());
        });
        drop(self_append);
        drop(left);
        drop(right);
        assert!(weak_left.upgrade().is_none());
        assert!(weak_right.upgrade().is_none());
    }

    #[test]
    fn map_rows_without_lists_can_share_spare_storage() {
        let original = seed();
        let row = Value::map([(Value::symbol(Symbol::intern("text")), Value::string("row"))]);
        let appended = original.list_append(row.clone()).unwrap();
        assert!(Arc::ptr_eq(storage(&original), storage(&appended)));
        assert_eq!(original.list_len(), Some(1));
        assert_eq!(appended.list_get(1), Some(row));
    }
}
