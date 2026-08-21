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
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use compio::dispatcher::DispatcherBuilder;
use std::collections::HashSet;
use std::num::NonZeroUsize;

#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::Path;

// Topology is read from Linux sysfs; every other platform reports "no
// preference" and lets the OS schedule.
#[cfg(target_os = "linux")]
const CPU_SYSFS_ROOT: &str = "/sys/devices/system/cpu";

// Scoring thresholds. The logic below is platform-independent and unit-tested
// everywhere, but only sysfs feeds it real data, so it reads as dead off Linux.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const MIN_HETEROGENEITY_RATIO: f64 = 0.10;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const PERFORMANCE_THRESHOLD_RATIO: f64 = 0.90;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatcherAffinity {
    Auto,
    Performance,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DispatcherConfig {
    pub workers: Option<NonZeroUsize>,
    pub affinity: DispatcherAffinity,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            workers: None,
            affinity: DispatcherAffinity::Auto,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatcherPlacement {
    pub worker_count: NonZeroUsize,
    pub pinned_core_ids: Option<Vec<usize>>,
}

impl DispatcherPlacement {
    pub fn is_pinned(&self) -> bool {
        self.pinned_core_ids.is_some()
    }
}

pub fn configure_dispatcher(
    mut builder: DispatcherBuilder,
    config: DispatcherConfig,
) -> (DispatcherBuilder, DispatcherPlacement) {
    let affinity_core_ids = match config.affinity {
        DispatcherAffinity::None => None,
        DispatcherAffinity::Auto | DispatcherAffinity::Performance => {
            detect_performance_logical_processors().unwrap_or_default()
        }
    };

    if let Some(core_ids) = affinity_core_ids {
        let worker_count = config
            .workers
            .map(NonZeroUsize::get)
            .unwrap_or(core_ids.len())
            .max(1);
        let worker_count = NonZeroUsize::new(worker_count).unwrap();
        builder = builder.worker_threads(worker_count);
        let pinned_core_ids = core_ids.clone();
        builder = builder.thread_affinity(move |index| {
            HashSet::from([pinned_core_ids[index % pinned_core_ids.len()]])
        });
        return (
            builder,
            DispatcherPlacement {
                worker_count,
                pinned_core_ids: Some(core_ids),
            },
        );
    }

    let worker_count = config.workers.unwrap_or_else(|| {
        std::thread::available_parallelism().unwrap_or(NonZeroUsize::new(1).unwrap())
    });
    builder = builder.worker_threads(worker_count);

    (
        builder,
        DispatcherPlacement {
            worker_count,
            pinned_core_ids: None,
        },
    )
}

#[derive(Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct PhysicalCoreMetric {
    logical_processor_ids: Vec<usize>,
    capacity: Option<u32>,
    max_freq_khz: Option<u32>,
}

