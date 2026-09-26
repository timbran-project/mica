// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::store::error;
use super::{BufferError, BufferState, Text};
use crate::{KernelError, RelationKernel, Snapshot, Transaction};
use mica_var::Identity;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

const HISTORY_DEPTH: usize = 32;
const HISTORY_BUFFERS: usize = 256;

#[derive(Default)]
struct Ring {
    versions: VecDeque<Arc<BufferState>>,
    touched: u64,
}

#[derive(Default)]
pub(crate) struct BufferHistory {
    rings: HashMap<Identity, Ring>,
    clock: u64,
}

impl BufferHistory {
    fn record(&mut self, state: Arc<BufferState>) {
        let id = state.metadata.id;
        if state.deleted {
            self.rings.remove(&id);
            return;
        }
        if !self.rings.contains_key(&id) && self.rings.len() >= HISTORY_BUFFERS {
            let oldest = self
                .rings
                .iter()
                .min_by_key(|(_, ring)| ring.touched)
                .map(|(id, _)| *id)
                .unwrap();
            self.rings.remove(&oldest);
        }
        self.clock = self.clock.saturating_add(1);
        let ring = self.rings.entry(id).or_default();
        ring.touched = self.clock;
        if ring
            .versions
            .back()
            .is_some_and(|last| last.revision == state.revision)
        {
            ring.versions.pop_back();
        }
        ring.versions.push_back(state);
        if ring.versions.len() > HISTORY_DEPTH {
            ring.versions.pop_front();
        }
    }

