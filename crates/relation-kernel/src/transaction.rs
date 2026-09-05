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

mod overlay;

use crate::computed::ComputedRelationRead;
use crate::index::{RelationMutationKind, RelationState};
use crate::metrics::{CommitOutcome, TransactionReadOperation, TransactionWriteOperation};
use crate::relation_algebra::{
    difference_ordered_tuple_rows, equality_join_tuple_rows, union_ordered_tuple_rows,
};
use crate::snapshot::{Commit, CommitResult, FactChange, FactChangeKind};
use crate::snapshot::{
    active_rules, build_derived_relations, derived_cache_with, empty_derived_cache,
    empty_dispatch_cache, empty_maintained_cache, empty_method_program_cache, empty_packed_cache,
    maintained_cache_with, relation_has_active_rule_head,
};
use crate::{
    ApplicableMethodCall, Conflict, ConflictKind, ConflictPolicy, DispatchRead, DispatchRelations,
    ExecutionContext, KernelError, PackedRelation, RelationCapabilities, RelationId,
    RelationKernel, RelationMetadata, RelationRead, RelationSource, RelationWorkspace, RuleSet,
    ScanControl, Snapshot, Tuple, ValueDomain, Version,
};
use mica_var::{Symbol, Value};
use overlay::{FunctionalVisibleMap, LocalChange, RelationWriteOverlay};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Instant;

type DerivedRelations = HashMap<RelationId, RelationState>;
type TransactionDerivedCache = HashMap<RelationId, Result<DerivedRelations, KernelError>>;

pub struct Transaction<'a> {
    kernel: &'a RelationKernel,
    pub(crate) base: Arc<Snapshot>,
    writes: HashMap<RelationId, RelationWriteOverlay>,
    functional_visible: HashMap<RelationId, FunctionalVisibleMap>,
    derived_cache: RefCell<TransactionDerivedCache>,
    #[cfg(test)]
    differential_overlay_work: RefCell<Option<crate::differential::MaintenanceWork>>,
    dispatch_inline_cache: TransactionDispatchCache,
    execution_context: ExecutionContext,
}

struct TransactionDispatchCache {
    state: Cell<u8>,
    entries: RefCell<Option<Box<TransactionDispatchCacheEntries>>>,
}

#[derive(Default)]
struct TransactionDispatchCacheEntries {
    positional: [Option<PositionalDispatchCacheEntry>; INLINE_DISPATCH_CACHE_CAPACITY],
    method_program: [Option<MethodProgramCacheEntry>; INLINE_DISPATCH_CACHE_CAPACITY],
}

// Keep the first two admitted keys transaction-local. Full caches do not evict on misses, which
// avoids turning workloads with more keys into per-dispatch cache rewrites.
const INLINE_DISPATCH_CACHE_CAPACITY: usize = 2;
const POSITIONAL_DISPATCH_SEEN: u8 = 1 << 0;
const POSITIONAL_DISPATCH_CACHED: u8 = 1 << 1;
const METHOD_PROGRAM_ENABLED: u8 = 1 << 2;
const METHOD_PROGRAM_CACHED: u8 = 1 << 3;

struct PositionalDispatchCacheEntry {
    relations: DispatchRelations,
    selector: Value,
    args: PositionalDispatchArgs,
    methods: Arc<[Value]>,
}

enum PositionalDispatchArgs {
    Empty,
    One(Value),
    Many(Vec<Value>),
}

struct MethodProgramCacheEntry {
    relation: RelationId,
    method: Value,
    program: Option<Value>,
}

impl PositionalDispatchArgs {
    fn new(args: &[Value]) -> Self {
        match args {
            [] => Self::Empty,
            [arg] => Self::One(arg.clone()),
            _ => Self::Many(args.to_vec()),
        }
    }

    fn matches(&self, args: &[Value]) -> bool {
        match (self, args) {
            (Self::Empty, []) => true,
            (Self::One(cached), [arg]) => cached == arg,
            (Self::Many(cached), args) => cached == args,
            _ => false,
        }
    }
}

impl TransactionDispatchCache {
    fn new() -> Self {
        Self {
            state: Cell::new(0),
            entries: RefCell::new(None),
        }
    }

    fn clear(&self) {
        self.state.set(0);
        self.entries.borrow_mut().take();
    }

    fn state(&self) -> u8 {
        self.state.get()
    }

    fn mark_positional_seen(&self, state: u8) {
        self.state.set(state | POSITIONAL_DISPATCH_SEEN);
    }

    fn positional(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        args: &[Value],
        state: u8,
    ) -> Option<Arc<[Value]>> {
        let entries = self.entries.borrow();
        let entry = entries.as_ref().and_then(|entries| {
            entries.positional.iter().flatten().find(|entry| {
                entry.relations == relations
                    && &entry.selector == selector
                    && entry.args.matches(args)
            })
        })?;
        self.state.set(state | METHOD_PROGRAM_ENABLED);
        Some(Arc::clone(&entry.methods))
    }

    fn remember_positional(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        args: &[Value],
        methods: Arc<[Value]>,
    ) {
        let mut cached = self.entries.borrow_mut();
        let entries =
            cached.get_or_insert_with(|| Box::new(TransactionDispatchCacheEntries::default()));
        let Some(entry) = entries.positional.iter_mut().find(|entry| entry.is_none()) else {
            return;
        };
        *entry = Some(PositionalDispatchCacheEntry {
            relations,
            selector: selector.clone(),
            args: PositionalDispatchArgs::new(args),
            methods,
        });
    }

    #[inline(never)]
    fn promote_positional(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        args: &[Value],
        methods: Arc<[Value]>,
        state: u8,
    ) {
        self.remember_positional(relations, selector, args, methods);
        self.state.set(state | POSITIONAL_DISPATCH_CACHED);
    }

    fn method_program(&self, relation: RelationId, method: &Value) -> Option<Option<Value>> {
        let entries = self.entries.borrow();
        entries.as_ref().and_then(|entries| {
            entries
                .method_program
                .iter()
                .flatten()
                .find(|entry| entry.relation == relation && &entry.method == method)
                .map(|entry| entry.program.clone())
        })
    }

