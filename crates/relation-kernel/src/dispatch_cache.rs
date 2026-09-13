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

use crate::{ApplicableMethod, ApplicableMethodCall, DispatchRelations};
use arc_swap::ArcSwap;
use mica_var::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub(crate) struct DispatchCache {
    entries: Arc<ArcSwap<BTreeMap<DispatchCacheKey, Arc<[ApplicableMethodCall]>>>>,
    positional_entries: Arc<ArcSwap<PositionalDispatchEntries>>,
    /// Candidate methods and their params per `(relations, selector)`. This is
    /// the value-independent part of dispatch: applicability still depends on
    /// the argument values (restrictions), but the candidate set and the param
    /// rows do not, so they are scanned once per selector rather than once per
    /// call.
    candidates: Arc<ArcSwap<BTreeMap<CandidateKey, Arc<[ApplicableMethod]>>>>,
    publish_lock: Arc<Mutex<()>>,
}

type CandidateKey = (DispatchRelationsKey, Value);

type PositionalDispatchEntries =
    BTreeMap<DispatchRelationsKey, Arc<[PositionalDispatchCacheEntry]>>;

/// Upper bound on cached positional entries per relation. The cache key
/// includes argument values, so an unbounded cache grows without limit for a
/// loop that dispatches on changing data; insertion also rewrites the
/// relation's entry vector, which is O(n) per insert. Both problems go away
/// with a bound: past the cap, calls simply take the uncached path.
const MAX_POSITIONAL_ENTRIES_PER_RELATION: usize = 1024;

/// Upper bound on keyed (named-dispatch) entries. The same O(n)-per-insert
/// rewrite applies, and the key holds a role vector per entry.
const MAX_KEYED_ENTRIES: usize = 4096;

impl DispatchCache {
    pub(crate) fn new() -> Self {
        Self {
            entries: Arc::new(ArcSwap::from_pointee(BTreeMap::new())),
            positional_entries: Arc::new(ArcSwap::from_pointee(BTreeMap::new())),
            candidates: Arc::new(ArcSwap::from_pointee(BTreeMap::new())),
            publish_lock: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn get_candidates(
        &self,
        relations: DispatchRelations,
        selector: &Value,
    ) -> Option<Arc<[ApplicableMethod]>> {
        let key = (DispatchRelationsKey::from(relations), selector.clone());
        self.candidates.load().get(&key).map(Arc::clone)
    }

    pub(crate) fn insert_candidates(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        candidates: Arc<[ApplicableMethod]>,
    ) {
        let key = (DispatchRelationsKey::from(relations), selector.clone());
        let _guard = self.publish_lock.lock().unwrap();
        let entries = self.candidates.load_full();
        if entries.contains_key(&key) || entries.len() >= MAX_KEYED_ENTRIES {
            return;
        }
        let mut next = (*entries).clone();
        next.insert(key, candidates);
        self.candidates.store(Arc::new(next));
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
        let entries = self.entries.load();
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
        let entries = self.positional_entries.load();
        let entries = entries.get(&DispatchRelationsKey::from(relations))?;
        let index = entries
            .binary_search_by(|entry| entry.compare_key(selector, args))
            .ok()?;
        Some(Arc::clone(&entries[index].methods))
    }

    pub(crate) fn insert_positional(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        args: &[Value],
        methods: Arc<[Value]>,
    ) {
        let relations = DispatchRelationsKey::from(relations);
        let _guard = self.publish_lock.lock().unwrap();
        let entries = self.positional_entries.load_full();
        let existing = entries.get(&relations);
        // Past the cap, stop inserting: the entry vector rewrite below is
        // O(cached entries) and the key contains argument values, so an
        // unbounded cache would grow and slow down with every distinct call.
        if existing.map_or(0, |entries| entries.len()) >= MAX_POSITIONAL_ENTRIES_PER_RELATION {
            return;
        }
        let mut relation_entries = existing.map_or_else(Vec::new, |entries| entries.to_vec());
        let index =
            match relation_entries.binary_search_by(|entry| entry.compare_key(selector, args)) {
                Ok(_) => return,
                Err(index) => index,
            };
        relation_entries.insert(
            index,
            PositionalDispatchCacheEntry {
                selector: selector.clone(),
                args: args.to_vec(),
                methods,
            },
        );
        let mut next = (*entries).clone();
        next.insert(relations, Arc::from(relation_entries));
        self.positional_entries.store(Arc::new(next));
    }

    fn insert_key(&self, key: DispatchCacheKey, methods: Vec<ApplicableMethodCall>) {
        let _guard = self.publish_lock.lock().unwrap();
        let entries = self.entries.load_full();
        if entries.contains_key(&key) {
            return;
        }
        // Bound the keyed cache too: insertion rewrites the whole map, so an
        // unbounded cache is O(n) per distinct role vector.
        if entries.len() >= MAX_KEYED_ENTRIES {
            return;
        }
        let methods = Arc::<[ApplicableMethodCall]>::from(methods);
        let mut next = (*entries).clone();
        next.insert(key, methods);
        self.entries.store(Arc::new(next));
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

#[derive(Clone, Debug)]
struct PositionalDispatchCacheEntry {
    selector: Value,
    args: Vec<Value>,
    methods: Arc<[Value]>,
}

impl PositionalDispatchCacheEntry {
    fn compare_key(&self, selector: &Value, args: &[Value]) -> std::cmp::Ordering {
        self.selector
            .cmp(selector)
            .then_with(|| self.args.as_slice().cmp(args))
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

        cache.insert_positional(relations, &selector, &args, Arc::clone(&methods));

        let first = cache.get_positional(relations, &selector, &args).unwrap();
        let second = cache.get_positional(relations, &selector, &args).unwrap();
        assert!(Arc::ptr_eq(&methods, &first));
        assert!(Arc::ptr_eq(&first, &second));
    }
}
