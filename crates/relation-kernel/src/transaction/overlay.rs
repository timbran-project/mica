use crate::index::{RelationMutationKind, RelationState};
use crate::radix_key::{RadixTupleKey, key_from_values};
use crate::tuple::TupleKey;
use crate::{RelationMetadata, Tuple};
use mica_var::Value;
use rart::{AdaptiveRadixTree, Slot, SlotUpdate};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LocalChange {
    Assert,
    Retract,
}

pub(super) struct RelationWriteOverlay {
    changes: OverlayChanges,
    change_count: usize,
    scan_indexes: RefCell<HashMap<Vec<u16>, LocalScanIndex>>,
    scan_requests: RefCell<HashMap<Vec<u16>, usize>>,
}

impl RelationWriteOverlay {
    pub(super) fn len(&self) -> usize {
        self.change_count
    }

    pub(super) fn insert(&mut self, tuple: Tuple, change: LocalChange) {
        for (positions, index) in self.scan_indexes.get_mut() {
            index.record_change(positions, &tuple, change);
        }
        if self.changes.insert(tuple, change) {
            self.change_count += 1;
        }
    }

    pub(super) fn for_each(&self, mut visitor: impl FnMut(&Tuple, LocalChange)) {
        self.changes.for_each(&mut visitor);
    }

    pub(super) fn try_for_each<E>(
        &self,
        mut visitor: impl FnMut(&Tuple, LocalChange) -> Result<(), E>,
    ) -> Result<(), E> {
        self.changes.try_for_each(&mut visitor)
    }

    pub(super) fn visit_touched_projected_keys<E>(
        &self,
        positions: &[u16],
        mut visitor: impl FnMut(&TupleKey, &Tuple) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut touched_keys = BTreeMap::new();
        self.for_each(|tuple, _change| {
            touched_keys.insert(TupleKey::project(tuple, positions), tuple.clone());
        });
        for (key, representative) in &touched_keys {
            visitor(key, representative)?;
        }
        Ok(())
    }

    pub(super) fn apply_ordered_changes(
        &self,
        relation: &mut RelationState,
        base_relation: &RelationState,
        mut on_applied: impl FnMut(&Tuple, RelationMutationKind),
    ) {
        if base_relation.is_empty() && relation.cardinality() == 0 && self.changes.all_are_asserts()
        {
            self.changes
                .apply_asserts_to_empty(relation, &mut on_applied);
            return;
        }

        self.changes.apply_to_relation(
            relation,
            |tuple| base_relation.contains_tuple(tuple),
            &mut on_applied,
        );
    }

    pub(super) fn visit_matches(
        &self,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
        visitor: &mut dyn FnMut(&Tuple, LocalChange),
    ) {
        let Some(prefix_positions) = best_local_prefix_positions(metadata, bindings) else {
            self.for_each(|tuple, change| {
                visitor(tuple, change);
            });
            return;
        };

        if !self.scan_indexes.borrow().contains_key(&prefix_positions) {
            let mut requests = self.scan_requests.borrow_mut();
            let request_count = requests.entry(prefix_positions.clone()).or_default();
            *request_count += 1;
            if *request_count == 1 {
                drop(requests);
                self.for_each(|tuple, change| {
                    visitor(tuple, change);
                });
                return;
            }
        }

        self.ensure_scan_index(&prefix_positions);
        let key = key_from_values(prefix_positions.iter().map(|position| {
            bindings[*position as usize]
                .as_ref()
                .expect("prefix positions should be bound")
        }));
        let indexes = self.scan_indexes.borrow();
        let index = indexes
            .get(&prefix_positions)
            .expect("ensured local scan index should exist");
        if let Some(rows) = index.rows.get_k(&key) {
            for (tuple, change) in rows {
                visitor(tuple, *change);
            }
        }
    }

    fn ensure_scan_index(&self, positions: &[u16]) {
        if self.scan_indexes.borrow().contains_key(positions) {
            return;
        }

        let mut rows = AdaptiveRadixTree::new();
        self.for_each(|tuple, change| {
            let key = key_from_values(
                positions
                    .iter()
                    .map(|position| &tuple.values()[*position as usize]),
            );
            rows.update_k(&key, |slot| match slot {
                Slot::Vacant => SlotUpdate::Insert(vec![(tuple.clone(), change)]),
                Slot::Occupied(rows) => {
                    rows.push((tuple.clone(), change));
                    SlotUpdate::Keep
                }
            });
        });
        self.scan_indexes
            .borrow_mut()
            .insert(positions.to_vec(), LocalScanIndex { rows });
    }
}

