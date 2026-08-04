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
    Atom, CatalogChange, CatalogFact, CatalogPredicate, Commit, CommitProvider, ComputedRelation,
    Conflict, ConflictKind, ConflictPolicy, DispatchRelations, Fact, FactChange, FactChangeKind,
    InMemoryCommitProvider, KernelError, MentionedFact, ProjectedStore, RelationDurability,
    RelationId, RelationKernel, RelationMetadata, RelationRead, RelationSource, RelationWorkspace,
    Rule, RuleBodyItem, RuleComparisonOp, RuleGuard, SubjectFact, Term, Tuple, ValueDomain,
    applicable_positional_methods_cached, method_program_id,
};
#[cfg(feature = "fjall-provider")]
use crate::{FjallDurabilityMode, FjallFormatStatus, FjallStateProvider};
use mica_var::{Identity, Symbol, Value};
#[cfg(feature = "fjall-provider")]
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
#[cfg(feature = "fjall-provider")]
use std::time::{SystemTime, UNIX_EPOCH};

fn rel(id: u64) -> RelationId {
    Identity::new(id).unwrap()
}

fn int(value: i64) -> Value {
    Value::int(value).unwrap()
}

fn cap(value: u64) -> Value {
    Value::capability_raw(value).unwrap()
}

fn var(name: &str) -> Term {
    Term::Var(Symbol::intern(name))
}

fn val(value: Value) -> Term {
    Term::Value(value)
}

#[cfg(feature = "fjall-provider")]
struct TempStore {
    path: PathBuf,
}

struct FailAfterCommitProvider {
    commits: Mutex<Vec<Commit>>,
    remaining_successes: Mutex<usize>,
}

impl FailAfterCommitProvider {
    fn new(remaining_successes: usize) -> Self {
        Self {
            commits: Mutex::new(Vec::new()),
            remaining_successes: Mutex::new(remaining_successes),
        }
    }
}

impl CommitProvider for FailAfterCommitProvider {
    fn persist_commit(&self, commit: &Commit) -> Result<(), String> {
        self.commits.lock().unwrap().push(commit.clone());
        let mut remaining_successes = self.remaining_successes.lock().unwrap();
        if *remaining_successes == 0 {
            Err("intentional persistence failure".to_owned())
        } else {
            *remaining_successes -= 1;
            Ok(())
        }
    }
}

#[cfg(feature = "fjall-provider")]
impl TempStore {
    fn new(name: &str) -> Self {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mica-relation-kernel-{name}-{}-{suffix}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(feature = "fjall-provider")]
impl Drop for TempStore {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn fact_identity_is_separate_from_set_tuple_identity() {
    let tuple = Tuple::from([int(1), int(2)]);
    let fact = Fact::new(rel(99), rel(1), tuple.clone());

    assert_eq!(fact.id(), rel(99));
    assert_eq!(fact.relation(), rel(1));
    assert_eq!(fact.tuple(), &tuple);
}

#[test]
fn staged_snapshot_publishes_catalog_and_facts_as_one_commit() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(RelationMetadata::new(rel(1), Symbol::intern("Reading"), 2))
        .unwrap();
    let old = Tuple::from([int(1), int(10)]);
    let new = Tuple::from([int(1), int(20)]);
    let detail = Tuple::from([int(1), Value::string("calibrated")]);
    let mut seed = kernel.begin();
    seed.assert(rel(1), old.clone()).unwrap();
    seed.commit().unwrap();

    let base_version = kernel.snapshot().version();
    let staged = kernel.fork_in_memory();
    staged
        .create_relation(RelationMetadata::new(
            rel(2),
            Symbol::intern("ReadingDetail"),
            2,
        ))
        .unwrap();
    let mut staged_tx = staged.begin();
    staged_tx.retract(rel(1), old.clone()).unwrap();
    staged_tx.assert(rel(1), new.clone()).unwrap();
    staged_tx.assert(rel(2), detail.clone()).unwrap();
    staged_tx.commit().unwrap();

    assert_eq!(
        kernel.snapshot().scan(rel(1), &[None, None]).unwrap(),
        vec![old]
    );
    assert!(matches!(
        kernel.snapshot().scan(rel(2), &[None, None]),
        Err(KernelError::UnknownRelation(relation)) if relation == rel(2)
    ));

    let result = kernel
        .commit_staged_snapshot(base_version, staged.snapshot())
        .unwrap();

    assert_eq!(result.commit().version(), base_version + 1);
    assert_eq!(result.commit().catalog_changes().len(), 1);
    assert_eq!(result.commit().changes().len(), 3);
    assert_eq!(
        kernel.snapshot().scan(rel(1), &[None, None]).unwrap(),
        vec![new]
    );
    assert_eq!(
        kernel.snapshot().scan(rel(2), &[None, None]).unwrap(),
        vec![detail]
    );
    assert_eq!(kernel.snapshot().commits_since(base_version).len(), 1);
}

#[test]
fn stale_staged_snapshot_is_rejected_without_publishing() {
    let kernel = kernel_with_located();
    let base_version = kernel.snapshot().version();
    let staged = kernel.fork_in_memory();
    let staged_tuple = Tuple::from([int(1), int(20)]);
    let mut staged_tx = staged.begin();
    staged_tx.assert(rel(1), staged_tuple.clone()).unwrap();
    staged_tx.commit().unwrap();

    let current_tuple = Tuple::from([int(1), int(30)]);
    let mut current_tx = kernel.begin();
    current_tx.assert(rel(1), current_tuple.clone()).unwrap();
    current_tx.commit().unwrap();

    assert_eq!(
        kernel
            .commit_staged_snapshot(base_version, staged.snapshot())
            .unwrap_err(),
        KernelError::StaleStagedSnapshot {
            expected: base_version,
            actual: base_version + 1,
        }
    );
    assert_eq!(
        kernel.snapshot().scan(rel(1), &[None, None]).unwrap(),
        vec![current_tuple]
    );
    assert!(
        !kernel
            .snapshot()
            .scan(rel(1), &[None, None])
            .unwrap()
            .contains(&staged_tuple)
    );
}

#[test]
fn failed_staged_snapshot_persistence_keeps_current_snapshot() {
    let provider = Arc::new(FailAfterCommitProvider::new(2));
    let kernel = RelationKernel::with_provider(provider);
    kernel
        .create_relation(RelationMetadata::new(rel(1), Symbol::intern("Reading"), 2))
        .unwrap();
    let original = Tuple::from([int(1), int(10)]);
    let replacement = Tuple::from([int(1), int(20)]);
    let mut seed = kernel.begin();
    seed.assert(rel(1), original.clone()).unwrap();
    seed.commit().unwrap();

    let base_version = kernel.snapshot().version();
    let staged = kernel.fork_in_memory();
    let mut staged_tx = staged.begin();
    staged_tx.retract(rel(1), original.clone()).unwrap();
    staged_tx.assert(rel(1), replacement).unwrap();
    staged_tx.commit().unwrap();

    assert!(matches!(
        kernel.commit_staged_snapshot(base_version, staged.snapshot()),
        Err(KernelError::Persistence(message)) if message == "intentional persistence failure"
    ));
    assert_eq!(kernel.snapshot().version(), base_version);
    assert_eq!(
        kernel.snapshot().scan(rel(1), &[None, None]).unwrap(),
        vec![original]
    );
}

fn kernel_with_located() -> RelationKernel {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(
            RelationMetadata::new(rel(1), Symbol::intern("LocatedIn"), 2)
                .with_index([0])
                .with_index([1, 0]),
        )
        .unwrap();
    kernel
}

struct EchoComputedRelation;

impl ComputedRelation for EchoComputedRelation {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("ComputedEcho") && metadata.arity() == 2
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        &[0]
    }

    fn scan(
        &self,
        _reader: &dyn crate::ComputedRelationRead,
        _metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        if bindings[0] != Some(int(7)) {
            return Ok(Vec::new());
        }
        Ok(vec![Tuple::from([int(7), int(11)])])
    }
}

struct MirrorLocatedInComputedRelation;

impl ComputedRelation for MirrorLocatedInComputedRelation {
    fn name(&self) -> &'static str {
        "mirror-located-in"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("MirrorLocatedIn") && metadata.arity() == 2
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        &[0]
    }

    fn scan(
        &self,
        reader: &dyn crate::ComputedRelationRead,
        _metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let Some(subject) = bindings[0].clone() else {
            return Ok(Vec::new());
        };
        reader.scan_relation(rel(1), &[Some(subject), None])
    }
}

