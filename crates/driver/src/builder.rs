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

use crate::{
    CompioTaskDriver, DriverError, DriverOwner, DriverResources, ExternalRequestHandler,
    ExternalStreamRequestHandler, FileinIncludeLoader,
};
use mica_runtime::{FileinMode, SourceRunner};
use mica_var::Symbol;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DriverDurability {
    Strict,
    Relaxed,
}

#[cfg(feature = "fjall")]
impl From<DriverDurability> for mica_runtime::FjallDurabilityMode {
    fn from(value: DriverDurability) -> Self {
        match value {
            DriverDurability::Strict => Self::Strict,
            DriverDurability::Relaxed => Self::Relaxed,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum DriverStorage {
    #[default]
    Memory,
    Fjall {
        path: PathBuf,
        durability: DriverDurability,
    },
}

enum InitialFilein {
    Source {
        source: String,
        include_loader: Option<FileinIncludeLoader>,
    },
    Unit {
        unit: Symbol,
        source: String,
        mode: FileinMode,
        include_loader: Option<FileinIncludeLoader>,
    },
}

pub struct DriverBuilder {
    resources: DriverResources,
    storage: DriverStorage,
    source_runner: Option<SourceRunner>,
    #[cfg(feature = "source-provider")]
    source_config: Option<mica_runtime::SourceConfig>,
    initial_fileins: Vec<InitialFilein>,
    external_request_handler: Option<ExternalRequestHandler>,
    external_stream_request_handler: Option<ExternalStreamRequestHandler>,
}

impl DriverBuilder {
    pub fn new(resources: DriverResources) -> Self {
        Self {
            resources,
            storage: DriverStorage::Memory,
            source_runner: None,
            #[cfg(feature = "source-provider")]
            source_config: None,
            initial_fileins: Vec::new(),
            external_request_handler: None,
            external_stream_request_handler: None,
        }
    }

    pub fn storage(mut self, storage: DriverStorage) -> Self {
        self.storage = storage;
        self
    }

    pub fn source_runner(mut self, runner: SourceRunner) -> Self {
        self.source_runner = Some(runner);
        self
    }

    #[cfg(feature = "source-provider")]
    pub fn source_config(mut self, config: mica_runtime::SourceConfig) -> Self {
        self.source_config = Some(config);
        self
    }

    pub fn initial_filein(
        mut self,
        source: impl Into<String>,
        include_loader: Option<FileinIncludeLoader>,
    ) -> Self {
        self.initial_fileins.push(InitialFilein::Source {
            source: source.into(),
            include_loader,
        });
        self
    }

    pub fn initial_filein_unit(
        mut self,
        unit: Symbol,
        source: impl Into<String>,
        mode: FileinMode,
        include_loader: Option<FileinIncludeLoader>,
    ) -> Self {
        self.initial_fileins.push(InitialFilein::Unit {
            unit,
            source: source.into(),
            mode,
            include_loader,
        });
        self
    }

    pub fn external_request_handler(mut self, handler: ExternalRequestHandler) -> Self {
        self.external_request_handler = Some(handler);
        self
    }

    pub fn external_stream_request_handler(
        mut self,
        handler: ExternalStreamRequestHandler,
    ) -> Self {
        self.external_stream_request_handler = Some(handler);
        self
    }

    pub fn build(self) -> Result<DriverOwner, DriverError> {
        self.build_driver().map(DriverOwner::new)
    }

    pub(crate) fn build_driver(self) -> Result<CompioTaskDriver, DriverError> {
        self.resources.validate()?;
        if self.source_runner.is_some() && !matches!(self.storage, DriverStorage::Memory) {
            return Err(DriverError::Configuration(
                "a source runner and driver storage cannot both be configured".to_owned(),
            ));
        }
        #[cfg(feature = "source-provider")]
        if self.source_runner.is_some() && self.source_config.is_some() {
            return Err(DriverError::Configuration(
                "a source runner and source-provider configuration cannot both be configured"
                    .to_owned(),
            ));
        }
        #[cfg(feature = "source-provider")]
        let source_config = self.source_config;
        let mut runner = match (self.source_runner, self.storage) {
            (Some(runner), DriverStorage::Memory) => runner,
            (None, DriverStorage::Memory) => {
                #[cfg(feature = "source-provider")]
                if let Some(config) = source_config {
                    SourceRunner::new_empty_with_source(config)
                } else {
                    SourceRunner::new_empty()
                }
                #[cfg(not(feature = "source-provider"))]
                SourceRunner::new_empty()
            }
            (None, DriverStorage::Fjall { path, durability }) => {
                #[cfg(feature = "fjall")]
                {
                    #[cfg(feature = "source-provider")]
                    if let Some(config) = source_config {
                        SourceRunner::open_fjall_with_embedding_provider_and_source(
                            path,
                            durability.into(),
                            mica_runtime::EmbeddingProviderKind::Deterministic,
                            config,
                        )
                        .map_err(DriverError::Storage)?
                    } else {
                        SourceRunner::open_fjall(path, durability.into())
                            .map_err(DriverError::Storage)?
                    }
                    #[cfg(not(feature = "source-provider"))]
                    SourceRunner::open_fjall(path, durability.into())
                        .map_err(DriverError::Storage)?
                }
                #[cfg(not(feature = "fjall"))]
                {
                    let _ = (path, durability);
                    return Err(DriverError::Configuration(
                        "Fjall storage requires the mica-driver `fjall` feature".to_owned(),
                    ));
                }
            }
            (Some(_), DriverStorage::Fjall { .. }) => unreachable!("validated above"),
        };
        for filein in self.initial_fileins {
            let reports: Vec<_> = match filein {
                InitialFilein::Source {
                    source,
                    include_loader,
                } => match include_loader {
                    Some(loader) => runner
                        .run_filein_with_include_loader(&source, |path| loader(path))
                        .map(|reports| reports.into_iter().map(|report| report.task_id).collect()),
                    None => runner
                        .run_filein(&source)
                        .map(|reports| reports.into_iter().map(|report| report.task_id).collect()),
                },
                InitialFilein::Unit {
                    unit,
                    source,
                    mode,
                    include_loader,
                } => match include_loader {
                    Some(loader) => runner
                        .run_filein_with_unit_and_include_loader(unit, &source, mode, |path| {
                            loader(path)
                        })
                        .map(|report| {
                            report
                                .reports
                                .into_iter()
                                .map(|report| report.task_id)
                                .collect()
                        }),
                    None => runner
                        .run_filein_with_unit(unit, &source, mode)
                        .map(|report| {
                            report
                                .reports
                                .into_iter()
                                .map(|report| report.task_id)
                                .collect()
                        }),
                },
            }
            .map_err(DriverError::Source)?;
            for task_id in reports {
                runner.forget_terminal_task(task_id);
            }
        }
        CompioTaskDriver::spawn(
            runner,
            self.resources,
            self.external_request_handler,
            self.external_stream_request_handler,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mica_var::Symbol;
    use std::num::NonZeroUsize;
    use std::sync::Arc;

    #[test]
    fn builds_driver_with_initial_sources_and_units() {
        crate::test_support::run(async {
            let resources = DriverResources::new(NonZeroUsize::new(1).unwrap());
            let include_loader = Arc::new(|path: &str| match path {
                "label.txt" => Ok("embedded".to_owned()),
                _ => Err(format!("unknown include {path}")),
            });
            let driver = DriverOwner::builder(resources)
                .initial_filein("make_identity(:base)", None)
                .initial_filein_unit(
                    Symbol::intern("equipment"),
                    "make_identity(:sensor)\n\
                     make_relation(:Label, 2)\n\
                     assert Label(#sensor, include_text(\"label.txt\"))\n",
                    FileinMode::Add,
                    Some(include_loader),
                )
                .build_driver()
                .unwrap();

            driver.named_identity(Symbol::intern("base")).unwrap();
            let source = driver
                .fileout_unit(Symbol::intern("equipment"))
                .await
                .unwrap();
            assert!(source.contains("embedded"));
            driver.shutdown().await.unwrap();
        });
    }

    #[cfg(feature = "fjall")]
    #[test]
    fn shutdown_flushes_relaxed_persistent_storage() {
        crate::test_support::run(async {
            let path = std::env::temp_dir().join(format!(
                "mica-driver-shutdown-flush-{}-{}",
                std::process::id(),
                Symbol::intern("shutdown_flushes_relaxed_persistent_storage").id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            let resources = DriverResources::new(NonZeroUsize::new(1).unwrap());
            let driver = DriverOwner::builder(resources.clone())
                .storage(DriverStorage::Fjall {
                    path: path.clone(),
                    durability: DriverDurability::Relaxed,
                })
                .initial_filein("make_identity(:persisted)", None)
                .build_driver()
                .unwrap();

            driver.shutdown().await.unwrap();
            drop(driver);

            let reopened = DriverOwner::builder(resources)
                .storage(DriverStorage::Fjall {
                    path: path.clone(),
                    durability: DriverDurability::Relaxed,
                })
                .build_driver()
                .unwrap();
            reopened
                .named_identity(Symbol::intern("persisted"))
                .unwrap();
            reopened.shutdown().await.unwrap();
            drop(reopened);
            let _ = std::fs::remove_dir_all(path);
        });
    }

    #[cfg(not(feature = "fjall"))]
    #[test]
    fn reports_unavailable_fjall_provider_without_changing_the_api() {
        let resources = DriverResources::new(NonZeroUsize::new(1).unwrap());
        let result = DriverOwner::builder(resources)
            .storage(DriverStorage::Fjall {
                path: PathBuf::from("unused"),
                durability: DriverDurability::Strict,
            })
            .build_driver();

        assert!(matches!(
            result,
            Err(DriverError::Configuration(message)) if message.contains("`fjall` feature")
        ));
    }
}
