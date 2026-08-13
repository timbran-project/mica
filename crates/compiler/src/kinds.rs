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

use crate::{
    BinaryOp, Binding, BindingId, HirCollectionItem, HirExpr, HirFunctionBody, HirItem, HirPlace,
    Literal, LocalKind, UnaryOp,
};
use mica_var::ValueKind;

const KIND_COUNT: u32 = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct KindSet(u32);

impl KindSet {
    pub(crate) const EMPTY: Self = Self(0);
    pub(crate) const ALL: Self = Self((1 << KIND_COUNT) - 1);

    pub(crate) const fn exact(kind: ValueKind) -> Self {
        Self(1 << kind_bit(kind))
    }

    pub(crate) const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub(crate) const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub(crate) const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub(crate) const fn is_subset(self, other: Self) -> bool {
        self.0 & !other.0 == 0
    }

    pub(crate) const fn is_disjoint(self, other: Self) -> bool {
        self.0 & other.0 == 0
    }

    pub(crate) fn singleton(self) -> Option<ValueKind> {
        if self.0.count_ones() != 1 {
            return None;
        }
        kind_for_bit(self.0.trailing_zeros())
    }

    pub(crate) fn iter(self) -> impl Iterator<Item = ValueKind> {
        (0..KIND_COUNT)
            .filter(move |bit| self.0 & (1 << bit) != 0)
            .filter_map(kind_for_bit)
    }

    pub(crate) fn names(self) -> String {
        if self == Self::ALL {
            return "any value kind".to_owned();
        }
        if self.is_empty() {
            return "no normally completing value".to_owned();
        }
        let names = (0..KIND_COUNT)
            .filter(|bit| self.0 & (1 << bit) != 0)
            .filter_map(kind_for_bit)
            .map(ValueKind::name)
            .collect::<Vec<_>>();
        names.join(" or ")
    }
}

pub(crate) fn iteration_binding_kinds(
    collection: KindSet,
    two_bindings: bool,
) -> (KindSet, Option<KindSet>) {
    let may_be = |kind| !collection.is_disjoint(KindSet::exact(kind));
    let int = KindSet::exact(ValueKind::Int);
    let map = KindSet::exact(ValueKind::Map);

    if !two_bindings {
        if may_be(ValueKind::List) || may_be(ValueKind::Map) {
            return (KindSet::ALL, None);
        }
        let mut item = KindSet::EMPTY;
        if may_be(ValueKind::Range) {
            item = item.union(int);
        }
        if may_be(ValueKind::Relation) {
            item = item.union(map);
        }
        return (item, None);
    }

    let key = if may_be(ValueKind::Map) {
        KindSet::ALL
    } else if may_be(ValueKind::Range) || may_be(ValueKind::List) || may_be(ValueKind::Relation) {
        int
    } else {
        KindSet::EMPTY
    };
    let value = if may_be(ValueKind::List) || may_be(ValueKind::Map) {
        KindSet::ALL
    } else {
        let mut value = KindSet::EMPTY;
        if may_be(ValueKind::Range) {
            value = value.union(int);
        }
        if may_be(ValueKind::Relation) {
            value = value.union(map);
        }
        value
    };
    (key, Some(value))
}

pub(crate) struct KindInference<'a> {
    bindings: &'a [Binding],
    direct_result: &'a dyn Fn(BindingId) -> Option<KindSet>,
    runtime_result: &'a dyn Fn(&str) -> Option<KindSet>,
}

impl<'a> KindInference<'a> {
    pub(crate) const fn new(
        bindings: &'a [Binding],
        direct_result: &'a dyn Fn(BindingId) -> Option<KindSet>,
        runtime_result: &'a dyn Fn(&str) -> Option<KindSet>,
    ) -> Self {
        Self {
            bindings,
            direct_result,
            runtime_result,
        }
    }

    pub(crate) fn expr(&self, expr: &HirExpr) -> KindSet {
        self.flow(expr).normal
    }

    pub(crate) fn function_result(&self, body: &HirFunctionBody) -> KindSet {
        let flow = match body {
            HirFunctionBody::Expr(expr) => self.flow(expr),
            HirFunctionBody::Block(items) => self.items(items),
        };
        flow.normal.union(flow.returns)
    }

    pub(crate) fn block_result(&self, items: &[HirItem]) -> KindSet {
        let flow = self.items(items);
        flow.normal.union(flow.returns)
    }