    fn consider_method_program(
        &self,
        relation: RelationId,
        method: &Value,
        program: Option<Value>,
        state: u8,
    ) {
        if state & METHOD_PROGRAM_ENABLED == 0 {
            return;
        }
        let mut cached = self.entries.borrow_mut();
        let entries =
            cached.get_or_insert_with(|| Box::new(TransactionDispatchCacheEntries::default()));
        let Some(entry) = entries
            .method_program
            .iter_mut()
            .find(|entry| entry.is_none())
        else {
            return;
        };
        *entry = Some(MethodProgramCacheEntry {
            relation,
            method: method.clone(),
            program,
        });
        self.state.set(state | METHOD_PROGRAM_CACHED);
    }
}

impl<'a> Transaction<'a> {
    pub(crate) fn new(
        kernel: &'a RelationKernel,
        base: Arc<Snapshot>,
        execution_context: ExecutionContext,
    ) -> Self {
        Self {
            kernel,
            base,
            writes: HashMap::new(),
            functional_visible: HashMap::new(),
            derived_cache: RefCell::new(HashMap::new()),
            #[cfg(test)]
            differential_overlay_work: RefCell::new(None),
            dispatch_inline_cache: TransactionDispatchCache::new(),
            execution_context,
        }
    }

    pub fn base_version(&self) -> Version {
        self.base.version()
    }