fn detect_performance_logical_processors() -> Result<Option<Vec<usize>>, String> {
    #[cfg(not(target_os = "linux"))]
    {
        Ok(None)
    }

    #[cfg(target_os = "linux")]
    {
        let cores = read_physical_core_metrics(Path::new(CPU_SYSFS_ROOT))?;
        if cores.is_empty() {
            return Ok(None);
        }
        Ok(select_performance_core_ids(&cores, |core| core.capacity)
            .or_else(|| select_performance_core_ids(&cores, |core| core.max_freq_khz)))
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn select_performance_core_ids(
    cores: &[PhysicalCoreMetric],
    metric: impl Fn(&PhysicalCoreMetric) -> Option<u32>,
) -> Option<Vec<usize>> {
    let mut values = cores.iter().filter_map(&metric).collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let min_metric = *values.first().unwrap();
    let max_metric = *values.last().unwrap();
    if max_metric == 0 {
        return None;
    }
    let ratio = (max_metric - min_metric) as f64 / max_metric as f64;
    if ratio < MIN_HETEROGENEITY_RATIO {
        return None;
    }
    let threshold = (max_metric as f64 * PERFORMANCE_THRESHOLD_RATIO).ceil() as u32;
    let mut logical_processor_ids = Vec::new();
    let mut selected_physical_cores = 0usize;
    for core in cores {
        let Some(core_metric) = metric(core) else {
            continue;
        };
        if core_metric < threshold {
            continue;
        }
        selected_physical_cores += 1;
        logical_processor_ids.extend_from_slice(&core.logical_processor_ids);
    }
    if logical_processor_ids.is_empty() || selected_physical_cores == cores.len() {
        return None;
    }
    logical_processor_ids.sort_unstable();
    logical_processor_ids.dedup();
    Some(logical_processor_ids)
}

#[cfg(target_os = "linux")]
fn read_physical_core_metrics(root: &Path) -> Result<Vec<PhysicalCoreMetric>, String> {
    let mut physical_cores: HashMap<(usize, usize), PhysicalCoreMetric> = HashMap::new();
    for entry in
        fs::read_dir(root).map_err(|error| format!("failed to read {}: {error}", root.display()))?
    {
        let entry = entry.map_err(|error| format!("failed to read CPU directory: {error}"))?;
        let Some(logical_processor_id) =
            parse_cpu_dir(entry.file_name().to_string_lossy().as_ref())
        else {
            continue;
        };
        let cpu_path = entry.path();
        let topology_path = cpu_path.join("topology");
        let package_id = read_usize(topology_path.join("physical_package_id")).unwrap_or(0);
        let core_id = read_usize(topology_path.join("core_id")).unwrap_or(logical_processor_id);
        let key = (package_id, core_id);
        let entry = physical_cores.entry(key).or_insert(PhysicalCoreMetric {
            logical_processor_ids: Vec::new(),
            capacity: None,
            max_freq_khz: None,
        });
        entry.logical_processor_ids.push(logical_processor_id);
        entry.capacity = entry
            .capacity
            .or_else(|| read_u32(cpu_path.join("cpu_capacity")));
        entry.max_freq_khz = entry
            .max_freq_khz
            .or_else(|| read_u32(cpu_path.join("cpufreq/cpuinfo_max_freq")));
    }

    let mut cores = physical_cores.into_values().collect::<Vec<_>>();
    for core in &mut cores {
        core.logical_processor_ids.sort_unstable();
    }
    cores.sort_by(|left, right| {
        left.logical_processor_ids
            .first()
            .cmp(&right.logical_processor_ids.first())
    });
    Ok(cores)
}

#[cfg(target_os = "linux")]
fn parse_cpu_dir(name: &str) -> Option<usize> {
    let id = name.strip_prefix("cpu")?;
    id.parse().ok()
}

#[cfg(target_os = "linux")]
fn read_usize(path: impl AsRef<Path>) -> Option<usize> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(target_os = "linux")]
fn read_u32(path: impl AsRef<Path>) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_high_capacity_tier() {
        let cores = vec![
            PhysicalCoreMetric {
                logical_processor_ids: vec![0, 10],
                capacity: Some(512),
                max_freq_khz: None,
            },
            PhysicalCoreMetric {
                logical_processor_ids: vec![1, 11],
                capacity: Some(1024),
                max_freq_khz: None,
            },
        ];

        assert_eq!(
            select_performance_core_ids(&cores, |core| core.capacity),
            Some(vec![1, 11])
        );
    }

    #[test]
    fn rejects_homogeneous_topology() {
        let cores = vec![
            PhysicalCoreMetric {
                logical_processor_ids: vec![0],
                capacity: Some(1024),
                max_freq_khz: None,
            },
            PhysicalCoreMetric {
                logical_processor_ids: vec![1],
                capacity: Some(1024),
                max_freq_khz: None,
            },
        ];

        assert_eq!(
            select_performance_core_ids(&cores, |core| core.capacity),
            None
        );
    }
}