#[test]
fn catalog_facts_expose_relation_metadata_as_relations() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(
            RelationMetadata::new(rel(77), Symbol::intern("Delegates"), 3)
                .with_argument_name(0, Symbol::intern("child"))
                .with_argument_name(1, Symbol::intern("proto"))
                .with_argument_name(2, Symbol::intern("rank"))
                .with_index([0, 2, 1])
                .with_conflict_policy(ConflictPolicy::Functional {
                    key_positions: vec![0, 2],
                })
                .with_durability(RelationDurability::Volatile),
        )
        .unwrap();

    let facts = kernel.snapshot().catalog_facts();
    assert!(facts.contains(&CatalogFact {
        predicate: CatalogPredicate::RelationName,
        tuple: Tuple::from([
            Value::identity(rel(77)),
            Value::symbol(Symbol::intern("Delegates"))
        ]),
    }));
    assert!(facts.contains(&CatalogFact {
        predicate: CatalogPredicate::ArgumentName,
        tuple: Tuple::from([
            Value::identity(rel(77)),
            int(1),
            Value::symbol(Symbol::intern("proto")),
        ]),
    }));
    assert!(facts.contains(&CatalogFact {
        predicate: CatalogPredicate::RelationDurability,
        tuple: Tuple::from([
            Value::identity(rel(77)),
            Value::symbol(Symbol::intern("volatile")),
        ]),
    }));
    assert!(facts.contains(&CatalogFact {
        predicate: CatalogPredicate::FunctionalKey,
        tuple: Tuple::from([Value::identity(rel(77)), int(1), int(2)]),
    }));
    assert!(facts.iter().any(|fact| {
        fact.predicate == CatalogPredicate::IndexStorageKind
            && fact.tuple.values()[1] == Value::symbol(Symbol::intern("btree"))
    }));
}

#[test]
fn creating_duplicate_relation_id_is_rejected() {
    let kernel = kernel_with_located();
    let error = kernel
        .create_relation(RelationMetadata::new(
            rel(1),
            Symbol::intern("LocatedElsewhere"),
            2,
        ))
        .unwrap_err();

    assert_eq!(error, KernelError::RelationAlreadyExists(rel(1)));
}

#[test]
fn custom_computed_relations_scan_and_reject_writes() {
    let kernel = RelationKernel::with_provider_and_computed_relations(
        Arc::new(InMemoryCommitProvider::new()),
        vec![Arc::new(EchoComputedRelation) as Arc<dyn ComputedRelation>],
    );
    kernel
        .create_relation(RelationMetadata::new(
            rel(200),
            Symbol::intern("ComputedEcho"),
            2,
        ))
        .unwrap();

    assert_eq!(
        kernel
            .snapshot()
            .scan_relation(rel(200), &[Some(int(7)), None])
            .unwrap(),
        vec![Tuple::from([int(7), int(11)])]
    );
    assert_eq!(
        kernel
            .snapshot()
            .scan_relation(rel(200), &[None, None])
            .unwrap_err(),
        KernelError::MissingRequiredBindings {
            relation: rel(200),
            positions: vec![0],
        }
    );

    let mut tx = kernel.begin();
    assert_eq!(
        tx.assert(rel(200), Tuple::from([int(7), int(11)]))
            .unwrap_err(),
        KernelError::ReadOnlyRelation(rel(200))
    );
}

#[test]
fn transaction_computed_relations_see_same_task_writes() {
    let kernel = RelationKernel::with_provider_and_computed_relations(
        Arc::new(InMemoryCommitProvider::new()),
        vec![Arc::new(MirrorLocatedInComputedRelation) as Arc<dyn ComputedRelation>],
    );
    kernel
        .create_relation(
            RelationMetadata::new(rel(1), Symbol::intern("LocatedIn"), 2).with_index([0]),
        )
        .unwrap();
    kernel
        .create_relation(RelationMetadata::new(
            rel(201),
            Symbol::intern("MirrorLocatedIn"),
            2,
        ))
        .unwrap();

    let mut tx = kernel.begin();
    tx.assert(rel(1), Tuple::from([int(42), int(99)])).unwrap();
    assert_eq!(
        tx.scan(rel(201), &[Some(int(42)), None]).unwrap(),
        vec![Tuple::from([int(42), int(99)])]
    );
}

#[test]
fn transaction_reads_own_asserts_and_retracts() {
    let kernel = kernel_with_located();
    let mut tx = kernel.begin();
    let tuple = Tuple::from([int(10), int(20)]);
    tx.assert(rel(1), tuple.clone()).unwrap();
    assert_eq!(
        tx.scan(rel(1), &[Some(int(10)), None]).unwrap(),
        vec![tuple.clone()]
    );
    tx.retract(rel(1), tuple).unwrap();
    assert!(tx.scan(rel(1), &[Some(int(10)), None]).unwrap().is_empty());
}

#[test]
fn pure_local_transaction_scan_returns_canonical_order() {
    let kernel = kernel_with_located();
    let mut tx = kernel.begin();

    for id in (0..80).rev() {
        tx.assert(rel(1), Tuple::from([int(id), int(id + 1000)]))
            .unwrap();
    }

    let expected = (0..80)
        .map(|id| Tuple::from([int(id), int(id + 1000)]))
        .collect::<Vec<_>>();
    assert_eq!(tx.scan(rel(1), &[None, None]).unwrap(), expected);
}

#[test]
fn transaction_scan_with_local_retractions_preserves_canonical_order() {
    let kernel = kernel_with_located();
    let mut seed = kernel.begin();
    for id in 0..80 {
        seed.assert(rel(1), Tuple::from([int(10), int(id)]))
            .unwrap();
    }
    seed.commit().unwrap();

    let mut tx = kernel.begin();
    for id in (0..80).rev() {
        if id % 2 == 0 {
            tx.retract(rel(1), Tuple::from([int(10), int(id)])).unwrap();
        }
    }

    let expected = (0..80)
        .filter(|id| id % 2 != 0)
        .map(|id| Tuple::from([int(10), int(id)]))
        .collect::<Vec<_>>();
    assert_eq!(tx.scan(rel(1), &[Some(int(10)), None]).unwrap(), expected);
}

#[test]
fn transaction_scan_with_local_assertions_preserves_canonical_order() {
    let kernel = kernel_with_located();
    let mut seed = kernel.begin();
    for id in 0..80 {
        seed.assert(rel(1), Tuple::from([int(10), int(id * 2)]))
            .unwrap();
    }
    seed.commit().unwrap();

    let mut tx = kernel.begin();
    for id in (0..80).rev() {
        tx.assert(rel(1), Tuple::from([int(10), int(id * 2 + 1)]))
            .unwrap();
    }
    tx.assert(rel(1), Tuple::from([int(10), int(20)])).unwrap();

    let expected = (0..160)
        .map(|id| Tuple::from([int(10), int(id)]))
        .collect::<Vec<_>>();
    assert_eq!(tx.scan(rel(1), &[Some(int(10)), None]).unwrap(), expected);
}

#[test]
fn promoted_transaction_overlay_preserves_local_visibility() {
    let kernel = kernel_with_located();
    let mut tx = kernel.begin();

    for id in 0..160 {
        tx.assert(rel(1), Tuple::from([int(id), int(id + 1000)]))
            .unwrap();
    }

    let original = Tuple::from([int(42), int(1042)]);
    let replacement = Tuple::from([int(42), int(9000)]);
    tx.retract(rel(1), original).unwrap();
    tx.assert(rel(1), replacement.clone()).unwrap();

    for _ in 0..3 {
        assert_eq!(
            tx.scan(rel(1), &[Some(int(42)), None]).unwrap(),
            vec![replacement.clone()]
        );
    }

    tx.commit().unwrap();
    assert_eq!(
        kernel
            .snapshot()
            .scan_relation(rel(1), &[Some(int(42)), None])
            .unwrap(),
        vec![replacement]
    );
}

#[test]
fn large_committed_relation_preserves_canonical_tuple_visibility() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(
            RelationMetadata::new(rel(61), Symbol::intern("LargeCanonical"), 2).without_indexes(),
        )
        .unwrap();

    let mut seed = kernel.begin();
    for id in 0..5000 {
        seed.assert(rel(61), Tuple::from([int(id), int(id + 10_000)]))
            .unwrap();
    }
    seed.commit().unwrap();

    let replacement = Tuple::from([int(42), int(9000)]);
    let mut tx = kernel.begin();
    tx.retract(rel(61), Tuple::from([int(42), int(10_042)]))
        .unwrap();
    tx.assert(rel(61), replacement.clone()).unwrap();
    tx.commit().unwrap();

    let snapshot = kernel.snapshot();
    assert_eq!(
        snapshot
            .scan_relation(rel(61), &[Some(int(42)), None])
            .unwrap(),
        vec![replacement]
    );
    assert_eq!(
        snapshot
            .scan_relation(rel(61), &[None, Some(int(10_043))])
            .unwrap(),
        vec![Tuple::from([int(43), int(10_043)])]
    );
}

