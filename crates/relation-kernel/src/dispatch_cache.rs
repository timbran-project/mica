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
    ApplicableMethod, ApplicableMethodCall, ApplicablePositionalMethod, DispatchRelations,
};
use mica_var::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

#[derive(Clone, Debug)]
pub(crate) struct DispatchCache {
    entries: Arc<RwLock<BTreeMap<DispatchCacheKey, Arc<[ApplicableMethodCall]>>>>,
    positional_entries: Arc<RwLock<PositionalDispatchEntries>>,
    candidates: Arc<RwLock<BTreeMap<CandidateKey, Arc<[ApplicableMethod]>>>>,
}

type CandidateKey = (DispatchRelationsKey, Value);

type PositionalDispatchEntries = BTreeMap<DispatchRelationsKey, PositionalRelationEntries>;

#[derive(Debug, Default)]
struct PositionalRelationEntries {
    selectors: BTreeMap<Value, PositionalMethods>,
    count: usize,
}

#[derive(Debug, Default)]
struct PositionalMethods {
    by_arity: BTreeMap<usize, Arc<[ApplicablePositionalMethod]>>,
    by_values: BTreeMap<Vec<Value>, Arc<[ApplicablePositionalMethod]>>,
}

// Bound retained argument values and selector candidates within each snapshot.
const MAX_POSITIONAL_ENTRIES_PER_RELATION: usize = 1024;
const MAX_KEYED_ENTRIES: usize = 4096;

impl DispatchCache {
    pub(crate) fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(BTreeMap::new())),
            positional_entries: Arc::new(RwLock::new(BTreeMap::new())),
            candidates: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    pub(crate) fn get_candidates(
        &self,
        relations: DispatchRelations,
        selector: &Value,
    ) -> Option<Arc<[ApplicableMethod]>> {
        let key = (DispatchRelationsKey::from(relations), selector.clone());
        self.candidates.read().unwrap().get(&key).map(Arc::clone)
    }

    pub(crate) fn insert_candidates(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        candidates: Arc<[ApplicableMethod]>,
    ) {
        let key = (DispatchRelationsKey::from(relations), selector.clone());
        let mut entries = self.candidates.write().unwrap();
        if entries.len() < MAX_KEYED_ENTRIES {
            entries.entry(key).or_insert(candidates);
        }
    }

    pub(crate) fn get(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        roles: &[(Value, Value)],
    ) -> Option<Vec<ApplicableMethodCall>> {
        let key = DispatchCacheKey::new(relations, selector, roles);
        self.get_key(&key)
    }

    pub(crate) fn get_normalized(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        roles: &[(Value, Value)],
    ) -> Option<Vec<ApplicableMethodCall>> {
        let key = DispatchCacheKey::new_normalized(relations, selector, roles);
        self.get_key(&key)
    }

    fn get_key(&self, key: &DispatchCacheKey) -> Option<Vec<ApplicableMethodCall>> {
        let entries = self.entries.read().unwrap();
        entries.get(key).map(|methods| methods.to_vec())
    }

    pub(crate) fn insert(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        roles: &[(Value, Value)],
        methods: Vec<ApplicableMethodCall>,
    ) {
        let key = DispatchCacheKey::new(relations, selector, roles);
        self.insert_key(key, methods);
    }

    pub(crate) fn insert_normalized(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        roles: &[(Value, Value)],
        methods: Vec<ApplicableMethodCall>,
    ) {
        let key = DispatchCacheKey::new_normalized(relations, selector, roles);
        self.insert_key(key, methods);
    }

    pub(crate) fn get_positional(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        args: &[Value],
    ) -> Option<Arc<[ApplicablePositionalMethod]>> {
        let entries = self.positional_entries.read().unwrap();
        let methods = entries
            .get(&DispatchRelationsKey::from(relations))?
            .selectors
            .get(selector)?;
        methods
            .by_arity
            .get(&args.len())
            .or_else(|| methods.by_values.get(args))
            .cloned()
    }

    pub(crate) fn insert_positional(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        args: &[Value],
        methods: Arc<[ApplicablePositionalMethod]>,
        argument_independent: bool,
    ) {
        let mut entries = self.positional_entries.write().unwrap();
        let relation = entries
            .entry(DispatchRelationsKey::from(relations))
            .or_default();
        if relation.count >= MAX_POSITIONAL_ENTRIES_PER_RELATION {
            return;
        }
        let entry = relation.selectors.entry(selector.clone()).or_default();
        let inserted = if argument_independent {
            match entry.by_arity.entry(args.len()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(methods);
                    true
                }
                std::collections::btree_map::Entry::Occupied(_) => false,
            }
        } else {
            match entry.by_values.entry(args.to_vec()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(methods);
                    true
                }
                std::collections::btree_map::Entry::Occupied(_) => false,
            }
        };
        relation.count += usize::from(inserted);
    }

    fn insert_key(&self, key: DispatchCacheKey, methods: Vec<ApplicableMethodCall>) {
        let mut entries = self.entries.write().unwrap();
        if entries.len() < MAX_KEYED_ENTRIES {
            entries.entry(key).or_insert_with(|| methods.into());
        }
    }
}

impl Default for DispatchCache {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct DispatchCacheKey {
    relations: DispatchRelationsKey,
    selector: Value,
    roles: Vec<(Value, Value)>,
}

impl DispatchCacheKey {
    fn new(relations: DispatchRelations, selector: &Value, roles: &[(Value, Value)]) -> Self {
        let mut roles = roles.to_vec();
        crate::normalize_dispatch_roles(&mut roles);
        Self::from_normalized_roles(relations, selector, roles)
    }