impl Default for RelationWriteOverlay {
    fn default() -> Self {
        Self {
            changes: OverlayChanges::Small(Vec::new()),
            change_count: 0,
            scan_indexes: RefCell::new(HashMap::new()),
            scan_requests: RefCell::new(HashMap::new()),
        }
    }
}

const LOCAL_RADIX_OVERLAY_THRESHOLD: usize = 64;

enum OverlayChanges {
    Small(Vec<OverlayEntry>),
    Radix(AdaptiveRadixTree<RadixTupleKey, OverlayEntry>),
}

impl OverlayChanges {
    fn for_each(&self, visitor: &mut impl FnMut(&Tuple, LocalChange)) {
        match self {
            Self::Small(changes) => {
                for entry in changes {
                    visitor(&entry.tuple, entry.change);
                }
            }
            Self::Radix(changes) => {
                for entry in changes.values_iter() {
                    visitor(&entry.tuple, entry.change);
                }
            }
        }
    }

    fn try_for_each<E>(
        &self,
        visitor: &mut impl FnMut(&Tuple, LocalChange) -> Result<(), E>,
    ) -> Result<(), E> {
        match self {
            Self::Small(changes) => {
                for entry in changes {
                    visitor(&entry.tuple, entry.change)?;
                }
            }
            Self::Radix(changes) => {
                for entry in changes.values_iter() {
                    visitor(&entry.tuple, entry.change)?;
                }
            }
        }
        Ok(())
    }

    fn all_are_asserts(&self) -> bool {
        match self {
            Self::Small(changes) => changes
                .iter()
                .all(|entry| entry.change == LocalChange::Assert),
            Self::Radix(changes) => changes
                .values_iter()
                .all(|entry| entry.change == LocalChange::Assert),
        }
    }

    fn apply_asserts_to_empty(
        &self,
        relation: &mut RelationState,
        on_applied: &mut impl FnMut(&Tuple, RelationMutationKind),
    ) {
        match self {
            Self::Small(changes) => {
                relation.apply_ordered_asserts_to_empty(
                    changes.iter().map(|entry| &entry.tuple),
                    on_applied,
                );
            }
            Self::Radix(changes) => {
                relation.apply_ordered_asserts_to_empty(
                    changes.values_iter().map(|entry| &entry.tuple),
                    on_applied,
                );
            }
        }
    }

    fn insert(&mut self, tuple: Tuple, change: LocalChange) -> bool {
        let mut inserted = false;
        let promote = match self {
            Self::Small(changes) => {
                match changes.binary_search_by(|entry| entry.tuple.cmp(&tuple)) {
                    Ok(index) => changes[index].change = change,
                    Err(index) => {
                        changes.insert(index, OverlayEntry { tuple, change });
                        inserted = true;
                    }
                }
                if changes.len() > LOCAL_RADIX_OVERLAY_THRESHOLD {
                    Some(std::mem::take(changes))
                } else {
                    None
                }
            }
            Self::Radix(changes) => {
                let key = key_from_values(tuple.values());
                inserted = changes.get_k(&key).is_none();
                changes.insert_k(&key, OverlayEntry { tuple, change });
                None
            }
        };

        if let Some(changes) = promote {
            let mut radix = AdaptiveRadixTree::new();
            for entry in changes {
                let key = key_from_values(entry.tuple.values());
                radix.insert_k(&key, entry);
            }
            *self = Self::Radix(radix);
        }
        inserted
    }

    fn apply_to_relation(
        &self,
        relation: &mut RelationState,
        mut base_contains: impl FnMut(&Tuple) -> bool,
        on_applied: &mut impl FnMut(&Tuple, RelationMutationKind),
    ) {
        match self {
            Self::Small(changes) => {
                relation.apply_ordered_changes(
                    changes
                        .iter()
                        .map(|entry| (&entry.tuple, RelationMutationKind::from(entry.change))),
                    &mut base_contains,
                    on_applied,
                );
            }
            Self::Radix(changes) => {
                relation.apply_ordered_changes(
                    changes
                        .values_iter()
                        .map(|entry| (&entry.tuple, RelationMutationKind::from(entry.change))),
                    &mut base_contains,
                    on_applied,
                );
            }
        }
    }
}

