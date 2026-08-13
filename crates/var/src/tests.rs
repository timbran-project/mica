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

use crate::abi::{
    VALUE_ABI_VERSION, VALUE_INT_MAX, VALUE_INT_MIN, VALUE_INT_TAG, VALUE_PAYLOAD_MASK,
    VALUE_TAG_SHIFT, borrowed_value_bits, borrowed_value_cmp, borrowed_value_numeric_cmp,
    borrowed_value_numeric_eq, clone_value_bits, drop_value_bits, from_owned_value_bits,
    into_owned_value_bits, pack_value, value_is_immediate, value_payload, value_tag,
};
use crate::value::{INT_MAX, INT_MIN};
use crate::{
    CapabilityId, Identity, Symbol, SymbolEncoding, SymbolMetadata, Tuple, Value, ValueCodecError,
    ValueCodecOptions, ValueKind, ValueRef, ValueSegment, ValueSink, ValueVisitor, VisitDecision,
    decode_value, decode_value_exact, decode_value_exact_with_options, encode_value,
    encode_value_segments, encode_value_segments_with_options, encode_value_to_sink,
    encode_value_with_options,
};
use std::cmp::Ordering;
use std::mem::{align_of, size_of};

#[test]
fn value_is_one_word() {
    assert_eq!(size_of::<Value>(), 8);
    assert_eq!(align_of::<Value>(), 8);
}

#[test]
fn process_local_value_abi_matches_value_layout() {
    assert_eq!(VALUE_ABI_VERSION, 3);
    assert_eq!(VALUE_TAG_SHIFT, 56);
    assert_eq!(VALUE_PAYLOAD_MASK, 0x00ff_ffff_ffff_ffff);
    assert_eq!(VALUE_INT_MIN, INT_MIN);
    assert_eq!(VALUE_INT_MAX, INT_MAX);

    for integer in [INT_MIN, -1, 0, 1, INT_MAX] {
        let value = Value::int(integer).unwrap();
        let bits = borrowed_value_bits(&value);
        assert_eq!(value_tag(bits), VALUE_INT_TAG);
        assert_eq!(value_payload(bits), integer as u64 & VALUE_PAYLOAD_MASK);
        assert_eq!(pack_value(VALUE_INT_TAG, value_payload(bits)), bits);
    }
}

#[test]
fn process_local_value_abi_classifies_heap_ownership() {
    let immediate_values = [
        Value::nothing(),
        Value::bool(true),
        Value::int(1).unwrap(),
        Value::float(1.5).unwrap(),
        Value::identity_raw(1).unwrap(),
        Value::symbol(Symbol::intern("value-abi-symbol")),
        Value::error_code(Symbol::intern("E_VALUE_ABI")),
        Value::capability_raw(1).unwrap(),
        Value::function_raw(1).unwrap(),
    ];
    for value in immediate_values {
        assert!(value_is_immediate(borrowed_value_bits(&value)));
    }

    let heap_values = [
        Value::string("value ABI"),
        Value::bytes([1, 2, 3]),
        Value::list([Value::int(1).unwrap()]),
        Value::map([(Value::bool(true), Value::int(1).unwrap())]),
        Value::relation(
            [Symbol::intern("value")],
            [Tuple::from([Value::int(1).unwrap()])],
        )
        .unwrap(),
        Value::range(Value::int(1).unwrap(), Some(Value::int(2).unwrap())),
        Value::error(Symbol::intern("E_VALUE_ABI_HEAP"), None::<Box<str>>, None),
        Value::frob(Identity::new(1).unwrap(), Value::nothing()),
    ];
    for value in heap_values {
        assert!(!value_is_immediate(borrowed_value_bits(&value)));
    }
    assert!(!value_is_immediate(pack_value(
        ValueKind::Relation as u8,
        0
    )));
}

#[test]
fn process_local_value_abi_preserves_heap_reference_ownership() {
    let value = Value::string("owned value word");
    let borrowed_bits = borrowed_value_bits(&value);
    assert_eq!(value.heap_strong_count(), Some(1));

    let cloned_bits = unsafe { clone_value_bits(borrowed_bits) };
    assert_eq!(value.heap_strong_count(), Some(2));
    unsafe { drop_value_bits(cloned_bits) };
    assert_eq!(value.heap_strong_count(), Some(1));

    let owned_bits = into_owned_value_bits(value);
    let reconstructed = unsafe { from_owned_value_bits(owned_bits) };
    assert_eq!(
        reconstructed.with_str(str::to_owned).as_deref(),
        Some("owned value word")
    );
    assert_eq!(reconstructed.heap_strong_count(), Some(1));
}

#[test]
fn process_local_value_abi_compares_borrowed_words_without_taking_ownership() {
    let left = Value::string("borrowed equality");
    let equal = Value::string("borrowed equality");
    let different = Value::string("different");
    assert!(unsafe {
        borrowed_value_numeric_eq(borrowed_value_bits(&left), borrowed_value_bits(&equal))
    });
    assert!(!unsafe {
        borrowed_value_numeric_eq(borrowed_value_bits(&left), borrowed_value_bits(&different))
    });
    assert!(unsafe {
        borrowed_value_numeric_eq(
            borrowed_value_bits(&Value::int(1).unwrap()),
            borrowed_value_bits(&Value::float(1.0).unwrap()),
        )
    });
    assert_eq!(left.heap_strong_count(), Some(1));
    assert_eq!(equal.heap_strong_count(), Some(1));
    assert_eq!(different.heap_strong_count(), Some(1));
}

