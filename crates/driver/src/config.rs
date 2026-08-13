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

use crate::{DispatcherAffinity, DispatcherConfig, DriverError};
use mica_runtime::{RelationAccelerator, TaskLimits};
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::Arc;

pub const DEFAULT_EVENT_QUEUE_CAPACITY: usize = 1024;
pub const DEFAULT_EXTERNAL_REQUEST_CAPACITY: usize = 64;
pub const DEFAULT_SUBSCRIPTION_QUEUE_BUDGET: usize = 256;
pub const DEFAULT_ACTIVE_TASK_CAPACITY: usize = 4096;
pub const DEFAULT_SUSPENDED_TASK_CAPACITY: usize = 1024;
pub const DEFAULT_TIMER_CAPACITY: usize = 1024;
pub const DEFAULT_TERMINAL_TASK_RETENTION: usize = 1024;
pub const DEFAULT_ENDPOINT_CAPACITY: usize = 1024;
pub const DEFAULT_SUBSCRIPTION_CAPACITY: usize = 4096;
pub const DEFAULT_SUBSCRIPTION_MAILBOX_CAPACITY: usize = 64;
pub const DEFAULT_EPHEMERAL_IDENTITY_CAPACITY: usize = 65_536;

/// Relation execution backend selected when the driver is constructed.
#[derive(Clone)]
pub enum RelationAcceleration {
    /// Use the complete CPU implementation without initializing WGPU.
    Disabled,
    /// Lazily create and share Mica's process-wide WGPU accelerator, falling
    /// back to CPU when no suitable Vulkan device is available.
    Automatic,
    /// Use an accelerator constructed and owned by the embedding host.
    HostProvided(Arc<dyn RelationAccelerator>),
}

impl fmt::Debug for RelationAcceleration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("Disabled"),
            Self::Automatic => formatter.write_str("Automatic"),
            Self::HostProvided(_) => formatter.write_str("HostProvided"),
        }
    }
}

/// Process resources reserved for one Mica driver.
///
/// A host must choose the dispatcher worker count. Other limits start from
/// conservative defaults and remain explicit fields that can be adjusted before
/// construction.
#[derive(Clone, Debug)]
pub struct DriverResources {
    pub worker_count: NonZeroUsize,
    pub relation_parallelism: NonZeroUsize,
    pub affinity: DispatcherAffinity,
    pub task_limits: TaskLimits,
    pub event_queue_capacity: NonZeroUsize,
    pub external_request_capacity: NonZeroUsize,
    pub subscription_queue_budget: NonZeroUsize,
    pub active_task_capacity: NonZeroUsize,
    pub suspended_task_capacity: NonZeroUsize,
    pub timer_capacity: NonZeroUsize,
    pub terminal_task_retention: NonZeroUsize,
    pub endpoint_capacity: NonZeroUsize,
    pub subscription_capacity: NonZeroUsize,
    pub subscription_mailbox_capacity: NonZeroUsize,
    pub ephemeral_identity_capacity: NonZeroUsize,
    pub relation_acceleration: RelationAcceleration,
}

impl DriverResources {
    pub fn new(worker_count: NonZeroUsize) -> Self {
        Self {
            worker_count,
            relation_parallelism: worker_count,
            affinity: DispatcherAffinity::None,
            task_limits: TaskLimits::default(),
            event_queue_capacity: NonZeroUsize::new(DEFAULT_EVENT_QUEUE_CAPACITY).unwrap(),
            external_request_capacity: NonZeroUsize::new(DEFAULT_EXTERNAL_REQUEST_CAPACITY)
                .unwrap(),
            subscription_queue_budget: NonZeroUsize::new(DEFAULT_SUBSCRIPTION_QUEUE_BUDGET)
                .unwrap(),
            active_task_capacity: NonZeroUsize::new(DEFAULT_ACTIVE_TASK_CAPACITY).unwrap(),
            suspended_task_capacity: NonZeroUsize::new(DEFAULT_SUSPENDED_TASK_CAPACITY).unwrap(),
            timer_capacity: NonZeroUsize::new(DEFAULT_TIMER_CAPACITY).unwrap(),
            terminal_task_retention: NonZeroUsize::new(DEFAULT_TERMINAL_TASK_RETENTION).unwrap(),
            endpoint_capacity: NonZeroUsize::new(DEFAULT_ENDPOINT_CAPACITY).unwrap(),
            subscription_capacity: NonZeroUsize::new(DEFAULT_SUBSCRIPTION_CAPACITY).unwrap(),
            subscription_mailbox_capacity: NonZeroUsize::new(DEFAULT_SUBSCRIPTION_MAILBOX_CAPACITY)
                .unwrap(),
            ephemeral_identity_capacity: NonZeroUsize::new(DEFAULT_EPHEMERAL_IDENTITY_CAPACITY)
                .unwrap(),
            relation_acceleration: RelationAcceleration::Disabled,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), DriverError> {
        if self.relation_parallelism > self.worker_count {
            return Err(DriverError::Configuration(format!(
                "relation parallelism {} exceeds dispatcher worker count {}",
                self.relation_parallelism, self.worker_count
            )));
        }
        if self.task_limits.instruction_budget == 0 {
            return Err(DriverError::Configuration(
                "task instruction budget must be non-zero".to_owned(),
            ));
        }
        if self.task_limits.max_call_depth == 0 {
            return Err(DriverError::Configuration(
                "task call-depth limit must be non-zero".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn dispatcher_config(&self) -> DispatcherConfig {
        DispatcherConfig {
            workers: Some(self.worker_count),
            affinity: self.affinity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_relation_parallelism_above_dispatcher_capacity() {
        let mut resources = DriverResources::new(NonZeroUsize::new(1).unwrap());
        resources.relation_parallelism = NonZeroUsize::new(2).unwrap();

        assert!(matches!(
            resources.validate(),
            Err(DriverError::Configuration(message))
                if message.contains("relation parallelism 2")
        ));
    }

    #[test]
    fn embedded_resource_policy_does_not_initialize_gpu_by_default() {
        let resources = DriverResources::new(NonZeroUsize::new(1).unwrap());

        assert!(matches!(
            resources.relation_acceleration,
            RelationAcceleration::Disabled
        ));
    }
}
