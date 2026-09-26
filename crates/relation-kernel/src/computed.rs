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

use crate::query::RelationRead;
use crate::{KernelError, RelationId, RelationMetadata, RuleDefinition, Tuple, Version};
use mica_var::{Symbol, Value};
use std::any::Any;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::sync::Arc;

type PreparedEntry = (RelationId, Value, Arc<dyn Any + Send + Sync>);

/// Ephemeral read grants compiled by the host at a task boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadAuthority {
    All,
    Relations(Arc<BTreeSet<RelationId>>),
}

impl ReadAuthority {
    pub fn empty() -> Self {
        Self::Relations(Arc::default())
    }

    pub fn allows(&self, relation: RelationId) -> bool {
        match self {
            Self::All => true,
            Self::Relations(relations) => relations.contains(&relation),
        }
    }

    pub fn grant(&mut self, relation: RelationId) {
        if let Self::Relations(relations) = self {
            Arc::make_mut(relations).insert(relation);
        }
    }

    pub(crate) fn same_grants(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::All, Self::All) => true,
            (Self::Relations(left), Self::Relations(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ComputedBufferView {
    pub id: RelationId,
    pub text: crate::buffer::Text,
    pub revision: u64,
}

/// Prepared provider data owned by one transaction read view.
/// Every local write clears it. Entries never cross a transaction boundary.
#[derive(Default)]
pub struct ComputedPreparationCache {
    entries: RefCell<Vec<PreparedEntry>>,
}

impl ComputedPreparationCache {
    pub fn get_or_prepare<T: Any + Send + Sync>(
        &self,
        relation: RelationId,
        key: &Value,
        prepare: impl FnOnce() -> Result<T, KernelError>,
    ) -> Result<Arc<T>, KernelError> {
        if let Some(prepared) =
            self.entries
                .borrow()
                .iter()
                .find_map(|(stored_relation, stored_key, value)| {
                    (*stored_relation == relation && stored_key == key)
                        .then(|| Arc::clone(value).downcast::<T>().ok())
                        .flatten()
                })
        {
            return Ok(prepared);
        }
        // Preparation can itself query computed relations. Do not hold a borrow across it.
        let prepared = Arc::new(prepare()?);
        let mut entries = self.entries.borrow_mut();
        // Bound the number of retained indexes. A full cache continues serving admitted keys.
        if entries.len() < 16 {
            entries.push((relation, key.clone(), prepared.clone()));
        }
        Ok(prepared)
    }

    pub(crate) fn clear(&mut self) {
        self.entries.get_mut().clear();
    }
}

pub trait ComputedRelationRead: RelationRead {
    /// Trusted kernel readers have unrestricted access unless the host supplies grants.
    fn require_read(&self, _relation: RelationId) -> Result<(), KernelError> {
        Ok(())
    }

    fn buffer_view(&self, _name: Symbol) -> Result<Option<ComputedBufferView>, KernelError> {
        Ok(None)
    }

    fn preparation_cache(&self) -> Option<&ComputedPreparationCache> {
        None
    }

    fn version(&self) -> Version;

    fn relation_metadata_vec(&self) -> Vec<RelationMetadata>;

    fn relation_id(&self, name: Symbol, arity: u16) -> Option<RelationId> {
        self.relation_metadata_vec()
            .into_iter()
            .find(|metadata| metadata.name() == name && metadata.arity() == arity)
            .map(|metadata| metadata.id())
    }

    fn rules_vec(&self) -> Vec<RuleDefinition>;

    /// Physical index representation in the committed snapshot, when stored by the kernel.
    fn index_storage_kind(&self, relation: RelationId, ordinal: usize) -> Option<Symbol>;

    fn extensional_facts(&self) -> Result<Vec<(RelationId, Tuple)>, KernelError>;
}

/// One candidate produced for a row of batched scan bindings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComputedRow {
    pub input_row: usize,
    pub tuple: Tuple,
}

pub trait ComputedRelation: Send + Sync {
    fn name(&self) -> &'static str;

    /// Providers that inspect read grants must keep derived results task-local.
    fn uses_read_authority(&self) -> bool {
        false
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool;

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        &[]
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError>;

    /// Produces candidates for all input rows using the same read view.
    /// The registry validates row indexes, arity, and every input/output binding.
    fn scan_batch(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Vec<Option<Value>>],
    ) -> Result<Vec<ComputedRow>, KernelError> {
        let mut rows = Vec::new();
        for (input_row, bindings) in bindings.iter().enumerate() {
            rows.extend(
                self.scan(reader, metadata, bindings)?
                    .into_iter()
                    .map(|tuple| ComputedRow { input_row, tuple }),
            );
        }
        Ok(rows)
    }

    fn estimate(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<usize, KernelError> {
        Ok(self.scan(reader, metadata, bindings)?.len())
    }
}

#[derive(Clone, Default)]
pub struct ComputedRelationRegistry {
    relations: Vec<Arc<dyn ComputedRelation>>,
    bindings: HashMap<RelationId, Option<usize>>,
    uses_read_authority: bool,
}

impl ComputedRelationRegistry {
    pub fn new(relations: impl IntoIterator<Item = Arc<dyn ComputedRelation>>) -> Self {
        Self {
            relations: relations.into_iter().collect(),
            bindings: HashMap::new(),
            uses_read_authority: false,
        }
    }

    pub(crate) fn bind_relations<'a>(
        mut self,
        metadata: impl IntoIterator<Item = &'a RelationMetadata>,
    ) -> Self {
        for metadata in metadata {
            self.bind_relation(metadata);
        }
        self
    }

    pub(crate) fn with_relation(&self, metadata: &RelationMetadata) -> Self {
        let mut next = self.clone();
        next.bind_relation(metadata);
        next
    }

    pub fn is_computed_relation(&self, metadata: &RelationMetadata) -> bool {
        self.find(metadata).is_some()
    }

    pub(crate) fn uses_read_authority(&self) -> bool {
        self.uses_read_authority
    }

    pub fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Option<Result<Vec<Tuple>, KernelError>> {
        let relation = self.find(metadata)?;
        Some(scan_checked(relation.as_ref(), reader, metadata, bindings))
    }

    pub fn required_bound_positions(&self, metadata: &RelationMetadata) -> Option<&[u16]> {
        self.find(metadata)
            .map(|relation| relation.required_bound_positions(metadata))
    }

    pub fn scan_batch(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Vec<Option<Value>>],
    ) -> Option<Result<Vec<ComputedRow>, KernelError>> {
        let relation = self.find(metadata)?;
        Some((|| {
            for bindings in bindings {
                validate_bindings(relation.as_ref(), metadata, bindings)?;
            }
            if bindings.is_empty() {
                return Ok(Vec::new());
            }
            let mut rows = relation.scan_batch(reader, metadata, bindings)?;
            for row in &rows {
                if row.input_row >= bindings.len() {
                    return Err(KernelError::InvalidComputedRelation {
                        relation: metadata.id(),
                        message: "batch result input row is out of bounds".to_owned(),
                    });
                }
                validate_tuple(metadata, &row.tuple)?;
            }
            rows.retain(|row| row.tuple.matches_bindings(&bindings[row.input_row]));
            Ok(rows)
        })())
    }

    pub fn estimate(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Option<Result<usize, KernelError>> {
        let relation = self.find(metadata)?;
        Some(estimate_checked(
            relation.as_ref(),
            reader,
            metadata,
            bindings,
        ))
    }

    fn find(&self, metadata: &RelationMetadata) -> Option<&Arc<dyn ComputedRelation>> {
        match self.bindings.get(&metadata.id()) {
            Some(Some(index)) => self.relations.get(*index),
            Some(None) => None,
            None => self
                .relations
                .iter()
                .find(|relation| relation.matches(metadata)),
        }
    }

    fn bind_relation(&mut self, metadata: &RelationMetadata) {
        let index = self
            .relations
            .iter()
            .position(|relation| relation.matches(metadata));
        self.bindings.insert(metadata.id(), index);
        self.uses_read_authority |=
            index.is_some_and(|index| self.relations[index].uses_read_authority());
    }
}

impl fmt::Debug for ComputedRelationRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = self
            .relations
            .iter()
            .map(|relation| relation.name())
            .collect::<Vec<_>>();
        f.debug_struct("ComputedRelationRegistry")
            .field("relations", &names)
            .finish()
    }
}