#[test]
fn process_local_value_abi_orders_borrowed_words_without_taking_ownership() {
    let alpha = Value::string("alpha");
    let beta = Value::string("beta");
    let list = Value::list([Value::int(1).unwrap()]);
    let map = Value::map([(Value::string("key"), Value::int(1).unwrap())]);

    assert_eq!(
        unsafe { borrowed_value_cmp(borrowed_value_bits(&alpha), borrowed_value_bits(&beta)) },
        Ordering::Less,
    );
    assert_eq!(
        unsafe { borrowed_value_cmp(borrowed_value_bits(&list), borrowed_value_bits(&map)) },
        list.cmp(&map),
    );
    assert_ne!(
        unsafe {
            borrowed_value_cmp(
                borrowed_value_bits(&Value::int(1).unwrap()),
                borrowed_value_bits(&Value::float(1.0).unwrap()),
            )
        },
        Ordering::Equal,
    );
    assert_eq!(
        unsafe {
            borrowed_value_numeric_cmp(
                borrowed_value_bits(&Value::int(16_777_217).unwrap()),
                borrowed_value_bits(&Value::float(16_777_216.0).unwrap()),
            )
        },
        Ordering::Greater,
    );
    assert_eq!(
        unsafe {
            borrowed_value_numeric_cmp(
                borrowed_value_bits(&Value::int(1).unwrap()),
                borrowed_value_bits(&Value::float(1.0).unwrap()),
            )
        },
        Ordering::Equal,
    );
    assert_eq!(
        unsafe {
            borrowed_value_numeric_cmp(borrowed_value_bits(&alpha), borrowed_value_bits(&beta))
        },
        Ordering::Less,
    );

    for value in [&alpha, &beta, &list, &map] {
        assert_eq!(value.heap_strong_count(), Some(1));
    }
}

#[test]
fn immediate_constructors_round_trip() {
    let nothing = Value::nothing();
    assert_eq!(nothing.kind(), ValueKind::Relation);
    assert!(nothing.is_empty_relation());
    assert_eq!(nothing.with_relation(|relation| relation.arity()), Some(0));
    assert_eq!(nothing.with_relation(|relation| relation.len()), Some(0));
    assert_eq!(Value::relation([], []).unwrap(), nothing);
    let unit = Value::unit();
    assert!(unit.is_unit());
    assert!(!unit.is_empty_relation());
    assert_eq!(unit.with_relation(|relation| relation.arity()), Some(0));
    assert_eq!(unit.with_relation(|relation| relation.len()), Some(1));
    assert_eq!(Value::bool(true).as_bool(), Some(true));
    assert_eq!(Value::bool(false).as_bool(), Some(false));
    assert_eq!(Value::int(INT_MIN).unwrap().as_int(), Some(INT_MIN));
    assert_eq!(Value::int(INT_MAX).unwrap().as_int(), Some(INT_MAX));
    assert!(Value::int(INT_MIN - 1).is_err());
    assert!(Value::int(INT_MAX + 1).is_err());

    let id = Identity::new(0x00ab_cdef).unwrap();
    assert_eq!(Value::identity(id).as_identity(), Some(id));

    let capability = CapabilityId::new(7).unwrap();
    assert_eq!(
        Value::capability(capability).as_capability(),
        Some(capability)
    );
    assert_eq!(format!("{}", Value::capability(capability)), "<cap>");

    let symbol = Symbol::intern("take");
    assert_eq!(Value::symbol(symbol).as_symbol(), Some(symbol));
    assert_eq!(symbol.name(), Some("take"));
    assert_eq!(
        symbol.metadata(),
        Some(SymbolMetadata {
            byte_len: 4,
            char_len: 4,
            is_ascii: true,
        })
    );
    assert_eq!(Symbol::intern("take"), symbol);
    assert_ne!(Symbol::intern("TAKE"), symbol);

    let error_code = Symbol::intern("E_NOT_PORTABLE");
    assert_eq!(Value::error_code(error_code).kind(), ValueKind::ErrorCode);
    assert_eq!(
        Value::error_code(error_code).as_error_code(),
        Some(error_code)
    );
    assert_ne!(Value::error_code(error_code), Value::symbol(error_code));
    assert_eq!(
        format!("{}", Value::error_code(error_code)),
        "E_NOT_PORTABLE"
    );

    let error = Value::error(
        error_code,
        Some("That cannot be taken."),
        Some(Value::symbol(Symbol::intern("lamp"))),
    );
    assert_eq!(error.kind(), ValueKind::Error);
    assert_eq!(error.error_code_symbol(), Some(error_code));
    assert_eq!(
        error.with_error(|error| (
            error.code(),
            error.message().map(str::to_string),
            error.value().cloned(),
        )),
        Some((
            error_code,
            Some("That cannot be taken.".to_string()),
            Some(Value::symbol(Symbol::intern("lamp"))),
        ))
    );
    assert_eq!(
        format!("{error:?}"),
        "error(E_NOT_PORTABLE, \"That cannot be taken.\", :lamp)"
    );
}

#[test]
fn structural_unit_option_and_result_values_round_trip_through_the_codec() {
    let unit = Value::unit();
    let none = Value::relation([Symbol::intern("value")], []).unwrap();
    let some_unit =
        Value::relation([Symbol::intern("value")], [Tuple::from([unit.clone()])]).unwrap();
    let result = Value::relation(
        [Symbol::intern("case"), Symbol::intern("value")],
        [Tuple::from([
            Value::symbol(Symbol::intern("ok")),
            Value::int(7).unwrap(),
        ])],
    )
    .unwrap();

    for value in [unit, none, some_unit, result] {
        let mut encoded = Vec::new();
        encode_value(&value, &mut encoded).unwrap();
        assert_eq!(decode_value_exact(&encoded).unwrap(), value);
    }

    let non_persistable = Value::relation(
        [Symbol::intern("value")],
        [Tuple::from([Value::capability(
            CapabilityId::new(9).unwrap(),
        )])],
    )
    .unwrap();
    assert!(!non_persistable.is_persistable());
    assert_eq!(
        encode_value(&non_persistable, &mut Vec::new()),
        Err(ValueCodecError::CapabilityNotEncodable)
    );
}

#[test]
fn value_kinds_have_canonical_names() {
    assert_eq!(ValueKind::Bool.name(), "bool");
    assert_eq!(ValueKind::Int.name(), "int");
    assert_eq!(ValueKind::Float.name(), "float");
    assert_eq!(ValueKind::Identity.name(), "identity");
    assert_eq!(ValueKind::Symbol.name(), "symbol");
    assert_eq!(ValueKind::ErrorCode.name(), "error_code");
    assert_eq!(ValueKind::String.name(), "string");
    assert_eq!(ValueKind::Bytes.name(), "bytes");
    assert_eq!(ValueKind::List.name(), "list");
    assert_eq!(ValueKind::Map.name(), "map");
    assert_eq!(ValueKind::Range.name(), "range");
    assert_eq!(ValueKind::Error.name(), "error");
    assert_eq!(ValueKind::Capability.name(), "capability");
    assert_eq!(ValueKind::Frob.name(), "frob");
    assert_eq!(ValueKind::Function.name(), "function");
    assert_eq!(ValueKind::Relation.name(), "relation");
}

