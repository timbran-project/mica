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

use crate::metrics::{self, ParallelAdmissionOutcome};
use mica_runtime::ExecutionAdmission;
use std::collections::VecDeque;
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::{Arc, Mutex, TryLockError};
use std::task::{Context, Poll, Waker};
use std::time::Instant;

const TASK_ADMISSION_HEADROOM: usize = 1;

#[repr(align(128))]
pub(crate) struct CpuAdmission {
    dispatch_capacity: NonZeroUsize,
    relation_parallelism: NonZeroUsize,
    state: Mutex<AdmissionState>,
}

#[derive(Default)]
struct AdmissionState {
    occupied: usize,
    parallel_occupied: usize,
    next_waiter_id: u64,
    task_waiters: VecDeque<TaskWaiter>,
}

struct TaskWaiter {
    id: u64,
    waker: Waker,
}

impl CpuAdmission {
    pub(crate) fn new(dispatch_capacity: NonZeroUsize, relation_parallelism: NonZeroUsize) -> Self {
        Self {
            dispatch_capacity,
            relation_parallelism,
            state: Mutex::new(AdmissionState::default()),
        }
    }

    pub(crate) fn acquire_dispatch(self: &Arc<Self>) -> DispatchAdmission {
        DispatchAdmission {
            admission: Arc::clone(self),
            waiter_id: None,
            wait_started: None,
            completed: false,
        }
    }

    fn release_workers(&self, workers: NonZeroUsize) {
        let wake = {
            let mut state = self.state.lock().unwrap();
            debug_assert!(state.occupied >= workers.get());
            state.occupied -= workers.get();
            next_task_waker(&state, self.dispatch_capacity)
        };
        if let Some(waker) = wake {
            waker.wake();
        }
    }

    fn release_parallel_workers(&self, workers: NonZeroUsize) {
        let wake = {
            let mut state = self.state.lock().unwrap();
            debug_assert!(state.occupied >= workers.get());
            debug_assert!(state.parallel_occupied >= workers.get());
            state.occupied -= workers.get();
            state.parallel_occupied -= workers.get();
            next_task_waker(&state, self.dispatch_capacity)
        };
        if let Some(waker) = wake {
            waker.wake();
        }
    }

    #[cfg(test)]
    fn state(&self) -> (usize, usize) {
        let state = self.state.lock().unwrap();
        (state.occupied, state.task_waiters.len())
    }
}

impl ExecutionAdmission for CpuAdmission {
    fn capacity(&self) -> NonZeroUsize {
        self.relation_parallelism
    }

    fn try_reserve_parallel(&self, additional_workers: NonZeroUsize) -> bool {
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::WouldBlock) => {
                metrics::record_parallel_admission(ParallelAdmissionOutcome::Contended);
                return false;
            }
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        let relation_capacity_exhausted = state
            .parallel_occupied
            .checked_add(additional_workers.get())
            .is_none_or(|occupied| occupied >= self.relation_parallelism.get());
        let dispatch_capacity_exhausted = state
            .occupied
            .checked_add(additional_workers.get())
            .and_then(|occupied| occupied.checked_add(TASK_ADMISSION_HEADROOM))
            .is_none_or(|occupied| occupied > self.dispatch_capacity.get());
        let outcome = if !state.task_waiters.is_empty() {
            ParallelAdmissionOutcome::TaskWaiting
        } else if relation_capacity_exhausted || dispatch_capacity_exhausted {
            ParallelAdmissionOutcome::Capacity
        } else {
            state.occupied += additional_workers.get();
            state.parallel_occupied += additional_workers.get();
            ParallelAdmissionOutcome::Admitted
        };
        drop(state);
        metrics::record_parallel_admission(outcome);
        outcome == ParallelAdmissionOutcome::Admitted
    }

    fn release_parallel(&self, additional_workers: NonZeroUsize) {
        self.release_parallel_workers(additional_workers);
    }
}

pub(crate) struct DispatchAdmission {
    admission: Arc<CpuAdmission>,
    waiter_id: Option<u64>,
    wait_started: Option<Instant>,
    completed: bool,
}

impl Future for DispatchAdmission {
    type Output = DispatchPermit;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let admission_future = self.get_mut();
        assert!(
            !admission_future.completed,
            "completed admission polled again"
        );
        let mut newly_waiting = false;
        let mut waited = None;
        let mut wake_next = None;
        let admitted = {
            let mut state = admission_future.admission.state.lock().unwrap();
            match admission_future.waiter_id {
                Some(waiter_id) => {
                    let position = state
                        .task_waiters
                        .iter()
                        .position(|waiter| waiter.id == waiter_id)
                        .expect("pending admission waiter should remain registered");
                    if position == 0
                        && state.occupied < admission_future.admission.dispatch_capacity.get()
                    {
                        state.task_waiters.pop_front();
                        state.occupied += 1;
                        admission_future.waiter_id = None;
                        waited = admission_future
                            .wait_started
                            .take()
                            .map(|start| start.elapsed());
                        wake_next =
                            next_task_waker(&state, admission_future.admission.dispatch_capacity);
                        true
                    } else {
                        let waiter = &mut state.task_waiters[position];
                        if !waiter.waker.will_wake(context.waker()) {
                            waiter.waker = context.waker().clone();
                        }
                        false
                    }
                }
                None if state.task_waiters.is_empty()
                    && state.occupied < admission_future.admission.dispatch_capacity.get() =>
                {
                    state.occupied += 1;
                    true
                }
                None => {
                    let waiter_id = state.next_waiter_id;
                    state.next_waiter_id = state.next_waiter_id.wrapping_add(1);
                    state.task_waiters.push_back(TaskWaiter {
                        id: waiter_id,
                        waker: context.waker().clone(),
                    });
                    admission_future.waiter_id = Some(waiter_id);
                    admission_future.wait_started = Some(Instant::now());
                    newly_waiting = true;
                    false
                }
            }
        };

