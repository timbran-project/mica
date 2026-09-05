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

use crate::computed::{ComputedRelation, ComputedRelationRead};
use crate::relation_algebra::finish_tuple_rows;
use crate::{ConflictPolicy, KernelError, RelationMetadata, Snapshot, Tuple};
use mica_var::{Identity, Symbol, Value};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum CatalogPredicate {
    Relation,
    RelationName,
    Arity,
    RelationDurability,
    Rule,
    RuleHead,
    RuleSource,
    ActiveRule,
    ArgumentName,
    ConflictPolicy,
    FunctionalKey,
    Index,
    IndexPosition,
    IndexStorageKind,
}

impl CatalogPredicate {
    pub fn symbol(self) -> Symbol {
        match self {
            Self::Relation => Symbol::intern("Relation"),
            Self::RelationName => Symbol::intern("RelationName"),
            Self::Arity => Symbol::intern("Arity"),
            Self::RelationDurability => Symbol::intern("RelationDurability"),
            Self::Rule => Symbol::intern("Rule"),
            Self::RuleHead => Symbol::intern("RuleHead"),
            Self::RuleSource => Symbol::intern("RuleSource"),
            Self::ActiveRule => Symbol::intern("ActiveRule"),
            Self::ArgumentName => Symbol::intern("ArgumentName"),
            Self::ConflictPolicy => Symbol::intern("ConflictPolicy"),
            Self::FunctionalKey => Symbol::intern("FunctionalKey"),
            Self::Index => Symbol::intern("Index"),
            Self::IndexPosition => Symbol::intern("IndexPosition"),
            Self::IndexStorageKind => Symbol::intern("IndexStorageKind"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogFact {
    pub predicate: CatalogPredicate,
    pub tuple: Tuple,
}

impl Snapshot {
    pub fn catalog_facts(&self) -> Vec<CatalogFact> {
        catalog_facts(self)
    }
}

pub(crate) fn catalog_facts(reader: &dyn ComputedRelationRead) -> Vec<CatalogFact> {
    let mut facts = Vec::new();
    for metadata in reader.relation_metadata_vec() {
        let relation_id = metadata.id();
        facts.push(catalog_fact(
            CatalogPredicate::Relation,
            [Value::identity(relation_id)],
        ));
        facts.push(catalog_fact(
            CatalogPredicate::RelationName,
            [Value::identity(relation_id), Value::symbol(metadata.name())],
        ));
        facts.push(catalog_fact(
            CatalogPredicate::Arity,
            [
                Value::identity(relation_id),
                Value::int(metadata.arity() as i64).unwrap(),
            ],
        ));
        facts.push(catalog_fact(
            CatalogPredicate::RelationDurability,
            [
                Value::identity(relation_id),
                Value::symbol(metadata.durability().symbol()),
            ],
        ));
        for position in 0..metadata.arity() {
            if let Some(name) = metadata.argument_name(position) {
                facts.push(catalog_fact(
                    CatalogPredicate::ArgumentName,
                    [
                        Value::identity(relation_id),
                        Value::int(position as i64).unwrap(),
                        Value::symbol(name),
                    ],
                ));
            }
        }

        push_conflict_policy_facts(&mut facts, relation_id, metadata.conflict_policy());
        for (ordinal, index) in metadata.indexes().iter().enumerate() {
            let index_value = Value::identity(index_identity(relation_id, ordinal as u16));
            facts.push(catalog_fact(
                CatalogPredicate::Index,
                [Value::identity(relation_id), index_value.clone()],
            ));
            if let Some(kind) = reader.index_storage_kind(relation_id, ordinal) {
                facts.push(catalog_fact(
                    CatalogPredicate::IndexStorageKind,
                    [index_value.clone(), Value::symbol(kind)],
                ));
            }
            for (slot, position) in index.positions().iter().enumerate() {
                facts.push(catalog_fact(
                    CatalogPredicate::IndexPosition,
                    [
                        index_value.clone(),
                        Value::int(slot as i64).unwrap(),
                        Value::int(*position as i64).unwrap(),
                    ],
                ));
            }
        }
    }
    for rule in reader.rules_vec() {
        let rule_id = Value::identity(rule.id());
        facts.push(catalog_fact(CatalogPredicate::Rule, [rule_id.clone()]));
        facts.push(catalog_fact(
            CatalogPredicate::RuleHead,
            [
                rule_id.clone(),
                Value::identity(rule.rule().head_relation()),
            ],
        ));
        facts.push(catalog_fact(
            CatalogPredicate::RuleSource,
            [rule_id.clone(), Value::string(rule.source())],
        ));
        facts.push(catalog_fact(
            CatalogPredicate::ActiveRule,
            [rule_id, Value::bool(rule.active())],
        ));
    }
    facts
}

pub(crate) fn is_system_relation(metadata: &RelationMetadata) -> bool {
    system_catalog_predicate(metadata).is_some()
        || matches!(
            metadata.name().name(),
            Some("SubjectFact" | "MentionedFact" | "ExtensionalMentionedFact")
        )
}

pub fn system_computed_relations() -> Vec<Arc<dyn ComputedRelation>> {
    vec![Arc::new(SystemComputedRelation)]
}

pub fn system_row_source_relation(metadata: &RelationMetadata, tuple: &Tuple) -> Option<Identity> {
    match metadata.name().name()? {
        "SubjectFact" | "MentionedFact" | "ExtensionalMentionedFact" => {
            tuple.values().get(1)?.as_identity()
        }
        _ => None,
    }
}

pub(crate) fn system_relation_rows(
    reader: &dyn ComputedRelationRead,
    metadata: &RelationMetadata,
    bindings: &[Option<Value>],
) -> Option<Result<Vec<Tuple>, KernelError>> {
    if bindings.len() != metadata.arity() as usize {
        return Some(Err(KernelError::ArityMismatch {
            relation: metadata.id(),
            expected: metadata.arity(),
            actual: bindings.len(),
        }));
    }

    if let Some(predicate) = system_catalog_predicate(metadata) {
        let rows = catalog_facts(reader)
            .into_iter()
            .filter(|fact| fact.predicate == predicate)
            .map(|fact| fact.tuple)
            .filter(|tuple| tuple.matches_bindings(bindings))
            .collect::<Vec<_>>();
        return Some(Ok(finish_tuple_rows(rows)));
    }

    match metadata.name().name() {
        Some("SubjectFact") if metadata.arity() == 3 => {
            Some(system_subject_facts(reader, bindings))
        }
        Some("MentionedFact") if metadata.arity() == 4 => {
            Some(system_mentioned_facts(reader, bindings))
        }
        Some("ExtensionalMentionedFact") if metadata.arity() == 4 => {
            Some(system_extensional_mentioned_facts(reader, bindings))
        }
        _ => None,
    }
}

struct SystemComputedRelation;

impl ComputedRelation for SystemComputedRelation {
    fn name(&self) -> &'static str {
        "system-reflection"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        is_system_relation(metadata)
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        system_relation_rows(reader, metadata, bindings).unwrap_or_else(|| {
            Err(KernelError::InvalidComputedRelation {
                relation: metadata.id(),
                message: "system reflection relation did not produce rows".to_owned(),
            })
        })
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

fn system_catalog_predicate(metadata: &RelationMetadata) -> Option<CatalogPredicate> {
    let expected = match metadata.name().name()? {
        "Relation" => (CatalogPredicate::Relation, 1),
        "RelationName" => (CatalogPredicate::RelationName, 2),
        "Arity" => (CatalogPredicate::Arity, 2),
        "RelationDurability" => (CatalogPredicate::RelationDurability, 2),
        "Rule" => (CatalogPredicate::Rule, 1),
        "RuleHead" => (CatalogPredicate::RuleHead, 2),
        "RuleSource" => (CatalogPredicate::RuleSource, 2),
        "ActiveRule" => (CatalogPredicate::ActiveRule, 2),
        "ArgumentName" => (CatalogPredicate::ArgumentName, 3),
        "ConflictPolicy" => (CatalogPredicate::ConflictPolicy, 2),
        "FunctionalKey" => (CatalogPredicate::FunctionalKey, 3),
        "Index" => (CatalogPredicate::Index, 2),
        "IndexPosition" => (CatalogPredicate::IndexPosition, 3),
        "IndexStorageKind" => (CatalogPredicate::IndexStorageKind, 2),
        _ => return None,
    };
    if metadata.arity() == expected.1 {
        Some(expected.0)
    } else {
        None
    }
}

fn system_subject_facts(
    reader: &dyn ComputedRelationRead,
    bindings: &[Option<Value>],
) -> Result<Vec<Tuple>, KernelError> {
    let mut rows = Vec::new();
    if let Some(subject) = &bindings[0] {
        rows.extend(subject_fact_rows(reader, subject)?);
    } else {
        for (relation, tuple) in reader.extensional_facts()? {
            if let Some(subject) = tuple.values().first() {
                rows.push(subject_fact_tuple(subject.clone(), relation, tuple));
            }
        }
    }
    Ok(finish_tuple_rows(
        rows.into_iter()
            .filter(|tuple| tuple.matches_bindings(bindings))
            .collect(),
    ))
}

fn system_mentioned_facts(
    reader: &dyn ComputedRelationRead,
    bindings: &[Option<Value>],
) -> Result<Vec<Tuple>, KernelError> {
    let mut rows = Vec::new();
    if let Some(value) = &bindings[0] {
        rows.extend(mentioned_fact_rows(reader, value)?);
    } else {
        for (relation, tuple) in reader.extensional_facts()? {
            for (position, value) in tuple.values().iter().enumerate() {
                rows.push(mentioned_fact_tuple(
                    value.clone(),
                    relation,
                    position as u16,
                    tuple.clone(),
                ));
            }
        }
    }
    Ok(finish_tuple_rows(
        rows.into_iter()
            .filter(|tuple| tuple.matches_bindings(bindings))
            .collect(),
    ))
}

fn subject_fact_rows(
    reader: &dyn ComputedRelationRead,
    subject: &Value,
) -> Result<Vec<Tuple>, KernelError> {
    let mut rows = Vec::new();
    for (relation, tuple) in reader.extensional_facts()? {
        if let Some(tuple_subject) = tuple.values().first()
            && tuple_subject == subject
        {
            rows.push(subject_fact_tuple(subject.clone(), relation, tuple));
        }
    }
    Ok(rows)
}

fn mentioned_fact_rows(
    reader: &dyn ComputedRelationRead,
    value: &Value,
) -> Result<Vec<Tuple>, KernelError> {
    extensional_mentioned_fact_rows(reader, value)
}

fn system_extensional_mentioned_facts(
    reader: &dyn ComputedRelationRead,
    bindings: &[Option<Value>],
) -> Result<Vec<Tuple>, KernelError> {
    let mut rows = Vec::new();
    if let Some(value) = &bindings[0] {
        rows.extend(extensional_mentioned_fact_rows(reader, value)?);
    } else {
        for (relation, tuple) in reader.extensional_facts()? {
            for (position, value) in tuple.values().iter().enumerate() {
                rows.push(mentioned_fact_tuple(
                    value.clone(),
                    relation,
                    position as u16,
                    tuple.clone(),
                ));
            }
        }
    }
    Ok(finish_tuple_rows(
        rows.into_iter()
            .filter(|tuple| tuple.matches_bindings(bindings))
            .collect(),
    ))
}

fn extensional_mentioned_fact_rows(
    reader: &dyn ComputedRelationRead,
    value: &Value,
) -> Result<Vec<Tuple>, KernelError> {
    let mut rows = Vec::new();
    for (relation, tuple) in reader.extensional_facts()? {
        for (position, tuple_value) in tuple.values().iter().enumerate() {
            if tuple_value == value {
                rows.push(mentioned_fact_tuple(
                    value.clone(),
                    relation,
                    position as u16,
                    tuple.clone(),
                ));
            }
        }
    }
    Ok(rows)
}

fn subject_fact_tuple(subject: Value, relation: Identity, tuple: Tuple) -> Tuple {
    Tuple::from([
        subject,
        Value::identity(relation),
        Value::list(tuple.values().iter().cloned()),
    ])
}

fn mentioned_fact_tuple(value: Value, relation: Identity, position: u16, tuple: Tuple) -> Tuple {
    Tuple::from([
        value,
        Value::identity(relation),
        Value::int(position as i64).unwrap(),
        Value::list(tuple.values().iter().cloned()),
    ])
}

fn push_conflict_policy_facts(
    facts: &mut Vec<CatalogFact>,
    relation_id: Identity,
    conflict_policy: &ConflictPolicy,
) {
    let relation = Value::identity(relation_id);
    match conflict_policy {
        ConflictPolicy::Set => facts.push(catalog_fact(
            CatalogPredicate::ConflictPolicy,
            [relation, Value::symbol(Symbol::intern("set"))],
        )),
        ConflictPolicy::Functional { key_positions } => {
            facts.push(catalog_fact(
                CatalogPredicate::ConflictPolicy,
                [
                    relation.clone(),
                    Value::symbol(Symbol::intern("functional")),
                ],
            ));
            for (slot, position) in key_positions.iter().enumerate() {
                facts.push(catalog_fact(
                    CatalogPredicate::FunctionalKey,
                    [
                        relation.clone(),
                        Value::int(slot as i64).unwrap(),
                        Value::int(*position as i64).unwrap(),
                    ],
                ));
            }
        }
        ConflictPolicy::EventAppend => facts.push(catalog_fact(
            CatalogPredicate::ConflictPolicy,
            [relation, Value::symbol(Symbol::intern("event_append"))],
        )),
    }
}

fn catalog_fact<const N: usize>(predicate: CatalogPredicate, values: [Value; N]) -> CatalogFact {
    CatalogFact {
        predicate,
        tuple: Tuple::from(values),
    }
}

fn index_identity(relation_id: Identity, ordinal: u16) -> Identity {
    let raw = relation_id
        .raw()
        .wrapping_mul(65_537)
        .wrapping_add(ordinal as u64)
        & Identity::MAX;
    Identity::new(raw).unwrap()
}
