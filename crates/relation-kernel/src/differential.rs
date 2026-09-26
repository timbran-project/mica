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

use crate::index::RelationState;
use crate::rules::{
    CompiledAtom, CompiledBodyItem, CompiledGuard, CompiledRule, CompiledTerm, compare_values,
};
use crate::snapshot::active_rules;
use crate::{
    ExecutionContext, FactChange, FactChangeKind, KernelError, RelationId, RuleSet, Snapshot,
    Tuple, Version,
};
use mica_var::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

pub(crate) type Diff = i64;
type WeightedRows = BTreeMap<Tuple, Diff>;
type Binding = Vec<Option<Value>>;
type WeightedBindings = BTreeMap<Binding, Diff>;
type Collection = Arc<BTreeSet<Tuple>>;
type Collections = BTreeMap<RelationId, Collection>;
const TRACE_COMPACTION_BATCHES: usize = 8;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct MaintenanceWork {
    pub(crate) input_changes: usize,
    pub(crate) affected_components: usize,
    pub(crate) candidate_changes: usize,
    pub(crate) consolidated_changes: usize,
    pub(crate) visible_changes: usize,
    pub(crate) arrangement_lookups: usize,
    pub(crate) rows_visited: usize,
    pub(crate) trace_batches: usize,
    pub(crate) trace_bytes: usize,
    pub(crate) compaction_rows: usize,
    pub(crate) recursive_iterations: usize,
    pub(crate) frontier_rows: Vec<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct ArrangementSpec {
    relation: RelationId,
    positions: Vec<u16>,
}

#[derive(Clone, Debug)]
struct Arrangement {
    spec: ArrangementSpec,
    rows_by_key: BTreeMap<Vec<Value>, Arc<[Tuple]>>,
}

impl Arrangement {
    fn build(spec: ArrangementSpec, rows: &BTreeSet<Tuple>) -> Self {
        let mut grouped = BTreeMap::<Vec<Value>, Vec<Tuple>>::new();
        for tuple in rows {
            grouped
                .entry(arrangement_tuple_key(tuple, &spec.positions))
                .or_default()
                .push(tuple.clone());
        }
        Self {
            spec,
            rows_by_key: grouped
                .into_iter()
                .map(|(key, rows)| (key, Arc::from(rows)))
                .collect(),
        }
    }

    fn apply(&self, changes: &WeightedRows) -> Self {
        let mut rows_by_key = self.rows_by_key.clone();
        for (tuple, difference) in changes {
            let key = arrangement_tuple_key(tuple, &self.spec.positions);
            let mut rows = rows_by_key
                .get(&key)
                .map(|rows| rows.to_vec())
                .unwrap_or_default();
            match difference.cmp(&0) {
                std::cmp::Ordering::Greater => {
                    if let Err(position) = rows.binary_search(tuple) {
                        rows.insert(position, tuple.clone());
                    }
                }
                std::cmp::Ordering::Less => {
                    if let Ok(position) = rows.binary_search(tuple) {
                        rows.remove(position);
                    }
                }
                std::cmp::Ordering::Equal => {}
            }
            if rows.is_empty() {
                rows_by_key.remove(&key);
            } else {
                rows_by_key.insert(key, Arc::from(rows));
            }
        }
        Self {
            spec: self.spec.clone(),
            rows_by_key,
        }
    }

    fn lookup(&self, key: &[Value]) -> &[Tuple] {
        self.rows_by_key.get(key).map_or(&[], AsRef::as_ref)
    }
}

#[derive(Clone, Debug)]
struct TraceBatch {
    epoch: Version,
    rows: Arc<[Tuple]>,
    differences: Arc<[Diff]>,
    retained_bytes: usize,
    packed: OnceLock<Option<WeightedPackedBatch>>,
}

#[derive(Clone, Debug)]
struct WeightedPackedBatch {
    rows: Arc<crate::PackedRelation>,
    differences: Arc<[Diff]>,
}

impl TraceBatch {
    fn from_rows(epoch: Version, rows: impl IntoIterator<Item = (Tuple, Diff)>) -> Self {
        let (rows, differences): (Vec<_>, Vec<_>) = rows
            .into_iter()
            .filter(|(_, difference)| *difference != 0)
            .unzip();
        let retained_bytes = rows
            .iter()
            .map(|tuple| {
                std::mem::size_of::<Tuple>()
                    + tuple.arity() * std::mem::size_of::<Value>()
                    + std::mem::size_of::<Diff>()
            })
            .sum();
        Self {
            epoch,
            rows: rows.into(),
            differences: differences.into(),
            retained_bytes,
            packed: OnceLock::new(),
        }
    }

    fn from_set(epoch: Version, rows: &BTreeSet<Tuple>) -> Self {
        Self::from_rows(epoch, rows.iter().cloned().map(|tuple| (tuple, 1)))
    }

    fn packed(&self) -> Option<&WeightedPackedBatch> {
        self.packed
            .get_or_init(|| {
                let arity = self.rows.first()?.arity();
                let rows = crate::PackedRelation::from_shared_canonical_tuples(
                    Arc::clone(&self.rows),
                    arity,
                )?;
                Some(WeightedPackedBatch {
                    rows: Arc::new(rows),
                    differences: Arc::clone(&self.differences),
                })
            })
            .as_ref()
    }
}

#[derive(Clone, Debug)]
struct Trace {
    base: Arc<TraceBatch>,
    batches: Arc<[Arc<TraceBatch>]>,
}

impl Trace {
    fn initialize(epoch: Version, rows: &BTreeSet<Tuple>) -> Self {
        Self {
            base: Arc::new(TraceBatch::from_set(epoch, rows)),
            batches: Arc::from([]),
        }
    }

    fn append(
        &self,
        epoch: Version,
        changes: &WeightedRows,
        current: &BTreeSet<Tuple>,
    ) -> (Self, usize) {
        debug_assert!(epoch > self.base.epoch);
        let batch = Arc::new(TraceBatch::from_rows(
            epoch,
            changes
                .iter()
                .map(|(tuple, difference)| (tuple.clone(), *difference)),
        ));
        let mut batches = self.batches.to_vec();
        batches.push(batch);
        let delta_bytes = batches
            .iter()
            .map(|batch| batch.retained_bytes)
            .sum::<usize>();
        let should_compact = batches.len() >= TRACE_COMPACTION_BATCHES
            || delta_bytes.saturating_mul(4) >= self.base.retained_bytes.max(1);
        if should_compact {
            return (Self::initialize(epoch, current), current.len());
        }
        (
            Self {
                base: Arc::clone(&self.base),
                batches: batches.into(),
            },
            0,
        )
    }

    fn batch_count(&self) -> usize {
        1 + self.batches.len()
    }

    fn retained_bytes(&self) -> usize {
        self.base.retained_bytes
            + self
                .batches
                .iter()
                .map(|batch| {
                    debug_assert_eq!(batch.rows.len(), batch.differences.len());
                    batch.retained_bytes
                })
                .sum::<usize>()
    }

    fn batches(&self) -> impl Iterator<Item = &TraceBatch> {
        std::iter::once(self.base.as_ref()).chain(self.batches.iter().map(AsRef::as_ref))
    }
}

#[derive(Clone, Debug)]
struct NegatedRuleState {
    positive_bindings: WeightedBindings,
    left_by_negative_key: Vec<BTreeMap<Tuple, BTreeSet<Binding>>>,
}

#[derive(Clone, Debug)]
pub(crate) struct MaintainedState {
    version: Version,
    program: Arc<MaintainedProgram>,
    requested_targets: BTreeSet<RelationId>,
    collections: Collections,
    derived_support: BTreeMap<RelationId, WeightedRows>,
    negated_rules: BTreeMap<(usize, usize), NegatedRuleState>,
    negative_key_counts: BTreeMap<RelationId, BTreeMap<Tuple, usize>>,
    arrangements: BTreeMap<ArrangementSpec, Arc<Arrangement>>,
    traces: BTreeMap<RelationId, Trace>,
    visible_changes: Arc<[FactChange]>,
    work: MaintenanceWork,
}

impl MaintainedState {
    pub(crate) fn initialize(
        snapshot: &Snapshot,
        complete: &BTreeMap<RelationId, RelationState>,
        requested_targets: BTreeSet<RelationId>,
    ) -> Result<Option<Arc<Self>>, KernelError> {
        let Some(program) = MaintainedProgram::compile(snapshot, &requested_targets)? else {
            return Ok(None);
        };
        let program = Arc::new(program);
        let mut collections = BTreeMap::new();
        for relation in &program.relations {
            collections.insert(*relation, Arc::new(extensional_rows(snapshot, *relation)?));
        }
        let mut derived_support = BTreeMap::new();
        let mut negated_rules = BTreeMap::new();
        for (component_index, component) in program.components.iter().enumerate() {
            if component.recursive {
                for target in &component.targets {
                    let derived = complete
                        .get(target)
                        .map(|state| state.scan(&vec![None; state.metadata().arity() as usize]))
                        .transpose()?
                        .unwrap_or_default()
                        .into_iter()
                        .collect::<BTreeSet<_>>();
                    let support = derived.iter().cloned().map(|tuple| (tuple, 1)).collect();
                    Arc::make_mut(
                        collections
                            .get_mut(target)
                            .expect("recursive target should have a maintained collection"),
                    )
                    .extend(derived);
                    derived_support.insert(*target, support);
                }
                continue;
            }
            let target = component.target();
            let mut support = WeightedRows::new();
            for (rule_index, rule) in component.rules.iter().enumerate() {
                let (contribution, negated) = evaluate_rule_full(
                    rule,
                    &collections,
                    &BTreeMap::new(),
                    snapshot.version(),
                    &ExecutionContext::serial(),
                    &mut MaintenanceWork::default(),
                )?;
                if let Some(negated) = negated {
                    negated_rules.insert((component_index, rule_index), negated);
                }
                accumulate_rows(
                    &mut support,
                    contribution,
                    target,
                    "rule union",
                    snapshot.version(),
                )?;
            }
            ensure_non_negative(&support, target, snapshot.version())?;
            let derived = positive_rows(&support);
            Arc::make_mut(
                collections
                    .get_mut(&target)
                    .expect("rule target should have a maintained collection"),
            )
            .extend(derived);
            derived_support.insert(target, support);
        }

        if !matches_complete_output(&program, &derived_support, complete) {
            return Ok(None);
        }
        let arrangements = build_arrangements(&program.arrangement_specs, &collections);
        let negative_key_counts = program
            .negative_relations
            .iter()
            .map(|relation| {
                let counts = collections[relation]
                    .iter()
                    .cloned()
                    .map(|tuple| (tuple, 1))
                    .collect();
                (*relation, counts)
            })
            .collect();
        let traces = collections
            .iter()
            .map(|(relation, rows)| (*relation, Trace::initialize(snapshot.version(), rows)))
            .collect();
        Ok(Some(Arc::new(Self {
            version: snapshot.version(),
            program,
            requested_targets,
            collections,
            derived_support,
            negated_rules,
            negative_key_counts,
            arrangements,
            traces,
            visible_changes: Arc::from([]),
            work: MaintenanceWork::default(),
        })))
    }

    pub(crate) fn advance(
        &self,
        current: &Snapshot,
        next: &Snapshot,
        fact_changes: &[FactChange],
        execution_context: &ExecutionContext,
    ) -> Result<Arc<Self>, KernelError> {
        debug_assert_eq!(self.version, current.version());
        let version = next.version();
        let mut work = MaintenanceWork {
            input_changes: fact_changes.len(),
            ..MaintenanceWork::default()
        };
        let mut collections = self.collections.clone();
        let mut derived_support = self.derived_support.clone();
        let mut negated_rules = self.negated_rules.clone();
        let mut negative_key_counts = self.negative_key_counts.clone();
        let mut arrangements = self.arrangements.clone();
        let mut relation_deltas = BTreeMap::<RelationId, WeightedRows>::new();
        let changed_by_relation = group_fact_changes(fact_changes);

        for (relation, changes) in &changed_by_relation {
            if !self.program.relations.contains(relation) {
                continue;
            }
            let old_support = self.derived_support.get(relation);
            let collection = Arc::make_mut(
                collections
                    .entry(*relation)
                    .or_insert_with(|| Arc::new(BTreeSet::new())),
            );
            let deltas = relation_deltas.entry(*relation).or_default();
            for change in changes {
                let old_visible = collection.contains(&change.tuple);
                let new_visible = extensional_contains(next, *relation, &change.tuple)?
                    || support_is_positive(old_support, &change.tuple);
                set_presence_delta(
                    collection,
                    deltas,
                    change.tuple.clone(),
                    old_visible,
                    new_visible,
                );
            }
            update_arrangements(&mut arrangements, *relation, deltas);
            refresh_negative_key_counts(
                &mut negative_key_counts,
                &self.negative_key_counts,
                *relation,
                deltas,
            );
        }

        for (component_index, component) in self.program.components.iter().enumerate() {
            let body_changed = component.rules.iter().any(|rule| {
                rule_atoms(rule).any(|atom| {
                    relation_deltas
                        .get(&atom.relation)
                        .is_some_and(|changes| !changes.is_empty())
                })
            });
            let target_changed = component
                .targets
                .iter()
                .any(|target| changed_by_relation.contains_key(target));
            if !body_changed && !target_changed {
                continue;
            }
            work.affected_components += 1;

            if component.recursive {
                advance_recursive_component(
                    component,
                    RecursiveAdvance {
                        current: self,
                        next,
                        fact_changes: &changed_by_relation,
                        collections: &mut collections,
                        derived_support: &mut derived_support,
                        arrangements: &mut arrangements,
                        relation_deltas: &mut relation_deltas,
                        negative_key_counts: &mut negative_key_counts,
                        work: &mut work,
                        execution_context,
                    },
                )?;
                continue;
            }
            let target = component.target();

            let mut support_delta = WeightedRows::new();
            if body_changed {
                let evaluation = DeltaEvaluation {
                    old_collections: &self.collections,
                    new_collections: &collections,
                    relation_deltas: &relation_deltas,
                    old_arrangements: &self.arrangements,
                    new_arrangements: &arrangements,
                    old_traces: Some(&self.traces),
                    target,
                    version,
                    execution_context,
                };
                for (rule_index, rule) in component.rules.iter().enumerate() {
                    let contribution = if has_negation(rule) {
                        let state = negated_rules
                            .get_mut(&(component_index, rule_index))
                            .expect("negated rule should retain its left state");
                        evaluate_negated_rule_delta(
                            rule,
                            state,
                            &evaluation,
                            &self.negative_key_counts,
                            &negative_key_counts,
                            &mut work,
                        )?
                    } else {
                        evaluate_rule_delta(rule, &evaluation, &mut work)?
                    };
                    accumulate_rows(
                        &mut support_delta,
                        contribution,
                        target,
                        "rule union",
                        version,
                    )?;
                }
            }
            work.consolidated_changes += support_delta.len();

            let support = derived_support.entry(target).or_default();
            for (tuple, difference) in &support_delta {
                checked_accumulate(
                    support,
                    tuple.clone(),
                    *difference,
                    target,
                    "head contribution",
                    version,
                )?;
            }
            ensure_non_negative(support, target, version)?;

            let mut touched = support_delta.keys().cloned().collect::<BTreeSet<_>>();
            if let Some(changes) = changed_by_relation.get(&target) {
                touched.extend(changes.iter().map(|change| change.tuple.clone()));
            }
            let collection = Arc::make_mut(
                collections
                    .entry(target)
                    .or_insert_with(|| Arc::new(BTreeSet::new())),
            );
            let deltas = relation_deltas.entry(target).or_default();
            for tuple in touched {
                let old_visible = self
                    .collections
                    .get(&target)
                    .is_some_and(|rows| rows.contains(&tuple));
                let new_visible = extensional_contains(next, target, &tuple)?
                    || support_is_positive(Some(support), &tuple);
                set_presence_delta(collection, deltas, tuple, old_visible, new_visible);
            }
            update_arrangements(&mut arrangements, target, deltas);
            refresh_negative_key_counts(
                &mut negative_key_counts,
                &self.negative_key_counts,
                target,
                deltas,
            );
        }

        let mut traces = self.traces.clone();
        for (relation, changes) in &relation_deltas {
            if changes.is_empty() {
                continue;
            }
            let trace = traces
                .get(relation)
                .expect("maintained relation should have a trace");
            let (next_trace, compacted_rows) = trace.append(
                version,
                changes,
                collections
                    .get(relation)
                    .expect("maintained relation should have current rows"),
            );
            work.compaction_rows += compacted_rows;
            traces.insert(*relation, next_trace);
        }
        work.trace_batches = traces.values().map(Trace::batch_count).sum();
        work.trace_bytes = traces.values().map(Trace::retained_bytes).sum();

        let visible_changes = relation_deltas
            .into_iter()
            .flat_map(|(relation, changes)| {
                changes
                    .into_iter()
                    .map(move |(tuple, difference)| FactChange {
                        relation,
                        tuple,
                        kind: if difference > 0 {
                            FactChangeKind::Assert
                        } else {
                            FactChangeKind::Retract
                        },
                    })
            })
            .collect::<Vec<_>>();
        work.visible_changes = visible_changes.len();

        Ok(Arc::new(Self {
            version,
            program: Arc::clone(&self.program),
            requested_targets: self.requested_targets.clone(),
            collections,
            derived_support,
            negated_rules,
            negative_key_counts,
            arrangements,
            traces,
            visible_changes: visible_changes.into(),
            work,
        }))
    }

    pub(crate) fn build_derived_relations(
        &self,
        snapshot: &Snapshot,
    ) -> Result<BTreeMap<RelationId, RelationState>, KernelError> {
        let derived = self
            .derived_support
            .iter()
            .map(|(relation, support)| (*relation, positive_rows(support).into_iter().collect()))
            .collect();
        crate::snapshot::build_derived_relations(&snapshot.relations, derived)
    }

    #[cfg(test)]
    pub(crate) fn version(&self) -> Version {
        self.version
    }

    pub(crate) fn visible_changes(&self) -> &[FactChange] {
        &self.visible_changes
    }

    pub(crate) fn work(&self) -> &MaintenanceWork {
        &self.work
    }

    pub(crate) fn serves(&self, relation: RelationId) -> bool {
        self.program.targets.contains(&relation)
    }

    pub(crate) fn requested_targets(&self) -> &BTreeSet<RelationId> {
        &self.requested_targets
    }

    #[cfg(test)]
    pub(crate) fn trace_batch_count(&self, relation: RelationId) -> Option<usize> {
        self.traces.get(&relation).map(Trace::batch_count)
    }

    #[cfg(test)]
    pub(crate) fn arrangement_count(&self) -> usize {
        self.arrangements.len()
    }
}

#[derive(Clone, Debug)]
struct MaintainedProgram {
    components: Vec<MaintainedComponent>,
    relations: BTreeSet<RelationId>,
    targets: BTreeSet<RelationId>,
    negative_relations: BTreeSet<RelationId>,
    arrangement_specs: BTreeSet<ArrangementSpec>,
}

impl MaintainedProgram {
    fn compile(
        snapshot: &Snapshot,
        requested_targets: &BTreeSet<RelationId>,
    ) -> Result<Option<Self>, KernelError> {
        let rules = active_rules(snapshot.rules());
        if rules.is_empty() {
            return Ok(None);
        }
        let rules = RuleSet::new(rules);
        let compiled = rules.compile().map_err(KernelError::Rule)?;
        let all_targets = compiled
            .strata
            .iter()
            .flat_map(|stratum| &stratum.components)
            .flat_map(|component| component.target_relations.iter().copied())
            .collect::<BTreeSet<_>>();
        let mut required_targets = requested_targets
            .intersection(&all_targets)
            .copied()
            .collect::<BTreeSet<_>>();
        if required_targets.is_empty() {
            return Ok(None);
        }
        loop {
            let before = required_targets.len();
            for stratum in &compiled.strata {
                for component in &stratum.components {
                    if component.target_relations.is_disjoint(&required_targets) {
                        continue;
                    }
                    for rule_index in &component.rule_indices {
                        required_targets.extend(
                            rule_atoms(&stratum.rules[*rule_index])
                                .map(|atom| atom.relation)
                                .filter(|relation| all_targets.contains(relation)),
                        );
                    }
                }
            }
            if required_targets.len() == before {
                break;
            }
        }
        let mut relations = BTreeSet::new();
        let mut negative_relations = BTreeSet::new();
        let mut components = Vec::new();
        for stratum in &compiled.strata {
            for component in &stratum.components {
                if component.target_relations.is_disjoint(&required_targets) {
                    continue;
                }
                let rules = component
                    .rule_indices
                    .iter()
                    .map(|index| stratum.rules[*index].clone())
                    .collect::<Vec<_>>();
                relations.extend(component.target_relations.iter().copied());
                relations.extend(rules.iter().flat_map(rule_atoms).map(|atom| atom.relation));
                negative_relations.extend(
                    rules
                        .iter()
                        .flat_map(negated_rule_atoms)
                        .map(|atom| atom.relation),
                );
                components.push(MaintainedComponent {
                    targets: component.target_relations.clone(),
                    rules,
                    recursive: !component.recursive_variants.is_empty(),
                });
            }
        }
        for relation in &relations {
            let state = snapshot
                .relations
                .get(relation)
                .ok_or(KernelError::UnknownRelation(*relation))?;
            if snapshot
                .computed_relations
                .is_computed_relation(state.metadata())
            {
                return Ok(None);
            }
        }
        let arrangement_specs = components
            .iter()
            .flat_map(|component| &component.rules)
            .flat_map(required_arrangements)
            .collect();

        Ok(Some(Self {
            components,
            relations,
            targets: required_targets,
            negative_relations,
            arrangement_specs,
        }))
    }
}

#[derive(Clone, Debug)]
struct MaintainedComponent {
    targets: BTreeSet<RelationId>,
    rules: Vec<CompiledRule>,
    recursive: bool,
}

impl MaintainedComponent {
    fn target(&self) -> RelationId {
        debug_assert_eq!(self.targets.len(), 1);
        *self.targets.first().unwrap()
    }
}

fn required_arrangements(rule: &CompiledRule) -> BTreeSet<ArrangementSpec> {
    let mut arrangements = BTreeSet::new();
    let atoms = positive_rule_atoms(rule).collect::<Vec<_>>();
    for first in 0..atoms.len().max(1) {
        let mut bound_slots = BTreeSet::new();
        let order = std::iter::once(first).chain((0..atoms.len()).filter(|index| *index != first));
        for index in order {
            let Some(atom) = atoms.get(index) else {
                continue;
            };
            let positions = atom
                .terms
                .iter()
                .enumerate()
                .filter_map(|(position, term)| match term {
                    CompiledTerm::Value(_) => Some(position as u16),
                    CompiledTerm::Var { slot, .. } if bound_slots.contains(slot) => {
                        Some(position as u16)
                    }
                    CompiledTerm::Var { .. } => None,
                })
                .collect::<Vec<_>>();
            if !positions.is_empty() {
                arrangements.insert(ArrangementSpec {
                    relation: atom.relation,
                    positions,
                });
            }
            bound_slots.extend(atom.terms.iter().filter_map(|term| match term {
                CompiledTerm::Var { slot, .. } => Some(*slot),
                CompiledTerm::Value(_) => None,
            }));
        }
    }
    arrangements
}

fn build_arrangements(
    specs: &BTreeSet<ArrangementSpec>,
    collections: &Collections,
) -> BTreeMap<ArrangementSpec, Arc<Arrangement>> {
    specs
        .iter()
        .cloned()
        .map(|spec| {
            let rows = collections
                .get(&spec.relation)
                .expect("arranged relation should have a maintained collection");
            let arrangement = Arc::new(Arrangement::build(spec.clone(), rows));
            (spec, arrangement)
        })
        .collect()
}

fn update_arrangements(
    arrangements: &mut BTreeMap<ArrangementSpec, Arc<Arrangement>>,
    relation: RelationId,
    changes: &WeightedRows,
) {
    if changes.is_empty() {
        return;
    }
    let specs = arrangements
        .keys()
        .filter(|spec| spec.relation == relation)
        .cloned()
        .collect::<Vec<_>>();
    for spec in specs {
        let next = arrangements[&spec].apply(changes);
        arrangements.insert(spec, Arc::new(next));
    }
}

fn arrangement_tuple_key(tuple: &Tuple, positions: &[u16]) -> Vec<Value> {
    positions
        .iter()
        .map(|position| tuple.values()[*position as usize].clone())
        .collect()
}

fn rule_atoms(rule: &CompiledRule) -> impl Iterator<Item = &CompiledAtom> {
    rule.body.iter().filter_map(|item| match item {
        CompiledBodyItem::Atom(atom) => Some(atom),
        CompiledBodyItem::Guard(_) => None,
    })
}

fn positive_rule_atoms(rule: &CompiledRule) -> impl Iterator<Item = &CompiledAtom> {
    rule_atoms(rule).filter(|atom| !atom.negated)
}

fn negated_rule_atoms(rule: &CompiledRule) -> impl Iterator<Item = &CompiledAtom> {
    rule_atoms(rule).filter(|atom| atom.negated)
}

fn has_negation(rule: &CompiledRule) -> bool {
    negated_rule_atoms(rule).next().is_some()
}

fn evaluate_rule_full(
    rule: &CompiledRule,
    collections: &Collections,
    arrangements: &BTreeMap<ArrangementSpec, Arc<Arrangement>>,
    version: Version,
    execution_context: &ExecutionContext,
    work: &mut MaintenanceWork,
) -> Result<(WeightedRows, Option<NegatedRuleState>), KernelError> {
    let mut bindings = unit_binding(rule.slot_count);
    for atom in positive_rule_atoms(rule) {
        let rows = collections
            .get(&atom.relation)
            .expect("compiled relation should have a maintained collection");
        bindings = join_arranged(
            bindings,
            atom,
            FullJoinInput {
                rows,
                arrangements,
                trace: None,
                target: rule.head_relation,
                version,
                execution_context,
            },
            work,
        )?;
    }
    bindings = filter_guards(rule, bindings);
    if !has_negation(rule) {
        return Ok((project_rule(rule, bindings, version, work)?, None));
    }
    let contribution = project_active_negated_bindings(
        rule,
        &bindings,
        |relation, tuple| collections[&relation].contains(tuple),
        version,
        work,
    )?;
    let state = NegatedRuleState {
        left_by_negative_key: build_negative_left_indexes(rule, &bindings)?,
        positive_bindings: bindings,
    };
    Ok((contribution, Some(state)))
}

struct DeltaEvaluation<'a> {
    old_collections: &'a Collections,
    new_collections: &'a Collections,
    relation_deltas: &'a BTreeMap<RelationId, WeightedRows>,
    old_arrangements: &'a BTreeMap<ArrangementSpec, Arc<Arrangement>>,
    new_arrangements: &'a BTreeMap<ArrangementSpec, Arc<Arrangement>>,
    old_traces: Option<&'a BTreeMap<RelationId, Trace>>,
    target: RelationId,
    version: Version,
    execution_context: &'a ExecutionContext,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct LogicalTime {
    epoch: Version,
    iteration: usize,
}

