use crate::ScanControl;
use crate::error::KernelError;
use crate::tuple::Tuple;
use mica_var::Value;
use rart::{VersionedAdaptiveRadixTree, VisitControl};
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use crate::radix_key::{RadixTupleKey, key_from_values};

use super::natural_prefix_covers_all_bindings;

const TUPLE_STORE_RADIX_THRESHOLD: usize = 4096;

#[derive(Clone)]
pub(super) enum TupleStore {
    Small(Arc<BTreeSet<Tuple>>),
    Radix {
        entries: VersionedAdaptiveRadixTree<RadixTupleKey, Tuple>,
        len: usize,
    },
}

impl fmt::Debug for TupleStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TupleStore")
            .field("len", &self.len())
            .field(
                "kind",
                match self {
                    Self::Small(_) => &"small",
                    Self::Radix { .. } => &"radix",
                },
            )
            .finish_non_exhaustive()
    }
}

impl TupleStore {
    pub(super) fn empty() -> Self {
        Self::Small(Arc::new(BTreeSet::new()))
    }

    pub(super) fn from_sorted_unique(tuples: &[Tuple]) -> Self {
        if tuples.len() <= TUPLE_STORE_RADIX_THRESHOLD {
            return Self::Small(Arc::new(tuples.iter().cloned().collect()));
        }

        let mut entries = VersionedAdaptiveRadixTree::new();
        for tuple in tuples {
            let key = key_from_values(tuple.values());
            entries.insert_k(&key, tuple.clone());
        }
        Self::Radix {
            entries,
            len: tuples.len(),
        }
    }