    fn flow(&self, expr: &HirExpr) -> KindFlow {
        match expr {
            HirExpr::Literal { value, .. } => KindFlow::value(KindSet::exact(literal_kind(value))),
            HirExpr::LocalRef { binding, .. } => KindFlow::value(self.binding_kind(*binding)),
            HirExpr::Identity { .. } => KindFlow::value(KindSet::exact(ValueKind::Identity)),
            HirExpr::Frob { value, .. } => self
                .flow(value)
                .with_normal(KindSet::exact(ValueKind::Frob)),
            HirExpr::Symbol { .. } => KindFlow::value(KindSet::exact(ValueKind::Symbol)),
            HirExpr::List { items, .. } => {
                let mut flow = KindFlow::reachable();
                for item in items {
                    let expr = match item {
                        HirCollectionItem::Expr(expr) | HirCollectionItem::Splice(expr) => expr,
                    };
                    flow = flow.then(self.flow(expr));
                }
                flow.with_normal(KindSet::exact(ValueKind::List))
            }
            HirExpr::Relation { rows, .. } => {
                let mut flow = KindFlow::reachable();
                for expr in rows.iter().flatten() {
                    flow = flow.then(self.flow(expr));
                }
                flow.with_normal(KindSet::exact(ValueKind::Relation))
            }
            HirExpr::Map { entries, .. } => {
                let mut flow = KindFlow::reachable();
                for (key, value) in entries {
                    flow = flow.then(self.flow(key)).then(self.flow(value));
                }
                flow.with_normal(KindSet::exact(ValueKind::Map))
            }
            HirExpr::Unary { op, expr, .. } => {
                let operand = self.flow(expr);
                operand.with_normal(self.unary(*op, operand.normal))
            }
            HirExpr::Binary {
                op, left, right, ..
            } => {
                let left = self.flow(left);
                let right = self.flow(right);
                let returns = if left.normal.is_empty() {
                    left.returns
                } else {
                    left.returns.union(right.returns)
                };
                KindFlow {
                    normal: self.binary(*op, left.normal, right.normal),
                    returns,
                }
            }
            HirExpr::Assign { target, value, .. } => match target {
                HirPlace::Local { .. } => self.flow(value),
                HirPlace::Dot { base, .. } => {
                    let value = self.flow(value);
                    let normal = value.normal;
                    value.then(self.flow(base)).with_normal(normal)
                }
                HirPlace::Index {
                    collection, index, ..
                } => {
                    let mut flow = self.flow(value);
                    if let Some(index) = index {
                        flow = flow.then(self.flow(index));
                    }
                    flow.with_normal(self.expr(collection))
                }
                HirPlace::Invalid { .. } => self.flow(value).with_normal(KindSet::ALL),
            },
            HirExpr::RelationAtom(atom) => {
                let normal =
                    if atom.args.iter().any(|arg| {
                        matches!(arg.value, HirExpr::QueryVar { .. } | HirExpr::Hole { .. })
                    }) {
                        KindSet::exact(ValueKind::Relation)
                    } else {
                        KindSet::exact(ValueKind::Bool)
                    };
                self.args(&atom.args).with_normal(normal)
            }
            HirExpr::FactChange { atom, .. } => self
                .args(&atom.args)
                .with_normal(KindSet::exact(ValueKind::Relation)),
            HirExpr::Require { condition, .. } => self
                .flow(condition)
                .with_normal(KindSet::exact(ValueKind::Bool)),
            HirExpr::Binding { value, .. } => match value {
                Some(value) => self.flow(value),
                None => KindFlow::value(KindSet::exact(ValueKind::Relation)),
            },
            HirExpr::If {
                condition,
                then_items,
                elseif,
                else_items,
                ..
            } => {
                let mut fallback = self.items(else_items);
                for (condition, items) in elseif.iter().rev() {
                    fallback = self.conditional(condition, items, fallback);
                }
                self.conditional(condition, then_items, fallback)
            }
            HirExpr::Match { value, cases, .. } => {
                let value = self.flow(value);
                if value.normal.is_empty() {
                    return value;
                }
                let branches = cases.iter().fold(KindFlow::unreachable(), |result, case| {
                    let mut branch = KindFlow::reachable();
                    if let Some(guard) = &case.guard {
                        branch = branch.then(self.flow(guard));
                    }
                    result.union(branch.then(self.items(&case.body)))
                });
                KindFlow {
                    normal: branches.normal,
                    returns: value.returns.union(branches.returns),
                }
            }
            HirExpr::Block { items, .. } => self.items(items),
            HirExpr::For { iter, body, .. } => {
                let iter = self.flow(iter);
                if iter.normal.is_empty() {
                    return iter;
                }
                let body = self.items(body);
                KindFlow {
                    normal: KindSet::exact(ValueKind::Relation),
                    returns: iter.returns.union(body.returns),
                }
            }
            HirExpr::While {
                condition, body, ..
            } => {
                let condition = self.flow(condition);
                if condition.normal.is_empty() {
                    return condition;
                }
                let body = self.items(body);
                KindFlow {
                    normal: KindSet::exact(ValueKind::Relation),
                    returns: condition.returns.union(body.returns),
                }
            }
            HirExpr::Return { value, .. } => {
                let value = value.as_deref().map_or_else(
                    || KindFlow::value(KindSet::exact(ValueKind::Relation)),
                    |value| self.flow(value),
                );
                KindFlow {
                    normal: KindSet::EMPTY,
                    returns: value.returns.union(value.normal),
                }
            }
            HirExpr::Raise {
                error,
                message,
                value,
                ..
            } => {
                let mut flow = self.flow(error);
                if let Some(message) = message {
                    flow = flow.then(self.flow(message));
                }
                if let Some(value) = value {
                    flow = flow.then(self.flow(value));
                }
                KindFlow {
                    normal: KindSet::EMPTY,
                    returns: flow.returns,
                }
            }
            HirExpr::Recover { expr, catches, .. } => {
                let mut result = self.flow(expr);
                for catch in catches {
                    let mut flow = KindFlow::reachable();
                    if let Some(condition) = &catch.condition {
                        flow = flow.then(self.flow(condition));
                    }
                    flow = flow.then(self.flow(&catch.value));
                    result = result.union(flow);
                }
                result
            }
            HirExpr::Break { .. } | HirExpr::Continue { .. } | HirExpr::Error { .. } => {
                KindFlow::unreachable()
            }
            HirExpr::Try {
                body,
                catches,
                finally,
                ..
            } => {
                let mut result = self.items(body);
                for catch in catches {
                    let mut flow = KindFlow::reachable();
                    if let Some(condition) = &catch.condition {
                        flow = flow.then(self.flow(condition));
                    }
                    flow = flow.then(self.items(&catch.body));
                    result = result.union(flow);
                }
                if finally.is_empty() {
                    return result;
                }
                let finally = self.items(finally);
                let preceding = if finally.normal.is_empty() {
                    KindFlow::unreachable()
                } else {
                    result
                };
                KindFlow {
                    normal: preceding.normal,
                    returns: preceding.returns.union(finally.returns),
                }
            }
            HirExpr::Function { name: None, .. } => {
                KindFlow::value(KindSet::exact(ValueKind::Function))
            }
            HirExpr::Call { callee, args, .. } => {
                let mut flow = self.flow(callee);
                for arg in args {
                    flow = flow.then(self.flow(&arg.value));
                }
                flow.with_normal(self.direct_call_result(callee))
            }
            HirExpr::RoleDispatch { selector, args, .. } => self
                .flow(selector)
                .then(self.args(args))
                .with_normal(KindSet::ALL),
            HirExpr::ReceiverDispatch {
                receiver,
                selector,
                args,
                ..
            } => self
                .flow(receiver)
                .then(self.flow(selector))
                .then(self.args(args))
                .with_normal(KindSet::ALL),
            HirExpr::Spawn { target, delay, .. } => {
                let mut flow = self.flow(target);
                if let Some(delay) = delay {
                    flow = flow.then(self.flow(delay));
                }
                flow.with_normal(KindSet::ALL)
            }
            HirExpr::Index {
                collection, index, ..
            } => {
                let mut flow = self.flow(collection);
                if let Some(index) = index {
                    flow = flow.then(self.flow(index));
                }
                flow.with_normal(KindSet::ALL)
            }
            HirExpr::Field { base, .. } => self.flow(base).with_normal(KindSet::ALL),
            HirExpr::ExternalRef { name, .. } if name == "none" => {
                KindFlow::value(KindSet::exact(ValueKind::Relation))
            }
            HirExpr::ExternalRef { .. }
            | HirExpr::QueryVar { .. }
            | HirExpr::Hole { .. }
            | HirExpr::Function { name: Some(_), .. } => KindFlow::value(KindSet::ALL),
        }
    }