#[test]
fn large_default_relation_preserves_natural_prefix_scans() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(RelationMetadata::new(
            rel(62),
            Symbol::intern("LargeDefault"),
            2,
        ))
        .unwrap();

    let mut seed = kernel.begin();
    for id in 0..5000 {
        seed.assert(rel(62), Tuple::from([int(id), int(id + 10_000)]))
            .unwrap();
    }
    seed.commit().unwrap();

    assert_eq!(
        kernel
            .snapshot()
            .scan_relation(rel(62), &[Some(int(42)), None])
            .unwrap(),
        vec![Tuple::from([int(42), int(10_042)])]
    );
}

#[test]
fn bulk_empty_relation_commit_preserves_secondary_indexes() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(
            RelationMetadata::new(rel(63), Symbol::intern("BulkIndexed"), 3).with_index([1, 0]),
        )
        .unwrap();

    let mut seed = kernel.begin();
    for group in 0..100 {
        for item in 0..50 {
            seed.assert(
                rel(63),
                Tuple::from([int(item), int(group), int(group * 1000 + item)]),
            )
            .unwrap();
        }
    }
    seed.commit().unwrap();

    let snapshot = kernel.snapshot();
    let found = snapshot
        .scan(rel(63), &[None, Some(int(42)), None])
        .unwrap();
    assert_eq!(found.len(), 50);
    assert_eq!(
        found.first(),
        Some(&Tuple::from([int(0), int(42), int(42_000)]))
    );
    assert_eq!(
        found.last(),
        Some(&Tuple::from([int(49), int(42), int(42_049)]))
    );
    assert_eq!(
        snapshot
            .scan(rel(63), &[Some(int(7)), Some(int(42)), None])
            .unwrap(),
        vec![Tuple::from([int(7), int(42), int(42_007)])]
    );
}

#[test]
fn transaction_rejects_capability_values_in_tuples() {
    let kernel = kernel_with_located();
    let mut tx = kernel.begin();
    let tuple = Tuple::from([int(10), cap(1)]);

    assert_eq!(
        tx.assert(rel(1), tuple.clone()).unwrap_err(),
        KernelError::NonPersistentValue {
            relation: rel(1),
            tuple,
        }
    );

    let nested = Tuple::from([
        int(10),
        Value::map([(Value::symbol(Symbol::intern("cap")), cap(2))]),
    ]);
    assert_eq!(
        tx.replace_functional(rel(1), nested.clone()).unwrap_err(),
        KernelError::NonPersistentValue {
            relation: rel(1),
            tuple: nested,
        }
    );
}

#[test]
fn transaction_accepts_persistable_relation_value_cells() {
    let kernel = kernel_with_located();
    let nested = Value::relation(
        [Symbol::intern("value")],
        [Tuple::from([Value::string("nested")])],
    )
    .unwrap();
    let tuple = Tuple::from([int(10), nested]);

    let mut tx = kernel.begin();
    tx.assert(rel(1), tuple.clone()).unwrap();
    tx.commit().unwrap();

    assert_eq!(
        kernel.snapshot().scan(rel(1), &[None, None]).unwrap(),
        vec![tuple]
    );
}

#[test]
fn transaction_rejects_capabilities_nested_in_relation_values() {
    let kernel = kernel_with_located();
    let nested = Value::relation([Symbol::intern("value")], [Tuple::from([cap(1)])]).unwrap();
    let tuple = Tuple::from([int(10), nested]);

    let mut tx = kernel.begin();
    assert_eq!(
        tx.assert(rel(1), tuple.clone()).unwrap_err(),
        KernelError::NonPersistentValue {
            relation: rel(1),
            tuple,
        }
    );
}

#[test]
fn installed_rules_derive_tuples_as_relation_reads() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(RelationMetadata::new(
            rel(1),
            Symbol::intern("LocatedIn"),
            2,
        ))
        .unwrap();
    kernel
        .create_relation(RelationMetadata::new(
            rel(2),
            Symbol::intern("VisibleTo"),
            2,
        ))
        .unwrap();
    kernel
        .install_rule(
            Rule::new(
                rel(2),
                [var("actor"), var("obj")],
                [
                    Atom::positive(rel(1), [var("actor"), var("room")]),
                    Atom::positive(rel(1), [var("obj"), var("room")]),
                ],
            ),
            "VisibleTo(actor, obj) :- LocatedIn(actor, room), LocatedIn(obj, room)",
        )
        .unwrap();

    let mut tx = kernel.begin();
    tx.assert(rel(1), Tuple::from([int(10), int(1)])).unwrap();
    tx.assert(rel(1), Tuple::from([int(20), int(1)])).unwrap();

    assert_eq!(
        tx.scan(rel(2), &[Some(int(10)), None]).unwrap(),
        vec![
            Tuple::from([int(10), int(10)]),
            Tuple::from([int(10), int(20)])
        ]
    );
}

#[test]
fn relation_reads_union_asserted_and_rule_derived_tuples() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(RelationMetadata::new(
            rel(1),
            Symbol::intern("LocatedIn"),
            2,
        ))
        .unwrap();
    kernel
        .create_relation(RelationMetadata::new(
            rel(2),
            Symbol::intern("VisibleTo"),
            2,
        ))
        .unwrap();
    kernel
        .install_rule(
            Rule::new(
                rel(2),
                [var("actor"), var("obj")],
                [Atom::positive(rel(1), [var("actor"), var("obj")])],
            ),
            "VisibleTo(actor, obj) :- LocatedIn(actor, obj)",
        )
        .unwrap();

    let mut seed = kernel.begin();
    seed.assert(rel(1), Tuple::from([int(10), int(20)]))
        .unwrap();
    seed.assert(rel(2), Tuple::from([int(99), int(100)]))
        .unwrap();
    seed.commit().unwrap();

    assert_eq!(
        kernel.snapshot().scan(rel(2), &[None, None]).unwrap(),
        vec![
            Tuple::from([int(10), int(20)]),
            Tuple::from([int(99), int(100)])
        ]
    );
}

#[test]
fn exact_relation_indexes_remain_visible_when_rules_exist() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(
            RelationMetadata::new(rel(1), Symbol::intern("Indexed"), 2).with_index([0]),
        )
        .unwrap();
    kernel
        .create_relation(RelationMetadata::new(rel(2), Symbol::intern("Base"), 2))
        .unwrap();
    kernel
        .create_relation(RelationMetadata::new(rel(3), Symbol::intern("Derived"), 2))
        .unwrap();
    kernel
        .install_rule(
            Rule::new(
                rel(3),
                [var("left"), var("right")],
                [Atom::positive(rel(2), [var("left"), var("right")])],
            ),
            "Derived(left, right) :- Base(left, right)",
        )
        .unwrap();

    let snapshot = kernel.snapshot();
    assert!(
        snapshot
            .has_exact_relation_index(rel(1), &[0])
            .expect("indexed relation should answer index metadata")
    );
    assert!(
        snapshot
            .has_exact_relation_index(rel(3), &[0, 1])
            .expect("derived relation should expose natural full tuple index")
    );
}

#[test]
fn transaction_derived_cache_invalidates_after_local_write() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(RelationMetadata::new(rel(1), Symbol::intern("Base"), 1))
        .unwrap();
    kernel
        .create_relation(RelationMetadata::new(rel(2), Symbol::intern("Derived"), 1))
        .unwrap();
    kernel
        .install_rule(
            Rule::new(
                rel(2),
                [var("item")],
                [Atom::positive(rel(1), [var("item")])],
            ),
            "Derived(item) :- Base(item)",
        )
        .unwrap();

    let mut tx = kernel.begin();
    tx.assert(rel(1), Tuple::from([int(1)])).unwrap();
    assert_eq!(
        tx.scan(rel(2), &[None]).unwrap(),
        vec![Tuple::from([int(1)])]
    );

    tx.assert(rel(1), Tuple::from([int(2)])).unwrap();
    assert_eq!(
        tx.scan(rel(2), &[None]).unwrap(),
        vec![Tuple::from([int(1)]), Tuple::from([int(2)])]
    );
}