    pub fn kernel(&self) -> &'a RelationKernel {
        self.kernel
    }

    pub fn execution_context(&self) -> &ExecutionContext {
        &self.execution_context
    }

    pub fn is_read_only(&self) -> bool {
        self.writes.is_empty()
    }

    pub(crate) fn has_local_writes(&self, relation: RelationId) -> bool {
        self.writes.contains_key(&relation)
    }

    pub(crate) fn cached_applicable_method_calls(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        roles: &[(Value, Value)],
    ) -> Result<Vec<ApplicableMethodCall>, KernelError> {
        if !self.dispatch_view_matches_base(relations)? {
            return crate::dispatch::applicable_method_calls_uncached(
                self, relations, selector, roles,
            );
        }
        self.base
            .cached_applicable_method_calls(relations, selector, roles)
    }

    pub(crate) fn cached_applicable_method_calls_normalized(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        roles: &[(Value, Value)],
    ) -> Result<Vec<ApplicableMethodCall>, KernelError> {
        if !self.dispatch_view_matches_base(relations)? {
            return crate::dispatch::applicable_method_calls_uncached(
                self, relations, selector, roles,
            );
        }
        self.base
            .cached_applicable_method_calls_normalized(relations, selector, roles)
    }

    pub(crate) fn cached_method_program(
        &self,
        relation: RelationId,
        method: &Value,
    ) -> Result<Option<Value>, KernelError> {
        let state = self.dispatch_inline_cache.state();
        if state & METHOD_PROGRAM_ENABLED == 0 {
            return self.method_program_from_view(relation, method);
        }
        self.cached_method_program_active(relation, method, state)
    }

    #[inline(never)]
    fn cached_method_program_active(
        &self,
        relation: RelationId,
        method: &Value,
        state: u8,
    ) -> Result<Option<Value>, KernelError> {
        if state & METHOD_PROGRAM_CACHED != 0
            && let Some(program) = self.dispatch_inline_cache.method_program(relation, method)
        {
            return Ok(program);
        }
        let program = self.method_program_from_view(relation, method)?;
        self.dispatch_inline_cache.consider_method_program(
            relation,
            method,
            program.clone(),
            state,
        );
        Ok(program)
    }

    pub(crate) fn cached_applicable_positional_methods(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        args: &[Value],
    ) -> Result<Arc<[Value]>, KernelError> {
        let state = self.dispatch_inline_cache.state();
        if state & POSITIONAL_DISPATCH_CACHED != 0 {
            return self
                .cached_applicable_positional_methods_active(relations, selector, args, state);
        }
        let methods = self.positional_methods_from_view(relations, selector, args)?;
        if state & POSITIONAL_DISPATCH_SEEN == 0 {
            self.dispatch_inline_cache.mark_positional_seen(state);
        } else {
            self.dispatch_inline_cache.promote_positional(
                relations,
                selector,
                args,
                Arc::clone(&methods),
                state,
            );
        }
        Ok(methods)
    }

    #[inline(never)]
    fn cached_applicable_positional_methods_active(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        args: &[Value],
        state: u8,
    ) -> Result<Arc<[Value]>, KernelError> {
        if let Some(methods) = self
            .dispatch_inline_cache
            .positional(relations, selector, args, state)
        {
            return Ok(methods);
        }
        let methods = self.positional_methods_from_view(relations, selector, args)?;
        self.dispatch_inline_cache.remember_positional(
            relations,
            selector,
            args,
            Arc::clone(&methods),
        );
        Ok(methods)
    }

    fn method_program_from_view(
        &self,
        relation: RelationId,
        method: &Value,
    ) -> Result<Option<Value>, KernelError> {
        if self.relation_view_matches_base(relation)? {
            self.base.cached_method_program(relation, method)
        } else {
            crate::dispatch::method_program_id_uncached(self, relation, method)
        }
    }

    fn positional_methods_from_view(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        args: &[Value],
    ) -> Result<Arc<[Value]>, KernelError> {
        if self.dispatch_view_matches_base(relations)? {
            self.base
                .cached_applicable_positional_methods(relations, selector, args)
        } else {
            crate::dispatch::applicable_positional_methods(self, relations, selector.clone(), args)
                .map(Arc::from)
        }
    }

    fn dispatch_view_matches_base(
        &self,
        relations: DispatchRelations,
    ) -> Result<bool, KernelError> {
        if self.writes.is_empty() {
            return Ok(true);
        }
        Ok(self.relation_view_matches_base(relations.method_selector)?
            && self.relation_view_matches_base(relations.param)?
            && self.relation_view_matches_base(relations.delegates)?)
    }

    fn relation_view_matches_base(&self, relation: RelationId) -> Result<bool, KernelError> {
        if self.writes.is_empty() {
            return Ok(true);
        }
        if self.has_local_writes(relation) {
            return Ok(false);
        }
        if relation_has_active_rule_head(self.base.rules(), relation) {
            return Ok(false);
        }
        let metadata = self.base.relation(relation)?.metadata();
        Ok(!self.base.computed_relations.is_computed_relation(metadata))
    }

    pub fn assert(&mut self, relation: RelationId, tuple: Tuple) -> Result<(), KernelError> {
        self.apply_local_change(relation, tuple, LocalChange::Assert)
    }

    pub fn retract(&mut self, relation: RelationId, tuple: Tuple) -> Result<(), KernelError> {
        self.apply_local_change(relation, tuple, LocalChange::Retract)
    }

    pub fn replace_functional(
        &mut self,
        relation: RelationId,
        tuple: Tuple,
    ) -> Result<(), KernelError> {
        self.base.relation(relation)?.validate_tuple(&tuple)?;
        let ConflictPolicy::Functional { .. } =
            self.base.relation(relation)?.metadata().conflict_policy()
        else {
            self.assert(relation, tuple)?;
            return Ok(());
        };
        if let Some(old_tuple) = self.visible_tuple_for_key(relation, &tuple)? {
            self.retract(relation, old_tuple)?;
        }
        let result = self.assert(relation, tuple);
        if result.is_ok() {
            crate::metrics::metrics()
                .transaction_functional_replacements
                .inc();
        }
        result
    }

    pub fn scan(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        crate::metrics::metrics()
            .transaction_read_operations
            .inc(TransactionReadOperation::Scan);
        let metadata = self.base.relation(relation)?.metadata();
        if self.writes.is_empty() {
            let rows = self.base.scan(relation, bindings)?;
            crate::metrics::metrics()
                .transaction_read_rows
                .record(TransactionReadOperation::Scan, rows.len() as u64);
            return Ok(rows);
        }
        if self.base.rules().is_empty() && !self.writes.contains_key(&relation) {
            if self.base.computed_relations.is_computed_relation(metadata) {
                let rows = self.scan_extensional_rows(relation, bindings)?;
                crate::metrics::metrics()
                    .transaction_read_rows
                    .record(TransactionReadOperation::Scan, rows.len() as u64);
                return Ok(rows);
            }
            let rows = self.base.scan_extensional(relation, bindings)?;
            crate::metrics::metrics()
                .transaction_read_rows
                .record(TransactionReadOperation::Scan, rows.len() as u64);
            return Ok(rows);
        }

        let mut visible = self.scan_extensional_rows(relation, bindings)?;

        if relation_has_active_rule_head(self.base.rules(), relation) {
            let derived = self.derived_relations(relation)?;
            if let Some(rows) = derived.get(&relation) {
                visible = union_ordered_tuple_rows(visible, rows.scan(bindings)?);
            }
        }

        crate::metrics::metrics()
            .transaction_read_rows
            .record(TransactionReadOperation::Scan, visible.len() as u64);
        Ok(visible)
    }

    pub(crate) fn estimate_scan(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<usize, KernelError> {
        crate::metrics::metrics()
            .transaction_read_operations
            .inc(TransactionReadOperation::EstimateScan);
        if self.writes.is_empty() {
            let rows = self.base.estimate_scan(relation, bindings)?;
            crate::metrics::metrics()
                .transaction_read_rows
                .record(TransactionReadOperation::EstimateScan, rows as u64);
            return Ok(rows);
        }
        let mut rows = self.estimate_extensional_scan(relation, bindings)?;
        if relation_has_active_rule_head(self.base.rules(), relation)
            && let Some(derived) = self.derived_relations(relation)?.get(&relation)
        {
            rows = rows.saturating_add(derived.estimate_scan_count(bindings)?);
        }
        crate::metrics::metrics()
            .transaction_read_rows
            .record(TransactionReadOperation::EstimateScan, rows as u64);
        Ok(rows)
    }

    fn derived_relations(&self, relation: RelationId) -> Result<DerivedRelations, KernelError> {
        if let Some(derived) = self.derived_cache.borrow().get(&relation).cloned() {
            return derived;
        }
        let derived = self
            .incremental_derived_relations(relation)?
            .map(Ok)
            .unwrap_or_else(|| {
                RuleSet::new(active_rules(self.base.rules()))
                    .evaluate_fixpoint(
                        &ExtensionalTransactionReader { tx: self },
                        &self.execution_context,
                    )
                    .map_err(KernelError::from)
                    .and_then(|derived| build_derived_relations(&self.base.relations, derived))
                    .map(|derived| derived.into_iter().collect())
            });
        self.derived_cache
            .borrow_mut()
            .insert(relation, derived.clone());
        derived
    }

    fn incremental_derived_relations(
        &self,
        relation: RelationId,
    ) -> Result<Option<DerivedRelations>, KernelError> {
        if self.writes.is_empty() {
            return Ok(None);
        }
        let mut maintained = self.base.maintained_state();
        if maintained
            .as_ref()
            .is_none_or(|maintained| !maintained.serves(relation))
        {
            self.base.warm_maintained_relation_result(relation)?;
            maintained = self.base.maintained_state();
        }
        let Some(maintained) = maintained.filter(|maintained| maintained.serves(relation)) else {
            return Ok(None);
        };
        let (overlay, changes) = self.build_overlay_snapshot()?;
        let started = Instant::now();
        let maintained =
            maintained.advance(&self.base, &overlay, &changes, &self.execution_context)?;
        crate::metrics::record_transaction_differential_overlay(
            started.elapsed(),
            maintained.work(),
        );
        #[cfg(test)]
        self.differential_overlay_work
            .replace(Some(maintained.work().clone()));
        Ok(Some(
            maintained
                .build_derived_relations(&overlay)?
                .into_iter()
                .collect(),
        ))
    }

    fn build_overlay_snapshot(&self) -> Result<(Snapshot, Vec<FactChange>), KernelError> {
        let mut overlay = (*self.base).clone();
        let mut changes = Vec::new();
        let mut relation_ids = self.writes.keys().copied().collect::<Vec<_>>();
        relation_ids.sort_unstable();
        for relation_id in relation_ids {
            let writes = &self.writes[&relation_id];
            let relation = overlay
                .relations
                .get_mut(&relation_id)
                .ok_or(KernelError::UnknownRelation(relation_id))?;
            let base_relation = self.base.relation(relation_id)?;
            writes.apply_ordered_changes(relation, base_relation, |tuple, kind| {
                changes.push(FactChange {
                    relation: relation_id,
                    tuple: tuple.clone(),
                    kind: match kind {
                        RelationMutationKind::Assert => FactChangeKind::Assert,
                        RelationMutationKind::Retract => FactChangeKind::Retract,
                    },
                });
            });
        }
        overlay.version = self.base.version() + 1;
        overlay.derived_cache = empty_derived_cache();
        overlay.maintained_cache = empty_maintained_cache();
        overlay.packed_cache = empty_packed_cache();
        overlay.dispatch_cache = empty_dispatch_cache();
        overlay.method_program_cache = empty_method_program_cache();
        Ok((overlay, changes))
    }

    pub(crate) fn estimate_extensional_scan(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<usize, KernelError> {
        let metadata = self.base.relation(relation)?.metadata();
        if self.base.computed_relations.is_computed_relation(metadata) {
            return self
                .base
                .computed_relations
                .estimate(self, metadata, bindings)
                .expect("computed relation should have a registered handler");
        }
        if !self.writes.contains_key(&relation) {
            return self.base.estimate_extensional_scan(relation, bindings);
        }
        let base = self.base.estimate_extensional_scan(relation, bindings)?;
        let local = self.writes[&relation].len();
        Ok(base.saturating_add(local))
    }

    pub fn visit(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
        visitor: &mut dyn FnMut(&Tuple) -> Result<ScanControl, KernelError>,
    ) -> Result<(), KernelError> {
        crate::metrics::metrics()
            .transaction_read_operations
            .inc(TransactionReadOperation::Visit);
        let mut rows = 0usize;
        let metadata = self.base.relation(relation)?.metadata();
        if !relation_has_active_rule_head(self.base.rules(), relation)
            && !self.writes.contains_key(&relation)
        {
            if self.base.computed_relations.is_computed_relation(metadata) {
                for tuple in self.scan_extensional_rows(relation, bindings)? {
                    rows += 1;
                    if visitor(&tuple)? == ScanControl::Stop {
                        break;
                    }
                }
                crate::metrics::metrics()
                    .transaction_read_rows
                    .record(TransactionReadOperation::Visit, rows as u64);
                return Ok(());
            }
            let result = self
                .base
                .visit_extensional(relation, bindings, &mut |tuple| {
                    rows += 1;
                    visitor(tuple)
                });
            if result.is_ok() {
                crate::metrics::metrics()
                    .transaction_read_rows
                    .record(TransactionReadOperation::Visit, rows as u64);
            }
            return result;
        }

        for tuple in self.scan(relation, bindings)? {
            rows += 1;
            if visitor(&tuple)? == ScanControl::Stop {
                break;
            }
        }
        crate::metrics::metrics()
            .transaction_read_rows
            .record(TransactionReadOperation::Visit, rows as u64);
        Ok(())
    }

    pub(crate) fn scan_extensional(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<BTreeSet<Tuple>, KernelError> {
        Ok(self
            .scan_extensional_rows(relation, bindings)?
            .into_iter()
            .collect())
    }

    fn scan_extensional_rows(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        crate::metrics::metrics()
            .transaction_read_operations
            .inc(TransactionReadOperation::ScanExtensional);
        let metadata = self.base.relation(relation)?.metadata();
        if self.base.computed_relations.is_computed_relation(metadata) {
            let rows = self
                .base
                .computed_relations
                .scan(self, metadata, bindings)
                .expect("computed relation should have a registered handler")?;
            crate::metrics::metrics()
                .transaction_read_rows
                .record(TransactionReadOperation::ScanExtensional, rows.len() as u64);
            return Ok(rows);
        }
        let mut visible = self.base.scan_extensional(relation, bindings)?;

        if let Some(writes) = self.writes.get(&relation) {
            let metadata = self.base.relation(relation)?.metadata();
            let mut local_asserts = Vec::new();
            let mut local_retracts = Vec::new();
            writes.visit_matches(metadata, bindings, &mut |tuple, change| {
                if !tuple.matches_bindings(bindings) {
                    return;
                }
                match change {
                    LocalChange::Assert => {
                        local_asserts.push(tuple.clone());
                    }
                    LocalChange::Retract => {
                        local_retracts.push(tuple.clone());
                    }
                }
            });
            if visible.is_empty() && local_retracts.is_empty() {
                crate::metrics::metrics().transaction_read_rows.record(
                    TransactionReadOperation::ScanExtensional,
                    local_asserts.len() as u64,
                );
                return Ok(local_asserts);
            }
            if !local_asserts.is_empty() {
                visible = union_ordered_tuple_rows(visible, local_asserts);
            }
            if !local_retracts.is_empty() {
                visible = difference_ordered_tuple_rows(visible, local_retracts);
            }
        }

        crate::metrics::metrics().transaction_read_rows.record(
            TransactionReadOperation::ScanExtensional,
            visible.len() as u64,
        );
        Ok(visible)
    }

    pub fn relation_metadata(&self, relation: RelationId) -> Option<RelationMetadata> {
        self.base
            .relations
            .get(&relation)
            .map(|relation| relation.metadata().clone())
    }

    fn extensional_facts_with_local_writes(&self) -> Result<Vec<(RelationId, Tuple)>, KernelError> {
        let mut facts = Vec::new();
        for metadata in self.base.relation_metadata() {
            if self.base.computed_relations.is_computed_relation(metadata) {
                continue;
            }
            let bindings = vec![None; metadata.arity() as usize];
            facts.extend(
                self.scan_extensional_rows(metadata.id(), &bindings)?
                    .into_iter()
                    .map(|tuple| (metadata.id(), tuple)),
            );
        }
        facts.sort();
        Ok(facts)
    }

    pub fn reconcile_relation(
        &mut self,
        relation: RelationId,
        desired: impl IntoIterator<Item = Tuple>,
    ) -> Result<(), KernelError> {
        let arity = self.base.relation(relation)?.metadata().arity();
        let desired = desired.into_iter().collect::<BTreeSet<_>>();
        for tuple in &desired {
            if tuple.arity() != arity as usize {
                return Err(KernelError::ArityMismatch {
                    relation,
                    expected: arity,
                    actual: tuple.arity(),
                });
            }
        }

        let current = self
            .scan(relation, &vec![None; arity as usize])?
            .into_iter()
            .collect::<BTreeSet<_>>();

        for tuple in current.difference(&desired) {
            self.retract(relation, tuple.clone())?;
        }
        for tuple in desired.difference(&current) {
            self.assert(relation, tuple.clone())?;
        }
        Ok(())
    }

    pub fn commit(self) -> Result<CommitResult, KernelError> {
        self.commit_with_post_publish(|_| {})
    }

    pub fn commit_with_post_publish(
        self,
        post_publish: impl FnOnce(&CommitResult),
    ) -> Result<CommitResult, KernelError> {
        let start = Instant::now();
        let result = self.commit_inner(post_publish);
        let elapsed = start.elapsed();
        let elapsed_us = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
        crate::metrics::metrics()
            .transaction_commit_duration_us
            .record(elapsed_us);
        crate::metrics::metrics()
            .transaction_commit_duration
            .record_elapsed(elapsed);
        match &result {
            Ok(result) => {
                crate::metrics::metrics()
                    .transaction_commits
                    .inc(CommitOutcome::Committed);
                crate::metrics::metrics()
                    .transaction_commit_changes
                    .record(result.commit().changes().len() as u64);
            }
            Err(KernelError::Conflict(_)) => crate::metrics::metrics()
                .transaction_commits
                .inc(CommitOutcome::Conflict),
            Err(KernelError::Persistence(_)) => crate::metrics::metrics()
                .transaction_commits
                .inc(CommitOutcome::PersistenceError),
            Err(_) => crate::metrics::metrics()
                .transaction_commits
                .inc(CommitOutcome::Error),
        }
        result
    }

    fn commit_inner(
        self,
        post_publish: impl FnOnce(&CommitResult),
    ) -> Result<CommitResult, KernelError> {
        let _guard = self.kernel.commit_guard();
        let current = self.kernel.snapshot();
        if current.version() != self.base.version() {
            self.validate_conflicts(&current)?;
        }
        let (next, commit) = self.build_next_snapshot(&current)?;
        self.kernel.persist_commit(&commit)?;
        if !self.kernel.try_publish(current.version(), next.clone()) {
            return Err(KernelError::Persistence(
                "commit publish failed after serialized persistence".to_owned(),
            ));
        }
        let result = CommitResult {
            snapshot: next,
            commit,
        };
        post_publish(&result);
        Ok(result)
    }

    fn visible_tuple_for_key(
        &mut self,
        relation: RelationId,
        tuple: &Tuple,
    ) -> Result<Option<Tuple>, KernelError> {
        let base_relation = self.base.relation(relation)?;
        let ConflictPolicy::Functional { key_positions } =
            base_relation.metadata().conflict_policy()
        else {
            unreachable!("functional key lookup requires a functional relation");
        };
        let visible = self.functional_visible.entry(relation).or_insert_with(|| {
            FunctionalVisibleMap::from_writes(key_positions, self.writes.get(&relation), |tuple| {
                base_relation.tuple_for_key(key_positions, tuple)
            })
        });
        if let Some(tuple) = visible.tracked_tuple(tuple) {
            return Ok(tuple);
        }
        Ok(base_relation.tuple_for_key(key_positions, tuple))
    }

    fn apply_local_change(
        &mut self,
        relation: RelationId,
        tuple: Tuple,
        change: LocalChange,
    ) -> Result<(), KernelError> {
        let metadata = self.base.relation(relation)?.metadata();
        if self.base.computed_relations.is_computed_relation(metadata) {
            return Err(KernelError::ReadOnlyRelation(relation));
        }
        self.base.relation(relation)?.validate_tuple(&tuple)?;
        if matches!(change, LocalChange::Assert)
            && matches!(
                metadata.conflict_policy(),
                ConflictPolicy::Functional { .. }
            )
            && let Some(existing) = self.visible_tuple_for_key(relation, &tuple)?
            && existing != tuple
        {
            return Err(KernelError::FunctionalKeyViolation {
                relation,
                existing,
                attempted: tuple,
            });
        }
        self.writes
            .entry(relation)
            .or_default()
            .insert(tuple.clone(), change);
        self.dispatch_inline_cache.clear();
        match change {
            LocalChange::Assert => crate::metrics::metrics()
                .transaction_write_operations
                .inc(TransactionWriteOperation::Assert),
            LocalChange::Retract => crate::metrics::metrics()
                .transaction_write_operations
                .inc(TransactionWriteOperation::Retract),
        }
        self.record_functional_change(relation, &tuple, change)?;
        self.derived_cache.borrow_mut().clear();
        #[cfg(test)]
        self.differential_overlay_work.replace(None);
        Ok(())
    }

    fn record_functional_change(
        &mut self,
        relation: RelationId,
        tuple: &Tuple,
        change: LocalChange,
    ) -> Result<(), KernelError> {
        if self.functional_visible.is_empty() {
            return Ok(());
        }

        if !self.functional_visible.contains_key(&relation) {
            return Ok(());
        }
        let base_relation = self.base.relation(relation)?;
        self.functional_visible
            .get_mut(&relation)
            .expect("checked functional visibility map should exist")
            .record_change_with_base_lookup(tuple, change, |positions| {
                base_relation.tuple_for_key(positions, tuple)
            });
        Ok(())
    }

    fn validate_conflicts(&self, current: &Snapshot) -> Result<(), KernelError> {
        for (relation_id, writes) in &self.writes {
            let base_relation = self.base.relation(*relation_id)?;
            let current_relation = current.relation(*relation_id)?;
            match base_relation.metadata().conflict_policy() {
                ConflictPolicy::Set => self.validate_set_conflicts(
                    *relation_id,
                    writes,
                    base_relation,
                    current_relation,
                )?,
                ConflictPolicy::Functional { key_positions } => {
                    self.validate_functional_conflicts(
                        *relation_id,
                        key_positions,
                        writes,
                        base_relation,
                        current_relation,
                    )?;
                }
                ConflictPolicy::EventAppend => {}
            }
        }
        Ok(())
    }

    fn validate_set_conflicts(
        &self,
        relation_id: RelationId,
        writes: &RelationWriteOverlay,
        base_relation: &RelationState,
        current_relation: &RelationState,
    ) -> Result<(), KernelError> {
        writes.try_for_each(|tuple, change| {
            if matches!(change, LocalChange::Assert)
                && base_relation.contains_tuple(tuple)
                && !current_relation.contains_tuple(tuple)
            {
                Err(KernelError::Conflict(Conflict {
                    relation: relation_id,
                    tuple: tuple.clone(),
                    kind: ConflictKind::AssertRetract,
                }))
            } else {
                Ok(())
            }
        })
    }

    fn validate_functional_conflicts(
        &self,
        relation_id: RelationId,
        key_positions: &[u16],
        writes: &RelationWriteOverlay,
        base_relation: &RelationState,
        current_relation: &RelationState,
    ) -> Result<(), KernelError> {
        if let Some(visible) = self.functional_visible.get(&relation_id) {
            for entry in visible.conflict_entries() {
                let base = base_relation.tuple_for_key(key_positions, entry.representative);
                let current = current_relation.tuple_for_key(key_positions, entry.representative);
                if base != current {
                    return Err(KernelError::Conflict(Conflict {
                        relation: relation_id,
                        tuple: entry.tuple.clone().or(base).unwrap_or_else(|| {
                            entry.representative.select(key_positions.iter().copied())
                        }),
                        kind: ConflictKind::FunctionalKeyChanged,
                    }));
                }
            }
            return Ok(());
        }

        writes.visit_touched_projected_keys(key_positions, |key, representative| {
            let base = base_relation.tuple_for_projected_key(key_positions, key);
            let current = current_relation.tuple_for_projected_key(key_positions, key);
            if base != current {
                Err(KernelError::Conflict(Conflict {
                    relation: relation_id,
                    tuple: representative.clone(),
                    kind: ConflictKind::FunctionalKeyChanged,
                }))
            } else {
                Ok(())
            }
        })
    }

    fn build_next_snapshot(
        &self,
        current: &Snapshot,
    ) -> Result<(Arc<Snapshot>, Commit), KernelError> {
        let mut next = current.clone();
        let mut changes = Vec::new();

        let mut relation_ids = self.writes.keys().copied().collect::<Vec<_>>();
        relation_ids.sort();

        for relation_id in relation_ids {
            let writes = self
                .writes
                .get(&relation_id)
                .expect("relation id should come from write set");
            let relation = next
                .relations
                .get_mut(&relation_id)
                .ok_or(KernelError::UnknownRelation(relation_id))?;
            let base_relation = self.base.relation(relation_id)?;
            writes.apply_ordered_changes(relation, base_relation, |tuple, kind| {
                changes.push(FactChange {
                    relation: relation_id,
                    tuple: tuple.clone(),
                    kind: match kind {
                        RelationMutationKind::Assert => FactChangeKind::Assert,
                        RelationMutationKind::Retract => FactChangeKind::Retract,
                    },
                });
            });
        }

        next.version = current.version() + 1;
        next.derived_cache = empty_derived_cache();
        next.maintained_cache = empty_maintained_cache();
        next.packed_cache = empty_packed_cache();
        next.dispatch_cache = empty_dispatch_cache();
        next.method_program_cache = empty_method_program_cache();
        let mut relation_changes = public_fact_changes(current, &next, &changes)?;
        if let Some(maintained) = current.maintained_state() {
            let maintenance_start = Instant::now();
            let maintained =
                maintained.advance(current, &next, &changes, &self.execution_context)?;
            let derived = maintained.build_derived_relations(&next)?;
            crate::metrics::record_differential_maintenance(
                maintenance_start.elapsed(),
                maintained.work(),
            );
            next.derived_cache = derived_cache_with(derived);
            merge_fact_changes(&mut relation_changes, maintained.visible_changes());
            next.maintained_cache = maintained_cache_with(maintained);
        }
        let commit = Commit {
            version: next.version,
            catalog_changes: Arc::from([]),
            changes: changes.into(),
            relation_changes: relation_changes.into(),
            settled_relation_changes_available: true,
        };
        next.commits = current.commits.append(commit.clone());
        Ok((Arc::new(next), commit))
    }
}

fn public_fact_changes(
    current: &Snapshot,
    next: &Snapshot,
    changes: &[FactChange],
) -> Result<Vec<FactChange>, KernelError> {
    let mut visible = Vec::new();
    for change in changes {
        if !relation_has_active_rule_head(current.rules(), change.relation) {
            visible.push(change.clone());
            continue;
        }
        if current
            .maintained_state()
            .is_none_or(|maintained| !maintained.serves(change.relation))
        {
            continue;
        }
        let before = current.contains(change.relation, &change.tuple)?;
        let after = next.contains(change.relation, &change.tuple)?;
        if before == after {
            continue;
        }
        visible.push(FactChange {
            relation: change.relation,
            tuple: change.tuple.clone(),
            kind: if after {
                FactChangeKind::Assert
            } else {
                FactChangeKind::Retract
            },
        });
    }
    Ok(visible)
}

fn merge_fact_changes(changes: &mut Vec<FactChange>, additional: &[FactChange]) {
    changes.extend(additional.iter().cloned());
    changes.sort_by(|left, right| {
        left.relation
            .cmp(&right.relation)
            .then_with(|| left.tuple.cmp(&right.tuple))
    });
    changes.dedup_by(|later, earlier| {
        if later.relation != earlier.relation || later.tuple != earlier.tuple {
            return false;
        }
        earlier.kind = later.kind;
        true
    });
}

impl RelationWorkspace for Transaction<'_> {
    fn assert_tuple(&mut self, relation: RelationId, tuple: Tuple) -> Result<(), KernelError> {
        self.assert(relation, tuple)
    }

    fn retract_tuple(&mut self, relation: RelationId, tuple: Tuple) -> Result<(), KernelError> {
        self.retract(relation, tuple)
    }

    fn replace_functional_tuple(
        &mut self,
        relation: RelationId,
        tuple: Tuple,
    ) -> Result<(), KernelError> {
        self.replace_functional(relation, tuple)
    }
}

impl DispatchRead for Transaction<'_> {
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

impl RelationRead for Transaction<'_> {
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
        if !self.has_local_writes(relation) {
            return self.base.relation_capabilities(relation);
        }
        let mut capabilities = self.base.extensional_relation_capabilities(relation)?;
        capabilities.source = RelationSource::TransactionOverlay;
        capabilities.cardinality = capabilities
            .cardinality
            .map(|rows| rows.saturating_add(self.writes[&relation].len()));
        capabilities.exact_indexes.clear();
        capabilities.value_domains =
            vec![ValueDomain::Unknown; self.base.relation(relation)?.metadata().arity() as usize];
        capabilities.supports_batch_export = false;
        Ok(capabilities)
    }

    fn export_relation_batch(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Option<Arc<PackedRelation>>, KernelError> {
        if self.has_local_writes(relation) {
            return Ok(None);
        }
        self.base.export_relation_batch(relation, bindings)
    }

    fn has_exact_relation_index(
        &self,
        relation: RelationId,
        positions: &[u16],
    ) -> Result<bool, KernelError> {
        self.base.relation_has_exact_index(relation, positions)
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
        if !relation_has_active_rule_head(self.base.rules(), left_relation)
            && !relation_has_active_rule_head(self.base.rules(), right_relation)
            && !self.has_local_writes(left_relation)
            && !self.has_local_writes(right_relation)
            && let Some(rows) = self.base.join_extensional_relation_scans(
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
        Ok(Some(equality_join_tuple_rows(
            left_rows,
            right_rows,
            left_positions,
            right_positions,
        )))
    }
}

impl ComputedRelationRead for Transaction<'_> {
    fn version(&self) -> Version {
        self.base.version()
    }

    fn relation_metadata_vec(&self) -> Vec<crate::RelationMetadata> {
        self.base.relation_metadata().cloned().collect()
    }

    fn relation_id(&self, name: Symbol, arity: u16) -> Option<RelationId> {
        self.base
            .relations
            .values()
            .map(|relation| relation.metadata())
            .find(|metadata| metadata.name() == name && metadata.arity() == arity)
            .map(|metadata| metadata.id())
    }

    fn rules_vec(&self) -> Vec<crate::RuleDefinition> {
        self.base.rules().to_vec()
    }

    fn extensional_facts(&self) -> Result<Vec<(RelationId, Tuple)>, KernelError> {
        self.extensional_facts_with_local_writes()
    }
}