    fn items(&self, items: &[HirItem]) -> KindFlow {
        let mut result = KindFlow::reachable();
        for item in items {
            let HirItem::Expr { expr, .. } = item else {
                return result.then(KindFlow::value(KindSet::ALL));
            };
            result = result.then(self.flow(expr));
            if result.normal.is_empty() {
                return result;
            }
        }
        result
    }

    fn args(&self, args: &[crate::HirArg]) -> KindFlow {
        args.iter().fold(KindFlow::reachable(), |flow, arg| {
            flow.then(self.flow(&arg.value))
        })
    }

    fn conditional(
        &self,
        condition: &HirExpr,
        then_items: &[HirItem],
        fallback: KindFlow,
    ) -> KindFlow {
        let condition = self.flow(condition);
        if condition.normal.is_empty() {
            return condition;
        }
        let branches = self.items(then_items).union(fallback);
        KindFlow {
            normal: branches.normal,
            returns: condition.returns.union(branches.returns),
        }
    }

    fn binding_kind(&self, id: BindingId) -> KindSet {
        self.binding(id)
            .map(|binding| {
                binding.declared_kind.map_or_else(
                    || match binding.kind {
                        LocalKind::Catch => KindSet::exact(ValueKind::Error),
                        _ => KindSet::ALL,
                    },
                    KindSet::exact,
                )
            })
            .unwrap_or(KindSet::ALL)
    }

