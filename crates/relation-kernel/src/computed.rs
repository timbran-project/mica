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
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

pub trait ComputedRelationRead: RelationRead {
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

pub trait ComputedRelation: Send + Sync {
    fn name(&self) -> &'static str;

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
}

impl ComputedRelationRegistry {
    pub fn new(relations: impl IntoIterator<Item = Arc<dyn ComputedRelation>>) -> Self {
        Self {
            relations: relations.into_iter().collect(),
            bindings: HashMap::new(),
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

    pub fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Option<Result<Vec<Tuple>, KernelError>> {
        let relation = self.find(metadata)?;
        Some(scan_checked(relation.as_ref(), reader, metadata, bindings))
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
    relation.scan(reader, metadata, bindings)
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
}
