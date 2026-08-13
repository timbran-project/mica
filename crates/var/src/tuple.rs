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

use crate::{Symbol, Value};
use std::fmt;
use std::sync::{Arc, LazyLock};

/// An immutable row of Mica values.
///
/// A tuple has no relation identity or schema of its own. Relation storage and
/// relation values supply that surrounding context.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Tuple(Arc<[Value]>);

impl Tuple {
    pub fn new(values: impl IntoIterator<Item = Value>) -> Self {
        Self(values.into_iter().collect::<Vec<_>>().into())
    }

    pub fn values(&self) -> &[Value] {
        &self.0
    }

    pub fn arity(&self) -> usize {
        self.0.len()
    }

    pub fn select(&self, positions: impl IntoIterator<Item = u16>) -> Self {
        Self::new(
            positions
                .into_iter()
                .map(|position| self.0[position as usize].clone()),
        )
    }

    pub fn concat(&self, other: &Self) -> Self {
        Self::new(self.0.iter().cloned().chain(other.0.iter().cloned()))
    }

    pub fn matches_bindings(&self, bindings: &[Option<Value>]) -> bool {
        bindings
            .iter()
            .enumerate()
            .all(|(index, binding)| binding.as_ref().is_none_or(|value| &self.0[index] == value))
    }
}

impl<const N: usize> From<[Value; N]> for Tuple {
    fn from(value: [Value; N]) -> Self {
        Self::new(value)
    }
}

/// An immutable finite relation with a named heading and canonical set of rows.
///
/// Relation values use the generic value codec for task, storage, and host
/// boundaries. Persistability is determined recursively from their cells.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RelationValue {
    heading: Arc<[Symbol]>,
    rows: Arc<[Tuple]>,
}

impl RelationValue {
    pub fn new(
        heading: impl IntoIterator<Item = Symbol>,
        rows: impl IntoIterator<Item = Tuple>,
    ) -> Result<Self, RelationValueError> {
        let heading = heading.into_iter().collect::<Vec<_>>();
        if heading.len() > u16::MAX as usize {
            return Err(RelationValueError::HeadingTooWide(heading.len()));
        }

        let mut order = (0..heading.len()).collect::<Vec<_>>();
        order.sort_by_key(|position| heading[*position]);
        for positions in order.windows(2) {
            if heading[positions[0]] == heading[positions[1]] {
                return Err(RelationValueError::DuplicateColumn(heading[positions[0]]));
            }
        }

        let canonical_heading = order
            .iter()
            .map(|position| heading[*position])
            .collect::<Arc<[_]>>();
        let already_ordered = order
            .iter()
            .enumerate()
            .all(|(canonical, original)| canonical == *original);

        let mut canonical_rows = Vec::new();
        let mut rows_are_strictly_ordered = true;
        for row in rows {
            if row.arity() != heading.len() {
                return Err(RelationValueError::ArityMismatch {
                    expected: heading.len(),
                    actual: row.arity(),
                });
            }
            let row = if already_ordered {
                row
            } else {
                row.select(order.iter().map(|position| *position as u16))
            };
            if rows_are_strictly_ordered
                && canonical_rows
                    .last()
                    .is_some_and(|previous| previous >= &row)
            {
                rows_are_strictly_ordered = false;
            }
            canonical_rows.push(row);
        }
        if !rows_are_strictly_ordered {
            canonical_rows.sort();
            canonical_rows.dedup();
        }

        Ok(Self {
            heading: canonical_heading,
            rows: canonical_rows.into(),
        })
    }

    /// Constructs a relation with at most one row without allocating sorting
    /// scratch when its heading is already canonical.
    pub fn new_small(
        heading: impl IntoIterator<Item = Symbol>,
        row: Option<Tuple>,
    ) -> Result<Self, RelationValueError> {
        let heading = heading.into_iter().collect::<Vec<_>>();
        if heading.len() > u16::MAX as usize {
            return Err(RelationValueError::HeadingTooWide(heading.len()));
        }
        if !heading.windows(2).all(|columns| columns[0] < columns[1]) {
            return Self::new(heading, row);
        }
        if let Some(row) = &row
            && row.arity() != heading.len()
        {
            return Err(RelationValueError::ArityMismatch {
                expected: heading.len(),
                actual: row.arity(),
            });
        }
        Ok(Self {
            heading: heading.into(),
            rows: row.into_iter().collect::<Vec<_>>().into(),
        })
    }

    pub fn heading(&self) -> &[Symbol] {
        &self.heading
    }

    pub fn rows(&self) -> &[Tuple] {
        &self.rows
    }