#[test]
fn capability_values_are_ephemeral() {
    let cap = Value::capability_raw(1).unwrap();
    assert_eq!(cap.kind(), ValueKind::Capability);
    assert!(!cap.is_persistable());
    assert!(!Value::list([Value::int(1).unwrap(), cap.clone()]).is_persistable());
    assert!(!Value::map([(Value::symbol(Symbol::intern("cap")), cap.clone())]).is_persistable());
    assert!(!Value::error(Symbol::intern("E_CAP"), None::<Box<str>>, Some(cap)).is_persistable());
    assert!(Value::capability_raw(0).is_err());
}

#[test]
fn function_values_are_ephemeral() {
    let function = Value::function_raw(1).unwrap();
    assert_eq!(function.kind(), ValueKind::Function);
    assert_eq!(function.as_function().unwrap().raw(), 1);
    assert_eq!(format!("{function}"), "<function>");
    assert_eq!(format!("{function:?}"), "<function>");
    assert!(!function.is_persistable());
    assert!(!Value::list([Value::int(1).unwrap(), function.clone()]).is_persistable());
}

#[test]
fn persistable_relation_values_are_serializable_and_durable() {
    let name = Symbol::intern("name");
    let score = Symbol::intern("score");
    let relation = Value::relation(
        [score, name],
        [
            Tuple::from([Value::float(8.0).unwrap(), Value::string("Grace")]),
            Tuple::from([Value::float(9.5).unwrap(), Value::string("Ada")]),
        ],
    )
    .unwrap();

    assert_eq!(relation.kind(), ValueKind::Relation);
    let expected_heading = if name < score {
        vec![name, score]
    } else {
        vec![score, name]
    };
    assert_eq!(
        relation.with_relation(|relation| relation.heading().to_vec()),
        Some(expected_heading)
    );
    assert!(relation.is_persistable());

    let mut encoded = Vec::new();
    encode_value(&relation, &mut encoded).unwrap();
    assert_eq!(decode_value_exact(&encoded).unwrap(), relation);
}

#[test]
fn frob_values_carry_delegate_and_payload() {
    let delegate = Identity::new(42).unwrap();
    let payload = Value::map([(Value::symbol(Symbol::intern("item")), Value::string("coin"))]);
    let frob = Value::frob(delegate, payload.clone());

    assert_eq!(frob.kind(), ValueKind::Frob);
    assert_eq!(frob.frob_delegate(), Some(delegate));
    assert_eq!(frob.frob_value(), Some(&payload));
    assert_eq!(
        frob.with_frob(|delegate, value| (delegate, value.clone())),
        Some((delegate, payload.clone()))
    );
    assert_eq!(frob, Value::frob(delegate, payload.clone()));
    assert_ne!(
        frob,
        Value::frob(Identity::new(43).unwrap(), payload.clone())
    );
    assert_ne!(frob, Value::frob(delegate, Value::string("coin")));
}

#[test]
fn frob_persistability_follows_payload() {
    let delegate = Identity::new(42).unwrap();
    assert!(Value::frob(delegate, Value::string("coin")).is_persistable());
    assert!(!Value::frob(delegate, Value::capability_raw(1).unwrap()).is_persistable());
}

#[test]
fn symbols_intern_consistently_across_threads() {
    let handles = (0..8)
        .map(|_| {
            std::thread::spawn(|| {
                for _ in 0..256 {
                    assert_eq!(Symbol::intern("look"), Symbol::intern("look"));
                }
                Symbol::intern("look")
            })
        })
        .collect::<Vec<_>>();

    let expected = Symbol::intern("look");
    for handle in handles {
        assert_eq!(handle.join().unwrap(), expected);
    }
}

#[test]
fn float_is_reduced_precision_and_canonicalizes_zero() {
    let value = Value::float(1.25).unwrap();
    assert_eq!(value.as_float(), Some(1.25));
    assert_eq!(Value::float(-0.0).unwrap(), Value::float(0.0).unwrap());
    assert!(Value::float(f32::NAN).is_err());
    assert!(Value::float(f32::INFINITY).is_err());
    assert!(Value::float(f32::NEG_INFINITY).is_err());
}

#[test]
fn numeric_operations_preserve_ints_when_exact() {
    let six = Value::int(6).unwrap();
    let three = Value::int(3).unwrap();
    let four = Value::int(4).unwrap();

    assert_eq!(
        six.checked_add(&three).and_then(|value| value.as_int()),
        Some(9)
    );
    assert_eq!(
        six.checked_sub(&three).and_then(|value| value.as_int()),
        Some(3)
    );
    assert_eq!(
        six.checked_mul(&three).and_then(|value| value.as_int()),
        Some(18)
    );
    assert_eq!(
        six.checked_div(&three).and_then(|value| value.as_int()),
        Some(2)
    );
    assert_eq!(
        six.checked_rem(&four).and_then(|value| value.as_int()),
        Some(2)
    );
    assert_eq!(
        three.checked_neg().and_then(|value| value.as_int()),
        Some(-3)
    );
}

#[test]
fn numeric_operations_fall_back_to_floats() {
    let five = Value::int(5).unwrap();
    let two = Value::int(2).unwrap();
    let half = Value::float(0.5).unwrap();

    assert_eq!(
        five.checked_div(&two).and_then(|value| value.as_float()),
        Some(2.5)
    );
    assert_eq!(
        five.checked_add(&half).and_then(|value| value.as_float()),
        Some(5.5)
    );
    assert_eq!(five.checked_div(&Value::int(0).unwrap()), None);
}

