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

use crate::computed::{ComputedRelationRead, ComputedRelationRegistry};
use crate::differential::MaintainedState;
use crate::dispatch_cache::DispatchCache;
use crate::index::RelationState;
use crate::method_program_cache::MethodProgramCache;
use crate::relation_algebra::union_ordered_tuple_rows;
use crate::relation_states::RelationStates;
use crate::{
    ApplicableMethod, ApplicableMethodCall, DispatchRead, DispatchRelations, KernelError,
    PackedRelation,
    RelationCapabilities, RelationId, RelationMetadata, RelationRead, RelationSource,
    RuleDefinition, RuleEvalError, RuleSet, ScanControl, Tuple, ValueDomain, Version,
};
use mica_var::{Identity, Symbol, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub(crate) struct DerivedCacheEntry {
    complete: bool,
    relations: Result<Arc<BTreeMap<RelationId, RelationState>>, KernelError>,
}

pub(crate) type DerivedCache = Arc<Mutex<Option<DerivedCacheEntry>>>;
pub(crate) type MaintainedCache = Arc<Mutex<Option<Arc<MaintainedState>>>>;
pub(crate) type PackedCache =
    Arc<Mutex<BTreeMap<(RelationId, Vec<Option<Value>>), Arc<PackedRelation>>>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Commit {
    pub(crate) version: Version,
    pub(crate) catalog_changes: Arc<[CatalogChange]>,
    pub(crate) changes: Arc<[FactChange]>,
    pub(crate) relation_changes: Arc<[FactChange]>,
    pub(crate) settled_relation_changes_available: bool,
}

impl Commit {
    pub fn version(&self) -> Version {
        self.version
    }

    pub fn catalog_changes(&self) -> &[CatalogChange] {
        &self.catalog_changes
    }

    pub fn changes(&self) -> &[FactChange] {
        &self.changes
    }

    pub fn relation_changes(&self) -> &[FactChange] {
        &self.relation_changes
    }

    pub fn settled_relation_changes_available(&self) -> bool {
        self.settled_relation_changes_available
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CommitHistory {
    head: Option<Arc<CommitHistoryNode>>,
    len: usize,
}

#[derive(Debug)]
struct CommitHistoryNode {
    commit: Commit,
    previous: Option<Arc<CommitHistoryNode>>,
}

impl CommitHistory {
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    pub(crate) fn from_commits(commits: impl IntoIterator<Item = Commit>) -> Self {
        let mut history = Self::empty();
        for commit in commits {
            history = history.append(commit);
        }
        history
    }

    pub(crate) fn append(&self, commit: Commit) -> Self {
        Self {
            head: Some(Arc::new(CommitHistoryNode {
                commit,
                previous: self.head.clone(),
            })),
            len: self.len + 1,
        }
    }

    pub(crate) fn since(&self, version: Version) -> Vec<Commit> {
        let mut commits = Vec::new();
        let mut current = self.head.as_ref();
        while let Some(node) = current {
            if node.commit.version() <= version {
                break;
            }
            commits.push(node.commit.clone());
            current = node.previous.as_ref();
        }
        commits.reverse();
        commits
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogChange {
    RelationCreated(RelationMetadata),
    RuleInstalled(RuleDefinition),
    RuleDisabled(Identity),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactChange {
    pub relation: RelationId,
    pub tuple: Tuple,
    pub kind: FactChangeKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FactChangeKind {
    Assert,
    Retract,
}

#[derive(Clone, Debug)]
pub struct CommitResult {
    pub(crate) snapshot: Arc<Snapshot>,
    pub(crate) commit: Commit,
}

impl CommitResult {
    pub fn snapshot(&self) -> &Arc<Snapshot> {
        &self.snapshot
    }

    pub fn commit(&self) -> &Commit {
        &self.commit
    }

    pub fn into_snapshot(self) -> Arc<Snapshot> {
        self.snapshot
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub(crate) version: Version,
    pub(crate) relations: RelationStates,
    pub(crate) rules: Vec<RuleDefinition>,
    pub(crate) computed_relations: Arc<ComputedRelationRegistry>,
    pub(crate) derived_cache: DerivedCache,
    pub(crate) maintained_cache: MaintainedCache,
    pub(crate) packed_cache: PackedCache,
    pub(crate) dispatch_cache: DispatchCache,
    pub(crate) method_program_cache: MethodProgramCache,
    pub(crate) commits: CommitHistory,
}

impl Snapshot {
    pub fn version(&self) -> Version {
        self.version
    }

    pub fn scan(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let mut visible = self.scan_extensional(relation, bindings)?;
        if !relation_has_active_rule_head(&self.rules, relation) {
            return Ok(visible);
        }

        let derived = self.derived_relations(relation)?;
        if let Some(rows) = derived.get(&relation) {
            visible = union_ordered_tuple_rows(visible, rows.scan(bindings)?);
        }
        Ok(visible)
    }

    pub fn scan_facts(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        self.scan_extensional(relation, bindings)
    }

    pub fn visit(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
        visitor: &mut dyn FnMut(&Tuple) -> Result<ScanControl, KernelError>,
    ) -> Result<(), KernelError> {
        if !relation_has_active_rule_head(&self.rules, relation) {
            return self.visit_extensional(relation, bindings, visitor);
        }

        for tuple in self.scan(relation, bindings)? {
            if visitor(&tuple)? == ScanControl::Stop {
                break;
            }
        }
        Ok(())
    }

    pub fn contains(&self, relation: RelationId, tuple: &Tuple) -> Result<bool, KernelError> {
        let bindings = tuple.values().iter().cloned().map(Some).collect::<Vec<_>>();
        Ok(!self.scan(relation, &bindings)?.is_empty())
    }

    pub fn commits_since(&self, version: Version) -> Vec<Commit> {
        self.commits.since(version)
    }

    pub fn relation_metadata(&self) -> impl Iterator<Item = &RelationMetadata> {
        let mut metadata = self
            .relations
            .values()
            .map(|relation| relation.metadata())
            .collect::<Vec<_>>();
        metadata.sort_by_key(|metadata| metadata.id());
        metadata.into_iter()
    }

    pub fn relation_metadata_named(&self, name: Symbol) -> Option<&RelationMetadata> {
        self.relations.get_named(name).map(RelationState::metadata)
    }

    pub fn extensional_facts(&self) -> Result<Vec<(RelationId, Tuple)>, KernelError> {
        let mut facts = Vec::new();
        for (relation_id, relation) in self.relations.iter() {
            let bindings = vec![None; relation.metadata().arity() as usize];
            facts.extend(
                relation
                    .scan(&bindings)?
                    .into_iter()
                    .map(|tuple| (relation_id, tuple)),
            );
        }
        facts.sort();
        Ok(facts)
    }

    pub fn rules(&self) -> &[RuleDefinition] {
        &self.rules
    }

    #[cfg(test)]
    pub(crate) fn evaluate_complete_rules(
        &self,
        execution_context: &crate::ExecutionContext,
    ) -> Result<crate::rules::CompleteRuleEvaluation, KernelError> {
        RuleSet::new(active_rules(&self.rules))
            .evaluate_fixpoint_with_stats(
                &ExtensionalSnapshotReader { snapshot: self },
                execution_context,
            )
            .map_err(KernelError::from)
    }

    pub(crate) fn scan_extensional(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let relation = self.relation(relation)?;
        if let Some(rows) = self
            .computed_relations
            .scan(self, relation.metadata(), bindings)
        {
            return rows;
        }
        relation.scan(bindings)
    }

    pub(crate) fn join_extensional_relation_scans(
        &self,
        left_relation: RelationId,
        left_bindings: &[Option<Value>],
        left_positions: &[u16],
        right_relation: RelationId,
        right_bindings: &[Option<Value>],
        right_positions: &[u16],
    ) -> Result<Option<Vec<Tuple>>, KernelError> {
        let left = self.relation(left_relation)?;
        let right = self.relation(right_relation)?;
        if self
            .computed_relations
            .is_computed_relation(left.metadata())
            || self
                .computed_relations
                .is_computed_relation(right.metadata())
        {
            return Ok(None);
        }
        left.join_eq(
            left_bindings,
            left_positions,
            right,
            right_bindings,
            right_positions,
        )
    }

    pub(crate) fn relation_has_exact_index(
        &self,
        relation: RelationId,
        positions: &[u16],
    ) -> Result<bool, KernelError> {
        let relation = self.relation(relation)?;
        if self
            .computed_relations
            .is_computed_relation(relation.metadata())
        {
            return Ok(false);
        }
        Ok(relation.has_exact_index(positions))
    }

    pub(crate) fn estimate_scan(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<usize, KernelError> {
        let mut estimate = self.relation(relation)?.estimate_scan_count(bindings)?;
        if relation_has_active_rule_head(&self.rules, relation)
            && let Some(rows) = self.derived_relations(relation)?.get(&relation)
        {
            estimate += rows.estimate_scan_count(bindings)?;
        }
        Ok(estimate)
    }

    pub(crate) fn estimate_extensional_scan(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<usize, KernelError> {
        let relation = self.relation(relation)?;
        if let Some(estimate) =
            self.computed_relations
                .estimate(self, relation.metadata(), bindings)
        {
            return estimate;
        }
        relation.estimate_scan_count(bindings)
    }

    pub(crate) fn extensional_relation_capabilities(
        &self,
        relation: RelationId,
    ) -> Result<RelationCapabilities, KernelError> {
        let relation = self.relation(relation)?;
        let computed = self
            .computed_relations
            .is_computed_relation(relation.metadata());
        Ok(RelationCapabilities {
            source: if computed {
                RelationSource::Computed
            } else {
                RelationSource::Snapshot
            },
            cardinality: (!computed).then_some(relation.cardinality()),
            exact_indexes: relation
                .metadata()
                .indexes()
                .iter()
                .map(|index| index.positions().to_vec())
                .collect(),
            value_domains: if computed {
                vec![ValueDomain::Unknown; relation.metadata().arity() as usize]
            } else {
                relation.value_domains()
            },
            supports_streaming: true,
            supports_batch_export: !computed,
        })
    }

    pub(crate) fn visit_extensional(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
        visitor: &mut dyn FnMut(&Tuple) -> Result<ScanControl, KernelError>,
    ) -> Result<(), KernelError> {
        let relation = self.relation(relation)?;
        if let Some(rows) = self
            .computed_relations
            .scan(self, relation.metadata(), bindings)
        {
            for tuple in rows? {
                if visitor(&tuple)? == ScanControl::Stop {
                    break;
                }
            }
            return Ok(());
        }
        relation.visit(bindings, visitor)
    }

    pub(crate) fn relation(&self, relation: RelationId) -> Result<&RelationState, KernelError> {
        self.relations
            .get(&relation)
            .ok_or(KernelError::UnknownRelation(relation))
    }

    fn derived_relations(
        &self,
        relation: RelationId,
    ) -> Result<Arc<BTreeMap<RelationId, RelationState>>, KernelError> {
        let cached = self.derived_cache.lock().unwrap().clone();
        if let Some(cached) = cached {
            if cached.complete {
                if let Ok(relations) = &cached.relations {
                    self.warm_maintained_relation(relation, relations)?;
                }
                return cached.relations;
            }
            if self.maintained_serves(relation) {
                return cached.relations;
            }
        }

        let start = std::time::Instant::now();
        let derived = RuleSet::new(active_rules(&self.rules))
            .evaluate_fixpoint(
                &ExtensionalSnapshotReader { snapshot: self },
                &crate::ExecutionContext::serial(),
            )
            .map_err(KernelError::from)
            .and_then(|derived| build_derived_relations(&self.relations, derived))
            .map(Arc::new);
        if let Ok(derived) = &derived {
            self.warm_maintained_relation(relation, derived)?;
            crate::metrics::record_derived_materialization(
                start.elapsed(),
                derived.iter().map(|(relation, state)| {
                    let name = self
                        .relation(*relation)
                        .ok()
                        .and_then(|relation| relation.metadata().name().name())
                        .unwrap_or("<unknown>")
                        .to_owned();
                    (*relation, name, state.cardinality())
                }),
            );
        }
        *self.derived_cache.lock().unwrap() = Some(DerivedCacheEntry {
            complete: true,
            relations: derived.clone(),
        });
        derived
    }

    fn warm_maintained_relation(
        &self,
        relation: RelationId,
        complete: &BTreeMap<RelationId, RelationState>,
    ) -> Result<(), KernelError> {
        let current = self.maintained_state();
        if current.as_ref().is_some_and(|state| state.serves(relation)) {
            return Ok(());
        }
        let mut targets = current
            .as_ref()
            .map(|state| state.requested_targets().clone())
            .unwrap_or_default();
        targets.insert(relation);
        let Some(maintained) = MaintainedState::initialize(self, complete, targets)? else {
            return Ok(());
        };
        *self.maintained_cache.lock().unwrap() = Some(maintained);
        Ok(())
    }

    fn maintained_serves(&self, relation: RelationId) -> bool {
        self.maintained_state()
            .is_some_and(|state| state.serves(relation))
    }

    pub(crate) fn maintained_state(&self) -> Option<Arc<MaintainedState>> {
        self.maintained_cache.lock().unwrap().clone()
    }

    pub(crate) fn warm_maintained_relation_result(
        &self,
        relation: RelationId,
    ) -> Result<(), KernelError> {
        self.derived_relations(relation).map(|_| ())
    }

    pub(crate) fn cached_applicable_method_calls(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        roles: &[(Value, Value)],
    ) -> Result<Vec<ApplicableMethodCall>, KernelError> {
        if let Some(methods) = self.dispatch_cache.get(relations, selector, roles) {
            return Ok(methods);
        }

        let methods =
            crate::dispatch::applicable_method_calls_uncached(self, relations, selector, roles)?;
        self.dispatch_cache
            .insert(relations, selector, roles, methods.clone());
        Ok(methods)
    }

    pub(crate) fn cached_applicable_method_calls_normalized(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        roles: &[(Value, Value)],
    ) -> Result<Vec<ApplicableMethodCall>, KernelError> {
        if let Some(methods) = self
            .dispatch_cache
            .get_normalized(relations, selector, roles)
        {
            return Ok(methods);
        }

        let methods =
            crate::dispatch::applicable_method_calls_uncached(self, relations, selector, roles)?;
        self.dispatch_cache
            .insert_normalized(relations, selector, roles, methods.clone());
        Ok(methods)
    }

    pub(crate) fn cached_applicable_positional_methods(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        args: &[Value],
    ) -> Result<Arc<[Value]>, KernelError> {
        if let Some(methods) = self
            .dispatch_cache
            .get_positional(relations, selector, args)
        {
            return Ok(methods);
        }

        // Reuse the value-independent candidate scan across calls that differ
        // only in argument values, then filter per call. The candidate set is
        // scanned once per selector instead of once per (selector, args).
        let candidates = if let Some(cached) =
            self.dispatch_cache.get_candidates(relations, selector)
        {
            cached
        } else {
            let scanned = crate::dispatch::scan_method_candidates(self, relations, selector)?;
            let shared: Arc<[ApplicableMethod]> = scanned.into();
            self.dispatch_cache.insert_candidates(
                relations,
                selector,
                Arc::clone(&shared),
            );
            shared
        };
        let filtered =
            crate::dispatch::filter_positional_candidates(self, relations, args, &candidates)?;
        let methods: Arc<[Value]> = filtered.into();
        // Only cache the per-call result while there is room; past the bound
        // the candidate cache still avoids the rescan.
        self.dispatch_cache
            .insert_positional(relations, selector, args, Arc::clone(&methods));
        Ok(methods)
    }

    pub(crate) fn cached_method_program(
        &self,
        relation: RelationId,
        method: &Value,
    ) -> Result<Option<Value>, KernelError> {
        if let Some(program) = self.method_program_cache.get(relation, method) {
            return Ok(program);
        }

        let program = crate::dispatch::method_program_id_uncached(self, relation, method)?;
        self.method_program_cache
            .insert(relation, method, program.clone());
        Ok(program)
    }
}

pub(crate) fn build_derived_relations(
    relations: &RelationStates,
    derived: BTreeMap<RelationId, Vec<Tuple>>,
) -> Result<BTreeMap<RelationId, RelationState>, KernelError> {
    derived
        .into_iter()
        .map(|(relation_id, rows)| {
            let metadata = relations
                .get(&relation_id)
                .ok_or(KernelError::UnknownRelation(relation_id))?
                .metadata()
                .clone();
            let mut state = RelationState::empty(metadata)?;
            state.apply_ordered_asserts_to_empty(rows.iter(), |_, _| {});
            Ok((relation_id, state))
        })
        .collect()
}

pub(crate) fn empty_derived_cache() -> DerivedCache {
    Arc::new(Mutex::new(None))
}

pub(crate) fn derived_cache_with(derived: BTreeMap<RelationId, RelationState>) -> DerivedCache {
    Arc::new(Mutex::new(Some(DerivedCacheEntry {
        complete: false,
        relations: Ok(Arc::new(derived)),
    })))
}

pub(crate) fn empty_maintained_cache() -> MaintainedCache {
    Arc::new(Mutex::new(None))
}

pub(crate) fn maintained_cache_with(state: Arc<MaintainedState>) -> MaintainedCache {
    Arc::new(Mutex::new(Some(state)))
}

pub(crate) fn empty_packed_cache() -> PackedCache {
    Arc::new(Mutex::new(BTreeMap::new()))
}

pub(crate) fn empty_dispatch_cache() -> DispatchCache {
    DispatchCache::new()
}

pub(crate) fn empty_method_program_cache() -> MethodProgramCache {
    MethodProgramCache::new()
}

pub(crate) fn active_rules(rules: &[RuleDefinition]) -> Vec<crate::Rule> {
    rules
        .iter()
        .filter(|rule| rule.active())
        .map(|rule| rule.rule().clone())
        .collect()
}

pub(crate) fn relation_has_active_rule_head(
    rules: &[RuleDefinition],
    relation: RelationId,
) -> bool {
    rules
        .iter()
        .any(|rule| rule.active() && rule.rule().head_relation() == relation)
}

impl From<RuleEvalError> for KernelError {
    fn from(value: RuleEvalError) -> Self {
        match value {
            RuleEvalError::Kernel(error) => error,
            RuleEvalError::Rule(error) => Self::Rule(error),
        }
    }
}

impl DispatchRead for Snapshot {
    fn cached_applicable_method_calls(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        roles: &[(Value, Value)],
    ) -> Result<Option<Vec<ApplicableMethodCall>>, KernelError> {
        self.cached_applicable_method_calls(relations, selector, roles)
            .map(Some)
    }

    fn cached_applicable_method_calls_normalized(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        roles: &[(Value, Value)],
    ) -> Result<Option<Vec<ApplicableMethodCall>>, KernelError> {
        self.cached_applicable_method_calls_normalized(relations, selector, roles)
            .map(Some)
    }

    fn cached_method_program(
        &self,
        relation: RelationId,
        method: &Value,
    ) -> Result<Option<Option<Value>>, KernelError> {
        self.cached_method_program(relation, method).map(Some)
    }

    fn cached_method_candidates(
        &self,
        relations: DispatchRelations,
        selector: &Value,
    ) -> Result<Option<Arc<[ApplicableMethod]>>, KernelError> {
        Ok(self.dispatch_cache.get_candidates(relations, selector))
    }

    fn store_method_candidates(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        candidates: Arc<[ApplicableMethod]>,
    ) {
        self.dispatch_cache
            .insert_candidates(relations, selector, candidates);
    }

    fn store_positional_methods(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        args: &[Value],
        methods: Arc<[Value]>,
    ) {
        self.dispatch_cache
            .insert_positional(relations, selector, args, methods);
    }

    fn cached_applicable_positional_methods(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        args: &[Value],
    ) -> Result<Option<Arc<[Value]>>, KernelError> {
        self.cached_applicable_positional_methods(relations, selector, args)
            .map(Some)
    }
}

impl RelationRead for Snapshot {
    fn scan_relation(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        self.scan(relation, bindings)
    }

    fn visit_relation(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
        visitor: &mut dyn FnMut(&Tuple) -> Result<ScanControl, KernelError>,
    ) -> Result<(), KernelError> {
        self.visit(relation, bindings, visitor)
    }

    fn estimate_relation_scan(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Option<usize>, KernelError> {
        self.estimate_scan(relation, bindings).map(Some)
    }

    fn relation_capabilities(
        &self,
        relation: RelationId,
    ) -> Result<RelationCapabilities, KernelError> {
        let mut capabilities = self.extensional_relation_capabilities(relation)?;
        if capabilities.source == RelationSource::Computed
            || !relation_has_active_rule_head(&self.rules, relation)
        {
            return Ok(capabilities);
        }
        if let Some(derived) = self.derived_relations(relation)?.get(&relation) {
            let base_rows = capabilities.cardinality.unwrap_or(0);
            capabilities.cardinality = Some(base_rows.saturating_add(derived.cardinality()));
            capabilities.value_domains = combine_value_domains(
                &capabilities.value_domains,
                base_rows,
                &derived.value_domains(),
                derived.cardinality(),
            );
        }
        Ok(capabilities)
    }

    fn export_relation_batch(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Option<Arc<PackedRelation>>, KernelError> {
        let capabilities = self.relation_capabilities(relation)?;
        if !capabilities.supports_batch_export || !capabilities.immediate_only() {
            return Ok(None);
        }
        let key = (relation, bindings.to_vec());
        if let Some(batch) = self.packed_cache.lock().unwrap().get(&key).cloned() {
            return Ok(Some(batch));
        }
        let rows = self.scan(relation, bindings)?;
        let Some(batch) =
            PackedRelation::from_canonical_tuples(rows, capabilities.value_domains.len())
        else {
            return Ok(None);
        };
        let batch = Arc::new(batch);
        self.packed_cache.lock().unwrap().insert(key, batch.clone());
        Ok(Some(batch))
    }

    fn has_exact_relation_index(
        &self,
        relation: RelationId,
        positions: &[u16],
    ) -> Result<bool, KernelError> {
        self.relation_has_exact_index(relation, positions)
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
        if !relation_has_active_rule_head(&self.rules, left_relation)
            && !relation_has_active_rule_head(&self.rules, right_relation)
            && let Some(rows) = self.join_extensional_relation_scans(
                left_relation,
                left_bindings,
                left_positions,
                right_relation,
                right_bindings,
                right_positions,
            )?
        {
            return Ok(Some(rows));
        }

        let left_rows = self.scan(left_relation, left_bindings)?;
        let right_rows = self.scan(right_relation, right_bindings)?;
        Ok(Some(crate::relation_algebra::equality_join_tuple_rows(
            left_rows,
            right_rows,
            left_positions,
            right_positions,
        )))
    }
}

impl ComputedRelationRead for Snapshot {
    fn version(&self) -> Version {
        Snapshot::version(self)
    }

    fn relation_metadata_vec(&self) -> Vec<RelationMetadata> {
        self.relation_metadata().cloned().collect()
    }

    fn relation_id(&self, name: Symbol, arity: u16) -> Option<RelationId> {
        self.relations
            .values()
            .map(|relation| relation.metadata())
            .find(|metadata| metadata.name() == name && metadata.arity() == arity)
            .map(|metadata| metadata.id())
    }

    fn rules_vec(&self) -> Vec<RuleDefinition> {
        Snapshot::rules(self).to_vec()
    }

    fn index_storage_kind(&self, relation: RelationId, ordinal: usize) -> Option<Symbol> {
        let state = self.relations.get(&relation)?;
        if self
            .computed_relations
            .is_computed_relation(state.metadata())
        {
            return None;
        }
        state.index_storage_kind(ordinal).map(Symbol::intern)
    }

    fn extensional_facts(&self) -> Result<Vec<(RelationId, Tuple)>, KernelError> {
        Snapshot::extensional_facts(self)
    }
}

struct ExtensionalSnapshotReader<'a> {
    snapshot: &'a Snapshot,
}

fn combine_value_domains(
    left: &[ValueDomain],
    left_rows: usize,
    right: &[ValueDomain],
    right_rows: usize,
) -> Vec<ValueDomain> {
    if left_rows == 0 {
        return right.to_vec();
    }
    if right_rows == 0 {
        return left.to_vec();
    }
    left.iter()
        .zip(right)
        .map(|(left, right)| match (*left, *right) {
            (ValueDomain::Unknown, _) | (_, ValueDomain::Unknown) => ValueDomain::Unknown,
            (ValueDomain::Immediate, ValueDomain::Immediate) => ValueDomain::Immediate,
            (ValueDomain::Heap, ValueDomain::Heap) => ValueDomain::Heap,
            _ => ValueDomain::Mixed,
        })
        .collect()
}

impl crate::RelationRead for ExtensionalSnapshotReader<'_> {
    fn scan_relation(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        self.snapshot.scan_extensional(relation, bindings)
    }

    fn estimate_relation_scan(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Option<usize>, KernelError> {
        self.snapshot
            .estimate_extensional_scan(relation, bindings)
            .map(Some)
    }

    fn relation_capabilities(
        &self,
        relation: RelationId,
    ) -> Result<RelationCapabilities, KernelError> {
        self.snapshot.extensional_relation_capabilities(relation)
    }

    fn has_exact_relation_index(
        &self,
        relation: RelationId,
        positions: &[u16],
    ) -> Result<bool, KernelError> {
        self.snapshot.relation_has_exact_index(relation, positions)
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
