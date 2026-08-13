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

use mica_var::{Identity, Symbol, Value, decode_value_exact, encode_value};
use proptest::prelude::*;
use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

fn leaf_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::empty_relation()),
        any::<bool>().prop_map(Value::bool),
        (-(1i64 << 55)..(1i64 << 55)).prop_map(|value| Value::int(value).unwrap()),
        any::<f32>().prop_map(|value| Value::float(value).unwrap()),
        (0u64..=Identity::MAX).prop_map(|raw| Value::identity(Identity::new(raw).unwrap())),
        "[a-z_][a-z0-9_]{0,12}".prop_map(|name| Value::symbol(Symbol::intern(&name))),
        "E_[A-Z][A-Z0-9_]{0,12}".prop_map(|name| Value::error_code(Symbol::intern(&name))),
        "\\PC{0,24}".prop_map(Value::string),
        prop::collection::vec(any::<u8>(), 0..24).prop_map(Value::bytes),
    ]
}

fn arb_value() -> impl Strategy<Value = Value> {
    leaf_value().prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..8).prop_map(Value::list),
            prop::collection::vec((inner.clone(), inner.clone()), 0..8).prop_map(Value::map),
            (
                "E_[A-Z][A-Z0-9_]{0,12}",
                prop::option::of("\\PC{0,24}"),
                prop::option::of(inner.clone()),
            )
                .prop_map(|(code, message, value)| {
                    Value::error(Symbol::intern(&code), message, value)
                }),
            (0u64..=Identity::MAX, inner).prop_map(|(delegate, value)| {
                Value::frob(Identity::new(delegate).unwrap(), value)
            }),
        ]
    })
}

fn hash(value: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

proptest! {
    #[test]
    fn equality_ordering_and_hash_are_coherent(left in arb_value(), right in arb_value()) {
        prop_assert_eq!(left == right, left.cmp(&right) == Ordering::Equal);
        prop_assert_eq!(left.partial_cmp(&right), Some(left.cmp(&right)));

        if left == right {
            prop_assert_eq!(hash(&left), hash(&right));
        }
    }

    #[test]
    fn ordered_key_bytes_match_value_order(left in arb_value(), right in arb_value()) {
        prop_assert_eq!(
            left.ordered_key_bytes().cmp(&right.ordered_key_bytes()),
            left.cmp(&right)
        );
    }

    #[test]
    fn map_entries_are_sorted_unique_and_last_write_wins(entries in prop::collection::vec((arb_value(), arb_value()), 0..24)) {
        let mut expected = entries.clone();
        expected.sort_by(|(left, _), (right, _)| left.cmp(right));
        let mut canonical = Vec::with_capacity(expected.len());
        for (key, value) in expected {
            if let Some((last_key, last_value)) = canonical.last_mut()
                && last_key == &key
            {
                *last_value = value;
                continue;
            }
            canonical.push((key, value));
        }

        let map = Value::map(entries.clone());
        map.with_map(|actual| {
            for window in actual.windows(2) {
                prop_assert!(window[0].0 < window[1].0);
            }

            prop_assert_eq!(actual, canonical.as_slice());

            Ok(())
        }).unwrap()?;
    }

    #[test]
    fn cloned_heap_values_preserve_semantics(value in arb_value()) {
        let cloned = value.clone();
        prop_assert_eq!(&value, &cloned);
        prop_assert_eq!(hash(&value), hash(&cloned));
    }

    #[test]
    fn value_codec_round_trips_generated_values(value in arb_value()) {
        let mut encoded = Vec::new();
        encode_value(&value, &mut encoded).unwrap();
        prop_assert_eq!(decode_value_exact(&encoded).unwrap(), value);
    }
}
