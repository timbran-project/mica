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

#![doc = include_str!("../README.md")]

mod affinity;
mod builder;
mod config;
mod execution;
pub mod metrics;
mod pool;
mod types;

pub use affinity::{
    DispatcherAffinity, DispatcherConfig, DispatcherPlacement, configure_dispatcher,
};
pub use builder::{CompioTaskDriverBuilder, DriverDurability, DriverStorage};
pub use config::{
    DEFAULT_EVENT_QUEUE_CAPACITY, DEFAULT_EXTERNAL_REQUEST_CAPACITY,
    DEFAULT_SUBSCRIPTION_QUEUE_BUDGET, DriverResources, RelationAcceleration,
};
#[cfg(feature = "wgpu")]
pub use mica_relation_wgpu::{WgpuAccelerator, WgpuAcceleratorOptions};
pub use mica_runtime::{
    AuthorityContext, EPHEMERAL_HOST_IDENTITY_END, EPHEMERAL_HOST_IDENTITY_START, FileinMode,
    FileinReport, ReadOnlySourceQueryOptions, ReadOnlySourceQueryReport, ReadOnlySourceQueryStatus,
    RelationAccelerator, RunReport, SubmittedTask, SubscriptionInitialDelivery,
    SubscriptionSubject, SuspendKind, TaskId, TaskLimits, TaskOutcome, TaskRequest,
};
pub use mica_var::{Identity, Symbol, Value};
pub use pool::CompioTaskDriver;
pub use types::{
    DriverError, DriverEvent, DriverSubscriptionMailbox, DriverSubscriptionRequest,
    EndpointCloseReport, ExternalRequestCancellation, ExternalRequestContext,
    ExternalRequestFuture, ExternalRequestHandler, ExternalStreamEmitFuture, ExternalStreamEmitter,
    ExternalStreamRequestHandler, FileinIncludeLoader, TaskCancellationReason, TaskContext,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod test_support {
    use std::future::Future;
    use std::sync::Mutex;

    // Libtest runs unit tests concurrently, while every driver test owns a
    // dispatcher and a Compio runtime. Acquire this before constructing either.
    static COMPIO_RUNTIME_LOCK: Mutex<()> = Mutex::new(());

    pub(crate) fn run(future: impl Future<Output = ()>) {
        let _guard = COMPIO_RUNTIME_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        compio::runtime::Runtime::new()
            .expect("test Compio runtime should start")
            .block_on(future);
    }
}
