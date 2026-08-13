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

use crate::{KernelError, RelationId, RelationRead, ScanControl, delegates_reaches};
use mica_var::{FROB_PROTOTYPE, Identity, Symbol, Value, primitive_prototype_for_value};
use std::sync::{Arc, OnceLock};

fn unrestricted_marker() -> &'static Value {
    static MARKER: OnceLock<Value> = OnceLock::new();
    MARKER.get_or_init(|| Value::symbol(Symbol::intern("dispatch/unrestricted")))
}

fn frob_only_marker() -> &'static Value {
    static MARKER: OnceLock<Value> = OnceLock::new();
    MARKER.get_or_init(|| Value::symbol(Symbol::intern("dispatch/frob_only")))
}

pub fn unrestricted_dispatch_restriction() -> Value {
    unrestricted_marker().clone()
}

pub fn frob_only_dispatch_restriction(delegate: Identity) -> Value {
    Value::frob(delegate, frob_only_marker().clone())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DispatchRelations {
    pub method_selector: RelationId,
    pub param: RelationId,
    pub delegates: RelationId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicableMethod {
    pub method: Value,
    pub params: Vec<crate::Tuple>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicableMethodCall {
    pub method: Value,
    pub args: Option<Vec<Value>>,
}

pub trait DispatchRead: RelationRead {
    fn cached_applicable_method_calls(
        &self,
        _relations: DispatchRelations,
        _selector: &Value,
        _roles: &[(Value, Value)],
    ) -> Result<Option<Vec<ApplicableMethodCall>>, KernelError> {
        Ok(None)
    }

    fn cached_applicable_method_calls_normalized(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        roles: &[(Value, Value)],
    ) -> Result<Option<Vec<ApplicableMethodCall>>, KernelError> {
        self.cached_applicable_method_calls(relations, selector, roles)
    }

    fn cached_method_program(
        &self,
        _relation: RelationId,
        _method: &Value,
    ) -> Result<Option<Option<Value>>, KernelError> {
        Ok(None)
    }

    fn cached_applicable_positional_methods(
        &self,
        _relations: DispatchRelations,
        _selector: &Value,
        _args: &[Value],
    ) -> Result<Option<Arc<[Value]>>, KernelError> {
        Ok(None)
    }
}

pub fn applicable_methods(
    reader: &impl RelationRead,
    relations: DispatchRelations,
    selector: Value,
    roles: impl IntoIterator<Item = (Value, Value)>,
) -> Result<Vec<Value>, KernelError> {
    let roles = roles.into_iter().collect::<Vec<_>>();
    Ok(
        applicable_method_entries(reader, relations, selector, &roles)?
            .into_iter()
            .map(|entry| entry.method)
            .collect(),
    )
}

pub fn applicable_method_entries(
    reader: &impl RelationRead,
    relations: DispatchRelations,
    selector: Value,
    roles: &[(Value, Value)],
) -> Result<Vec<ApplicableMethod>, KernelError> {
    let mut methods = Vec::new();

    reader.visit_relation(
        relations.method_selector,
        &[None, Some(selector)],
        &mut |row| {
            let method = row.values()[0].clone();
            let mut params = Vec::new();
            reader.visit_relation(
                relations.param,
                &[Some(method.clone()), None, None, None],
                &mut |param| {
                    params.push(param.clone());
                    Ok(ScanControl::Continue)
                },
            )?;
            if params_match(reader, relations.delegates, roles, &params)? {
                methods.push(ApplicableMethod { method, params });
            }
            Ok(ScanControl::Continue)
        },
    )?;

    methods.sort_by(|left, right| left.method.cmp(&right.method));
    methods.dedup_by(|left, right| left.method == right.method);
    prune_named_methods(reader, relations.delegates, methods)
}

pub fn applicable_method_calls(
    reader: &impl DispatchRead,
    relations: DispatchRelations,
    selector: Value,
    roles: &[(Value, Value)],
) -> Result<Vec<ApplicableMethodCall>, KernelError> {
    if let Some(methods) = reader.cached_applicable_method_calls(relations, &selector, roles)? {
        return Ok(methods);
    }
    applicable_method_calls_uncached(reader, relations, &selector, roles)
}

pub fn applicable_method_calls_normalized(
    reader: &impl DispatchRead,
    relations: DispatchRelations,
    selector: Value,
    roles: &[(Value, Value)],
) -> Result<Vec<ApplicableMethodCall>, KernelError> {
    if let Some(methods) =
        reader.cached_applicable_method_calls_normalized(relations, &selector, roles)?
    {
        return Ok(methods);
    }
    applicable_method_calls_uncached(reader, relations, &selector, roles)
}

pub fn method_program_id(
    reader: &impl DispatchRead,
    relation: RelationId,
    method: &Value,
) -> Result<Option<Value>, KernelError> {
    if let Some(cached) = reader.cached_method_program(relation, method)? {
        return Ok(cached);
    }

    method_program_id_uncached(reader, relation, method)
}

pub(crate) fn method_program_id_uncached(
    reader: &impl RelationRead,
    relation: RelationId,
    method: &Value,
) -> Result<Option<Value>, KernelError> {
    let mut program = None;
    reader.visit_relation(relation, &[Some(method.clone()), None], &mut |row| {
        program = Some(row.values()[1].clone());
        Ok(ScanControl::Stop)
    })?;
    Ok(program)
}

pub(crate) fn applicable_method_calls_uncached(
    reader: &impl RelationRead,
    relations: DispatchRelations,
    selector: &Value,
    roles: &[(Value, Value)],
) -> Result<Vec<ApplicableMethodCall>, KernelError> {
    applicable_method_entries(reader, relations, selector.clone(), roles).map(|methods| {
        methods
            .into_iter()
            .map(|entry| ApplicableMethodCall {
                args: method_call_args_from_params(&entry.params, roles),
                method: entry.method,
            })
            .collect()
    })
}

pub fn applicable_positional_methods(
    reader: &impl RelationRead,
    relations: DispatchRelations,
    selector: Value,
    args: &[Value],
) -> Result<Vec<Value>, KernelError> {
    let mut methods = Vec::new();

    reader.visit_relation(
        relations.method_selector,
        &[None, Some(selector)],
        &mut |row| {
            let method = row.values()[0].clone();
            let mut params = Vec::new();
            reader.visit_relation(
                relations.param,
                &[Some(method.clone()), None, None, None],
                &mut |param| {
                    params.push(param.clone());
                    Ok(ScanControl::Continue)
                },
            )?;
            if positional_params_match(reader, relations.delegates, args, &params)? {
                methods.push(ApplicableMethod { method, params });
            }
            Ok(ScanControl::Continue)
        },
    )?;

    methods.sort_by(|left, right| left.method.cmp(&right.method));
    methods.dedup_by(|left, right| left.method == right.method);
    Ok(
        prune_positional_methods(reader, relations.delegates, methods)?
            .into_iter()
            .map(|entry| entry.method)
            .collect(),
    )
}

pub fn applicable_positional_methods_cached(
    reader: &impl DispatchRead,
    relations: DispatchRelations,
    selector: Value,
    args: &[Value],
) -> Result<Arc<[Value]>, KernelError> {
    if let Some(methods) =
        reader.cached_applicable_positional_methods(relations, &selector, args)?
    {
        return Ok(methods);
    }
    applicable_positional_methods(reader, relations, selector, args).map(Arc::from)
}

fn method_call_args_from_params(
    params: &[crate::Tuple],
    roles: &[(Value, Value)],
) -> Option<Vec<Value>> {
    let mut args = Vec::with_capacity(params.len());
    let mut ordered = params.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|param| param_position(param).unwrap_or(u16::MAX));
    if ordered.iter().any(|param| param_position(param).is_none()) {
        return None;
    }

    for param in ordered {
        let value = role_value(roles, &param.values()[1])?;
        args.push(value.clone());
    }

    Some(args)
}

fn prune_named_methods(
    reader: &impl RelationRead,
    delegates_relation: RelationId,
    methods: Vec<ApplicableMethod>,
) -> Result<Vec<ApplicableMethod>, KernelError> {
    let mut pruned = Vec::new();
    for candidate in &methods {
        let mut dominated = false;
        for other in &methods {
            if candidate.method == other.method {
                continue;
            }
            if named_method_more_specific(reader, delegates_relation, other, candidate)? {
                dominated = true;
                break;
            }
        }
        if !dominated {
            pruned.push(candidate.clone());
        }
    }
    Ok(pruned)
}

fn prune_positional_methods(
    reader: &impl RelationRead,
    delegates_relation: RelationId,
    methods: Vec<ApplicableMethod>,
) -> Result<Vec<ApplicableMethod>, KernelError> {
    let mut pruned = Vec::new();
    for candidate in &methods {
        let mut dominated = false;
        for other in &methods {
            if candidate.method == other.method {
                continue;
            }
            if positional_method_more_specific(reader, delegates_relation, other, candidate)? {
                dominated = true;
                break;
            }
        }
        if !dominated {
            pruned.push(candidate.clone());
        }
    }
    Ok(pruned)
}

fn named_method_more_specific(
    reader: &impl RelationRead,
    delegates_relation: RelationId,
    left: &ApplicableMethod,
    right: &ApplicableMethod,
) -> Result<bool, KernelError> {
    let mut stricter = left.params.len() > right.params.len();

    for right_param in &right.params {
        let role = &right_param.values()[1];
        let Some(left_param) = param_for_role(&left.params, role) else {
            return Ok(false);
        };
        let left_restriction = &left_param.values()[2];
        let right_restriction = &right_param.values()[2];
        if !restriction_implies(
            reader,
            delegates_relation,
            left_restriction,
            right_restriction,
        )? {
            return Ok(false);
        }
        if !restriction_implies(
            reader,
            delegates_relation,
            right_restriction,
            left_restriction,
        )? {
            stricter = true;
        }
    }

    Ok(stricter)
}

fn positional_method_more_specific(
    reader: &impl RelationRead,
    delegates_relation: RelationId,
    left: &ApplicableMethod,
    right: &ApplicableMethod,
) -> Result<bool, KernelError> {
    let Some(left_params) = ordered_params(&left.params) else {
        return Ok(false);
    };
    let Some(right_params) = ordered_params(&right.params) else {
        return Ok(false);
    };
    if left_params.len() != right_params.len() {
        return Ok(false);
    }

    let mut stricter = false;
    for (left_param, right_param) in left_params.iter().zip(right_params.iter()) {
        let left_restriction = &left_param.values()[2];
        let right_restriction = &right_param.values()[2];
        if !restriction_implies(
            reader,
            delegates_relation,
            left_restriction,
            right_restriction,
        )? {
            return Ok(false);
        }
        if !restriction_implies(
            reader,
            delegates_relation,
            right_restriction,
            left_restriction,
        )? {
            stricter = true;
        }
    }

    Ok(stricter)
}

fn param_for_role<'a>(params: &'a [crate::Tuple], role: &Value) -> Option<&'a crate::Tuple> {
    params.iter().find(|param| &param.values()[1] == role)
}