struct OverlayEntry {
    tuple: Tuple,
    change: LocalChange,
}

struct LocalScanIndex {
    rows: AdaptiveRadixTree<RadixTupleKey, Vec<(Tuple, LocalChange)>>,
}

impl LocalScanIndex {
    fn record_change(&mut self, positions: &[u16], tuple: &Tuple, change: LocalChange) {
        let key = projected_key(tuple, positions);
        self.rows.update_k(&key, |slot| match slot {
            Slot::Vacant => SlotUpdate::Insert(vec![(tuple.clone(), change)]),
            Slot::Occupied(rows) => {
                match rows.binary_search_by(|(candidate, _)| candidate.cmp(tuple)) {
                    Ok(index) => rows[index].1 = change,
                    Err(index) => rows.insert(index, (tuple.clone(), change)),
                }
                SlotUpdate::Keep
            }
        });
    }
}

pub(super) struct FunctionalVisibleMap {
    positions: Vec<u16>,
    tuples: AdaptiveRadixTree<RadixTupleKey, FunctionalVisibleEntry>,
}

impl FunctionalVisibleMap {
    pub(super) fn new(positions: &[u16]) -> Self {
        Self {
            positions: positions.to_vec(),
            tuples: AdaptiveRadixTree::new(),
        }
    }

    pub(super) fn from_writes(
        positions: &[u16],
        writes: Option<&RelationWriteOverlay>,
        mut base_tuple_for_key: impl FnMut(&Tuple) -> Option<Tuple>,
    ) -> Self {
        let mut visible = Self::new(positions);
        if let Some(writes) = writes {
            writes.for_each(|tuple, change| {
                let base_current = base_tuple_for_key(tuple);
                visible.record_change(tuple, change, base_current);
            });
        }
        visible
    }

    pub(super) fn tracked_tuple(&self, tuple: &Tuple) -> Option<Option<Tuple>> {
        let key = projected_key(tuple, &self.positions);
        self.tuples.get_k(&key).map(|entry| entry.tuple.clone())
    }

    pub(super) fn record_change_with_base_lookup(
        &mut self,
        tuple: &Tuple,
        change: LocalChange,
        base_tuple_for_key: impl FnOnce(&[u16]) -> Option<Tuple>,
    ) {
        let base_current = match change {
            LocalChange::Assert => None,
            LocalChange::Retract if self.has_entry_for(tuple) => None,
            LocalChange::Retract => base_tuple_for_key(&self.positions),
        };
        self.record_change(tuple, change, base_current);
    }

    fn has_entry_for(&self, tuple: &Tuple) -> bool {
        let key = projected_key(tuple, &self.positions);
        self.tuples.get_k(&key).is_some()
    }

    fn record_change(&mut self, tuple: &Tuple, change: LocalChange, base_current: Option<Tuple>) {
        let key = projected_key(tuple, &self.positions);
        self.tuples.update_k(&key, |slot| match slot {
            Slot::Vacant => SlotUpdate::Insert(FunctionalVisibleEntry {
                representative: tuple.clone(),
                tuple: visible_after_change(base_current, tuple, change),
            }),
            Slot::Occupied(entry) => {
                entry.record_change(tuple, change);
                SlotUpdate::Keep
            }
        });
    }

    pub(super) fn conflict_entries(&self) -> impl Iterator<Item = FunctionalConflictEntry<'_>> {
        self.tuples
            .values_iter()
            .map(|entry| FunctionalConflictEntry {
                representative: &entry.representative,
                tuple: &entry.tuple,
            })
    }
}

struct FunctionalVisibleEntry {
    representative: Tuple,
    tuple: Option<Tuple>,
}

impl FunctionalVisibleEntry {
    fn record_change(&mut self, tuple: &Tuple, change: LocalChange) {
        self.tuple = visible_after_change(self.tuple.take(), tuple, change);
    }
}

pub(super) struct FunctionalConflictEntry<'a> {
    pub(super) representative: &'a Tuple,
    pub(super) tuple: &'a Option<Tuple>,
}