    pub fn arity(&self) -> usize {
        self.heading.len()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn column_position(&self, column: Symbol) -> Option<usize> {
        self.heading.binary_search(&column).ok()
    }
}

pub(crate) fn empty_relation() -> &'static RelationValue {
    static EMPTY: LazyLock<RelationValue> = LazyLock::new(|| {
        RelationValue::new([], []).expect("the zero-column empty relation is valid")
    });
    &EMPTY
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelationValueError {
    HeadingTooWide(usize),
    DuplicateColumn(Symbol),
    ArityMismatch { expected: usize, actual: usize },
}

impl fmt::Display for RelationValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HeadingTooWide(arity) => {
                write!(
                    f,
                    "relation heading has {arity} columns; maximum is {}",
                    u16::MAX
                )
            }
            Self::DuplicateColumn(column) => match column.name() {
                Some(name) => write!(f, "duplicate relation column :{name}"),
                None => write!(f, "duplicate relation column :#{}", column.id()),
            },
            Self::ArityMismatch { expected, actual } => {
                write!(
                    f,
                    "relation row arity mismatch: expected {expected}, got {actual}"
                )
            }
        }
    }
}

impl std::error::Error for RelationValueError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tuple_selects_and_concatenates_values() {
        let row = Tuple::from([Value::int(1).unwrap(), Value::int(2).unwrap()]);
        let selected = row.select([1, 0]);
        let concatenated = selected.concat(&Tuple::from([Value::bool(true)]));

        assert_eq!(
            concatenated.values(),
            [
                Value::int(2).unwrap(),
                Value::int(1).unwrap(),
                Value::bool(true)
            ]
        );
    }

    #[test]
    fn tuple_matches_partial_bindings() {
        let row = Tuple::from([Value::int(1).unwrap(), Value::bool(true)]);

        assert!(row.matches_bindings(&[Some(Value::int(1).unwrap()), None]));
        assert!(!row.matches_bindings(&[None, Some(Value::bool(false))]));
    }

    #[test]
    fn relation_canonicalizes_heading_rows_and_duplicates() {
        let a = Symbol::intern("relation-canonical-column-a");
        let b = Symbol::intern("relation-canonical-column-b");
        let relation = RelationValue::new(
            [b, a],
            [
                Tuple::from([Value::int(4).unwrap(), Value::int(3).unwrap()]),
                Tuple::from([Value::int(2).unwrap(), Value::int(1).unwrap()]),
                Tuple::from([Value::int(2).unwrap(), Value::int(1).unwrap()]),
            ],
        )
        .unwrap();

        let equivalent = RelationValue::new(
            [a, b],
            [
                Tuple::from([Value::int(1).unwrap(), Value::int(2).unwrap()]),
                Tuple::from([Value::int(3).unwrap(), Value::int(4).unwrap()]),
            ],
        )
        .unwrap();
        assert_eq!(relation, equivalent);
        assert_eq!(relation.heading(), [a.min(b), a.max(b)]);
        assert_eq!(
            relation.column_position(b),
            relation.heading().iter().position(|c| *c == b)
        );
    }

    #[test]
    fn empty_relation_retains_its_heading() {
        let relation = RelationValue::new([Symbol::intern("value")], []).unwrap();

        assert_eq!(relation.arity(), 1);
        assert!(relation.is_empty());
    }

    #[test]
    fn zero_arity_relations_distinguish_empty_from_unit() {
        let empty = RelationValue::new([], []).unwrap();
        let unit = RelationValue::new([], [Tuple::new([]), Tuple::new([])]).unwrap();

        assert!(empty.is_empty());
        assert_eq!(unit.len(), 1);
        assert_ne!(empty, unit);
    }

    #[test]
    fn relation_rejects_duplicate_columns_and_wrong_arity() {
        let value = Symbol::intern("value");
        assert_eq!(
            RelationValue::new([value, value], []).unwrap_err(),
            RelationValueError::DuplicateColumn(value)
        );
        assert_eq!(
            RelationValue::new([value], [Tuple::new([])]).unwrap_err(),
            RelationValueError::ArityMismatch {
                expected: 1,
                actual: 0
            }
        );
    }

    #[test]
    fn small_relation_constructor_preserves_general_canonical_semantics() {
        let left = Symbol::intern("left");
        let right = Symbol::intern("right");
        let row = Tuple::from([Value::int(1).unwrap(), Value::string("one")]);
        assert_eq!(
            RelationValue::new_small([left, right], Some(row.clone())).unwrap(),
            RelationValue::new([left, right], [row]).unwrap()
        );

        let unsorted = Tuple::from([Value::string("one"), Value::int(1).unwrap()]);
        assert_eq!(
            RelationValue::new_small([right, left], Some(unsorted.clone())).unwrap(),
            RelationValue::new([right, left], [unsorted]).unwrap()
        );
        assert_eq!(
            RelationValue::new_small([left], None).unwrap(),
            RelationValue::new([left], []).unwrap()
        );
        assert_eq!(
            RelationValue::new_small([left, left], None).unwrap_err(),
            RelationValueError::DuplicateColumn(left)
        );
    }
}
