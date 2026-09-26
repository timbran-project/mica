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

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

const INDEX_STRIDE: usize = 32;

/// A fixed allocation whose initialized prefix never changes.
struct StringBytes {
    bytes: Box<[UnsafeCell<u8>]>,
    claimed: AtomicUsize,
}

// SAFETY: readers only access immutable prefixes. An append claims a disjoint tail
// with compare_exchange and initializes it before publishing a longer string view.
// The allocation never moves or shrinks. A losing writer allocates another buffer.
unsafe impl Sync for StringBytes {}

impl StringBytes {
    fn new(text: &str, capacity: usize) -> Self {
        let mut bytes = Vec::with_capacity(capacity);
        bytes.extend(text.bytes().map(UnsafeCell::new));
        bytes.resize_with(capacity, || UnsafeCell::new(0));
        Self {
            bytes: bytes.into_boxed_slice(),
            claimed: AtomicUsize::new(text.len()),
        }
    }

    fn append(&self, prefix_len: usize, suffix: &str) -> bool {
        let Some(end) = prefix_len.checked_add(suffix.len()) else {
            return false;
        };
        if end > self.bytes.len()
            || self
                .claimed
                .compare_exchange(prefix_len, end, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return false;
        }
        // SAFETY: this writer exclusively claimed the tail. Every published view
        // ends before it. A suffix borrowed from this allocation is in that prefix,
        // so source and destination cannot overlap. No fallible operation follows
        // the claim before initialization completes.
        unsafe {
            std::ptr::copy_nonoverlapping(
                suffix.as_ptr(),
                self.bytes.as_ptr().cast::<u8>().cast_mut().add(prefix_len),
                suffix.len(),
            );
        }
        true
    }
}

/// Immutable UTF-8 view with cached scalar count and sampled scalar offsets.
pub(crate) struct HeapString {
    storage: Arc<StringBytes>,
    bytes: usize,
    scalars: usize,
    // A thin pointer keeps this header within the existing heap enum footprint.
    // The allocation has scalars.div_ceil(INDEX_STRIDE) entries.
    offsets: AtomicPtr<usize>,
}

