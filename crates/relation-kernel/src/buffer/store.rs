// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{Delta, DeltaBudget, DeltaError, Text, TextError};
use crate::{KernelError, RelationDurability, Snapshot, Transaction};
use mica_var::{Identity, Symbol};
use rart::{ArrayKey, VersionedAdaptiveRadixTree};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BufferConflictPolicy {
    #[default]
    Reject,
    Span,
    Whole,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BufferMetadata {
    pub id: Identity,
    pub name: Symbol,
    pub durability: RelationDurability,
    pub conflict: BufferConflictPolicy,
}

#[derive(Clone, Debug)]
pub struct BufferState {
    pub(crate) metadata: BufferMetadata,
    pub(crate) revision: u64,
    pub(crate) text: Text,
    pub(crate) deleted: bool,
}

impl BufferState {
    pub fn metadata(&self) -> &BufferMetadata {
        &self.metadata
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn text(&self) -> &Text {
        &self.text
    }
    pub fn is_deleted(&self) -> bool {
        self.deleted
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistedBufferState {
    pub metadata: BufferMetadata,
    pub revision: u64,
    pub text: String,
    pub deleted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BufferChange {
    pub metadata: BufferMetadata,
    pub base_revision: u64,
    pub revision: u64,
    pub delta: Delta,
    pub deleted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BufferError {
    Unknown,
    AlreadyExists,
    MetadataMismatch,
    Conflict { expected: u64, actual: u64 },
    RevisionExhausted,
    Text(TextError),
    Delta(DeltaError),
    InvalidRecovery,
}

pub(crate) fn error(buffer: Identity, error: BufferError) -> KernelError {
    KernelError::Buffer { buffer, error }
}

#[derive(Clone)]
pub(crate) struct BufferStates {
    entries: VersionedAdaptiveRadixTree<ArrayKey<8>, Arc<BufferState>>,
    names: Arc<HashMap<Symbol, Identity>>,
}

impl Default for BufferStates {
    fn default() -> Self {
        Self {
            entries: VersionedAdaptiveRadixTree::new(),
            names: Arc::new(HashMap::new()),
        }
    }
}

impl fmt::Debug for BufferStates {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BufferStates")
            .field("names", &self.names)
            .finish_non_exhaustive()
    }
}

impl BufferStates {
    pub(crate) fn get(&self, id: Identity) -> Option<&Arc<BufferState>> {
        self.entries.get(id.raw())
    }
    pub(crate) fn named(&self, name: Symbol) -> Option<&Arc<BufferState>> {
        self.names.get(&name).and_then(|id| self.get(*id))
    }
    pub(crate) fn values(&self) -> impl Iterator<Item = &Arc<BufferState>> {
        self.entries.values_iter()
    }

    fn insert(&mut self, state: BufferState) {
        let id = state.metadata.id;
        let name = state.metadata.name;
        if state.deleted && self.names.get(&name) == Some(&id) {
            Arc::make_mut(&mut self.names).remove(&name);
        } else if !state.deleted && self.names.get(&name) != Some(&id) {
            Arc::make_mut(&mut self.names).insert(name, id);
        }
        self.entries.insert(id.raw(), Arc::new(state));
    }

    pub(crate) fn restore(records: Vec<PersistedBufferState>) -> Result<Self, KernelError> {
        let mut states = Self::default();
        for record in records {
            let id = record.metadata.id;
            if record.revision == 0
                || (record.deleted && !record.text.is_empty())
                || states.get(id).is_some()
                || (!record.deleted && states.named(record.metadata.name).is_some())
            {
                return Err(error(id, BufferError::InvalidRecovery));
            }
            let (text, revision) =
                if record.metadata.durability == RelationDurability::Volatile && !record.deleted {
                    (
                        Text::default(),
                        record
                            .revision
                            .checked_add(1)
                            .ok_or_else(|| error(id, BufferError::RevisionExhausted))?,
                    )
                } else {
                    (Text::from_text(&record.text), record.revision)
                };
            states.insert(BufferState {
                metadata: record.metadata,
                revision,
                text,
                deleted: record.deleted,
            });
        }
        Ok(states)
    }

    pub(crate) fn replay(&mut self, change: &BufferChange) -> Result<(), KernelError> {
        let id = change.metadata.id;
        let previous = self.get(id);
        let previous_revision = previous.map_or(0, |state| state.revision);
        if change.revision <= previous_revision
            || change.base_revision >= change.revision
            || (previous.is_none() && change.base_revision != 0)
            || change.base_revision < previous_revision
            || previous.is_some_and(|state| state.metadata != change.metadata || state.deleted)
            || (change.metadata.durability == RelationDurability::Durable
                && change.base_revision != previous_revision)
            || (previous.is_none() && self.named(change.metadata.name).is_some())
        {
            return Err(error(id, BufferError::InvalidRecovery));
        }
        let text = if change.deleted || change.metadata.durability == RelationDurability::Volatile {
            Text::default()
        } else {
            change
                .delta
                .apply(previous.map_or(&Text::default(), |state| &state.text))
                .map_err(|failure| error(id, BufferError::Delta(failure)))?
        };
        self.insert(BufferState {
            metadata: change.metadata.clone(),
            revision: change.revision,
            text,
            deleted: change.deleted,
        });
        Ok(())
    }

    pub(crate) fn validate_catalog(
        &self,
        relations: &crate::relation_states::RelationStates,
    ) -> Result<(), KernelError> {
        for state in self.values() {
            if relations.contains_key(&state.metadata.id)
                || (!state.deleted && relations.get_named(state.metadata.name).is_some())
            {
                return Err(error(state.metadata.id, BufferError::MetadataMismatch));
            }
        }
        Ok(())
    }

    pub(crate) fn persisted(&self) -> Vec<PersistedBufferState> {
        self.values()
            .map(|state| PersistedBufferState {
                metadata: state.metadata.clone(),
                revision: state.revision,
                text: state.text.to_text(),
                deleted: state.deleted,
            })
            .collect()
    }
}

pub(crate) struct PendingBuffer {
    metadata: BufferMetadata,
    base: Option<Arc<BufferState>>,
    text: Text,
    deleted: bool,
}

pub(crate) type BufferWrites = BTreeMap<Identity, PendingBuffer>;

impl Snapshot {
    /// Includes tombstones, which keep identities reserved after deletion.
    pub fn buffer_catalog(&self) -> impl Iterator<Item = &BufferState> {
        self.buffers.values().map(Arc::as_ref)
    }

    pub fn buffer_named(&self, name: Symbol) -> Option<&BufferState> {
        self.buffers.named(name).map(Arc::as_ref)
    }

    pub fn buffer(&self, id: Identity) -> Result<&BufferState, KernelError> {
        self.buffers
            .get(id)
            .filter(|state| !state.deleted)
            .map(Arc::as_ref)
            .ok_or_else(|| error(id, BufferError::Unknown))
    }
}

impl Transaction<'_> {
    pub fn create_buffer(&mut self, metadata: BufferMetadata) -> Result<(), KernelError> {
        let id = metadata.id;
        if self.base.buffers.get(id).is_some()
            || self.buffer_writes.contains_key(&id)
            || self.base.relations.contains_key(&id)
            || self.base.relations.get_named(metadata.name).is_some()
            || self.buffer_named(metadata.name).is_some()
        {
            return Err(error(id, BufferError::AlreadyExists));
        }
        self.buffer_writes.insert(
            id,
            PendingBuffer {
                metadata,
                base: None,
                text: Text::default(),
                deleted: false,
            },
        );
        self.invalidate_computed_views();
        Ok(())
    }

    pub fn buffer_named(&self, name: Symbol) -> Option<Identity> {
        if let Some((id, _)) = self
            .buffer_writes
            .iter()
            .find(|(_, pending)| pending.metadata.name == name && !pending.deleted)
        {
            return Some(*id);
        }
        self.base
            .buffers
            .named(name)
            .filter(|state| {
                self.buffer_writes
                    .get(&state.metadata.id)
                    .is_none_or(|pending| !pending.deleted)
            })
            .map(|state| state.metadata.id)
    }

    pub fn buffer_metadata(&self, id: Identity) -> Result<&BufferMetadata, KernelError> {
        if let Some(pending) = self.buffer_writes.get(&id) {
            if pending.deleted {
                return Err(error(id, BufferError::Unknown));
            }
            return Ok(&pending.metadata);
        }
        self.base.buffer(id).map(|state| &state.metadata)
    }

    pub fn buffer_text(&self, id: Identity) -> Result<&Text, KernelError> {
        if let Some(pending) = self.buffer_writes.get(&id) {
            if pending.deleted {
                return Err(error(id, BufferError::Unknown));
            }
            return Ok(&pending.text);
        }
        self.base.buffer(id).map(|state| &state.text)
    }

    pub fn buffer_revision(&self, id: Identity) -> Result<u64, KernelError> {
        self.buffer_metadata(id)?;
        Ok(self.base.buffers.get(id).map_or(0, |state| state.revision))
    }

    pub fn replace_buffer(
        &mut self,
        id: Identity,
        range: Range<usize>,
        text: &str,
    ) -> Result<(), KernelError> {
        let previous = self.buffer_text(id)?;
        let next = previous
            .replace(range, text)
            .map_err(|failure| error(id, BufferError::Text(failure)))?;
        if previous.shares_root(&next) {
            return Ok(());
        }
        self.buffer_write(id)?.text = next;
        self.invalidate_computed_views();
        Ok(())
    }

    pub fn delete_buffer(&mut self, id: Identity) -> Result<(), KernelError> {
        let pending = self.buffer_write(id)?;
        pending.deleted = true;
        pending.text = Text::default();
        self.invalidate_computed_views();
        Ok(())
    }

    fn buffer_write(&mut self, id: Identity) -> Result<&mut PendingBuffer, KernelError> {
        self.buffer_metadata(id)?;
        if !self.buffer_writes.contains_key(&id) {
            let base = self.base.buffers.get(id).unwrap().clone();
            self.buffer_writes.insert(
                id,
                PendingBuffer {
                    metadata: base.metadata.clone(),
                    text: base.text.clone(),
                    base: Some(base),
                    deleted: false,
                },
            );
        }
        Ok(self.buffer_writes.get_mut(&id).unwrap())
    }

    pub(crate) fn materialize_buffers(
        &self,
        current: &Snapshot,
    ) -> Result<(BufferStates, Vec<BufferChange>), KernelError> {
        let mut states = current.buffers.clone();
        let mut changes = Vec::new();
        let mut rebase_budget = DeltaBudget::default();
        for (&id, pending) in self
            .buffer_writes
            .iter()
            .filter(|(_, pending)| pending.deleted)
            .chain(
                self.buffer_writes
                    .iter()
                    .filter(|(_, pending)| !pending.deleted),
            )
        {
            let (text, base_revision, delta) = match &pending.base {
                None => {
                    if states.get(id).is_some()
                        || states.named(pending.metadata.name).is_some()
                        || current.relations.contains_key(&id)
                        || current.relations.get_named(pending.metadata.name).is_some()
                    {
                        return Err(error(
                            id,
                            BufferError::Conflict {
                                expected: 0,
                                actual: states
                                    .get(id)
                                    .or_else(|| states.named(pending.metadata.name))
                                    .map_or(0, |state| state.revision),
                            },
                        ));
                    }
                    let delta = complete_delta(&Text::default(), &pending.text)
                        .map_err(|failure| error(id, BufferError::Delta(failure)))?;
                    (pending.text.clone(), 0, delta)
                }
                Some(base) => {
                    let latest = states
                        .get(id)
                        .ok_or_else(|| error(id, BufferError::Unknown))?;
                    if latest.deleted
                        || (latest.revision != base.revision
                            && (pending.deleted
                                || pending.metadata.conflict == BufferConflictPolicy::Reject))
                    {
                        return Err(error(
                            id,
                            BufferError::Conflict {
                                expected: base.revision,
                                actual: latest.revision,
                            },
                        ));
                    }
                    if latest.revision == base.revision {
                        let delta = complete_delta(&base.text, &pending.text)
                            .map_err(|failure| error(id, BufferError::Delta(failure)))?;
                        (pending.text.clone(), latest.revision, delta)
                    } else if pending.metadata.conflict == BufferConflictPolicy::Whole {
                        let delta = Delta::new(vec![super::Replacement {
                            range: 0..latest.text.len(),
                            text: pending.text.to_text(),
                        }])
                        .map_err(|failure| error(id, BufferError::Delta(failure)))?;
                        (pending.text.clone(), latest.revision, delta)
                    } else {
                        let own = Delta::between(&base.text, &pending.text, &mut rebase_budget)
                            .map_err(|failure| error(id, BufferError::Delta(failure)))?;
                        let concurrent =
                            Delta::between(&base.text, &latest.text, &mut rebase_budget)
                                .map_err(|failure| error(id, BufferError::Delta(failure)))?;
                        let delta =
                            own.transform_after(&concurrent)
                                .map_err(|failure| match failure {
                                    DeltaError::Conflict => error(
                                        id,
                                        BufferError::Conflict {
                                            expected: base.revision,
                                            actual: latest.revision,
                                        },
                                    ),
                                    failure => error(id, BufferError::Delta(failure)),
                                })?;
                        let text = delta
                            .apply(&latest.text)
                            .map_err(|failure| error(id, BufferError::Delta(failure)))?;
                        (text, latest.revision, delta)
                    }
                }
            };
            if pending.base.is_some() && delta.is_empty() && !pending.deleted {
                continue;
            }
            let revision = base_revision
                .checked_add(1)
                .ok_or_else(|| error(id, BufferError::RevisionExhausted))?;
            states.insert(BufferState {
                metadata: pending.metadata.clone(),
                revision,
                text,
                deleted: pending.deleted,
            });
            changes.push(BufferChange {
                metadata: pending.metadata.clone(),
                base_revision,
                revision,
                delta,
                deleted: pending.deleted,
            });
        }
        Ok((states, changes))
    }
}

fn complete_delta(base: &Text, view: &Text) -> Result<Delta, DeltaError> {
    Delta::between(
        base,
        view,
        &mut DeltaBudget {
            pieces: usize::MAX,
            text_bytes: usize::MAX,
            replacements: usize::MAX,
        },
    )
}

pub(crate) fn staged_changes(
    current: &Snapshot,
    staged: &Snapshot,
) -> Result<Vec<BufferChange>, KernelError> {
    let mut changes = Vec::new();
    for state in current.buffer_catalog() {
        if staged.buffers.get(state.metadata.id).is_none() {
            return Err(error(state.metadata.id, BufferError::MetadataMismatch));
        }
    }
    for state in staged.buffer_catalog() {
        let id = state.metadata.id;
        let previous = current.buffers.get(id);
        if previous.is_some_and(|previous| {
            previous.metadata != state.metadata || previous.revision > state.revision
        }) || staged.relations.contains_key(&id)
            || (!state.deleted && staged.relations.get_named(state.metadata.name).is_some())
        {
            return Err(error(id, BufferError::MetadataMismatch));
        }
        if previous.is_some_and(|previous| previous.revision == state.revision) {
            continue;
        }
        let delta = complete_delta(
            previous.map_or(&Text::default(), |previous| &previous.text),
            &state.text,
        )
        .map_err(|failure| error(id, BufferError::Delta(failure)))?;
        changes.push(BufferChange {
            metadata: state.metadata.clone(),
            base_revision: previous.map_or(0, |previous| previous.revision),
            revision: state.revision,
            delta,
            deleted: state.deleted,
        });
    }
    changes.sort_by_key(|change| (!change.deleted, change.metadata.id));
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Commit, CommitProvider, InMemoryCommitProvider, RelationKernel, RelationMetadata, Tuple,
    };
    use mica_var::Value;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn id(raw: u64) -> Identity {
        Identity::new(raw).unwrap()
    }
    fn metadata(raw: u64, name: &str, conflict: BufferConflictPolicy) -> BufferMetadata {
        BufferMetadata {
            id: id(raw),
            name: Symbol::intern(name),
            durability: RelationDurability::Durable,
            conflict,
        }
    }
    fn seed(kernel: &RelationKernel, conflict: BufferConflictPolicy) {
        let mut tx = kernel.begin();
        tx.create_buffer(metadata(10, "notes", conflict)).unwrap();
        tx.replace_buffer(id(10), 0..0, "héllo→").unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn buffer_creation_and_facts_publish_together_and_preserve_snapshots() {
        let provider = Arc::new(InMemoryCommitProvider::new());
        let kernel = RelationKernel::with_provider(provider.clone());
        kernel
            .create_relation(RelationMetadata::new(id(1), Symbol::intern("Ready"), 1))
            .unwrap();
        let before = kernel.snapshot();
        let mut tx = kernel.begin();
        tx.create_buffer(metadata(10, "notes", BufferConflictPolicy::Reject))
            .unwrap();
        tx.replace_buffer(id(10), 0..0, "hé").unwrap();
        tx.replace_buffer(id(10), 2..2, "llo→").unwrap();
        tx.assert(id(1), Tuple::from([Value::identity(id(10))]))
            .unwrap();
        assert_eq!(tx.buffer_text(id(10)).unwrap().to_text(), "héllo→");
        assert_eq!(tx.buffer_revision(id(10)).unwrap(), 0);
        assert!(before.buffer(id(10)).is_err());
        assert!(before.scan(id(1), &[None]).unwrap().is_empty());
        let result = tx.commit().unwrap();
        assert_eq!(result.commit().buffer_changes().len(), 1);
        assert_eq!(result.commit().changes().len(), 1);
        assert_eq!(
            result.snapshot().buffer(id(10)).unwrap().text().to_text(),
            "héllo→"
        );
        assert_eq!(result.snapshot().buffer(id(10)).unwrap().revision(), 1);
        assert_eq!(result.snapshot().scan(id(1), &[None]).unwrap().len(), 1);
        assert!(before.buffer(id(10)).is_err());
        let mut abandoned = kernel.begin();
        abandoned.replace_buffer(id(10), 0..1, "x").unwrap();
        drop(abandoned);
        assert_eq!(
            kernel.snapshot().buffer(id(10)).unwrap().text().to_text(),
            "héllo→"
        );
        let restored = RelationKernel::load_from_commit_log(
            provider.commits(),
            Arc::new(InMemoryCommitProvider::new()),
        )
        .unwrap();
        assert_eq!(
            restored.snapshot().buffer(id(10)).unwrap().text().to_text(),
            "héllo→"
        );
        assert_eq!(restored.snapshot().scan(id(1), &[None]).unwrap().len(), 1);
    }

    #[test]
    fn concurrent_buffer_policies_keep_their_declared_semantics() {
        for policy in [
            BufferConflictPolicy::Reject,
            BufferConflictPolicy::Span,
            BufferConflictPolicy::Whole,
        ] {
            let kernel = RelationKernel::new();
            seed(&kernel, policy);
            let before = kernel.snapshot();
            let mut left = kernel.begin();
            let mut right = kernel.begin();
            left.replace_buffer(id(10), 0..1, "H").unwrap();
            right.replace_buffer(id(10), 5..6, "!").unwrap();
            left.commit().unwrap();
            let result = right.commit();
            match policy {
                BufferConflictPolicy::Reject => assert!(matches!(
                    result,
                    Err(KernelError::Buffer {
                        error: BufferError::Conflict {
                            expected: 1,
                            actual: 2
                        },
                        ..
                    })
                )),
                BufferConflictPolicy::Span => assert_eq!(
                    result
                        .unwrap()
                        .snapshot()
                        .buffer(id(10))
                        .unwrap()
                        .text()
                        .to_text(),
                    "Héllo!"
                ),
                BufferConflictPolicy::Whole => assert_eq!(
                    result
                        .unwrap()
                        .snapshot()
                        .buffer(id(10))
                        .unwrap()
                        .text()
                        .to_text(),
                    "héllo!"
                ),
            }
            assert_eq!(before.buffer(id(10)).unwrap().text().to_text(), "héllo→");
        }
        let kernel = RelationKernel::new();
        seed(&kernel, BufferConflictPolicy::Span);
        let mut left = kernel.begin();
        let mut right = kernel.begin();
        left.replace_buffer(id(10), 2..2, "a").unwrap();
        right.replace_buffer(id(10), 2..2, "b").unwrap();
        left.commit().unwrap();
        assert!(matches!(
            right.commit(),
            Err(KernelError::Buffer {
                error: BufferError::Conflict { .. },
                ..
            })
        ));
    }

    struct FailingProvider {
        fail: AtomicBool,
    }

    #[test]
    fn concurrent_creation_and_deletion_report_retryable_conflicts() {
        let kernel = RelationKernel::new();
        let mut first = kernel.begin();
        let mut second = kernel.begin();
        first
            .create_buffer(metadata(10, "notes", BufferConflictPolicy::Span))
            .unwrap();
        second
            .create_buffer(metadata(11, "notes", BufferConflictPolicy::Span))
            .unwrap();
        first.commit().unwrap();
        assert!(matches!(
            second.commit(),
            Err(KernelError::Buffer {
                error: BufferError::Conflict {
                    expected: 0,
                    actual: 1
                },
                ..
            })
        ));
        let mut writer = kernel.begin();
        writer.replace_buffer(id(10), 0..0, "private").unwrap();
        let mut deleter = kernel.begin();
        deleter.delete_buffer(id(10)).unwrap();
        deleter.commit().unwrap();
        assert!(matches!(
            writer.commit(),
            Err(KernelError::Buffer {
                error: BufferError::Conflict {
                    expected: 1,
                    actual: 2
                },
                ..
            })
        ));
    }

    #[test]
    fn recovery_rejects_missing_creation_and_nonempty_tombstones() {
        let mut metadata = metadata(10, "notes", BufferConflictPolicy::Reject);
        metadata.durability = RelationDurability::Volatile;
        let change = BufferChange {
            metadata: metadata.clone(),
            base_revision: 3,
            revision: 4,
            delta: Delta::default(),
            deleted: false,
        };
        assert!(BufferStates::default().replay(&change).is_err());
        assert!(
            BufferStates::restore(vec![PersistedBufferState {
                metadata,
                revision: 4,
                text: "deleted text".to_owned(),
                deleted: true,
            }])
            .is_err()
        );
    }
    impl CommitProvider for FailingProvider {
        fn persist_commit(&self, _: &Commit) -> Result<(), String> {
            if self.fail.load(Ordering::Relaxed) {
                return Err("injected persistence failure".to_owned());
            }
            Ok(())
        }
    }

    #[test]
    fn failed_persistence_publishes_neither_text_nor_facts() {
        let provider = Arc::new(FailingProvider {
            fail: AtomicBool::new(false),
        });
        let kernel = RelationKernel::with_provider(provider.clone());
        kernel
            .create_relation(RelationMetadata::new(id(1), Symbol::intern("Ready"), 1))
            .unwrap();
        let before = kernel.snapshot();
        provider.fail.store(true, Ordering::Relaxed);
        let mut tx = kernel.begin();
        tx.create_buffer(metadata(10, "notes", BufferConflictPolicy::Reject))
            .unwrap();
        tx.replace_buffer(id(10), 0..0, "text").unwrap();
        tx.assert(id(1), Tuple::from([Value::identity(id(10))]))
            .unwrap();
        assert!(matches!(tx.commit(), Err(KernelError::Persistence(_))));
        assert_eq!(kernel.snapshot().version(), before.version());
        assert!(kernel.snapshot().buffer(id(10)).is_err());
        assert!(kernel.snapshot().scan(id(1), &[None]).unwrap().is_empty());
    }

    #[test]
    fn tombstones_reserve_ids_and_allow_name_reuse_in_the_same_commit() {
        let provider = Arc::new(InMemoryCommitProvider::new());
        let kernel = RelationKernel::with_provider(provider.clone());
        seed(&kernel, BufferConflictPolicy::Reject);
        let mut tx = kernel.begin();
        tx.delete_buffer(id(10)).unwrap();
        tx.create_buffer(metadata(2, "notes", BufferConflictPolicy::Span))
            .unwrap();
        tx.replace_buffer(id(2), 0..0, "replacement").unwrap();
        tx.commit().unwrap();
        let restored = RelationKernel::load_from_commit_log(
            provider.commits(),
            Arc::new(InMemoryCommitProvider::new()),
        )
        .unwrap();
        for snapshot in [kernel.snapshot(), restored.snapshot()] {
            assert_eq!(
                snapshot
                    .buffer_named(Symbol::intern("notes"))
                    .unwrap()
                    .metadata
                    .id,
                id(2)
            );
            assert!(snapshot.buffer(id(10)).is_err());
            assert_eq!(snapshot.buffer_catalog().count(), 2);
        }
        assert!(
            kernel
                .begin()
                .create_buffer(metadata(10, "other", BufferConflictPolicy::Reject))
                .is_err()
        );
    }

    #[test]
    fn staged_kernel_publication_includes_buffers_and_rejects_stale_forks() {
        let provider = Arc::new(InMemoryCommitProvider::new());
        let kernel = RelationKernel::with_provider(provider.clone());
        let base_version = kernel.snapshot().version();
        let staged = kernel.fork_in_memory();
        seed(&staged, BufferConflictPolicy::Reject);
        assert!(kernel.snapshot().buffer(id(10)).is_err());
        kernel
            .commit_staged_snapshot(base_version, staged.snapshot())
            .unwrap();
        assert_eq!(
            kernel.snapshot().buffer(id(10)).unwrap().text().to_text(),
            "héllo→"
        );
        assert!(matches!(
            kernel.commit_staged_snapshot(base_version, staged.snapshot()),
            Err(KernelError::StaleStagedSnapshot { .. })
        ));
        assert_eq!(provider.commits().last().unwrap().buffer_changes().len(), 1);

        let base_version = kernel.snapshot().version();
        let staged = kernel.fork_in_memory();
        let mut tx = staged.begin();
        tx.delete_buffer(id(10)).unwrap();
        tx.create_buffer(metadata(2, "notes", BufferConflictPolicy::Span))
            .unwrap();
        tx.replace_buffer(id(2), 0..0, "replacement").unwrap();
        tx.commit().unwrap();
        kernel
            .commit_staged_snapshot(base_version, staged.snapshot())
            .unwrap();
        let restored = RelationKernel::load_from_commit_log(
            provider.commits(),
            Arc::new(InMemoryCommitProvider::new()),
        )
        .unwrap();
        assert_eq!(
            restored.snapshot().buffer(id(2)).unwrap().text().to_text(),
            "replacement"
        );
        assert!(restored.snapshot().buffer(id(10)).is_err());
    }
}
