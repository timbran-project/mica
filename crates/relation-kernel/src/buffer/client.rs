// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::store::error;
use super::{BufferError, Delta, DeltaBudget, DeltaError, Replacement, Text};
use crate::{KernelError, RelationKernel, Snapshot, Transaction};
use mica_var::Identity;
use std::collections::{HashMap, VecDeque};
use std::num::NonZeroU64;
use std::sync::Arc;

const MAX_RESULTS: usize = 1024;
const MAX_RESULT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferApplyStatus {
    Staged,
    Stale,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BufferApplyOutcome {
    Pending,
    Ok { applied: Delta },
    Resync,
    Conflict,
    Aborted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BufferApplyResult {
    pub token: NonZeroU64,
    pub buffer: Identity,
    pub revision: u64,
    pub outcome: BufferApplyOutcome,
}

impl BufferApplyResult {
    fn retained_bytes(&self) -> usize {
        match &self.outcome {
            BufferApplyOutcome::Ok { applied } => applied.retained_bytes(),
            _ => 0,
        }
    }
}

#[derive(Default)]
pub(crate) struct ClientResults {
    entries: HashMap<NonZeroU64, Arc<BufferApplyResult>>,
    completed: VecDeque<NonZeroU64>,
    bytes: usize,
}

impl ClientResults {
    fn evict_completed(&mut self) -> bool {
        let Some(token) = self.completed.pop_front() else {
            return false;
        };
        if let Some(result) = self.entries.remove(&token) {
            self.bytes -= result.retained_bytes();
        }
        true
    }

    fn reserve(
        &mut self,
        token: NonZeroU64,
        buffer: Identity,
        revision: u64,
    ) -> Result<(), KernelError> {
        if self.entries.contains_key(&token) {
            return Err(error(buffer, BufferError::TokenInUse));
        }
        while self.entries.len() >= MAX_RESULTS {
            if !self.evict_completed() {
                return Err(error(buffer, BufferError::ResultsFull));
            }
        }
        self.entries.insert(
            token,
            Arc::new(BufferApplyResult {
                token,
                buffer,
                revision,
                outcome: BufferApplyOutcome::Pending,
            }),
        );
        Ok(())
    }

    fn finish(&mut self, mut result: BufferApplyResult) {
        if result.retained_bytes() > MAX_RESULT_BYTES {
            result.outcome = BufferApplyOutcome::Resync;
        }
        let bytes = result.retained_bytes();
        while self.bytes + bytes > MAX_RESULT_BYTES && self.evict_completed() {}
        self.bytes += bytes;
        self.completed.push_back(result.token);
        self.entries.insert(result.token, Arc::new(result));
    }
}

impl RelationKernel {
    pub fn buffer_apply_result(&self, token: NonZeroU64) -> Option<Arc<BufferApplyResult>> {
        self.buffer_results
            .lock()
            .unwrap()
            .entries
            .get(&token)
            .cloned()
    }
}

impl Transaction<'_> {
    /// Stages sequential, view-relative edits only when the base revision matches.
    /// All ranges are validated on a private root before the transaction changes.
    pub fn apply_buffer(
        &mut self,
        id: Identity,
        expected_revision: u64,
        edits: &[Replacement],
        token: Option<NonZeroU64>,
    ) -> Result<BufferApplyStatus, KernelError> {
        if self.buffer_revision(id)? != expected_revision {
            return Ok(BufferApplyStatus::Stale);
        }
        if self
            .buffer_writes
            .get(&id)
            .is_some_and(|pending| pending.touched || pending.sealed)
        {
            return Err(error(id, BufferError::AlreadyApplied));
        }
        let mut text = self.buffer_text(id)?.clone();
        for edit in edits {
            text = text
                .replace(edit.range.clone(), &edit.text)
                .map_err(|failure| error(id, BufferError::Text(failure)))?;
        }
        if let Some(token) = token {
            self.kernel()
                .buffer_results
                .lock()
                .unwrap()
                .reserve(token, id, expected_revision)?;
        }
        let pending = self.buffer_write(id)?;
        pending.text = text;
        pending.sealed = true;
        pending.token = token;
        self.invalidate_computed_views();
        Ok(BufferApplyStatus::Staged)
    }

    pub fn has_tagged_buffer_apply(&self) -> bool {
        self.buffer_writes
            .values()
            .any(|pending| pending.token.is_some())
    }

    pub(crate) fn complete_buffer_applies(&mut self, snapshot: &Snapshot) {
        if !self.has_tagged_buffer_apply() {
            return;
        }
        let kernel = self.kernel();
        let mut budget = DeltaBudget::default();
        for (&id, pending) in &mut self.buffer_writes {
            let Some(token) = pending.token.take() else {
                continue;
            };
            let state = snapshot
                .buffer(id)
                .expect("a sealed buffer publishes live state");
            let base = pending
                .base
                .as_ref()
                .map_or(&Text::default(), |base| &base.text)
                .clone();
            let outcome = match Delta::between(&base, state.text(), &mut budget) {
                Ok(applied) => BufferApplyOutcome::Ok { applied },
                Err(_) => BufferApplyOutcome::Resync,
            };
            kernel
                .buffer_results
                .lock()
                .unwrap()
                .finish(BufferApplyResult {
                    token,
                    buffer: id,
                    revision: state.revision(),
                    outcome,
                });
        }
    }

    pub(crate) fn fail_buffer_applies(&mut self, failure: &KernelError, current: &Snapshot) {
        let outcome = match failure {
            KernelError::Conflict(_)
            | KernelError::Buffer {
                error: BufferError::Conflict { .. },
                ..
            } => BufferApplyOutcome::Conflict,
            KernelError::Buffer {
                error:
                    BufferError::Delta(DeltaError::BudgetExceeded | DeltaError::ForeignProvenance),
                ..
            } => BufferApplyOutcome::Resync,
            _ => BufferApplyOutcome::Aborted,
        };
        let kernel = self.kernel();
        for (&id, pending) in &mut self.buffer_writes {
            let Some(token) = pending.token.take() else {
                continue;
            };
            kernel
                .buffer_results
                .lock()
                .unwrap()
                .finish(BufferApplyResult {
                    token,
                    buffer: id,
                    revision: current.buffers.get(id).map_or(0, |state| state.revision),
                    outcome: outcome.clone(),
                });
        }
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.has_tagged_buffer_apply() {
            return;
        }
        let kernel = self.kernel();
        let mut results = kernel.buffer_results.lock().unwrap();
        for (&id, pending) in &mut self.buffer_writes {
            let Some(token) = pending.token.take() else {
                continue;
            };
            results.finish(BufferApplyResult {
                token,
                buffer: id,
                revision: pending.base.as_ref().map_or(0, |base| base.revision),
                outcome: BufferApplyOutcome::Aborted,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{BufferConflictPolicy, BufferMetadata};
    use crate::{Commit, CommitProvider, RelationDurability};
    use mica_var::Symbol;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn id() -> Identity {
        Identity::new(10).unwrap()
    }
    fn token(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).unwrap()
    }
    fn edit(start: usize, end: usize, text: &str) -> Replacement {
        Replacement {
            range: start..end,
            text: text.to_owned(),
        }
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
        tx.replace_buffer(id(), 0..0, "héllo→").unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn stale_and_invalid_batches_leave_the_transaction_untouched() {
        let kernel = RelationKernel::new();
        seed(&kernel, BufferConflictPolicy::Span);
        let mut tx = kernel.begin();
        assert_eq!(
            tx.apply_buffer(id(), 0, &[edit(usize::MAX, 0, "bad")], Some(token(1)))
                .unwrap(),
            BufferApplyStatus::Stale
        );
        assert!(tx.is_read_only());
        assert!(kernel.buffer_apply_result(token(1)).is_none());
        assert!(
            tx.apply_buffer(
                id(),
                1,
                &[edit(0, 1, "valid"), edit(999, 999, "invalid")],
                Some(token(1))
            )
            .is_err()
        );
        assert!(tx.is_read_only());
        assert_eq!(tx.buffer_text(id()).unwrap().to_text(), "héllo→");
        assert!(kernel.buffer_apply_result(token(1)).is_none());
        assert_eq!(
            tx.apply_buffer(
                id(),
                1,
                &[edit(0, 1, "abc"), edit(2, 3, "X")],
                Some(token(1))
            )
            .unwrap(),
            BufferApplyStatus::Staged
        );
        assert_eq!(tx.buffer_text(id()).unwrap().to_text(), "abXéllo→");
        assert!(matches!(
            kernel.buffer_apply_result(token(1)).unwrap().outcome,
            BufferApplyOutcome::Pending
        ));
        assert!(matches!(
            tx.replace_buffer(id(), 0..0, ""),
            Err(KernelError::Buffer {
                error: BufferError::AlreadyApplied,
                ..
            })
        ));
        assert!(tx.delete_buffer(id()).is_err());
        assert!(tx.apply_buffer(id(), 1, &[], None).is_err());
        drop(tx);
        assert!(matches!(
            kernel.buffer_apply_result(token(1)).unwrap().outcome,
            BufferApplyOutcome::Aborted
        ));
        assert_eq!(
            kernel.snapshot().buffer(id()).unwrap().text().to_text(),
            "héllo→"
        );
    }

    #[test]
    fn successful_apply_reports_the_complete_change_in_client_coordinates() {
        let kernel = RelationKernel::new();
        seed(&kernel, BufferConflictPolicy::Span);
        let base = kernel.snapshot().buffer(id()).unwrap().text().clone();
        let mut client = kernel.begin();
        client
            .apply_buffer(id(), 1, &[edit(5, 6, "!")], Some(token(1)))
            .unwrap();
        let mut concurrent = kernel.begin();
        concurrent.replace_buffer(id(), 0..0, "prefix ").unwrap();
        concurrent.commit().unwrap();
        let committed = client
            .commit_with_post_publish(|result| {
                assert!(matches!(
                    kernel.buffer_apply_result(token(1)).unwrap().outcome,
                    BufferApplyOutcome::Ok { .. }
                ));
                assert_eq!(result.snapshot().buffer(id()).unwrap().revision(), 3);
            })
            .unwrap();
        let result = kernel.buffer_apply_result(token(1)).unwrap();
        let BufferApplyOutcome::Ok { applied } = &result.outcome else {
            panic!("{result:?}");
        };
        assert_eq!(result.revision, 3);
        assert_eq!(applied.apply(&base).unwrap().to_text(), "prefix héllo!");
        assert_eq!(
            applied.apply(&base).unwrap().to_text(),
            committed.snapshot().buffer(id()).unwrap().text().to_text()
        );
        let mut duplicate = kernel.begin();
        assert!(matches!(
            duplicate.apply_buffer(id(), 3, &[], Some(token(1))),
            Err(KernelError::Buffer {
                error: BufferError::TokenInUse,
                ..
            })
        ));
        assert!(duplicate.is_read_only());
    }

    #[test]
    fn conflicts_and_budget_exhaustion_have_distinct_client_results() {
        let kernel = RelationKernel::new();
        seed(&kernel, BufferConflictPolicy::Span);
        let mut client = kernel.begin();
        client
            .apply_buffer(id(), 1, &[edit(0, 1, "client")], Some(token(1)))
            .unwrap();
        let mut concurrent = kernel.begin();
        concurrent.replace_buffer(id(), 0..1, "winner").unwrap();
        concurrent.commit().unwrap();
        assert!(client.commit().is_err());
        assert!(matches!(
            kernel.buffer_apply_result(token(1)).unwrap().outcome,
            BufferApplyOutcome::Conflict
        ));
        assert_eq!(kernel.buffer_apply_result(token(1)).unwrap().revision, 2);
        let mut client = kernel.begin();
        client
            .apply_buffer(
                id(),
                2,
                &[edit(0, 0, &"x".repeat(1024 * 1024 + 1))],
                Some(token(2)),
            )
            .unwrap();
        client.commit().unwrap();
        assert!(matches!(
            kernel.buffer_apply_result(token(2)).unwrap().outcome,
            BufferApplyOutcome::Resync
        ));
        assert_eq!(kernel.snapshot().buffer(id()).unwrap().revision(), 3);
    }

    struct FailingProvider(AtomicBool);
    impl CommitProvider for FailingProvider {
        fn persist_commit(&self, _: &Commit) -> Result<(), String> {
            if self.0.load(Ordering::Relaxed) {
                Err("injected write failure".to_owned())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn persistence_failure_records_abort_without_publishing_text() {
        let provider = Arc::new(FailingProvider(AtomicBool::new(false)));
        let kernel = RelationKernel::with_provider(provider.clone());
        seed(&kernel, BufferConflictPolicy::Reject);
        let mut tx = kernel.begin();
        tx.apply_buffer(id(), 1, &[edit(0, 0, "private")], Some(token(1)))
            .unwrap();
        provider.0.store(true, Ordering::Relaxed);
        assert!(matches!(tx.commit(), Err(KernelError::Persistence(_))));
        assert!(matches!(
            kernel.buffer_apply_result(token(1)).unwrap().outcome,
            BufferApplyOutcome::Aborted
        ));
        assert_eq!(kernel.snapshot().buffer(id()).unwrap().revision(), 1);
    }

    #[test]
    fn new_and_unchanged_buffers_settle_tagged_applies() {
        let kernel = RelationKernel::new();
        let mut tx = kernel.begin();
        tx.create_buffer(BufferMetadata {
            id: id(),
            name: Symbol::intern("notes"),
            durability: RelationDurability::Durable,
            conflict: BufferConflictPolicy::Span,
        })
        .unwrap();
        tx.apply_buffer(id(), 0, &[edit(0, 0, "created")], Some(token(1)))
            .unwrap();
        tx.commit().unwrap();
        assert_eq!(kernel.buffer_apply_result(token(1)).unwrap().revision, 1);
        let mut tx = kernel.begin();
        tx.apply_buffer(id(), 1, &[], Some(token(2))).unwrap();
        assert!(!tx.is_read_only());
        tx.commit().unwrap();
        let result = kernel.buffer_apply_result(token(2)).unwrap();
        assert_eq!(result.revision, 1);
        assert!(
            matches!(&result.outcome, BufferApplyOutcome::Ok { applied } if applied.is_empty())
        );
    }

    #[test]
    fn result_retention_bounds_completed_entries_without_evicting_pending_applies() {
        let mut results = ClientResults::default();
        results.reserve(token(1), id(), 1).unwrap();
        for value in 2..=(MAX_RESULTS as u64 + 10) {
            let token = token(value);
            results.reserve(token, id(), 1).unwrap();
            results.finish(BufferApplyResult {
                token,
                buffer: id(),
                revision: 2,
                outcome: BufferApplyOutcome::Aborted,
            });
        }
        assert_eq!(results.entries.len(), MAX_RESULTS);
        assert!(results.entries.contains_key(&token(1)));
        assert!(!results.entries.contains_key(&token(2)));
        for value in 2000..2020 {
            let token = token(value);
            results.reserve(token, id(), 1).unwrap();
            let applied = Delta::new(vec![edit(0, 0, &"x".repeat(1024 * 1024))]).unwrap();
            results.finish(BufferApplyResult {
                token,
                buffer: id(),
                revision: 2,
                outcome: BufferApplyOutcome::Ok { applied },
            });
            assert!(results.bytes <= MAX_RESULT_BYTES);
        }
        assert!(results.entries.contains_key(&token(1)));
        let mut pending = ClientResults::default();
        for value in 1..=MAX_RESULTS as u64 {
            pending.reserve(token(value), id(), 1).unwrap();
        }
        assert!(matches!(
            pending.reserve(token(MAX_RESULTS as u64 + 1), id(), 1),
            Err(KernelError::Buffer {
                error: BufferError::ResultsFull,
                ..
            })
        ));
    }
}