fn scan_checked(
    relation: &dyn ComputedRelation,
    reader: &dyn ComputedRelationRead,
    metadata: &RelationMetadata,
    bindings: &[Option<Value>],
) -> Result<Vec<Tuple>, KernelError> {
    validate_bindings(relation, metadata, bindings)?;
    let mut rows = relation.scan(reader, metadata, bindings)?;
    for row in &rows {
        validate_tuple(metadata, row)?;
    }
    rows.retain(|row| row.matches_bindings(bindings));
    Ok(rows)
}

fn validate_tuple(metadata: &RelationMetadata, tuple: &Tuple) -> Result<(), KernelError> {
    if tuple.arity() != metadata.arity() as usize {
        return Err(KernelError::ArityMismatch {
            relation: metadata.id(),
            expected: metadata.arity(),
            actual: tuple.arity(),
        });
    }
    Ok(())
}

fn estimate_checked(
    relation: &dyn ComputedRelation,
    reader: &dyn ComputedRelationRead,
    metadata: &RelationMetadata,
    bindings: &[Option<Value>],
) -> Result<usize, KernelError> {
    validate_bindings(relation, metadata, bindings)?;
    relation.estimate(reader, metadata, bindings)
}

fn validate_bindings(
    relation: &dyn ComputedRelation,
    metadata: &RelationMetadata,
    bindings: &[Option<Value>],
) -> Result<(), KernelError> {
    if bindings.len() != metadata.arity() as usize {
        return Err(KernelError::ArityMismatch {
            relation: metadata.id(),
            expected: metadata.arity(),
            actual: bindings.len(),
        });
    }

    let missing = relation
        .required_bound_positions(metadata)
        .iter()
        .copied()
        .filter(|position| bindings.get(*position as usize).is_none_or(Option::is_none))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }

    Err(KernelError::MissingRequiredBindings {
        relation: metadata.id(),
        positions: missing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mica_var::Identity;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingRelation {
        name: Symbol,
        matches: Arc<AtomicUsize>,
    }

    impl ComputedRelation for CountingRelation {
        fn name(&self) -> &'static str {
            "counting"
        }

        fn matches(&self, metadata: &RelationMetadata) -> bool {
            self.matches.fetch_add(1, Ordering::Relaxed);
            metadata.name() == self.name
        }

        fn scan(
            &self,
            _reader: &dyn ComputedRelationRead,
            _metadata: &RelationMetadata,
            _bindings: &[Option<Value>],
        ) -> Result<Vec<Tuple>, KernelError> {
            Ok(Vec::new())
        }
    }

    fn relation(id: u64, name: &str) -> RelationMetadata {
        RelationMetadata::new(
            Identity::new(id).expect("test relation identities must be valid"),
            Symbol::intern(name),
            1,
        )
    }

    #[test]
    fn bound_relations_do_not_repeat_provider_matching() {
        let matches = Arc::new(AtomicUsize::new(0));
        let provider: Arc<dyn ComputedRelation> = Arc::new(CountingRelation {
            name: Symbol::intern("Computed"),
            matches: matches.clone(),
        });
        let computed = relation(1, "Computed");
        let ordinary = relation(2, "Ordinary");

        let registry =
            ComputedRelationRegistry::new([provider]).bind_relations([&computed, &ordinary]);
        assert_eq!(matches.load(Ordering::Relaxed), 2);

        for _ in 0..4 {
            assert!(registry.is_computed_relation(&computed));
            assert!(!registry.is_computed_relation(&ordinary));
        }
        assert_eq!(matches.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn newly_bound_relation_uses_cached_provider_match() {
        let matches = Arc::new(AtomicUsize::new(0));
        let provider: Arc<dyn ComputedRelation> = Arc::new(CountingRelation {
            name: Symbol::intern("Computed"),
            matches: matches.clone(),
        });
        let metadata = relation(3, "Computed");
        let registry = ComputedRelationRegistry::new([provider]).with_relation(&metadata);
        assert_eq!(matches.load(Ordering::Relaxed), 1);

        assert!(registry.is_computed_relation(&metadata));
        assert_eq!(matches.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn unbound_registry_matches_relations_dynamically() {
        let matches = Arc::new(AtomicUsize::new(0));
        let provider: Arc<dyn ComputedRelation> = Arc::new(CountingRelation {
            name: Symbol::intern("Computed"),
            matches: matches.clone(),
        });
        let metadata = relation(4, "Computed");
        let registry = ComputedRelationRegistry::new([provider]);

        assert!(registry.is_computed_relation(&metadata));
        assert!(registry.is_computed_relation(&metadata));
        assert_eq!(matches.load(Ordering::Relaxed), 2);
    }
    struct LookupRelation {
        batches: Arc<AtomicUsize>,
    }

    impl ComputedRelation for LookupRelation {
        fn name(&self) -> &'static str {
            "lookup-test"
        }
        fn matches(&self, metadata: &RelationMetadata) -> bool {
            metadata.id() == id(11)
        }
        fn required_bound_positions(&self, _: &RelationMetadata) -> &[u16] {
            &[0]
        }
        fn scan(
            &self,
            reader: &dyn ComputedRelationRead,
            _: &RelationMetadata,
            _: &[Option<Value>],
        ) -> Result<Vec<Tuple>, KernelError> {
            // Deliberately returns candidates; the registry applies the bindings.
            reader.scan_relation(id(12), &[None, None])
        }
        fn estimate(
            &self,
            _: &dyn ComputedRelationRead,
            _: &RelationMetadata,
            _: &[Option<Value>],
        ) -> Result<usize, KernelError> {
            Ok(10)
        }
        fn scan_batch(
            &self,
            reader: &dyn ComputedRelationRead,
            metadata: &RelationMetadata,
            keys: &[Vec<Option<Value>>],
        ) -> Result<Vec<ComputedRow>, KernelError> {
            self.batches.fetch_add(1, Ordering::Relaxed);
            let candidates = self.scan(reader, metadata, &[])?;
            Ok((0..keys.len())
                .flat_map(|input_row| {
                    candidates
                        .iter()
                        .cloned()
                        .map(move |tuple| ComputedRow { input_row, tuple })
                })
                .collect())
        }
    }

    fn id(raw: u64) -> RelationId {
        Identity::new(raw).unwrap()
    }
    fn int(value: i64) -> Value {
        Value::int(value).unwrap()
    }

    fn lookup_kernel() -> (crate::RelationKernel, Arc<AtomicUsize>) {
        let batches = Arc::new(AtomicUsize::new(0));
        let kernel = crate::RelationKernel::with_provider_and_computed_relations(
            Arc::new(crate::InMemoryCommitProvider::new()),
            [Arc::new(LookupRelation {
                batches: batches.clone(),
            }) as Arc<dyn ComputedRelation>],
        );
        for (raw, name, arity) in [
            (10, "Keys", 1),
            (11, "Lookup", 2),
            (12, "Backing", 2),
            (13, "Answer", 2),
        ] {
            kernel
                .create_relation(RelationMetadata::new(id(raw), Symbol::intern(name), arity))
                .unwrap();
        }
        let mut tx = kernel.begin();
        for key in 1..=3 {
            tx.assert(id(10), Tuple::from([int(key)])).unwrap();
            tx.assert(id(12), Tuple::from([int(key), int(key * 10)]))
                .unwrap();
        }
        tx.commit().unwrap();
        (kernel, batches)
    }

    #[test]
    fn batch_matches_scalar_scans_and_observes_transaction_changes() {
        let (kernel, batches) = lookup_kernel();
        let snapshot = kernel.snapshot();
        let mut tx = kernel.begin();
        tx.retract(id(12), Tuple::from([int(1), int(10)])).unwrap();
        tx.assert(id(12), Tuple::from([int(1), int(99)])).unwrap();
        let keys = vec![
            vec![Some(int(1)), None],
            vec![Some(int(2)), Some(int(99))],
            vec![Some(int(1)), Some(int(99))],
        ];
        let rows = tx.scan_relation_batch(id(11), &keys).unwrap().unwrap();
        assert_eq!(
            rows,
            vec![
                ComputedRow {
                    input_row: 0,
                    tuple: Tuple::from([int(1), int(99)])
                },
                ComputedRow {
                    input_row: 2,
                    tuple: Tuple::from([int(1), int(99)])
                }
            ]
        );
        for (input, key) in keys.iter().enumerate() {
            assert_eq!(
                rows.iter()
                    .filter(|row| row.input_row == input)
                    .map(|row| row.tuple.clone())
                    .collect::<Vec<_>>(),
                tx.scan_relation(id(11), key).unwrap()
            );
        }
        assert_eq!(
            snapshot
                .scan_relation_batch(id(11), &keys)
                .unwrap()
                .unwrap(),
            vec![ComputedRow {
                input_row: 0,
                tuple: Tuple::from([int(1), int(10)])
            }]
        );
        let calls = batches.load(Ordering::Relaxed);
        assert!(
            tx.scan_relation_batch(id(11), &[])
                .unwrap()
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            tx.scan_relation_batch(id(11), &[vec![None, None]]),
            Err(KernelError::MissingRequiredBindings { .. })
        ));
        assert!(matches!(
            tx.scan_relation_batch(id(11), &[vec![Some(int(1))]]),
            Err(KernelError::ArityMismatch { .. })
        ));
        assert_eq!(batches.load(Ordering::Relaxed), calls);
    }

    #[test]
    fn rule_planning_binds_computed_inputs_before_batching() {
        let (kernel, batches) = lookup_kernel();
        let x = crate::Term::Var(Symbol::intern("x"));
        let y = crate::Term::Var(Symbol::intern("y"));
        let rules = crate::RuleSet::new([crate::Rule::new(
            id(13),
            [x.clone(), y.clone()],
            [
                crate::Atom::positive(id(11), [x.clone(), y]),
                crate::Atom::positive(id(10), [x]),
            ],
        )]);
        let result = rules
            .evaluate(&kernel.begin(), &crate::ExecutionContext::serial())
            .unwrap();
        assert_eq!(
            result[&id(13)],
            vec![
                Tuple::from([int(1), int(10)]),
                Tuple::from([int(2), int(20)]),
                Tuple::from([int(3), int(30)])
            ]
        );
        assert!(batches.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn query_join_batches_bound_computed_probes() {
        let (kernel, batches) = lookup_kernel();
        let query = crate::QueryPlan::join_eq(
            crate::QueryPlan::scan(id(10), [None]),
            crate::QueryPlan::scan(id(11), [None, None]),
            [0],
            [0],
        );
        let rows = query
            .execute(&kernel.begin(), &crate::ExecutionContext::serial())
            .unwrap();
        assert_eq!(
            rows,
            vec![
                Tuple::from([int(1), int(1), int(10)]),
                Tuple::from([int(2), int(2), int(20)]),
                Tuple::from([int(3), int(3), int(30)])
            ]
        );
        assert_eq!(batches.load(Ordering::Relaxed), 1);
    }
    struct InvalidBatchRelation {
        invalid_index: bool,
    }

    impl ComputedRelation for InvalidBatchRelation {
        fn name(&self) -> &'static str {
            "invalid-batch-test"
        }
        fn matches(&self, _: &RelationMetadata) -> bool {
            true
        }
        fn scan(
            &self,
            _: &dyn ComputedRelationRead,
            _: &RelationMetadata,
            _: &[Option<Value>],
        ) -> Result<Vec<Tuple>, KernelError> {
            Ok(vec![Tuple::from([int(1), int(2)])])
        }
        fn scan_batch(
            &self,
            _: &dyn ComputedRelationRead,
            _: &RelationMetadata,
            _: &[Vec<Option<Value>>],
        ) -> Result<Vec<ComputedRow>, KernelError> {
            Ok(vec![ComputedRow {
                input_row: usize::from(self.invalid_index),
                tuple: if self.invalid_index {
                    Tuple::from([int(1)])
                } else {
                    Tuple::from([int(1), int(2)])
                },
            }])
        }
    }

    #[test]
    fn malformed_provider_results_return_errors_before_filtering() {
        let snapshot = crate::RelationKernel::new().snapshot();
        let metadata = relation(1, "Invalid");
        for invalid_index in [false, true] {
            let registry =
                ComputedRelationRegistry::new([
                    Arc::new(InvalidBatchRelation { invalid_index }) as Arc<dyn ComputedRelation>
                ]);
            assert!(matches!(
                registry
                    .scan(snapshot.as_ref(), &metadata, &[None])
                    .unwrap(),
                Err(KernelError::ArityMismatch { .. })
            ));
            let result = registry
                .scan_batch(snapshot.as_ref(), &metadata, &[vec![Some(int(99))]])
                .unwrap();
            if invalid_index {
                assert!(matches!(
                    result,
                    Err(KernelError::InvalidComputedRelation { .. })
                ));
            } else {
                assert!(matches!(result, Err(KernelError::ArityMismatch { .. })));
            }
        }
    }
}