    fn direct_call_result(&self, callee: &HirExpr) -> KindSet {
        match callee {
            HirExpr::LocalRef { binding, .. } => (self.direct_result)(*binding)
                .or_else(|| {
                    let binding = self.binding(*binding)?;
                    (binding.kind == LocalKind::InstalledParam)
                        .then(|| (self.runtime_result)(&binding.name))
                        .flatten()
                })
                .unwrap_or(KindSet::ALL),
            HirExpr::ExternalRef { name, .. } => {
                if matches!(name.as_str(), "some" | "ok" | "err") {
                    KindSet::exact(ValueKind::Relation)
                } else {
                    (self.runtime_result)(name).unwrap_or(KindSet::ALL)
                }
            }
            _ => KindSet::ALL,
        }
    }

    fn unary(&self, op: UnaryOp, operand: KindSet) -> KindSet {
        match op {
            UnaryOp::Not => KindSet::exact(ValueKind::Bool),
            UnaryOp::Neg => operand.intersection(numeric_kinds()),
        }
    }

    fn binary(&self, op: BinaryOp, left: KindSet, right: KindSet) -> KindSet {
        if left.is_empty() {
            return KindSet::EMPTY;
        }
        if right.is_empty() && !matches!(op, BinaryOp::And | BinaryOp::Or) {
            return KindSet::EMPTY;
        }
        match op {
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge => KindSet::exact(ValueKind::Bool),
            BinaryOp::Range => KindSet::exact(ValueKind::Range),
            BinaryOp::And | BinaryOp::Or if left.is_empty() => KindSet::EMPTY,
            BinaryOp::And | BinaryOp::Or => KindSet::exact(ValueKind::Bool).union(right),
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => {
                arithmetic_kinds(op, left, right)
            }
        }
    }