        if newly_waiting {
            metrics::record_task_admission_wait();
        }
        if let Some(elapsed) = waited {
            metrics::record_task_admission_wait_duration(elapsed);
        }
        if let Some(waker) = wake_next {
            waker.wake();
        }
        if !admitted {
            return Poll::Pending;
        }

        admission_future.completed = true;
        Poll::Ready(DispatchPermit {
            admission: Arc::clone(&admission_future.admission),
        })
    }
}

impl Drop for DispatchAdmission {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let Some(waiter_id) = self.waiter_id else {
            return;
        };
        let wake = {
            let mut state = self.admission.state.lock().unwrap();
            let Some(position) = state
                .task_waiters
                .iter()
                .position(|waiter| waiter.id == waiter_id)
            else {
                return;
            };
            state.task_waiters.remove(position);
            (position == 0)
                .then(|| next_task_waker(&state, self.admission.dispatch_capacity))
                .flatten()
        };
        if let Some(waker) = wake {
            waker.wake();
        }
    }
}

pub(crate) struct DispatchPermit {
    admission: Arc<CpuAdmission>,
}

impl Drop for DispatchPermit {
    fn drop(&mut self) {
        self.admission
            .release_workers(NonZeroUsize::new(1).unwrap());
    }
}

fn next_task_waker(state: &AdmissionState, capacity: NonZeroUsize) -> Option<Waker> {
    (state.occupied < capacity.get())
        .then(|| {
            state
                .task_waiters
                .front()
                .map(|waiter| waiter.waker.clone())
        })
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poll(admission: &mut Pin<Box<DispatchAdmission>>) -> Poll<DispatchPermit> {
        let mut context = Context::from_waker(Waker::noop());
        admission.as_mut().poll(&mut context)
    }

    fn ready(poll: Poll<DispatchPermit>) -> DispatchPermit {
        let Poll::Ready(permit) = poll else {
            panic!("admission should be ready");
        };
        permit
    }

    #[test]
    fn dispatch_waits_outside_pool_and_has_priority_over_parallel_work() {
        let capacity = NonZeroUsize::new(2).unwrap();
        let admission = Arc::new(CpuAdmission::new(capacity, capacity));
        let mut first = Box::pin(admission.acquire_dispatch());
        let mut second = Box::pin(admission.acquire_dispatch());
        let first = ready(poll(&mut first));
        let second = ready(poll(&mut second));
        let mut waiting = Box::pin(admission.acquire_dispatch());

        assert!(poll(&mut waiting).is_pending());
        assert_eq!(admission.state(), (2, 1));
        drop(first);
        assert!(!admission.try_reserve_parallel(NonZeroUsize::new(1).unwrap()));

        let waiting = ready(poll(&mut waiting));
        assert_eq!(admission.state(), (2, 0));
        drop(second);
        drop(waiting);
    }

    #[test]
    fn cancelling_front_waiter_allows_next_task_to_run() {
        let capacity = NonZeroUsize::new(1).unwrap();
        let admission = Arc::new(CpuAdmission::new(capacity, capacity));
        let mut running = Box::pin(admission.acquire_dispatch());
        let running = ready(poll(&mut running));
        let mut cancelled = Box::pin(admission.acquire_dispatch());
        let mut next = Box::pin(admission.acquire_dispatch());
        assert!(poll(&mut cancelled).is_pending());
        assert!(poll(&mut next).is_pending());

        drop(cancelled);
        drop(running);
        let next = ready(poll(&mut next));
        assert_eq!(admission.state(), (1, 0));
        drop(next);
    }

    #[test]
    fn parallel_reservation_uses_only_unoccupied_capacity() {
        let capacity = NonZeroUsize::new(4).unwrap();
        let admission = Arc::new(CpuAdmission::new(capacity, capacity));
        let mut running = Box::pin(admission.acquire_dispatch());
        let running = ready(poll(&mut running));
        let parallel_workers = NonZeroUsize::new(2).unwrap();

        assert!(admission.try_reserve_parallel(parallel_workers));
        assert_eq!(admission.state(), (3, 0));
        assert!(!admission.try_reserve_parallel(NonZeroUsize::new(1).unwrap()));

        admission.release_parallel(parallel_workers);
        drop(running);
        assert_eq!(admission.state(), (0, 0));
    }

    #[test]
    fn relation_parallelism_is_bounded_below_dispatch_capacity() {
        let admission = Arc::new(CpuAdmission::new(
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        ));
        let mut running = Box::pin(admission.acquire_dispatch());
        let running = ready(poll(&mut running));
        let parallel_worker = NonZeroUsize::new(1).unwrap();

        assert_eq!(admission.capacity(), NonZeroUsize::new(2).unwrap());
        assert!(admission.try_reserve_parallel(parallel_worker));
        assert!(!admission.try_reserve_parallel(parallel_worker));

        admission.release_parallel(parallel_worker);
        drop(running);
    }
}