    fn lookup(&self, id: Identity, revision: u64) -> Option<Arc<BufferState>> {
        self.rings
            .get(&id)?
            .versions
            .iter()
            .find(|state| state.revision == revision)
            .cloned()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferRevertStatus {
    Staged,
    Stale,
    Unknown,
}

impl RelationKernel {
    pub(crate) fn try_publish_with_buffer_history(
        &self,
        before: &Snapshot,
        after: Arc<Snapshot>,
        ids: impl IntoIterator<Item = Identity>,
    ) -> bool {
        let mut ids = ids.into_iter().peekable();
        if ids.peek().is_none() {
            return self.try_publish(before.version(), after);
        }
        // A reader of the published revision must also see its retained predecessor.
        let mut history = self.buffer_history.lock().unwrap();
        if !self.try_publish(before.version(), after.clone()) {
            return false;
        }
        for id in ids {
            if before
                .buffers
                .get(id)
                .zip(after.buffers.get(id))
                .is_some_and(|(before, after)| Arc::ptr_eq(before, after))
            {
                continue;
            }
            if let Some(previous) = before.buffers.get(id) {
                history.record(previous.clone());
            }
            if let Some(current) = after.buffers.get(id) {
                history.record(current.clone());
            }
        }
        true
    }
}

impl Transaction<'_> {
    pub fn compact_buffer(&mut self, id: Identity) -> Result<(), KernelError> {
        self.buffer_metadata(id)?;
        if self.base.buffers.get(id).is_none() {
            return Ok(());
        }
        if self
            .buffer_writes
            .get(&id)
            .is_some_and(|pending| pending.touched || pending.sealed)
        {
            return Err(error(id, BufferError::AlreadyApplied));
        }
        let text = Text::from_text(&self.buffer_text(id)?.to_text());
        let pending = self.buffer_write(id)?;
        pending.text = text;
        pending.compact = true;
        pending.sealed = true;
        self.invalidate_computed_views();
        Ok(())
    }

    pub fn revert_buffer(
        &mut self,
        id: Identity,
        revision: u64,
        expected_revision: u64,
    ) -> Result<BufferRevertStatus, KernelError> {
        let current = self.buffer_revision(id)?;
        if expected_revision != current {
            return Ok(BufferRevertStatus::Stale);
        }
        if revision > current {
            return Ok(BufferRevertStatus::Unknown);
        }
        if revision == current {
            return Ok(BufferRevertStatus::Staged);
        }
        if self
            .buffer_writes
            .get(&id)
            .is_some_and(|pending| pending.touched || pending.sealed)
        {
            return Err(error(id, BufferError::AlreadyApplied));
        }
        let historical = self
            .kernel()
            .buffer_history
            .lock()
            .unwrap()
            .lookup(id, revision);
        let Some(historical) = historical else {
            return Ok(BufferRevertStatus::Unknown);
        };
        let text = Text::from_text(&historical.text.to_text());
        let pending = self.buffer_write(id)?;
        pending.text = text;
        pending.reverted = true;
        pending.sealed = true;
        self.invalidate_computed_views();
        Ok(BufferRevertStatus::Staged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{BufferApplyOutcome, BufferConflictPolicy, BufferMetadata, Replacement};
    use crate::{InMemoryCommitProvider, RelationDurability};
    use mica_var::Symbol;
    use std::num::NonZeroU64;

    fn id() -> Identity {
        Identity::new(10).unwrap()
    }
    fn seed(kernel: &RelationKernel, policy: BufferConflictPolicy) {
        let mut tx = kernel.begin();
        tx.create_buffer(BufferMetadata {
            id: id(),
            name: Symbol::intern("notes"),
            durability: RelationDurability::Durable,
            conflict: policy,
        })
        .unwrap();
        tx.replace_buffer(id(), 0..0, "one").unwrap();
        tx.commit().unwrap();
    }
    fn replace(kernel: &RelationKernel, text: &str) {
        let mut tx = kernel.begin();
        let length = tx.buffer_text(id()).unwrap().len();
        tx.replace_buffer(id(), 0..length, text).unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn reversion_splices_retained_text_and_preserves_monotonic_revisions() {
        let provider = Arc::new(InMemoryCommitProvider::new());
        let kernel = RelationKernel::with_provider(provider.clone());
        seed(&kernel, BufferConflictPolicy::Span);
        replace(&kernel, "two");
        replace(&kernel, "three");
        let mut tx = kernel.begin();
        assert_eq!(
            tx.revert_buffer(id(), 1, 2).unwrap(),
            BufferRevertStatus::Stale
        );
        assert_eq!(
            tx.revert_buffer(id(), 99, 3).unwrap(),
            BufferRevertStatus::Unknown
        );
        assert!(tx.is_read_only());
        assert_eq!(
            tx.revert_buffer(id(), 1, 3).unwrap(),
            BufferRevertStatus::Staged
        );
        assert_eq!(tx.buffer_text(id()).unwrap().to_text(), "one");
        assert_eq!(tx.buffer_revision(id()).unwrap(), 3);
        assert!(tx.replace_buffer(id(), 0..0, "later").is_err());
        let committed = tx.commit().unwrap();
        assert_eq!(committed.snapshot().buffer(id()).unwrap().revision(), 4);
        assert_eq!(
            committed.commit().buffer_changes()[0].delta.replacements(),
            &[Replacement {
                range: 0..5,
                text: "one".to_owned()
            }]
        );
        let mut tx = kernel.begin();
        assert_eq!(
            tx.revert_buffer(id(), 4, 4).unwrap(),
            BufferRevertStatus::Staged
        );
        assert!(tx.is_read_only());
        let restored = RelationKernel::load_from_commit_log(
            provider.commits(),
            Arc::new(InMemoryCommitProvider::new()),
        )
        .unwrap();
        assert_eq!(
            restored.snapshot().buffer(id()).unwrap().text().to_text(),
            "one"
        );
        assert_eq!(restored.snapshot().buffer(id()).unwrap().revision(), 4);
    }

    #[test]
    fn compaction_preserves_content_revision_and_rejects_stale_structure_for_every_policy() {
        for policy in [
            BufferConflictPolicy::Reject,
            BufferConflictPolicy::Span,
            BufferConflictPolicy::Whole,
        ] {
            let kernel = RelationKernel::new();
            seed(&kernel, policy);
            let old = kernel.snapshot();
            let mut stale = kernel.begin();
            stale.replace_buffer(id(), 0..0, "stale ").unwrap();
            let mut compact = kernel.begin();
            compact.compact_buffer(id()).unwrap();
            assert!(compact.replace_buffer(id(), 0..0, "later").is_err());
            assert!(compact.compact_buffer(id()).is_err());
            let result = compact.commit().unwrap();
            let state = result.snapshot().buffer(id()).unwrap();
            assert_eq!(state.revision(), 1);
            assert_eq!(state.text().to_text(), "one");
            assert!(!state.text().shares_root(old.buffer(id()).unwrap().text()));
            assert!(result.commit().buffer_changes().is_empty());
            assert!(matches!(
                stale.commit(),
                Err(KernelError::Buffer {
                    error: BufferError::StructureConflict { .. },
                    ..
                })
            ));
            let retained = kernel
                .buffer_history
                .lock()
                .unwrap()
                .lookup(id(), 1)
                .unwrap();
            assert!(retained.text().shares_root(state.text()));
            assert_eq!(old.buffer(id()).unwrap().text().to_text(), "one");
        }
    }

    #[test]
    fn reversion_and_compaction_refuse_concurrent_edits_and_dirty_views() {
        let kernel = RelationKernel::new();
        seed(&kernel, BufferConflictPolicy::Whole);
        replace(&kernel, "two");
        let mut reverted = kernel.begin();
        reverted.revert_buffer(id(), 1, 2).unwrap();
        let mut compacted = kernel.begin();
        compacted.compact_buffer(id()).unwrap();
        replace(&kernel, "winner");
        for tx in [reverted, compacted] {
            assert!(matches!(
                tx.commit(),
                Err(KernelError::Buffer {
                    error: BufferError::Conflict { .. },
                    ..
                })
            ));
        }
        let mut dirty = kernel.begin();
        dirty.replace_buffer(id(), 0..0, "private ").unwrap();
        assert!(dirty.compact_buffer(id()).is_err());
        assert!(dirty.revert_buffer(id(), 1, 3).is_err());
        assert_eq!(dirty.buffer_text(id()).unwrap().to_text(), "private winner");
        let mut client = kernel.begin();
        let token = NonZeroU64::new(1).unwrap();
        client
            .apply_buffer(
                id(),
                3,
                &[Replacement {
                    range: 0..0,
                    text: "client ".to_owned(),
                }],
                Some(token),
            )
            .unwrap();
        let mut compact = kernel.begin();
        compact.compact_buffer(id()).unwrap();
        compact.commit().unwrap();
        assert!(client.commit().is_err());
        assert!(matches!(
            kernel.buffer_apply_result(token).unwrap().outcome,
            BufferApplyOutcome::Resync
        ));
    }

    #[test]
    fn revision_history_has_depth_and_buffer_limits_and_releases_retired_buffers() {
        let kernel = RelationKernel::new();
        seed(&kernel, BufferConflictPolicy::Reject);
        for revision in 2..=40 {
            replace(&kernel, &revision.to_string());
        }
        let history = kernel.buffer_history.lock().unwrap();
        assert_eq!(history.rings[&id()].versions.len(), HISTORY_DEPTH);
        assert!(history.lookup(id(), 1).is_none());
        assert!(history.lookup(id(), 9).is_some());
        drop(history);
        assert_eq!(
            kernel.begin().revert_buffer(id(), 1, 40).unwrap(),
            BufferRevertStatus::Unknown
        );
        for raw in 100..(100 + HISTORY_BUFFERS as u64) {
            let mut tx = kernel.begin();
            tx.create_buffer(BufferMetadata {
                id: Identity::new(raw).unwrap(),
                name: Symbol::intern(&format!("buffer_{raw}")),
                durability: RelationDurability::Volatile,
                conflict: BufferConflictPolicy::Reject,
            })
            .unwrap();
            tx.commit().unwrap();
        }
        let history = kernel.buffer_history.lock().unwrap();
        assert_eq!(history.rings.len(), HISTORY_BUFFERS);
        assert!(!history.rings.contains_key(&id()));
        drop(history);
        replace(&kernel, "current");
        assert!(
            kernel
                .buffer_history
                .lock()
                .unwrap()
                .rings
                .contains_key(&id())
        );
        let mut tx = kernel.begin();
        tx.delete_buffer(id()).unwrap();
        tx.commit().unwrap();
        assert!(
            !kernel
                .buffer_history
                .lock()
                .unwrap()
                .rings
                .contains_key(&id())
        );
    }

    #[test]
    fn compaction_of_a_new_buffer_keeps_its_view_open_and_staged_publication_records_history() {
        let kernel = RelationKernel::new();
        let version = kernel.snapshot().version();
        let staged = kernel.fork_in_memory();
        let mut tx = staged.begin();
        tx.create_buffer(BufferMetadata {
            id: id(),
            name: Symbol::intern("notes"),
            durability: RelationDurability::Durable,
            conflict: BufferConflictPolicy::Span,
        })
        .unwrap();
        tx.replace_buffer(id(), 0..0, "first").unwrap();
        tx.compact_buffer(id()).unwrap();
        tx.replace_buffer(id(), 5..5, "!").unwrap();
        tx.commit().unwrap();
        kernel
            .commit_staged_snapshot(version, staged.snapshot())
            .unwrap();
        let version = kernel.snapshot().version();
        let staged = kernel.fork_in_memory();
        replace(&staged, "second");
        kernel
            .commit_staged_snapshot(version, staged.snapshot())
            .unwrap();
        let mut tx = kernel.begin();
        assert_eq!(
            tx.revert_buffer(id(), 1, 2).unwrap(),
            BufferRevertStatus::Staged
        );
        assert_eq!(tx.buffer_text(id()).unwrap().to_text(), "first!");
    }
}