fn restriction_implies(
    reader: &impl RelationRead,
    delegates_relation: RelationId,
    specific: &Value,
    general: &Value,
) -> Result<bool, KernelError> {
    if specific == general || general == unrestricted_marker() {
        return Ok(true);
    }
    if specific == unrestricted_marker() {
        return Ok(false);
    }
    matches_restriction(reader, delegates_relation, specific, general)
}

fn params_match(
    reader: &impl RelationRead,
    delegates_relation: RelationId,
    roles: &[(Value, Value)],
    params: &[crate::Tuple],
) -> Result<bool, KernelError> {
    for param in params {
        let role = &param.values()[1];
        let restriction = &param.values()[2];
        let Some(value) = role_value(roles, role) else {
            return Ok(false);
        };
        if !matches_restriction(reader, delegates_relation, value, restriction)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn role_value<'a>(roles: &'a [(Value, Value)], role: &Value) -> Option<&'a Value> {
    roles
        .iter()
        .find_map(|(candidate, value)| (candidate == role).then_some(value))
}

pub fn normalize_dispatch_roles(roles: &mut [(Value, Value)]) {
    roles.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
}

pub fn positional_method_args(
    params: &[crate::Tuple],
    args: &[Value],
) -> Option<Vec<(Value, Value)>> {
    let mut params = ordered_params(params)?;
    if params.len() != args.len() {
        return None;
    }
    Some(
        params
            .drain(..)
            .zip(args)
            .map(|(param, value)| (param.values()[1].clone(), value.clone()))
            .collect(),
    )
}