#[test]
fn volatile_rows_participate_in_rules_until_recovery() {
    let provider = Arc::new(InMemoryCommitProvider::new());
    let kernel = RelationKernel::with_provider(provider.clone());
    kernel
        .create_relation(
            RelationMetadata::new(rel(1), Symbol::intern("VolatileBase"), 1)
                .with_durability(RelationDurability::Volatile),
        )
        .unwrap();
    kernel
        .create_relation(RelationMetadata::new(rel(2), Symbol::intern("Derived"), 1))
        .unwrap();
    kernel
        .install_rule(
            Rule::new(
                rel(2),
                [var("item")],
                [Atom::positive(rel(1), [var("item")])],
            ),
            "Derived(item) :- VolatileBase(item)",
        )
        .unwrap();

    let mut transaction = kernel.begin();
    transaction.assert(rel(1), Tuple::from([int(42)])).unwrap();
    assert_eq!(
        transaction.scan(rel(2), &[None]).unwrap(),
        vec![Tuple::from([int(42)])]
    );
    transaction.commit().unwrap();
    assert_eq!(
        kernel.snapshot().scan(rel(2), &[None]).unwrap(),
        vec![Tuple::from([int(42)])]
    );

    let recovered = RelationKernel::load_from_commit_log(
        provider.commits(),
        Arc::new(InMemoryCommitProvider::new()),
    )
    .unwrap();
    assert!(
        recovered
            .snapshot()
            .scan(rel(1), &[None])
            .unwrap()
            .is_empty()
    );
    assert!(
        recovered
            .snapshot()
            .scan(rel(2), &[None])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn installed_rules_have_catalog_facts_and_can_be_disabled() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(RelationMetadata::new(
            rel(1),
            Symbol::intern("LocatedIn"),
            2,
        ))
        .unwrap();
    kernel
        .create_relation(RelationMetadata::new(
            rel(2),
            Symbol::intern("VisibleTo"),
            2,
        ))
        .unwrap();
    let definition = kernel
        .install_rule(
            Rule::new(
                rel(2),
                [var("actor"), var("obj")],
                [Atom::positive(rel(1), [var("actor"), var("obj")])],
            ),
            "VisibleTo(actor, obj) :- LocatedIn(actor, obj)",
        )
        .unwrap();
    let rule_id = definition.id();

    let facts = kernel.snapshot().catalog_facts();
    assert!(facts.contains(&CatalogFact {
        predicate: CatalogPredicate::Rule,
        tuple: Tuple::from([Value::identity(rule_id)]),
    }));
    assert!(facts.contains(&CatalogFact {
        predicate: CatalogPredicate::RuleHead,
        tuple: Tuple::from([Value::identity(rule_id), Value::identity(rel(2))]),
    }));
    assert!(facts.contains(&CatalogFact {
        predicate: CatalogPredicate::RuleSource,
        tuple: Tuple::from([
            Value::identity(rule_id),
            Value::string("VisibleTo(actor, obj) :- LocatedIn(actor, obj)")
        ]),
    }));
    assert!(facts.contains(&CatalogFact {
        predicate: CatalogPredicate::ActiveRule,
        tuple: Tuple::from([Value::identity(rule_id), Value::bool(true)]),
    }));

    let mut seed = kernel.begin();
    seed.assert(rel(1), Tuple::from([int(10), int(20)]))
        .unwrap();
    seed.commit().unwrap();
    assert_eq!(
        kernel.snapshot().scan(rel(2), &[None, None]).unwrap(),
        vec![Tuple::from([int(10), int(20)])]
    );

    kernel.disable_rule(rule_id).unwrap();
    assert_eq!(
        kernel.snapshot().scan(rel(2), &[None, None]).unwrap(),
        vec![]
    );
    assert!(kernel.snapshot().catalog_facts().contains(&CatalogFact {
        predicate: CatalogPredicate::ActiveRule,
        tuple: Tuple::from([Value::identity(rule_id), Value::bool(false)]),
    }));
}

#[test]
fn committed_snapshot_is_immutable_for_existing_transactions() {
    let kernel = kernel_with_located();
    let old = kernel.begin();

    let mut tx = kernel.begin();
    tx.assert(rel(1), Tuple::from([int(1), int(2)])).unwrap();
    tx.commit().unwrap();

    assert!(old.scan(rel(1), &[None, None]).unwrap().is_empty());
    assert_eq!(
        kernel.snapshot().scan(rel(1), &[None, None]).unwrap().len(),
        1
    );
}

#[test]
fn concurrent_identical_set_asserts_merge() {
    let kernel = kernel_with_located();
    let mut left = kernel.begin();
    let mut right = kernel.begin();
    let tuple = Tuple::from([int(1), int(2)]);
    left.assert(rel(1), tuple.clone()).unwrap();
    right.assert(rel(1), tuple.clone()).unwrap();
    left.commit().unwrap();
    let right_commit = right.commit().unwrap();
    assert!(right_commit.commit().changes().is_empty());
    assert_eq!(
        kernel.snapshot().scan(rel(1), &[None, None]).unwrap(),
        vec![tuple]
    );
}

#[test]
fn stale_disjoint_set_asserts_both_commit() {
    let kernel = kernel_with_located();
    let mut left = kernel.begin();
    let mut right = kernel.begin();
    let left_tuple = Tuple::from([int(1), int(2)]);
    let right_tuple = Tuple::from([int(3), int(4)]);

    left.assert(rel(1), left_tuple.clone()).unwrap();
    right.assert(rel(1), right_tuple.clone()).unwrap();
    left.commit().unwrap();
    right.commit().unwrap();

    assert_eq!(
        kernel.snapshot().scan(rel(1), &[None, None]).unwrap(),
        vec![left_tuple, right_tuple]
    );
}

#[test]
fn stale_retract_of_absent_tuple_does_not_delete_concurrent_assert() {
    let kernel = kernel_with_located();
    let tuple = Tuple::from([int(1), int(2)]);
    let mut stale = kernel.begin();
    let mut inserter = kernel.begin();

    stale.retract(rel(1), tuple.clone()).unwrap();
    inserter.assert(rel(1), tuple.clone()).unwrap();
    inserter.commit().unwrap();
    let result = stale.commit().unwrap();

    assert!(result.commit().changes().is_empty());
    assert_eq!(
        kernel.snapshot().scan(rel(1), &[None, None]).unwrap(),
        vec![tuple]
    );
}

#[test]
fn method_program_cache_is_scoped_to_snapshot_version() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(
            RelationMetadata::new(rel(90), Symbol::intern("MethodProgram"), 2).with_index([0]),
        )
        .unwrap();
    let method = Value::identity(rel(91));
    let program = Value::identity(rel(92));
    let snapshot = kernel.snapshot();

    assert_eq!(
        method_program_id(snapshot.as_ref(), rel(90), &method).unwrap(),
        None
    );

    let mut tx = kernel.begin();
    tx.assert(rel(90), Tuple::from([method.clone(), program.clone()]))
        .unwrap();
    tx.commit().unwrap();

    assert_eq!(
        method_program_id(kernel.snapshot().as_ref(), rel(90), &method).unwrap(),
        Some(program)
    );
}

#[test]
fn transaction_method_program_cache_bypasses_snapshot_after_local_writes() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(
            RelationMetadata::new(rel(93), Symbol::intern("MethodProgram"), 2).with_index([0]),
        )
        .unwrap();
    let method = Value::identity(rel(94));
    let program = Value::identity(rel(95));
    let snapshot = kernel.snapshot();
    assert_eq!(
        method_program_id(snapshot.as_ref(), rel(93), &method).unwrap(),
        None
    );

    let mut tx = kernel.begin();
    tx.assert(rel(93), Tuple::from([method.clone(), program.clone()]))
        .unwrap();

    assert_eq!(
        method_program_id(&tx, rel(93), &method).unwrap(),
        Some(program)
    );
}

#[test]
fn transaction_method_program_cache_survives_unrelated_local_writes() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(
            RelationMetadata::new(rel(93), Symbol::intern("MethodProgram"), 2).with_index([0]),
        )
        .unwrap();
    kernel
        .create_relation(RelationMetadata::new(rel(94), Symbol::intern("Event"), 2))
        .unwrap();
    let method = Value::identity(rel(95));
    let program = Value::identity(rel(96));
    let mut seed = kernel.begin();
    seed.assert(rel(93), Tuple::from([method.clone(), program.clone()]))
        .unwrap();
    seed.commit().unwrap();

    let mut tx = kernel.begin();
    tx.assert(rel(94), Tuple::from([int(1), int(2)])).unwrap();

    assert_eq!(
        method_program_id(&tx, rel(93), &method).unwrap(),
        Some(program)
    );
}