#[test]
fn string_bytes_list_map_and_range_are_values() {
    let string = Value::string("brass lamp");
    assert_eq!(
        string.with_str(|s| s.to_string()),
        Some("brass lamp".to_string())
    );

    let bytes = Value::bytes([0xde, 0xad, 0xbe, 0xef]);
    assert_eq!(
        bytes.with_bytes(|value| value.to_vec()),
        Some(vec![0xde, 0xad, 0xbe, 0xef])
    );
    assert_eq!(format!("{bytes:?}"), "b\"3q2-7w==\"");

    let list = Value::list([Value::int(1).unwrap(), Value::int(2).unwrap()]);
    assert_eq!(list.with_list(|values| values.len()), Some(2));

    let k = Value::symbol(Symbol::intern("color"));
    let red = Value::string("red");
    let blue = Value::string("blue");
    let map = Value::map([(k.clone(), red), (k.clone(), blue)]);
    assert_eq!(map.with_map(|entries| entries.len()), Some(1));
    assert_eq!(
        map.with_map(|entries| entries[0].1.with_str(|s| s.to_string()).unwrap()),
        Some("blue".to_string())
    );
    assert_eq!(map.map_len(), Some(1));
    assert_eq!(
        map.map_get(&k)
            .and_then(|value| value.with_str(str::to_string)),
        Some("blue".to_string())
    );

    let range = Value::range(Value::int(1).unwrap(), Some(Value::int(3).unwrap()));
    assert_eq!(
        range.with_range(|start, end| (start.as_int(), end.and_then(Value::as_int))),
        Some((Some(1), Some(3)))
    );
    assert_eq!(format!("{range}"), "1..3");

    let open_range = Value::range(Value::int(2).unwrap(), None);
    assert_eq!(format!("{open_range}"), "2.._");
}

#[test]
fn heap_values_are_arc_shared_and_acyclic() {
    let value = Value::list([
        Value::string("alpha"),
        Value::symbol(Symbol::intern("beta")),
        Value::identity_raw(42).unwrap(),
    ]);
    assert_eq!(value.heap_strong_count(), Some(1));
    let cloned = value.clone();
    assert_eq!(value.heap_strong_count(), Some(2));
    assert_eq!(value, cloned);
    drop(value);
    assert_eq!(cloned.heap_strong_count(), Some(1));
    assert_eq!(cloned.list_len(), Some(3));
    assert_eq!(
        cloned
            .list_get(0)
            .and_then(|value| value.with_str(str::to_string)),
        Some("alpha".to_string())
    );
}

#[test]
fn value_refs_borrow_immediate_and_heap_payloads() {
    let identity = Identity::new(42).unwrap();
    assert_eq!(
        Value::identity(identity).as_value_ref(),
        ValueRef::Identity(identity)
    );

    let string = Value::string("brass lamp");
    assert_eq!(string.as_value_ref(), ValueRef::String("brass lamp"));
    assert_eq!(string.as_value_ref().kind(), ValueKind::String);
    assert!(string.as_value_ref().is_heap());
    assert_eq!(string.as_value_ref().child_count(), 0);

    let list = Value::list([Value::int(1).unwrap(), Value::int(2).unwrap()]);
    match list.as_value_ref() {
        ValueRef::List(values) => {
            assert_eq!(values.len(), 2);
            assert_eq!(values[0].as_int(), Some(1));
            assert_eq!(values[1].as_int(), Some(2));
        }
        value_ref => panic!("expected list ref, got {value_ref:?}"),
    }
    assert_eq!(list.as_value_ref().child_count(), 2);

    let relation = Value::relation(
        [Symbol::intern("payload")],
        [Tuple::from([Value::string("row value")])],
    )
    .unwrap();
    match relation.as_value_ref() {
        ValueRef::Relation(relation) => {
            assert_eq!(relation.len(), 1);
            assert_eq!(relation.rows()[0].values()[0], Value::string("row value"));
        }
        value_ref => panic!("expected relation ref, got {value_ref:?}"),
    }
    assert_eq!(relation.as_value_ref().child_count(), 1);

    let frob = Value::frob(identity, Value::string("payload"));
    match frob.as_value_ref() {
        ValueRef::Frob { delegate, value } => {
            assert_eq!(delegate, identity);
            assert_eq!(value.with_str(str::to_owned), Some("payload".to_owned()));
        }
        value_ref => panic!("expected frob ref, got {value_ref:?}"),
    }
    assert_eq!(frob.as_value_ref().child_count(), 1);
}

#[test]
fn value_walk_visits_nested_values_depth_first() {
    struct KindCollector(Vec<ValueKind>);

    impl ValueVisitor for KindCollector {
        type Error = std::convert::Infallible;

        fn visit_value(
            &mut self,
            _value: &Value,
            value_ref: ValueRef<'_>,
        ) -> Result<VisitDecision, Self::Error> {
            self.0.push(value_ref.kind());
            Ok(VisitDecision::Descend)
        }
    }

    let value = Value::map([(
        Value::symbol(Symbol::intern("items")),
        Value::list([
            Value::string("lamp"),
            Value::error(
                Symbol::intern("E_TEST"),
                Some("nested"),
                Some(Value::int(7).unwrap()),
            ),
        ]),
    )]);
    let mut collector = KindCollector(Vec::new());
    value.walk(&mut collector).unwrap();
    assert_eq!(
        collector.0,
        vec![
            ValueKind::Map,
            ValueKind::Symbol,
            ValueKind::List,
            ValueKind::String,
            ValueKind::Error,
            ValueKind::Int,
        ]
    );
}

#[test]
fn value_walk_visits_frob_payload() {
    struct KindCollector(Vec<ValueKind>);

    impl ValueVisitor for KindCollector {
        type Error = std::convert::Infallible;

        fn visit_value(
            &mut self,
            _value: &Value,
            value_ref: ValueRef<'_>,
        ) -> Result<VisitDecision, Self::Error> {
            self.0.push(value_ref.kind());
            Ok(VisitDecision::Descend)
        }
    }

    let value = Value::frob(Identity::new(42).unwrap(), Value::string("payload"));
    let mut collector = KindCollector(Vec::new());
    value.walk(&mut collector).unwrap();
    assert_eq!(collector.0, vec![ValueKind::Frob, ValueKind::String]);
}

#[test]
fn value_walk_visits_relation_cells() {
    struct KindCollector(Vec<ValueKind>);

    impl ValueVisitor for KindCollector {
        type Error = std::convert::Infallible;

        fn visit_value(
            &mut self,
            _value: &Value,
            value_ref: ValueRef<'_>,
        ) -> Result<VisitDecision, Self::Error> {
            self.0.push(value_ref.kind());
            Ok(VisitDecision::Descend)
        }
    }

    let value = Value::relation(
        [Symbol::intern("payload")],
        [Tuple::from([Value::list([
            Value::int(1).unwrap(),
            Value::bool(true),
        ])])],
    )
    .unwrap();
    let mut collector = KindCollector(Vec::new());
    value.walk(&mut collector).unwrap();
    assert_eq!(
        collector.0,
        vec![
            ValueKind::Relation,
            ValueKind::List,
            ValueKind::Int,
            ValueKind::Bool,
        ]
    );
}