struct ExtensionalTransactionReader<'a, 'kernel> {
    tx: &'a Transaction<'kernel>,
}

impl crate::RelationRead for ExtensionalTransactionReader<'_, '_> {
    fn scan_relation(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        Ok(self
            .tx
            .scan_extensional(relation, bindings)?
            .into_iter()
            .collect())
    }

    fn estimate_relation_scan(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Option<usize>, KernelError> {
        self.tx
            .estimate_extensional_scan(relation, bindings)
            .map(Some)
    }

    fn relation_capabilities(
        &self,
        relation: RelationId,
    ) -> Result<RelationCapabilities, KernelError> {
        if !self.tx.has_local_writes(relation) {
            return self.tx.base.extensional_relation_capabilities(relation);
        }
        let mut capabilities = self.tx.base.extensional_relation_capabilities(relation)?;
        capabilities.source = RelationSource::TransactionOverlay;
        capabilities.cardinality = capabilities
            .cardinality
            .map(|rows| rows.saturating_add(self.tx.writes[&relation].len()));
        capabilities.exact_indexes.clear();
        capabilities.value_domains = vec![
            ValueDomain::Unknown;
            self.tx.base.relation(relation)?.metadata().arity()
                as usize
        ];
        capabilities.supports_batch_export = false;
        Ok(capabilities)
    }

    fn has_exact_relation_index(
        &self,
        relation: RelationId,
        positions: &[u16],
    ) -> Result<bool, KernelError> {
        if self.tx.has_local_writes(relation) {
            return Ok(false);
        }
        self.tx.base.relation_has_exact_index(relation, positions)
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
        if !self.tx.has_local_writes(left_relation)
            && !self.tx.has_local_writes(right_relation)
            && let Some(rows) = self.tx.base.join_extensional_relation_scans(
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
        Ok(None)
    }
}

impl From<LocalChange> for RelationMutationKind {
    fn from(change: LocalChange) -> Self {
        match change {
            LocalChange::Assert => Self::Assert,
            LocalChange::Retract => Self::Retract,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Atom, ComputedRelation, ComputedRelationRead, RelationMetadata, Rule, Term};
    use mica_var::Identity;

    fn rel(id: u64) -> RelationId {
        Identity::new(id).unwrap()
    }

    fn int(value: i64) -> Value {
        Value::int(value).unwrap()
    }

    fn var(name: &str) -> Term {
        Term::Var(Symbol::intern(name))
    }

    #[test]
    fn full_positional_inline_cache_does_not_displace_existing_entries() {
        let cache = TransactionDispatchCache::new();
        let relations = DispatchRelations {
            method_selector: rel(1),
            param: rel(2),
            delegates: rel(3),
        };
        let selectors = [
            Value::symbol(Symbol::intern("first")),
            Value::symbol(Symbol::intern("second")),
            Value::symbol(Symbol::intern("third")),
        ];
        let methods = [
            Arc::<[Value]>::from([Value::identity(rel(11))]),
            Arc::<[Value]>::from([Value::identity(rel(12))]),
            Arc::<[Value]>::from([Value::identity(rel(13))]),
        ];

        for index in 0..INLINE_DISPATCH_CACHE_CAPACITY {
            cache.remember_positional(
                relations,
                &selectors[index],
                &[],
                Arc::clone(&methods[index]),
            );
        }
        for _ in 0..3 {
            cache.remember_positional(relations, &selectors[2], &[], Arc::clone(&methods[2]));
        }

        for index in 0..INLINE_DISPATCH_CACHE_CAPACITY {
            let cached = cache
                .positional(
                    relations,
                    &selectors[index],
                    &[],
                    POSITIONAL_DISPATCH_CACHED,
                )
                .unwrap();
            assert!(Arc::ptr_eq(&cached, &methods[index]));
        }
        assert!(
            cache
                .positional(relations, &selectors[2], &[], POSITIONAL_DISPATCH_CACHED,)
                .is_none()
        );
    }

    #[test]
    fn transaction_overlay_reuses_committed_state_with_less_work_than_complete_evaluation() {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(rel(101), Symbol::intern("Base"), 1))
            .unwrap();
        kernel
            .create_relation(RelationMetadata::new(
                rel(102),
                Symbol::intern("Derived"),
                1,
            ))
            .unwrap();
        kernel
            .install_rule(
                Rule::new(
                    rel(102),
                    [var("item")],
                    [Atom::positive(rel(101), [var("item")])],
                ),
                "Derived(item) :- Base(item)",
            )
            .unwrap();
        let mut seed = kernel.begin();
        for item in 0..4_096 {
            seed.assert(rel(101), Tuple::from([int(item)])).unwrap();
        }
        seed.commit().unwrap();
        let committed = kernel.snapshot();
        assert_eq!(committed.scan(rel(102), &[None]).unwrap().len(), 4_096);
        let committed_maintained = committed.maintained_state().unwrap();

        let mut tx = kernel.begin();
        tx.assert(rel(101), Tuple::from([int(4_096)])).unwrap();
        let complete = RuleSet::new(active_rules(tx.base.rules()))
            .evaluate_fixpoint_with_stats(
                &ExtensionalTransactionReader { tx: &tx },
                &ExecutionContext::serial(),
            )
            .unwrap();
        let actual = tx.scan(rel(102), &[None]).unwrap();
        assert_eq!(actual, complete.derived[&rel(102)]);
        let work = tx.differential_overlay_work.borrow().clone().unwrap();
        assert_eq!(work.input_changes, 1);
        assert_eq!(work.rows_visited, 1);
        assert!(work.rows_visited.saturating_mul(100) < complete.stats.candidate_rows);

        assert_eq!(
            kernel.snapshot().scan(rel(102), &[None]).unwrap().len(),
            4_096
        );
        assert!(Arc::ptr_eq(
            &committed_maintained,
            &kernel.snapshot().maintained_state().unwrap(),
        ));
        drop(tx);
        assert_eq!(
            kernel.snapshot().scan(rel(102), &[None]).unwrap().len(),
            4_096
        );
    }

    struct ConstantComputed;

    impl ComputedRelation for ConstantComputed {
        fn name(&self) -> &'static str {
            "transaction-test-constant"
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
    fn transaction_overlay_preserves_computed_relation_fallback() {
        let kernel = RelationKernel::with_provider_and_computed_relations(
            Arc::new(crate::InMemoryCommitProvider::new()),
            [Arc::new(ConstantComputed) as Arc<dyn ComputedRelation>],
        );
        for (relation, name) in [(111, "Computed"), (112, "Copy"), (113, "Local")] {
            kernel
                .create_relation(RelationMetadata::new(
                    rel(relation),
                    Symbol::intern(name),
                    1,
                ))
                .unwrap();
        }
        kernel
            .install_rule(
                Rule::new(
                    rel(112),
                    [var("value")],
                    [Atom::positive(rel(111), [var("value")])],
                ),
                "Copy(value) :- Computed(value)",
            )
            .unwrap();

        let mut tx = kernel.begin();
        tx.assert(rel(113), Tuple::from([int(1)])).unwrap();
        assert_eq!(
            tx.scan(rel(112), &[None]).unwrap(),
            vec![Tuple::from([int(7)])]
        );
        assert!(tx.differential_overlay_work.borrow().is_none());
        assert!(kernel.snapshot().maintained_state().is_none());
    }

    #[test]
    fn recursive_and_negated_transaction_overlays_match_complete_evaluation() {
        let kernel = RelationKernel::new();
        for (relation, name, arity) in [
            (121, "Edge", 2),
            (122, "Reachable", 2),
            (123, "Node", 1),
            (124, "Blocked", 1),
            (125, "Visible", 1),
        ] {
            kernel
                .create_relation(RelationMetadata::new(
                    rel(relation),
                    Symbol::intern(name),
                    arity,
                ))
                .unwrap();
        }
        kernel
            .install_rule(
                Rule::new(
                    rel(122),
                    [var("from"), var("to")],
                    [Atom::positive(rel(121), [var("from"), var("to")])],
                ),
                "Reachable(from, to) :- Edge(from, to)",
            )
            .unwrap();
        kernel
            .install_rule(
                Rule::new(
                    rel(122),
                    [var("from"), var("to")],
                    [
                        Atom::positive(rel(121), [var("from"), var("middle")]),
                        Atom::positive(rel(122), [var("middle"), var("to")]),
                    ],
                ),
                "Reachable(from, to) :- Edge(from, middle), Reachable(middle, to)",
            )
            .unwrap();
        kernel
            .install_rule(
                Rule::new(
                    rel(125),
                    [var("node")],
                    [
                        Atom::positive(rel(123), [var("node")]),
                        Atom::negated(rel(124), [var("node")]),
                    ],
                ),
                "Visible(node) :- Node(node), !Blocked(node)",
            )
            .unwrap();

        let mut seed = kernel.begin();
        for tuple in [(1, 2), (2, 3)] {
            seed.assert(rel(121), Tuple::from([int(tuple.0), int(tuple.1)]))
                .unwrap();
        }
        for node in [1, 2, 3] {
            seed.assert(rel(123), Tuple::from([int(node)])).unwrap();
        }
        seed.commit().unwrap();
        assert_eq!(
            kernel
                .snapshot()
                .scan(rel(122), &[None, None])
                .unwrap()
                .len(),
            3
        );
        assert_eq!(kernel.snapshot().scan(rel(125), &[None]).unwrap().len(), 3);

        let mut tx = kernel.begin();
        tx.retract(rel(121), Tuple::from([int(1), int(2)])).unwrap();
        tx.assert(rel(121), Tuple::from([int(3), int(4)])).unwrap();
        tx.assert(rel(124), Tuple::from([int(2)])).unwrap();
        let complete = RuleSet::new(active_rules(tx.base.rules()))
            .evaluate_fixpoint_with_stats(
                &ExtensionalTransactionReader { tx: &tx },
                &ExecutionContext::serial(),
            )
            .unwrap();

        assert_eq!(
            tx.scan(rel(122), &[None, None]).unwrap(),
            complete.derived[&rel(122)]
        );
        assert!(tx.differential_overlay_work.borrow().is_some());
        assert_eq!(
            tx.scan(rel(125), &[None]).unwrap(),
            complete.derived[&rel(125)]
        );
        assert!(tx.differential_overlay_work.borrow().is_some());

        tx.commit().unwrap();
        assert_eq!(
            kernel.snapshot().scan(rel(122), &[None, None]).unwrap(),
            complete.derived[&rel(122)]
        );
        assert_eq!(
            kernel.snapshot().scan(rel(125), &[None]).unwrap(),
            complete.derived[&rel(125)]
        );
    }
}