#[test]
fn transaction_method_program_cache_tracks_computed_relation_writes() {
    let kernel = RelationKernel::with_provider_and_computed_relations(
        Arc::new(InMemoryCommitProvider::new()),
        vec![Arc::new(MirrorLocatedInComputedRelation) as Arc<dyn ComputedRelation>],
    );
    kernel
        .create_relation(
            RelationMetadata::new(rel(1), Symbol::intern("ProgramBinding"), 2).with_index([0]),
        )
        .unwrap();
    kernel
        .create_relation(RelationMetadata::new(
            rel(201),
            Symbol::intern("MirrorLocatedIn"),
            2,
        ))
        .unwrap();
    kernel
        .create_relation(
            RelationMetadata::new(rel(40), Symbol::intern("MethodSelector"), 2).with_index([1, 0]),
        )
        .unwrap();
    kernel
        .create_relation(
            RelationMetadata::new(rel(41), Symbol::intern("Param"), 4).with_index([0, 1]),
        )
        .unwrap();
    kernel
        .create_relation(
            RelationMetadata::new(rel(42), Symbol::intern("Delegates"), 3).with_index([0, 2, 1]),
        )
        .unwrap();

    let method = Value::identity(rel(202));
    let program = Value::identity(rel(203));
    let selector = Value::symbol(Symbol::intern("look"));
    let binding = Tuple::from([method.clone(), program.clone()]);
    let dispatch_relations = DispatchRelations {
        method_selector: rel(40),
        param: rel(41),
        delegates: rel(42),
    };
    let mut seed = kernel.begin();
    seed.assert(rel(40), Tuple::from([method.clone(), selector.clone()]))
        .unwrap();
    seed.commit().unwrap();
    let mut tx = kernel.begin();

    for _ in 0..3 {
        assert_eq!(
            applicable_positional_methods_cached(&tx, dispatch_relations, selector.clone(), &[],)
                .unwrap()
                .as_ref(),
            std::slice::from_ref(&method)
        );
    }
    assert_eq!(method_program_id(&tx, rel(201), &method).unwrap(), None);
    assert_eq!(method_program_id(&tx, rel(201), &method).unwrap(), None);

    tx.assert(rel(1), binding.clone()).unwrap();
    assert_eq!(
        method_program_id(&tx, rel(201), &method).unwrap(),
        Some(program)
    );

    tx.retract(rel(1), binding).unwrap();
    assert_eq!(method_program_id(&tx, rel(201), &method).unwrap(), None);
}

#[test]
fn projected_store_applies_server_commits_without_provider() {
    let kernel = RelationKernel::new();
    let name = RelationMetadata::new(rel(73), Symbol::intern("Name"), 2)
        .with_index([0])
        .with_conflict_policy(ConflictPolicy::Functional {
            key_positions: vec![0],
        });
    let create_snapshot = kernel.create_relation(name.clone()).unwrap();
    let create_commit = create_snapshot.commits_since(0).last().unwrap().clone();
    let mut tx = kernel.begin();
    tx.replace_functional(rel(73), Tuple::from([int(1), Value::string("lamp")]))
        .unwrap();
    let write_commit = tx.commit().unwrap().commit().clone();

    let mut projected = ProjectedStore::new();
    projected.apply_commit(&create_commit).unwrap();
    projected.apply_commit(&write_commit).unwrap();

    assert_eq!(
        projected
            .scan_relation(rel(73), &[Some(int(1)), None])
            .unwrap(),
        vec![Tuple::from([int(1), Value::string("lamp")])]
    );
}

#[test]
fn projected_store_implements_workspace_for_local_mutation() {
    fn rename(
        workspace: &mut impl RelationWorkspace,
        relation: RelationId,
        object: Value,
        name: &str,
    ) {
        workspace
            .replace_functional_tuple(relation, Tuple::from([object, Value::string(name)]))
            .unwrap();
    }

    let mut projected = ProjectedStore::new();
    projected
        .create_relation(
            RelationMetadata::new(rel(74), Symbol::intern("Name"), 2)
                .with_index([0])
                .with_conflict_policy(ConflictPolicy::Functional {
                    key_positions: vec![0],
                }),
        )
        .unwrap();

    rename(&mut projected, rel(74), int(1), "brass lamp");
    rename(&mut projected, rel(74), int(1), "golden lamp");

    assert_eq!(
        projected
            .scan_relation(rel(74), &[Some(int(1)), None])
            .unwrap(),
        vec![Tuple::from([int(1), Value::string("golden lamp")])]
    );
    let capabilities = projected.relation_capabilities(rel(74)).unwrap();
    assert_eq!(capabilities.source, RelationSource::Projected);
    assert_eq!(capabilities.cardinality, Some(1));
    assert_eq!(
        capabilities.value_domains,
        vec![ValueDomain::Immediate, ValueDomain::Heap]
    );
}

#[test]
fn projected_store_evaluates_recursive_rules() {
    let mut projected = ProjectedStore::new();
    projected
        .create_relation(RelationMetadata::new(rel(75), Symbol::intern("Edge"), 2))
        .unwrap();
    projected
        .create_relation(RelationMetadata::new(
            rel(76),
            Symbol::intern("Reachable"),
            2,
        ))
        .unwrap();
    projected
        .install_rule(
            Rule::new(
                rel(76),
                [var("from"), var("to")],
                [Atom::positive(rel(75), [var("from"), var("to")])],
            ),
            "Reachable(from, to) :- Edge(from, to)",
        )
        .unwrap();
    projected
        .install_rule(
            Rule::new(
                rel(76),
                [var("from"), var("to")],
                [
                    Atom::positive(rel(75), [var("from"), var("mid")]),
                    Atom::positive(rel(76), [var("mid"), var("to")]),
                ],
            ),
            "Reachable(from, to) :- Edge(from, mid), Reachable(mid, to)",
        )
        .unwrap();
    projected
        .assert_tuple(rel(75), Tuple::from([int(1), int(2)]))
        .unwrap();
    projected
        .assert_tuple(rel(75), Tuple::from([int(2), int(3)]))
        .unwrap();

    assert_eq!(
        projected
            .scan_relation(rel(76), &[Some(int(1)), Some(int(3))])
            .unwrap(),
        vec![Tuple::from([int(1), int(3)])]
    );
}

#[test]
fn functional_replace_conflicts_when_key_changes_concurrently() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(
            RelationMetadata::new(rel(2), Symbol::intern("Name"), 2)
                .with_index([0])
                .with_conflict_policy(ConflictPolicy::Functional {
                    key_positions: vec![0],
                }),
        )
        .unwrap();

    let mut seed = kernel.begin();
    seed.replace_functional(rel(2), Tuple::from([int(1), Value::string("old")]))
        .unwrap();
    seed.commit().unwrap();

    let mut left = kernel.begin();
    let mut right = kernel.begin();
    left.replace_functional(rel(2), Tuple::from([int(1), Value::string("left")]))
        .unwrap();
    right
        .replace_functional(rel(2), Tuple::from([int(1), Value::string("right")]))
        .unwrap();
    left.commit().unwrap();

    let error = right.commit().unwrap_err();
    assert!(matches!(
        error,
        KernelError::Conflict(Conflict {
            kind: ConflictKind::FunctionalKeyChanged,
            ..
        })
    ));
}

#[test]
fn functional_replace_uses_latest_local_value_in_transaction() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(
            RelationMetadata::new(rel(82), Symbol::intern("Name"), 2)
                .with_index([0])
                .with_conflict_policy(ConflictPolicy::Functional {
                    key_positions: vec![0],
                }),
        )
        .unwrap();

    let mut seed = kernel.begin();
    seed.replace_functional(rel(82), Tuple::from([int(1), Value::string("old")]))
        .unwrap();
    seed.commit().unwrap();

    let mut tx = kernel.begin();
    tx.replace_functional(rel(82), Tuple::from([int(1), Value::string("middle")]))
        .unwrap();
    tx.replace_functional(rel(82), Tuple::from([int(1), Value::string("new")]))
        .unwrap();

    assert_eq!(
        tx.scan(rel(82), &[Some(int(1)), None]).unwrap(),
        vec![Tuple::from([int(1), Value::string("new")])]
    );

    tx.commit().unwrap();
    assert_eq!(
        kernel
            .snapshot()
            .scan_relation(rel(82), &[Some(int(1)), None])
            .unwrap(),
        vec![Tuple::from([int(1), Value::string("new")])]
    );
}