#[test]
fn value_walk_can_skip_children() {
    struct SkippingCollector(Vec<ValueKind>);

    impl ValueVisitor for SkippingCollector {
        type Error = std::convert::Infallible;

        fn visit_value(
            &mut self,
            _value: &Value,
            value_ref: ValueRef<'_>,
        ) -> Result<VisitDecision, Self::Error> {
            self.0.push(value_ref.kind());
            if matches!(value_ref, ValueRef::List(_)) {
                Ok(VisitDecision::SkipChildren)
            } else {
                Ok(VisitDecision::Descend)
            }
        }
    }

    let value = Value::list([
        Value::string("hidden"),
        Value::list([Value::int(1).unwrap(), Value::int(2).unwrap()]),
    ]);
    let mut collector = SkippingCollector(Vec::new());
    value.walk(&mut collector).unwrap();
    assert_eq!(collector.0, vec![ValueKind::List]);
}

#[test]
fn value_walk_pairs_enter_and_leave_events() {
    struct EventCollector(Vec<(&'static str, ValueKind)>);

    impl ValueVisitor for EventCollector {
        type Error = std::convert::Infallible;

        fn visit_value(
            &mut self,
            _value: &Value,
            value_ref: ValueRef<'_>,
        ) -> Result<VisitDecision, Self::Error> {
            self.0.push(("enter", value_ref.kind()));
            Ok(VisitDecision::Descend)
        }

        fn leave_value(
            &mut self,
            _value: &Value,
            value_ref: ValueRef<'_>,
        ) -> Result<(), Self::Error> {
            self.0.push(("leave", value_ref.kind()));
            Ok(())
        }
    }

    let value = Value::range(Value::int(1).unwrap(), Some(Value::int(3).unwrap()));
    let mut collector = EventCollector(Vec::new());
    value.walk(&mut collector).unwrap();
    assert_eq!(
        collector.0,
        vec![
            ("enter", ValueKind::Range),
            ("enter", ValueKind::Int),
            ("leave", ValueKind::Int),
            ("enter", ValueKind::Int),
            ("leave", ValueKind::Int),
            ("leave", ValueKind::Range),
        ]
    );
}

#[test]
fn value_codec_round_trips_serializable_values() {
    let values = [
        Value::nothing(),
        Value::bool(true),
        Value::bool(false),
        Value::int(INT_MIN).unwrap(),
        Value::int(INT_MAX).unwrap(),
        Value::float(12.5).unwrap(),
        Value::identity(Identity::new(99).unwrap()),
        Value::symbol(Symbol::intern("symbolic")),
        Value::error_code(Symbol::intern("E_PERSIST")),
        Value::string("stored"),
        Value::bytes([0xde, 0xad, 0xbe, 0xef]),
        Value::list([
            Value::int(1).unwrap(),
            Value::string("two"),
            Value::bool(false),
        ]),
        Value::map([(Value::symbol(Symbol::intern("k")), Value::string("v"))]),
        Value::relation(
            [Symbol::intern("name"), Symbol::intern("score")],
            [
                Tuple::from([Value::string("Ada"), Value::float(9.5).unwrap()]),
                Tuple::from([Value::string("Grace"), Value::float(8.0).unwrap()]),
            ],
        )
        .unwrap(),
        Value::range(Value::int(1).unwrap(), Some(Value::int(4).unwrap())),
        Value::range(Value::int(2).unwrap(), None),
        Value::error(
            Symbol::intern("E_RICH"),
            Some("rich error"),
            Some(Value::int(7).unwrap()),
        ),
        Value::frob(Identity::new(42).unwrap(), Value::string("payload")),
    ];

    for value in values {
        let mut encoded = Vec::new();
        encode_value(&value, &mut encoded).unwrap();
        let (decoded, consumed) = decode_value(&encoded).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, value);
        assert_eq!(decode_value_exact(&encoded).unwrap(), value);
    }
}

#[test]
fn value_codec_uses_little_endian_inline_words() {
    let values = [
        Value::nothing(),
        Value::bool(true),
        Value::int(-123).unwrap(),
        Value::float(12.5).unwrap(),
        Value::identity(Identity::new(99).unwrap()),
    ];

    for value in values {
        let mut encoded = Vec::new();
        encode_value(&value, &mut encoded).unwrap();
        assert_eq!(encoded, value.raw_bits().to_le_bytes());
        assert_eq!(decode_value_exact(&encoded).unwrap(), value);
    }
}

#[test]
fn value_codec_sink_matches_vec_encoder() {
    #[derive(Default)]
    struct ChunkSink(Vec<Vec<u8>>);

    impl ValueSink for ChunkSink {
        fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), ValueCodecError> {
            self.0.push(bytes.to_vec());
            Ok(())
        }
    }

    let values = [
        Value::nothing(),
        Value::identity(Identity::new(42).unwrap()),
        Value::string("brass lamp"),
        Value::bytes([1, 2, 3, 4]),
        Value::list([
            Value::symbol(Symbol::intern("item")),
            Value::error(
                Symbol::intern("E_TEST"),
                Some("nested"),
                Some(Value::int(7).unwrap()),
            ),
        ]),
        Value::relation(
            [Symbol::intern("payload")],
            [Tuple::from([Value::string("relation payload")])],
        )
        .unwrap(),
        Value::frob(Identity::new(42).unwrap(), Value::string("payload")),
    ];

    for value in values {
        let mut expected = Vec::new();
        encode_value(&value, &mut expected).unwrap();

        let mut sink = ChunkSink::default();
        encode_value_to_sink(&value, &mut sink, ValueCodecOptions::default()).unwrap();
        let actual = sink.0.concat();
        assert_eq!(actual, expected);
    }
}

