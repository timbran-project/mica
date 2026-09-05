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

use crate::ScanControl;
use crate::error::KernelError;
use crate::metadata::RelationMetadata;
use crate::relation_algebra::finish_tuple_rows;
use crate::tuple::{Tuple, TupleKey};
use mica_var::Value;
pub(crate) use tuple_index::ProjectedTupleIndex;
use tuple_index::TupleIndex;
use tuple_store::TupleStore;

mod tuple_bucket;
mod tuple_index;
mod tuple_store;

#[derive(Clone, Copy)]
pub(crate) enum RelationMutationKind {
    Assert,
    Retract,
}

#[derive(Clone, Debug)]
pub(crate) struct RelationState {
    metadata: RelationMetadata,
    tuples: TupleStore,
    indexes: Vec<TupleIndex>,
    non_immediate_counts: Vec<usize>,
}

impl RelationState {
    pub(crate) fn empty(metadata: RelationMetadata) -> Result<Self, KernelError> {
        validate_metadata(&metadata)?;
        let arity = metadata.arity();
        let indexes = metadata
            .indexes
            .iter()
            .filter(|spec| !is_natural_full_tuple_index(spec.positions(), arity))
            .cloned()
            .map(|spec| TupleIndex::empty(spec, arity))
            .collect();
        Ok(Self {
            metadata,
            tuples: TupleStore::empty(),
            indexes,
            non_immediate_counts: vec![0; arity as usize],
        })
    }

    pub(crate) fn metadata(&self) -> &RelationMetadata {
        &self.metadata
    }