    fn new_normalized(
        relations: DispatchRelations,
        selector: &Value,
        roles: &[(Value, Value)],
    ) -> Self {
        Self::from_normalized_roles(relations, selector, roles.to_vec())
    }

    fn from_normalized_roles(
        relations: DispatchRelations,
        selector: &Value,
        roles: Vec<(Value, Value)>,
    ) -> Self {
        Self {
            relations: DispatchRelationsKey::from(relations),
            selector: selector.clone(),
            roles,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct DispatchRelationsKey {
    method_selector: crate::RelationId,
    param: crate::RelationId,
    delegates: crate::RelationId,
}

impl From<DispatchRelations> for DispatchRelationsKey {
    fn from(value: DispatchRelations) -> Self {
        Self {
            method_selector: value.method_selector,
            param: value.param,
            delegates: value.delegates,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mica_var::Identity;

    fn relation(id: u64) -> crate::RelationId {
        Identity::new(id).unwrap()
    }

    fn value(value: i64) -> Value {
        Value::int(value).unwrap()
    }

    #[test]
    fn positional_cache_hits_share_the_published_method_slice() {
        let cache = DispatchCache::new();
        let relations = DispatchRelations {
            method_selector: relation(1),
            param: relation(2),
            delegates: relation(3),
        };
        let selector = value(4);
        let args = [value(5), value(6)];
        let methods = Arc::<[ApplicablePositionalMethod]>::from(
            [value(7), value(8)].map(ApplicablePositionalMethod::required),
        );

        cache.insert_positional(relations, &selector, &args, Arc::clone(&methods), true);

        let first = cache.get_positional(relations, &selector, &args).unwrap();
        let second = cache.get_positional(relations, &selector, &args).unwrap();
        assert!(Arc::ptr_eq(&methods, &first));
        assert!(Arc::ptr_eq(&first, &second));
        let changed = [Value::list([value(99)]), Value::string("different")];
        assert!(Arc::ptr_eq(
            &first,
            &cache
                .get_positional(relations, &selector, &changed)
                .unwrap()
        ));
        assert!(
            cache
                .get_positional(relations, &selector, &changed[..1])
                .is_none()
        );
    }
    #[test]
    fn concurrent_cache_publication_preserves_entries_and_owned_results() {
        let cache = DispatchCache::new();
        let relations = DispatchRelations {
            method_selector: relation(1),
            param: relation(2),
            delegates: relation(3),
        };
        let selector = value(4);
        let stable = Arc::<[ApplicablePositionalMethod]>::from(
            [value(9)].map(ApplicablePositionalMethod::required),
        );
        cache.insert_positional(relations, &selector, &[], Arc::clone(&stable), false);
        std::thread::scope(|scope| {
            for worker in 0..4 {
                let cache = &cache;
                let selector = &selector;
                let stable = &stable;
                scope.spawn(move || {
                    for item in 0..128 {
                        let argument = value(worker * 128 + item);
                        let methods = Arc::<[ApplicablePositionalMethod]>::from(
                            [argument.clone()].map(ApplicablePositionalMethod::required),
                        );
                        cache.insert_positional(
                            relations,
                            selector,
                            std::slice::from_ref(&argument),
                            Arc::clone(&methods),
                            false,
                        );
                        assert!(Arc::ptr_eq(
                            &methods,
                            &cache
                                .get_positional(
                                    relations,
                                    selector,
                                    std::slice::from_ref(&argument)
                                )
                                .unwrap()
                        ));
                        assert!(Arc::ptr_eq(
                            stable,
                            &cache.get_positional(relations, selector, &[]).unwrap()
                        ));
                        let roles = [(value(1), argument.clone())];
                        let call = ApplicableMethodCall {
                            method: argument,
                            args: None,
                        };
                        cache.insert(relations, selector, &roles, vec![call.clone()]);
                        assert_eq!(cache.get(relations, selector, &roles), Some(vec![call]));
                    }
                });
            }
        });
        for item in 0..512 {
            let argument = value(item);
            assert_eq!(
                &*cache
                    .get_positional(relations, &selector, std::slice::from_ref(&argument))
                    .unwrap(),
                &[ApplicablePositionalMethod::required(argument)]
            );
        }
        assert_eq!(&*stable, &[ApplicablePositionalMethod::required(value(9))]);
    }
    #[test]
    fn full_positional_cache_preserves_existing_entries() {
        let cache = DispatchCache::new();
        let relations = DispatchRelations {
            method_selector: relation(1),
            param: relation(2),
            delegates: relation(3),
        };
        let selector = value(4);
        let methods: Arc<[ApplicablePositionalMethod]> =
            [ApplicablePositionalMethod::required(value(9))].into();
        for item in 0..MAX_POSITIONAL_ENTRIES_PER_RELATION {
            cache.insert_positional(
                relations,
                &selector,
                &[value(item as i64)],
                Arc::clone(&methods),
                false,
            );
        }
        cache.insert_positional(
            relations,
            &selector,
            &[value(-1)],
            Arc::clone(&methods),
            false,
        );
        assert!(
            cache
                .get_positional(relations, &selector, &[value(-1)])
                .is_none()
        );
        for item in 0..MAX_POSITIONAL_ENTRIES_PER_RELATION {
            assert!(Arc::ptr_eq(
                &methods,
                &cache
                    .get_positional(relations, &selector, &[value(item as i64)])
                    .unwrap()
            ));
        }
    }
}