#[test]
fn value_codec_segments_match_vec_encoder_and_borrow_payloads() {
    let values = [
        Value::identity(Identity::new(42).unwrap()),
        Value::string("brass lamp"),
        Value::bytes([1, 2, 3, 4]),
        Value::list([
            Value::symbol(Symbol::intern("item")),
            Value::error(
                Symbol::intern("E_TEST"),
                Some("nested"),
                Some(Value::int(7).unwrap()),
            ),
        ]),
        Value::relation(
            [Symbol::intern("payload")],
            [Tuple::from([Value::string("relation payload")])],
        )
        .unwrap(),
        Value::frob(Identity::new(42).unwrap(), Value::string("payload")),
    ];

    for value in values {
        let mut expected = Vec::new();
        encode_value(&value, &mut expected).unwrap();

        let segments = encode_value_segments(&value).unwrap();
        assert_eq!(segments.to_vec(), expected);
        assert_eq!(segments.len(), expected.len());
        assert!(!segments.is_empty());
    }

    let string = Value::string("brass lamp");
    let string_segments = encode_value_segments(&string).unwrap();
    assert!(matches!(
        string_segments.segments(),
        [
            ValueSegment::Scratch(_),
            ValueSegment::Borrowed(b"brass lamp")
        ]
    ));

    let bytes = Value::bytes([1, 2, 3, 4]);
    let byte_segments = encode_value_segments(&bytes).unwrap();
    assert!(matches!(
        byte_segments.segments(),
        [
            ValueSegment::Scratch(_),
            ValueSegment::Borrowed([1, 2, 3, 4])
        ]
    ));

    let relation = Value::relation(
        [Symbol::intern("payload")],
        [Tuple::from([Value::string("relation payload")])],
    )
    .unwrap();
    let relation_segments = encode_value_segments(&relation).unwrap();
    assert!(
        relation_segments
            .segments()
            .iter()
            .any(|segment| matches!(segment, ValueSegment::Borrowed(b"relation payload")))
    );
}

#[test]
fn value_codec_segments_support_inline_symbol_ids() {
    let options = ValueCodecOptions {
        symbol_encoding: SymbolEncoding::Id,
        allow_capabilities: false,
    };
    let symbol = Value::symbol(Symbol::from_id(123));

    let mut expected = Vec::new();
    encode_value_with_options(&symbol, &mut expected, options).unwrap();

    let segments = encode_value_segments_with_options(&symbol, options).unwrap();
    assert_eq!(segments.to_vec(), expected);
    assert!(matches!(segments.segments(), [ValueSegment::Scratch(_)]));
}

#[test]
fn value_codec_defaults_to_named_symbol_records() {
    let symbol = Value::symbol(Symbol::intern("symbolic"));
    let mut encoded = Vec::new();
    encode_value(&symbol, &mut encoded).unwrap();

    assert_ne!(encoded, symbol.raw_bits().to_le_bytes());
    assert_eq!(encoded.len(), 8 + "symbolic".len());
    let header = u64::from_le_bytes(encoded[0..8].try_into().unwrap());
    assert_eq!((header >> 56) as u8, 0xff);
    assert_eq!(((header >> 48) & 0xff) as u8, ValueKind::Symbol as u8);
    assert_eq!(
        decode_value_exact(&encoded).unwrap().as_symbol(),
        Some(Symbol::intern("symbolic"))
    );
}

#[test]
fn value_codec_can_use_inline_symbol_ids_when_requested() {
    let options = ValueCodecOptions {
        symbol_encoding: SymbolEncoding::Id,
        allow_capabilities: false,
    };
    let symbol = Value::symbol(Symbol::from_id(u32::MAX));
    let mut encoded = Vec::new();
    encode_value_with_options(&symbol, &mut encoded, options).unwrap();

    assert_eq!(encoded, symbol.raw_bits().to_le_bytes());
    assert_eq!(
        decode_value_exact_with_options(&encoded, options).unwrap(),
        symbol
    );
    assert!(decode_value_exact(&encoded).is_err());
}

#[test]
fn value_codec_rejects_ephemeral_capabilities() {
    let cap = Value::capability_raw(1).unwrap();
    let mut encoded = Vec::new();
    assert_eq!(
        encode_value(&cap, &mut encoded),
        Err(ValueCodecError::CapabilityNotEncodable)
    );

    let encoded_cap = cap.raw_bits().to_le_bytes();
    assert_eq!(
        decode_value(&encoded_cap),
        Err(ValueCodecError::CapabilityNotDecodable)
    );

    let options = ValueCodecOptions {
        symbol_encoding: SymbolEncoding::Name,
        allow_capabilities: true,
    };
    encode_value_with_options(&cap, &mut encoded, options).unwrap();
    assert_eq!(
        decode_value_exact_with_options(&encoded, options).unwrap(),
        cap
    );
}

#[test]
fn value_codec_rejects_ephemeral_functions() {
    let function = Value::function_raw(1).unwrap();
    let mut encoded = Vec::new();
    assert_eq!(
        encode_value(&function, &mut encoded),
        Err(ValueCodecError::FunctionNotEncodable)
    );

    let encoded_function = function.raw_bits().to_le_bytes();
    assert_eq!(
        decode_value(&encoded_function),
        Err(ValueCodecError::FunctionNotDecodable)
    );
}

#[test]
fn value_codec_rejects_unnamed_symbols_and_trailing_bytes() {
    let mut encoded = Vec::new();
    assert_eq!(
        encode_value(&Value::symbol(Symbol::from_id(u32::MAX)), &mut encoded),
        Err(ValueCodecError::UnnamedSymbol(u32::MAX))
    );

    encoded.clear();
    encode_value(&Value::nothing(), &mut encoded).unwrap();
    encoded.push(0xff);
    assert_eq!(
        decode_value_exact(&encoded),
        Err(ValueCodecError::TrailingBytes(1))
    );
}

#[test]
fn value_codec_rejects_malformed_inline_words() {
    let mut heap_word = 0u64;
    heap_word |= (ValueKind::String as u64) << 56;
    assert_eq!(
        decode_value(&heap_word.to_le_bytes()),
        Err(ValueCodecError::InlineHeapValue(ValueKind::String as u8))
    );

    let invalid_bool = ((ValueKind::Bool as u64) << 56) | 2;
    assert_eq!(
        decode_value(&invalid_bool.to_le_bytes()),
        Err(ValueCodecError::InvalidBoolPayload(2))
    );
}

#[test]
fn value_codec_rejects_noncanonical_zero_arity_relation_count() {
    let relation_header = ((0xff_u64) << 56) | ((ValueKind::Relation as u64) << 48);
    let mut encoded = relation_header.to_le_bytes().to_vec();
    encoded.extend_from_slice(&2_u64.to_le_bytes());

    assert_eq!(
        decode_value_exact(&encoded),
        Err(ValueCodecError::InvalidValue(
            "zero-arity relation has more than one row".to_owned()
        ))
    );
}

