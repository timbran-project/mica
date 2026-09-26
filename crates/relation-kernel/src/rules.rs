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

use crate::metrics::record_rule_fixpoint;
use crate::query::PhysicalQueryPlan;
use crate::{
    ExecutionContext, KernelError, PackedRelation, RelationCapabilities, RelationId, RelationRead,
    RelationSource, ScanControl, Tuple, ValueDomain,
};
use mica_var::{Identity, Symbol, Value, language_cmp};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Term {
    Var(Symbol),
    Value(Value),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Atom {
    relation: RelationId,
    terms: Vec<Term>,
    negated: bool,
}

impl Atom {
    pub fn positive(relation: RelationId, terms: impl IntoIterator<Item = Term>) -> Self {
        Self {
            relation,
            terms: terms.into_iter().collect(),
            negated: false,
        }
    }

    pub fn negated(relation: RelationId, terms: impl IntoIterator<Item = Term>) -> Self {
        Self {
            relation,
            terms: terms.into_iter().collect(),
            negated: true,
        }
    }

    pub fn relation(&self) -> RelationId {
        self.relation
    }

    pub fn terms(&self) -> &[Term] {
        &self.terms
    }

    pub fn is_negated(&self) -> bool {
        self.negated
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuleComparisonOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleGuard {
    op: RuleComparisonOp,
    left: Term,
    right: Term,
}

impl RuleGuard {
    pub fn new(op: RuleComparisonOp, left: Term, right: Term) -> Self {
        Self { op, left, right }
    }

    pub fn op(&self) -> RuleComparisonOp {
        self.op
    }

    pub fn left(&self) -> &Term {
        &self.left
    }

    pub fn right(&self) -> &Term {
        &self.right
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuleBodyItem {
    Atom(Atom),
    Guard(RuleGuard),
}

impl From<Atom> for RuleBodyItem {
    fn from(value: Atom) -> Self {
        Self::Atom(value)
    }
}

impl From<RuleGuard> for RuleBodyItem {
    fn from(value: RuleGuard) -> Self {
        Self::Guard(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rule {
    head_relation: RelationId,
    head_terms: Vec<Term>,
    body: Vec<RuleBodyItem>,
}

impl Rule {
    pub fn new<T>(
        head_relation: RelationId,
        head_terms: impl IntoIterator<Item = Term>,
        body: impl IntoIterator<Item = T>,
    ) -> Self
    where
        T: Into<RuleBodyItem>,
    {
        Self {
            head_relation,
            head_terms: head_terms.into_iter().collect(),
            body: body.into_iter().map(Into::into).collect(),
        }
    }

    pub fn head_relation(&self) -> RelationId {
        self.head_relation
    }

    pub fn head_terms(&self) -> &[Term] {
        &self.head_terms
    }

    pub fn body(&self) -> &[RuleBodyItem] {
        &self.body
    }

    pub fn body_atoms(&self) -> impl Iterator<Item = &Atom> {
        self.body.iter().filter_map(|item| match item {
            RuleBodyItem::Atom(atom) => Some(atom),
            RuleBodyItem::Guard(_) => None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleDefinition {
    id: Identity,
    rule: Rule,
    source: String,
    active: bool,
}

impl RuleDefinition {
    pub fn new(id: Identity, rule: Rule, source: impl Into<String>) -> Self {
        Self {
            id,
            rule,
            source: source.into(),
            active: true,
        }
    }

    pub fn id(&self) -> Identity {
        self.id
    }

    pub fn rule(&self) -> &Rule {
        &self.rule
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn active(&self) -> bool {
        self.active
    }

    pub fn deactivate(&mut self) {
        self.active = false;
    }
}

pub struct RuleSet {
    rules: Vec<Rule>,
    compiled: OnceLock<Result<CompiledRuleSet, RuleError>>,
}

impl Clone for RuleSet {
    fn clone(&self) -> Self {
        let compiled = OnceLock::new();
        if let Some(program) = self.compiled.get() {
            compiled.set(program.clone()).unwrap();
        }
        Self {
            rules: self.rules.clone(),
            compiled,
        }
    }
}

impl std::fmt::Debug for RuleSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuleSet")
            .field("rules", &self.rules)
            .finish_non_exhaustive()
    }
}

impl Default for RuleSet {
    fn default() -> Self {
        Self::new([])
    }
}

impl PartialEq for RuleSet {
    fn eq(&self, other: &Self) -> bool {
        self.rules == other.rules
    }
}

impl Eq for RuleSet {}

impl RuleSet {
    pub fn new(rules: impl IntoIterator<Item = Rule>) -> Self {
        Self {
            rules: rules.into_iter().collect(),
            compiled: OnceLock::new(),
        }
    }
}

impl RuleSet {
    pub fn validate_stratified(&self) -> Result<(), RuleError> {
        self.stratified_rules()?;
        Ok(())
    }

    pub fn evaluate(
        &self,
        reader: &impl RelationRead,
        execution_context: &ExecutionContext,
    ) -> Result<BTreeMap<RelationId, Vec<Tuple>>, RuleEvalError> {
        let program = self.compile()?;
        let mut derived: BTreeMap<RelationId, BTreeSet<Tuple>> = BTreeMap::new();

        for stratum in &program.strata {
            let overlay = DerivedReader {
                base: reader,
                derived: &derived,
            };
            let mut stratum_out: BTreeMap<RelationId, BTreeSet<Tuple>> = BTreeMap::new();
            evaluate_rules_once(
                &overlay,
                &stratum.rules,
                &mut stratum_out,
                execution_context,
            )?;
            for (relation, tuples) in stratum_out {
                derived.entry(relation).or_default().extend(tuples);
            }
        }

        Ok(derived
            .into_iter()
            .map(|(relation, tuples)| (relation, tuples.into_iter().collect()))
            .collect())
    }

    pub fn evaluate_fixpoint(
        &self,
        reader: &impl RelationRead,
        execution_context: &ExecutionContext,
    ) -> Result<BTreeMap<RelationId, Vec<Tuple>>, RuleEvalError> {
        let evaluation = self.evaluate_fixpoint_with_stats(reader, execution_context)?;
        let stats = &evaluation.stats;
        record_rule_fixpoint(
            stats.rounds,
            stats.rule_evaluations,
            stats.variant_evaluations,
            stats.candidate_rows,
            stats.novel_rows,
            &stats.frontier_rows,
        );
        Ok(evaluation.derived)
    }

    pub(crate) fn evaluate_fixpoint_with_stats(
        &self,
        reader: &impl RelationRead,
        execution_context: &ExecutionContext,
    ) -> Result<CompleteRuleEvaluation, RuleEvalError> {
        let start = Instant::now();
        let program = self.compile()?;
        let mut derived: BTreeMap<RelationId, BTreeSet<Tuple>> = BTreeMap::new();
        let mut stats = RuleEvaluationStats::default();

        for stratum in &program.strata {
            for component in &stratum.components {
                stats.component_evaluations += 1;
                evaluate_component(
                    reader,
                    &stratum.rules,
                    component,
                    &mut derived,
                    &mut stats,
                    execution_context,
                )?;
            }
        }

        stats.elapsed = start.elapsed();
        Ok(CompleteRuleEvaluation {
            derived: derived
                .into_iter()
                .map(|(relation, tuples)| (relation, tuples.into_iter().collect()))
                .collect(),
            stats,
        })
    }

    #[cfg(test)]
    pub(crate) fn affected_component_count(
        &self,
        changed_relations: impl IntoIterator<Item = RelationId>,
    ) -> Result<usize, RuleError> {
        let program = self.compile()?;
        let mut changed_relations = changed_relations.into_iter().collect::<BTreeSet<_>>();
        let mut affected = 0;
        for stratum in &program.strata {
            for component in &stratum.components {
                let body_changed = component.rule_indices.iter().any(|rule_index| {
                    stratum.rules[*rule_index]
                        .body
                        .iter()
                        .any(|item| match item {
                            CompiledBodyItem::Atom(atom) => {
                                changed_relations.contains(&atom.relation)
                            }
                            CompiledBodyItem::Guard(_) => false,
                        })
                });
                if !body_changed && component.target_relations.is_disjoint(&changed_relations) {
                    continue;
                }
                affected += 1;
                changed_relations.extend(component.target_relations.iter().copied());
            }
        }
        Ok(affected)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Rule> {
        self.rules.iter()
    }

    pub(crate) fn compile(&self) -> Result<&CompiledRuleSet, RuleError> {
        self.compiled
            .get_or_init(|| {
                let strata = self.stratified_rules()?;
                Ok(CompiledRuleSet {
                    strata: strata
                        .into_iter()
                        .map(|rules| compile_stratum(&rules))
                        .collect(),
                })
            })
            .as_ref()
            .map_err(|error| *error)
    }

    fn stratified_rules(&self) -> Result<Vec<Vec<&Rule>>, RuleError> {
        let relation_strata = self.relation_strata()?;
        let mut strata = BTreeMap::<usize, Vec<&Rule>>::new();
        for rule in &self.rules {
            strata
                .entry(relation_strata[&rule.head_relation])
                .or_default()
                .push(rule);
        }
        Ok(strata.into_values().collect())
    }

    fn relation_strata(&self) -> Result<BTreeMap<RelationId, usize>, RuleError> {
        let derived = self
            .rules
            .iter()
            .map(|rule| rule.head_relation)
            .collect::<BTreeSet<_>>();
        let mut strata = derived
            .iter()
            .copied()
            .map(|relation| (relation, 0))
            .collect::<BTreeMap<_, _>>();

        for _ in 0..=derived.len() {
            let mut changed = false;
            for rule in &self.rules {
                let mut head_stratum = strata[&rule.head_relation];
                for atom in rule.body_atoms() {
                    if !derived.contains(&atom.relation) {
                        continue;
                    }
                    let dependency_stratum = strata[&atom.relation];
                    let required = dependency_stratum + usize::from(atom.negated);
                    if head_stratum < required {
                        head_stratum = required;
                    }
                }
                if strata[&rule.head_relation] != head_stratum {
                    strata.insert(rule.head_relation, head_stratum);
                    changed = true;
                }
            }
            if !changed {
                return Ok(strata);
            }
        }

        Err(RuleError::UnstratifiedNegation)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuleError {
    UnstratifiedNegation,
    UnsafeNegation { relation: RelationId },
    UnsafeGuard,
    UnboundHeadVariable { variable: Symbol },
}

#[derive(Debug, Eq, PartialEq)]
pub enum RuleEvalError {
    Rule(RuleError),
    Kernel(KernelError),
}

impl From<RuleError> for RuleEvalError {
    fn from(value: RuleError) -> Self {
        Self::Rule(value)
    }
}

impl From<KernelError> for RuleEvalError {
    fn from(value: KernelError) -> Self {
        Self::Kernel(value)
    }
}

type Binding = Vec<Option<Value>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompiledRule {
    pub(crate) head_relation: RelationId,
    pub(crate) head_terms: Vec<CompiledTerm>,
    pub(crate) body: Vec<CompiledBodyItem>,
    pub(crate) slot_count: usize,
    head_slots: BTreeSet<usize>,
    body_slots: Vec<BTreeSet<usize>>,
    batch_plan: Option<PhysicalQueryPlan>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompiledRuleSet {
    pub(crate) strata: Vec<CompiledStratum>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompiledStratum {
    pub(crate) rules: Vec<CompiledRule>,
    pub(crate) components: Vec<CompiledScc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompiledScc {
    pub(crate) target_relations: BTreeSet<RelationId>,
    pub(crate) rule_indices: Vec<usize>,
    seed_rule_indices: Vec<usize>,
    pub(crate) recursive_variants: Vec<CompiledRuleVariant>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CompiledRuleVariant {
    rule_index: usize,
    delta_body_index: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RuleEvaluationStats {
    pub(crate) component_evaluations: usize,
    pub(crate) rounds: usize,
    pub(crate) rule_evaluations: usize,
    pub(crate) variant_evaluations: usize,
    pub(crate) candidate_rows: usize,
    pub(crate) novel_rows: usize,
    pub(crate) frontier_rows: Vec<usize>,
    pub(crate) elapsed: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompleteRuleEvaluation {
    pub(crate) derived: BTreeMap<RelationId, Vec<Tuple>>,
    pub(crate) stats: RuleEvaluationStats,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CompiledBodyItem {
    Atom(CompiledAtom),
    Guard(CompiledGuard),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompiledAtom {
    pub(crate) relation: RelationId,
    pub(crate) terms: Vec<CompiledTerm>,
    pub(crate) negated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompiledGuard {
    pub(crate) op: RuleComparisonOp,
    pub(crate) left: CompiledTerm,
    pub(crate) right: CompiledTerm,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CompiledTerm {
    Var { symbol: Symbol, slot: usize },
    Value(Value),
}

struct DerivedReader<'a, R> {
    base: &'a R,
    derived: &'a BTreeMap<RelationId, BTreeSet<Tuple>>,
}

struct SccReader<'a, R> {
    base: &'a R,
    completed: &'a BTreeMap<RelationId, BTreeSet<Tuple>>,
    full: &'a BTreeMap<RelationId, BTreeSet<Tuple>>,
}

struct TupleMapReader<'a> {
    tuples: &'a BTreeMap<RelationId, BTreeSet<Tuple>>,
}

impl<R: RelationRead> RelationRead for DerivedReader<'_, R> {
    fn computed_required_bindings(
        &self,
        relation: RelationId,
    ) -> Result<Option<&[u16]>, KernelError> {
        match self.base.computed_required_bindings(relation) {
            Err(KernelError::UnknownRelation(unknown)) if unknown == relation => Ok(None),
            result => result,
        }
    }

    fn scan_relation_batch(
        &self,
        relation: RelationId,
        bindings: &[Vec<Option<Value>>],
    ) -> Result<Option<Vec<crate::ComputedRow>>, KernelError> {
        if self.derived.contains_key(&relation) {
            return Ok(None);
        }
        self.base.scan_relation_batch(relation, bindings)
    }

    fn scan_relation(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let mut rows = match self.base.scan_relation(relation, bindings) {
            Ok(rows) => rows.into_iter().collect::<BTreeSet<_>>(),
            Err(KernelError::UnknownRelation(unknown)) if unknown == relation => BTreeSet::new(),
            Err(error) => return Err(error),
        };
        if let Some(derived) = self.derived.get(&relation) {
            rows.extend(
                derived
                    .iter()
                    .filter(|tuple| tuple.matches_bindings(bindings))
                    .cloned(),
            );
        }
        Ok(rows.into_iter().collect())
    }

    fn visit_relation(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
        visitor: &mut dyn FnMut(&Tuple) -> Result<ScanControl, KernelError>,
    ) -> Result<(), KernelError> {
        if !self.derived.contains_key(&relation) {
            return match self.base.visit_relation(relation, bindings, visitor) {
                Ok(()) => Ok(()),
                Err(KernelError::UnknownRelation(unknown)) if unknown == relation => Ok(()),
                Err(error) => Err(error),
            };
        }

        for tuple in self.scan_relation(relation, bindings)? {
            if visitor(&tuple)? == ScanControl::Stop {
                break;
            }
        }
        Ok(())
    }

    fn estimate_relation_scan(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Option<usize>, KernelError> {
        let base_estimate = match self.base.estimate_relation_scan(relation, bindings) {
            Ok(estimate) => estimate,
            Err(KernelError::UnknownRelation(unknown)) if unknown == relation => Some(0),
            Err(error) => return Err(error),
        };
        let derived_estimate = self.derived.get(&relation).map(BTreeSet::len).unwrap_or(0);
        Ok(base_estimate.map(|estimate| estimate + derived_estimate))
    }

    fn relation_capabilities(
        &self,
        relation: RelationId,
    ) -> Result<RelationCapabilities, KernelError> {
        combined_derived_capabilities(
            relation,
            optional_relation_capabilities(self.base, relation)?,
            self.derived.get(&relation),
            None,
            RelationSource::DerivedFull,
        )
    }

    fn export_relation_batch(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Option<Arc<PackedRelation>>, KernelError> {
        if !self.derived.contains_key(&relation) {
            return self.base.export_relation_batch(relation, bindings);
        }
        export_reader_batch(self, relation, bindings)
    }
}

impl<R: RelationRead> RelationRead for SccReader<'_, R> {
    fn computed_required_bindings(
        &self,
        relation: RelationId,
    ) -> Result<Option<&[u16]>, KernelError> {
        match self.base.computed_required_bindings(relation) {
            Err(KernelError::UnknownRelation(unknown)) if unknown == relation => Ok(None),
            result => result,
        }
    }

    fn scan_relation_batch(
        &self,
        relation: RelationId,
        bindings: &[Vec<Option<Value>>],
    ) -> Result<Option<Vec<crate::ComputedRow>>, KernelError> {
        if self.completed.contains_key(&relation) || self.full.contains_key(&relation) {
            return Ok(None);
        }
        self.base.scan_relation_batch(relation, bindings)
    }

    fn scan_relation(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let mut rows = match self.base.scan_relation(relation, bindings) {
            Ok(rows) => rows.into_iter().collect::<BTreeSet<_>>(),
            Err(KernelError::UnknownRelation(unknown)) if unknown == relation => BTreeSet::new(),
            Err(error) => return Err(error),
        };
        extend_matching(&mut rows, self.completed.get(&relation), bindings);
        extend_matching(&mut rows, self.full.get(&relation), bindings);
        Ok(rows.into_iter().collect())
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
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Option<usize>, KernelError> {
        let base = match self.base.estimate_relation_scan(relation, bindings) {
            Ok(estimate) => estimate,
            Err(KernelError::UnknownRelation(unknown)) if unknown == relation => Some(0),
            Err(error) => return Err(error),
        };
        let completed = self
            .completed
            .get(&relation)
            .map(BTreeSet::len)
            .unwrap_or(0);
        let full = self.full.get(&relation).map(BTreeSet::len).unwrap_or(0);
        Ok(base.map(|base| base.saturating_add(completed).saturating_add(full)))
    }

    fn relation_capabilities(
        &self,
        relation: RelationId,
    ) -> Result<RelationCapabilities, KernelError> {
        combined_derived_capabilities(
            relation,
            optional_relation_capabilities(self.base, relation)?,
            self.completed.get(&relation),
            self.full.get(&relation),
            RelationSource::DerivedFull,
        )
    }

    fn export_relation_batch(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Option<Arc<PackedRelation>>, KernelError> {
        if !self.completed.contains_key(&relation) && !self.full.contains_key(&relation) {
            return self.base.export_relation_batch(relation, bindings);
        }
        export_reader_batch(self, relation, bindings)
    }
}

impl RelationRead for TupleMapReader<'_> {
    fn scan_relation(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        Ok(self
            .tuples
            .get(&relation)
            .into_iter()
            .flatten()
            .filter(|tuple| tuple.matches_bindings(bindings))
            .cloned()
            .collect())
    }

    fn visit_relation(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
        visitor: &mut dyn FnMut(&Tuple) -> Result<ScanControl, KernelError>,
    ) -> Result<(), KernelError> {
        let Some(tuples) = self.tuples.get(&relation) else {
            return Ok(());
        };
        for tuple in tuples {
            if tuple.matches_bindings(bindings) && visitor(tuple)? == ScanControl::Stop {
                break;
            }
        }
        Ok(())
    }

    fn estimate_relation_scan(
        &self,
        relation: RelationId,
        _bindings: &[Option<Value>],
    ) -> Result<Option<usize>, KernelError> {
        Ok(Some(
            self.tuples.get(&relation).map(BTreeSet::len).unwrap_or(0),
        ))
    }

    fn relation_capabilities(
        &self,
        relation: RelationId,
    ) -> Result<RelationCapabilities, KernelError> {
        combined_derived_capabilities(
            relation,
            None,
            self.tuples.get(&relation),
            None,
            RelationSource::DerivedDelta,
        )
    }

    fn export_relation_batch(
        &self,
        relation: RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Option<Arc<PackedRelation>>, KernelError> {
        export_reader_batch(self, relation, bindings)
    }
}

fn export_reader_batch(
    reader: &dyn RelationRead,
    relation: RelationId,
    bindings: &[Option<Value>],
) -> Result<Option<Arc<PackedRelation>>, KernelError> {
    let capabilities = reader.relation_capabilities(relation)?;
    if !capabilities.supports_batch_export || !capabilities.immediate_only() {
        return Ok(None);
    }
    let rows = reader.scan_relation(relation, bindings)?;
    Ok(PackedRelation::from_canonical_tuples(rows, capabilities.value_domains.len()).map(Arc::new))
}

fn optional_relation_capabilities(
    reader: &impl RelationRead,
    relation: RelationId,
) -> Result<Option<RelationCapabilities>, KernelError> {
    match reader.relation_capabilities(relation) {
        Ok(capabilities) => Ok(Some(capabilities)),
        Err(KernelError::UnknownRelation(unknown)) if unknown == relation => Ok(None),
        Err(error) => Err(error),
    }
}

fn combined_derived_capabilities(
    relation: RelationId,
    base: Option<RelationCapabilities>,
    first: Option<&BTreeSet<Tuple>>,
    second: Option<&BTreeSet<Tuple>>,
    source: RelationSource,
) -> Result<RelationCapabilities, KernelError> {
    let base_rows = base.as_ref().and_then(|base| base.cardinality).unwrap_or(0);
    let derived_rows =
        first.map(BTreeSet::len).unwrap_or(0) + second.map(BTreeSet::len).unwrap_or(0);
    let arity = first
        .and_then(|tuples| tuples.first())
        .or_else(|| second.and_then(|tuples| tuples.first()))
        .map(Tuple::arity)
        .or_else(|| base.as_ref().map(|base| base.value_domains.len()))
        .ok_or(KernelError::UnknownRelation(relation))?;
    let derived_domains = tuple_sets_value_domains(first, second, arity);
    let value_domains = match base {
        Some(base) if base_rows > 0 && derived_rows > 0 => base
            .value_domains
            .iter()
            .zip(&derived_domains)
            .map(|(base, derived)| match (*base, *derived) {
                (ValueDomain::Immediate, ValueDomain::Immediate) => ValueDomain::Immediate,
                (ValueDomain::Heap, ValueDomain::Heap) => ValueDomain::Heap,
                (ValueDomain::Unknown, _) | (_, ValueDomain::Unknown) => ValueDomain::Unknown,
                _ => ValueDomain::Mixed,
            })
            .collect(),
        Some(base) if derived_rows == 0 => base.value_domains,
        _ => derived_domains,
    };
    let supports_batch_export = value_domains
        .iter()
        .all(|domain| *domain == ValueDomain::Immediate);
    Ok(RelationCapabilities {
        source,
        cardinality: Some(base_rows.saturating_add(derived_rows)),
        exact_indexes: Vec::new(),
        value_domains,
        supports_streaming: true,
        supports_batch_export,
    })
}

fn tuple_sets_value_domains(
    first: Option<&BTreeSet<Tuple>>,
    second: Option<&BTreeSet<Tuple>>,
    arity: usize,
) -> Vec<ValueDomain> {
    let mut immediate = vec![true; arity];
    let mut heap = vec![true; arity];
    for tuple in first.into_iter().chain(second).flatten() {
        for (position, value) in tuple.values().iter().enumerate() {
            immediate[position] &= value.is_immediate();
            heap[position] &= !value.is_immediate();
        }
    }
    immediate
        .into_iter()
        .zip(heap)
        .map(|(immediate, heap)| match (immediate, heap) {
            (true, _) => ValueDomain::Immediate,
            (_, true) => ValueDomain::Heap,
            _ => ValueDomain::Mixed,
        })
        .collect()
}

fn extend_matching(
    rows: &mut BTreeSet<Tuple>,
    tuples: Option<&BTreeSet<Tuple>>,
    bindings: &[Option<Value>],
) {
    let Some(tuples) = tuples else {
        return;
    };
    rows.extend(
        tuples
            .iter()
            .filter(|tuple| tuple.matches_bindings(bindings))
            .cloned(),
    );
}

fn evaluate_component<R: RelationRead>(
    reader: &R,
    rules: &[CompiledRule],
    component: &CompiledScc,
    derived: &mut BTreeMap<RelationId, BTreeSet<Tuple>>,
    stats: &mut RuleEvaluationStats,
    execution_context: &ExecutionContext,
) -> Result<(), RuleEvalError> {
    if component.recursive_variants.is_empty() {
        let overlay = DerivedReader {
            base: reader,
            derived,
        };
        let mut component_out = BTreeMap::new();
        stats.rule_evaluations += component.seed_rule_indices.len();
        evaluate_selected_rules(
            &overlay,
            rules,
            &component.seed_rule_indices,
            &mut component_out,
            execution_context,
        )?;
        let output_rows = tuple_map_len(&component_out);
        stats.candidate_rows += output_rows;
        stats.novel_rows += output_rows;
        merge_derived(derived, component_out);
        return Ok(());
    }

    evaluate_recursive_component(reader, rules, component, derived, stats, execution_context)
}

fn evaluate_recursive_component<R: RelationRead>(
    reader: &R,
    rules: &[CompiledRule],
    component: &CompiledScc,
    derived: &mut BTreeMap<RelationId, BTreeSet<Tuple>>,
    stats: &mut RuleEvaluationStats,
    execution_context: &ExecutionContext,
) -> Result<(), RuleEvalError> {
    let mut full = read_extensional_targets(reader, rules, component)?;
    let mut delta = full.clone();
    let mut accepted = BTreeMap::<RelationId, BTreeSet<Tuple>>::new();

    let full_reader = SccReader {
        base: reader,
        completed: derived,
        full: &full,
    };
    let mut seed_out = BTreeMap::new();
    stats.rule_evaluations += component.seed_rule_indices.len();
    evaluate_selected_rules(
        &full_reader,
        rules,
        &component.seed_rule_indices,
        &mut seed_out,
        execution_context,
    )?;
    stats.candidate_rows += tuple_map_len(&seed_out);
    for (relation, tuples) in seed_out {
        accepted
            .entry(relation)
            .or_default()
            .extend(tuples.iter().cloned());
        let relation_full = full.entry(relation).or_default();
        let relation_delta = delta.entry(relation).or_default();
        for tuple in tuples {
            if relation_full.insert(tuple.clone()) {
                stats.novel_rows += 1;
                relation_delta.insert(tuple);
            }
        }
    }

    while delta.values().any(|tuples| !tuples.is_empty()) {
        stats.rounds += 1;
        stats.frontier_rows.push(tuple_map_len(&delta));
        let full_reader = SccReader {
            base: reader,
            completed: derived,
            full: &full,
        };
        let delta_reader = TupleMapReader { tuples: &delta };
        let mut candidates = BTreeMap::new();
        stats.variant_evaluations += component.recursive_variants.len();
        evaluate_recursive_variants(
            &full_reader,
            &delta_reader,
            rules,
            &component.recursive_variants,
            &mut candidates,
            execution_context,
        )?;
        stats.candidate_rows += tuple_map_len(&candidates);
        for (relation, tuples) in &candidates {
            accepted
                .entry(*relation)
                .or_default()
                .extend(tuples.iter().cloned());
        }
        let newt = novel_candidates(candidates, &full);
        stats.novel_rows += tuple_map_len(&newt);
        if newt.values().all(BTreeSet::is_empty) {
            break;
        }
        for (relation, tuples) in &newt {
            full.entry(*relation)
                .or_default()
                .extend(tuples.iter().cloned());
        }
        delta = newt;
    }

    merge_derived(derived, accepted);
    Ok(())
}

fn read_extensional_targets<R: RelationRead>(
    reader: &R,
    rules: &[CompiledRule],
    component: &CompiledScc,
) -> Result<BTreeMap<RelationId, BTreeSet<Tuple>>, RuleEvalError> {
    let mut targets = BTreeMap::new();
    for relation in &component.target_relations {
        let arity = component
            .rule_indices
            .iter()
            .map(|index| &rules[*index])
            .find(|rule| rule.head_relation == *relation)
            .map(|rule| rule.head_terms.len())
            .unwrap();
        let bindings = vec![None; arity];
        let tuples = match reader.scan_relation(*relation, &bindings) {
            Ok(tuples) => tuples.into_iter().collect(),
            Err(KernelError::UnknownRelation(unknown)) if unknown == *relation => BTreeSet::new(),
            Err(error) => return Err(error.into()),
        };
        targets.insert(*relation, tuples);
    }
    Ok(targets)
}

fn evaluate_selected_rules(
    reader: &dyn RelationRead,
    rules: &[CompiledRule],
    rule_indices: &[usize],
    out: &mut BTreeMap<RelationId, BTreeSet<Tuple>>,
    execution_context: &ExecutionContext,
) -> Result<(), RuleEvalError> {
    for &rule_index in rule_indices {
        let rule = &rules[rule_index];
        for binding in evaluate_body(reader, rule, execution_context)? {
            out.entry(rule.head_relation)
                .or_default()
                .insert(instantiate_head(rule, &binding)?);
        }
    }
    Ok(())
}

fn evaluate_recursive_variants(
    full_reader: &dyn RelationRead,
    delta_reader: &dyn RelationRead,
    rules: &[CompiledRule],
    variants: &[CompiledRuleVariant],
    out: &mut BTreeMap<RelationId, BTreeSet<Tuple>>,
    execution_context: &ExecutionContext,
) -> Result<(), RuleEvalError> {
    for variant in variants {
        let rule = &rules[variant.rule_index];
        for binding in evaluate_body_variant(
            full_reader,
            delta_reader,
            variant.delta_body_index,
            rule,
            execution_context,
        )? {
            out.entry(rule.head_relation)
                .or_default()
                .insert(instantiate_head(rule, &binding)?);
        }
    }
    Ok(())
}

fn novel_candidates(
    candidates: BTreeMap<RelationId, BTreeSet<Tuple>>,
    full: &BTreeMap<RelationId, BTreeSet<Tuple>>,
) -> BTreeMap<RelationId, BTreeSet<Tuple>> {
    candidates
        .into_iter()
        .map(|(relation, tuples)| {
            let existing = full.get(&relation);
            let novel = tuples
                .into_iter()
                .filter(|tuple| existing.is_none_or(|existing| !existing.contains(tuple)))
                .collect();
            (relation, novel)
        })
        .collect()
}

fn merge_derived(
    derived: &mut BTreeMap<RelationId, BTreeSet<Tuple>>,
    additional: BTreeMap<RelationId, BTreeSet<Tuple>>,
) {
    for (relation, tuples) in additional {
        derived.entry(relation).or_default().extend(tuples);
    }
}

fn tuple_map_len(tuples: &BTreeMap<RelationId, BTreeSet<Tuple>>) -> usize {
    tuples.values().map(BTreeSet::len).sum()
}

fn evaluate_rules_once(
    reader: &impl RelationRead,
    rules: &[CompiledRule],
    out: &mut BTreeMap<RelationId, BTreeSet<Tuple>>,
    execution_context: &ExecutionContext,
) -> Result<(), RuleEvalError> {
    for rule in rules {
        for binding in evaluate_body(reader, rule, execution_context)? {
            out.entry(rule.head_relation)
                .or_default()
                .insert(instantiate_head(rule, &binding)?);
        }
    }
    Ok(())
}

fn compile_stratum(rules: &[&Rule]) -> CompiledStratum {
    let compiled_rules = rules
        .iter()
        .map(|rule| compile_rule(rule))
        .collect::<Vec<_>>();
    let target_relations = rules
        .iter()
        .map(|rule| rule.head_relation)
        .collect::<BTreeSet<_>>();
    let dependencies = positive_dependencies(rules, &target_relations);
    let relation_components = strongly_connected_relations(&target_relations, &dependencies);
    let ordered_components = order_relation_components(&relation_components, &dependencies);
    let components = ordered_components
        .into_iter()
        .map(|relations| compile_component(&compiled_rules, relations))
        .collect();
    CompiledStratum {
        rules: compiled_rules,
        components,
    }
}

fn positive_dependencies(
    rules: &[&Rule],
    target_relations: &BTreeSet<RelationId>,
) -> BTreeMap<RelationId, BTreeSet<RelationId>> {
    let mut dependencies = target_relations
        .iter()
        .copied()
        .map(|relation| (relation, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for rule in rules {
        let head_dependencies = dependencies.get_mut(&rule.head_relation).unwrap();
        for atom in rule.body_atoms() {
            if !atom.negated && target_relations.contains(&atom.relation) {
                head_dependencies.insert(atom.relation);
            }
        }
    }
    dependencies
}

fn strongly_connected_relations(
    relations: &BTreeSet<RelationId>,
    dependencies: &BTreeMap<RelationId, BTreeSet<RelationId>>,
) -> Vec<BTreeSet<RelationId>> {
    let reachable = relations
        .iter()
        .copied()
        .map(|relation| (relation, reachable_relations(relation, dependencies)))
        .collect::<BTreeMap<_, _>>();
    let mut remaining = relations.clone();
    let mut components = Vec::new();
    while let Some(root) = remaining.first().copied() {
        let component = remaining
            .iter()
            .copied()
            .filter(|relation| {
                reachable[&root].contains(relation) && reachable[relation].contains(&root)
            })
            .collect::<BTreeSet<_>>();
        remaining.retain(|relation| !component.contains(relation));
        components.push(component);
    }
    components
}

fn reachable_relations(
    start: RelationId,
    dependencies: &BTreeMap<RelationId, BTreeSet<RelationId>>,
) -> BTreeSet<RelationId> {
    let mut reachable = BTreeSet::new();
    let mut pending = vec![start];
    while let Some(relation) = pending.pop() {
        if !reachable.insert(relation) {
            continue;
        }
        pending.extend(dependencies[&relation].iter().copied());
    }
    reachable
}

fn order_relation_components(
    components: &[BTreeSet<RelationId>],
    dependencies: &BTreeMap<RelationId, BTreeSet<RelationId>>,
) -> Vec<BTreeSet<RelationId>> {
    let relation_components = components
        .iter()
        .enumerate()
        .flat_map(|(index, relations)| {
            relations
                .iter()
                .copied()
                .map(move |relation| (relation, index))
        })
        .collect::<BTreeMap<_, _>>();
    let component_dependencies = components
        .iter()
        .enumerate()
        .map(|(index, relations)| {
            let dependencies = relations
                .iter()
                .flat_map(|relation| dependencies[relation].iter())
                .map(|relation| relation_components[relation])
                .filter(|dependency| *dependency != index)
                .collect::<BTreeSet<_>>();
            (index, dependencies)
        })
        .collect::<BTreeMap<_, _>>();
    let mut ordered = Vec::with_capacity(components.len());
    let mut visited = BTreeSet::new();
    for index in 0..components.len() {
        visit_component(index, &component_dependencies, &mut visited, &mut ordered);
    }
    ordered
        .into_iter()
        .map(|index| components[index].clone())
        .collect()
}

fn visit_component(
    index: usize,
    dependencies: &BTreeMap<usize, BTreeSet<usize>>,
    visited: &mut BTreeSet<usize>,
    ordered: &mut Vec<usize>,
) {
    if !visited.insert(index) {
        return;
    }
    for dependency in &dependencies[&index] {
        visit_component(*dependency, dependencies, visited, ordered);
    }
    ordered.push(index);
}

fn compile_component(
    rules: &[CompiledRule],
    target_relations: BTreeSet<RelationId>,
) -> CompiledScc {
    let rule_indices = rules
        .iter()
        .enumerate()
        .filter_map(|(index, rule)| {
            target_relations
                .contains(&rule.head_relation)
                .then_some(index)
        })
        .collect::<Vec<_>>();
    let mut seed_rule_indices = Vec::new();
    let mut recursive_variants = Vec::new();
    for &rule_index in &rule_indices {
        let recursive_body_indices = rules[rule_index]
            .body
            .iter()
            .enumerate()
            .filter_map(|(body_index, item)| match item {
                CompiledBodyItem::Atom(atom)
                    if !atom.negated && target_relations.contains(&atom.relation) =>
                {
                    Some(body_index)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        if recursive_body_indices.is_empty() {
            seed_rule_indices.push(rule_index);
        }
        recursive_variants.extend(recursive_body_indices.into_iter().map(|delta_body_index| {
            CompiledRuleVariant {
                rule_index,
                delta_body_index,
            }
        }));
    }
    CompiledScc {
        target_relations,
        rule_indices,
        seed_rule_indices,
        recursive_variants,
    }
}

fn compile_rule(rule: &Rule) -> CompiledRule {
    let mut variables = HashMap::new();
    let head_terms: Vec<CompiledTerm> = rule
        .head_terms
        .iter()
        .map(|term| compile_term(term, &mut variables))
        .collect();
    let body: Vec<CompiledBodyItem> = rule
        .body
        .iter()
        .map(|item| match item {
            RuleBodyItem::Atom(atom) => CompiledBodyItem::Atom(CompiledAtom {
                relation: atom.relation,
                terms: atom
                    .terms
                    .iter()
                    .map(|term| compile_term(term, &mut variables))
                    .collect(),
                negated: atom.negated,
            }),
            RuleBodyItem::Guard(guard) => CompiledBodyItem::Guard(CompiledGuard {
                op: guard.op,
                left: compile_term(&guard.left, &mut variables),
                right: compile_term(&guard.right, &mut variables),
            }),
        })
        .collect();
    let head_slots = head_terms.iter().filter_map(compiled_term_slot).collect();
    let body_slots = body.iter().map(compiled_body_slots).collect();
    let batch_plan = compile_two_atom_batch_plan(&body);
    CompiledRule {
        head_relation: rule.head_relation,
        head_terms,
        body,
        slot_count: variables.len(),
        head_slots,
        body_slots,
        batch_plan,
    }
}

fn compile_two_atom_batch_plan(body: &[CompiledBodyItem]) -> Option<PhysicalQueryPlan> {
    let [CompiledBodyItem::Atom(left), CompiledBodyItem::Atom(right)] = body else {
        return None;
    };
    if left.negated || right.negated || !atoms_have_unique_variables(left, right) {
        return None;
    }

    let mut left_positions = Vec::new();
    let mut right_positions = Vec::new();
    for (left_position, left_term) in left.terms.iter().enumerate() {
        let CompiledTerm::Var {
            slot: left_slot, ..
        } = left_term
        else {
            return None;
        };
        for (right_position, right_term) in right.terms.iter().enumerate() {
            if matches!(right_term, CompiledTerm::Var { slot, .. } if slot == left_slot) {
                left_positions.push(left_position as u16);
                right_positions.push(right_position as u16);
            }
        }
    }
    if !matches!(left_positions.len(), 1 | 2) {
        return None;
    }

    Some(PhysicalQueryPlan::JoinEq {
        left: Box::new(PhysicalQueryPlan::Scan {
            relation: left.relation,
            bindings: vec![None; left.terms.len()],
        }),
        right: Box::new(PhysicalQueryPlan::Scan {
            relation: right.relation,
            bindings: vec![None; right.terms.len()],
        }),
        left_positions,
        right_positions,
    })
}

fn compiled_body_slots(item: &CompiledBodyItem) -> BTreeSet<usize> {
    match item {
        CompiledBodyItem::Atom(atom) => atom.terms.iter().filter_map(compiled_term_slot).collect(),
        CompiledBodyItem::Guard(guard) => [&guard.left, &guard.right]
            .into_iter()
            .filter_map(compiled_term_slot)
            .collect(),
    }
}

fn compiled_term_slot(term: &CompiledTerm) -> Option<usize> {
    match term {
        CompiledTerm::Var { slot, .. } => Some(*slot),
        CompiledTerm::Value(_) => None,
    }
}

fn compile_term(term: &Term, variables: &mut HashMap<Symbol, usize>) -> CompiledTerm {
    match term {
        Term::Value(value) => CompiledTerm::Value(value.clone()),
        Term::Var(symbol) => {
            let next_slot = variables.len();
            let slot = *variables.entry(*symbol).or_insert(next_slot);
            CompiledTerm::Var {
                symbol: *symbol,
                slot,
            }
        }
    }
}

fn evaluate_body(
    reader: &dyn RelationRead,
    rule: &CompiledRule,
    execution_context: &ExecutionContext,
) -> Result<Vec<Binding>, RuleEvalError> {
    evaluate_body_with_readers(reader, None, rule, execution_context)
}

fn evaluate_body_variant(
    full_reader: &dyn RelationRead,
    delta_reader: &dyn RelationRead,
    delta_body_index: usize,
    rule: &CompiledRule,
    execution_context: &ExecutionContext,
) -> Result<Vec<Binding>, RuleEvalError> {
    evaluate_body_with_readers(
        full_reader,
        Some((delta_reader, delta_body_index)),
        rule,
        execution_context,
    )
}

fn evaluate_body_with_readers(
    full_reader: &dyn RelationRead,
    delta: Option<(&dyn RelationRead, usize)>,
    rule: &CompiledRule,
    execution_context: &ExecutionContext,
) -> Result<Vec<Binding>, RuleEvalError> {
    if let Some(bindings) = evaluate_two_atom_batch(full_reader, delta, rule, execution_context)? {
        return Ok(bindings);
    }
    let mut bindings = vec![vec![None; rule.slot_count]];
    let mut remaining = rule.body.iter().enumerate().collect::<Vec<_>>();
    while !remaining.is_empty() {
        let next = select_next_item(full_reader, delta, rule, &bindings, &remaining)?;
        let (body_index, item) = remaining.remove(next);
        let reader = reader_for_body_item(full_reader, delta, body_index);
        bindings = match item {
            CompiledBodyItem::Atom(atom) if atom.negated => {
                apply_negated_atom(reader, atom, bindings)?
            }
            CompiledBodyItem::Atom(atom) => apply_positive_atom(reader, atom, bindings)?,
            CompiledBodyItem::Guard(guard) => apply_guard(guard, bindings)?,
        };
    }
    Ok(bindings)
}

fn evaluate_two_atom_batch(
    full_reader: &dyn RelationRead,
    delta: Option<(&dyn RelationRead, usize)>,
    rule: &CompiledRule,
    execution_context: &ExecutionContext,
) -> Result<Option<Vec<Binding>>, RuleEvalError> {
    let [CompiledBodyItem::Atom(left), CompiledBodyItem::Atom(right)] = rule.body.as_slice() else {
        return Ok(None);
    };
    let Some(PhysicalQueryPlan::JoinEq {
        left: plan_left,
        right: plan_right,
        left_positions,
        right_positions,
    }) = &rule.batch_plan
    else {
        return Ok(None);
    };
    let (
        PhysicalQueryPlan::Scan {
            relation: left_relation,
            bindings: left_bindings,
        },
        PhysicalQueryPlan::Scan {
            relation: right_relation,
            bindings: right_bindings,
        },
    ) = (plan_left.as_ref(), plan_right.as_ref())
    else {
        return Ok(None);
    };

    let left_reader = reader_for_body_item(full_reader, delta, 0);
    let right_reader = reader_for_body_item(full_reader, delta, 1);
    let output = crate::batch::execute_packed_relation_join(
        crate::batch::PackedJoinInput {
            reader: left_reader,
            relation: *left_relation,
            bindings: left_bindings,
        },
        crate::batch::PackedJoinInput {
            reader: right_reader,
            relation: *right_relation,
            bindings: right_bindings,
        },
        left_positions,
        right_positions,
        execution_context,
        |rows| {
            let mut output = Vec::with_capacity(rows.row_count());
            let empty = vec![None; rule.slot_count];
            for row in 0..rows.row_count() {
                let Some(binding) = unify_packed_row(left, &empty, &rows, row, 0) else {
                    continue;
                };
                if let Some(binding) =
                    unify_packed_row(right, &binding, &rows, row, left.terms.len())
                {
                    output.push(binding);
                }
            }
            output
        },
    )?;
    Ok(output)
}

fn unify_packed_row(
    atom: &CompiledAtom,
    binding: &Binding,
    rows: &crate::batch::PackedRows<'_>,
    row: usize,
    column_offset: usize,
) -> Option<Binding> {
    let mut next = binding.clone();
    for (position, term) in atom.terms.iter().enumerate() {
        let value = rows.value(row, column_offset + position);
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

fn atoms_have_unique_variables(left: &CompiledAtom, right: &CompiledAtom) -> bool {
    [left, right].into_iter().all(|atom| {
        let slots = atom
            .terms
            .iter()
            .filter_map(|term| match term {
                CompiledTerm::Var { slot, .. } => Some(*slot),
                CompiledTerm::Value(_) => None,
            })
            .collect::<BTreeSet<_>>();
        slots.len() == atom.terms.len()
    })
}

fn reader_for_body_item<'a>(
    full_reader: &'a dyn RelationRead,
    delta: Option<(&'a dyn RelationRead, usize)>,
    body_index: usize,
) -> &'a dyn RelationRead {
    match delta {
        Some((delta_reader, delta_body_index)) if delta_body_index == body_index => delta_reader,
        _ => full_reader,
    }
}

fn select_next_item(
    full_reader: &dyn RelationRead,
    delta: Option<(&dyn RelationRead, usize)>,
    rule: &CompiledRule,
    bindings: &[Binding],
    items: &[(usize, &CompiledBodyItem)],
) -> Result<usize, RuleEvalError> {
    let mut best = None;
    let mut missing_inputs = None;
    for (index, (body_index, item)) in items.iter().enumerate() {
        let reader = reader_for_body_item(full_reader, delta, *body_index);
        let rank = match item {
            CompiledBodyItem::Atom(atom)
                if atom.negated
                    && !bindings
                        .iter()
                        .all(|binding| negated_atom_is_safe(atom, binding)) =>
            {
                continue;
            }
            CompiledBodyItem::Atom(atom) => {
                if let Some(required) = reader.computed_required_bindings(atom.relation)? {
                    let missing = required
                        .iter()
                        .copied()
                        .filter(|position| {
                            bindings.iter().any(|binding| {
                                atom.terms
                                    .get(*position as usize)
                                    .is_none_or(|term| !term_is_bound(term, binding))
                            })
                        })
                        .collect::<Vec<_>>();
                    if !missing.is_empty() {
                        missing_inputs = Some(KernelError::MissingRequiredBindings {
                            relation: atom.relation,
                            positions: missing,
                        });
                        continue;
                    }
                }
                let estimate = atom_estimate(reader, atom, bindings)?;
                let bound_terms = bindings
                    .iter()
                    .map(|binding| bound_term_count(atom, &rule.body_slots[*body_index], binding))
                    .max()
                    .unwrap_or(0);
                (
                    estimate,
                    usize::from(atom.negated),
                    usize::MAX - bound_terms,
                    index,
                )
            }
            CompiledBodyItem::Guard(guard) => {
                if !bindings.iter().all(|binding| guard_is_safe(guard, binding)) {
                    continue;
                }
                (0, 0, 0, index)
            }
        };
        if best.is_none_or(|(_, best_rank)| rank < best_rank) {
            best = Some((index, rank));
        }
    }
    best.map(|(index, _)| index).ok_or_else(|| {
        missing_inputs
            .map(RuleEvalError::from)
            .unwrap_or_else(|| first_unsafe_error(items))
    })
}

fn first_unsafe_error(items: &[(usize, &CompiledBodyItem)]) -> RuleEvalError {
    for (_, item) in items {
        if let CompiledBodyItem::Atom(atom) = item
            && atom.negated
        {
            return RuleError::UnsafeNegation {
                relation: atom.relation,
            }
            .into();
        }
    }
    RuleError::UnsafeGuard.into()
}

fn atom_estimate(
    reader: &dyn RelationRead,
    atom: &CompiledAtom,
    bindings: &[Binding],
) -> Result<usize, RuleEvalError> {
    let mut total = 0usize;
    for binding in bindings {
        let scan_bindings = scan_bindings(atom, binding)?;
        total = total.saturating_add(
            reader
                .estimate_relation_scan(atom.relation, &scan_bindings)?
                .unwrap_or(usize::MAX / 4),
        );
    }
    Ok(total)
}

fn bound_term_count(
    atom: &CompiledAtom,
    referenced_slots: &BTreeSet<usize>,
    binding: &Binding,
) -> usize {
    let constant_terms = atom.terms.len() - referenced_slots.len();
    constant_terms
        + referenced_slots
            .iter()
            .filter(|slot| binding[**slot].is_some())
            .count()
}

fn negated_atom_is_safe(atom: &CompiledAtom, binding: &Binding) -> bool {
    atom.terms.iter().all(|term| match term {
        CompiledTerm::Value(_) => true,
        CompiledTerm::Var { slot, .. } => binding[*slot].is_some(),
    })
}

fn guard_is_safe(guard: &CompiledGuard, binding: &Binding) -> bool {
    term_is_bound(&guard.left, binding) && term_is_bound(&guard.right, binding)
}

fn term_is_bound(term: &CompiledTerm, binding: &Binding) -> bool {
    match term {
        CompiledTerm::Value(_) => true,
        CompiledTerm::Var { slot, .. } => binding[*slot].is_some(),
    }
}

fn apply_positive_atom(
    reader: &dyn RelationRead,
    atom: &CompiledAtom,
    bindings: Vec<Binding>,
) -> Result<Vec<Binding>, RuleEvalError> {
    let mut out = Vec::new();
    if bindings.len() > 1 && reader.computed_required_bindings(atom.relation)?.is_some() {
        let keys = bindings
            .iter()
            .map(|binding| scan_bindings(atom, binding))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(rows) = reader.scan_relation_batch(atom.relation, &keys)? {
            for row in rows {
                if let Some(next) = unify_tuple(atom, &bindings[row.input_row], &row.tuple) {
                    out.push(next);
                }
            }
            return Ok(out);
        }
    }
    for binding in bindings {
        let scan_bindings = scan_bindings(atom, &binding)?;
        reader.visit_relation(atom.relation, &scan_bindings, &mut |tuple| {
            if let Some(next) = unify_tuple(atom, &binding, tuple) {
                out.push(next);
            }
            Ok(ScanControl::Continue)
        })?;
    }
    Ok(out)
}

fn apply_negated_atom(
    reader: &dyn RelationRead,
    atom: &CompiledAtom,
    bindings: Vec<Binding>,
) -> Result<Vec<Binding>, RuleEvalError> {
    let mut out = Vec::new();
    for binding in bindings {
        ensure_negation_safe(atom, &binding)?;
        let scan_bindings = scan_bindings(atom, &binding)?;
        let mut found = false;
        reader.visit_relation(atom.relation, &scan_bindings, &mut |_| {
            found = true;
            Ok(ScanControl::Stop)
        })?;
        if !found {
            out.push(binding);
        }
    }
    Ok(out)
}

fn apply_guard(
    guard: &CompiledGuard,
    bindings: Vec<Binding>,
) -> Result<Vec<Binding>, RuleEvalError> {
    let mut out = Vec::new();
    for binding in bindings {
        if !guard_is_safe(guard, &binding) {
            return Err(RuleError::UnsafeGuard.into());
        }
        let left = guard_value(&guard.left, &binding)?;
        let right = guard_value(&guard.right, &binding)?;
        if compare_values(guard.op, left, right) {
            out.push(binding);
        }
    }
    Ok(out)
}

fn guard_value<'a>(
    term: &'a CompiledTerm,
    binding: &'a Binding,
) -> Result<&'a Value, RuleEvalError> {
    match term {
        CompiledTerm::Value(value) => Ok(value),
        CompiledTerm::Var { symbol, slot } => binding[*slot]
            .as_ref()
            .ok_or(RuleError::UnboundHeadVariable { variable: *symbol }.into()),
    }
}

pub(crate) fn compare_values(op: RuleComparisonOp, left: &Value, right: &Value) -> bool {
    use std::cmp::Ordering;
    match op {
        RuleComparisonOp::Eq => language_cmp::numeric_eq(left, right),
        RuleComparisonOp::Ne => !language_cmp::numeric_eq(left, right),
        RuleComparisonOp::Lt => language_cmp::numeric_cmp(left, right) == Ordering::Less,
        RuleComparisonOp::Le => matches!(
            language_cmp::numeric_cmp(left, right),
            Ordering::Less | Ordering::Equal
        ),
        RuleComparisonOp::Gt => language_cmp::numeric_cmp(left, right) == Ordering::Greater,
        RuleComparisonOp::Ge => matches!(
            language_cmp::numeric_cmp(left, right),
            Ordering::Greater | Ordering::Equal
        ),
    }
}

fn scan_bindings(
    atom: &CompiledAtom,
    binding: &Binding,
) -> Result<Vec<Option<Value>>, RuleEvalError> {
    let mut out = Vec::with_capacity(atom.terms.len());
    for term in &atom.terms {
        out.push(match term {
            CompiledTerm::Value(value) => Some(value.clone()),
            CompiledTerm::Var { slot, .. } => binding[*slot].clone(),
        });
    }
    Ok(out)
}

fn unify_tuple(atom: &CompiledAtom, binding: &Binding, tuple: &Tuple) -> Option<Binding> {
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

fn ensure_negation_safe(atom: &CompiledAtom, binding: &Binding) -> Result<(), RuleEvalError> {
    if negated_atom_is_safe(atom, binding) {
        return Ok(());
    }

    Err(RuleError::UnsafeNegation {
        relation: atom.relation,
    }
    .into())
}

fn instantiate_head(rule: &CompiledRule, binding: &Binding) -> Result<Tuple, RuleEvalError> {
    if !rule.head_slots.iter().all(|slot| binding[*slot].is_some()) {
        for term in &rule.head_terms {
            if let CompiledTerm::Var { symbol, slot } = term
                && binding[*slot].is_none()
            {
                return Err(RuleError::UnboundHeadVariable { variable: *symbol }.into());
            }
        }
    }
    let mut values = Vec::with_capacity(rule.head_terms.len());
    for term in &rule.head_terms {
        values.push(match term {
            CompiledTerm::Value(value) => value.clone(),
            CompiledTerm::Var { symbol, slot } => binding[*slot]
                .clone()
                .ok_or(RuleError::UnboundHeadVariable { variable: *symbol })?,
        });
    }
    Ok(Tuple::new(values))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RelationKernel, RelationMetadata};
    use mica_var::Identity;
    use std::cell::{Cell, RefCell};

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

    fn kernel_with_visibility_relations() -> RelationKernel {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(
                rel(50),
                Symbol::intern("LocatedIn"),
                2,
            ))
            .unwrap();
        kernel
            .create_relation(RelationMetadata::new(
                rel(51),
                Symbol::intern("CanSeeRoom"),
                2,
            ))
            .unwrap();
        kernel
            .create_relation(RelationMetadata::new(
                rel(52),
                Symbol::intern("HiddenFrom"),
                2,
            ))
            .unwrap();
        kernel
    }

    #[test]
    fn nonrecursive_rule_evaluation_supports_safe_negation() {
        let kernel = kernel_with_visibility_relations();
        let mut tx = kernel.begin();
        tx.assert(rel(50), Tuple::from([int(10), int(1)])).unwrap();
        tx.assert(rel(50), Tuple::from([int(11), int(1)])).unwrap();
        tx.assert(rel(51), Tuple::from([int(99), int(1)])).unwrap();
        tx.assert(rel(52), Tuple::from([int(11), int(99)])).unwrap();

        let visible = Rule::new(
            rel(53),
            [var("actor"), var("obj")],
            [
                Atom::positive(rel(50), [var("obj"), var("room")]),
                Atom::positive(rel(51), [var("actor"), var("room")]),
                Atom::negated(rel(52), [var("obj"), var("actor")]),
            ],
        );

        assert_eq!(
            RuleSet::new([visible])
                .evaluate(&tx, &ExecutionContext::serial())
                .unwrap()[&rel(53)],
            vec![Tuple::from([int(99), int(10)])]
        );
    }

    #[test]
    fn rule_evaluation_supports_safe_comparison_guards() {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(
                rel(54),
                Symbol::intern("FileRevision"),
                2,
            ))
            .unwrap();
        kernel
            .create_relation(RelationMetadata::new(
                rel(55),
                Symbol::intern("IndexRevision"),
                2,
            ))
            .unwrap();
        let mut tx = kernel.begin();
        tx.assert(rel(54), Tuple::from([int(1), int(10)])).unwrap();
        tx.assert(rel(54), Tuple::from([int(2), int(20)])).unwrap();
        tx.assert(rel(55), Tuple::from([int(99), int(10)])).unwrap();

        let stale = Rule::new(
            rel(56),
            [var("index"), var("file")],
            vec![
                RuleBodyItem::from(Atom::positive(
                    rel(55),
                    [var("index"), var("index_revision")],
                )),
                RuleBodyItem::from(Atom::positive(rel(54), [var("file"), var("file_revision")])),
                RuleGuard::new(
                    RuleComparisonOp::Ne,
                    var("index_revision"),
                    var("file_revision"),
                )
                .into(),
            ],
        );

        assert_eq!(
            RuleSet::new([stale])
                .evaluate(&tx, &ExecutionContext::serial())
                .unwrap()[&rel(56)],
            vec![Tuple::from([int(99), int(2)])]
        );
    }

    #[test]
    fn rule_guard_uses_mixed_numeric_equality() {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(rel(60), Symbol::intern("Value"), 2))
            .unwrap();
        let mut tx = kernel.begin();
        // Store int 1 and float 1.0 as distinct facts.
        tx.assert(rel(60), Tuple::from([int(1), int(10)])).unwrap();
        tx.assert(rel(60), Tuple::from([Value::float(1.0).unwrap(), int(20)]))
            .unwrap();

        // Rule: find pairs where the first column equals float 1.0.
        // Language numeric equality means int 1 == float 1.0 is true.
        let rule = Rule::new(
            rel(61),
            [var("row")],
            vec![
                RuleBodyItem::from(Atom::positive(rel(60), [var("row"), var("val")])),
                RuleGuard::new(
                    RuleComparisonOp::Eq,
                    var("row"),
                    val(Value::float(1.0).unwrap()),
                )
                .into(),
            ],
        );

        let results = RuleSet::new([rule])
            .evaluate(&tx, &ExecutionContext::serial())
            .unwrap();
        // Both int 1 and float 1.0 should match because language numeric
        // equality considers them equal.
        assert_eq!(
            results[&rel(61)].len(),
            2,
            "expected both int 1 and float 1.0 to match the guard"
        );
    }

    #[test]
    fn rule_evaluation_rejects_unsafe_comparison_guards() {
        let kernel = RelationKernel::new();
        let rules = RuleSet::new([Rule::new(
            rel(57),
            [var("x")],
            [RuleBodyItem::from(RuleGuard::new(
                RuleComparisonOp::Ne,
                var("x"),
                val(int(1)),
            ))],
        )]);

        assert_eq!(
            rules.evaluate(&kernel.begin(), &ExecutionContext::serial()),
            Err(RuleEvalError::Rule(RuleError::UnsafeGuard))
        );
    }

    #[test]
    fn rule_set_rejects_unstratified_negation() {
        let rules = RuleSet::new([Rule::new(
            rel(60),
            [var("x")],
            [Atom::negated(rel(60), [var("x")])],
        )]);

        assert_eq!(
            rules.validate_stratified(),
            Err(RuleError::UnstratifiedNegation)
        );
    }

    #[test]
    fn rule_set_allows_negation_of_lower_derived_strata() {
        let kernel = kernel_with_visibility_relations();
        let mut tx = kernel.begin();
        tx.assert(rel(50), Tuple::from([int(10), int(1)])).unwrap();
        tx.assert(rel(50), Tuple::from([int(11), int(1)])).unwrap();
        tx.assert(rel(51), Tuple::from([int(99), int(1)])).unwrap();
        tx.assert(rel(52), Tuple::from([int(11), int(99)])).unwrap();

        let hidden = Rule::new(
            rel(62),
            [var("actor"), var("obj")],
            [Atom::positive(rel(52), [var("obj"), var("actor")])],
        );
        let visible = Rule::new(
            rel(63),
            [var("actor"), var("obj")],
            [
                Atom::positive(rel(50), [var("obj"), var("room")]),
                Atom::positive(rel(51), [var("actor"), var("room")]),
                Atom::negated(rel(62), [var("actor"), var("obj")]),
            ],
        );

        assert_eq!(
            RuleSet::new([visible, hidden])
                .evaluate(&tx, &ExecutionContext::serial())
                .unwrap()[&rel(63)],
            vec![Tuple::from([int(99), int(10)])]
        );
    }

    #[test]
    fn rule_evaluation_rejects_unsafe_negation() {
        let kernel = kernel_with_visibility_relations();
        let rules = RuleSet::new([Rule::new(
            rel(61),
            [val(int(1))],
            [Atom::negated(rel(52), [var("obj"), val(int(99))])],
        )]);

        assert_eq!(
            rules.evaluate(&*kernel.snapshot(), &ExecutionContext::serial()),
            Err(RuleEvalError::Rule(RuleError::UnsafeNegation {
                relation: rel(52)
            }))
        );
    }

    struct PlanningReader {
        scanned: RefCell<Vec<RelationId>>,
    }

    impl RelationRead for PlanningReader {
        fn scan_relation(
            &self,
            relation: RelationId,
            bindings: &[Option<Value>],
        ) -> Result<Vec<Tuple>, KernelError> {
            self.scanned.borrow_mut().push(relation);
            match relation {
                relation if relation == rel(80) => {
                    assert_eq!(bindings, &[None]);
                    Ok(vec![Tuple::from([int(1)])])
                }
                relation if relation == rel(81) => {
                    assert_eq!(bindings, &[Some(int(1)), None]);
                    Ok(vec![Tuple::from([int(1), int(2)])])
                }
                _ => panic!("unexpected relation scan: {relation:?}"),
            }
        }

        fn estimate_relation_scan(
            &self,
            relation: RelationId,
            bindings: &[Option<Value>],
        ) -> Result<Option<usize>, KernelError> {
            let estimate = match relation {
                relation if relation == rel(80) => 1,
                relation if relation == rel(81) && bindings == [None, None] => 1_000,
                relation if relation == rel(81) => 1,
                _ => 1_000,
            };
            Ok(Some(estimate))
        }
    }

    #[test]
    fn rule_body_planner_starts_with_selective_atom_not_source_order() {
        let reader = PlanningReader {
            scanned: RefCell::new(Vec::new()),
        };
        let rule = compile_rule(&Rule::new(
            rel(82),
            [var("y")],
            [
                Atom::positive(rel(81), [var("x"), var("y")]),
                Atom::positive(rel(80), [var("x")]),
            ],
        ));
        let bindings = evaluate_body(&reader, &rule, &ExecutionContext::serial()).unwrap();

        assert_eq!(reader.scanned.borrow().as_slice(), &[rel(80), rel(81)]);
        assert_eq!(
            instantiate_head(&rule, &bindings[0]).unwrap(),
            Tuple::from([int(2)])
        );
    }

    #[test]
    fn rule_evaluation_enforces_repeated_variables_in_one_atom() {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(rel(90), Symbol::intern("Pair"), 2))
            .unwrap();
        let mut tx = kernel.begin();
        tx.assert(rel(90), Tuple::from([int(1), int(1)])).unwrap();
        tx.assert(rel(90), Tuple::from([int(1), int(2)])).unwrap();

        let same = Rule::new(
            rel(91),
            [var("x")],
            [Atom::positive(rel(90), [var("x"), var("x")])],
        );

        assert_eq!(
            RuleSet::new([same])
                .evaluate(&tx, &ExecutionContext::serial())
                .unwrap()[&rel(91)],
            vec![Tuple::from([int(1)])]
        );
    }

    #[test]
    fn rule_evaluation_rejects_unbound_head_variable() {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(rel(92), Symbol::intern("Source"), 1))
            .unwrap();
        let mut tx = kernel.begin();
        tx.assert(rel(92), Tuple::from([int(1)])).unwrap();

        let invalid = Rule::new(
            rel(93),
            [var("missing")],
            [Atom::positive(rel(92), [var("present")])],
        );

        assert_eq!(
            RuleSet::new([invalid]).evaluate(&tx, &ExecutionContext::serial()),
            Err(RuleEvalError::Rule(RuleError::UnboundHeadVariable {
                variable: Symbol::intern("missing")
            }))
        );
    }

    struct VisitOnlyReader;

    impl RelationRead for VisitOnlyReader {
        fn scan_relation(
            &self,
            _relation: RelationId,
            _bindings: &[Option<Value>],
        ) -> Result<Vec<Tuple>, KernelError> {
            panic!("rule atom application should use visit_relation")
        }

        fn visit_relation(
            &self,
            relation: RelationId,
            bindings: &[Option<Value>],
            visitor: &mut dyn FnMut(&Tuple) -> Result<ScanControl, KernelError>,
        ) -> Result<(), KernelError> {
            assert_eq!(relation, rel(94));
            assert_eq!(bindings, &[None]);
            visitor(&Tuple::from([int(1)]))?;
            Ok(())
        }
    }

    #[test]
    fn rule_atom_application_streams_relation_visits() {
        let rule = Rule::new(rel(95), [var("x")], [Atom::positive(rel(94), [var("x")])]);

        assert_eq!(
            RuleSet::new([rule])
                .evaluate(&VisitOnlyReader, &ExecutionContext::serial())
                .unwrap()[&rel(95)],
            vec![Tuple::from([int(1)])]
        );
    }

    #[test]
    fn compiled_stratum_groups_mutual_recursion_and_orders_dependencies_first() {
        let rules = RuleSet::new([
            Rule::new(rel(101), [var("x")], [Atom::positive(rel(100), [var("x")])]),
            Rule::new(rel(102), [var("x")], [Atom::positive(rel(101), [var("x")])]),
            Rule::new(rel(103), [var("x")], [Atom::positive(rel(104), [var("x")])]),
            Rule::new(rel(104), [var("x")], [Atom::positive(rel(103), [var("x")])]),
        ]);

        let program = rules.compile().unwrap();
        let components = &program.strata[0].components;
        let dependency = components
            .iter()
            .position(|component| component.target_relations == BTreeSet::from([rel(101)]))
            .unwrap();
        let dependent = components
            .iter()
            .position(|component| component.target_relations == BTreeSet::from([rel(102)]))
            .unwrap();

        assert!(dependency < dependent);
        assert!(components.iter().any(|component| {
            component.target_relations == BTreeSet::from([rel(103), rel(104)])
        }));
    }

    #[test]
    fn compiled_component_classifies_seeds_and_one_variant_per_recursive_atom() {
        let rules = RuleSet::new([
            Rule::new(rel(111), [var("x")], [Atom::positive(rel(110), [var("x")])]),
            Rule::new(rel(111), [var("x")], [Atom::positive(rel(111), [var("x")])]),
            Rule::new(
                rel(111),
                [var("x")],
                [
                    Atom::positive(rel(111), [var("x")]),
                    Atom::positive(rel(111), [var("x")]),
                ],
            ),
        ]);

        let program = rules.compile().unwrap();
        let component = &program.strata[0].components[0];

        assert_eq!(component.target_relations, BTreeSet::from([rel(111)]));
        assert_eq!(component.rule_indices, vec![0, 1, 2]);
        assert_eq!(component.seed_rule_indices, vec![0]);
        assert_eq!(
            component.recursive_variants,
            vec![
                CompiledRuleVariant {
                    rule_index: 1,
                    delta_body_index: 0,
                },
                CompiledRuleVariant {
                    rule_index: 2,
                    delta_body_index: 0,
                },
                CompiledRuleVariant {
                    rule_index: 2,
                    delta_body_index: 1,
                },
            ]
        );
    }

    #[test]
    fn rule_set_reuses_its_compiled_program() {
        let rules = RuleSet::new([Rule::new(
            rel(119),
            [var("x")],
            [Atom::positive(rel(118), [var("x")])],
        )]);

        let first = rules.compile().unwrap();
        let second = rules.compile().unwrap();

        assert!(std::ptr::eq(first, second));
    }

    #[test]
    fn two_atom_rule_uses_the_shared_compiled_physical_plan() {
        let rules = RuleSet::new([Rule::new(
            rel(203),
            [var("from"), var("to")],
            [
                Atom::positive(rel(200), [var("from"), var("mid")]),
                Atom::positive(rel(201), [var("mid"), var("to")]),
            ],
        )]);
        let compiled = rules.compile().unwrap();
        let rule = &compiled.strata[0].rules[0];

        assert_eq!(
            rule.batch_plan,
            Some(PhysicalQueryPlan::JoinEq {
                left: Box::new(PhysicalQueryPlan::Scan {
                    relation: rel(200),
                    bindings: vec![None, None],
                }),
                right: Box::new(PhysicalQueryPlan::Scan {
                    relation: rel(201),
                    bindings: vec![None, None],
                }),
                left_positions: vec![1],
                right_positions: vec![0],
            })
        );
    }

    struct BatchExportReader {
        snapshot: Arc<crate::Snapshot>,
        exports: Cell<usize>,
    }

    impl RelationRead for BatchExportReader {
        fn scan_relation(
            &self,
            relation: RelationId,
            bindings: &[Option<Value>],
        ) -> Result<Vec<Tuple>, KernelError> {
            self.snapshot.scan_relation(relation, bindings)
        }

        fn estimate_relation_scan(
            &self,
            relation: RelationId,
            bindings: &[Option<Value>],
        ) -> Result<Option<usize>, KernelError> {
            self.snapshot.estimate_relation_scan(relation, bindings)
        }

        fn relation_capabilities(
            &self,
            relation: RelationId,
        ) -> Result<RelationCapabilities, KernelError> {
            self.snapshot.relation_capabilities(relation)
        }

        fn export_relation_batch(
            &self,
            relation: RelationId,
            bindings: &[Option<Value>],
        ) -> Result<Option<Arc<PackedRelation>>, KernelError> {
            self.exports.set(self.exports.get() + 1);
            self.snapshot.export_relation_batch(relation, bindings)
        }
    }

    #[test]
    fn large_two_atom_rule_uses_packed_join_and_matches_expected_output() {
        let kernel = RelationKernel::new();
        for (relation, name) in [(rel(200), "Left"), (rel(201), "Right")] {
            kernel
                .create_relation(RelationMetadata::new(relation, Symbol::intern(name), 2))
                .unwrap();
        }
        let mut tx = kernel.begin();
        for row in 0..300 {
            tx.assert(rel(200), Tuple::from([int(row), int(row + 1)]))
                .unwrap();
            tx.assert(rel(201), Tuple::from([int(row + 1), int(row + 2)]))
                .unwrap();
        }
        tx.commit().unwrap();
        let reader = BatchExportReader {
            snapshot: kernel.snapshot(),
            exports: Cell::new(0),
        };
        let rules = RuleSet::new([Rule::new(
            rel(202),
            [var("from"), var("to")],
            [
                Atom::positive(rel(200), [var("from"), var("mid")]),
                Atom::positive(rel(201), [var("mid"), var("to")]),
            ],
        )]);

        let packed = rules
            .evaluate_fixpoint(&reader, &ExecutionContext::serial())
            .unwrap();
        assert_eq!(reader.exports.get(), 2);
        assert_eq!(
            packed[&rel(202)],
            (0..300)
                .map(|row| Tuple::from([int(row), int(row + 2)]))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn semi_naive_evaluation_combines_overlapping_clauses_independently_of_order() {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(rel(120), Symbol::intern("Edge"), 2))
            .unwrap();
        kernel
            .create_relation(RelationMetadata::new(
                rel(121),
                Symbol::intern("Shortcut"),
                2,
            ))
            .unwrap();
        let mut tx = kernel.begin();
        tx.assert(rel(120), Tuple::from([int(1), int(2)])).unwrap();
        tx.assert(rel(120), Tuple::from([int(2), int(3)])).unwrap();
        tx.assert(rel(121), Tuple::from([int(1), int(2)])).unwrap();

        let edge = Rule::new(
            rel(122),
            [var("from"), var("to")],
            [Atom::positive(rel(120), [var("from"), var("to")])],
        );
        let shortcut = Rule::new(
            rel(122),
            [var("from"), var("to")],
            [Atom::positive(rel(121), [var("from"), var("to")])],
        );
        let step = Rule::new(
            rel(122),
            [var("from"), var("to")],
            [
                Atom::positive(rel(122), [var("from"), var("middle")]),
                Atom::positive(rel(120), [var("middle"), var("to")]),
            ],
        );
        let forward = RuleSet::new([edge.clone(), shortcut.clone(), step.clone()]);
        let reversed = RuleSet::new([step, shortcut, edge]);

        assert_eq!(
            forward
                .evaluate_fixpoint(&tx, &ExecutionContext::serial())
                .unwrap()[&rel(122)],
            vec![
                Tuple::from([int(1), int(2)]),
                Tuple::from([int(1), int(3)]),
                Tuple::from([int(2), int(3)]),
            ]
        );
        assert_eq!(
            forward
                .evaluate_fixpoint(&tx, &ExecutionContext::serial())
                .unwrap(),
            reversed
                .evaluate_fixpoint(&tx, &ExecutionContext::serial())
                .unwrap()
        );
    }

    #[test]
    fn semi_naive_evaluation_supports_multiple_recursive_atoms() {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(rel(130), Symbol::intern("Edge"), 2))
            .unwrap();
        let mut tx = kernel.begin();
        for (from, to) in [(1, 2), (2, 3), (3, 4)] {
            tx.assert(rel(130), Tuple::from([int(from), int(to)]))
                .unwrap();
        }
        let rules = RuleSet::new([
            Rule::new(
                rel(131),
                [var("from"), var("to")],
                [Atom::positive(rel(130), [var("from"), var("to")])],
            ),
            Rule::new(
                rel(131),
                [var("from"), var("to")],
                [
                    Atom::positive(rel(131), [var("from"), var("middle")]),
                    Atom::positive(rel(131), [var("middle"), var("to")]),
                ],
            ),
        ]);

        assert_eq!(
            rules
                .evaluate_fixpoint(&tx, &ExecutionContext::serial())
                .unwrap()[&rel(131)],
            vec![
                Tuple::from([int(1), int(2)]),
                Tuple::from([int(1), int(3)]),
                Tuple::from([int(1), int(4)]),
                Tuple::from([int(2), int(3)]),
                Tuple::from([int(2), int(4)]),
                Tuple::from([int(3), int(4)]),
            ]
        );
    }

    #[test]
    fn semi_naive_evaluation_supports_same_round_mutual_recursion() {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(rel(140), Symbol::intern("Seed"), 1))
            .unwrap();
        kernel
            .create_relation(RelationMetadata::new(rel(141), Symbol::intern("Next"), 2))
            .unwrap();
        let mut tx = kernel.begin();
        tx.assert(rel(140), Tuple::from([int(1)])).unwrap();
        tx.assert(rel(141), Tuple::from([int(1), int(2)])).unwrap();
        tx.assert(rel(141), Tuple::from([int(2), int(3)])).unwrap();
        let rules = RuleSet::new([
            Rule::new(rel(142), [var("x")], [Atom::positive(rel(140), [var("x")])]),
            Rule::new(rel(143), [var("x")], [Atom::positive(rel(142), [var("x")])]),
            Rule::new(
                rel(142),
                [var("next")],
                [
                    Atom::positive(rel(143), [var("x")]),
                    Atom::positive(rel(141), [var("x"), var("next")]),
                ],
            ),
        ]);

        let derived = rules
            .evaluate_fixpoint(&tx, &ExecutionContext::serial())
            .unwrap();
        let expected = vec![
            Tuple::from([int(1)]),
            Tuple::from([int(2)]),
            Tuple::from([int(3)]),
        ];
        assert_eq!(derived[&rel(142)], expected);
        assert_eq!(derived[&rel(143)], expected);
    }

    #[test]
    fn semi_naive_evaluation_uses_extensional_recursive_targets_as_frontiers() {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(rel(150), Symbol::intern("Edge"), 2))
            .unwrap();
        kernel
            .create_relation(RelationMetadata::new(
                rel(151),
                Symbol::intern("Reachable"),
                2,
            ))
            .unwrap();
        let mut tx = kernel.begin();
        tx.assert(rel(151), Tuple::from([int(1), int(2)])).unwrap();
        tx.assert(rel(150), Tuple::from([int(2), int(3)])).unwrap();
        tx.assert(rel(150), Tuple::from([int(3), int(4)])).unwrap();
        let rules = RuleSet::new([Rule::new(
            rel(151),
            [var("from"), var("to")],
            [
                Atom::positive(rel(151), [var("from"), var("middle")]),
                Atom::positive(rel(150), [var("middle"), var("to")]),
            ],
        )]);

        assert_eq!(
            rules
                .evaluate_fixpoint(&tx, &ExecutionContext::serial())
                .unwrap()[&rel(151)],
            vec![Tuple::from([int(1), int(3)]), Tuple::from([int(1), int(4)]),]
        );
    }

    #[test]
    fn semi_naive_evaluation_reads_negation_from_completed_lower_strata() {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(rel(160), Symbol::intern("Node"), 1))
            .unwrap();
        kernel
            .create_relation(RelationMetadata::new(
                rel(161),
                Symbol::intern("Blocked"),
                1,
            ))
            .unwrap();
        kernel
            .create_relation(RelationMetadata::new(rel(162), Symbol::intern("Next"), 2))
            .unwrap();
        let mut tx = kernel.begin();
        for node in 1..=3 {
            tx.assert(rel(160), Tuple::from([int(node)])).unwrap();
        }
        tx.assert(rel(161), Tuple::from([int(2)])).unwrap();
        tx.assert(rel(162), Tuple::from([int(1), int(2)])).unwrap();
        tx.assert(rel(162), Tuple::from([int(2), int(3)])).unwrap();
        let rules = RuleSet::new([
            Rule::new(rel(163), [var("x")], [Atom::positive(rel(161), [var("x")])]),
            Rule::new(
                rel(164),
                [var("x")],
                [
                    Atom::positive(rel(160), [var("x")]),
                    Atom::negated(rel(163), [var("x")]),
                ],
            ),
            Rule::new(rel(165), [var("x")], [Atom::positive(rel(164), [var("x")])]),
            Rule::new(
                rel(165),
                [var("next")],
                [
                    Atom::positive(rel(165), [var("x")]),
                    Atom::positive(rel(162), [var("x"), var("next")]),
                ],
            ),
        ]);

        assert_eq!(
            rules
                .evaluate_fixpoint(&tx, &ExecutionContext::serial())
                .unwrap()[&rel(165)],
            vec![
                Tuple::from([int(1)]),
                Tuple::from([int(2)]),
                Tuple::from([int(3)])
            ]
        );
    }

    #[test]
    fn semi_naive_evaluation_matches_graph_closure_for_generated_small_graphs() {
        let possible_edges = [(1, 1), (1, 2), (1, 3), (2, 2), (2, 3), (3, 1), (3, 3)];
        for mask in 0u16..(1 << possible_edges.len()) {
            let kernel = RelationKernel::new();
            kernel
                .create_relation(RelationMetadata::new(rel(170), Symbol::intern("Edge"), 2))
                .unwrap();
            let mut tx = kernel.begin();
            for (index, (from, to)) in possible_edges.iter().enumerate() {
                if mask & (1 << index) != 0 {
                    tx.assert(rel(170), Tuple::from([int(*from), int(*to)]))
                        .unwrap();
                }
            }
            let rules = RuleSet::new([
                Rule::new(
                    rel(171),
                    [var("from"), var("to")],
                    [Atom::positive(rel(170), [var("from"), var("to")])],
                ),
                Rule::new(
                    rel(171),
                    [var("from"), var("to")],
                    [
                        Atom::positive(rel(171), [var("from"), var("middle")]),
                        Atom::positive(rel(170), [var("middle"), var("to")]),
                    ],
                ),
            ]);
            let mut expected = possible_edges
                .iter()
                .enumerate()
                .filter(|(index, _)| mask & (1 << index) != 0)
                .map(|(_, edge)| *edge)
                .collect::<BTreeSet<_>>();
            loop {
                let before = expected.len();
                let paths = expected.iter().copied().collect::<Vec<_>>();
                for (from, middle) in &paths {
                    for (next_from, to) in &paths {
                        if middle == next_from {
                            expected.insert((*from, *to));
                        }
                    }
                }
                if expected.len() == before {
                    break;
                }
            }
            let expected = expected
                .into_iter()
                .map(|(from, to)| Tuple::from([int(from), int(to)]))
                .collect::<Vec<_>>();
            assert_eq!(
                rules
                    .evaluate_fixpoint(&tx, &ExecutionContext::serial())
                    .unwrap()
                    .get(&rel(171))
                    .cloned()
                    .unwrap_or_default(),
                expected
            );
        }
    }

    #[test]
    fn semi_naive_evaluation_handles_empty_frontiers() {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(rel(180), Symbol::intern("Edge"), 2))
            .unwrap();
        let rules = RuleSet::new([
            Rule::new(
                rel(181),
                [var("from"), var("to")],
                [Atom::positive(rel(180), [var("from"), var("to")])],
            ),
            Rule::new(
                rel(181),
                [var("from"), var("to")],
                [
                    Atom::positive(rel(181), [var("from"), var("middle")]),
                    Atom::positive(rel(180), [var("middle"), var("to")]),
                ],
            ),
        ]);
        let tx = kernel.begin();

        assert!(
            !rules
                .evaluate_fixpoint(&tx, &ExecutionContext::serial())
                .unwrap()
                .contains_key(&rel(181))
        );
    }

    #[test]
    fn semi_naive_evaluation_reads_transaction_local_retractions() {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(rel(190), Symbol::intern("Edge"), 2))
            .unwrap();
        let mut seed = kernel.begin();
        seed.assert(rel(190), Tuple::from([int(1), int(2)]))
            .unwrap();
        seed.assert(rel(190), Tuple::from([int(2), int(3)]))
            .unwrap();
        seed.commit().unwrap();

        let mut tx = kernel.begin();
        tx.retract(rel(190), Tuple::from([int(2), int(3)])).unwrap();
        tx.assert(rel(190), Tuple::from([int(2), int(4)])).unwrap();
        let rules = RuleSet::new([
            Rule::new(
                rel(191),
                [var("from"), var("to")],
                [Atom::positive(rel(190), [var("from"), var("to")])],
            ),
            Rule::new(
                rel(191),
                [var("from"), var("to")],
                [
                    Atom::positive(rel(191), [var("from"), var("middle")]),
                    Atom::positive(rel(190), [var("middle"), var("to")]),
                ],
            ),
        ]);

        assert_eq!(
            rules
                .evaluate_fixpoint(&tx, &ExecutionContext::serial())
                .unwrap()[&rel(191)],
            vec![
                Tuple::from([int(1), int(2)]),
                Tuple::from([int(1), int(4)]),
                Tuple::from([int(2), int(4)]),
            ]
        );
    }

    #[test]
    fn semi_naive_evaluation_preserves_constants_repeated_variables_and_guards() {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(rel(200), Symbol::intern("Edge"), 2))
            .unwrap();
        let mut tx = kernel.begin();
        for (from, to) in [(1, 1), (1, 2), (2, 3), (3, 4)] {
            tx.assert(rel(200), Tuple::from([int(from), int(to)]))
                .unwrap();
        }
        let rules = RuleSet::new([
            Rule::new(
                rel(201),
                [var("node")],
                [Atom::positive(rel(200), [val(int(1)), var("node")])],
            ),
            Rule::new(
                rel(201),
                [var("next")],
                vec![
                    RuleBodyItem::from(Atom::positive(rel(201), [var("node")])),
                    RuleBodyItem::from(Atom::positive(rel(200), [var("node"), var("next")])),
                    RuleGuard::new(RuleComparisonOp::Ne, var("next"), val(int(4))).into(),
                ],
            ),
            Rule::new(
                rel(202),
                [var("node")],
                [Atom::positive(rel(200), [var("node"), var("node")])],
            ),
        ]);

        assert_eq!(
            rules
                .evaluate_fixpoint(&tx, &ExecutionContext::serial())
                .unwrap()[&rel(201)],
            vec![
                Tuple::from([int(1)]),
                Tuple::from([int(2)]),
                Tuple::from([int(3)])
            ]
        );
        assert_eq!(
            rules
                .evaluate_fixpoint(&tx, &ExecutionContext::serial())
                .unwrap()[&rel(202)],
            vec![Tuple::from([int(1)])]
        );
    }

    #[test]
    fn fixpoint_evaluation_supports_positive_recursion() {
        let kernel = RelationKernel::new();
        kernel
            .create_relation(RelationMetadata::new(rel(70), Symbol::intern("Edge"), 2))
            .unwrap();
        let mut tx = kernel.begin();
        tx.assert(rel(70), Tuple::from([int(1), int(2)])).unwrap();
        tx.assert(rel(70), Tuple::from([int(2), int(3)])).unwrap();
        tx.assert(rel(70), Tuple::from([int(3), int(4)])).unwrap();

        let reachable_base = Rule::new(
            rel(71),
            [var("a"), var("b")],
            [Atom::positive(rel(70), [var("a"), var("b")])],
        );
        let reachable_step = Rule::new(
            rel(71),
            [var("a"), var("c")],
            [
                Atom::positive(rel(70), [var("a"), var("b")]),
                Atom::positive(rel(71), [var("b"), var("c")]),
            ],
        );

        assert_eq!(
            RuleSet::new([reachable_base, reachable_step])
                .evaluate_fixpoint(&tx, &ExecutionContext::serial())
                .unwrap()[&rel(71)],
            vec![
                Tuple::from([int(1), int(2)]),
                Tuple::from([int(1), int(3)]),
                Tuple::from([int(1), int(4)]),
                Tuple::from([int(2), int(3)]),
                Tuple::from([int(2), int(4)]),
                Tuple::from([int(3), int(4)]),
            ]
        );
    }
}