    pub(crate) fn index_storage_kind(&self, ordinal: usize) -> Option<&'static str> {
        let index = self.metadata.indexes().get(ordinal)?;
        Some(
            if is_natural_full_tuple_index(index.positions(), self.metadata.arity()) {
                self.tuples.storage_kind()
            } else {
                "radix"
            },
        )
    }

    pub(crate) fn cardinality(&self) -> usize {
        self.tuples.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.tuples.is_empty()
    }

    pub(crate) fn value_domains(&self) -> Vec<crate::ValueDomain> {
        self.non_immediate_counts
            .iter()
            .map(|non_immediate| match *non_immediate {
                0 => crate::ValueDomain::Immediate,
                count if count == self.cardinality() => crate::ValueDomain::Heap,
                _ => crate::ValueDomain::Mixed,
            })
            .collect()
    }

    pub(crate) fn contains_tuple(&self, tuple: &Tuple) -> bool {
        self.tuples.contains(tuple)
    }

    pub(crate) fn validate_tuple(&self, tuple: &Tuple) -> Result<(), KernelError> {
        if self.metadata.arity() as usize != tuple.arity() {
            return Err(KernelError::ArityMismatch {
                relation: self.metadata.id(),
                expected: self.metadata.arity(),
                actual: tuple.arity(),
            });
        }
        if tuple.values().iter().any(|value| !value.is_persistable()) {
            return Err(KernelError::NonPersistentValue {
                relation: self.metadata.id(),
                tuple: tuple.clone(),
            });
        }
        Ok(())
    }

    pub(crate) fn estimate_scan_count(
        &self,
        bindings: &[Option<Value>],
    ) -> Result<usize, KernelError> {
        self.validate_bindings(bindings)?;
        if let Some(values) = full_tuple_binding_values(bindings) {
            return Ok(usize::from(self.tuples.tuple_for_values(&values).is_some()));
        }

        match self.checked_access(bindings)? {
            Some(access) => access.estimate_prefix_count(bindings),
            None if bindings.iter().any(Option::is_some) => Ok(self.cardinality()),
            None => Ok(self.cardinality()),
        }
    }

    pub(crate) fn scan(&self, bindings: &[Option<Value>]) -> Result<Vec<Tuple>, KernelError> {
        self.validate_bindings(bindings)?;
        if let Some(values) = full_tuple_binding_values(bindings) {
            return Ok(self.tuples.tuple_for_values(&values).into_iter().collect());
        }

        match self.checked_access(bindings)? {
            Some(access) => access.scan_prefix(bindings),
            None => Ok(self.tuples.matching(bindings)),
        }
    }

    pub(crate) fn visit(
        &self,
        bindings: &[Option<Value>],
        visitor: &mut dyn FnMut(&Tuple) -> Result<ScanControl, KernelError>,
    ) -> Result<(), KernelError> {
        self.validate_bindings(bindings)?;
        if let Some(values) = full_tuple_binding_values(bindings) {
            if let Some(tuple) = self.tuples.tuple_for_values(&values) {
                let _ = visitor(&tuple)?;
            }
            return Ok(());
        }

        match self.checked_access(bindings)? {
            Some(access) => access.visit_prefix(bindings, visitor),
            None => self.visit_matching(bindings, visitor),
        }
    }

    pub(crate) fn join_eq(
        &self,
        left_bindings: &[Option<Value>],
        left_positions: &[u16],
        right: &Self,
        right_bindings: &[Option<Value>],
        right_positions: &[u16],
    ) -> Result<Option<Vec<Tuple>>, KernelError> {
        if left_bindings.len() != self.metadata.arity() as usize {
            return Err(KernelError::ArityMismatch {
                relation: self.metadata.id(),
                expected: self.metadata.arity(),
                actual: left_bindings.len(),
            });
        }
        if right_bindings.len() != right.metadata.arity() as usize {
            return Err(KernelError::ArityMismatch {
                relation: right.metadata.id(),
                expected: right.metadata.arity(),
                actual: right_bindings.len(),
            });
        }

        if is_natural_full_tuple_positions(left_positions, self.metadata.arity())
            && is_natural_full_tuple_positions(right_positions, right.metadata.arity())
        {
            return Ok(Some(self.natural_full_tuple_join(
                left_bindings,
                right,
                right_bindings,
            )));
        }

        let Some(left_index) = self.index_for_positions(left_positions) else {
            return Ok(None);
        };
        let Some(right_index) = right.index_for_positions(right_positions) else {
            return Ok(None);
        };

        let mut out = Vec::new();
        left_index.matching_row_pairs(
            right_index,
            left_bindings,
            right_bindings,
            |left_tuple, right_tuple| {
                out.push(left_tuple.concat(right_tuple));
            },
        );
        Ok(Some(finish_tuple_rows(out)))
    }

    fn natural_full_tuple_join(
        &self,
        left_bindings: &[Option<Value>],
        right: &Self,
        right_bindings: &[Option<Value>],
    ) -> Vec<Tuple> {
        let mut out = Vec::new();
        self.tuples
            .matching_row_pairs(&right.tuples, |left, right| {
                if left.matches_bindings(left_bindings) && right.matches_bindings(right_bindings) {
                    out.push(left.concat(right));
                }
            });
        finish_tuple_rows(out)
    }

    pub(crate) fn has_exact_index(&self, positions: &[u16]) -> bool {
        is_natural_full_tuple_positions(positions, self.metadata.arity())
            || self.index_for_positions(positions).is_some()
    }

    pub(crate) fn insert(&mut self, tuple: Tuple) -> bool {
        self.insert_indexed(tuple)
    }

    pub(crate) fn remove(&mut self, tuple: &Tuple) -> bool {
        self.remove_indexed(tuple)
    }

    pub(crate) fn apply_ordered_changes<'a>(
        &mut self,
        changes: impl IntoIterator<Item = (&'a Tuple, RelationMutationKind)>,
        mut base_contains: impl FnMut(&Tuple) -> bool,
        mut on_applied: impl FnMut(&Tuple, RelationMutationKind),
    ) {
        for (tuple, kind) in changes {
            match kind {
                RelationMutationKind::Assert => {
                    if self.insert_indexed(tuple.clone()) {
                        on_applied(tuple, kind);
                    }
                }
                RelationMutationKind::Retract => {
                    if base_contains(tuple) && self.remove_indexed(tuple) {
                        on_applied(tuple, kind);
                    }
                }
            }
        }
    }

    pub(crate) fn apply_ordered_asserts_to_empty<'a>(
        &mut self,
        tuples: impl IntoIterator<Item = &'a Tuple>,
        mut on_applied: impl FnMut(&Tuple, RelationMutationKind),
    ) {
        debug_assert!(self.tuples.is_empty());

        let mut rows = Vec::new();
        for tuple in tuples {
            if rows.last().is_some_and(|existing| existing == tuple) {
                continue;
            }
            rows.push(tuple.clone());
        }

        if rows.is_empty() {
            return;
        }

        self.tuples = TupleStore::from_sorted_unique(&rows);
        self.non_immediate_counts.fill(0);
        for tuple in &rows {
            self.record_value_domains(tuple, true);
        }
        let arity = self.metadata.arity();
        for index in &mut self.indexes {
            index.rebuild_from_sorted_unique_rows(arity, &rows);
        }
        for tuple in &rows {
            on_applied(tuple, RelationMutationKind::Assert);
        }
    }

    pub(crate) fn tuple_for_key(&self, positions: &[u16], key_tuple: &Tuple) -> Option<Tuple> {
        if is_natural_full_tuple_positions(positions, self.metadata.arity()) {
            return self.tuples.tuple_for_tuple(key_tuple);
        }
        if let Some(index) = self.index_for_positions(positions) {
            return index.tuple_for_key_tuple(key_tuple);
        }

        let mut bindings = vec![None; self.metadata.arity() as usize];
        for position in positions {
            bindings[*position as usize] = Some(key_tuple.values()[*position as usize].clone());
        }

        let mut found = None;
        self.visit(&bindings, &mut |tuple| {
            found = Some(tuple.clone());
            Ok(ScanControl::Stop)
        })
        .ok()?;
        found
    }

    pub(crate) fn tuple_for_projected_key(
        &self,
        positions: &[u16],
        key: &TupleKey,
    ) -> Option<Tuple> {
        self.tuple_for_projected_values(positions, &key.0)
    }

    fn tuple_for_projected_values(&self, positions: &[u16], key_values: &[Value]) -> Option<Tuple> {
        if is_natural_full_tuple_positions(positions, self.metadata.arity()) {
            return self.tuples.tuple_for_values(key_values);
        }
        if let Some(index) = self.index_for_positions(positions) {
            return index.tuple_for_key_values(key_values);
        }

        let mut bindings = vec![None; self.metadata.arity() as usize];
        for (position, value) in positions.iter().zip(key_values) {
            bindings[*position as usize] = Some(value.clone());
        }

        let mut found = None;
        self.visit(&bindings, &mut |tuple| {
            found = Some(tuple.clone());
            Ok(ScanControl::Stop)
        })
        .ok()?;
        found
    }

    fn checked_access(
        &self,
        bindings: &[Option<Value>],
    ) -> Result<Option<ScanAccess<'_>>, KernelError> {
        self.validate_bindings(bindings)?;
        Ok(self.best_access(bindings))
    }

    fn validate_bindings(&self, bindings: &[Option<Value>]) -> Result<(), KernelError> {
        if bindings.len() != self.metadata.arity() as usize {
            return Err(KernelError::ArityMismatch {
                relation: self.metadata.id(),
                expected: self.metadata.arity(),
                actual: bindings.len(),
            });
        }
        Ok(())
    }

    fn best_access(&self, bindings: &[Option<Value>]) -> Option<ScanAccess<'_>> {
        let tuple_store_bound_count = natural_leading_bound_count(bindings);
        let best_index = self
            .indexes
            .iter()
            .map(|index| (index, index.leading_bound_count(bindings)))
            .filter(|(_, count)| *count > 0)
            .max_by_key(|(_, count)| *count);

        match (tuple_store_bound_count, best_index) {
            (0, None) => None,
            (count, None) => Some(ScanAccess::TupleStore(&self.tuples, count)),
            (0, Some((index, count))) => Some(ScanAccess::Index(index, count)),
            (tuple_count, Some((_index, index_count))) if tuple_count > index_count => {
                Some(ScanAccess::TupleStore(&self.tuples, tuple_count))
            }
            (_, Some((index, count))) => Some(ScanAccess::Index(index, count)),
        }
    }

    fn visit_matching(
        &self,
        bindings: &[Option<Value>],
        visitor: &mut dyn FnMut(&Tuple) -> Result<ScanControl, KernelError>,
    ) -> Result<(), KernelError> {
        let mut stopped = false;
        self.tuples.try_for_each_matching(bindings, &mut |tuple| {
            if visitor(tuple)? == ScanControl::Stop {
                stopped = true;
            }
            Ok(stopped)
        })?;
        Ok(())
    }

    fn index_for_positions(&self, positions: &[u16]) -> Option<&TupleIndex> {
        self.indexes
            .iter()
            .find(|index| index.positions() == positions)
    }

    fn insert_indexed(&mut self, tuple: Tuple) -> bool {
        if !self.tuples.insert(tuple.clone()) {
            return false;
        }

        for index in &mut self.indexes {
            index.insert(tuple.clone());
        }
        self.record_value_domains(&tuple, true);
        true
    }

    fn remove_indexed(&mut self, tuple: &Tuple) -> bool {
        if !self.tuples.remove(tuple) {
            return false;
        }

        for index in &mut self.indexes {
            index.remove(tuple);
        }
        self.record_value_domains(tuple, false);
        true
    }

    fn record_value_domains(&mut self, tuple: &Tuple, inserted: bool) {
        for (position, value) in tuple.values().iter().enumerate() {
            if value.is_immediate() {
                continue;
            }
            if inserted {
                self.non_immediate_counts[position] += 1;
            } else {
                self.non_immediate_counts[position] =
                    self.non_immediate_counts[position].saturating_sub(1);
            }
        }
    }
}

