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

//! Compio-driven task driver for Mica.
//!
//! `mica-runtime` exposes a synchronous task manager. This crate schedules that
//! task-manager work on compio tasks and owns the wake policy for timed
//! suspensions and endpoint input.

mod affinity;
mod config;
mod execution;
pub mod metrics;
mod pool;
mod types;

#[cfg(test)]
mod tests;

pub use affinity::{
    DispatcherAffinity, DispatcherConfig, DispatcherPlacement, configure_dispatcher,
};
pub use config::{
    DEFAULT_EVENT_QUEUE_CAPACITY, DEFAULT_SUBSCRIPTION_QUEUE_BUDGET, DriverResources,
};
pub use mica_runtime::TaskLimits;
pub use pool::CompioTaskDriver;
pub use types::{
    DriverError, DriverEvent, DriverSubscriptionMailbox, DriverSubscriptionRequest,
    EndpointCloseReport, ExternalRequestHandler, ExternalStreamEmitter,
    ExternalStreamRequestHandler, FileinIncludeLoader, TaskCancellationReason, TaskContext,
};