impl HeapString {
    pub(crate) fn new(text: &str) -> Self {
        Self {
            storage: Arc::new(StringBytes::new(text, text.len())),
            bytes: text.len(),
            scalars: text.chars().count(),
            offsets: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        // SAFETY: constructors publish only fully initialized UTF-8 prefixes.
        // Appends never change those bytes, and storage owns the allocation.
        unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                self.storage.bytes.as_ptr().cast::<u8>(),
                self.bytes,
            ))
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.scalars
    }

    pub(crate) fn byte_offset(&self, position: usize) -> Option<usize> {
        if position > self.scalars {
            return None;
        }
        if self.bytes == self.scalars {
            return Some(position);
        }
        if position == self.scalars {
            return Some(self.bytes);
        }
        let text = self.as_str();
        if self.scalars < INDEX_STRIDE {
            return text.char_indices().nth(position).map(|(offset, _)| offset);
        }
        let offsets = self.scalar_offsets();
        let base = offsets[position / INDEX_STRIDE];
        text[base..]
            .char_indices()
            .nth(position % INDEX_STRIDE)
            .map(|(offset, _)| base + offset)
    }

    fn scalar_offsets(&self) -> &[usize] {
        let mut pointer = self.offsets.load(Ordering::Acquire);
        if pointer.is_null() {
            let offsets = self
                .as_str()
                .char_indices()
                .step_by(INDEX_STRIDE)
                .map(|(offset, _)| offset)
                .collect::<Box<[usize]>>();
            let allocated = Box::into_raw(offsets).cast::<usize>();
            pointer = match self.offsets.compare_exchange(
                std::ptr::null_mut(),
                allocated,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => allocated,
                Err(published) => {
                    // SAFETY: this thread still owns its unpublished allocation.
                    // The sampled scalar count determines its exact slice length.
                    unsafe {
                        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                            allocated,
                            self.scalars.div_ceil(INDEX_STRIDE),
                        )));
                    }
                    published
                }
            };
        }
        // SAFETY: acquire observes a fully initialized, immutable index owned by
        // this header. It remains alive until the header's exclusive drop.
        unsafe { std::slice::from_raw_parts(pointer, self.scalars.div_ceil(INDEX_STRIDE)) }
    }

    pub(crate) fn scalar_at(&self, position: usize) -> Option<char> {
        self.as_str()[self.byte_offset(position)?..].chars().next()
    }

    pub(crate) fn slice(&self, start: usize, end: usize) -> Option<&str> {
        if start > end {
            return None;
        }
        Some(&self.as_str()[self.byte_offset(start)?..self.byte_offset(end)?])
    }

    pub(crate) fn scan_ascii(&self, start: usize, members: &[bool; 128], span: bool) -> usize {
        let start = start.min(self.scalars);
        let byte = self.byte_offset(start).expect("clamped scalar position");
        for (offset, scalar) in self.as_str()[byte..].chars().enumerate() {
            let member = scalar.is_ascii() && members[scalar as usize];
            if member != span {
                return start + offset;
            }
        }
        self.scalars
    }

    pub(crate) fn append(&self, suffix: &str) -> Self {
        let bytes = self
            .bytes
            .checked_add(suffix.len())
            .expect("string length overflow");
        let storage = if self.storage.append(self.bytes, suffix) {
            Arc::clone(&self.storage)
        } else {
            let capacity = bytes.max(self.bytes.saturating_mul(2)).max(16);
            let storage = Arc::new(StringBytes::new(self.as_str(), capacity));
            assert!(storage.append(self.bytes, suffix));
            storage
        };
        Self {
            storage,
            bytes,
            scalars: self.scalars + suffix.chars().count(),
            offsets: AtomicPtr::new(std::ptr::null_mut()),
        }
    }
}

impl Drop for HeapString {
    fn drop(&mut self) {
        let pointer = *self.offsets.get_mut();
        if !pointer.is_null() {
            // SAFETY: exclusive access means no reader retains an index borrow.
            // The published pointer owns one box with this exact slice length.
            unsafe {
                drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    pointer,
                    self.scalars.div_ceil(INDEX_STRIDE),
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HeapString;

    #[test]
    fn string_header_does_not_enlarge_other_heap_variants() {
        assert!(size_of::<HeapString>() <= size_of::<crate::RelationValue>());
    }

    #[test]
    fn unicode_offsets_are_published_once_to_concurrent_readers() {
        let text = "aé🦀".repeat(50);
        let string = HeapString::new(&text);
        std::thread::scope(|scope| {
            let threads = (0..4)
                .map(|_| {
                    let string = &string;
                    let text = &text;
                    scope.spawn(move || {
                        for (index, scalar) in text.chars().enumerate() {
                            assert_eq!(string.scalar_at(index), Some(scalar));
                        }
                    })
                })
                .collect::<Vec<_>>();
            for thread in threads {
                thread.join().unwrap();
            }
        });
    }

    #[test]
    fn borrowed_prefixes_remain_valid_during_concurrent_appends() {
        let seed = HeapString::new("é").append("x");
        let borrowed = seed.as_str();
        std::thread::scope(|scope| {
            let threads = (0..4)
                .map(|_| {
                    let seed = &seed;
                    scope.spawn(move || {
                        let first = seed.append("🦀");
                        let prefix = first.as_str();
                        let second = first.append(prefix);
                        let empty = first.append("");
                        assert_eq!(second.as_str(), "éx🦀éx🦀");
                        assert_eq!(empty.as_str(), "éx🦀");
                        assert_eq!(prefix, "éx🦀");
                    })
                })
                .collect::<Vec<_>>();
            for thread in threads {
                thread.join().unwrap();
            }
        });
        assert_eq!(borrowed, "éx");
    }
}