#[test]
fn list_slices_return_new_list_values() {
    let list = Value::list([
        Value::int(10).unwrap(),
        Value::int(20).unwrap(),
        Value::int(30).unwrap(),
        Value::int(40).unwrap(),
    ]);

    let slice = list.list_slice(1, 3).unwrap();
    assert_eq!(slice.list_len(), Some(2));
    assert_eq!(slice.list_get(0).and_then(|value| value.as_int()), Some(20));
    assert_eq!(slice.list_get(1).and_then(|value| value.as_int()), Some(30));
    assert!(list.list_slice(3, 5).is_none());
}

#[test]
fn indexed_updates_return_new_collection_values() {
    let list = Value::list([
        Value::int(1).unwrap(),
        Value::int(2).unwrap(),
        Value::int(3).unwrap(),
    ]);
    let updated = list
        .index_set(&Value::int(1).unwrap(), Value::int(20).unwrap())
        .unwrap();
    assert_eq!(list.list_get(1).and_then(|value| value.as_int()), Some(2));
    assert_eq!(
        updated.list_get(1).and_then(|value| value.as_int()),
        Some(20)
    );
    assert!(
        list.index_set(&Value::int(10).unwrap(), Value::int(20).unwrap())
            .is_none()
    );

    let key = Value::symbol(Symbol::intern("count"));
    let map = Value::map([(key.clone(), Value::int(1).unwrap())]);
    let replaced = map.index_set(&key, Value::int(2).unwrap()).unwrap();
    assert_eq!(map.map_get(&key).and_then(|value| value.as_int()), Some(1));
    assert_eq!(
        replaced.map_get(&key).and_then(|value| value.as_int()),
        Some(2)
    );

    let inserted_key = Value::symbol(Symbol::intern("other"));
    let inserted = replaced
        .index_set(&inserted_key, Value::int(3).unwrap())
        .unwrap();
    assert_eq!(
        inserted
            .map_get(&inserted_key)
            .and_then(|value| value.as_int()),
        Some(3)
    );
}

#[test]
fn total_order_is_stable() {
    let values = vec![
        Value::relation([], []).unwrap(),
        Value::map([]),
        Value::function_raw(1).unwrap(),
        Value::capability_raw(1).unwrap(),
        Value::error(Symbol::intern("E_TEST"), None::<Box<str>>, None),
        Value::range(Value::int(1).unwrap(), None),
        Value::list([]),
        Value::bytes([1, 2, 3]),
        Value::string("x"),
        Value::error_code(Symbol::intern("E_NOT_PORTABLE")),
        Value::symbol(Symbol::intern("x")),
        Value::identity_raw(1).unwrap(),
        Value::float(1.0).unwrap(),
        Value::int(1).unwrap(),
        Value::bool(true),
        Value::nothing(),
    ];
    let mut sorted = values.clone();
    sorted.sort();
    for pair in sorted.windows(2) {
        assert!(pair[0] <= pair[1]);
    }
    assert_eq!(sorted.first(), Some(&Value::bool(true)));
    assert_eq!(sorted.last().unwrap().kind(), ValueKind::Relation);
}

#[test]
fn ordered_encoding_preserves_string_order() {
    let a = Value::string("a").ordered_key_bytes();
    let aa = Value::string("aa").ordered_key_bytes();
    let b = Value::string("b").ordered_key_bytes();
    assert!(a < aa);
    assert!(aa < b);
}

#[test]
fn ordered_encoding_preserves_bytes_order() {
    let a = Value::bytes([0x00]).ordered_key_bytes();
    let aa = Value::bytes([0x00, 0x00]).ordered_key_bytes();
    let b = Value::bytes([0x01]).ordered_key_bytes();
    assert!(a < aa);
    assert!(aa < b);
}

#[test]
fn arithmetic_fast_path() {
    assert_eq!(
        Value::int(41).unwrap().checked_add(&Value::int(1).unwrap()),
        Some(Value::int(42).unwrap())
    );
    assert_eq!(
        Value::float(1.5)
            .unwrap()
            .checked_add(&Value::int(2).unwrap())
            .unwrap()
            .as_float(),
        Some(3.5)
    );
}

#[test]
fn float_rejects_non_finite() {
    assert!(Value::float(f32::NAN).is_err());
    assert!(Value::float(f32::INFINITY).is_err());
    assert!(Value::float(f32::NEG_INFINITY).is_err());
}

#[test]
fn float_from_bits_rejects_non_finite_and_canonicalizes_zero() {
    assert!(Value::float_from_bits(0x7f80_0000).is_err()); // +inf
    assert!(Value::float_from_bits(0xff80_0000).is_err()); // -inf
    assert!(Value::float_from_bits(0x7fc0_0000).is_err()); // NaN
    assert!(Value::float_from_bits(0x0000_0001).is_ok()); // smallest subnormal
    assert!(Value::float_from_bits(0x0080_0000).is_ok()); // smallest normal
    let neg_zero = Value::float_from_bits(0x8000_0000).unwrap();
    let pos_zero = Value::float_from_bits(0x0000_0000).unwrap();
    assert_eq!(neg_zero, pos_zero);
}

#[test]
fn language_numeric_eq_distinguishes_from_canonical() {
    let one_int = Value::int(1).unwrap();
    let one_float = Value::float(1.0).unwrap();
    // Canonical: distinct kinds, not equal
    assert_ne!(one_int, one_float);
    // Language: numerically equal
    assert!(crate::language_cmp::numeric_eq(&one_int, &one_float));
    assert!(crate::language_cmp::numeric_eq(&one_float, &one_int));
}

#[test]
fn numeric_equality_does_not_merge_canonical_map_keys() {
    let integer = Value::int(1).unwrap();
    let float = Value::float(1.0).unwrap();
    let map = Value::map([
        (integer.clone(), Value::string("integer")),
        (float.clone(), Value::string("float")),
    ]);

    assert!(crate::language_cmp::numeric_eq(&integer, &float));
    assert_eq!(map.map_len(), Some(2));
    assert_eq!(
        map.map_get(&integer)
            .and_then(|value| value.with_str(str::to_owned)),
        Some("integer".to_owned())
    );
    assert_eq!(
        map.map_get(&float)
            .and_then(|value| value.with_str(str::to_owned)),
        Some("float".to_owned())
    );
}