fn visible_after_change(
    mut current: Option<Tuple>,
    tuple: &Tuple,
    change: LocalChange,
) -> Option<Tuple> {
    match change {
        LocalChange::Assert => Some(tuple.clone()),
        LocalChange::Retract => {
            if current.as_ref() == Some(tuple) {
                current = None;
            }
            current
        }
    }
}

fn best_local_prefix_positions(
    metadata: &RelationMetadata,
    bindings: &[Option<Value>],
) -> Option<Vec<u16>> {
    metadata
        .indexes
        .iter()
        .map(|index| (index, index.leading_bound_count(bindings)))
        .filter(|(_, count)| *count > 0)
        .max_by_key(|(_, count)| *count)
        .map(|(index, count)| index.positions.iter().take(count).copied().collect())
}

pub(super) fn projected_key(tuple: &Tuple, positions: &[u16]) -> RadixTupleKey {
    key_from_values(
        positions
            .iter()
            .map(|position| &tuple.values()[*position as usize]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mica_var::{Identity, Symbol};

    fn matches(
        overlay: &RelationWriteOverlay,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Vec<(Tuple, LocalChange)> {
        let mut rows = Vec::new();
        overlay.visit_matches(metadata, bindings, &mut |tuple, change| {
            if tuple.matches_bindings(bindings) {
                rows.push((tuple.clone(), change));
            }
        });
        rows
    }

    #[test]
    fn local_indexes_survive_writes_and_preserve_order_through_radix_promotion() {
        let metadata =
            RelationMetadata::new(Identity::new(1).unwrap(), Symbol::intern("LocalIndexed"), 2)
                .with_index([1]);
        let mut overlay = RelationWriteOverlay::default();
        let mut expected = BTreeMap::new();
        let bindings = [Some(Value::int(2).unwrap()), None];
        for _ in 0..2 {
            assert!(matches(&overlay, &metadata, &bindings).is_empty());
        }
        assert_eq!(overlay.scan_indexes.borrow().len(), 1);
        let second = [None, Some(Value::string("é🦀"))];
        for _ in 0..2 {
            assert!(matches(&overlay, &metadata, &second).is_empty());
        }
        assert_eq!(overlay.scan_indexes.borrow().len(), 2);

        // A permuted insertion order crosses the compact overlay threshold.
        // Later rounds replace existing changes, including retract/assert cycles.
        for round in 0..3 {
            for index in 0..160 {
                let value = (index * 73) % 160;
                let tuple = Tuple::from([
                    Value::int(value % 5).unwrap(),
                    Value::string(format!("é🦀/{value:03}/{}", "x".repeat(80))),
                ]);
                let change = if round == 1 {
                    LocalChange::Retract
                } else {
                    LocalChange::Assert
                };
                expected.insert(tuple.clone(), change);
                overlay.insert(tuple.clone(), change);
                assert_eq!(
                    overlay.scan_indexes.borrow().len(),
                    2,
                    "a write discarded a maintained index"
                );
                for probe in [
                    bindings.clone(),
                    [None, Some(tuple.values()[1].clone())],
                    [None, Some(Value::string("absent"))],
                ] {
                    let wanted = expected
                        .iter()
                        .filter(|(tuple, _)| tuple.matches_bindings(&probe))
                        .map(|(tuple, change)| (tuple.clone(), *change))
                        .collect::<Vec<_>>();
                    assert_eq!(matches(&overlay, &metadata, &probe), wanted);
                }
            }
        }
        assert_eq!(overlay.len(), 160);
    }

    #[test]
    fn repeated_probes_across_writes_build_an_index() {
        let metadata = RelationMetadata::new(Identity::new(1).unwrap(), Symbol::intern("Local"), 2);
        let mut overlay = RelationWriteOverlay::default();
        let bindings = [Some(Value::int(1).unwrap()), None];
        assert!(matches(&overlay, &metadata, &bindings).is_empty());
        overlay.insert(
            Tuple::from([Value::int(1).unwrap(), Value::int(2).unwrap()]),
            LocalChange::Assert,
        );
        assert_eq!(matches(&overlay, &metadata, &bindings).len(), 1);
        assert_eq!(
            overlay.scan_indexes.borrow().len(),
            1,
            "writes reset repeated-probe history"
        );
    }
}