struct RecursiveFrontier {
    time: LogicalTime,
    changes: BTreeMap<RelationId, WeightedRows>,
}

struct RecursiveAdvance<'a> {
    current: &'a MaintainedState,
    next: &'a Snapshot,
    fact_changes: &'a BTreeMap<RelationId, Vec<&'a FactChange>>,
    collections: &'a mut Collections,
    derived_support: &'a mut BTreeMap<RelationId, WeightedRows>,
    arrangements: &'a mut BTreeMap<ArrangementSpec, Arc<Arrangement>>,
    relation_deltas: &'a mut BTreeMap<RelationId, WeightedRows>,
    negative_key_counts: &'a mut BTreeMap<RelationId, BTreeMap<Tuple, usize>>,
    work: &'a mut MaintenanceWork,
    execution_context: &'a ExecutionContext,
}

fn advance_recursive_component(
    component: &MaintainedComponent,
    mut advance: RecursiveAdvance<'_>,
) -> Result<(), KernelError> {
    let version = advance.next.version();
    let old_derived = component
        .targets
        .iter()
        .map(|target| {
            (
                *target,
                advance
                    .current
                    .derived_support
                    .get(target)
                    .map(positive_rows)
                    .unwrap_or_default(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let target_seed_retracted = component.targets.iter().any(|target| {
        advance.fact_changes.get(target).is_some_and(|changes| {
            changes
                .iter()
                .any(|change| change.kind == FactChangeKind::Retract)
        })
    });
    let negative_became_present = component
        .rules
        .iter()
        .flat_map(negated_rule_atoms)
        .any(|atom| {
            advance
                .relation_deltas
                .get(&atom.relation)
                .is_some_and(|changes| changes.values().any(|difference| *difference > 0))
        });

    let mut overdeleted = BTreeMap::<RelationId, BTreeSet<Tuple>>::new();
    if target_seed_retracted || negative_became_present {
        overdeleted.clone_from(&old_derived);
    } else {
        overdeleted = overdelete_recursive_derivations(component, &mut advance, &old_derived)?;
    }

    let mut next_derived = BTreeMap::<RelationId, BTreeSet<Tuple>>::new();
    let mut settled = advance.collections.clone();
    for target in &component.targets {
        let remaining = old_derived[target]
            .difference(overdeleted.get(target).unwrap_or(&BTreeSet::new()))
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut visible = extensional_rows(advance.next, *target)?;
        visible.extend(remaining.iter().cloned());
        settled.insert(*target, Arc::new(visible));
        next_derived.insert(*target, remaining);
    }

    let mut settled_arrangements = advance.arrangements.clone();
    for target in &component.targets {
        reset_relation_arrangements(
            &mut settled_arrangements,
            &advance.current.arrangements,
            *target,
        );
        let changes =
            set_difference_changes(&advance.current.collections[target], &settled[target]);
        update_arrangements(&mut settled_arrangements, *target, &changes);
    }
    let needs_full_seed = overdeleted.values().any(|rows| !rows.is_empty())
        || component
            .rules
            .iter()
            .flat_map(negated_rule_atoms)
            .any(|atom| {
                advance
                    .relation_deltas
                    .get(&atom.relation)
                    .is_some_and(|changes| !changes.is_empty())
            });
    let mut frontier = if needs_full_seed {
        recursive_full_candidates(
            component,
            &settled,
            &settled_arrangements,
            &next_derived,
            version,
            advance.execution_context,
            advance.work,
        )?
    } else {
        let seed_deltas = collection_differences(
            component
                .rules
                .iter()
                .flat_map(positive_rule_atoms)
                .map(|atom| atom.relation),
            &advance.current.collections,
            &settled,
        );
        let evaluation = DeltaEvaluation {
            old_collections: &advance.current.collections,
            new_collections: &settled,
            relation_deltas: &seed_deltas,
            old_arrangements: &advance.current.arrangements,
            new_arrangements: &settled_arrangements,
            old_traces: None,
            target: *component.targets.first().unwrap(),
            version,
            execution_context: advance.execution_context,
        };
        recursive_delta_candidates(component, &evaluation, &next_derived, advance.work)?
    };
    let mut iteration = 0;
    while frontier.values().any(|rows| !rows.is_empty()) {
        let changes = frontier
            .iter()
            .map(|(relation, rows)| {
                (
                    *relation,
                    rows.iter()
                        .cloned()
                        .map(|tuple| (tuple, 1))
                        .collect::<WeightedRows>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let settled_frontier = RecursiveFrontier {
            time: LogicalTime {
                epoch: version,
                iteration,
            },
            changes,
        };
        advance.work.recursive_iterations += 1;
        advance.work.frontier_rows.push(
            settled_frontier
                .changes
                .values()
                .map(BTreeMap::len)
                .sum::<usize>(),
        );
        debug_assert_eq!(settled_frontier.time.epoch, version);

        let old_settled = settled.clone();
        let old_arrangements = settled_arrangements.clone();
        for (relation, changes) in &settled_frontier.changes {
            let rows = Arc::make_mut(
                settled
                    .get_mut(relation)
                    .expect("recursive target should have a settled collection"),
            );
            rows.extend(changes.keys().cloned());
            next_derived
                .get_mut(relation)
                .expect("recursive target should have derived state")
                .extend(changes.keys().cloned());
            update_arrangements(&mut settled_arrangements, *relation, changes);
        }
        let evaluation = DeltaEvaluation {
            old_collections: &old_settled,
            new_collections: &settled,
            relation_deltas: &settled_frontier.changes,
            old_arrangements: &old_arrangements,
            new_arrangements: &settled_arrangements,
            old_traces: None,
            target: *component.targets.first().unwrap(),
            version,
            execution_context: advance.execution_context,
        };
        frontier = recursive_delta_candidates(component, &evaluation, &next_derived, advance.work)?;
        iteration = settled_frontier.time.iteration + 1;
    }

    for target in &component.targets {
        let derived = next_derived.remove(target).unwrap_or_default();
        let support = derived
            .iter()
            .cloned()
            .map(|tuple| (tuple, 1))
            .collect::<WeightedRows>();
        let next_rows = settled
            .remove(target)
            .map(Arc::unwrap_or_clone)
            .unwrap_or_default();
        let changes = set_difference_changes(&advance.current.collections[target], &next_rows);
        advance.work.consolidated_changes += changes.len();
        advance.derived_support.insert(*target, support);
        advance.collections.insert(*target, Arc::new(next_rows));
        reset_relation_arrangements(advance.arrangements, &advance.current.arrangements, *target);
        update_arrangements(advance.arrangements, *target, &changes);
        advance.relation_deltas.insert(*target, changes.clone());
        refresh_negative_key_counts(
            advance.negative_key_counts,
            &advance.current.negative_key_counts,
            *target,
            &changes,
        );
    }
    Ok(())
}

fn overdelete_recursive_derivations(
    component: &MaintainedComponent,
    advance: &mut RecursiveAdvance<'_>,
    old_derived: &BTreeMap<RelationId, BTreeSet<Tuple>>,
) -> Result<BTreeMap<RelationId, BTreeSet<Tuple>>, KernelError> {
    let positive_relations = component
        .rules
        .iter()
        .flat_map(positive_rule_atoms)
        .map(|atom| atom.relation)
        .collect::<BTreeSet<_>>();
    let mut frontier = advance
        .relation_deltas
        .iter()
        .filter(|(relation, _)| positive_relations.contains(relation))
        .filter_map(|(relation, changes)| {
            let retractions = changes
                .iter()
                .filter(|(_, difference)| **difference < 0)
                .map(|(tuple, _)| (tuple.clone(), -1))
                .collect::<WeightedRows>();
            (!retractions.is_empty()).then_some((*relation, retractions))
        })
        .collect::<BTreeMap<_, _>>();
    let mut overdeleted = component
        .targets
        .iter()
        .map(|target| (*target, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    let mut collections = advance.current.collections.clone();
    let mut arrangements = advance.current.arrangements.clone();
    let mut iteration = 0;
    while frontier.values().any(|changes| !changes.is_empty()) {
        advance.work.recursive_iterations += 1;
        advance
            .work
            .frontier_rows
            .push(frontier.values().map(BTreeMap::len).sum());
        let time = LogicalTime {
            epoch: advance.next.version(),
            iteration,
        };
        debug_assert_eq!(time.epoch, advance.next.version());

        let mut next_collections = collections.clone();
        let mut next_arrangements = arrangements.clone();
        for (relation, changes) in &frontier {
            let rows = Arc::make_mut(
                next_collections
                    .get_mut(relation)
                    .expect("recursive dependency should have a maintained collection"),
            );
            for tuple in changes.keys() {
                rows.remove(tuple);
            }
            update_arrangements(&mut next_arrangements, *relation, changes);
        }

        let mut next_frontier = BTreeMap::<RelationId, WeightedRows>::new();
        for rule in &component.rules {
            let evaluation = DeltaEvaluation {
                old_collections: &collections,
                new_collections: &next_collections,
                relation_deltas: &frontier,
                old_arrangements: &arrangements,
                new_arrangements: &next_arrangements,
                old_traces: None,
                target: rule.head_relation,
                version: advance.next.version(),
                execution_context: advance.execution_context,
            };
            let bindings = evaluate_positive_binding_delta(rule, &evaluation, advance.work)?;
            let bindings = filter_active_negated_bindings(rule, bindings, &collections)?;
            let candidates = project_rule(rule, bindings, advance.next.version(), advance.work)?;
            for (tuple, difference) in candidates {
                if difference >= 0
                    || !old_derived[&rule.head_relation].contains(&tuple)
                    || !overdeleted
                        .get_mut(&rule.head_relation)
                        .expect("recursive head should have overdelete state")
                        .insert(tuple.clone())
                {
                    continue;
                }
                if !extensional_contains(advance.next, rule.head_relation, &tuple)? {
                    next_frontier
                        .entry(rule.head_relation)
                        .or_default()
                        .insert(tuple, -1);
                }
            }
        }
        collections = next_collections;
        arrangements = next_arrangements;
        frontier = next_frontier;
        iteration = time.iteration + 1;
    }
    Ok(overdeleted)
}

fn recursive_full_candidates(
    component: &MaintainedComponent,
    collections: &Collections,
    arrangements: &BTreeMap<ArrangementSpec, Arc<Arrangement>>,
    derived: &BTreeMap<RelationId, BTreeSet<Tuple>>,
    version: Version,
    execution_context: &ExecutionContext,
    work: &mut MaintenanceWork,
) -> Result<BTreeMap<RelationId, BTreeSet<Tuple>>, KernelError> {
    let mut frontier = BTreeMap::<RelationId, BTreeSet<Tuple>>::new();
    for rule in &component.rules {
        let (candidates, _) = evaluate_rule_full(
            rule,
            collections,
            arrangements,
            version,
            execution_context,
            work,
        )?;
        for (tuple, difference) in candidates {
            if difference > 0 && !derived[&rule.head_relation].contains(&tuple) {
                frontier
                    .entry(rule.head_relation)
                    .or_default()
                    .insert(tuple);
            }
        }
    }
    Ok(frontier)
}

fn recursive_delta_candidates(
    component: &MaintainedComponent,
    sources: &DeltaEvaluation<'_>,
    derived: &BTreeMap<RelationId, BTreeSet<Tuple>>,
    work: &mut MaintenanceWork,
) -> Result<BTreeMap<RelationId, BTreeSet<Tuple>>, KernelError> {
    let mut frontier = BTreeMap::<RelationId, BTreeSet<Tuple>>::new();
    for rule in &component.rules {
        let evaluation = DeltaEvaluation {
            old_collections: sources.old_collections,
            new_collections: sources.new_collections,
            relation_deltas: sources.relation_deltas,
            old_arrangements: sources.old_arrangements,
            new_arrangements: sources.new_arrangements,
            old_traces: sources.old_traces,
            target: rule.head_relation,
            version: sources.version,
            execution_context: sources.execution_context,
        };
        let bindings = evaluate_positive_binding_delta(rule, &evaluation, work)?;
        let bindings = filter_active_negated_bindings(rule, bindings, sources.new_collections)?;
        let candidates = project_rule(rule, bindings, sources.version, work)?;
        for (tuple, difference) in candidates {
            if difference > 0 && !derived[&rule.head_relation].contains(&tuple) {
                frontier
                    .entry(rule.head_relation)
                    .or_default()
                    .insert(tuple);
            }
        }
    }
    Ok(frontier)
}

fn filter_active_negated_bindings(
    rule: &CompiledRule,
    bindings: WeightedBindings,
    collections: &Collections,
) -> Result<WeightedBindings, KernelError> {
    if !has_negation(rule) {
        return Ok(bindings);
    }
    let mut active = WeightedBindings::new();
    for (binding, difference) in bindings {
        if negated_binding_is_active(rule, &binding, &|relation, tuple| {
            collections[&relation].contains(tuple)
        })? {
            active.insert(binding, difference);
        }
    }
    Ok(active)
}

fn collection_differences(
    relations: impl IntoIterator<Item = RelationId>,
    old: &Collections,
    new: &Collections,
) -> BTreeMap<RelationId, WeightedRows> {
    relations
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|relation| {
            let changes = set_difference_changes(&old[&relation], &new[&relation]);
            (!changes.is_empty()).then_some((relation, changes))
        })
        .collect()
}

fn set_difference_changes(old: &BTreeSet<Tuple>, new: &BTreeSet<Tuple>) -> WeightedRows {
    old.difference(new)
        .cloned()
        .map(|tuple| (tuple, -1))
        .chain(new.difference(old).cloned().map(|tuple| (tuple, 1)))
        .collect()
}

fn reset_relation_arrangements(
    arrangements: &mut BTreeMap<ArrangementSpec, Arc<Arrangement>>,
    old_arrangements: &BTreeMap<ArrangementSpec, Arc<Arrangement>>,
    relation: RelationId,
) {
    for (spec, old) in old_arrangements {
        if spec.relation == relation {
            arrangements.insert(spec.clone(), Arc::clone(old));
        }
    }
}

fn evaluate_rule_delta(
    rule: &CompiledRule,
    evaluation: &DeltaEvaluation<'_>,
    work: &mut MaintenanceWork,
) -> Result<WeightedRows, KernelError> {
    let bindings = evaluate_positive_binding_delta(rule, evaluation, work)?;
    project_rule(rule, bindings, evaluation.version, work)
}

fn evaluate_positive_binding_delta(
    rule: &CompiledRule,
    evaluation: &DeltaEvaluation<'_>,
    work: &mut MaintenanceWork,
) -> Result<WeightedBindings, KernelError> {
    let mut output = WeightedBindings::new();
    let mut atoms = positive_rule_atoms(rule).collect::<Vec<_>>();
    if let Some((first, _)) = atoms.iter().enumerate().min_by_key(|(_, atom)| {
        evaluation
            .relation_deltas
            .get(&atom.relation)
            .map_or(usize::MAX, BTreeMap::len)
    }) && evaluation
        .relation_deltas
        .get(&atoms[first].relation)
        .is_some_and(|delta| !delta.is_empty())
    {
        let first = atoms.remove(first);
        atoms.insert(0, first);
    }
    // Expanding one changed atom at a time as NEW * DELTA * OLD accounts for every term in
    // the product exactly once, including commits that change more than one input relation.
    for pivot in 0..atoms.len() {
        let delta = evaluation.relation_deltas.get(&atoms[pivot].relation);
        if delta.is_none_or(BTreeMap::is_empty) {
            continue;
        }
        let mut bindings = unit_binding(rule.slot_count);
        for (index, atom) in atoms.iter().enumerate() {
            bindings = match index.cmp(&pivot) {
                std::cmp::Ordering::Less => join_arranged(
                    bindings,
                    atom,
                    FullJoinInput {
                        rows: evaluation
                            .new_collections
                            .get(&atom.relation)
                            .expect("compiled relation should have a maintained collection"),
                        arrangements: evaluation.new_arrangements,
                        trace: None,
                        target: evaluation.target,
                        version: evaluation.version,
                        execution_context: evaluation.execution_context,
                    },
                    work,
                )?,
                std::cmp::Ordering::Equal => join_weighted(
                    bindings,
                    atom,
                    delta.unwrap(),
                    evaluation.target,
                    evaluation.version,
                    work,
                )?,
                std::cmp::Ordering::Greater => join_arranged(
                    bindings,
                    atom,
                    FullJoinInput {
                        rows: evaluation
                            .old_collections
                            .get(&atom.relation)
                            .expect("compiled relation should have a maintained collection"),
                        arrangements: evaluation.old_arrangements,
                        trace: evaluation
                            .old_traces
                            .and_then(|traces| traces.get(&atom.relation)),
                        target: evaluation.target,
                        version: evaluation.version,
                        execution_context: evaluation.execution_context,
                    },
                    work,
                )?,
            };
            if bindings.is_empty() {
                break;
            }
        }
        bindings = filter_guards(rule, bindings);
        for (binding, difference) in bindings {
            checked_accumulate(
                &mut output,
                binding,
                difference,
                evaluation.target,
                "delta union",
                evaluation.version,
            )?;
        }
    }
    Ok(output)
}

fn evaluate_negated_rule_delta(
    rule: &CompiledRule,
    state: &mut NegatedRuleState,
    evaluation: &DeltaEvaluation<'_>,
    old_right_counts: &BTreeMap<RelationId, BTreeMap<Tuple, usize>>,
    new_right_counts: &BTreeMap<RelationId, BTreeMap<Tuple, usize>>,
    work: &mut MaintenanceWork,
) -> Result<WeightedRows, KernelError> {
    let positive_changed = positive_rule_atoms(rule).any(|atom| {
        evaluation
            .relation_deltas
            .get(&atom.relation)
            .is_some_and(|delta| !delta.is_empty())
    });
    let positive_delta = if positive_changed {
        evaluate_positive_binding_delta(rule, evaluation, work)?
    } else {
        WeightedBindings::new()
    };
    let mut next_bindings = state.positive_bindings.clone();
    for (binding, difference) in &positive_delta {
        checked_accumulate(
            &mut next_bindings,
            binding.clone(),
            *difference,
            rule.head_relation,
            "anti-join left input",
            evaluation.version,
        )?;
    }
    if let Some((binding, difference)) = next_bindings
        .iter()
        .find(|(_, difference)| **difference < 0)
    {
        return Err(KernelError::NegativeDifferentialSupport {
            relation: rule.head_relation,
            tuple: instantiate_head(rule, binding)?,
            version: evaluation.version,
            support: *difference,
        });
    }

    let mut touched = positive_delta.keys().cloned().collect::<BTreeSet<_>>();
    for (negative_index, atom) in negated_rule_atoms(rule).enumerate() {
        let Some(changes) = evaluation.relation_deltas.get(&atom.relation) else {
            continue;
        };
        for tuple in changes.keys() {
            if let Some(bindings) = state.left_by_negative_key[negative_index].get(tuple) {
                touched.extend(bindings.iter().cloned());
            }
        }
    }

    let mut output = WeightedRows::new();
    for binding in touched {
        let old_weight = state.positive_bindings.get(&binding).copied().unwrap_or(0);
        let new_weight = next_bindings.get(&binding).copied().unwrap_or(0);
        let old_active = negated_binding_is_active(rule, &binding, &|relation, tuple| {
            right_key_is_present(old_right_counts, relation, tuple)
        })?;
        let new_active = negated_binding_is_active(rule, &binding, &|relation, tuple| {
            right_key_is_present(new_right_counts, relation, tuple)
        })?;
        let old_contribution = if old_active { old_weight } else { 0 };
        let new_contribution = if new_active { new_weight } else { 0 };
        let difference = new_contribution.checked_sub(old_contribution).ok_or(
            KernelError::DifferentialWeightOverflow {
                relation: rule.head_relation,
                operation: "anti-join transition",
                version: evaluation.version,
                left: new_contribution,
                right: old_contribution,
            },
        )?;
        if difference == 0 {
            continue;
        }
        work.candidate_changes += 1;
        checked_accumulate(
            &mut output,
            instantiate_head(rule, &binding)?,
            difference,
            rule.head_relation,
            "anti-join projection",
            evaluation.version,
        )?;
    }

    for binding in positive_delta.keys() {
        let old_present = state
            .positive_bindings
            .get(binding)
            .is_some_and(|weight| *weight > 0);
        let new_present = next_bindings.get(binding).is_some_and(|weight| *weight > 0);
        if old_present == new_present {
            continue;
        }
        for (negative_index, atom) in negated_rule_atoms(rule).enumerate() {
            let key = instantiate_atom(atom, binding)?;
            let index = &mut state.left_by_negative_key[negative_index];
            if new_present {
                index.entry(key).or_default().insert(binding.clone());
            } else if let Some(bindings) = index.get_mut(&key) {
                bindings.remove(binding);
                if bindings.is_empty() {
                    index.remove(&key);
                }
            }
        }
    }
    state.positive_bindings = next_bindings;
    Ok(output)
}

fn refresh_negative_key_counts(
    counts: &mut BTreeMap<RelationId, BTreeMap<Tuple, usize>>,
    old_counts: &BTreeMap<RelationId, BTreeMap<Tuple, usize>>,
    relation: RelationId,
    changes: &WeightedRows,
) {
    let Some(old) = old_counts.get(&relation) else {
        return;
    };
    let mut next = old.clone();
    for (tuple, difference) in changes {
        if *difference > 0 {
            next.insert(tuple.clone(), 1);
        } else if *difference < 0 {
            next.remove(tuple);
        }
    }
    counts.insert(relation, next);
}

fn right_key_is_present(
    counts: &BTreeMap<RelationId, BTreeMap<Tuple, usize>>,
    relation: RelationId,
    tuple: &Tuple,
) -> bool {
    counts
        .get(&relation)
        .and_then(|counts| counts.get(tuple))
        .is_some_and(|count| *count > 0)
}

fn unit_binding(slot_count: usize) -> WeightedBindings {
    BTreeMap::from([(vec![None; slot_count], 1)])
}

fn join_weighted(
    bindings: WeightedBindings,
    atom: &CompiledAtom,
    rows: &WeightedRows,
    target: RelationId,
    version: Version,
    work: &mut MaintenanceWork,
) -> Result<WeightedBindings, KernelError> {
    work.rows_visited = work
        .rows_visited
        .saturating_add(bindings.len().saturating_mul(rows.len()));
    join_rows(
        bindings,
        atom,
        rows.iter().map(|(tuple, difference)| (tuple, *difference)),
        target,
        version,
    )
}

fn join_weighted_trace(
    bindings: &WeightedBindings,
    atom: &CompiledAtom,
    trace: &Trace,
    target: RelationId,
    version: Version,
    execution_context: &ExecutionContext,
    work: &mut MaintenanceWork,
) -> Result<Option<WeightedBindings>, KernelError> {
    if bindings.is_empty() {
        return Ok(Some(WeightedBindings::new()));
    }
    if !execution_context.accelerates_weighted_joins() {
        return Ok(None);
    }
    let mut positions = None;
    let mut binding_entries = Vec::with_capacity(bindings.len());
    let mut key_rows = Vec::with_capacity(bindings.len());
    for (binding, difference) in bindings {
        let Some((binding_positions, key)) = arrangement_binding_key(atom, binding) else {
            return Ok(None);
        };
        if !matches!(binding_positions.len(), 1 | 2)
            || key.iter().any(|value| !value.is_immediate())
            || positions
                .as_ref()
                .is_some_and(|positions| positions != &binding_positions)
        {
            return Ok(None);
        }
        positions = Some(binding_positions);
        binding_entries.push((binding, *difference));
        key_rows.push(Tuple::new(key));
    }
    let positions = positions.unwrap();
    let delta_positions = (0..positions.len() as u16).collect::<Vec<_>>();
    let Some(delta) = crate::PackedRelation::from_canonical_tuples(key_rows, positions.len())
    else {
        return Ok(None);
    };
    let packed_batches = trace
        .batches()
        .filter(|batch| !batch.rows.is_empty())
        .map(|batch| batch.packed().map(|packed| (batch, packed)))
        .collect::<Option<Vec<_>>>();
    let Some(packed_batches) = packed_batches else {
        return Ok(None);
    };
    let Some(accelerated_batch) = packed_batches.iter().position(|(_, packed)| {
        crate::batch::differential_equality_join_eligible(
            delta.row_count(),
            packed.rows.row_count(),
        )
    }) else {
        return Ok(None);
    };

    let mut output = WeightedBindings::new();
    for (batch_index, (batch, packed)) in packed_batches.into_iter().enumerate() {
        let matches = if batch_index == accelerated_batch {
            let Some(matches) = crate::batch::execute_differential_equality_join(
                &delta,
                &packed.rows,
                &delta_positions,
                &positions,
                execution_context,
            ) else {
                return Ok(None);
            };
            matches
        } else {
            crate::batch::execute_native_equality_join_matches(
                &delta,
                &packed.rows,
                &delta_positions,
                &positions,
            )
            .expect("validated packed trace keys should support a native join")
        };
        work.rows_visited = work.rows_visited.saturating_add(matches.len());
        for pair in matches {
            let (binding, binding_difference) = binding_entries[pair.left_row];
            let tuple = &batch.rows[pair.right_row];
            let Some(next) = unify(atom, binding, tuple) else {
                continue;
            };
            let difference = checked_multiply(
                binding_difference,
                packed.differences[pair.right_row],
                target,
                "packed join",
                version,
            )?;
            checked_accumulate(
                &mut output,
                next,
                difference,
                target,
                "packed join",
                version,
            )?;
        }
    }
    Ok(Some(output))
}

struct FullJoinInput<'a> {
    rows: &'a BTreeSet<Tuple>,
    arrangements: &'a BTreeMap<ArrangementSpec, Arc<Arrangement>>,
    trace: Option<&'a Trace>,
    target: RelationId,
    version: Version,
    execution_context: &'a ExecutionContext,
}

fn join_arranged(
    bindings: WeightedBindings,
    atom: &CompiledAtom,
    input: FullJoinInput<'_>,
    work: &mut MaintenanceWork,
) -> Result<WeightedBindings, KernelError> {
    if let Some(trace) = input.trace
        && let Some(output) = join_weighted_trace(
            &bindings,
            atom,
            trace,
            input.target,
            input.version,
            input.execution_context,
            work,
        )?
    {
        return Ok(output);
    }
    let mut output = WeightedBindings::new();
    for (binding, binding_difference) in bindings {
        let key = arrangement_binding_key(atom, &binding);
        let arranged = key.as_ref().and_then(|(positions, key)| {
            let spec = ArrangementSpec {
                relation: atom.relation,
                positions: positions.clone(),
            };
            input
                .arrangements
                .get(&spec)
                .map(|arrangement| arrangement.lookup(key))
        });
        if let Some(candidates) = arranged {
            work.arrangement_lookups += 1;
            work.rows_visited = work.rows_visited.saturating_add(candidates.len());
            join_candidates(
                &mut output,
                atom,
                &binding,
                binding_difference,
                candidates.iter(),
                input.target,
                input.version,
            )?;
        } else {
            work.rows_visited = work.rows_visited.saturating_add(input.rows.len());
            join_candidates(
                &mut output,
                atom,
                &binding,
                binding_difference,
                input.rows.iter(),
                input.target,
                input.version,
            )?;
        }
    }
    Ok(output)
}

fn join_candidates<'a>(
    output: &mut WeightedBindings,
    atom: &CompiledAtom,
    binding: &Binding,
    binding_difference: Diff,
    candidates: impl IntoIterator<Item = &'a Tuple>,
    target: RelationId,
    version: Version,
) -> Result<(), KernelError> {
    for tuple in candidates {
        let Some(next) = unify(atom, binding, tuple) else {
            continue;
        };
        checked_accumulate(
            output,
            next,
            binding_difference,
            target,
            "arranged join",
            version,
        )?;
    }
    Ok(())
}

fn arrangement_binding_key(
    atom: &CompiledAtom,
    binding: &Binding,
) -> Option<(Vec<u16>, Vec<Value>)> {
    let mut positions = Vec::new();
    let mut key = Vec::new();
    for (position, term) in atom.terms.iter().enumerate() {
        match term {
            CompiledTerm::Value(value) => {
                positions.push(position as u16);
                key.push(value.clone());
            }
            CompiledTerm::Var { slot, .. } if let Some(value) = &binding[*slot] => {
                positions.push(position as u16);
                key.push(value.clone());
            }
            CompiledTerm::Var { .. } => {}
        }
    }
    (!positions.is_empty()).then_some((positions, key))
}

fn join_rows<'a>(
    bindings: WeightedBindings,
    atom: &CompiledAtom,
    rows: impl IntoIterator<Item = (&'a Tuple, Diff)> + Clone,
    target: RelationId,
    version: Version,
) -> Result<WeightedBindings, KernelError> {
    let mut output = WeightedBindings::new();
    for (binding, binding_difference) in bindings {
        for (tuple, tuple_difference) in rows.clone() {
            let Some(next) = unify(atom, &binding, tuple) else {
                continue;
            };
            let difference = checked_multiply(
                binding_difference,
                tuple_difference,
                target,
                "join",
                version,
            )?;
            checked_accumulate(&mut output, next, difference, target, "join", version)?;
        }
    }
    Ok(output)
}

fn unify(atom: &CompiledAtom, binding: &Binding, tuple: &Tuple) -> Option<Binding> {
    let mut next = binding.clone();
    for (term, value) in atom.terms.iter().zip(tuple.values()) {
        match term {
            CompiledTerm::Value(expected) if expected != value => return None,
            CompiledTerm::Value(_) => {}
            CompiledTerm::Var { slot, .. } => {
                if let Some(bound) = &next[*slot] {
                    if bound != value {
                        return None;
                    }
                } else {
                    next[*slot] = Some(value.clone());
                }
            }
        }
    }
    Some(next)
}

fn filter_guards(rule: &CompiledRule, mut bindings: WeightedBindings) -> WeightedBindings {
    bindings.retain(|binding, _| {
        rule.body.iter().all(|item| match item {
            CompiledBodyItem::Atom(_) => true,
            CompiledBodyItem::Guard(guard) => guard_matches(guard, binding),
        })
    });
    bindings
}

fn project_rule(
    rule: &CompiledRule,
    bindings: WeightedBindings,
    version: Version,
    work: &mut MaintenanceWork,
) -> Result<WeightedRows, KernelError> {
    let mut output = WeightedRows::new();
    for (binding, difference) in bindings {
        let tuple = instantiate_head(rule, &binding)?;
        work.candidate_changes += 1;
        checked_accumulate(
            &mut output,
            tuple,
            difference,
            rule.head_relation,
            "projection",
            version,
        )?;
    }
    Ok(output)
}

fn project_active_negated_bindings(
    rule: &CompiledRule,
    bindings: &WeightedBindings,
    right_contains: impl Fn(RelationId, &Tuple) -> bool,
    version: Version,
    work: &mut MaintenanceWork,
) -> Result<WeightedRows, KernelError> {
    let mut output = WeightedRows::new();
    for (binding, difference) in bindings {
        if !negated_binding_is_active(rule, binding, &right_contains)? {
            continue;
        }
        work.candidate_changes += 1;
        checked_accumulate(
            &mut output,
            instantiate_head(rule, binding)?,
            *difference,
            rule.head_relation,
            "anti-join projection",
            version,
        )?;
    }
    Ok(output)
}

fn negated_binding_is_active(
    rule: &CompiledRule,
    binding: &Binding,
    right_contains: &impl Fn(RelationId, &Tuple) -> bool,
) -> Result<bool, KernelError> {
    for atom in negated_rule_atoms(rule) {
        let tuple = instantiate_atom(atom, binding)?;
        if right_contains(atom.relation, &tuple) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn instantiate_atom(atom: &CompiledAtom, binding: &Binding) -> Result<Tuple, KernelError> {
    let mut values = Vec::with_capacity(atom.terms.len());
    for term in &atom.terms {
        match term {
            CompiledTerm::Value(value) => values.push(value.clone()),
            CompiledTerm::Var { slot, .. } => {
                let value = binding[*slot].clone().ok_or(KernelError::Rule(
                    crate::RuleError::UnsafeNegation {
                        relation: atom.relation,
                    },
                ))?;
                values.push(value);
            }
        }
    }
    Ok(Tuple::new(values))
}

fn build_negative_left_indexes(
    rule: &CompiledRule,
    bindings: &WeightedBindings,
) -> Result<Vec<BTreeMap<Tuple, BTreeSet<Binding>>>, KernelError> {
    negated_rule_atoms(rule)
        .map(|atom| {
            let mut index = BTreeMap::<Tuple, BTreeSet<Binding>>::new();
            for binding in bindings.keys() {
                index
                    .entry(instantiate_atom(atom, binding)?)
                    .or_default()
                    .insert(binding.clone());
            }
            Ok(index)
        })
        .collect()
}

fn guard_matches(guard: &CompiledGuard, binding: &Binding) -> bool {
    let Some(left) = term_value(&guard.left, binding) else {
        return false;
    };
    let Some(right) = term_value(&guard.right, binding) else {
        return false;
    };
    compare_values(guard.op, left, right)
}

fn term_value<'a>(term: &'a CompiledTerm, binding: &'a Binding) -> Option<&'a Value> {
    match term {
        CompiledTerm::Value(value) => Some(value),
        CompiledTerm::Var { slot, .. } => binding[*slot].as_ref(),
    }
}

fn instantiate_head(rule: &CompiledRule, binding: &Binding) -> Result<Tuple, KernelError> {
    let mut values = Vec::with_capacity(rule.head_terms.len());
    for term in &rule.head_terms {
        match term {
            CompiledTerm::Value(value) => values.push(value.clone()),
            CompiledTerm::Var { symbol, slot } => {
                let value = binding[*slot].clone().ok_or(KernelError::Rule(
                    crate::RuleError::UnboundHeadVariable { variable: *symbol },
                ))?;
                values.push(value);
            }
        }
    }
    Ok(Tuple::new(values))
}

fn checked_multiply(
    left: Diff,
    right: Diff,
    relation: RelationId,
    operation: &'static str,
    version: Version,
) -> Result<Diff, KernelError> {
    left.checked_mul(right)
        .ok_or(KernelError::DifferentialWeightOverflow {
            relation,
            operation,
            version,
            left,
            right,
        })
}

fn checked_accumulate<K: Ord>(
    values: &mut BTreeMap<K, Diff>,
    key: K,
    difference: Diff,
    relation: RelationId,
    operation: &'static str,
    version: Version,
) -> Result<(), KernelError> {
    if difference == 0 {
        return Ok(());
    }
    match values.entry(key) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(difference);
        }
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            let next = entry.get().checked_add(difference).ok_or(
                KernelError::DifferentialWeightOverflow {
                    relation,
                    operation,
                    version,
                    left: *entry.get(),
                    right: difference,
                },
            )?;
            if next == 0 {
                entry.remove();
            } else {
                *entry.get_mut() = next;
            }
        }
    }
    Ok(())
}

fn accumulate_rows(
    target: &mut WeightedRows,
    rows: WeightedRows,
    relation: RelationId,
    operation: &'static str,
    version: Version,
) -> Result<(), KernelError> {
    for (tuple, difference) in rows {
        checked_accumulate(target, tuple, difference, relation, operation, version)?;
    }
    Ok(())
}

fn ensure_non_negative(
    support: &WeightedRows,
    relation: RelationId,
    version: Version,
) -> Result<(), KernelError> {
    if let Some((tuple, difference)) = support.iter().find(|(_, difference)| **difference < 0) {
        return Err(KernelError::NegativeDifferentialSupport {
            relation,
            tuple: tuple.clone(),
            version,
            support: *difference,
        });
    }
    Ok(())
}

fn positive_rows(support: &WeightedRows) -> BTreeSet<Tuple> {
    support
        .iter()
        .filter(|(_, difference)| **difference > 0)
        .map(|(tuple, _)| tuple.clone())
        .collect()
}

fn support_is_positive(support: Option<&WeightedRows>, tuple: &Tuple) -> bool {
    support
        .and_then(|support| support.get(tuple))
        .is_some_and(|difference| *difference > 0)
}

fn group_fact_changes(changes: &[FactChange]) -> BTreeMap<RelationId, Vec<&FactChange>> {
    let mut grouped = BTreeMap::<RelationId, Vec<&FactChange>>::new();
    for change in changes {
        grouped.entry(change.relation).or_default().push(change);
    }
    grouped
}

fn set_presence_delta(
    collection: &mut BTreeSet<Tuple>,
    deltas: &mut WeightedRows,
    tuple: Tuple,
    old_visible: bool,
    new_visible: bool,
) {
    match (old_visible, new_visible) {
        (false, true) => {
            collection.insert(tuple.clone());
            deltas.insert(tuple, 1);
        }
        (true, false) => {
            collection.remove(&tuple);
            deltas.insert(tuple, -1);
        }
        _ => {
            deltas.remove(&tuple);
        }
    }
}

fn extensional_rows(
    snapshot: &Snapshot,
    relation: RelationId,
) -> Result<BTreeSet<Tuple>, KernelError> {
    let state = snapshot
        .relations
        .get(&relation)
        .ok_or(KernelError::UnknownRelation(relation))?;
    Ok(state
        .scan(&vec![None; state.metadata().arity() as usize])?
        .into_iter()
        .collect())
}

fn extensional_contains(
    snapshot: &Snapshot,
    relation: RelationId,
    tuple: &Tuple,
) -> Result<bool, KernelError> {
    Ok(snapshot
        .relations
        .get(&relation)
        .ok_or(KernelError::UnknownRelation(relation))?
        .contains_tuple(tuple))
}

fn matches_complete_output(
    program: &MaintainedProgram,
    support: &BTreeMap<RelationId, WeightedRows>,
    complete: &BTreeMap<RelationId, RelationState>,
) -> bool {
    program.components.iter().all(|component| {
        component.targets.iter().all(|target| {
            let incremental = support
                .get(target)
                .map(positive_rows)
                .unwrap_or_default()
                .into_iter()
                .collect::<Vec<_>>();
            let complete = complete
                .get(target)
                .map(|state| {
                    state
                        .scan(&vec![None; state.metadata().arity() as usize])
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            incremental == complete
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mica_var::Identity;

    fn relation() -> RelationId {
        Identity::new(900).unwrap()
    }

    #[test]
    fn differential_arithmetic_reports_multiplication_overflow() {
        assert_eq!(
            checked_multiply(i64::MAX, 2, relation(), "join", 17).unwrap_err(),
            KernelError::DifferentialWeightOverflow {
                relation: relation(),
                operation: "join",
                version: 17,
                left: i64::MAX,
                right: 2,
            }
        );
    }

    #[test]
    fn differential_arithmetic_reports_consolidation_overflow() {
        let mut values = BTreeMap::from([(Tuple::from([Value::int(1).unwrap()]), i64::MAX)]);
        assert_eq!(
            checked_accumulate(
                &mut values,
                Tuple::from([Value::int(1).unwrap()]),
                1,
                relation(),
                "projection",
                18,
            )
            .unwrap_err(),
            KernelError::DifferentialWeightOverflow {
                relation: relation(),
                operation: "projection",
                version: 18,
                left: i64::MAX,
                right: 1,
            }
        );
    }

    #[test]
    fn settled_support_cannot_be_negative() {
        let tuple = Tuple::from([Value::int(1).unwrap()]);
        assert_eq!(
            ensure_non_negative(&BTreeMap::from([(tuple.clone(), -1)]), relation(), 19)
                .unwrap_err(),
            KernelError::NegativeDifferentialSupport {
                relation: relation(),
                tuple,
                version: 19,
                support: -1,
            }
        );
    }

    #[test]
    fn weighted_trace_batch_reuses_packed_columns_and_keeps_differences() {
        let batch = TraceBatch::from_rows(
            20,
            [
                (Tuple::from([Value::int(1).unwrap()]), 3),
                (Tuple::from([Value::int(2).unwrap()]), -1),
            ],
        );
        let first = batch.packed().unwrap();
        let second = batch.packed().unwrap();
        assert!(Arc::ptr_eq(&first.rows, &second.rows));
        assert_eq!(first.rows.rows(), batch.rows.as_ref());
        assert_eq!(first.differences.as_ref(), &[3, -1]);
    }
}