enum ScanAccess<'a> {
    TupleStore(&'a TupleStore, usize),
    Index(&'a TupleIndex, usize),
}

impl ScanAccess<'_> {
    fn estimate_prefix_count(&self, bindings: &[Option<Value>]) -> Result<usize, KernelError> {
        match self {
            Self::TupleStore(tuples, bound_count) => {
                tuples.estimate_prefix_count(bindings, *bound_count)
            }
            Self::Index(index, bound_count) => index.estimate_prefix_count(bindings, *bound_count),
        }
    }

    fn scan_prefix(&self, bindings: &[Option<Value>]) -> Result<Vec<Tuple>, KernelError> {
        match self {
            Self::TupleStore(tuples, bound_count) => tuples.scan_prefix(bindings, *bound_count),
            Self::Index(index, bound_count) => index.scan_prefix(bindings, *bound_count),
        }
    }

    fn visit_prefix(
        &self,
        bindings: &[Option<Value>],
        visitor: &mut dyn FnMut(&Tuple) -> Result<ScanControl, KernelError>,
    ) -> Result<(), KernelError> {
        match self {
            Self::TupleStore(tuples, bound_count) => {
                tuples.visit_prefix(bindings, *bound_count, visitor)
            }
            Self::Index(index, bound_count) => index.visit_prefix(bindings, *bound_count, visitor),
        }
    }
}