    pub(super) fn len(&self) -> usize {
        match self {
            Self::Small(tuples) => tuples.len(),
            Self::Radix { len, .. } => *len,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(super) fn storage_kind(&self) -> &'static str {
        match self {
            Self::Small(_) => "btree",
            Self::Radix { .. } => "radix",
        }
    }

    pub(super) fn contains(&self, tuple: &Tuple) -> bool {
        match self {
            Self::Small(tuples) => tuples.contains(tuple),
            Self::Radix { entries, .. } => {
                let key = key_from_values(tuple.values());
                entries.get_k(&key).is_some()
            }
        }
    }

    pub(super) fn tuple_for_values(&self, values: &[Value]) -> Option<Tuple> {
        match self {
            Self::Small(tuples) => {
                let tuple = Tuple::new(values.iter().cloned());
                tuples.get(&tuple).cloned()
            }
            Self::Radix { entries, .. } => {
                let key = key_from_values(values);
                entries.get_k(&key).cloned()
            }
        }
    }

    pub(super) fn tuple_for_tuple(&self, tuple: &Tuple) -> Option<Tuple> {
        match self {
            Self::Small(tuples) => tuples.get(tuple).cloned(),
            Self::Radix { entries, .. } => {
                let key = key_from_values(tuple.values());
                entries.get_k(&key).cloned()
            }
        }
    }

    pub(super) fn insert(&mut self, tuple: Tuple) -> bool {
        match self {
            Self::Small(tuples) => {
                let tuples = Arc::make_mut(tuples);
                let inserted = tuples.insert(tuple);
                if inserted && tuples.len() > TUPLE_STORE_RADIX_THRESHOLD {
                    self.promote_to_radix();
                }
                inserted
            }
            Self::Radix { entries, len } => {
                let key = key_from_values(tuple.values());
                if entries.insert_k(&key, tuple) {
                    return false;
                }
                *len += 1;
                true
            }
        }
    }

    pub(super) fn remove(&mut self, tuple: &Tuple) -> bool {
        match self {
            Self::Small(tuples) => Arc::make_mut(tuples).remove(tuple),
            Self::Radix { entries, len } => {
                let key = key_from_values(tuple.values());
                if !entries.delete_k(&key) {
                    return false;
                }
                *len -= 1;
                true
            }
        }
    }

    pub(super) fn estimate_prefix_count(
        &self,
        bindings: &[Option<Value>],
        bound_count: usize,
    ) -> Result<usize, KernelError> {
        let row_count = self.len();
        if row_count == 0 || bound_count == 0 {
            return Ok(row_count);
        }
        let estimated_distinct_prefixes = (row_count as f64)
            .powf(bound_count as f64 / bindings.len() as f64)
            .round()
            .max(1.0) as usize;
        Ok(row_count.div_ceil(estimated_distinct_prefixes))
    }

    pub(super) fn matching(&self, bindings: &[Option<Value>]) -> Vec<Tuple> {
        if bindings.iter().all(Option::is_none) {
            return self.all_tuples();
        }

        let mut out = Vec::new();
        self.for_each_matching(bindings, |tuple| out.push(tuple.clone()));
        out
    }

    fn all_tuples(&self) -> Vec<Tuple> {
        let mut out = Vec::with_capacity(self.len());
        match self {
            Self::Small(tuples) => out.extend(tuples.iter().cloned()),
            Self::Radix { entries, .. } => out.extend(entries.values_iter().cloned()),
        }
        out
    }

    pub(super) fn scan_prefix(
        &self,
        bindings: &[Option<Value>],
        bound_count: usize,
    ) -> Result<Vec<Tuple>, KernelError> {
        let mut out = Vec::new();
        self.visit_prefix(bindings, bound_count, &mut |tuple| {
            out.push(tuple.clone());
            Ok(ScanControl::Continue)
        })?;
        Ok(out)
    }

    pub(super) fn visit_prefix(
        &self,
        bindings: &[Option<Value>],
        bound_count: usize,
        visitor: &mut dyn FnMut(&Tuple) -> Result<ScanControl, KernelError>,
    ) -> Result<(), KernelError> {
        match self {
            Self::Small(_) => self.try_for_each_matching(bindings, &mut |tuple| {
                Ok(visitor(tuple)? == ScanControl::Stop)
            }),
            Self::Radix { entries, .. } => {
                let prefix_covers_all_bindings =
                    natural_prefix_covers_all_bindings(bindings, bound_count);
                let prefix = key_from_values(
                    bindings
                        .iter()
                        .take(bound_count)
                        .map(|binding| binding.as_ref().expect("natural prefix should be bound")),
                );
                entries.try_prefix_values_for_each_k(&prefix, |tuple| {
                    if !prefix_covers_all_bindings && !tuple.matches_bindings(bindings) {
                        return Ok(VisitControl::Continue);
                    }
                    match visitor(tuple) {
                        Ok(ScanControl::Continue) => Ok(VisitControl::Continue),
                        Ok(ScanControl::Stop) => Ok(VisitControl::Stop),
                        Err(error) => Err(error),
                    }
                })
            }
        }
    }

    pub(super) fn try_for_each_matching(
        &self,
        bindings: &[Option<Value>],
        visitor: &mut dyn FnMut(&Tuple) -> Result<bool, KernelError>,
    ) -> Result<(), KernelError> {
        if bindings.iter().all(Option::is_none) {
            return self.try_for_each_tuple(visitor);
        }

        match self {
            Self::Small(tuples) => {
                for tuple in tuples.iter() {
                    if tuple.matches_bindings(bindings) && visitor(tuple)? {
                        return Ok(());
                    }
                }
            }
            Self::Radix { entries, .. } => {
                for tuple in entries.values_iter() {
                    if tuple.matches_bindings(bindings) && visitor(tuple)? {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn matching_row_pairs(&self, other: &Self, mut visitor: impl FnMut(&Tuple, &Tuple)) {
        match (self, other) {
            (Self::Small(left), Self::Small(right)) => {
                for tuple in left.intersection(right) {
                    visitor(tuple, tuple);
                }
            }
            (Self::Small(left), Self::Radix { entries, .. }) => {
                for left_tuple in left.iter() {
                    let key = key_from_values(left_tuple.values());
                    if let Some(right_tuple) = entries.get_k(&key) {
                        visitor(left_tuple, right_tuple);
                    }
                }
            }
            (Self::Radix { entries, .. }, Self::Small(right)) => {
                for right_tuple in right.iter() {
                    let key = key_from_values(right_tuple.values());
                    if let Some(left_tuple) = entries.get_k(&key) {
                        visitor(left_tuple, right_tuple);
                    }
                }
            }
            (
                Self::Radix {
                    entries: left_entries,
                    ..
                },
                Self::Radix {
                    entries: right_entries,
                    ..
                },
            ) => {
                left_entries.intersect_values_with(right_entries, visitor);
            }
        }
    }

    fn for_each_matching(&self, bindings: &[Option<Value>], mut visitor: impl FnMut(&Tuple)) {
        match self {
            Self::Small(tuples) => {
                for tuple in tuples.iter() {
                    if tuple.matches_bindings(bindings) {
                        visitor(tuple);
                    }
                }
            }
            Self::Radix { entries, .. } => {
                for tuple in entries.values_iter() {
                    if tuple.matches_bindings(bindings) {
                        visitor(tuple);
                    }
                }
            }
        }
    }

    fn try_for_each_tuple(
        &self,
        visitor: &mut dyn FnMut(&Tuple) -> Result<bool, KernelError>,
    ) -> Result<(), KernelError> {
        match self {
            Self::Small(tuples) => {
                for tuple in tuples.iter() {
                    if visitor(tuple)? {
                        return Ok(());
                    }
                }
            }
            Self::Radix { entries, .. } => {
                for tuple in entries.values_iter() {
                    if visitor(tuple)? {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    fn promote_to_radix(&mut self) {
        let Self::Small(tuples) = std::mem::replace(self, Self::empty()) else {
            return;
        };
        let tuples = match Arc::try_unwrap(tuples) {
            Ok(tuples) => tuples,
            Err(tuples) => (*tuples).clone(),
        };
        let len = tuples.len();
        let mut entries = VersionedAdaptiveRadixTree::new();
        for tuple in tuples {
            let key = key_from_values(tuple.values());
            entries.insert_k(&key, tuple);
        }
        *self = Self::Radix { entries, len };
    }
}
