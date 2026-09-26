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
    AccelerationDecline, AccelerationOutcome, Atom, Commit, ComputedRelation, ComputedRelationRead,
    EqualityJoin, EqualityJoinMatch, ExecutionContext, FactChangeKind, InMemoryCommitProvider,
    KernelError, MembershipSelection, RelationAccelerator, RelationId, RelationKernel,
    RelationMetadata, Rule, RuleBodyItem, RuleComparisonOp, RuleGuard, RuleSet, Term, Tuple,
};
use mica_var::{Identity, Symbol, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::Duration;

fn rel(id: u64) -> RelationId {
    Identity::new(id).unwrap()
}

fn int(value: i64) -> Value {
    Value::int(value).unwrap()
}

fn var(name: &str) -> Term {
    Term::Var(Symbol::intern(name))
}

fn val(value: Value) -> Term {
    Term::Value(value)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct OracleEpoch {
    version: u64,
    input_changes: usize,
    affected_rule_components: usize,
    candidate_changes: usize,
    consolidated_changes: usize,
    visible_output_changes: usize,
    fixpoint_iterations: usize,
    frontier_rows: Vec<usize>,
    elapsed: Duration,
    derived: BTreeMap<RelationId, Vec<Tuple>>,
    visible: BTreeMap<RelationId, Vec<Tuple>>,
}

struct CompleteOracle {
    head_arities: BTreeMap<RelationId, usize>,
    previous_visible: BTreeMap<RelationId, Vec<Tuple>>,
    epochs: Vec<OracleEpoch>,
}

impl CompleteOracle {
    fn new(head_arities: impl IntoIterator<Item = (RelationId, usize)>) -> Self {
        Self {
            head_arities: head_arities.into_iter().collect(),
            previous_visible: BTreeMap::new(),
            epochs: Vec::new(),
        }
    }

    fn capture(&mut self, kernel: &RelationKernel, commit: &Commit) {
        let snapshot = kernel.snapshot();
        assert_eq!(snapshot.version(), commit.version());
        let evaluation = snapshot
            .evaluate_complete_rules(&ExecutionContext::serial())
            .unwrap();
        let rules = RuleSet::new(
            snapshot
                .rules()
                .iter()
                .filter(|rule| rule.active())
                .map(|rule| rule.rule().clone()),
        );
        let affected_rule_components = rules
            .affected_component_count(commit.changes().iter().map(|change| change.relation))
            .unwrap();
        let visible = self
            .head_arities
            .iter()
            .map(|(relation, arity)| {
                (
                    *relation,
                    snapshot.scan(*relation, &vec![None; *arity]).unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let visible_output_changes = visible_change_count(&self.previous_visible, &visible);
        self.previous_visible.clone_from(&visible);
        self.epochs.push(OracleEpoch {
            version: snapshot.version(),
            input_changes: commit.changes().len(),
            affected_rule_components,
            candidate_changes: evaluation.stats.candidate_rows,
            consolidated_changes: evaluation.stats.novel_rows,
            visible_output_changes,
            fixpoint_iterations: evaluation.stats.rounds,
            frontier_rows: evaluation.stats.frontier_rows,
            elapsed: evaluation.stats.elapsed,
            derived: evaluation.derived,
            visible,
        });
    }
}

fn visible_change_count(
    previous: &BTreeMap<RelationId, Vec<Tuple>>,
    next: &BTreeMap<RelationId, Vec<Tuple>>,
) -> usize {
    previous
        .keys()
        .chain(next.keys())
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|relation| {
            let previous = previous
                .get(&relation)
                .into_iter()
                .flatten()
                .collect::<BTreeSet<_>>();
            let next = next
                .get(&relation)
                .into_iter()
                .flatten()
                .collect::<BTreeSet<_>>();
            previous.symmetric_difference(&next).count()
        })
        .sum()
}

fn create_relations(kernel: &RelationKernel, relations: &[(u64, &str, u16)]) {
    for (id, name, arity) in relations {
        kernel
            .create_relation(RelationMetadata::new(
                rel(*id),
                Symbol::intern(name),
                *arity,
            ))
            .unwrap();
    }
}

fn assert_rows(kernel: &RelationKernel, relation: RelationId, rows: &[(i64, i64)]) -> Commit {
    let mut tx = kernel.begin();
    for (left, right) in rows {
        tx.assert(relation, Tuple::from([int(*left), int(*right)]))
            .unwrap();
    }
    tx.commit().unwrap().commit().clone()
}

fn retract_rows(kernel: &RelationKernel, relation: RelationId, rows: &[(i64, i64)]) -> Commit {
    let mut tx = kernel.begin();
    for (left, right) in rows {
        tx.retract(relation, Tuple::from([int(*left), int(*right)]))
            .unwrap();
    }
    tx.commit().unwrap().commit().clone()
}

fn derived_rows(snapshot: &crate::Snapshot, relation: RelationId, arity: usize) -> Vec<Tuple> {
    snapshot
        .maintained_state()
        .expect("snapshot should retain eligible maintained state")
        .build_derived_relations(snapshot)
        .unwrap()
        .get(&relation)
        .map(|state| state.scan(&vec![None; arity]).unwrap())
        .unwrap_or_default()
}

fn assert_maintained_matches_complete(
    snapshot: &crate::Snapshot,
    head_arities: &[(RelationId, usize)],
) {
    let maintained = snapshot
        .maintained_state()
        .expect("eligible snapshot should be maintained");
    assert_eq!(maintained.version(), snapshot.version());
    let complete = snapshot
        .evaluate_complete_rules(&ExecutionContext::serial())
        .unwrap()
        .derived;
    for (relation, arity) in head_arities {
        assert_eq!(
            derived_rows(snapshot, *relation, *arity),
            complete.get(relation).cloned().unwrap_or_default(),
            "derived relation #{relation:?} diverged at version {}",
            snapshot.version()
        );
    }
}

fn visible_rows(
    snapshot: &crate::Snapshot,
    relation_arities: &[(RelationId, usize)],
) -> BTreeMap<RelationId, BTreeSet<Tuple>> {
    relation_arities
        .iter()
        .map(|(relation, arity)| {
            (
                *relation,
                snapshot
                    .scan(*relation, &vec![None; *arity])
                    .unwrap()
                    .into_iter()
                    .collect(),
            )
        })
        .collect()
}

fn apply_visible_changes(
    mut visible: BTreeMap<RelationId, BTreeSet<Tuple>>,
    changes: &[crate::FactChange],
) -> BTreeMap<RelationId, BTreeSet<Tuple>> {
    for change in changes {
        let rows = visible.entry(change.relation).or_default();
        match change.kind {
            FactChangeKind::Assert => assert!(rows.insert(change.tuple.clone())),
            FactChangeKind::Retract => assert!(rows.remove(&change.tuple)),
        }
    }
    visible
}

#[test]
fn complete_oracle_captures_cyclic_commit_sequences_and_work() {
    let kernel = RelationKernel::new();
    create_relations(&kernel, &[(300, "Edge", 2), (301, "Reachable", 2)]);
    kernel
        .install_rule(
            Rule::new(
                rel(301),
                [var("from"), var("to")],
                [Atom::positive(rel(300), [var("from"), var("to")])],
            ),
            "Reachable(from, to) :- Edge(from, to)",
        )
        .unwrap();
    kernel
        .install_rule(
            Rule::new(
                rel(301),
                [var("from"), var("to")],
                [
                    Atom::positive(rel(301), [var("from"), var("middle")]),
                    Atom::positive(rel(300), [var("middle"), var("to")]),
                ],
            ),
            "Reachable(from, to) :- Reachable(from, middle), Edge(middle, to)",
        )
        .unwrap();

    let mut oracle = CompleteOracle::new([(rel(301), 2)]);
    let commit = assert_rows(&kernel, rel(300), &[(1, 2), (2, 3), (3, 1)]);
    oracle.capture(&kernel, &commit);
    let commit = retract_rows(&kernel, rel(300), &[(3, 1)]);
    oracle.capture(&kernel, &commit);

    assert_eq!(oracle.epochs.len(), 2);
    assert_eq!(oracle.epochs[0].input_changes, 3);
    assert_eq!(oracle.epochs[0].affected_rule_components, 1);
    assert!(oracle.epochs[0].candidate_changes >= 9);
    assert_eq!(oracle.epochs[0].consolidated_changes, 9);
    assert_eq!(oracle.epochs[0].visible_output_changes, 9);
    assert!(oracle.epochs[0].fixpoint_iterations >= 2);
    assert!(!oracle.epochs[0].frontier_rows.is_empty());
    assert_eq!(oracle.epochs[1].input_changes, 1);
    assert_eq!(oracle.epochs[1].visible_output_changes, 6);
    assert_eq!(
        oracle.epochs[1].visible[&rel(301)],
        vec![
            Tuple::from([int(1), int(2)]),
            Tuple::from([int(1), int(3)]),
            Tuple::from([int(2), int(3)]),
        ]
    );
}

#[test]
fn complete_oracle_captures_multi_proof_zero_crossings() {
    let kernel = RelationKernel::new();
    create_relations(
        &kernel,
        &[
            (310, "Left", 2),
            (311, "Right", 2),
            (312, "Joined", 2),
            (313, "Unrelated", 1),
        ],
    );
    kernel
        .install_rule(
            Rule::new(
                rel(312),
                [var("from"), var("to")],
                [
                    Atom::positive(rel(310), [var("from"), var("middle")]),
                    Atom::positive(rel(311), [var("middle"), var("to")]),
                ],
            ),
            "Joined(from, to) :- Left(from, middle), Right(middle, to)",
        )
        .unwrap();
    assert_rows(&kernel, rel(310), &[(1, 2), (1, 3)]);

    let mut oracle = CompleteOracle::new([(rel(312), 2)]);
    let commit = assert_rows(&kernel, rel(311), &[(2, 4), (3, 4)]);
    oracle.capture(&kernel, &commit);
    let commit = retract_rows(&kernel, rel(311), &[(2, 4)]);
    oracle.capture(&kernel, &commit);
    let commit = retract_rows(&kernel, rel(311), &[(3, 4)]);
    oracle.capture(&kernel, &commit);
    let mut tx = kernel.begin();
    tx.assert(rel(313), Tuple::from([int(9)])).unwrap();
    let commit = tx.commit().unwrap().commit().clone();
    oracle.capture(&kernel, &commit);

    assert_eq!(oracle.epochs[0].visible_output_changes, 1);
    assert_eq!(oracle.epochs[0].candidate_changes, 1);
    assert_eq!(oracle.epochs[1].visible_output_changes, 0);
    assert_eq!(oracle.epochs[2].visible_output_changes, 1);
    assert!(oracle.epochs[2].visible[&rel(312)].is_empty());
    assert_eq!(oracle.epochs[3].affected_rule_components, 0);
    assert_eq!(oracle.epochs[3].visible_output_changes, 0);
}

#[test]
fn complete_oracle_captures_negation_changes_from_both_sides() {
    let kernel = RelationKernel::new();
    create_relations(
        &kernel,
        &[(320, "Node", 1), (321, "Blocked", 1), (322, "Visible", 1)],
    );
    kernel
        .install_rule(
            Rule::new(
                rel(322),
                [var("node")],
                [
                    RuleBodyItem::from(Atom::positive(rel(320), [var("node")])),
                    RuleBodyItem::from(Atom::negated(rel(321), [var("node")])),
                ],
            ),
            "Visible(node) :- Node(node), !Blocked(node)",
        )
        .unwrap();

    let mut oracle = CompleteOracle::new([(rel(322), 1)]);
    let mut tx = kernel.begin();
    tx.assert(rel(320), Tuple::from([int(1)])).unwrap();
    tx.assert(rel(320), Tuple::from([int(2)])).unwrap();
    let commit = tx.commit().unwrap().commit().clone();
    oracle.capture(&kernel, &commit);
    let mut tx = kernel.begin();
    tx.assert(rel(321), Tuple::from([int(2)])).unwrap();
    let commit = tx.commit().unwrap().commit().clone();
    oracle.capture(&kernel, &commit);
    let mut tx = kernel.begin();
    tx.retract(rel(321), Tuple::from([int(2)])).unwrap();
    let commit = tx.commit().unwrap().commit().clone();
    oracle.capture(&kernel, &commit);

    assert_eq!(
        oracle
            .epochs
            .iter()
            .map(|epoch| epoch.visible_output_changes)
            .collect::<Vec<_>>(),
        vec![2, 1, 1]
    );
    assert!(
        oracle
            .epochs
            .iter()
            .all(|epoch| epoch.fixpoint_iterations == 0)
    );
}

#[test]
fn complete_oracle_distinguishes_extensional_and_derived_visibility() {
    let kernel = RelationKernel::new();
    create_relations(&kernel, &[(330, "Source", 2), (331, "Combined", 2)]);
    kernel
        .install_rule(
            Rule::new(
                rel(331),
                [var("left"), var("right")],
                [Atom::positive(rel(330), [var("left"), var("right")])],
            ),
            "Combined(left, right) :- Source(left, right)",
        )
        .unwrap();

    let mut oracle = CompleteOracle::new([(rel(331), 2)]);
    let mut tx = kernel.begin();
    tx.assert(rel(330), Tuple::from([int(1), int(2)])).unwrap();
    tx.assert(rel(331), Tuple::from([int(1), int(2)])).unwrap();
    let commit = tx.commit().unwrap().commit().clone();
    oracle.capture(&kernel, &commit);
    let commit = retract_rows(&kernel, rel(330), &[(1, 2)]);
    oracle.capture(&kernel, &commit);
    let commit = retract_rows(&kernel, rel(331), &[(1, 2)]);
    oracle.capture(&kernel, &commit);

    assert_eq!(oracle.epochs[0].visible_output_changes, 1);
    assert_eq!(oracle.epochs[1].visible_output_changes, 0);
    assert!(
        oracle.epochs[1]
            .derived
            .get(&rel(331))
            .is_none_or(Vec::is_empty)
    );
    assert_eq!(oracle.epochs[2].visible_output_changes, 1);
    assert!(oracle.epochs[2].visible[&rel(331)].is_empty());
    assert_eq!(oracle.epochs[2].input_changes, 1);
    assert_eq!(oracle.epochs[2].affected_rule_components, 1);
    assert_eq!(oracle.epochs[2].candidate_changes, 0);
    assert_eq!(oracle.epochs[2].consolidated_changes, 0);
    assert_eq!(oracle.epochs[2].fixpoint_iterations, 0);
    assert_eq!(oracle.epochs[2].frontier_rows, Vec::<usize>::new());
    let _measured_elapsed = oracle.epochs[2].elapsed;
    assert_eq!(oracle.epochs[2].version, commit.version());
    assert_eq!(commit.changes()[0].kind, FactChangeKind::Retract);
}

#[test]
fn nonrecursive_positive_maintenance_matches_randomized_complete_recomputation() {
    let kernel = RelationKernel::new();
    create_relations(
        &kernel,
        &[
            (400, "Left", 2),
            (401, "Right", 2),
            (402, "Joined", 2),
            (403, "Projected", 1),
            (404, "Unrelated", 1),
        ],
    );
    kernel
        .install_rule(
            Rule::new(
                rel(402),
                [var("from"), var("to")],
                vec![
                    RuleBodyItem::from(RuleGuard::new(
                        RuleComparisonOp::Lt,
                        var("from"),
                        var("to"),
                    )),
                    RuleBodyItem::from(Atom::positive(rel(400), [var("from"), var("middle")])),
                    RuleBodyItem::from(Atom::positive(rel(401), [var("middle"), var("to")])),
                ],
            ),
            "Joined(from, to) :- from < to, Left(from, middle), Right(middle, to)",
        )
        .unwrap();
    kernel
        .install_rule(
            Rule::new(
                rel(403),
                [var("from")],
                [Atom::positive(rel(402), [var("from"), var("to")])],
            ),
            "Projected(from) :- Joined(from, to)",
        )
        .unwrap();
    kernel
        .install_rule(
            Rule::new(
                rel(403),
                [var("from")],
                [Atom::positive(rel(400), [var("from"), val(int(0))])],
            ),
            "Projected(from) :- Left(from, 0)",
        )
        .unwrap();

    let mut left = BTreeSet::from([(0, 0), (1, 1), (2, 2)]);
    let mut right = BTreeSet::from([(0, 3), (1, 3), (2, 4)]);
    let mut seed = kernel.begin();
    for (from, middle) in &left {
        seed.assert(rel(400), Tuple::from([int(*from), int(*middle)]))
            .unwrap();
    }
    for (middle, to) in &right {
        seed.assert(rel(401), Tuple::from([int(*middle), int(*to)]))
            .unwrap();
    }
    seed.commit().unwrap();

    let relation_arities = [(rel(400), 2), (rel(401), 2), (rel(402), 2), (rel(403), 1)];
    let mut previous = kernel.snapshot();
    assert_eq!(
        previous.scan(rel(403), &[None]).unwrap(),
        vec![
            Tuple::from([int(0)]),
            Tuple::from([int(1)]),
            Tuple::from([int(2)])
        ]
    );
    assert_maintained_matches_complete(&previous, &[(rel(402), 2), (rel(403), 1)]);
    let retained = Arc::clone(&previous);

    let mut random = 0x9e37_79b9_7f4a_7c15_u64;
    for _ in 0..256 {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let relation = if random & 1 == 0 { rel(400) } else { rel(401) };
        let tuple = (((random >> 8) % 5) as i64, ((random >> 16) % 5) as i64);
        let set = if relation == rel(400) {
            &mut left
        } else {
            &mut right
        };
        let mut tx = kernel.begin();
        if set.remove(&tuple) {
            tx.retract(relation, Tuple::from([int(tuple.0), int(tuple.1)]))
                .unwrap();
        } else {
            set.insert(tuple);
            tx.assert(relation, Tuple::from([int(tuple.0), int(tuple.1)]))
                .unwrap();
        }
        tx.commit().unwrap();

        let next = kernel.snapshot();
        assert_maintained_matches_complete(&next, &[(rel(402), 2), (rel(403), 1)]);
        let expected_visible = visible_rows(&next, &relation_arities);
        let transformed = apply_visible_changes(
            visible_rows(&previous, &relation_arities),
            next.maintained_state().unwrap().visible_changes(),
        );
        assert_eq!(transformed, expected_visible);
        assert_eq!(next.maintained_state().unwrap().work().input_changes, 1);
        assert!((1..=2).contains(&next.maintained_state().unwrap().work().affected_components));
        previous = next;
    }

    assert_eq!(
        retained.scan(rel(403), &[None]).unwrap(),
        vec![
            Tuple::from([int(0)]),
            Tuple::from([int(1)]),
            Tuple::from([int(2)])
        ]
    );

    let before_unrelated = kernel.snapshot();
    let mut tx = kernel.begin();
    tx.assert(rel(404), Tuple::from([int(99)])).unwrap();
    tx.commit().unwrap();
    let after_unrelated = kernel.snapshot();
    assert_maintained_matches_complete(&after_unrelated, &[(rel(402), 2), (rel(403), 1)]);
    assert_eq!(
        after_unrelated
            .maintained_state()
            .unwrap()
            .work()
            .affected_components,
        0
    );
    assert!(
        after_unrelated
            .maintained_state()
            .unwrap()
            .visible_changes()
            .is_empty()
    );
    assert_eq!(
        before_unrelated.scan(rel(403), &[None]).unwrap(),
        after_unrelated.scan(rel(403), &[None]).unwrap()
    );
}

#[test]
fn nonrecursive_maintenance_preserves_multiple_and_extensional_supports() {
    let kernel = RelationKernel::new();
    create_relations(
        &kernel,
        &[(410, "Left", 2), (411, "Right", 2), (412, "Output", 2)],
    );
    kernel
        .install_rule(
            Rule::new(
                rel(412),
                [var("from"), var("to")],
                [
                    Atom::positive(rel(410), [var("from"), var("middle")]),
                    Atom::positive(rel(411), [var("middle"), var("to")]),
                ],
            ),
            "Output(from, to) :- Left(from, middle), Right(middle, to)",
        )
        .unwrap();
    assert_rows(&kernel, rel(410), &[(1, 2), (1, 3)]);
    assert_rows(&kernel, rel(411), &[(2, 4), (3, 4)]);
    assert_rows(&kernel, rel(412), &[(1, 4)]);

    let snapshot = kernel.snapshot();
    assert_eq!(
        snapshot.scan(rel(412), &[None, None]).unwrap(),
        vec![Tuple::from([int(1), int(4)])]
    );
    assert_maintained_matches_complete(&snapshot, &[(rel(412), 2)]);

    retract_rows(&kernel, rel(411), &[(2, 4)]);
    let snapshot = kernel.snapshot();
    assert_maintained_matches_complete(&snapshot, &[(rel(412), 2)]);
    assert!(
        !snapshot
            .maintained_state()
            .unwrap()
            .visible_changes()
            .iter()
            .any(|change| change.relation == rel(412))
    );

    retract_rows(&kernel, rel(411), &[(3, 4)]);
    let snapshot = kernel.snapshot();
    assert_maintained_matches_complete(&snapshot, &[(rel(412), 2)]);
    assert!(derived_rows(&snapshot, rel(412), 2).is_empty());
    assert_eq!(
        snapshot.scan(rel(412), &[None, None]).unwrap(),
        vec![Tuple::from([int(1), int(4)])]
    );
    assert!(
        !snapshot
            .maintained_state()
            .unwrap()
            .visible_changes()
            .iter()
            .any(|change| change.relation == rel(412))
    );

    retract_rows(&kernel, rel(412), &[(1, 4)]);
    let snapshot = kernel.snapshot();
    assert_maintained_matches_complete(&snapshot, &[(rel(412), 2)]);
    assert!(snapshot.scan(rel(412), &[None, None]).unwrap().is_empty());
    assert!(
        snapshot
            .maintained_state()
            .unwrap()
            .visible_changes()
            .iter()
            .any(|change| {
                change.relation == rel(412) && change.kind == FactChangeKind::Retract
            })
    );
}

#[test]
fn warm_state_is_scoped_to_requested_dependency_components() {
    let kernel = RelationKernel::new();
    create_relations(
        &kernel,
        &[
            (460, "BaseA", 1),
            (461, "OutputA", 1),
            (462, "BaseB", 1),
            (463, "OutputB", 1),
        ],
    );
    for (base, output, name) in [
        (rel(460), rel(461), "OutputA"),
        (rel(462), rel(463), "OutputB"),
    ] {
        kernel
            .install_rule(
                Rule::new(
                    output,
                    [var("value")],
                    [Atom::positive(base, [var("value")])],
                ),
                format!("{name}(value) :- Base(value)"),
            )
            .unwrap();
    }
    let mut seed = kernel.begin();
    seed.assert(rel(460), Tuple::from([int(1)])).unwrap();
    seed.assert(rel(462), Tuple::from([int(2)])).unwrap();
    seed.commit().unwrap();

    let snapshot = kernel.snapshot();
    assert_eq!(
        snapshot.scan(rel(461), &[None]).unwrap(),
        vec![Tuple::from([int(1)])]
    );
    let maintained = snapshot.maintained_state().unwrap();
    assert!(maintained.serves(rel(461)));
    assert!(!maintained.serves(rel(463)));
    assert_eq!(maintained.requested_targets(), &BTreeSet::from([rel(461)]));

    let mut tx = kernel.begin();
    tx.assert(rel(462), Tuple::from([int(3)])).unwrap();
    tx.commit().unwrap();
    let snapshot = kernel.snapshot();
    assert_eq!(
        snapshot
            .maintained_state()
            .unwrap()
            .work()
            .affected_components,
        0
    );
    assert_eq!(
        snapshot.scan(rel(461), &[None]).unwrap(),
        vec![Tuple::from([int(1)])]
    );
    assert!(!snapshot.maintained_state().unwrap().serves(rel(463)));

    assert_eq!(
        snapshot.scan(rel(463), &[None]).unwrap(),
        vec![Tuple::from([int(2)]), Tuple::from([int(3)])]
    );
    let maintained = snapshot.maintained_state().unwrap();
    assert!(maintained.serves(rel(461)));
    assert!(maintained.serves(rel(463)));
    assert_eq!(
        maintained.requested_targets(),
        &BTreeSet::from([rel(461), rel(463)])
    );
}

#[test]
fn repeated_small_commits_reuse_shared_join_arrangements() {
    let kernel = RelationKernel::new();
    create_relations(
        &kernel,
        &[
            (470, "Left", 2),
            (471, "Right", 2),
            (472, "OutputA", 2),
            (473, "OutputB", 2),
        ],
    );
    for (output, name) in [(rel(472), "OutputA"), (rel(473), "OutputB")] {
        kernel
            .install_rule(
                Rule::new(
                    output,
                    [var("from"), var("to")],
                    [
                        Atom::positive(rel(470), [var("from"), var("key")]),
                        Atom::positive(rel(471), [var("key"), var("to")]),
                    ],
                ),
                format!("{name}(from, to) :- Left(from, key), Right(key, to)"),
            )
            .unwrap();
    }
    let row_count = 256_i64;
    let mut seed = kernel.begin();
    for key in 0..row_count {
        seed.assert(rel(470), Tuple::from([int(key), int(key)]))
            .unwrap();
        seed.assert(rel(471), Tuple::from([int(key), int(key + 10_000)]))
            .unwrap();
    }
    seed.commit().unwrap();

    let snapshot = kernel.snapshot();
    assert_eq!(snapshot.scan(rel(472), &[None, None]).unwrap().len(), 256);
    assert_eq!(snapshot.scan(rel(473), &[None, None]).unwrap().len(), 256);
    assert_eq!(snapshot.maintained_state().unwrap().arrangement_count(), 2);

    let mut total_rows_visited = 0;
    let mut total_complete_input_rows = 0;
    for commit_index in 0..32 {
        let mut tx = kernel.begin();
        let tuple = Tuple::from([int(0), int(10_000)]);
        if commit_index % 2 == 0 {
            tx.retract(rel(471), tuple).unwrap();
        } else {
            tx.assert(rel(471), tuple).unwrap();
        }
        tx.commit().unwrap();
        let snapshot = kernel.snapshot();
        let maintained = snapshot.maintained_state().unwrap();
        let work = maintained.work();
        assert_eq!(work.arrangement_lookups, 2);
        assert_eq!(work.rows_visited, 4);
        total_rows_visited += work.rows_visited;
        total_complete_input_rows += 2 * (256 + if commit_index % 2 == 0 { 255 } else { 256 });
    }

    assert!(total_rows_visited < total_complete_input_rows / 100);
    let snapshot = kernel.snapshot();
    assert_maintained_matches_complete(&snapshot, &[(rel(472), 2), (rel(473), 2)]);
}

#[test]
fn immutable_trace_batches_compact_without_changing_old_snapshots() {
    let kernel = RelationKernel::new();
    create_relations(&kernel, &[(480, "Base", 1), (481, "Copy", 1)]);
    kernel
        .install_rule(
            Rule::new(
                rel(481),
                [var("value")],
                [Atom::positive(rel(480), [var("value")])],
            ),
            "Copy(value) :- Base(value)",
        )
        .unwrap();
    let mut seed = kernel.begin();
    for value in 0..1_000 {
        seed.assert(rel(480), Tuple::from([int(value)])).unwrap();
    }
    seed.commit().unwrap();

    let retained = kernel.snapshot();
    assert_eq!(retained.scan(rel(481), &[None]).unwrap().len(), 1_000);
    assert_eq!(
        retained
            .maintained_state()
            .unwrap()
            .trace_batch_count(rel(480)),
        Some(1)
    );

    for offset in 0..7 {
        let mut tx = kernel.begin();
        tx.assert(rel(480), Tuple::from([int(2_000 + offset)]))
            .unwrap();
        tx.commit().unwrap();
    }
    let before_compaction = kernel.snapshot();
    assert_eq!(
        before_compaction
            .maintained_state()
            .unwrap()
            .trace_batch_count(rel(480)),
        Some(8)
    );

    let mut tx = kernel.begin();
    tx.assert(rel(480), Tuple::from([int(2_007)])).unwrap();
    tx.commit().unwrap();
    let after_batch_compaction = kernel.snapshot();
    let maintained = after_batch_compaction.maintained_state().unwrap();
    assert_eq!(maintained.trace_batch_count(rel(480)), Some(1));
    assert!(maintained.work().compaction_rows >= 2_016);

    let mut tx = kernel.begin();
    for value in 3_000..3_252 {
        tx.assert(rel(480), Tuple::from([int(value)])).unwrap();
    }
    tx.commit().unwrap();
    let after_size_compaction = kernel.snapshot();
    let maintained = after_size_compaction.maintained_state().unwrap();
    assert_eq!(maintained.trace_batch_count(rel(480)), Some(1));
    assert!(maintained.work().compaction_rows >= 2_520);

    assert_eq!(retained.scan(rel(481), &[None]).unwrap().len(), 1_000);
    assert_eq!(
        retained
            .maintained_state()
            .unwrap()
            .trace_batch_count(rel(480)),
        Some(1)
    );
    assert_eq!(
        before_compaction.scan(rel(481), &[None]).unwrap().len(),
        1_007
    );
}

struct PrefixJoinAccelerator {
    calls: AtomicUsize,
}

impl RelationAccelerator for PrefixJoinAccelerator {
    fn select_membership(
        &self,
        _selection: MembershipSelection<'_>,
    ) -> AccelerationOutcome<Vec<usize>> {
        AccelerationOutcome::Declined(AccelerationDecline::UnsupportedInput)
    }

    fn join_equality(&self, join: EqualityJoin<'_>) -> AccelerationOutcome<Vec<EqualityJoinMatch>> {
        self.calls.fetch_add(1, AtomicOrdering::Relaxed);
        assert_eq!(join.left.len(), 1);
        assert_eq!(join.right.len(), 1);
        assert!(join.right[0].len() >= join.left[0].len());
        assert!(
            join.left[0]
                .iter()
                .zip(join.right[0].iter())
                .all(|(left, right)| left == right)
        );
        AccelerationOutcome::Completed(
            (0..join.left[0].len())
                .map(|row| EqualityJoinMatch {
                    left_row: row,
                    right_row: row,
                })
                .collect(),
        )
    }
}

#[test]
#[ignore = "large weighted packed maintenance placement"]
fn configured_accelerator_executes_large_weighted_trace_join() {
    let accelerator = Arc::new(PrefixJoinAccelerator {
        calls: AtomicUsize::new(0),
    });
    let kernel = RelationKernel::new().with_execution_context(
        ExecutionContext::serial()
            .with_accelerator(accelerator.clone())
            .with_weighted_join_acceleration(),
    );
    create_relations(
        &kernel,
        &[(550, "Left", 2), (551, "Right", 2), (552, "Joined", 2)],
    );
    kernel
        .install_rule(
            Rule::new(
                rel(552),
                [var("from"), var("to")],
                [
                    Atom::positive(rel(550), [var("from"), var("key")]),
                    Atom::positive(rel(551), [var("key"), var("to")]),
                ],
            ),
            "Joined(from, to) :- Left(from, key), Right(key, to)",
        )
        .unwrap();
    let mut seed = kernel.begin();
    for key in 0..258_048_i64 {
        seed.assert(rel(550), Tuple::from([int(key), int(key)]))
            .unwrap();
    }
    seed.commit().unwrap();
    assert!(
        kernel
            .snapshot()
            .scan(rel(552), &[None, None])
            .unwrap()
            .is_empty()
    );

    let mut tx = kernel.begin();
    for key in 0..4_096_i64 {
        tx.assert(rel(551), Tuple::from([int(key), int(key + 1_000_000)]))
            .unwrap();
    }
    tx.commit().unwrap();
    let rows = kernel.snapshot().scan(rel(552), &[None, None]).unwrap();
    assert_eq!(rows.len(), 4_096);
    assert_eq!(rows.first(), Some(&Tuple::from([int(0), int(1_000_000)])));
    assert_eq!(
        rows.last(),
        Some(&Tuple::from([int(4_095), int(1_004_095)]))
    );
    assert_eq!(accelerator.calls.load(AtomicOrdering::Relaxed), 1);
}

struct ConstantComputed;

impl ComputedRelation for ConstantComputed {
    fn name(&self) -> &'static str {
        "constant"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("Computed")
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        &[]
    }

    fn scan(
        &self,
        _reader: &dyn ComputedRelationRead,
        _metadata: &RelationMetadata,
        _bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        Ok(vec![Tuple::from([int(7)])])
    }
}

#[test]
fn unsupported_programs_and_dirty_transactions_use_complete_fallback() {
    let recursive = RelationKernel::new();
    create_relations(&recursive, &[(420, "Edge", 2), (421, "Reachable", 2)]);
    recursive
        .install_rule(
            Rule::new(
                rel(421),
                [var("from"), var("to")],
                [Atom::positive(rel(420), [var("from"), var("to")])],
            ),
            "Reachable(from, to) :- Edge(from, to)",
        )
        .unwrap();
    recursive
        .install_rule(
            Rule::new(
                rel(421),
                [var("from"), var("to")],
                [
                    Atom::positive(rel(421), [var("from"), var("middle")]),
                    Atom::positive(rel(420), [var("middle"), var("to")]),
                ],
            ),
            "Reachable(from, to) :- Reachable(from, middle), Edge(middle, to)",
        )
        .unwrap();
    assert_rows(&recursive, rel(420), &[(1, 2), (2, 3)]);
    let snapshot = recursive.snapshot();
    assert_eq!(snapshot.scan(rel(421), &[None, None]).unwrap().len(), 3);
    assert!(snapshot.maintained_state().is_some());

    let negated = RelationKernel::new();
    create_relations(
        &negated,
        &[(430, "Node", 1), (431, "Blocked", 1), (432, "Visible", 1)],
    );
    negated
        .install_rule(
            Rule::new(
                rel(432),
                [var("node")],
                [
                    Atom::positive(rel(430), [var("node")]),
                    Atom::negated(rel(431), [var("node")]),
                ],
            ),
            "Visible(node) :- Node(node), !Blocked(node)",
        )
        .unwrap();
    let mut tx = negated.begin();
    tx.assert(rel(430), Tuple::from([int(1)])).unwrap();
    tx.commit().unwrap();
    let snapshot = negated.snapshot();
    assert_eq!(snapshot.scan(rel(432), &[None]).unwrap().len(), 1);
    assert!(snapshot.maintained_state().is_some());

    let computed = RelationKernel::with_provider_and_computed_relations(
        Arc::new(InMemoryCommitProvider::new()),
        [Arc::new(ConstantComputed) as Arc<dyn ComputedRelation>],
    );
    create_relations(&computed, &[(440, "Computed", 1), (441, "Copy", 1)]);
    computed
        .install_rule(
            Rule::new(
                rel(441),
                [var("value")],
                [Atom::positive(rel(440), [var("value")])],
            ),
            "Copy(value) :- Computed(value)",
        )
        .unwrap();
    let snapshot = computed.snapshot();
    assert_eq!(
        snapshot.scan(rel(441), &[None]).unwrap(),
        vec![Tuple::from([int(7)])]
    );
    assert!(snapshot.maintained_state().is_none());

    let eligible = RelationKernel::new();
    create_relations(&eligible, &[(450, "Base", 1), (451, "Copy", 1)]);
    eligible
        .install_rule(
            Rule::new(
                rel(451),
                [var("value")],
                [Atom::positive(rel(450), [var("value")])],
            ),
            "Copy(value) :- Base(value)",
        )
        .unwrap();
    let snapshot = eligible.snapshot();
    assert!(snapshot.scan(rel(451), &[None]).unwrap().is_empty());
    assert!(snapshot.maintained_state().is_some());
    let mut tx = eligible.begin();
    tx.assert(rel(450), Tuple::from([int(11)])).unwrap();
    assert_eq!(
        tx.scan(rel(451), &[None]).unwrap(),
        vec![Tuple::from([int(11)])]
    );
    tx.commit().unwrap();
    assert_maintained_matches_complete(&eligible.snapshot(), &[(rel(451), 1)]);

    let added = eligible
        .install_rule(
            Rule::new(
                rel(451),
                [var("value")],
                [Atom::positive(rel(450), [var("value")])],
            ),
            "Copy(value) :- Base(value)",
        )
        .unwrap();
    assert!(eligible.snapshot().maintained_state().is_some());
    assert_eq!(
        eligible.snapshot().scan(rel(451), &[None]).unwrap(),
        vec![Tuple::from([int(11)])]
    );
    assert_maintained_matches_complete(&eligible.snapshot(), &[(rel(451), 1)]);

    eligible.disable_rule(added.id()).unwrap();
    assert!(eligible.snapshot().maintained_state().is_some());
    assert_eq!(
        eligible.snapshot().scan(rel(451), &[None]).unwrap(),
        vec![Tuple::from([int(11)])]
    );
    assert_maintained_matches_complete(&eligible.snapshot(), &[(rel(451), 1)]);
}

#[test]
fn stratified_negation_tracks_right_zero_crossings_and_duplicate_left_proofs() {
    let kernel = RelationKernel::new();
    create_relations(
        &kernel,
        &[
            (490, "LeftA", 2),
            (491, "LeftB", 2),
            (492, "BlockOne", 1),
            (493, "BlockTwo", 1),
            (494, "Blocked", 1),
            (495, "Hidden", 1),
            (496, "Visible", 1),
        ],
    );
    for (source, name) in [(rel(492), "BlockOne"), (rel(493), "BlockTwo")] {
        kernel
            .install_rule(
                Rule::new(
                    rel(494),
                    [var("key")],
                    [Atom::positive(source, [var("key")])],
                ),
                format!("Blocked(key) :- {name}(key)"),
            )
            .unwrap();
    }
    for (left, name) in [(rel(490), "LeftA"), (rel(491), "LeftB")] {
        kernel
            .install_rule(
                Rule::new(
                    rel(496),
                    [var("item")],
                    [
                        Atom::positive(left, [var("item"), var("key")]),
                        Atom::negated(rel(494), [var("key")]),
                        Atom::negated(rel(495), [var("item")]),
                    ],
                ),
                format!("Visible(item) :- {name}(item, key), !Blocked(key), !Hidden(item)"),
            )
            .unwrap();
    }
    let mut seed = kernel.begin();
    seed.assert(rel(490), Tuple::from([int(1), int(10)]))
        .unwrap();
    seed.assert(rel(491), Tuple::from([int(1), int(10)]))
        .unwrap();
    seed.assert(rel(492), Tuple::from([int(10)])).unwrap();
    seed.assert(rel(493), Tuple::from([int(10)])).unwrap();
    seed.commit().unwrap();

    let snapshot = kernel.snapshot();
    assert!(snapshot.scan(rel(496), &[None]).unwrap().is_empty());
    assert_maintained_matches_complete(&snapshot, &[(rel(494), 1), (rel(496), 1)]);

    let apply = |relation: RelationId, tuple: Tuple, retract: bool| {
        let mut tx = kernel.begin();
        if retract {
            tx.retract(relation, tuple).unwrap();
        } else {
            tx.assert(relation, tuple).unwrap();
        }
        tx.commit().unwrap();
        let snapshot = kernel.snapshot();
        assert_maintained_matches_complete(&snapshot, &[(rel(494), 1), (rel(496), 1)]);
        snapshot
    };

    let snapshot = apply(rel(492), Tuple::from([int(10)]), true);
    assert!(snapshot.scan(rel(496), &[None]).unwrap().is_empty());
    assert!(
        !snapshot
            .maintained_state()
            .unwrap()
            .visible_changes()
            .iter()
            .any(|change| change.relation == rel(494) || change.relation == rel(496))
    );

    let snapshot = apply(rel(493), Tuple::from([int(10)]), true);
    assert_eq!(
        snapshot.scan(rel(496), &[None]).unwrap(),
        vec![Tuple::from([int(1)])]
    );
    assert_eq!(
        snapshot
            .maintained_state()
            .unwrap()
            .visible_changes()
            .iter()
            .filter(|change| change.relation == rel(496))
            .count(),
        1
    );

    let snapshot = apply(rel(495), Tuple::from([int(1)]), false);
    assert!(snapshot.scan(rel(496), &[None]).unwrap().is_empty());
    let snapshot = apply(rel(494), Tuple::from([int(10)]), false);
    assert!(snapshot.scan(rel(496), &[None]).unwrap().is_empty());
    let snapshot = apply(rel(495), Tuple::from([int(1)]), true);
    assert!(snapshot.scan(rel(496), &[None]).unwrap().is_empty());
    let snapshot = apply(rel(492), Tuple::from([int(10)]), false);
    assert!(snapshot.scan(rel(496), &[None]).unwrap().is_empty());
    let snapshot = apply(rel(494), Tuple::from([int(10)]), true);
    assert!(snapshot.scan(rel(496), &[None]).unwrap().is_empty());
    let snapshot = apply(rel(492), Tuple::from([int(10)]), true);
    assert_eq!(
        snapshot.scan(rel(496), &[None]).unwrap(),
        vec![Tuple::from([int(1)])]
    );
    let snapshot = apply(rel(490), Tuple::from([int(1), int(10)]), true);
    assert_eq!(
        snapshot.scan(rel(496), &[None]).unwrap(),
        vec![Tuple::from([int(1)])]
    );
    let snapshot = apply(rel(491), Tuple::from([int(1), int(10)]), true);
    assert!(snapshot.scan(rel(496), &[None]).unwrap().is_empty());
}

#[test]
fn stratified_negation_matches_randomized_positive_and_negative_changes() {
    let kernel = RelationKernel::new();
    create_relations(
        &kernel,
        &[
            (500, "Node", 1),
            (501, "Blocked", 1),
            (502, "Hidden", 1),
            (503, "Visible", 1),
        ],
    );
    kernel
        .install_rule(
            Rule::new(
                rel(503),
                [var("node")],
                [
                    Atom::positive(rel(500), [var("node")]),
                    Atom::negated(rel(501), [var("node")]),
                    Atom::negated(rel(502), [var("node")]),
                ],
            ),
            "Visible(node) :- Node(node), !Blocked(node), !Hidden(node)",
        )
        .unwrap();

    let mut sets = [BTreeSet::new(), BTreeSet::new(), BTreeSet::new()];
    let relation_arities = [(rel(500), 1), (rel(501), 1), (rel(502), 1), (rel(503), 1)];
    let mut previous = kernel.snapshot();
    assert!(previous.scan(rel(503), &[None]).unwrap().is_empty());
    let mut random = 0xd1b5_4a32_d192_ed03_u64;
    for _ in 0..256 {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let input = (random as usize) % sets.len();
        let value = ((random >> 16) % 16) as i64;
        let relation = rel(500 + input as u64);
        let tuple = Tuple::from([int(value)]);
        let mut tx = kernel.begin();
        if sets[input].remove(&value) {
            tx.retract(relation, tuple).unwrap();
        } else {
            sets[input].insert(value);
            tx.assert(relation, tuple).unwrap();
        }
        tx.commit().unwrap();

        let next = kernel.snapshot();
        assert_maintained_matches_complete(&next, &[(rel(503), 1)]);
        assert_eq!(
            apply_visible_changes(
                visible_rows(&previous, &relation_arities),
                next.maintained_state().unwrap().visible_changes(),
            ),
            visible_rows(&next, &relation_arities),
        );
        previous = next;
    }
}

#[test]
fn recursive_maintenance_matches_randomized_cyclic_graph_changes() {
    let kernel = RelationKernel::new();
    create_relations(&kernel, &[(510, "Edge", 2), (511, "Reachable", 2)]);
    kernel
        .install_rule(
            Rule::new(
                rel(511),
                [var("from"), var("to")],
                [Atom::positive(rel(510), [var("from"), var("to")])],
            ),
            "Reachable(from, to) :- Edge(from, to)",
        )
        .unwrap();
    kernel
        .install_rule(
            Rule::new(
                rel(511),
                [var("from"), var("to")],
                [
                    Atom::positive(rel(511), [var("from"), var("middle")]),
                    Atom::positive(rel(510), [var("middle"), var("to")]),
                ],
            ),
            "Reachable(from, to) :- Reachable(from, middle), Edge(middle, to)",
        )
        .unwrap();

    let relation_arities = [(rel(510), 2), (rel(511), 2)];
    let mut previous = kernel.snapshot();
    assert!(previous.scan(rel(511), &[None, None]).unwrap().is_empty());
    let retained_empty = Arc::clone(&previous);
    let mut edges = BTreeSet::new();
    let mut random = 0xa076_1d64_78bd_642f_u64;
    for _ in 0..256 {
        random = random
            .wrapping_mul(2_862_933_555_777_941_757)
            .wrapping_add(3_037_000_493);
        let edge = (((random >> 8) % 6) as i64, ((random >> 24) % 6) as i64);
        let tuple = Tuple::from([int(edge.0), int(edge.1)]);
        let mut tx = kernel.begin();
        if edges.remove(&edge) {
            tx.retract(rel(510), tuple).unwrap();
        } else {
            edges.insert(edge);
            tx.assert(rel(510), tuple).unwrap();
        }
        tx.commit().unwrap();

        let next = kernel.snapshot();
        assert_maintained_matches_complete(&next, &[(rel(511), 2)]);
        assert_eq!(
            apply_visible_changes(
                visible_rows(&previous, &relation_arities),
                next.maintained_state().unwrap().visible_changes(),
            ),
            visible_rows(&next, &relation_arities),
        );
        assert!(
            next.maintained_state()
                .unwrap()
                .work()
                .frontier_rows
                .iter()
                .all(|rows| *rows > 0)
        );
        previous = next;
    }
    assert!(
        retained_empty
            .scan(rel(511), &[None, None])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn recursive_retractions_remove_self_support_and_settle_mutual_recursion() {
    let self_cycle = RelationKernel::new();
    create_relations(&self_cycle, &[(520, "Seed", 1), (521, "Present", 1)]);
    self_cycle
        .install_rule(
            Rule::new(
                rel(521),
                [var("value")],
                [Atom::positive(rel(520), [var("value")])],
            ),
            "Present(value) :- Seed(value)",
        )
        .unwrap();
    self_cycle
        .install_rule(
            Rule::new(
                rel(521),
                [var("value")],
                [Atom::positive(rel(521), [var("value")])],
            ),
            "Present(value) :- Present(value)",
        )
        .unwrap();
    let mut tx = self_cycle.begin();
    tx.assert(rel(520), Tuple::from([int(7)])).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        self_cycle.snapshot().scan(rel(521), &[None]).unwrap(),
        vec![Tuple::from([int(7)])]
    );
    let mut tx = self_cycle.begin();
    tx.retract(rel(520), Tuple::from([int(7)])).unwrap();
    tx.commit().unwrap();
    let snapshot = self_cycle.snapshot();
    assert!(snapshot.scan(rel(521), &[None]).unwrap().is_empty());
    assert_maintained_matches_complete(&snapshot, &[(rel(521), 1)]);

    let extensional_target = RelationKernel::new();
    create_relations(&extensional_target, &[(522, "Loop", 1)]);
    extensional_target
        .install_rule(
            Rule::new(
                rel(522),
                [var("value")],
                [Atom::positive(rel(522), [var("value")])],
            ),
            "Loop(value) :- Loop(value)",
        )
        .unwrap();
    let mut tx = extensional_target.begin();
    tx.assert(rel(522), Tuple::from([int(8)])).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        extensional_target
            .snapshot()
            .scan(rel(522), &[None])
            .unwrap(),
        vec![Tuple::from([int(8)])]
    );
    let mut tx = extensional_target.begin();
    tx.retract(rel(522), Tuple::from([int(8)])).unwrap();
    tx.commit().unwrap();
    let snapshot = extensional_target.snapshot();
    assert!(snapshot.scan(rel(522), &[None]).unwrap().is_empty());
    assert_maintained_matches_complete(&snapshot, &[(rel(522), 1)]);

    let mutual = RelationKernel::new();
    create_relations(
        &mutual,
        &[(523, "Seed", 1), (524, "Alpha", 1), (525, "Beta", 1)],
    );
    for (head, body, source) in [
        (rel(524), rel(523), "Seed"),
        (rel(525), rel(524), "Alpha"),
        (rel(524), rel(525), "Beta"),
    ] {
        mutual
            .install_rule(
                Rule::new(head, [var("value")], [Atom::positive(body, [var("value")])]),
                format!("Result(value) :- {source}(value)"),
            )
            .unwrap();
    }
    let mut tx = mutual.begin();
    tx.assert(rel(523), Tuple::from([int(9)])).unwrap();
    tx.commit().unwrap();
    let snapshot = mutual.snapshot();
    assert_eq!(snapshot.scan(rel(525), &[None]).unwrap().len(), 1);
    assert_maintained_matches_complete(&snapshot, &[(rel(524), 1), (rel(525), 1)]);
    let mut tx = mutual.begin();
    tx.retract(rel(523), Tuple::from([int(9)])).unwrap();
    tx.commit().unwrap();
    let snapshot = mutual.snapshot();
    assert!(snapshot.scan(rel(524), &[None]).unwrap().is_empty());
    assert!(snapshot.scan(rel(525), &[None]).unwrap().is_empty());
    assert_maintained_matches_complete(&snapshot, &[(rel(524), 1), (rel(525), 1)]);
    let mut tx = mutual.begin();
    tx.assert(rel(523), Tuple::from([int(9)])).unwrap();
    tx.commit().unwrap();
    let snapshot = mutual.snapshot();
    assert_eq!(snapshot.scan(rel(524), &[None]).unwrap().len(), 1);
    assert_eq!(snapshot.scan(rel(525), &[None]).unwrap().len(), 1);
    assert_maintained_matches_complete(&snapshot, &[(rel(524), 1), (rel(525), 1)]);
}

#[test]
fn recursive_maintenance_handles_multiple_feedback_atoms_and_lower_negation() {
    let transitive = RelationKernel::new();
    create_relations(&transitive, &[(530, "Edge", 2), (531, "Reachable", 2)]);
    transitive
        .install_rule(
            Rule::new(
                rel(531),
                [var("from"), var("to")],
                [Atom::positive(rel(530), [var("from"), var("to")])],
            ),
            "Reachable(from, to) :- Edge(from, to)",
        )
        .unwrap();
    transitive
        .install_rule(
            Rule::new(
                rel(531),
                [var("from"), var("to")],
                [
                    Atom::positive(rel(531), [var("from"), var("middle")]),
                    Atom::positive(rel(531), [var("middle"), var("to")]),
                ],
            ),
            "Reachable(from, to) :- Reachable(from, middle), Reachable(middle, to)",
        )
        .unwrap();
    assert_rows(&transitive, rel(530), &[(1, 2), (2, 3), (3, 1)]);
    let snapshot = transitive.snapshot();
    assert_eq!(snapshot.scan(rel(531), &[None, None]).unwrap().len(), 9);
    assert_maintained_matches_complete(&snapshot, &[(rel(531), 2)]);
    retract_rows(&transitive, rel(530), &[(2, 3)]);
    let snapshot = transitive.snapshot();
    assert_maintained_matches_complete(&snapshot, &[(rel(531), 2)]);
    assert_rows(&transitive, rel(530), &[(2, 3)]);
    let snapshot = transitive.snapshot();
    assert_eq!(snapshot.scan(rel(531), &[None, None]).unwrap().len(), 9);
    assert_maintained_matches_complete(&snapshot, &[(rel(531), 2)]);

    let negated = RelationKernel::new();
    create_relations(
        &negated,
        &[
            (540, "Seed", 1),
            (541, "Step", 2),
            (542, "Blocked", 1),
            (543, "Live", 1),
        ],
    );
    negated
        .install_rule(
            Rule::new(
                rel(543),
                [var("node")],
                [
                    Atom::positive(rel(540), [var("node")]),
                    Atom::negated(rel(542), [var("node")]),
                ],
            ),
            "Live(node) :- Seed(node), !Blocked(node)",
        )
        .unwrap();
    negated
        .install_rule(
            Rule::new(
                rel(543),
                [var("to")],
                [
                    Atom::positive(rel(543), [var("from")]),
                    Atom::positive(rel(541), [var("from"), var("to")]),
                    Atom::negated(rel(542), [var("to")]),
                ],
            ),
            "Live(to) :- Live(from), Step(from, to), !Blocked(to)",
        )
        .unwrap();
    let mut tx = negated.begin();
    tx.assert(rel(540), Tuple::from([int(1)])).unwrap();
    tx.assert(rel(541), Tuple::from([int(1), int(2)])).unwrap();
    tx.assert(rel(541), Tuple::from([int(2), int(3)])).unwrap();
    tx.commit().unwrap();
    let snapshot = negated.snapshot();
    assert_eq!(snapshot.scan(rel(543), &[None]).unwrap().len(), 3);
    assert_maintained_matches_complete(&snapshot, &[(rel(543), 1)]);

    for (blocked, expected) in [(true, 1), (false, 3)] {
        let mut tx = negated.begin();
        if blocked {
            tx.assert(rel(542), Tuple::from([int(2)])).unwrap();
        } else {
            tx.retract(rel(542), Tuple::from([int(2)])).unwrap();
        }
        tx.commit().unwrap();
        let snapshot = negated.snapshot();
        assert_eq!(snapshot.scan(rel(543), &[None]).unwrap().len(), expected);
        assert_maintained_matches_complete(&snapshot, &[(rel(543), 1)]);
    }
}

#[test]
fn recursive_deletion_reseeds_through_maintained_join_indexes() {
    let kernel = RelationKernel::new();
    create_relations(&kernel, &[(580, "Edge", 2), (581, "Reach", 2)]);
    kernel
        .install_rule(
            Rule::new(
                rel(581),
                [var("a"), var("b")],
                [Atom::positive(rel(580), [var("a"), var("b")])],
            ),
            "Reach(a, b) :- Edge(a, b)",
        )
        .unwrap();
    kernel
        .install_rule(
            Rule::new(
                rel(581),
                [var("a"), var("c")],
                [
                    Atom::positive(rel(580), [var("a"), var("b")]),
                    Atom::positive(rel(581), [var("b"), var("c")]),
                ],
            ),
            "Reach(a, c) :- Edge(a, b), Reach(b, c)",
        )
        .unwrap();
    let mut seed = kernel.begin();
    for component in 0..8 {
        for edge in 0..12 {
            seed.assert(
                rel(580),
                Tuple::from([int(component * 100 + edge), int(component * 100 + edge + 1)]),
            )
            .unwrap();
        }
    }
    seed.commit().unwrap();
    let original = kernel.snapshot();
    assert_eq!(original.scan(rel(581), &[None, None]).unwrap().len(), 624);
    let mut deletion = kernel.begin();
    deletion
        .retract(rel(580), Tuple::from([int(11), int(12)]))
        .unwrap();
    deletion.commit().unwrap();
    let next = kernel.snapshot();
    assert_eq!(next.scan(rel(581), &[None, None]).unwrap().len(), 612);
    assert!(
        next.scan(rel(581), &[Some(int(0)), Some(int(12))])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        next.scan(rel(581), &[Some(int(700)), Some(int(712))])
            .unwrap()
            .len(),
        1
    );
    assert_maintained_matches_complete(&next, &[(rel(581), 2)]);
    let maintained = next.maintained_state().unwrap();
    // Each retained Edge binding probes the Reach index during reseeding.
    // Cartesian reseeding would inspect more than 58,000 candidate rows.
    assert!(
        maintained.work().arrangement_lookups >= 95,
        "{:?}",
        maintained.work()
    );
    assert!(
        maintained.work().rows_visited < 2_000,
        "{:?}",
        maintained.work()
    );
    assert_eq!(maintained.work().visible_changes, 13); // One edge and twelve derived rows.
    assert_eq!(original.scan(rel(581), &[None, None]).unwrap().len(), 624);
}
