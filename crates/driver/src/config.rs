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
use mica_runtime::TaskLimits;
use std::num::NonZeroUsize;

pub const DEFAULT_EVENT_QUEUE_CAPACITY: usize = 1024;
pub const DEFAULT_SUBSCRIPTION_QUEUE_BUDGET: usize = 256;

/// Process resources reserved for one Mica driver.
///
/// A host must choose the dispatcher worker count. Other limits start from
/// conservative defaults and remain explicit fields that can be adjusted before
/// construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DriverResources {
    pub worker_count: NonZeroUsize,
    pub relation_parallelism: NonZeroUsize,
    pub affinity: DispatcherAffinity,
    pub task_limits: TaskLimits,
    pub event_queue_capacity: NonZeroUsize,
    pub subscription_queue_budget: NonZeroUsize,
}

impl DriverResources {
    pub fn new(worker_count: NonZeroUsize) -> Self {
        Self {
            worker_count,
            relation_parallelism: worker_count,
            affinity: DispatcherAffinity::None,
            task_limits: TaskLimits::default(),
            event_queue_capacity: NonZeroUsize::new(DEFAULT_EVENT_QUEUE_CAPACITY).unwrap(),
            subscription_queue_budget: NonZeroUsize::new(DEFAULT_SUBSCRIPTION_QUEUE_BUDGET)
                .unwrap(),
        }
    }

    pub(crate) fn validate(self) -> Result<Self, DriverError> {
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
        Ok(self)
    }

    pub(crate) fn dispatcher_config(self) -> DispatcherConfig {
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
}