#[test]
fn functional_conflict_validation_supports_nonleading_keys() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(
            RelationMetadata::new(rel(3), Symbol::intern("OwnedName"), 3)
                .with_index([1])
                .with_conflict_policy(ConflictPolicy::Functional {
                    key_positions: vec![1],
                }),
        )
        .unwrap();

    let mut seed = kernel.begin();
    seed.replace_functional(
        rel(3),
        Tuple::from([int(100), int(1), Value::string("old")]),
    )
    .unwrap();
    seed.commit().unwrap();

    let mut left = kernel.begin();
    let mut right = kernel.begin();
    left.replace_functional(
        rel(3),
        Tuple::from([int(100), int(1), Value::string("left")]),
    )
    .unwrap();
    right
        .replace_functional(
            rel(3),
            Tuple::from([int(200), int(1), Value::string("right")]),
        )
        .unwrap();
    left.commit().unwrap();

    let error = right.commit().unwrap_err();
    assert!(matches!(
        error,
        KernelError::Conflict(Conflict {
            kind: ConflictKind::FunctionalKeyChanged,
            ..
        })
    ));
}

#[test]
fn indexed_scan_respects_non_leading_bindings() {
    let kernel = kernel_with_located();
    let mut tx = kernel.begin();
    tx.assert(rel(1), Tuple::from([int(1), int(9)])).unwrap();
    tx.assert(rel(1), Tuple::from([int(2), int(9)])).unwrap();
    tx.assert(rel(1), Tuple::from([int(3), int(8)])).unwrap();
    tx.commit().unwrap();

    let found = kernel
        .snapshot()
        .scan(rel(1), &[None, Some(int(9))])
        .unwrap();
    assert_eq!(found.len(), 2);
}

#[test]
fn relation_estimates_track_index_bucket_statistics_without_exact_scans() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(
            RelationMetadata::new(rel(3), Symbol::intern("Grouped"), 2).with_index([0]),
        )
        .unwrap();
    let mut tx = kernel.begin();
    tx.assert(rel(3), Tuple::from([int(1), int(10)])).unwrap();
    tx.assert(rel(3), Tuple::from([int(1), int(11)])).unwrap();
    tx.assert(rel(3), Tuple::from([int(2), int(20)])).unwrap();
    tx.commit().unwrap();

    let snapshot = kernel.snapshot();
    assert_eq!(
        snapshot
            .estimate_relation_scan(rel(3), &[Some(int(1)), None])
            .unwrap(),
        Some(2)
    );
    assert_eq!(
        snapshot
            .estimate_relation_scan(rel(3), &[None, Some(int(10))])
            .unwrap(),
        Some(3)
    );

    let mut tx = kernel.begin();
    tx.retract(rel(3), Tuple::from([int(1), int(10)])).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        kernel
            .snapshot()
            .estimate_relation_scan(rel(3), &[Some(int(1)), None])
            .unwrap(),
        Some(1)
    );
}

#[test]
fn relation_capabilities_describe_snapshot_domains_and_overlay_boundaries() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(RelationMetadata::new(
            rel(4),
            Symbol::intern("DomainExample"),
            2,
        ))
        .unwrap();
    let mut seed = kernel.begin();
    seed.assert(rel(4), Tuple::from([int(1), Value::string("heap")]))
        .unwrap();
    seed.assert(rel(4), Tuple::from([int(2), int(20)])).unwrap();
    seed.commit().unwrap();

    let snapshot = kernel.snapshot();
    let capabilities = snapshot.relation_capabilities(rel(4)).unwrap();
    assert_eq!(capabilities.source, RelationSource::Snapshot);
    assert_eq!(capabilities.cardinality, Some(2));
    assert_eq!(
        capabilities.value_domains,
        vec![ValueDomain::Immediate, ValueDomain::Mixed]
    );
    assert!(!capabilities.immediate_only());
    assert!(capabilities.supports_batch_export);

    let mut tx = kernel.begin();
    tx.assert(rel(4), Tuple::from([int(3), int(30)])).unwrap();
    let capabilities = tx.relation_capabilities(rel(4)).unwrap();
    assert_eq!(capabilities.source, RelationSource::TransactionOverlay);
    assert_eq!(
        capabilities.value_domains,
        vec![ValueDomain::Unknown, ValueDomain::Unknown]
    );
    assert!(capabilities.exact_indexes.is_empty());
    assert!(!capabilities.supports_batch_export);
}

#[test]
fn commit_result_records_semantic_fact_changes() {
    let kernel = kernel_with_located();
    let tuple = Tuple::from([int(1), int(2)]);

    let mut seed = kernel.begin();
    seed.assert(rel(1), tuple.clone()).unwrap();
    let seed_result = seed.commit().unwrap();
    assert_eq!(seed_result.commit().version(), 2);
    assert_eq!(
        seed_result.commit().changes(),
        &[FactChange {
            relation: rel(1),
            tuple: tuple.clone(),
            kind: FactChangeKind::Assert,
        }]
    );

    let mut tx = kernel.begin();
    tx.retract(rel(1), tuple.clone()).unwrap();
    let result = tx.commit().unwrap();
    assert_eq!(
        result.commit().changes(),
        &[FactChange {
            relation: rel(1),
            tuple,
            kind: FactChangeKind::Retract,
        }]
    );
    assert_eq!(result.snapshot().commits_since(1).len(), 2);
}

#[test]
fn settled_relation_changes_report_only_public_zero_crossings() {
    let kernel = RelationKernel::new();
    kernel
        .create_relation(RelationMetadata::new(rel(1), Symbol::intern("Public"), 1))
        .unwrap();
    kernel
        .create_relation(RelationMetadata::new(rel(2), Symbol::intern("Source"), 1))
        .unwrap();
    kernel
        .install_rule(
            Rule::new(
                rel(1),
                [var("value")],
                [Atom::positive(rel(2), [var("value")])],
            ),
            "Public(value) :- Source(value)",
        )
        .unwrap();
    let tuple = Tuple::from([int(1)]);
    let mut seed = kernel.begin();
    seed.assert(rel(2), tuple.clone()).unwrap();
    seed.commit().unwrap();
    assert_eq!(
        kernel.snapshot().scan(rel(1), &[None]).unwrap(),
        vec![tuple.clone()]
    );

    let mut add_second_proof = kernel.begin();
    add_second_proof.assert(rel(1), tuple.clone()).unwrap();
    let result = add_second_proof.commit().unwrap();
    assert!(result.commit().relation_changes().is_empty());

    let mut remove_rule_proof = kernel.begin();
    remove_rule_proof.retract(rel(2), tuple.clone()).unwrap();
    let result = remove_rule_proof.commit().unwrap();
    assert!(
        result
            .commit()
            .relation_changes()
            .iter()
            .all(|change| change.relation != rel(1))
    );

    let mut remove_last_proof = kernel.begin();
    remove_last_proof.retract(rel(1), tuple.clone()).unwrap();
    let result = remove_last_proof.commit().unwrap();
    assert_eq!(
        result.commit().relation_changes(),
        &[FactChange {
            relation: rel(1),
            tuple,
            kind: FactChangeKind::Retract,
        }]
    );
}

#[test]
fn successful_commits_are_persisted_as_fact_change_batches() {
    let provider = Arc::new(InMemoryCommitProvider::new());
    let kernel = RelationKernel::with_provider(provider.clone());
    kernel
        .create_relation(RelationMetadata::new(
            rel(1),
            Symbol::intern("LocatedIn"),
            2,
        ))
        .unwrap();

    let tuple = Tuple::from([int(1), int(2)]);
    let mut tx = kernel.begin();
    tx.assert(rel(1), tuple.clone()).unwrap();
    tx.commit().unwrap();

    let commits = provider.commits();
    assert_eq!(commits.len(), 2);
    assert_eq!(
        commits[0].catalog_changes(),
        &[CatalogChange::RelationCreated(RelationMetadata::new(
            rel(1),
            Symbol::intern("LocatedIn"),
            2,
        ))]
    );
    assert_eq!(commits[1].version(), 2);
    assert_eq!(
        commits[1].changes(),
        &[FactChange {
            relation: rel(1),
            tuple,
            kind: FactChangeKind::Assert,
        }]
    );
}

