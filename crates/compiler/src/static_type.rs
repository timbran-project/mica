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

use crate::kinds::{KindInference, KindSet};
use crate::{Binding, BindingId, HirArg, HirCollectionItem, HirExpr, Literal};
use mica_var::ValueKind;
use std::cmp::{max, min};
use std::collections::{BTreeMap, HashMap};
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TypeParameterId(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TypeAliasId(pub u32);

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum StaticLiteral {
    Bool(bool),
    Symbol(String),
    ErrorCode(String),
    Unit,
    EmptyRelation,
}

impl StaticLiteral {
    pub const fn outer_kind(&self) -> ValueKind {
        match self {
            Self::Bool(_) => ValueKind::Bool,
            Self::Symbol(_) => ValueKind::Symbol,
            Self::ErrorCode(_) => ValueKind::ErrorCode,
            Self::Unit | Self::EmptyRelation => ValueKind::Relation,
        }
    }

    fn relation_type(&self) -> Option<RelationType> {
        match self {
            Self::Unit => Some(RelationType {
                alternatives: vec![RowShape::empty()],
                cardinality: Cardinality::EXACTLY_ONE,
            }),
            Self::EmptyRelation => Some(RelationType {
                alternatives: vec![RowShape::empty()],
                cardinality: Cardinality::EMPTY,
            }),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Cardinality {
    pub min: usize,
    pub max: Option<usize>,
}

impl Cardinality {
    pub const EMPTY: Self = Self {
        min: 0,
        max: Some(0),
    };
    pub const EXACTLY_ONE: Self = Self {
        min: 1,
        max: Some(1),
    };
    pub const OPTIONAL: Self = Self {
        min: 0,
        max: Some(1),
    };
    pub const UNRESTRICTED: Self = Self { min: 0, max: None };
    pub const NON_EMPTY: Self = Self { min: 1, max: None };

    pub fn new(minimum: usize, maximum: Option<usize>) -> Result<Self, StaticTypeError> {
        if maximum.is_some_and(|maximum| minimum > maximum) {
            return Err(StaticTypeError::InvalidCardinality { minimum, maximum });
        }
        Ok(Self {
            min: minimum,
            max: maximum,
        })
    }

    pub const fn exact(rows: usize) -> Self {
        Self {
            min: rows,
            max: Some(rows),
        }
    }

    pub const fn is_subtype_of(self, other: Self) -> bool {
        if self.min < other.min {
            return false;
        }
        match (self.max, other.max) {
            (_, None) => true,
            (Some(actual), Some(expected)) => actual <= expected,
            (None, Some(_)) => false,
        }
    }

    pub const fn is_disjoint(self, other: Self) -> bool {
        if let Some(maximum) = self.max
            && maximum < other.min
        {
            return true;
        }
        if let Some(maximum) = other.max
            && maximum < self.min
        {
            return true;
        }
        false
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        let minimum = max(self.min, other.min);
        let maximum = match (self.max, other.max) {
            (Some(left), Some(right)) => Some(min(left, right)),
            (Some(maximum), None) | (None, Some(maximum)) => Some(maximum),
            (None, None) => None,
        };
        (!maximum.is_some_and(|maximum| minimum > maximum)).then_some(Self {
            min: minimum,
            max: maximum,
        })
    }

    const fn includes_zero(self) -> bool {
        self.min == 0
    }
}

impl fmt::Display for Cardinality {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.max {
            Some(maximum) if self.min == maximum => write!(formatter, "{}", self.min),
            Some(maximum) => write!(formatter, "{}..{maximum}", self.min),
            None => write!(formatter, "{}..*", self.min),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RowShape {
    columns: Vec<(String, StaticType)>,
}

impl RowShape {
    pub fn new(
        columns: impl IntoIterator<Item = (String, StaticType)>,
    ) -> Result<Self, StaticTypeError> {
        let mut columns = columns
            .into_iter()
            .map(|(name, ty)| (name, ty.canonical()))
            .collect::<Vec<_>>();
        columns.sort_by(|(left, _), (right, _)| left.cmp(right));
        for adjacent in columns.windows(2) {
            if adjacent[0].0 == adjacent[1].0 {
                return Err(StaticTypeError::DuplicateColumn(adjacent[0].0.clone()));
            }
        }
        Ok(Self { columns })
    }

    pub const fn empty() -> Self {
        Self {
            columns: Vec::new(),
        }
    }

    pub fn columns(&self) -> &[(String, StaticType)] {
        &self.columns
    }

    pub fn heading(&self) -> impl Iterator<Item = &str> {
        self.columns.iter().map(|(name, _)| name.as_str())
    }

    fn is_subtype_of(&self, other: &Self) -> bool {
        self.columns.len() == other.columns.len()
            && self.columns.iter().zip(&other.columns).all(
                |((left_name, left), (right_name, right))| {
                    left_name == right_name && left.is_subtype_of(right)
                },
            )
    }

    fn is_disjoint(&self, other: &Self) -> bool {
        self.columns.len() != other.columns.len()
            || self.columns.iter().zip(&other.columns).any(
                |((left_name, left), (right_name, right))| {
                    left_name != right_name || left.is_disjoint(right)
                },
            )
    }

    fn intersection(&self, other: &Self) -> Option<Self> {
        if self.columns.len() != other.columns.len() {
            return None;
        }
        let mut columns = Vec::with_capacity(self.columns.len());
        for ((left_name, left), (right_name, right)) in self.columns.iter().zip(&other.columns) {
            if left_name != right_name {
                return None;
            }
            let ty = left.intersection(right);
            if ty == StaticType::Never {
                return None;
            }
            columns.push((left_name.clone(), ty));
        }
        Some(Self { columns })
    }

    fn intersection_allowing_never(&self, other: &Self) -> Option<Self> {
        if self.columns.len() != other.columns.len() {
            return None;
        }
        let mut columns = Vec::with_capacity(self.columns.len());
        for ((left_name, left), (right_name, right)) in self.columns.iter().zip(&other.columns) {
            if left_name != right_name {
                return None;
            }
            columns.push((left_name.clone(), left.intersection(right)));
        }
        Some(Self { columns })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RelationType {
    alternatives: Vec<RowShape>,
    cardinality: Cardinality,
}

impl RelationType {
    pub fn new(
        alternatives: impl IntoIterator<Item = RowShape>,
        cardinality: Cardinality,
    ) -> Result<Self, StaticTypeError> {
        let mut alternatives = alternatives.into_iter().collect::<Vec<_>>();
        if alternatives.is_empty() {
            return Err(StaticTypeError::MissingRowAlternative);
        }
        alternatives.sort();
        alternatives.dedup();
        let heading = alternatives[0].heading().collect::<Vec<_>>();
        for alternative in alternatives.iter().skip(1) {
            if alternative.heading().collect::<Vec<_>>() != heading {
                return Err(StaticTypeError::AlternativeHeadingMismatch);
            }
        }
        Ok(Self {
            alternatives,
            cardinality,
        })
    }

    pub fn alternatives(&self) -> &[RowShape] {
        &self.alternatives
    }

    pub const fn cardinality(&self) -> Cardinality {
        self.cardinality
    }

    pub fn heading(&self) -> impl Iterator<Item = &str> {
        self.alternatives[0].heading()
    }

    pub fn iteration_row(&self) -> RowShape {
        let mut columns = self.alternatives[0].columns().to_vec();
        for alternative in self.alternatives.iter().skip(1) {
            for ((_, accumulated), (_, incoming)) in columns.iter_mut().zip(alternative.columns()) {
                *accumulated = StaticType::union([accumulated.clone(), incoming.clone()]);
            }
        }
        RowShape { columns }
    }

    fn is_subtype_of(&self, other: &Self) -> bool {
        self.cardinality.is_subtype_of(other.cardinality)
            && self.alternatives.iter().all(|left| {
                other
                    .alternatives
                    .iter()
                    .any(|right| left.is_subtype_of(right))
            })
    }

    fn is_disjoint(&self, other: &Self) -> bool {
        if self.cardinality.is_disjoint(other.cardinality) || !self.same_heading(other) {
            return true;
        }
        if self.cardinality.includes_zero() && other.cardinality.includes_zero() {
            return false;
        }
        self.alternatives.iter().all(|left| {
            other
                .alternatives
                .iter()
                .all(|right| left.is_disjoint(right))
        })
    }

    fn intersection(&self, other: &Self) -> Option<Self> {
        let mut cardinality = self.cardinality.intersection(other.cardinality)?;
        if !self.same_heading(other) {
            return None;
        }
        let mut alternatives = Vec::new();
        for left in &self.alternatives {
            for right in &other.alternatives {
                if let Some(intersection) = left.intersection(right) {
                    alternatives.push(intersection);
                }
            }
        }
        if alternatives.is_empty() {
            if !cardinality.includes_zero() {
                return None;
            }
            cardinality = Cardinality::EMPTY;
            alternatives
                .push(self.alternatives[0].intersection_allowing_never(&other.alternatives[0])?);
        }
        alternatives.sort();
        alternatives.dedup();
        Some(Self {
            alternatives,
            cardinality,
        })
    }

    fn same_heading(&self, other: &Self) -> bool {
        self.heading().eq(other.heading())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum StaticType {
    Dynamic,
    Never,
    Kind(ValueKind),
    Literal(StaticLiteral),
    Union(Vec<Self>),
    Relation(RelationType),
    Parameter(TypeParameterId),
    Alias(TypeAliasId, Vec<Self>),
}

impl StaticType {
    pub fn union(types: impl IntoIterator<Item = Self>) -> Self {
        let mut members = Vec::new();
        for ty in types {
            match ty.canonical() {
                Self::Dynamic => return Self::Dynamic,
                Self::Never => {}
                Self::Union(nested) => members.extend(nested),
                ty => members.push(ty),
            }
        }
        members.sort();
        members.dedup();
        let broad = members.clone();
        members.retain(|candidate| {
            !broad
                .iter()
                .any(|other| candidate != other && candidate.is_subtype_of(other))
        });
        match members.len() {
            0 => Self::Never,
            1 => members.pop().unwrap(),
            _ => Self::Union(members),
        }
    }

    pub fn canonical(self) -> Self {
        match self {
            Self::Union(types) => Self::union(types),
            Self::Relation(relation) => Self::Relation(relation),
            Self::Alias(alias, arguments) => {
                Self::Alias(alias, arguments.into_iter().map(Self::canonical).collect())
            }
            ty => ty,
        }
    }

    pub(crate) fn outer_kinds(&self) -> KindSet {
        match self {
            Self::Dynamic | Self::Parameter(_) | Self::Alias(_, _) => KindSet::ALL,
            Self::Never => KindSet::EMPTY,
            Self::Kind(kind) => KindSet::exact(*kind),
            Self::Literal(literal) => KindSet::exact(literal.outer_kind()),
            Self::Union(types) => types
                .iter()
                .fold(KindSet::EMPTY, |kinds, ty| kinds.union(ty.outer_kinds())),
            Self::Relation(_) => KindSet::exact(ValueKind::Relation),
        }
    }

    pub fn exact_outer_kind(&self) -> Option<ValueKind> {
        self.outer_kinds().singleton()
    }

    pub fn is_subtype_of(&self, other: &Self) -> bool {
        if self == other || matches!(self, Self::Never) || matches!(other, Self::Dynamic) {
            return true;
        }
        match (self, other) {
            (Self::Dynamic, _) | (_, Self::Never) => false,
            (Self::Literal(literal), Self::Kind(kind)) => literal.outer_kind() == *kind,
            (Self::Literal(literal), Self::Relation(relation)) => literal
                .relation_type()
                .is_some_and(|literal| literal.is_subtype_of(relation)),
            (Self::Relation(relation), Self::Literal(literal)) => {
                literal.relation_type().is_some_and(|literal| {
                    relation.is_subtype_of(&literal) && literal.is_subtype_of(relation)
                })
            }
            (Self::Relation(_), Self::Kind(ValueKind::Relation)) => true,
            (Self::Relation(left), Self::Relation(right)) => left.is_subtype_of(right),
            (Self::Union(types), _) => types.iter().all(|ty| ty.is_subtype_of(other)),
            (_, Self::Union(types)) => types.iter().any(|ty| self.is_subtype_of(ty)),
            (Self::Alias(left, left_args), Self::Alias(right, right_args)) => {
                left == right && left_args == right_args
            }
            (Self::Parameter(left), Self::Parameter(right)) => left == right,
            _ => false,
        }
    }

    pub fn is_disjoint(&self, other: &Self) -> bool {
        if matches!(self, Self::Never) || matches!(other, Self::Never) {
            return true;
        }
        if matches!(self, Self::Dynamic) || matches!(other, Self::Dynamic) {
            return false;
        }
        match (self, other) {
            (Self::Union(types), _) => types.iter().all(|ty| ty.is_disjoint(other)),
            (_, Self::Union(types)) => types.iter().all(|ty| self.is_disjoint(ty)),
            (Self::Kind(left), Self::Kind(right)) => left != right,
            (Self::Literal(left), Self::Literal(right)) => left != right,
            (Self::Literal(literal), Self::Kind(kind))
            | (Self::Kind(kind), Self::Literal(literal)) => literal.outer_kind() != *kind,
            (Self::Literal(literal), Self::Relation(relation))
            | (Self::Relation(relation), Self::Literal(literal)) => literal
                .relation_type()
                .is_none_or(|literal| literal.is_disjoint(relation)),
            (Self::Relation(_), Self::Kind(kind)) | (Self::Kind(kind), Self::Relation(_)) => {
                *kind != ValueKind::Relation
            }
            (Self::Relation(left), Self::Relation(right)) => left.is_disjoint(right),
            (Self::Parameter(_), Self::Parameter(_)) | (Self::Alias(_, _), Self::Alias(_, _)) => {
                false
            }
            _ => false,
        }
    }

    pub fn intersection(&self, other: &Self) -> Self {
        if self.is_subtype_of(other) {
            return self.clone();
        }
        if other.is_subtype_of(self) {
            return other.clone();
        }
        if self.is_disjoint(other) {
            return Self::Never;
        }
        match (self, other) {
            (Self::Union(types), _) => Self::union(
                types
                    .iter()
                    .map(|ty| ty.intersection(other))
                    .collect::<Vec<_>>(),
            ),
            (_, Self::Union(types)) => Self::union(
                types
                    .iter()
                    .map(|ty| self.intersection(ty))
                    .collect::<Vec<_>>(),
            ),
            (Self::Relation(left), Self::Relation(right)) => {
                left.intersection(right).map_or(Self::Never, Self::Relation)
            }
            _ => Self::Dynamic,
        }
    }
}

impl fmt::Display for StaticType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dynamic => formatter.write_str("dynamic"),
            Self::Never => formatter.write_str("never"),
            Self::Kind(kind) => formatter.write_str(kind.name()),
            Self::Literal(literal) => write!(formatter, "{literal}"),
            Self::Union(types) => {
                for (index, ty) in types.iter().enumerate() {
                    if index != 0 {
                        formatter.write_str(" | ")?;
                    }
                    write!(formatter, "{ty}")?;
                }
                Ok(())
            }
            Self::Relation(relation) => write!(formatter, "{relation}"),
            Self::Parameter(parameter) => write!(formatter, "T{}", parameter.0),
            Self::Alias(alias, arguments) => {
                write!(formatter, "alias#{}", alias.0)?;
                if !arguments.is_empty() {
                    formatter.write_str("<")?;
                    for (index, argument) in arguments.iter().enumerate() {
                        if index != 0 {
                            formatter.write_str(", ")?;
                        }
                        write!(formatter, "{argument}")?;
                    }
                    formatter.write_str(">")?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for StaticLiteral {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(value) => write!(formatter, "{value}"),
            Self::Symbol(value) => write!(formatter, ":{value}"),
            Self::ErrorCode(value) => formatter.write_str(value),
            Self::Unit => formatter.write_str("()"),
            Self::EmptyRelation => formatter.write_str("[] {}"),
        }
    }
}

impl fmt::Display for RowShape {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("{")?;
        for (index, (column, ty)) in self.columns.iter().enumerate() {
            if index != 0 {
                formatter.write_str(", ")?;
            }
            write!(formatter, ":{column} -> {ty}")?;
        }
        formatter.write_str("}")
    }
}

impl fmt::Display for RelationType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("relation<")?;
        for (index, alternative) in self.alternatives.iter().enumerate() {
            if index != 0 {
                formatter.write_str(" | ")?;
            }
            write!(formatter, "{alternative}")?;
        }
        write!(formatter, "> where rows in {}", self.cardinality)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StaticTypeError {
    InvalidCardinality {
        minimum: usize,
        maximum: Option<usize>,
    },
    DuplicateColumn(String),
    MissingRowAlternative,
    AlternativeHeadingMismatch,
}

impl fmt::Display for StaticTypeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCardinality { minimum, maximum } => {
                write!(
                    formatter,
                    "cardinality lower bound {minimum} exceeds upper bound {}",
                    maximum.unwrap()
                )
            }
            Self::DuplicateColumn(column) => {
                write!(formatter, "duplicate structural relation column :{column}")
            }
            Self::MissingRowAlternative => {
                formatter.write_str("structural relation type needs a row alternative")
            }
            Self::AlternativeHeadingMismatch => {
                formatter.write_str("structural relation alternatives must have the same heading")
            }
        }
    }
}

impl std::error::Error for StaticTypeError {}

pub(crate) struct StaticTypeInference<'a> {
    bindings: &'a [Binding],
    locals: Option<&'a HashMap<BindingId, StaticType>>,
    kinds: KindInference<'a>,
}

impl<'a> StaticTypeInference<'a> {
    pub(crate) fn new(
        bindings: &'a [Binding],
        direct_result: &'a dyn Fn(BindingId) -> Option<KindSet>,
        runtime_result: &'a dyn Fn(&str) -> Option<KindSet>,
    ) -> Self {
        Self {
            bindings,
            locals: None,
            kinds: KindInference::new(bindings, direct_result, runtime_result),
        }
    }

    pub(crate) fn with_locals(mut self, locals: &'a HashMap<BindingId, StaticType>) -> Self {
        self.locals = Some(locals);
        self
    }

    pub(crate) fn expr(&self, expr: &HirExpr) -> StaticType {
        match expr {
            HirExpr::Literal { value, .. } => static_literal_type(value),
            HirExpr::Symbol { name, .. } => {
                StaticType::Literal(StaticLiteral::Symbol(name.clone()))
            }
            HirExpr::Relation { heading, rows, .. } => self.relation(heading, rows),
            HirExpr::RelationAtom(atom) => self.relation_scan(&atom.args),
            HirExpr::Call { callee, args, .. } => self
                .relation_operation(callee, args)
                .unwrap_or_else(|| static_type_from_kinds(self.kinds.expr(expr))),
            HirExpr::LocalRef { binding, .. } => self
                .locals
                .and_then(|locals| locals.get(binding).cloned())
                .or_else(|| {
                    self.bindings
                        .get(binding.0 as usize)
                        .and_then(|binding| binding.declared_type.clone())
                })
                .unwrap_or_else(|| static_type_from_kinds(self.kinds.expr(expr))),
            _ => static_type_from_kinds(self.kinds.expr(expr)),
        }
    }

    pub(crate) fn relation(&self, heading: &[String], rows: &[Vec<HirExpr>]) -> StaticType {
        let mut order = (0..heading.len()).collect::<Vec<_>>();
        order.sort_by(|left, right| heading[*left].cmp(&heading[*right]));
        let alternatives = rows
            .iter()
            .map(|row| {
                RowShape::new(
                    order
                        .iter()
                        .map(|position| (heading[*position].clone(), self.expr(&row[*position])))
                        .collect::<Vec<_>>(),
                )
                .expect("validated relation literal headings are unique")
            })
            .collect::<Vec<_>>();
        let alternatives = if alternatives.is_empty() {
            vec![
                RowShape::new(
                    order
                        .iter()
                        .map(|position| (heading[*position].clone(), StaticType::Never)),
                )
                .expect("validated relation literal headings are unique"),
            ]
        } else {
            alternatives
        };

        let constants = rows
            .iter()
            .map(|row| {
                order
                    .iter()
                    .map(|position| constant_value(&row[*position]))
                    .collect::<Option<Vec<_>>>()
            })
            .collect::<Option<Vec<_>>>();
        let cardinality = if let Some(mut constants) = constants {
            constants.sort();
            constants.dedup();
            Cardinality::exact(constants.len())
        } else {
            Cardinality {
                min: 0,
                max: Some(rows.len()),
            }
        };
        StaticType::Relation(
            RelationType::new(alternatives, cardinality)
                .expect("relation literal alternatives share their heading"),
        )
    }

    fn relation_scan(&self, args: &[HirArg]) -> StaticType {
        let mut columns = args
            .iter()
            .filter_map(|arg| match &arg.value {
                HirExpr::QueryVar { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        columns.sort();
        columns.dedup();
        if columns.is_empty() {
            return StaticType::Kind(ValueKind::Bool);
        }
        relation_type_or_dynamic(
            [
                RowShape::new(columns.into_iter().map(|name| (name, StaticType::Dynamic)))
                    .expect("query output names are unique"),
            ],
            Cardinality::UNRESTRICTED,
        )
    }

    fn relation_operation(&self, callee: &HirExpr, args: &[HirArg]) -> Option<StaticType> {
        let HirExpr::ExternalRef { name, .. } = callee else {
            return None;
        };
        match name.as_str() {
            "project" => self.project(args),
            "union" => self.union_or_difference(args, true),
            "difference" => self.union_or_difference(args, false),
            "natural_join" => self.natural_join(args),
            _ => None,
        }
    }

    fn project(&self, args: &[HirArg]) -> Option<StaticType> {
        let source = args.first().map(|arg| self.expr(&arg.value))?;
        let StaticType::Relation(source) = source else {
            return None;
        };
        let mut columns = args
            .iter()
            .skip(1)
            .map(|arg| match &arg.value {
                HirExpr::Symbol { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        columns.sort();
        columns.dedup();
        let mut alternatives = Vec::new();
        for alternative in source.alternatives() {
            let projected = columns
                .iter()
                .map(|column| {
                    alternative
                        .columns()
                        .iter()
                        .find(|(name, _)| name == column)
                        .map(|(name, ty)| (name.clone(), ty.clone()))
                })
                .collect::<Option<Vec<_>>>()?;
            alternatives.push(RowShape::new(projected).ok()?);
        }
        let cardinality = if columns.is_empty() {
            Cardinality {
                min: usize::from(source.cardinality().min > 0),
                max: source
                    .cardinality()
                    .max
                    .map(|maximum| usize::from(maximum > 0)),
            }
        } else {
            Cardinality {
                min: usize::from(source.cardinality().min > 0),
                max: source.cardinality().max,
            }
        };
        Some(relation_type_or_dynamic(alternatives, cardinality))
    }

    fn union_or_difference(&self, args: &[HirArg], union: bool) -> Option<StaticType> {
        let [left, right] = args else {
            return None;
        };
        let StaticType::Relation(left) = self.expr(&left.value) else {
            return None;
        };
        let StaticType::Relation(right) = self.expr(&right.value) else {
            return None;
        };
        if !left.same_heading(&right) {
            return None;
        }
        let (alternatives, cardinality) = if union {
            let alternatives = left
                .alternatives()
                .iter()
                .chain(right.alternatives())
                .cloned()
                .collect::<Vec<_>>();
            let cardinality = Cardinality {
                min: max(left.cardinality().min, right.cardinality().min),
                max: match (left.cardinality().max, right.cardinality().max) {
                    (Some(left), Some(right)) => left.checked_add(right),
                    _ => None,
                },
            };
            (alternatives, cardinality)
        } else {
            (
                left.alternatives().to_vec(),
                Cardinality {
                    min: 0,
                    max: left.cardinality().max,
                },
            )
        };
        Some(relation_type_or_dynamic(alternatives, cardinality))
    }

    fn natural_join(&self, args: &[HirArg]) -> Option<StaticType> {
        let [left, right] = args else {
            return None;
        };
        let StaticType::Relation(left) = self.expr(&left.value) else {
            return None;
        };
        let StaticType::Relation(right) = self.expr(&right.value) else {
            return None;
        };
        let right_heading = right.heading().collect::<Vec<_>>();
        let shared = left.heading().any(|name| right_heading.contains(&name));
        let mut alternatives = Vec::new();
        for left_row in left.alternatives() {
            for right_row in right.alternatives() {
                let mut columns = left_row.columns().to_vec();
                let mut compatible = true;
                for (name, right_type) in right_row.columns() {
                    if let Some((_, left_type)) = columns.iter_mut().find(|(left, _)| left == name)
                    {
                        let intersection = left_type.intersection(right_type);
                        if intersection == StaticType::Never {
                            compatible = false;
                            break;
                        }
                        *left_type = intersection;
                    } else {
                        columns.push((name.clone(), right_type.clone()));
                    }
                }
                if compatible {
                    alternatives.push(RowShape::new(columns).ok()?);
                }
            }
        }
        let compatible = !alternatives.is_empty();
        if !compatible {
            let mut columns = left.alternatives()[0].columns().to_vec();
            for (name, right_type) in right.alternatives()[0].columns() {
                if let Some((_, left_type)) = columns.iter_mut().find(|(left, _)| left == name) {
                    *left_type = left_type.intersection(right_type);
                } else {
                    columns.push((name.clone(), right_type.clone()));
                }
            }
            alternatives.push(RowShape::new(columns).ok()?);
        }
        let cardinality = if !compatible {
            Cardinality::EMPTY
        } else {
            Cardinality {
                min: if shared {
                    0
                } else {
                    left.cardinality()
                        .min
                        .saturating_mul(right.cardinality().min)
                },
                max: match (left.cardinality().max, right.cardinality().max) {
                    (Some(left), Some(right)) => left.checked_mul(right),
                    _ => None,
                },
            }
        };
        Some(relation_type_or_dynamic(alternatives, cardinality))
    }
}

fn relation_type_or_dynamic(
    alternatives: impl IntoIterator<Item = RowShape>,
    cardinality: Cardinality,
) -> StaticType {
    RelationType::new(alternatives, cardinality)
        .map(StaticType::Relation)
        .unwrap_or(StaticType::Dynamic)
}

fn static_literal_type(literal: &Literal) -> StaticType {
    match literal {
        Literal::Bool(value) => StaticType::Literal(StaticLiteral::Bool(*value)),
        Literal::ErrorCode(value) => StaticType::Literal(StaticLiteral::ErrorCode(value.clone())),
        Literal::Nothing => StaticType::Literal(StaticLiteral::EmptyRelation),
        Literal::Int(_) => StaticType::Kind(ValueKind::Int),
        Literal::Float(_) => StaticType::Kind(ValueKind::Float),
        Literal::String(_) => StaticType::Kind(ValueKind::String),
        Literal::Bytes(_) => StaticType::Kind(ValueKind::Bytes),
    }
}

fn static_type_from_kinds(kinds: KindSet) -> StaticType {
    if kinds == KindSet::ALL {
        return StaticType::Dynamic;
    }
    StaticType::union(kinds.iter().map(StaticType::Kind))
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum ConstantValue {
    Int(String),
    Float(String),
    String(String),
    Bytes(Vec<u8>),
    Bool(bool),
    ErrorCode(String),
    EmptyRelation,
    Identity(String),
    Symbol(String),
    Frob(String, Box<Self>),
    List(Vec<Self>),
    Relation {
        heading: Vec<String>,
        rows: Vec<Vec<Self>>,
    },
    Map(Vec<(Self, Self)>),
}

fn constant_value(expr: &HirExpr) -> Option<ConstantValue> {
    match expr {
        HirExpr::Literal { value, .. } => Some(match value {
            Literal::Int(value) => ConstantValue::Int(value.clone()),
            Literal::Float(value) => ConstantValue::Float(value.clone()),
            Literal::String(value) => ConstantValue::String(value.clone()),
            Literal::Bytes(value) => ConstantValue::Bytes(value.clone()),
            Literal::Bool(value) => ConstantValue::Bool(*value),
            Literal::ErrorCode(value) => ConstantValue::ErrorCode(value.clone()),
            Literal::Nothing => ConstantValue::EmptyRelation,
        }),
        HirExpr::Identity { name, .. } => Some(ConstantValue::Identity(name.clone())),
        HirExpr::Symbol { name, .. } => Some(ConstantValue::Symbol(name.clone())),
        HirExpr::Frob {
            delegate, value, ..
        } => Some(ConstantValue::Frob(
            delegate.clone(),
            Box::new(constant_value(value)?),
        )),
        HirExpr::List { items, .. } => Some(ConstantValue::List(
            items
                .iter()
                .map(|item| match item {
                    HirCollectionItem::Expr(value) => constant_value(value),
                    HirCollectionItem::Splice(_) => None,
                })
                .collect::<Option<Vec<_>>>()?,
        )),
        HirExpr::Relation { heading, rows, .. } => {
            let mut order = (0..heading.len()).collect::<Vec<_>>();
            order.sort_by(|left, right| heading[*left].cmp(&heading[*right]));
            let heading = order
                .iter()
                .map(|position| heading[*position].clone())
                .collect();
            let mut rows = rows
                .iter()
                .map(|row| {
                    order
                        .iter()
                        .map(|position| constant_value(&row[*position]))
                        .collect::<Option<Vec<_>>>()
                })
                .collect::<Option<Vec<_>>>()?;
            rows.sort();
            rows.dedup();
            Some(ConstantValue::Relation { heading, rows })
        }
        HirExpr::Map { entries, .. } => {
            let entries = entries
                .iter()
                .map(|(key, value)| Some((constant_value(key)?, constant_value(value)?)))
                .collect::<Option<Vec<_>>>()?;
            let canonical = entries.into_iter().collect::<BTreeMap<_, _>>();
            Some(ConstantValue::Map(canonical.into_iter().collect()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HirProgram, parse_semantic};
    use mica_relation_kernel::relation_algebra;
    use mica_var::{RelationValue, Symbol, Tuple, Value};

    fn row(columns: impl IntoIterator<Item = (&'static str, StaticType)>) -> RowShape {
        RowShape::new(columns.into_iter().map(|(name, ty)| (name.to_owned(), ty))).unwrap()
    }

    fn relation(alternatives: Vec<RowShape>, cardinality: Cardinality) -> StaticType {
        StaticType::Relation(RelationType::new(alternatives, cardinality).unwrap())
    }

    fn inferred(source: &str) -> StaticType {
        let semantic = parse_semantic(source);
        assert!(semantic.parse_errors.is_empty());
        assert!(semantic.diagnostics.is_empty());
        let HirProgram { items } = &semantic.hir;
        let crate::HirItem::Expr { expr, .. } = &items[0] else {
            panic!("expected expression");
        };
        StaticTypeInference::new(&semantic.bindings, &|_| None, &|_| None).expr(expr)
    }

    #[test]
    fn canonicalizes_heading_and_union_order() {
        let left = relation(
            vec![row([
                ("z", StaticType::Kind(ValueKind::String)),
                ("a", StaticType::Kind(ValueKind::Int)),
            ])],
            Cardinality::EXACTLY_ONE,
        );
        let right = relation(
            vec![row([
                ("a", StaticType::Kind(ValueKind::Int)),
                ("z", StaticType::Kind(ValueKind::String)),
            ])],
            Cardinality::EXACTLY_ONE,
        );
        assert_eq!(left, right);
        assert_eq!(
            StaticType::union([
                StaticType::Kind(ValueKind::String),
                StaticType::Kind(ValueKind::Int),
            ]),
            StaticType::union([
                StaticType::Kind(ValueKind::Int),
                StaticType::Kind(ValueKind::String),
            ])
        );
    }

    #[test]
    fn incompatible_headings_are_disjoint_without_width_subtyping() {
        let left = relation(
            vec![row([("value", StaticType::Kind(ValueKind::Int))])],
            Cardinality::OPTIONAL,
        );
        let right = relation(
            vec![row([("other", StaticType::Kind(ValueKind::Int))])],
            Cardinality::OPTIONAL,
        );
        assert!(left.is_disjoint(&right));
        assert!(!left.is_subtype_of(&right));
    }

    #[test]
    fn cardinality_subtyping_and_intersection_are_inclusive() {
        assert!(Cardinality::EXACTLY_ONE.is_subtype_of(Cardinality::OPTIONAL));
        assert!(Cardinality::EXACTLY_ONE.is_subtype_of(Cardinality::NON_EMPTY));
        assert!(!Cardinality::OPTIONAL.is_subtype_of(Cardinality::EXACTLY_ONE));
        assert!(Cardinality::EMPTY.is_disjoint(Cardinality::NON_EMPTY));
        assert_eq!(
            Cardinality::OPTIONAL.intersection(Cardinality::NON_EMPTY),
            Some(Cardinality::EXACTLY_ONE)
        );
    }

    #[test]
    fn discriminator_intersection_narrows_row_alternatives() {
        let ok = StaticType::Literal(StaticLiteral::Symbol("ok".to_owned()));
        let error = StaticType::Literal(StaticLiteral::Symbol("error".to_owned()));
        let result = relation(
            vec![
                row([
                    ("case", ok.clone()),
                    ("value", StaticType::Kind(ValueKind::String)),
                ]),
                row([
                    ("case", error),
                    ("value", StaticType::Kind(ValueKind::Error)),
                ]),
            ],
            Cardinality::EXACTLY_ONE,
        );
        let ok_pattern = relation(
            vec![row([("case", ok), ("value", StaticType::Dynamic)])],
            Cardinality::EXACTLY_ONE,
        );
        assert_eq!(
            result.intersection(&ok_pattern),
            relation(
                vec![row([
                    (
                        "case",
                        StaticType::Literal(StaticLiteral::Symbol("ok".to_owned()))
                    ),
                    ("value", StaticType::Kind(ValueKind::String)),
                ])],
                Cardinality::EXACTLY_ONE,
            )
        );
    }

    #[test]
    fn relation_iteration_preserves_heading_and_joins_variant_cell_types() {
        let relation = RelationType::new(
            [
                row([
                    (
                        "case",
                        StaticType::Literal(StaticLiteral::Symbol("ok".to_owned())),
                    ),
                    ("value", StaticType::Kind(ValueKind::String)),
                ]),
                row([
                    (
                        "case",
                        StaticType::Literal(StaticLiteral::Symbol("error".to_owned())),
                    ),
                    ("value", StaticType::Kind(ValueKind::Error)),
                ]),
            ],
            Cardinality::UNRESTRICTED,
        )
        .unwrap();
        let iteration = relation.iteration_row();
        assert_eq!(
            iteration.heading().collect::<Vec<_>>(),
            vec!["case", "value"]
        );
        assert_eq!(
            iteration.columns()[1].1,
            StaticType::union([
                StaticType::Kind(ValueKind::String),
                StaticType::Kind(ValueKind::Error),
            ])
        );
    }

    #[test]
    fn none_some_unit_empty_relation_and_nested_options_remain_distinct() {
        let none = relation(
            vec![row([("value", StaticType::Never)])],
            Cardinality::EMPTY,
        );
        let some_unit = relation(
            vec![row([("value", StaticType::Literal(StaticLiteral::Unit))])],
            Cardinality::EXACTLY_ONE,
        );
        let some_empty = relation(
            vec![row([(
                "value",
                StaticType::Literal(StaticLiteral::EmptyRelation),
            )])],
            Cardinality::EXACTLY_ONE,
        );
        let some_none = relation(
            vec![row([("value", none.clone())])],
            Cardinality::EXACTLY_ONE,
        );
        assert_ne!(none, some_unit);
        assert_ne!(some_unit, some_empty);
        assert_ne!(some_empty, some_none);
        assert!(
            inferred("[] {}").is_subtype_of(&StaticType::Literal(StaticLiteral::EmptyRelation))
        );
    }

    #[test]
    fn relation_literal_inference_deduplicates_constant_rows() {
        let ty = inferred("[:value] { [1], [1], [2] }");
        let StaticType::Relation(relation) = ty else {
            panic!("expected relation type");
        };
        assert_eq!(relation.cardinality(), Cardinality::exact(2));
        assert_eq!(relation.alternatives().len(), 1);
        assert_eq!(
            relation.alternatives()[0].columns()[0].1,
            StaticType::Kind(ValueKind::Int)
        );
    }

    #[test]
    fn relation_literal_inference_uses_literal_discriminators_and_dynamic_bounds() {
        let result = inferred("[:case, :value] { [:ok, 1], [:error, E_FAIL] }");
        let StaticType::Relation(result) = result else {
            panic!("expected relation type");
        };
        assert_eq!(result.cardinality(), Cardinality::exact(2));
        assert_eq!(result.alternatives().len(), 2);

        let dynamic = inferred("[:value] { [actor()] }");
        let StaticType::Relation(dynamic) = dynamic else {
            panic!("expected relation type");
        };
        assert_eq!(
            dynamic.cardinality(),
            Cardinality {
                min: 0,
                max: Some(1)
            }
        );
    }

    #[test]
    fn relation_operations_preserve_headings_cells_and_conservative_cardinality() {
        let projected = inferred("project([:id, :name] { [1, \"one\"], [2, \"two\"] }, :name)");
        let StaticType::Relation(projected) = projected else {
            panic!("expected projected relation type");
        };
        assert_eq!(projected.heading().collect::<Vec<_>>(), vec!["name"]);
        assert_eq!(
            projected.cardinality(),
            Cardinality {
                min: 1,
                max: Some(2)
            }
        );
        assert_eq!(
            projected.alternatives()[0].columns()[0].1,
            StaticType::Kind(ValueKind::String)
        );

        let union = inferred("union([:id] { [1], [2] }, [:id] { [2], [3], [4] })");
        let StaticType::Relation(union) = union else {
            panic!("expected union relation type");
        };
        assert_eq!(
            union.cardinality(),
            Cardinality {
                min: 3,
                max: Some(5)
            }
        );

        let difference = inferred("difference([:id] { [1], [2] }, [:id] { [2] })");
        let StaticType::Relation(difference) = difference else {
            panic!("expected difference relation type");
        };
        assert_eq!(
            difference.cardinality(),
            Cardinality {
                min: 0,
                max: Some(2)
            }
        );

        let joined =
            inferred("natural_join([:id, :name] { [1, \"one\"] }, [:active, :id] { [true, 1] })");
        let StaticType::Relation(joined) = joined else {
            panic!("expected joined relation type");
        };
        assert_eq!(
            joined.heading().collect::<Vec<_>>(),
            vec!["active", "id", "name"]
        );
        assert_eq!(
            joined.cardinality(),
            Cardinality {
                min: 0,
                max: Some(1)
            }
        );
    }

    #[test]
    fn relation_scans_publish_dynamic_cells_and_unrestricted_cardinality() {
        let scan = inferred("Thing(?id, ?name)");
        let StaticType::Relation(scan) = scan else {
            panic!("expected scan relation type");
        };
        assert_eq!(scan.heading().collect::<Vec<_>>(), vec!["id", "name"]);
        assert_eq!(scan.cardinality(), Cardinality::UNRESTRICTED);
        assert!(
            scan.alternatives()[0]
                .columns()
                .iter()
                .all(|(_, ty)| *ty == StaticType::Dynamic)
        );
        assert_eq!(inferred("Thing(1)"), StaticType::Kind(ValueKind::Bool));
    }

    #[test]
    fn dynamic_relation_operations_fall_back_without_false_shapes() {
        assert_eq!(inferred("project(load(), :value)"), StaticType::Dynamic);
        assert_eq!(inferred("union(load(), [:value] {})"), StaticType::Dynamic);
    }

    #[test]
    fn inferred_operation_upper_bounds_cover_observed_set_results() {
        let id = Symbol::intern("id");
        let name = Symbol::intern("name");
        let active = Symbol::intern("active");
        let left = RelationValue::new(
            [id, name],
            [
                Tuple::from([Value::int(1).unwrap(), Value::string("one")]),
                Tuple::from([Value::int(2).unwrap(), Value::string("two")]),
            ],
        )
        .unwrap();
        let right = RelationValue::new(
            [active, id],
            [
                Tuple::from([Value::bool(true), Value::int(1).unwrap()]),
                Tuple::from([Value::bool(false), Value::int(3).unwrap()]),
            ],
        )
        .unwrap();
        let projected = relation_algebra::project(&left, [name]).unwrap();
        let joined = relation_algebra::natural_join(&left, &right).unwrap();

        for (source, observed) in [
            (
                "project([:id, :name] { [1, \"one\"], [2, \"two\"] }, :name)",
                projected.len(),
            ),
            (
                "natural_join([:id, :name] { [1, \"one\"], [2, \"two\"] }, [:active, :id] { [true, 1], [false, 3] })",
                joined.len(),
            ),
        ] {
            let StaticType::Relation(inferred) = inferred(source) else {
                panic!("expected inferred relation");
            };
            assert!(
                inferred
                    .cardinality()
                    .max
                    .is_none_or(|maximum| observed <= maximum),
                "observed {observed} rows exceeds inferred bound for {source}"
            );
        }
    }

    #[test]
    fn diagnostic_rendering_is_canonical_and_specific() {
        let ty = relation(
            vec![row([
                (
                    "case",
                    StaticType::Literal(StaticLiteral::Symbol("ok".to_owned())),
                ),
                ("value", StaticType::Kind(ValueKind::String)),
            ])],
            Cardinality::EXACTLY_ONE,
        );
        assert_eq!(
            ty.to_string(),
            "relation<{:case -> :ok, :value -> string}> where rows in 1"
        );
    }
}