pub fn ordered_params(params: &[crate::Tuple]) -> Option<Vec<crate::Tuple>> {
    let mut params = params.to_vec();
    params.sort_by_key(|param| param_position(param).unwrap_or(u16::MAX));
    if params.iter().any(|param| param_position(param).is_none()) {
        return None;
    }
    Some(params)
}

pub fn named_method_args(params: &[crate::Tuple], roles: &[(Value, Value)]) -> Option<Vec<Value>> {
    let mut args = Vec::with_capacity(params.len());

    if params.len() <= 1 {
        for param in params {
            param_position(param)?;
            if let Some(value) = role_value(roles, &param.values()[1]) {
                args.push(value.clone());
            }
        }
        return Some(args);
    }

    let mut ordered = params.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|param| param_position(param).unwrap_or(u16::MAX));
    if ordered.iter().any(|param| param_position(param).is_none()) {
        return None;
    }

    for param in ordered {
        if let Some(value) = role_value(roles, &param.values()[1]) {
            args.push(value.clone());
        }
    }
    Some(args)
}

fn positional_params_match(
    reader: &impl RelationRead,
    delegates_relation: RelationId,
    args: &[Value],
    params: &[crate::Tuple],
) -> Result<bool, KernelError> {
    let Some(params) = ordered_params(params) else {
        return Ok(false);
    };
    if params.len() != args.len() {
        return Ok(false);
    }
    for (param, value) in params.iter().zip(args) {
        if !matches_restriction(reader, delegates_relation, value, &param.values()[2])? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn param_position(param: &crate::Tuple) -> Option<u16> {
    let raw = param.values().get(3)?.as_int()?;
    u16::try_from(raw).ok()
}

fn matches_restriction(
    reader: &impl RelationRead,
    delegates_relation: RelationId,
    value: &Value,
    restriction: &Value,
) -> Result<bool, KernelError> {
    if restriction == unrestricted_marker() {
        return Ok(true);
    }
    if let Some(required_delegate) = frob_only_restriction(restriction) {
        let Some(value_delegate) = value.frob_delegate() else {
            return Ok(false);
        };
        return identity_matches(
            reader,
            delegates_relation,
            value_delegate,
            &required_delegate,
        );
    }
    if value == restriction {
        return Ok(true);
    }

    if value.frob_delegate().is_none()
        && delegates_reaches(reader, delegates_relation, value, restriction)?
    {
        return Ok(true);
    }

    if let Some(identity) = value.as_identity() {
        if identity_matches(reader, delegates_relation, identity, restriction)? {
            return Ok(true);
        }
        return identity_matches(
            reader,
            delegates_relation,
            primitive_prototype_for_value(value),
            restriction,
        );
    }

    if let Some(delegate) = value.frob_delegate() {
        if identity_matches(reader, delegates_relation, delegate, restriction)? {
            return Ok(true);
        }
        return identity_matches(reader, delegates_relation, FROB_PROTOTYPE, restriction);
    }

    let prototype = primitive_prototype_for_value(value);
    identity_matches(reader, delegates_relation, prototype, restriction)
}

fn frob_only_restriction(restriction: &Value) -> Option<Value> {
    restriction.with_frob(|delegate, payload| {
        (payload == frob_only_marker()).then_some(Value::identity(delegate))
    })?
}

fn identity_matches(
    reader: &impl RelationRead,
    delegates_relation: RelationId,
    identity: Identity,
    restriction: &Value,
) -> Result<bool, KernelError> {
    let prototype = Value::identity(identity);
    if &prototype == restriction {
        return Ok(true);
    }
    delegates_reaches(reader, delegates_relation, &prototype, restriction)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Atom, RelationKernel, RelationMetadata, Rule, Term, Tuple};
    use mica_var::{Identity, STRING_PROTOTYPE, Symbol, Value};

    fn rel(id: u64) -> RelationId {
        Identity::new(id).unwrap()
    }

    fn int(value: i64) -> Value {
        Value::int(value).unwrap()
    }

    fn sym(name: &str) -> Value {
        Value::symbol(Symbol::intern(name))
    }

    fn kernel_with_dispatch_relations() -> RelationKernel {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(
                RelationMetadata::new(rel(40), Symbol::intern("MethodSelector"), 2)
                    .with_index([1, 0]),
            )
            .unwrap();
        kernel
            .create_relation(
                RelationMetadata::new(rel(41), Symbol::intern("Param"), 4).with_index([0, 1]),
            )
            .unwrap();
        kernel
            .create_relation(
                RelationMetadata::new(rel(42), Symbol::intern("Delegates"), 3)
                    .with_index([0, 2, 1]),
            )
            .unwrap();
        kernel
    }

    fn dispatch_relations() -> DispatchRelations {
        DispatchRelations {
            method_selector: rel(40),
            param: rel(41),
            delegates: rel(42),
        }
    }

    #[test]
    fn dispatch_matches_method_params_through_delegation() {
        let kernel = kernel_with_dispatch_relations();
        let mut tx = kernel.begin();
        tx.assert(rel(40), Tuple::from([int(100), sym("take")]))
            .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([int(100), sym("actor"), int(11), int(0)]),
        )
        .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([int(100), sym("item"), int(2), int(1)]),
        )
        .unwrap();
        tx.assert(rel(42), Tuple::from([int(10), int(11), int(0)]))
            .unwrap();
        tx.assert(rel(42), Tuple::from([int(1), int(2), int(0)]))
            .unwrap();

        assert_eq!(
            applicable_methods(
                &tx,
                dispatch_relations(),
                sym("take"),
                [(sym("actor"), int(10)), (sym("item"), int(1))]
            )
            .unwrap(),
            vec![int(100)]
        );
    }

    #[test]
    fn dispatch_rejects_missing_roles() {
        let kernel = kernel_with_dispatch_relations();
        let mut tx = kernel.begin();
        tx.assert(rel(40), Tuple::from([int(100), sym("take")]))
            .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([int(100), sym("actor"), int(11), int(0)]),
        )
        .unwrap();

        assert!(
            applicable_methods(
                &tx,
                dispatch_relations(),
                sym("take"),
                [(sym("item"), int(1))]
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn dispatch_requires_unrestricted_params_without_matching_them() {
        let kernel = kernel_with_dispatch_relations();
        let mut tx = kernel.begin();
        tx.assert(rel(40), Tuple::from([int(100), sym("say")]))
            .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([int(100), sym("actor"), int(11), int(0)]),
        )
        .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([
                int(100),
                sym("message"),
                unrestricted_dispatch_restriction(),
                int(1),
            ]),
        )
        .unwrap();
        tx.assert(rel(42), Tuple::from([int(10), int(11), int(0)]))
            .unwrap();

        assert_eq!(
            applicable_methods(
                &tx,
                dispatch_relations(),
                sym("say"),
                [
                    (sym("actor"), int(10)),
                    (sym("message"), Value::string("hi"))
                ]
            )
            .unwrap(),
            vec![int(100)]
        );
        assert!(
            applicable_methods(
                &tx,
                dispatch_relations(),
                sym("say"),
                [(sym("actor"), int(10))]
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn positional_dispatch_matches_primitive_restrictions() {
        let kernel = kernel_with_dispatch_relations();
        let mut tx = kernel.begin();
        tx.assert(rel(40), Tuple::from([int(100), sym("split")]))
            .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([
                int(100),
                sym("text"),
                Value::identity(STRING_PROTOTYPE),
                int(0),
            ]),
        )
        .unwrap();

        assert_eq!(
            applicable_positional_methods(
                &tx,
                dispatch_relations(),
                sym("split"),
                &[Value::string("a b")]
            )
            .unwrap(),
            vec![int(100)]
        );
        assert!(
            applicable_positional_methods(&tx, dispatch_relations(), sym("split"), &[int(1)])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn dispatch_matches_frob_delegate_through_delegation() {
        let kernel = kernel_with_dispatch_relations();
        let mut tx = kernel.begin();
        let event = Value::identity(Identity::new(200).unwrap());
        let take_event = Value::identity(Identity::new(201).unwrap());
        let event_value = Value::frob(take_event.as_identity().unwrap(), Value::string("payload"));

        tx.assert(rel(40), Tuple::from([int(100), sym("render")]))
            .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([int(100), sym("event"), event.clone(), int(0)]),
        )
        .unwrap();
        tx.assert(rel(42), Tuple::from([take_event, event, int(0)]))
            .unwrap();

        assert_eq!(
            applicable_methods(
                &tx,
                dispatch_relations(),
                sym("render"),
                [(sym("event"), event_value)]
            )
            .unwrap(),
            vec![int(100)]
        );
    }

    #[test]
    fn dispatch_frob_only_restriction_rejects_bare_identity() {
        let kernel = kernel_with_dispatch_relations();
        let mut tx = kernel.begin();
        let event_identity = Identity::new(200).unwrap();
        let event = Value::identity(event_identity);
        let take_event_identity = Identity::new(201).unwrap();
        let take_event = Value::identity(take_event_identity);
        let frob_only = frob_only_dispatch_restriction(event_identity);

        tx.assert(rel(40), Tuple::from([int(100), sym("render")]))
            .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([int(100), sym("event"), frob_only, int(0)]),
        )
        .unwrap();
        tx.assert(rel(42), Tuple::from([take_event.clone(), event, int(0)]))
            .unwrap();

        assert!(
            applicable_methods(
                &tx,
                dispatch_relations(),
                sym("render"),
                [(sym("event"), take_event)]
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            applicable_methods(
                &tx,
                dispatch_relations(),
                sym("render"),
                [(
                    sym("event"),
                    Value::frob(take_event_identity, Value::string("payload"))
                )]
            )
            .unwrap(),
            vec![int(100)]
        );
    }

    #[test]
    fn dispatch_selects_most_specific_named_method() {
        let kernel = kernel_with_dispatch_relations();
        let mut tx = kernel.begin();
        let event = Value::identity(Identity::new(200).unwrap());
        let movement_event = Value::identity(Identity::new(201).unwrap());

        tx.assert(rel(40), Tuple::from([int(100), sym("label")]))
            .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([
                int(100),
                sym("kind"),
                unrestricted_dispatch_restriction(),
                int(0),
            ]),
        )
        .unwrap();
        tx.assert(rel(40), Tuple::from([int(101), sym("label")]))
            .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([int(101), sym("kind"), event.clone(), int(0)]),
        )
        .unwrap();
        tx.assert(rel(40), Tuple::from([int(102), sym("label")]))
            .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([int(102), sym("kind"), movement_event.clone(), int(0)]),
        )
        .unwrap();
        tx.assert(
            rel(42),
            Tuple::from([movement_event.clone(), event, int(0)]),
        )
        .unwrap();

        assert_eq!(
            applicable_methods(
                &tx,
                dispatch_relations(),
                sym("label"),
                [(sym("kind"), movement_event)]
            )
            .unwrap(),
            vec![int(102)]
        );
    }

    #[test]
    fn dispatch_keeps_incomparable_named_methods() {
        let kernel = kernel_with_dispatch_relations();
        let mut tx = kernel.begin();
        let player = Value::identity(Identity::new(200).unwrap());
        let portable = Value::identity(Identity::new(201).unwrap());
        let alice = Value::identity(Identity::new(202).unwrap());
        let coin = Value::identity(Identity::new(203).unwrap());

        tx.assert(rel(40), Tuple::from([int(100), sym("act")]))
            .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([int(100), sym("actor"), player.clone(), int(0)]),
        )
        .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([
                int(100),
                sym("item"),
                unrestricted_dispatch_restriction(),
                int(1),
            ]),
        )
        .unwrap();
        tx.assert(rel(40), Tuple::from([int(101), sym("act")]))
            .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([
                int(101),
                sym("actor"),
                unrestricted_dispatch_restriction(),
                int(0),
            ]),
        )
        .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([int(101), sym("item"), portable.clone(), int(1)]),
        )
        .unwrap();
        tx.assert(rel(42), Tuple::from([alice.clone(), player, int(0)]))
            .unwrap();
        tx.assert(rel(42), Tuple::from([coin.clone(), portable, int(0)]))
            .unwrap();

        assert_eq!(
            applicable_methods(
                &tx,
                dispatch_relations(),
                sym("act"),
                [(sym("actor"), alice), (sym("item"), coin)]
            )
            .unwrap(),
            vec![int(100), int(101)]
        );
    }

    #[test]
    fn dispatch_selects_most_specific_frob_delegate_method() {
        let kernel = kernel_with_dispatch_relations();
        let mut tx = kernel.begin();
        let event = Value::identity(Identity::new(200).unwrap());
        let take_event_identity = Identity::new(201).unwrap();
        let take_event = Value::identity(take_event_identity);
        let event_value = Value::frob(take_event_identity, Value::string("payload"));

        tx.assert(rel(40), Tuple::from([int(100), sym("render")]))
            .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([int(100), sym("event"), event.clone(), int(0)]),
        )
        .unwrap();
        tx.assert(rel(40), Tuple::from([int(101), sym("render")]))
            .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([int(101), sym("event"), take_event.clone(), int(0)]),
        )
        .unwrap();
        tx.assert(rel(42), Tuple::from([take_event, event, int(0)]))
            .unwrap();

        assert_eq!(
            applicable_methods(
                &tx,
                dispatch_relations(),
                sym("render"),
                [(sym("event"), event_value)]
            )
            .unwrap(),
            vec![int(101)]
        );
    }

    #[test]
    fn positional_dispatch_selects_most_specific_method() {
        let kernel = kernel_with_dispatch_relations();
        let mut tx = kernel.begin();

        tx.assert(rel(40), Tuple::from([int(100), sym("describe")]))
            .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([
                int(100),
                sym("value"),
                unrestricted_dispatch_restriction(),
                int(0),
            ]),
        )
        .unwrap();
        tx.assert(rel(40), Tuple::from([int(101), sym("describe")]))
            .unwrap();
        tx.assert(
            rel(41),
            Tuple::from([
                int(101),
                sym("value"),
                Value::identity(STRING_PROTOTYPE),
                int(0),
            ]),
        )
        .unwrap();

        assert_eq!(
            applicable_positional_methods(
                &tx,
                dispatch_relations(),
                sym("describe"),
                &[Value::string("hello")],
            )
            .unwrap(),
            vec![int(101)]
        );
    }

    #[test]
    fn snapshot_dispatch_cache_is_scoped_to_snapshot_version() {
        let kernel = kernel_with_dispatch_relations();
        let snapshot = kernel.snapshot();
        assert!(
            applicable_method_calls(&*snapshot, dispatch_relations(), sym("look"), &[])
                .unwrap()
                .is_empty()
        );

        let mut tx = kernel.begin();
        tx.assert(rel(40), Tuple::from([int(100), sym("look")]))
            .unwrap();
        let next = tx.commit().unwrap().into_snapshot();

        assert_eq!(
            applicable_method_calls(&*next, dispatch_relations(), sym("look"), &[]).unwrap(),
            vec![ApplicableMethodCall {
                method: int(100),
                args: Some(Vec::new())
            }]
        );
    }

    #[test]
    fn transaction_dispatch_bypasses_snapshot_cache_after_local_writes() {
        let kernel = kernel_with_dispatch_relations();
        let snapshot = kernel.snapshot();
        assert!(
            applicable_method_calls(&*snapshot, dispatch_relations(), sym("look"), &[])
                .unwrap()
                .is_empty()
        );

        let mut tx = kernel.begin();
        tx.assert(rel(40), Tuple::from([int(100), sym("look")]))
            .unwrap();

        assert_eq!(
            applicable_method_calls(&tx, dispatch_relations(), sym("look"), &[]).unwrap(),
            vec![ApplicableMethodCall {
                method: int(100),
                args: Some(Vec::new())
            }]
        );
    }

    #[test]
    fn transaction_positional_inline_cache_invalidates_after_dispatch_writes() {
        let kernel = kernel_with_dispatch_relations();
        let mut tx = kernel.begin();
        let binding = Tuple::from([int(100), sym("look")]);

        for _ in 0..3 {
            assert!(
                applicable_positional_methods_cached(&tx, dispatch_relations(), sym("look"), &[],)
                    .unwrap()
                    .is_empty()
            );
        }

        tx.assert(rel(40), binding.clone()).unwrap();

        let first =
            applicable_positional_methods_cached(&tx, dispatch_relations(), sym("look"), &[])
                .unwrap();
        assert_eq!(first.as_ref(), &[int(100)]);
        let second =
            applicable_positional_methods_cached(&tx, dispatch_relations(), sym("look"), &[])
                .unwrap();
        let third =
            applicable_positional_methods_cached(&tx, dispatch_relations(), sym("look"), &[])
                .unwrap();
        assert!(Arc::ptr_eq(&second, &third));

        tx.retract(rel(40), binding).unwrap();
        assert!(
            applicable_positional_methods_cached(&tx, dispatch_relations(), sym("look"), &[],)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn transaction_positional_inline_cache_invalidates_for_derived_dispatch_relation() {
        let kernel = kernel_with_dispatch_relations();
        kernel
            .create_relation(RelationMetadata::new(
                rel(43),
                Symbol::intern("PendingMethodSelector"),
                2,
            ))
            .unwrap();

        let method = Term::Var(Symbol::intern("method"));
        let selector = Term::Var(Symbol::intern("selector"));
        kernel
            .install_rule(
                Rule::new(
                    rel(40),
                    [method.clone(), selector.clone()],
                    [Atom::positive(rel(43), [method, selector])],
                ),
                "MethodSelector(method, selector) :- PendingMethodSelector(method, selector)",
            )
            .unwrap();

        let mut tx = kernel.begin();
        for _ in 0..3 {
            assert!(
                applicable_positional_methods_cached(&tx, dispatch_relations(), sym("look"), &[],)
                    .unwrap()
                    .is_empty()
            );
        }

        tx.assert(rel(43), Tuple::from([int(100), sym("look")]))
            .unwrap();

        assert_eq!(
            applicable_positional_methods_cached(&tx, dispatch_relations(), sym("look"), &[],)
                .unwrap()
                .as_ref(),
            &[int(100)]
        );
    }

    #[test]
    fn transaction_method_program_inline_cache_invalidates_after_relation_writes() {
        let kernel = kernel_with_dispatch_relations();
        kernel
            .create_relation(
                RelationMetadata::new(rel(43), Symbol::intern("MethodProgram"), 2).with_index([0]),
            )
            .unwrap();
        let method = int(100);
        let program = int(101);
        let selector_binding = Tuple::from([method.clone(), sym("look")]);
        let program_binding = Tuple::from([method.clone(), program.clone()]);
        let mut seed = kernel.begin();
        seed.assert(rel(40), selector_binding).unwrap();
        seed.assert(rel(43), program_binding.clone()).unwrap();
        seed.commit().unwrap();

        let mut tx = kernel.begin();
        for _ in 0..3 {
            assert_eq!(
                applicable_positional_methods_cached(&tx, dispatch_relations(), sym("look"), &[],)
                    .unwrap()
                    .as_ref(),
                std::slice::from_ref(&method)
            );
        }
        for _ in 0..2 {
            assert_eq!(
                method_program_id(&tx, rel(43), &method).unwrap(),
                Some(program.clone())
            );
        }

        tx.retract(rel(43), program_binding).unwrap();

        assert_eq!(method_program_id(&tx, rel(43), &method).unwrap(), None);
    }

    #[test]
    fn transaction_dispatch_uses_snapshot_cache_after_unrelated_local_writes() {
        let kernel = kernel_with_dispatch_relations();
        kernel
            .create_relation(RelationMetadata::new(rel(43), Symbol::intern("Event"), 2))
            .unwrap();
        let mut seed = kernel.begin();
        seed.assert(rel(40), Tuple::from([int(100), sym("look")]))
            .unwrap();
        seed.commit().unwrap();

        let mut tx = kernel.begin();
        tx.assert(rel(43), Tuple::from([int(1), int(2)])).unwrap();

        assert_eq!(
            applicable_method_calls(&tx, dispatch_relations(), sym("look"), &[]).unwrap(),
            vec![ApplicableMethodCall {
                method: int(100),
                args: Some(Vec::new())
            }]
        );
    }

    #[test]
    fn transaction_positional_dispatch_uses_snapshot_cache_after_unrelated_local_writes() {
        let kernel = kernel_with_dispatch_relations();
        kernel
            .create_relation(RelationMetadata::new(rel(43), Symbol::intern("Event"), 2))
            .unwrap();
        let mut seed = kernel.begin();
        seed.assert(rel(40), Tuple::from([int(100), sym("look")]))
            .unwrap();
        seed.commit().unwrap();

        let mut tx = kernel.begin();
        tx.assert(rel(43), Tuple::from([int(1), int(2)])).unwrap();

        assert_eq!(
            applicable_positional_methods(&tx, dispatch_relations(), sym("look"), &[]).unwrap(),
            vec![int(100)]
        );
        let methods =
            applicable_positional_methods_cached(&tx, dispatch_relations(), sym("look"), &[])
                .unwrap();
        assert_eq!(methods.as_ref(), &[int(100)]);
    }

    #[test]
    fn positional_dispatch_shares_snapshot_cache_results() {
        let kernel = kernel_with_dispatch_relations();
        let mut seed = kernel.begin();
        seed.assert(rel(40), Tuple::from([int(100), sym("look")]))
            .unwrap();
        seed.commit().unwrap();

        let tx = kernel.begin();
        let first = tx
            .cached_applicable_positional_methods(dispatch_relations(), &sym("look"), &[])
            .unwrap();
        let second = tx
            .cached_applicable_positional_methods(dispatch_relations(), &sym("look"), &[])
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn named_method_args_follow_param_positions() {
        let params = vec![
            Tuple::from([
                int(100),
                sym("item"),
                unrestricted_dispatch_restriction(),
                int(1),
            ]),
            Tuple::from([
                int(100),
                sym("actor"),
                unrestricted_dispatch_restriction(),
                int(0),
            ]),
        ];

        assert_eq!(
            named_method_args(&params, &[(sym("actor"), int(10)), (sym("item"), int(1))]).unwrap(),
            vec![int(10), int(1)]
        );
    }
}