#[test]
fn volatile_changes_publish_live_without_entering_persistent_commits() {
    let provider = Arc::new(InMemoryCommitProvider::new());
    let kernel = RelationKernel::with_provider(provider.clone());
    kernel
        .create_relation(RelationMetadata::new(rel(1), Symbol::intern("Durable"), 1))
        .unwrap();
    kernel
        .create_relation(
            RelationMetadata::new(rel(2), Symbol::intern("Volatile"), 1)
                .with_durability(RelationDurability::Volatile),
        )
        .unwrap();

    let mut mixed = kernel.begin();
    mixed.assert(rel(1), Tuple::from([int(10)])).unwrap();
    mixed.assert(rel(2), Tuple::from([int(20)])).unwrap();
    let result = mixed.commit().unwrap();
    assert_eq!(result.commit().changes().len(), 2);
    assert_eq!(
        kernel.snapshot().scan(rel(2), &[None]).unwrap(),
        vec![Tuple::from([int(20)])]
    );

    let persisted_after_mixed = provider.commits();
    assert_eq!(persisted_after_mixed.len(), 3);
    assert_eq!(persisted_after_mixed[2].version(), 3);
    assert_eq!(persisted_after_mixed[2].changes().len(), 1);
    assert_eq!(persisted_after_mixed[2].changes()[0].relation, rel(1));

    let mut volatile_only = kernel.begin();
    volatile_only
        .assert(rel(2), Tuple::from([int(21)]))
        .unwrap();
    assert_eq!(volatile_only.commit().unwrap().commit().version(), 4);
    assert_eq!(provider.commits().len(), 3);

    let mut durable_after_gap = kernel.begin();
    durable_after_gap
        .assert(rel(1), Tuple::from([int(11)]))
        .unwrap();
    assert_eq!(durable_after_gap.commit().unwrap().commit().version(), 5);
    let persisted = provider.commits();
    assert_eq!(persisted.last().unwrap().version(), 5);

    let recovered =
        RelationKernel::load_from_commit_log(persisted, Arc::new(InMemoryCommitProvider::new()))
            .unwrap();
    assert_eq!(
        recovered.snapshot().scan(rel(1), &[None]).unwrap(),
        vec![Tuple::from([int(10)]), Tuple::from([int(11)])]
    );
    assert!(
        recovered
            .snapshot()
            .scan(rel(2), &[None])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn persisted_state_loader_discards_volatile_facts() {
    let state = crate::PersistedKernelState {
        version: 7,
        relations: vec![
            RelationMetadata::new(rel(1), Symbol::intern("Durable"), 1),
            RelationMetadata::new(rel(2), Symbol::intern("Volatile"), 1)
                .with_durability(RelationDurability::Volatile),
        ],
        rules: Vec::new(),
        facts: vec![
            (rel(1), Tuple::from([int(10)])),
            (rel(2), Tuple::from([int(20)])),
        ],
    };

    let kernel =
        RelationKernel::load_from_state(state, Arc::new(InMemoryCommitProvider::new())).unwrap();

    assert_eq!(
        kernel.snapshot().scan(rel(1), &[None]).unwrap(),
        vec![Tuple::from([int(10)])]
    );
    assert!(kernel.snapshot().scan(rel(2), &[None]).unwrap().is_empty());
}

#[test]
fn failed_persistence_does_not_publish_live_snapshot() {
    let provider = Arc::new(FailAfterCommitProvider::new(0));
    let kernel = RelationKernel::with_provider(provider);
    let error = kernel
        .create_relation(RelationMetadata::new(
            rel(1),
            Symbol::intern("LocatedIn"),
            2,
        ))
        .unwrap_err();
    assert!(matches!(error, KernelError::Persistence(_)));
    assert_eq!(kernel.snapshot().version(), 0);
    assert!(matches!(
        kernel.snapshot().scan(rel(1), &[None, None]),
        Err(KernelError::UnknownRelation(id)) if id == rel(1)
    ));
}

#[test]
fn failed_fact_persistence_does_not_publish_live_snapshot() {
    let provider = Arc::new(FailAfterCommitProvider::new(1));
    let kernel = RelationKernel::with_provider(provider);
    kernel
        .create_relation(RelationMetadata::new(
            rel(1),
            Symbol::intern("LocatedIn"),
            2,
        ))
        .unwrap();

    let tuple = Tuple::from([int(1), int(2)]);
    let mut tx = kernel.begin();
    tx.assert(rel(1), tuple.clone()).unwrap();
    let error = tx.commit().unwrap_err();
    assert!(matches!(error, KernelError::Persistence(_)));
    assert_eq!(kernel.snapshot().version(), 1);
    assert!(!kernel.snapshot().contains(rel(1), &tuple).unwrap());
}

#[test]
fn kernel_can_replay_persisted_commit_batches() {
    let provider = Arc::new(InMemoryCommitProvider::new());
    let metadata = RelationMetadata::new(rel(1), Symbol::intern("LocatedIn"), 2);
    let kernel = RelationKernel::with_provider(provider.clone());
    kernel.create_relation(metadata.clone()).unwrap();

    let kept = Tuple::from([int(1), int(2)]);
    let removed = Tuple::from([int(3), int(4)]);
    let mut seed = kernel.begin();
    seed.assert(rel(1), kept.clone()).unwrap();
    seed.assert(rel(1), removed.clone()).unwrap();
    seed.commit().unwrap();

    let mut tx = kernel.begin();
    tx.retract(rel(1), removed).unwrap();
    tx.commit().unwrap();

    let loaded = RelationKernel::load_from_commits(
        [metadata],
        provider.commits(),
        Arc::new(InMemoryCommitProvider::new()),
    )
    .unwrap();
    assert_eq!(loaded.snapshot().version(), 3);
    assert_eq!(
        loaded.snapshot().scan(rel(1), &[None, None]).unwrap(),
        vec![kept]
    );
}

#[test]
fn kernel_can_replay_catalog_and_fact_commit_log() {
    let provider = Arc::new(InMemoryCommitProvider::new());
    let kernel = RelationKernel::with_provider(provider.clone());
    kernel
        .create_relation(RelationMetadata::new(
            rel(1),
            Symbol::intern("LocatedIn"),
            2,
        ))
        .unwrap();

    let tuple = Tuple::from([int(1), int(2)]);
    let mut tx = kernel.begin();
    tx.assert(rel(1), tuple.clone()).unwrap();
    tx.commit().unwrap();

    let loaded = RelationKernel::load_from_commit_log(
        provider.commits(),
        Arc::new(InMemoryCommitProvider::new()),
    )
    .unwrap();
    assert_eq!(loaded.snapshot().version(), 2);
    assert_eq!(
        loaded.snapshot().scan(rel(1), &[None, None]).unwrap(),
        vec![tuple]
    );
}

#[cfg(feature = "fjall-provider")]
#[test]
fn fjall_provider_persists_and_loads_canonical_state() {
    let store = TempStore::new("canonical-state");
    let values_tuple = Tuple::from([
        Value::nothing(),
        Value::bool(true),
        int(42),
        Value::float(12.5).unwrap(),
        Value::identity(rel(99)),
        Value::symbol(Symbol::intern("symbolic")),
        Value::error_code(Symbol::intern("E_PERSIST")),
        Value::string("stored"),
        Value::bytes([0xde, 0xad, 0xbe, 0xef]),
        Value::list([int(1), int(2), int(3)]),
        Value::map([(Value::symbol(Symbol::intern("k")), Value::string("v"))]),
        Value::range(int(1), Some(int(4))),
        Value::error(Symbol::intern("E_RICH"), Some("rich error"), Some(int(7))),
    ]);

    {
        let provider = Arc::new(FjallStateProvider::open_strict(store.path()).unwrap());
        assert_eq!(provider.durability(), FjallDurabilityMode::Strict);
        let kernel = RelationKernel::with_provider(provider.clone());
        kernel
            .create_relation(
                RelationMetadata::new(rel(10), Symbol::intern("ValueTuple"), 13)
                    .with_argument_name(0, Symbol::intern("nothing"))
                    .with_index([2, 0])
                    .with_conflict_policy(ConflictPolicy::Functional {
                        key_positions: vec![2],
                    }),
            )
            .unwrap();
        kernel
            .create_relation(RelationMetadata::new(rel(11), Symbol::intern("Base"), 1))
            .unwrap();
        kernel
            .create_relation(RelationMetadata::new(rel(12), Symbol::intern("Derived"), 1))
            .unwrap();
        kernel
            .create_relation(
                RelationMetadata::new(rel(13), Symbol::intern("VolatileScratch"), 1)
                    .with_durability(RelationDurability::Volatile),
            )
            .unwrap();
        kernel
            .install_rule(
                Rule::new(
                    rel(12),
                    [var("item")],
                    [
                        RuleBodyItem::from(Atom::positive(rel(11), [var("item")])),
                        RuleBodyItem::from(RuleGuard::new(
                            RuleComparisonOp::Ne,
                            var("item"),
                            val(int(88)),
                        )),
                    ],
                ),
                "Derived(item) :- Base(item), item != 88",
            )
            .unwrap();

        let mut tx = kernel.begin();
        tx.assert(rel(10), values_tuple.clone()).unwrap();
        tx.assert(rel(11), Tuple::from([int(77)])).unwrap();
        tx.assert(rel(11), Tuple::from([int(88)])).unwrap();
        let result = tx.commit().unwrap();
        assert_eq!(provider.queued_version(), result.commit().version());
        assert_eq!(provider.completed_version(), result.commit().version());
    }

    assert_eq!(
        FjallStateProvider::check_format(store.path()).unwrap(),
        FjallFormatStatus::Current
    );

    let provider = FjallStateProvider::open(store.path()).unwrap();
    let persisted = provider.load_state().unwrap();
    assert_eq!(persisted.version, 6);
    assert_eq!(provider.load_commits().unwrap().len(), 6);
    let loaded =
        RelationKernel::load_from_state(persisted, Arc::new(InMemoryCommitProvider::new()))
            .unwrap();

    assert_eq!(loaded.snapshot().version(), 6);
    assert_eq!(
        loaded
            .snapshot()
            .relation_metadata()
            .find(|metadata| metadata.id() == rel(13))
            .unwrap()
            .durability(),
        RelationDurability::Volatile
    );
    assert_eq!(
        loaded.snapshot().scan(rel(10), &vec![None; 13]).unwrap(),
        vec![values_tuple]
    );
    assert_eq!(
        loaded.snapshot().scan(rel(12), &[None]).unwrap(),
        vec![Tuple::from([int(77)])]
    );
}

#[cfg(feature = "fjall-provider")]
#[test]
fn fjall_provider_recovers_volatile_relations_without_their_rows() {
    let store = TempStore::new("volatile-recovery");
    {
        let provider = Arc::new(FjallStateProvider::open_strict(store.path()).unwrap());
        let kernel = RelationKernel::with_provider(provider.clone());
        kernel
            .create_relation(RelationMetadata::new(rel(1), Symbol::intern("Durable"), 1))
            .unwrap();
        kernel
            .create_relation(
                RelationMetadata::new(rel(2), Symbol::intern("Volatile"), 1)
                    .with_durability(RelationDurability::Volatile),
            )
            .unwrap();

        let mut mixed = kernel.begin();
        mixed.assert(rel(1), Tuple::from([int(10)])).unwrap();
        mixed.assert(rel(2), Tuple::from([int(20)])).unwrap();
        mixed.commit().unwrap();
        assert_eq!(provider.completed_version(), 3);

        let mut volatile_only = kernel.begin();
        volatile_only
            .assert(rel(2), Tuple::from([int(21)]))
            .unwrap();
        volatile_only.commit().unwrap();
        assert_eq!(kernel.snapshot().version(), 4);
        assert_eq!(provider.completed_version(), 3);

        let mut durable_after_gap = kernel.begin();
        durable_after_gap
            .assert(rel(1), Tuple::from([int(11)]))
            .unwrap();
        durable_after_gap.commit().unwrap();
        assert_eq!(provider.completed_version(), 5);
    }

    let provider = FjallStateProvider::open_strict(store.path()).unwrap();
    let state = provider.load_state().unwrap();
    assert_eq!(state.version, 5);
    assert_eq!(provider.load_commits().unwrap().len(), 4);
    let kernel =
        RelationKernel::load_from_state(state, Arc::new(InMemoryCommitProvider::new())).unwrap();
    assert_eq!(
        kernel.snapshot().scan(rel(1), &[None]).unwrap(),
        vec![Tuple::from([int(10)]), Tuple::from([int(11)])]
    );
    assert!(kernel.snapshot().scan(rel(2), &[None]).unwrap().is_empty());
    assert_eq!(
        kernel
            .snapshot()
            .relation_metadata()
            .find(|metadata| metadata.id() == rel(2))
            .unwrap()
            .durability(),
        RelationDurability::Volatile
    );
}

#[cfg(feature = "fjall-provider")]
#[test]
fn fjall_provider_reopens_loads_and_continues_committing() {
    let store = TempStore::new("reopen-continue");
    let first = Tuple::from([int(1), int(10)]);
    let second = Tuple::from([int(2), int(20)]);

    {
        let provider = Arc::new(FjallStateProvider::open(store.path()).unwrap());
        assert_eq!(provider.durability(), FjallDurabilityMode::Relaxed);
        let kernel = RelationKernel::with_provider(provider);
        kernel
            .create_relation(RelationMetadata::new(
                rel(20),
                Symbol::intern("LocatedIn"),
                2,
            ))
            .unwrap();
        let mut tx = kernel.begin();
        tx.assert(rel(20), first.clone()).unwrap();
        let result = tx.commit().unwrap();
        assert_eq!(result.commit().version(), 2);
    }

    {
        let provider = Arc::new(FjallStateProvider::open(store.path()).unwrap());
        let persisted = provider.load_state().unwrap();
        assert_eq!(persisted.version, 2);
        let kernel = RelationKernel::load_from_state(persisted, provider.clone()).unwrap();
        assert_eq!(
            kernel.snapshot().scan(rel(20), &[None, None]).unwrap(),
            vec![first.clone()]
        );

        let mut tx = kernel.begin();
        tx.assert(rel(20), second.clone()).unwrap();
        let result = tx.commit().unwrap();
        assert_eq!(result.commit().version(), 3);
        assert_eq!(provider.queued_version(), 3);
    }

    let provider = FjallStateProvider::open(store.path()).unwrap();
    let persisted = provider.load_state().unwrap();
    assert_eq!(persisted.version, 3);
    assert_eq!(provider.load_commits().unwrap().len(), 3);
    let loaded =
        RelationKernel::load_from_state(persisted, Arc::new(InMemoryCommitProvider::new()))
            .unwrap();
    assert_eq!(
        loaded.snapshot().scan(rel(20), &[None, None]).unwrap(),
        vec![first, second]
    );
}

#[cfg(feature = "fjall-provider")]
#[test]
fn fjall_provider_detects_shape_mismatch() {
    let store = TempStore::new("format-mismatch");
    assert_eq!(
        FjallStateProvider::check_format(store.path()).unwrap(),
        FjallFormatStatus::Fresh
    );

    {
        let database = fjall::Database::builder(store.path()).open().unwrap();
        let metadata = database
            .keyspace("metadata", fjall::KeyspaceCreateOptions::default)
            .unwrap();
        metadata.insert(b"format_version", b"old-format").unwrap();
        metadata.insert(b"shape", b"old-shape").unwrap();
    }

    assert!(matches!(
        FjallStateProvider::check_format(store.path()).unwrap(),
        FjallFormatStatus::MigrationRequired {
            stored_version: Some(version),
            stored_shape: Some(shape),
            ..
        } if version == "old-format" && shape == "old-shape"
    ));
    assert!(FjallStateProvider::open(store.path()).is_err());
}

#[test]
fn snapshot_neighborhood_views_find_subject_and_mentions() {
    let kernel = kernel_with_located();
    kernel
        .create_relation(RelationMetadata::new(rel(4), Symbol::intern("Tagged"), 3).with_index([2]))
        .unwrap();

    let mut tx = kernel.begin();
    tx.assert(rel(1), Tuple::from([int(1), int(9)])).unwrap();
    tx.assert(
        rel(4),
        Tuple::from([int(2), Value::symbol(Symbol::intern("kind")), int(9)]),
    )
    .unwrap();
    tx.commit().unwrap();
    let snapshot = kernel.snapshot();

    assert_eq!(
        snapshot.subject_facts(&int(1)).unwrap(),
        vec![SubjectFact {
            subject: int(1),
            relation: rel(1),
            tuple: Tuple::from([int(1), int(9)]),
        }]
    );
    assert_eq!(
        snapshot.mentioned_facts(&int(9)).unwrap(),
        vec![
            MentionedFact {
                identity: int(9),
                relation: rel(1),
                position: 1,
                tuple: Tuple::from([int(1), int(9)]),
            },
            MentionedFact {
                identity: int(9),
                relation: rel(4),
                position: 2,
                tuple: Tuple::from([int(2), Value::symbol(Symbol::intern("kind")), int(9)]),
            },
        ]
    );
}

#[test]
fn transaction_neighborhood_views_include_local_overlay() {
    let kernel = kernel_with_located();
    let committed = Tuple::from([int(1), int(2)]);

    let mut seed = kernel.begin();
    seed.assert(rel(1), committed.clone()).unwrap();
    seed.commit().unwrap();

    let mut tx = kernel.begin();
    let local = Tuple::from([int(1), int(3)]);
    tx.retract(rel(1), committed).unwrap();
    tx.assert(rel(1), local.clone()).unwrap();

    assert_eq!(
        tx.subject_facts(&int(1)).unwrap(),
        vec![SubjectFact {
            subject: int(1),
            relation: rel(1),
            tuple: local,
        }]
    );
}