#[test]
fn compare_int_float_respects_binary32_precision_boundary() {
    use crate::language_cmp::compare_int_float;
    use std::cmp::Ordering;

    let two_to_24 = f32::from_bits(0x4b80_0000);
    let below = f32::from_bits(two_to_24.to_bits() - 1);
    let above = f32::from_bits(two_to_24.to_bits() + 1);

    assert_eq!(below, 16_777_215.0);
    assert_eq!(two_to_24, 16_777_216.0);
    assert_eq!(above, 16_777_218.0);
    assert_eq!(compare_int_float(16_777_215, below), Ordering::Equal);
    assert_eq!(compare_int_float(16_777_216, two_to_24), Ordering::Equal);
    assert_eq!(compare_int_float(16_777_217, two_to_24), Ordering::Greater);
    assert_eq!(compare_int_float(16_777_217, above), Ordering::Less);
    assert_eq!(compare_int_float(-16_777_217, -two_to_24), Ordering::Less);
    assert_eq!(compare_int_float(-16_777_217, -above), Ordering::Greater);
}

#[test]
fn language_numeric_cmp_orders_by_value() {
    let one_int = Value::int(1).unwrap();
    let one_float = Value::float(1.0).unwrap();
    let two_int = Value::int(2).unwrap();
    let two_float = Value::float(2.0).unwrap();
    use std::cmp::Ordering;

    assert_eq!(
        crate::language_cmp::numeric_cmp(&one_int, &two_float),
        Ordering::Less
    );
    assert_eq!(
        crate::language_cmp::numeric_cmp(&two_int, &one_float),
        Ordering::Greater
    );
    assert_eq!(
        crate::language_cmp::numeric_cmp(&one_int, &one_float),
        Ordering::Equal
    );
    assert_eq!(
        crate::language_cmp::numeric_cmp(&one_float, &one_int),
        Ordering::Equal
    );
    assert_eq!(
        crate::language_cmp::numeric_cmp(&two_float, &one_int),
        Ordering::Greater
    );
}

#[test]
fn compare_int_float_boundary_constants() {
    use crate::language_cmp::compare_int_float;
    use std::cmp::Ordering;

    // Mica integer range boundaries.
    let int_max = (1i64 << 55) - 1; // 2^55 - 1
    let int_min = -(1i64 << 55); // -2^55

    // Float at the boundary: 2^55 is exactly representable.
    let upper_float = f32::from_bits(0x5b00_0000);
    assert_eq!(upper_float, 2.0f32.powi(55));

    // Every Mica integer (up to INT_MAX) is less than a float at 2^55.
    assert_eq!(compare_int_float(int_max, upper_float), Ordering::Less);
    assert_eq!(compare_int_float(int_max - 1, upper_float), Ordering::Less);

    // -2^55: the minimum Mica integer compares equal to the float -2^55.
    let lower_float = f32::from_bits(0xdb00_0000);
    assert_eq!(lower_float, -(2.0f32.powi(55)));
    assert_eq!(compare_int_float(int_min, lower_float), Ordering::Equal);
    assert_eq!(
        compare_int_float(int_min + 1, lower_float),
        Ordering::Greater
    );

    // The binary32 neighbours exercise each out-of-range branch as well as
    // the exact endpoint. The spacing here is much larger than one integer,
    // so none of these comparisons may round the Mica integer first.
    let upper_below = f32::from_bits(upper_float.to_bits() - 1);
    let upper_above = f32::from_bits(upper_float.to_bits() + 1);
    assert_eq!(compare_int_float(int_max, upper_below), Ordering::Greater);
    assert_eq!(compare_int_float(int_max, upper_above), Ordering::Less);

    let lower_below = f32::from_bits(lower_float.to_bits() + 1);
    let lower_above = f32::from_bits(lower_float.to_bits() - 1);
    assert_eq!(compare_int_float(int_min, lower_below), Ordering::Greater);
    assert_eq!(compare_int_float(int_min, lower_above), Ordering::Less);
}

#[test]
fn compare_int_float_fractional_branch() {
    use crate::language_cmp::compare_int_float;
    use std::cmp::Ordering;

    // 3.5 truncates to 3, with a positive fraction.
    assert_eq!(compare_int_float(3, 3.5), Ordering::Less);
    assert_eq!(compare_int_float(4, 3.5), Ordering::Greater);
    assert_eq!(compare_int_float(3, 3.0), Ordering::Equal);

    // Negative fractions.
    assert_eq!(compare_int_float(-3, -3.5), Ordering::Greater);
    assert_eq!(compare_int_float(-4, -3.5), Ordering::Less);
    assert_eq!(compare_int_float(-3, -3.0), Ordering::Equal);

    // Float-on-left reverses.
    assert_eq!(
        crate::language_cmp::numeric_cmp(&Value::float(3.5).unwrap(), &Value::int(3).unwrap()),
        Ordering::Greater
    );
}

#[test]
fn float_arithmetic_is_binary32() {
    // 0.1 in binary32 is not 0.1 in binary64. Verify arithmetic stays binary32.
    let f = Value::float(0.1).unwrap();
    let sum = f.checked_add(&Value::float(0.2).unwrap()).unwrap();
    let expected = Value::float(0.1f32 + 0.2f32).unwrap();
    assert_eq!(sum.as_float(), expected.as_float());

    // Non-finite result rejected.
    let big = Value::float(f32::MAX).unwrap();
    assert!(big.checked_mul(&Value::float(f32::MAX).unwrap()).is_none());
}

#[test]
fn division_result_kind_rule() {
    // Exact integer division stays integer.
    assert_eq!(
        Value::int(6).unwrap().checked_div(&Value::int(2).unwrap()),
        Some(Value::int(3).unwrap())
    );
    // Non-exact integer division produces float.
    let result = Value::int(5)
        .unwrap()
        .checked_div(&Value::int(2).unwrap())
        .unwrap();
    assert!(result.as_float().is_some());
    assert!(result.as_int().is_none());
}

#[test]
fn float_codec_rejects_non_finite_bits() {
    // Crafted NaN payload through codec.
    let nan_bits = 0x7fc0_0000u64;
    let word = (3u64 << 56) | nan_bits;
    let encoded = word.to_le_bytes();
    assert!(decode_value(&encoded).is_err());
}

#[test]
fn float_zero_canonicalizes_through_codec() {
    let neg_zero_bits = 0x8000_0000u64;
    let word = (3u64 << 56) | neg_zero_bits;
    let encoded = word.to_le_bytes();
    let decoded = decode_value(&encoded).unwrap().0;
    assert_eq!(decoded, Value::float(0.0).unwrap());
}