fn is_natural_full_tuple_index(positions: &[u16], arity: u16) -> bool {
    positions.len() == arity as usize && is_natural_full_tuple_positions(positions, arity)
}

fn is_natural_full_tuple_positions(positions: &[u16], arity: u16) -> bool {
    positions.iter().copied().eq(0..arity)
}

fn natural_leading_bound_count(bindings: &[Option<Value>]) -> usize {
    bindings
        .iter()
        .take_while(|binding| binding.is_some())
        .count()
}

fn full_tuple_binding_values(bindings: &[Option<Value>]) -> Option<Vec<Value>> {
    bindings.iter().cloned().collect()
}

fn natural_prefix_covers_all_bindings(bindings: &[Option<Value>], bound_count: usize) -> bool {
    bindings.iter().skip(bound_count).all(Option::is_none)
}

fn validate_metadata(metadata: &RelationMetadata) -> Result<(), KernelError> {
    for index in &metadata.indexes {
        for position in &index.positions {
            if *position >= metadata.arity() {
                return Err(KernelError::InvalidIndex {
                    relation: metadata.id(),
                    position: *position,
                    arity: metadata.arity(),
                });
            }
        }
    }
    if let crate::ConflictPolicy::Functional { key_positions } = metadata.conflict_policy() {
        for position in key_positions {
            if *position >= metadata.arity() {
                return Err(KernelError::InvalidIndex {
                    relation: metadata.id(),
                    position: *position,
                    arity: metadata.arity(),
                });
            }
        }
    }
    Ok(())
}
