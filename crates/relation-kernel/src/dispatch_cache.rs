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

use crate::{ApplicableMethodCall, DispatchRelations};
use mica_var::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

#[derive(Clone, Debug)]
pub(crate) struct DispatchCache {
    entries: Arc<RwLock<BTreeMap<DispatchCacheKey, Arc<[ApplicableMethodCall]>>>>,
    positional_entries: Arc<RwLock<PositionalDispatchEntries>>,
}

type PositionalDispatchEntries = BTreeMap<DispatchRelationsKey, BTreeMap<Value, PositionalMethods>>;

#[derive(Debug, Default)]
struct PositionalMethods {
    by_arity: BTreeMap<usize, Arc<[Value]>>,
    by_values: BTreeMap<Vec<Value>, Arc<[Value]>>,
}

impl DispatchCache {
    pub(crate) fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(BTreeMap::new())),
            positional_entries: Arc::new(RwLock::new(BTreeMap::new())),
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
    ) -> Option<Arc<[Value]>> {
        let entries = self.positional_entries.read().unwrap();
        let methods = entries
            .get(&DispatchRelationsKey::from(relations))?
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
        methods: Arc<[Value]>,
        argument_independent: bool,
    ) {
        let mut entries = self.positional_entries.write().unwrap();
        let entry = entries
            .entry(DispatchRelationsKey::from(relations))
            .or_default()
            .entry(selector.clone())
            .or_default();
        if argument_independent {
            entry.by_arity.entry(args.len()).or_insert(methods);
            return;
        }
        entry.by_values.entry(args.to_vec()).or_insert(methods);
    }

    fn insert_key(&self, key: DispatchCacheKey, methods: Vec<ApplicableMethodCall>) {
        self.entries
            .write()
            .unwrap()
            .entry(key)
            .or_insert_with(|| methods.into());
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
        let methods = Arc::<[Value]>::from([value(7), value(8)]);

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
        let stable = Arc::<[Value]>::from([value(9)]);
        cache.insert_positional(relations, &selector, &[], Arc::clone(&stable), false);
        std::thread::scope(|scope| {
            for worker in 0..4 {
                let cache = &cache;
                let selector = &selector;
                let stable = &stable;
                scope.spawn(move || {
                    for item in 0..512 {
                        let argument = value(worker * 512 + item);
                        let methods = Arc::<[Value]>::from([argument.clone()]);
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
        for item in 0..2048 {
            let argument = value(item);
            assert_eq!(
                &*cache
                    .get_positional(relations, &selector, std::slice::from_ref(&argument))
                    .unwrap(),
                &[argument]
            );
        }
        assert_eq!(&*stable, &[value(9)]);
    }
}