    fn binding(&self, id: crate::BindingId) -> Option<&Binding> {
        self.bindings.get(id.as_u32() as usize)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KindFlow {
    normal: KindSet,
    returns: KindSet,
}

impl KindFlow {
    const fn value(normal: KindSet) -> Self {
        Self {
            normal,
            returns: KindSet::EMPTY,
        }
    }

    const fn reachable() -> Self {
        Self::value(KindSet::exact(ValueKind::Relation))
    }

    const fn unreachable() -> Self {
        Self::value(KindSet::EMPTY)
    }

    fn then(self, next: Self) -> Self {
        if self.normal.is_empty() {
            return self;
        }
        Self {
            normal: next.normal,
            returns: self.returns.union(next.returns),
        }
    }

    const fn union(self, other: Self) -> Self {
        Self {
            normal: self.normal.union(other.normal),
            returns: self.returns.union(other.returns),
        }
    }

    const fn with_normal(self, normal: KindSet) -> Self {
        if self.normal.is_empty() {
            return self;
        }
        Self {
            normal,
            returns: self.returns,
        }
    }
}

const fn literal_kind(literal: &Literal) -> ValueKind {
    match literal {
        Literal::Int(_) => ValueKind::Int,
        Literal::Float(_) => ValueKind::Float,
        Literal::String(_) => ValueKind::String,
        Literal::Bytes(_) => ValueKind::Bytes,
        Literal::Bool(_) => ValueKind::Bool,
        Literal::ErrorCode(_) => ValueKind::ErrorCode,
        Literal::Unit => ValueKind::Relation,
    }
}

const fn numeric_kinds() -> KindSet {
    KindSet::exact(ValueKind::Int).union(KindSet::exact(ValueKind::Float))
}

fn arithmetic_kinds(op: BinaryOp, left: KindSet, right: KindSet) -> KindSet {
    let left = left.intersection(numeric_kinds());
    let right = right.intersection(numeric_kinds());
    if left.is_empty() || right.is_empty() {
        return KindSet::EMPTY;
    }

    let integers = KindSet::exact(ValueKind::Int);
    let floats = KindSet::exact(ValueKind::Float);
    let mut result = KindSet::EMPTY;
    if !left.is_disjoint(integers) && !right.is_disjoint(integers) {
        result = result.union(integers);
        if op == BinaryOp::Div {
            result = result.union(floats);
        }
    }
    if (!left.is_disjoint(floats) && !right.is_disjoint(numeric_kinds()))
        || (!right.is_disjoint(floats) && !left.is_disjoint(numeric_kinds()))
    {
        result = result.union(floats);
    }
    result
}

const fn kind_bit(kind: ValueKind) -> u32 {
    match kind {
        ValueKind::Bool => 0,
        ValueKind::Int => 1,
        ValueKind::Float => 2,
        ValueKind::Identity => 3,
        ValueKind::String => 4,
        ValueKind::Bytes => 5,
        ValueKind::Symbol => 6,
        ValueKind::ErrorCode => 7,
        ValueKind::Error => 8,
        ValueKind::Capability => 9,
        ValueKind::Frob => 10,
        ValueKind::Function => 11,
        ValueKind::List => 12,
        ValueKind::Map => 13,
        ValueKind::Range => 14,
        ValueKind::Relation => 15,
    }
}

const fn kind_for_bit(bit: u32) -> Option<ValueKind> {
    match bit {
        0 => Some(ValueKind::Bool),
        1 => Some(ValueKind::Int),
        2 => Some(ValueKind::Float),
        3 => Some(ValueKind::Identity),
        4 => Some(ValueKind::String),
        5 => Some(ValueKind::Bytes),
        6 => Some(ValueKind::Symbol),
        7 => Some(ValueKind::ErrorCode),
        8 => Some(ValueKind::Error),
        9 => Some(ValueKind::Capability),
        10 => Some(ValueKind::Frob),
        11 => Some(ValueKind::Function),
        12 => Some(ValueKind::List),
        13 => Some(ValueKind::Map),
        14 => Some(ValueKind::Range),
        15 => Some(ValueKind::Relation),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_sets_keep_compiler_bits_independent_and_composable() {
        let int = KindSet::exact(ValueKind::Int);
        let float = KindSet::exact(ValueKind::Float);
        let numeric = int.union(float);

        assert!(int.is_subset(numeric));
        assert!(int.is_disjoint(float));
        assert_eq!(int.singleton(), Some(ValueKind::Int));
        assert_eq!(numeric.singleton(), None);
        assert_eq!(numeric.names(), "int or float");
        assert_eq!(KindSet::EMPTY.intersection(KindSet::ALL), KindSet::EMPTY);
    }

    #[test]
    fn arithmetic_result_sets_follow_runtime_numeric_rules() {
        let int = KindSet::exact(ValueKind::Int);
        let float = KindSet::exact(ValueKind::Float);

        assert_eq!(arithmetic_kinds(BinaryOp::Add, int, int), int);
        assert_eq!(arithmetic_kinds(BinaryOp::Add, int, float), float);
        assert_eq!(arithmetic_kinds(BinaryOp::Div, int, int), int.union(float));
        assert_eq!(
            arithmetic_kinds(BinaryOp::Add, KindSet::ALL, int),
            int.union(float)
        );
    }

    #[test]
    fn iteration_binding_kinds_follow_collection_shapes() {
        let int = KindSet::exact(ValueKind::Int);
        let map = KindSet::exact(ValueKind::Map);

        assert_eq!(
            iteration_binding_kinds(KindSet::exact(ValueKind::Range), false),
            (int, None)
        );
        assert_eq!(
            iteration_binding_kinds(KindSet::exact(ValueKind::Range), true),
            (int, Some(int))
        );
        assert_eq!(
            iteration_binding_kinds(KindSet::exact(ValueKind::List), true),
            (int, Some(KindSet::ALL))
        );
        assert_eq!(
            iteration_binding_kinds(KindSet::exact(ValueKind::Map), true),
            (KindSet::ALL, Some(KindSet::ALL))
        );
        assert_eq!(
            iteration_binding_kinds(KindSet::exact(ValueKind::Relation), false),
            (map, None)
        );
        assert_eq!(
            iteration_binding_kinds(KindSet::exact(ValueKind::Relation), true),
            (int, Some(map))
        );
        assert_eq!(
            iteration_binding_kinds(
                KindSet::exact(ValueKind::Range).union(KindSet::exact(ValueKind::Relation)),
                true,
            ),
            (int, Some(int.union(map)))
        );
    }
}
