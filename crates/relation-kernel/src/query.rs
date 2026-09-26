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

use crate::index::ProjectedTupleIndex;
use crate::relation_algebra::{
    difference_tuple_rows, equality_join_tuple_rows, finish_tuple_rows, project_tuple_rows,
    union_tuple_rows,
};
use crate::tuple::TupleKey;
use crate::{ComputedRow, ExecutionContext, KernelError, RelationId, Tuple};
use mica_var::{RelationValue, Value};
use std::collections::BTreeSet;
use std::sync::Arc;

const PROBE_JOIN_LEFT_ROW_LIMIT: usize = 32;
const SMALL_RIGHT_SEMI_JOIN_FACTOR: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelationSource {
    Unknown,
    Snapshot,
    TransactionOverlay,
    Projected,
    Computed,
    DerivedFull,
    DerivedDelta,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueDomain {
    Immediate,
    Heap,
    Mixed,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationCapabilities {
    pub source: RelationSource,
    pub cardinality: Option<usize>,
    pub exact_indexes: Vec<Vec<u16>>,
    pub value_domains: Vec<ValueDomain>,
    pub supports_streaming: bool,
    pub supports_batch_export: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackedRelation {
    columns: Arc<[Arc<[Value]>]>,
    rows: Arc<[Tuple]>,
    row_count: usize,
}

impl PackedRelation {
    pub fn from_tuples(rows: Vec<Tuple>, arity: usize) -> Option<Self> {
        Self::from_canonical_tuples(finish_tuple_rows(rows), arity)
    }

    pub fn from_canonical_tuples(rows: Vec<Tuple>, arity: usize) -> Option<Self> {
        Self::from_shared_canonical_tuples(rows.into(), arity)
    }

    pub(crate) fn from_shared_canonical_tuples(rows: Arc<[Tuple]>, arity: usize) -> Option<Self> {
        if arity == 0 {
            return None;
        }
        let mut columns = (0..arity)
            .map(|_| Vec::with_capacity(rows.len()))
            .collect::<Vec<_>>();
        for tuple in rows.iter() {
            if tuple.arity() != arity || tuple.values().iter().any(|value| !value.is_immediate()) {
                return None;
            }
            for (column, value) in columns.iter_mut().zip(tuple.values()) {
                column.push(value.clone());
            }
        }
        Some(Self {
            row_count: rows.len(),
            columns: columns
                .into_iter()
                .map(Arc::<[Value]>::from)
                .collect::<Vec<_>>()
                .into(),
            rows,
        })
    }

    pub fn columns(&self) -> &[Arc<[Value]>] {
        &self.columns
    }

    pub fn row_count(&self) -> usize {
        self.row_count
    }

    pub fn rows(&self) -> &[Tuple] {
        &self.rows
    }

    pub(crate) fn shared_rows(&self) -> Arc<[Tuple]> {
        Arc::clone(&self.rows)
    }
}

impl RelationCapabilities {
    pub fn unknown() -> Self {
        Self {
            source: RelationSource::Unknown,
            cardinality: None,
            exact_indexes: Vec::new(),
            value_domains: Vec::new(),
            supports_streaming: true,
            supports_batch_export: false,
        }
    }

    pub fn immediate_only(&self) -> bool {
        !self.value_domains.is_empty()
            && self
                .value_domains
                .iter()
                .all(|domain| *domain == ValueDomain::Immediate)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanControl {
    Continue,
    Stop,
}

pub trait RelationRead {
    fn scan_relation(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError>;

    /// Required inputs when this view exposes a computed relation.
    fn computed_required_bindings(
        &self,
        _relation: RelationId,
    ) -> Result<Option<&[u16]>, KernelError> {
        Ok(None)
    }

    /// Returns candidates associated with each input row, or declines batching.
    /// Implementations must apply all bindings before returning candidates.
    fn scan_relation_batch(
        &self,
        _relation: RelationId,
        _bindings: &[Vec<Option<Value>>],
    ) -> Result<Option<Vec<ComputedRow>>, KernelError> {
        Ok(None)
    }

    fn visit_relation(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
        visitor: &mut dyn FnMut(&Tuple) -> Result<ScanControl, KernelError>,
    ) -> Result<(), KernelError> {
        for tuple in self.scan_relation(relation, bindings)? {
            if visitor(&tuple)? == ScanControl::Stop {
                break;
            }
        }
        Ok(())
    }

    fn estimate_relation_scan(
        &self,
        _relation: RelationId,
        _bindings: &[Option<Value>],
    ) -> Result<Option<usize>, KernelError> {
        Ok(None)
    }

    fn relation_capabilities(
        &self,
        _relation: RelationId,
    ) -> Result<RelationCapabilities, KernelError> {
        Ok(RelationCapabilities::unknown())
    }

    fn export_relation_batch(
        &self,
        _relation: RelationId,
        _bindings: &[Option<Value>],
    ) -> Result<Option<Arc<PackedRelation>>, KernelError> {
        Ok(None)
    }

    fn has_exact_relation_index(
        &self,
        _relation: RelationId,
        _positions: &[u16],
    ) -> Result<bool, KernelError> {
        Ok(false)
    }

    fn join_relation_scans(
        &self,
        _left_relation: RelationId,
        _left_bindings: &[Option<Value>],
        _left_positions: &[u16],
        _right_relation: RelationId,
        _right_bindings: &[Option<Value>],
        _right_positions: &[u16],
    ) -> Result<Option<Vec<Tuple>>, KernelError> {
        Ok(None)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryPlan {
    Scan {
        relation: RelationId,
        bindings: Vec<Option<Value>>,
    },
    Input {
        relation: RelationValue,
    },
    Project {
        input: Box<QueryPlan>,
        positions: Vec<u16>,
    },
    JoinEq {
        left: Box<QueryPlan>,
        right: Box<QueryPlan>,
        left_positions: Vec<u16>,
        right_positions: Vec<u16>,
    },
    SemiJoin {
        left: Box<QueryPlan>,
        right: Box<QueryPlan>,
        left_positions: Vec<u16>,
        right_positions: Vec<u16>,
    },
    AntiJoin {
        left: Box<QueryPlan>,
        right: Box<QueryPlan>,
        left_positions: Vec<u16>,
        right_positions: Vec<u16>,
    },
    Union {
        left: Box<QueryPlan>,
        right: Box<QueryPlan>,
    },
    Difference {
        left: Box<QueryPlan>,
        right: Box<QueryPlan>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedQuery {
    root: PhysicalQueryPlan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PhysicalQueryPlan {
    Scan {
        relation: RelationId,
        bindings: Vec<Option<Value>>,
    },
    Input {
        relation: RelationValue,
        packed: Option<Arc<PackedRelation>>,
    },
    Project {
        input: Box<PhysicalQueryPlan>,
        positions: Vec<u16>,
    },
    JoinEq {
        left: Box<PhysicalQueryPlan>,
        right: Box<PhysicalQueryPlan>,
        left_positions: Vec<u16>,
        right_positions: Vec<u16>,
    },
    SemiJoin {
        left: Box<PhysicalQueryPlan>,
        right: Box<PhysicalQueryPlan>,
        left_positions: Vec<u16>,
        right_positions: Vec<u16>,
    },
    AntiJoin {
        left: Box<PhysicalQueryPlan>,
        right: Box<PhysicalQueryPlan>,
        left_positions: Vec<u16>,
        right_positions: Vec<u16>,
    },
    Union {
        left: Box<PhysicalQueryPlan>,
        right: Box<PhysicalQueryPlan>,
    },
    Difference {
        left: Box<PhysicalQueryPlan>,
        right: Box<PhysicalQueryPlan>,
    },
}

impl QueryPlan {
    pub fn scan(relation: RelationId, bindings: impl IntoIterator<Item = Option<Value>>) -> Self {
        Self::Scan {
            relation,
            bindings: bindings.into_iter().collect(),
        }
    }

    /// Uses an immutable relation value as a query input.
    ///
    /// Query operators address columns by their canonical positions in the
    /// relation value's heading.
    pub fn input(relation: RelationValue) -> Self {
        Self::Input { relation }
    }

    pub fn project(self, positions: impl IntoIterator<Item = u16>) -> Self {
        Self::Project {
            input: Box::new(self),
            positions: positions.into_iter().collect(),
        }
    }

    pub fn join_eq(
        left: Self,
        right: Self,
        left_positions: impl IntoIterator<Item = u16>,
        right_positions: impl IntoIterator<Item = u16>,
    ) -> Self {
        Self::JoinEq {
            left: Box::new(left),
            right: Box::new(right),
            left_positions: left_positions.into_iter().collect(),
            right_positions: right_positions.into_iter().collect(),
        }
    }

    pub fn semi_join(
        left: Self,
        right: Self,
        left_positions: impl IntoIterator<Item = u16>,
        right_positions: impl IntoIterator<Item = u16>,
    ) -> Self {
        Self::SemiJoin {
            left: Box::new(left),
            right: Box::new(right),
            left_positions: left_positions.into_iter().collect(),
            right_positions: right_positions.into_iter().collect(),
        }
    }

    pub fn anti_join(
        left: Self,
        right: Self,
        left_positions: impl IntoIterator<Item = u16>,
        right_positions: impl IntoIterator<Item = u16>,
    ) -> Self {
        Self::AntiJoin {
            left: Box::new(left),
            right: Box::new(right),
            left_positions: left_positions.into_iter().collect(),
            right_positions: right_positions.into_iter().collect(),
        }
    }

    pub fn union(left: Self, right: Self) -> Self {
        Self::Union {
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    pub fn difference(left: Self, right: Self) -> Self {
        Self::Difference {
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    pub fn prepare(&self) -> PreparedQuery {
        PreparedQuery {
            root: compile_query(self),
        }
    }

    pub fn execute(
        &self,
        reader: &impl RelationRead,
        execution_context: &ExecutionContext,
    ) -> Result<Vec<Tuple>, KernelError> {
        self.prepare().execute(reader, execution_context)
    }
}

impl PreparedQuery {
    pub fn execute(
        &self,
        reader: &impl RelationRead,
        execution_context: &ExecutionContext,
    ) -> Result<Vec<Tuple>, KernelError> {
        if let Some(rows) =
            crate::batch::execute_packed_query(&self.root, reader, execution_context)?
        {
            return Ok(rows);
        }
        execute_physical_query(&self.root, reader)
    }
}

fn compile_query(plan: &QueryPlan) -> PhysicalQueryPlan {
    match plan {
        QueryPlan::Scan { relation, bindings } => PhysicalQueryPlan::Scan {
            relation: *relation,
            bindings: bindings.clone(),
        },
        QueryPlan::Input { relation } => PhysicalQueryPlan::Input {
            packed: pack_relation_input(relation),
            relation: relation.clone(),
        },
        QueryPlan::Project { input, positions } => {
            compile_projection(compile_query(input), positions)
        }
        QueryPlan::JoinEq {
            left,
            right,
            left_positions,
            right_positions,
        } => PhysicalQueryPlan::JoinEq {
            left: Box::new(compile_query(left)),
            right: Box::new(compile_query(right)),
            left_positions: left_positions.clone(),
            right_positions: right_positions.clone(),
        },
        QueryPlan::SemiJoin {
            left,
            right,
            left_positions,
            right_positions,
        } => PhysicalQueryPlan::SemiJoin {
            left: Box::new(compile_query(left)),
            right: Box::new(compile_query(right)),
            left_positions: left_positions.clone(),
            right_positions: right_positions.clone(),
        },
        QueryPlan::AntiJoin {
            left,
            right,
            left_positions,
            right_positions,
        } => PhysicalQueryPlan::AntiJoin {
            left: Box::new(compile_query(left)),
            right: Box::new(compile_query(right)),
            left_positions: left_positions.clone(),
            right_positions: right_positions.clone(),
        },
        QueryPlan::Union { left, right } => PhysicalQueryPlan::Union {
            left: Box::new(compile_query(left)),
            right: Box::new(compile_query(right)),
        },
        QueryPlan::Difference { left, right } => PhysicalQueryPlan::Difference {
            left: Box::new(compile_query(left)),
            right: Box::new(compile_query(right)),
        },
    }
}

fn pack_relation_input(relation: &RelationValue) -> Option<Arc<PackedRelation>> {
    if !matches!(relation.arity(), 1 | 2)
        || relation
            .rows()
            .iter()
            .flat_map(|row| row.values())
            .any(|value| !value.is_immediate())
    {
        return None;
    }
    PackedRelation::from_canonical_tuples(relation.rows().to_vec(), relation.arity()).map(Arc::new)
}

fn compile_projection(input: PhysicalQueryPlan, positions: &[u16]) -> PhysicalQueryPlan {
    let PhysicalQueryPlan::Project {
        input,
        positions: input_positions,
    } = input
    else {
        return PhysicalQueryPlan::Project {
            input: Box::new(input),
            positions: positions.to_vec(),
        };
    };
    let positions = positions
        .iter()
        .map(|position| input_positions[*position as usize])
        .collect();
    PhysicalQueryPlan::Project { input, positions }
}

fn execute_physical_query(
    plan: &PhysicalQueryPlan,
    reader: &impl RelationRead,
) -> Result<Vec<Tuple>, KernelError> {
    match plan {
        PhysicalQueryPlan::Scan { relation, bindings } => reader.scan_relation(*relation, bindings),
        PhysicalQueryPlan::Input { relation, .. } => Ok(relation.rows().to_vec()),
        PhysicalQueryPlan::Project { input, positions } => {
            let rows = execute_physical_query(input, reader)?;
            Ok(project_tuple_rows(rows, positions))
        }
        PhysicalQueryPlan::JoinEq {
            left,
            right,
            left_positions,
            right_positions,
        } => execute_join_eq(left, right, left_positions, right_positions, reader),
        PhysicalQueryPlan::SemiJoin {
            left,
            right,
            left_positions,
            right_positions,
        } => execute_semi_join(left, right, left_positions, right_positions, reader, true),
        PhysicalQueryPlan::AntiJoin {
            left,
            right,
            left_positions,
            right_positions,
        } => execute_semi_join(left, right, left_positions, right_positions, reader, false),
        PhysicalQueryPlan::Union { left, right } => Ok(union_tuple_rows(
            execute_physical_query(left, reader)?,
            execute_physical_query(right, reader)?,
        )),
        PhysicalQueryPlan::Difference { left, right } => Ok(difference_tuple_rows(
            execute_physical_query(left, reader)?,
            execute_physical_query(right, reader)?,
        )),
    }
}

fn execute_join_eq(
    left: &PhysicalQueryPlan,
    right: &PhysicalQueryPlan,
    left_positions: &[u16],
    right_positions: &[u16],
    reader: &impl RelationRead,
) -> Result<Vec<Tuple>, KernelError> {
    validate_join_positions(left_positions, right_positions);
    if let Some(rows) = direct_relation_join(left, right, left_positions, right_positions, reader)?
    {
        return Ok(rows);
    }

    let left_rows = execute_physical_query(left, reader)?;
    if let Some((relation, bindings)) = scan_parts(right)
        && should_probe_join(left_rows.len(), reader, relation, bindings, right_positions)?
    {
        return indexed_nested_loop_join_rows(
            left_rows,
            relation,
            bindings,
            left_positions,
            right_positions,
            reader,
        );
    }

    let right_rows = execute_physical_query(right, reader)?;
    Ok(equality_join_tuple_rows(
        left_rows,
        right_rows,
        left_positions,
        right_positions,
    ))
}

fn direct_relation_join(
    left: &PhysicalQueryPlan,
    right: &PhysicalQueryPlan,
    left_positions: &[u16],
    right_positions: &[u16],
    reader: &impl RelationRead,
) -> Result<Option<Vec<Tuple>>, KernelError> {
    let (
        PhysicalQueryPlan::Scan {
            relation: left_relation,
            bindings: left_bindings,
        },
        PhysicalQueryPlan::Scan {
            relation: right_relation,
            bindings: right_bindings,
        },
    ) = (left, right)
    else {
        return Ok(None);
    };

    if reader.computed_required_bindings(*left_relation)?.is_some()
        || reader
            .computed_required_bindings(*right_relation)?
            .is_some()
    {
        return Ok(None);
    }
    reader.join_relation_scans(
        *left_relation,
        left_bindings,
        left_positions,
        *right_relation,
        right_bindings,
        right_positions,
    )
}

fn execute_semi_join(
    left: &PhysicalQueryPlan,
    right: &PhysicalQueryPlan,
    left_positions: &[u16],
    right_positions: &[u16],
    reader: &impl RelationRead,
    keep_matches: bool,
) -> Result<Vec<Tuple>, KernelError> {
    validate_join_positions(left_positions, right_positions);
    let left_rows = execute_physical_query(left, reader)?;
    if let Some((relation, bindings)) = scan_parts(right)
        && should_probe_join(left_rows.len(), reader, relation, bindings, right_positions)?
    {
        return indexed_nested_loop_semi_join_rows(
            left_rows,
            relation,
            bindings,
            left_positions,
            right_positions,
            reader,
            keep_matches,
        );
    }

    let right_rows = execute_physical_query(right, reader)?;
    Ok(semi_join(
        left_rows,
        right_rows,
        left_positions,
        right_positions,
        keep_matches,
    ))
}

fn scan_parts(plan: &PhysicalQueryPlan) -> Option<(RelationId, &[Option<Value>])> {
    let PhysicalQueryPlan::Scan { relation, bindings } = plan else {
        return None;
    };
    Some((*relation, bindings))
}

fn should_probe_join(
    left_len: usize,
    reader: &impl RelationRead,
    right_relation: RelationId,
    right_bindings: &[Option<Value>],
    right_positions: &[u16],
) -> Result<bool, KernelError> {
    if reader.computed_required_bindings(right_relation)?.is_some() {
        return Ok(true);
    }
    if left_len <= PROBE_JOIN_LEFT_ROW_LIMIT {
        return Ok(true);
    }
    if reader.has_exact_relation_index(right_relation, right_positions)? {
        return Ok(true);
    }
    let Some(right_estimate) = reader.estimate_relation_scan(right_relation, right_bindings)?
    else {
        return Ok(false);
    };
    Ok(left_len.saturating_mul(4) < right_estimate)
}

fn indexed_nested_loop_join_rows(
    left_rows: Vec<Tuple>,
    right_relation: RelationId,
    right_bindings: &[Option<Value>],
    left_positions: &[u16],
    right_positions: &[u16],
    reader: &impl RelationRead,
) -> Result<Vec<Tuple>, KernelError> {
    let mut out = Vec::new();
    if left_rows.len() > 1 && reader.computed_required_bindings(right_relation)?.is_some() {
        let (inputs, keys): (Vec<_>, Vec<_>) = left_rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                probe_bindings(right_bindings, row, left_positions, right_positions)
                    .map(|key| (index, key))
            })
            .unzip();
        if let Some(rows) = reader.scan_relation_batch(right_relation, &keys)? {
            for row in rows {
                out.push(left_rows[inputs[row.input_row]].concat(&row.tuple));
            }
            return Ok(finish_tuple_rows(out));
        }
    }
    for left_row in left_rows {
        let Some(probe_bindings) =
            probe_bindings(right_bindings, &left_row, left_positions, right_positions)
        else {
            continue;
        };
        for right_row in reader.scan_relation(right_relation, &probe_bindings)? {
            out.push(left_row.concat(&right_row));
        }
    }
    Ok(finish_tuple_rows(out))
}

fn indexed_nested_loop_semi_join_rows(
    left_rows: Vec<Tuple>,
    right_relation: RelationId,
    right_bindings: &[Option<Value>],
    left_positions: &[u16],
    right_positions: &[u16],
    reader: &impl RelationRead,
    keep_matches: bool,
) -> Result<Vec<Tuple>, KernelError> {
    let mut out = Vec::new();
    for left_row in left_rows {
        let matched = if let Some(probe_bindings) =
            probe_bindings(right_bindings, &left_row, left_positions, right_positions)
        {
            relation_has_match(reader, right_relation, &probe_bindings)?
        } else {
            false
        };

        if matched == keep_matches {
            out.push(left_row);
        }
    }
    Ok(out)
}

fn relation_has_match(
    reader: &impl RelationRead,
    relation: RelationId,
    bindings: &[Option<Value>],
) -> Result<bool, KernelError> {
    let mut matched = false;
    reader.visit_relation(relation, bindings, &mut |_| {
        matched = true;
        Ok(ScanControl::Stop)
    })?;
    Ok(matched)
}

fn probe_bindings(
    bindings: &[Option<Value>],
    left_row: &Tuple,
    left_positions: &[u16],
    right_positions: &[u16],
) -> Option<Vec<Option<Value>>> {
    let mut probe_bindings = bindings.to_vec();
    for (left_position, right_position) in left_positions.iter().zip(right_positions.iter()) {
        let value = left_row.values()[*left_position as usize].clone();
        let binding = &mut probe_bindings[*right_position as usize];
        if binding.as_ref().is_some_and(|existing| existing != &value) {
            return None;
        }
        *binding = Some(value);
    }
    Some(probe_bindings)
}

fn semi_join(
    left_rows: Vec<Tuple>,
    right_rows: Vec<Tuple>,
    left_positions: &[u16],
    right_positions: &[u16],
    keep_matches: bool,
) -> Vec<Tuple> {
    if right_rows
        .len()
        .saturating_mul(SMALL_RIGHT_SEMI_JOIN_FACTOR)
        < left_rows.len()
    {
        return semi_join_with_right_key_set(
            left_rows,
            right_rows,
            left_positions,
            right_positions,
            keep_matches,
        );
    }

    let left_index = ProjectedTupleIndex::from_rows(&left_rows, left_positions);
    let right_index = ProjectedTupleIndex::from_rows(&right_rows, right_positions);
    let mut matches = BTreeSet::new();
    left_index.matching_left_rows(&right_index, |tuple| {
        matches.insert(tuple.clone());
    });
    left_rows
        .into_iter()
        .filter(|row| matches.contains(row) == keep_matches)
        .collect()
}

fn semi_join_with_right_key_set(
    left_rows: Vec<Tuple>,
    right_rows: Vec<Tuple>,
    left_positions: &[u16],
    right_positions: &[u16],
    keep_matches: bool,
) -> Vec<Tuple> {
    let right_keys = right_rows
        .iter()
        .map(|row| TupleKey::project(row, right_positions))
        .collect::<BTreeSet<TupleKey>>();
    left_rows
        .into_iter()
        .filter(|row| right_keys.contains(&TupleKey::project(row, left_positions)) == keep_matches)
        .collect()
}

fn validate_join_positions(left_positions: &[u16], right_positions: &[u16]) {
    assert_eq!(
        left_positions.len(),
        right_positions.len(),
        "join position lists must have the same length"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RelationKernel, RelationMetadata};
    use mica_var::{Identity, RelationValue, Symbol, Value};
    use std::cell::Cell;

    fn rel(id: u64) -> RelationId {
        Identity::new(id).unwrap()
    }

    fn int(value: i64) -> Value {
        Value::int(value).unwrap()
    }

    fn kernel_with_edges() -> RelationKernel {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(
                RelationMetadata::new(rel(10), Symbol::intern("Edge"), 2)
                    .with_index([0, 1])
                    .with_index([1, 0]),
            )
            .unwrap();
        kernel
    }

    fn kernel_with_large_immediate_relations() -> RelationKernel {
        let kernel = RelationKernel::new();
        for (relation, name, arity) in [
            (rel(100), "LeftUnary", 1),
            (rel(101), "RightUnary", 1),
            (rel(102), "LeftBinary", 2),
            (rel(103), "RightBinary", 2),
        ] {
            kernel
                .create_relation(RelationMetadata::new(relation, Symbol::intern(name), arity))
                .unwrap();
        }

        let mut tx = kernel.begin();
        for row in 0..384 {
            tx.assert(rel(100), Tuple::from([int(row)])).unwrap();
            tx.assert(rel(102), Tuple::from([int(row), int(row % 31)]))
                .unwrap();
        }
        for row in 192..576 {
            tx.assert(rel(101), Tuple::from([int(row)])).unwrap();
            tx.assert(rel(103), Tuple::from([int(row % 31), int(row)]))
                .unwrap();
        }
        tx.commit().unwrap();
        kernel
    }

    fn assert_packed_matches_tuple(query: QueryPlan, reader: &impl RelationRead) {
        let prepared = query.prepare();
        let packed =
            crate::batch::execute_packed_query(&prepared.root, reader, &ExecutionContext::serial())
                .unwrap()
                .expect("large immediate query should select the packed executor");
        let tuples = execute_physical_query(&prepared.root, reader).unwrap();
        assert_eq!(packed, tuples);
    }

    struct ProbeOnlyReader;

    impl RelationRead for ProbeOnlyReader {
        fn scan_relation(
            &self,
            relation: RelationId,
            bindings: &[Option<Value>],
        ) -> Result<Vec<Tuple>, KernelError> {
            if relation == rel(20) {
                assert_eq!(bindings, &[None, None]);
                return Ok(vec![
                    Tuple::from([int(1), int(10)]),
                    Tuple::from([int(2), int(20)]),
                ]);
            }

            assert_eq!(relation, rel(21));
            match bindings {
                [Some(value), None] if value == &int(10) => {
                    Ok(vec![Tuple::from([int(10), int(100)])])
                }
                [Some(value), None] if value == &int(20) => {
                    Ok(vec![Tuple::from([int(20), int(200)])])
                }
                other => panic!("right relation should be probed with a bound join key: {other:?}"),
            }
        }
    }

    struct IndexedProbeReader;

    impl RelationRead for IndexedProbeReader {
        fn scan_relation(
            &self,
            relation: RelationId,
            bindings: &[Option<Value>],
        ) -> Result<Vec<Tuple>, KernelError> {
            if relation == rel(22) {
                assert_eq!(bindings, &[None, None]);
                return Ok((0..40)
                    .map(|row| Tuple::from([int(row), int(row + 100)]))
                    .collect());
            }

            assert_eq!(relation, rel(23));
            match bindings {
                [Some(value), None] => Ok(vec![Tuple::from([value.clone(), int(1)])]),
                other => panic!("indexed probe should bind the right join key: {other:?}"),
            }
        }

        fn estimate_relation_scan(
            &self,
            relation: RelationId,
            bindings: &[Option<Value>],
        ) -> Result<Option<usize>, KernelError> {
            assert_eq!(relation, rel(23));
            assert_eq!(bindings, &[None, None]);
            Ok(Some(1))
        }

        fn has_exact_relation_index(
            &self,
            relation: RelationId,
            positions: &[u16],
        ) -> Result<bool, KernelError> {
            assert_eq!(relation, rel(23));
            Ok(positions == [0])
        }
    }

    struct DirectJoinReader {
        called: Cell<bool>,
    }

    impl RelationRead for DirectJoinReader {
        fn scan_relation(
            &self,
            _relation: RelationId,
            _bindings: &[Option<Value>],
        ) -> Result<Vec<Tuple>, KernelError> {
            panic!("direct relation join should avoid ordinary scans")
        }

        fn join_relation_scans(
            &self,
            left_relation: RelationId,
            left_bindings: &[Option<Value>],
            left_positions: &[u16],
            right_relation: RelationId,
            right_bindings: &[Option<Value>],
            right_positions: &[u16],
        ) -> Result<Option<Vec<Tuple>>, KernelError> {
            assert_eq!(left_relation, rel(30));
            assert_eq!(left_bindings, &[None]);
            assert_eq!(left_positions, &[0]);
            assert_eq!(right_relation, rel(31));
            assert_eq!(right_bindings, &[None]);
            assert_eq!(right_positions, &[0]);
            self.called.set(true);
            Ok(Some(vec![Tuple::from([int(1), int(1)])]))
        }
    }

    struct SnapshotJoinOnlyReader<'a> {
        snapshot: &'a crate::Snapshot,
        called: Cell<bool>,
    }

    impl RelationRead for SnapshotJoinOnlyReader<'_> {
        fn scan_relation(
            &self,
            _relation: RelationId,
            _bindings: &[Option<Value>],
        ) -> Result<Vec<Tuple>, KernelError> {
            panic!("indexed relation join should avoid materialized scans")
        }

        fn join_relation_scans(
            &self,
            left_relation: RelationId,
            left_bindings: &[Option<Value>],
            left_positions: &[u16],
            right_relation: RelationId,
            right_bindings: &[Option<Value>],
            right_positions: &[u16],
        ) -> Result<Option<Vec<Tuple>>, KernelError> {
            self.called.set(true);
            self.snapshot.join_extensional_relation_scans(
                left_relation,
                left_bindings,
                left_positions,
                right_relation,
                right_bindings,
                right_positions,
            )
        }
    }

    #[test]
    fn query_plan_executes_projected_join_against_transaction_overlay() {
        let kernel = kernel_with_edges();
        let mut tx = kernel.begin();
        tx.assert(rel(10), Tuple::from([int(1), int(2)])).unwrap();
        tx.assert(rel(10), Tuple::from([int(2), int(3)])).unwrap();

        let path2 = QueryPlan::join_eq(
            QueryPlan::scan(rel(10), [None, None]),
            QueryPlan::scan(rel(10), [None, None]),
            [1],
            [0],
        )
        .project([0, 3]);

        assert_eq!(
            path2.execute(&tx, &ExecutionContext::serial()).unwrap(),
            vec![Tuple::from([int(1), int(3)])]
        );
    }

    #[test]
    fn prepared_query_combines_nested_projections() {
        let prepared = QueryPlan::scan(rel(10), [None, None, None])
            .project([2, 0])
            .project([1])
            .prepare();

        assert_eq!(
            prepared.root,
            PhysicalQueryPlan::Project {
                input: Box::new(PhysicalQueryPlan::Scan {
                    relation: rel(10),
                    bindings: vec![None, None, None],
                }),
                positions: vec![0],
            }
        );
    }

    #[test]
    fn prepared_and_logical_queries_produce_identical_results() {
        let kernel = kernel_with_edges();
        let mut tx = kernel.begin();
        tx.assert(rel(10), Tuple::from([int(1), int(2)])).unwrap();
        tx.assert(rel(10), Tuple::from([int(2), int(3)])).unwrap();
        let query = QueryPlan::join_eq(
            QueryPlan::scan(rel(10), [None, None]),
            QueryPlan::scan(rel(10), [None, None]),
            [1],
            [0],
        )
        .project([0, 3]);
        let prepared = query.prepare();

        assert_eq!(
            query.execute(&tx, &ExecutionContext::serial()).unwrap(),
            prepared.execute(&tx, &ExecutionContext::serial()).unwrap()
        );
    }

    #[test]
    fn query_plan_uses_relation_values_as_canonical_positional_inputs() {
        let a = Symbol::intern("query-input-a");
        let z = Symbol::intern("query-input-z");
        let relation = RelationValue::new(
            [z, a],
            [
                Tuple::from([int(20), int(2)]),
                Tuple::from([int(10), int(1)]),
            ],
        )
        .unwrap();
        assert_eq!(relation.heading(), [a, z]);

        let kernel = RelationKernel::new();
        let rows = QueryPlan::input(relation)
            .project([1])
            .execute(kernel.snapshot().as_ref(), &ExecutionContext::serial())
            .unwrap();

        assert_eq!(rows, vec![Tuple::from([int(10)]), Tuple::from([int(20)])]);
    }

    #[test]
    fn relation_value_inputs_join_indexed_relation_scans() {
        let kernel = kernel_with_edges();
        let mut tx = kernel.begin();
        tx.assert(rel(10), Tuple::from([int(1), int(3)])).unwrap();
        tx.assert(rel(10), Tuple::from([int(2), int(4)])).unwrap();
        tx.commit().unwrap();

        let input = RelationValue::new(
            [Symbol::intern("input-item"), Symbol::intern("input-key")],
            [Tuple::from([int(8), int(2)]), Tuple::from([int(9), int(1)])],
        )
        .unwrap();
        let query = QueryPlan::join_eq(
            QueryPlan::input(input),
            QueryPlan::scan(rel(10), [None, None]),
            [1],
            [0],
        )
        .project([0, 3]);

        assert_eq!(
            query
                .execute(kernel.snapshot().as_ref(), &ExecutionContext::serial())
                .unwrap(),
            vec![Tuple::from([int(8), int(4)]), Tuple::from([int(9), int(3)])]
        );
    }

    #[test]
    fn packed_executor_accepts_large_immediate_relation_value_inputs() {
        let relation = RelationValue::new(
            [Symbol::intern("packed-input-value")],
            (0..384).map(|row| Tuple::from([int(row)])),
        )
        .unwrap();
        let prepared = QueryPlan::input(relation).prepare();
        let kernel = RelationKernel::new();
        let snapshot = kernel.snapshot();

        let packed = crate::batch::execute_packed_query(
            &prepared.root,
            snapshot.as_ref(),
            &ExecutionContext::serial(),
        )
        .unwrap()
        .expect("large immediate relation input should select the packed executor");
        let tuples = execute_physical_query(&prepared.root, snapshot.as_ref()).unwrap();

        assert_eq!(packed, tuples);
        assert_eq!(packed.len(), 384);
    }

    #[test]
    fn packed_executor_combines_relation_values_with_stored_relations() {
        let kernel = kernel_with_large_immediate_relations();
        let snapshot = kernel.snapshot();
        let input = RelationValue::new(
            [Symbol::intern("mixed-packed-input-value")],
            (380..390).map(|row| Tuple::from([int(row)])),
        )
        .unwrap();

        assert_packed_matches_tuple(
            QueryPlan::union(QueryPlan::scan(rel(100), [None]), QueryPlan::input(input)),
            snapshot.as_ref(),
        );
    }

    #[test]
    fn relation_value_inputs_retain_tuple_fallbacks() {
        let heap = RelationValue::new(
            [Symbol::intern("heap-input-value")],
            (0..300).map(|row| Tuple::from([Value::string(format!("value-{row}"))])),
        )
        .unwrap();
        let heap = QueryPlan::input(heap).prepare();
        let unit = RelationValue::new([], [Tuple::new([])]).unwrap();
        let kernel = RelationKernel::new();
        let snapshot = kernel.snapshot();

        assert!(
            crate::batch::execute_packed_query(
                &heap.root,
                snapshot.as_ref(),
                &ExecutionContext::serial(),
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(
            heap.execute(snapshot.as_ref(), &ExecutionContext::serial())
                .unwrap()
                .len(),
            300
        );
        assert_eq!(
            QueryPlan::input(unit)
                .execute(snapshot.as_ref(), &ExecutionContext::serial())
                .unwrap(),
            vec![Tuple::new([])]
        );
    }

    #[test]
    fn packed_executor_matches_tuple_executor_for_supported_operators() {
        let kernel = kernel_with_large_immediate_relations();
        let snapshot = kernel.snapshot();

        let left_unary = || QueryPlan::scan(rel(100), [None]);
        let right_unary = || QueryPlan::scan(rel(101), [None]);
        assert_packed_matches_tuple(
            QueryPlan::join_eq(left_unary(), right_unary(), [0], [0]).project([0]),
            snapshot.as_ref(),
        );
        assert_packed_matches_tuple(
            QueryPlan::semi_join(left_unary(), right_unary(), [0], [0]),
            snapshot.as_ref(),
        );
        assert_packed_matches_tuple(
            QueryPlan::anti_join(left_unary(), right_unary(), [0], [0]),
            snapshot.as_ref(),
        );
        assert_packed_matches_tuple(
            QueryPlan::union(left_unary(), right_unary()),
            snapshot.as_ref(),
        );
        assert_packed_matches_tuple(
            QueryPlan::difference(left_unary(), right_unary()),
            snapshot.as_ref(),
        );
        assert_packed_matches_tuple(
            QueryPlan::join_eq(
                QueryPlan::scan(rel(102), [None, None]),
                QueryPlan::scan(rel(103), [None, None]),
                [1],
                [0],
            )
            .project([0, 3]),
            snapshot.as_ref(),
        );
    }

    #[test]
    fn packed_executor_declines_heap_values_and_transaction_overlays() {
        let heap_kernel = RelationKernel::new();
        heap_kernel
            .create_relation(RelationMetadata::new(
                rel(110),
                Symbol::intern("HeapValues"),
                1,
            ))
            .unwrap();
        let mut seed = heap_kernel.begin();
        for row in 0..300 {
            seed.assert(
                rel(110),
                Tuple::from([Value::string(format!("value-{row}"))]),
            )
            .unwrap();
        }
        seed.commit().unwrap();
        let heap_query = QueryPlan::scan(rel(110), [None]).prepare();
        let heap_snapshot = heap_kernel.snapshot();
        assert!(
            crate::batch::execute_packed_query(
                &heap_query.root,
                heap_snapshot.as_ref(),
                &ExecutionContext::serial(),
            )
            .unwrap()
            .is_none()
        );

        let kernel = kernel_with_large_immediate_relations();
        let mut overlay = kernel.begin();
        overlay.assert(rel(100), Tuple::from([int(1_000)])).unwrap();
        let overlay_query = QueryPlan::scan(rel(100), [None]).prepare();
        assert!(
            crate::batch::execute_packed_query(
                &overlay_query.root,
                &overlay,
                &ExecutionContext::serial(),
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(
            overlay_query
                .execute(&overlay, &ExecutionContext::serial())
                .unwrap(),
            execute_physical_query(&overlay_query.root, &overlay).unwrap()
        );

        let snapshot = kernel.snapshot();
        let bound_query = QueryPlan::scan(rel(100), [Some(int(1))]).prepare();
        assert!(
            crate::batch::execute_packed_query(
                &bound_query.root,
                snapshot.as_ref(),
                &ExecutionContext::serial(),
            )
            .unwrap()
            .is_none(),
            "selective bound scans should retain the tuple/index path",
        );
    }

    #[test]
    fn packed_exports_are_reused_within_a_snapshot_and_invalidated_on_commit() {
        let kernel = kernel_with_large_immediate_relations();
        let old_snapshot = kernel.snapshot();
        let first = old_snapshot
            .export_relation_batch(rel(100), &[None])
            .unwrap()
            .unwrap();
        let second = old_snapshot
            .export_relation_batch(rel(100), &[None])
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));

        let mut tx = kernel.begin();
        tx.assert(rel(100), Tuple::from([int(2_000)])).unwrap();
        tx.commit().unwrap();
        let new_snapshot = kernel.snapshot();
        let new_batch = new_snapshot
            .export_relation_batch(rel(100), &[None])
            .unwrap()
            .unwrap();

        assert!(!Arc::ptr_eq(&first, &new_batch));
        assert_eq!(first.row_count(), 384);
        assert_eq!(new_batch.row_count(), 385);
        assert_eq!(
            old_snapshot.scan_relation(rel(100), &[None]).unwrap().len(),
            384
        );
    }

    #[test]
    fn query_plan_uses_indexed_probe_when_right_side_is_scan() {
        let path = QueryPlan::join_eq(
            QueryPlan::scan(rel(20), [None, None]),
            QueryPlan::scan(rel(21), [None, None]),
            [1],
            [0],
        )
        .project([0, 3]);

        assert_eq!(
            path.execute(&ProbeOnlyReader, &ExecutionContext::serial())
                .unwrap(),
            vec![
                Tuple::from([int(1), int(100)]),
                Tuple::from([int(2), int(200)])
            ]
        );
    }

    #[test]
    fn query_plan_uses_indexed_probe_when_right_side_has_exact_join_index() {
        let path = QueryPlan::join_eq(
            QueryPlan::scan(rel(22), [None, None]),
            QueryPlan::scan(rel(23), [None, None]),
            [1],
            [0],
        );

        assert_eq!(
            path.execute(&IndexedProbeReader, &ExecutionContext::serial())
                .unwrap()
                .len(),
            40
        );
    }

    #[test]
    fn query_plan_uses_relation_join_hook_for_scan_equality_join() {
        let reader = DirectJoinReader {
            called: Cell::new(false),
        };
        let path = QueryPlan::join_eq(
            QueryPlan::scan(rel(30), [None]),
            QueryPlan::scan(rel(31), [None]),
            [0],
            [0],
        );

        assert_eq!(
            path.execute(&reader, &ExecutionContext::serial()).unwrap(),
            vec![Tuple::from([int(1), int(1)])]
        );
        assert!(reader.called.get());
    }

    #[test]
    fn query_plan_uses_snapshot_index_join_for_scan_equality_join() {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(
                RelationMetadata::new(rel(40), Symbol::intern("Left"), 2).with_index([1]),
            )
            .unwrap();
        kernel
            .create_relation(
                RelationMetadata::new(rel(41), Symbol::intern("Right"), 2).with_index([0]),
            )
            .unwrap();
        let mut tx = kernel.begin();
        tx.assert(rel(40), Tuple::from([int(1), int(10)])).unwrap();
        tx.assert(rel(40), Tuple::from([int(2), int(20)])).unwrap();
        tx.assert(rel(41), Tuple::from([int(10), int(100)]))
            .unwrap();
        tx.assert(rel(41), Tuple::from([int(30), int(300)]))
            .unwrap();
        let snapshot = tx.commit().unwrap().into_snapshot();

        let reader = SnapshotJoinOnlyReader {
            snapshot: &snapshot,
            called: Cell::new(false),
        };
        let path = QueryPlan::join_eq(
            QueryPlan::scan(rel(40), [None, None]),
            QueryPlan::scan(rel(41), [None, None]),
            [1],
            [0],
        )
        .project([0, 3]);

        assert_eq!(
            path.execute(&reader, &ExecutionContext::serial()).unwrap(),
            vec![Tuple::from([int(1), int(100)])]
        );
        assert!(reader.called.get());
    }

    #[test]
    fn query_plan_uses_snapshot_natural_tuple_store_join() {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(rel(42), Symbol::intern("Active"), 1))
            .unwrap();
        kernel
            .create_relation(RelationMetadata::new(rel(43), Symbol::intern("Visible"), 1))
            .unwrap();
        let mut tx = kernel.begin();
        tx.assert(rel(42), Tuple::from([int(1)])).unwrap();
        tx.assert(rel(42), Tuple::from([int(2)])).unwrap();
        tx.assert(rel(43), Tuple::from([int(2)])).unwrap();
        tx.assert(rel(43), Tuple::from([int(3)])).unwrap();
        let snapshot = tx.commit().unwrap().into_snapshot();

        let reader = SnapshotJoinOnlyReader {
            snapshot: &snapshot,
            called: Cell::new(false),
        };
        let path = QueryPlan::join_eq(
            QueryPlan::scan(rel(42), [None]),
            QueryPlan::scan(rel(43), [None]),
            [0],
            [0],
        )
        .project([0]);

        assert_eq!(
            path.execute(&reader, &ExecutionContext::serial()).unwrap(),
            vec![Tuple::from([int(2)])]
        );
        assert!(reader.called.get());
    }

    #[test]
    fn query_plan_executes_semi_and_anti_join() {
        let kernel = kernel_with_edges();
        let mut tx = kernel.begin();
        tx.assert(rel(10), Tuple::from([int(1), int(2)])).unwrap();
        tx.assert(rel(10), Tuple::from([int(2), int(3)])).unwrap();
        tx.assert(rel(10), Tuple::from([int(4), int(5)])).unwrap();
        tx.commit().unwrap();

        let left = QueryPlan::scan(rel(10), [None, None]);
        let right = QueryPlan::scan(rel(10), [None, None]);
        let has_outgoing_from_target = QueryPlan::semi_join(left.clone(), right.clone(), [1], [0]);
        let terminal_edges = QueryPlan::anti_join(left, right, [1], [0]);

        assert_eq!(
            has_outgoing_from_target
                .execute(&*kernel.snapshot(), &ExecutionContext::serial())
                .unwrap(),
            vec![Tuple::from([int(1), int(2)])]
        );
        assert_eq!(
            terminal_edges
                .execute(&*kernel.snapshot(), &ExecutionContext::serial())
                .unwrap(),
            vec![Tuple::from([int(2), int(3)]), Tuple::from([int(4), int(5)])]
        );
    }
}
