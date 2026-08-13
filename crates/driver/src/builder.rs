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
    CompioTaskDriver, DriverError, DriverResources, ExternalRequestHandler,
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

pub struct CompioTaskDriverBuilder {
    resources: DriverResources,
    storage: DriverStorage,
    initial_fileins: Vec<InitialFilein>,
    external_request_handler: Option<ExternalRequestHandler>,
    external_stream_request_handler: Option<ExternalStreamRequestHandler>,
}

impl CompioTaskDriverBuilder {
    pub fn new(resources: DriverResources) -> Self {
        Self {
            resources,
            storage: DriverStorage::Memory,
            initial_fileins: Vec::new(),
            external_request_handler: None,
            external_stream_request_handler: None,
        }
    }

    pub fn storage(mut self, storage: DriverStorage) -> Self {
        self.storage = storage;
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

    pub fn build(self) -> Result<CompioTaskDriver, DriverError> {
        self.resources.validate()?;
        let mut runner = match self.storage {
            DriverStorage::Memory => SourceRunner::new_empty(),
            DriverStorage::Fjall { path, durability } => {
                #[cfg(feature = "fjall")]
                {
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
        };
        for filein in self.initial_fileins {
            match filein {
                InitialFilein::Source {
                    source,
                    include_loader,
                } => match include_loader {
                    Some(loader) => runner
                        .run_filein_with_include_loader(&source, |path| loader(path))
                        .map(|_| ()),
                    None => runner.run_filein(&source).map(|_| ()),
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
                        .map(|_| ()),
                    None => runner.run_filein_with_unit(unit, &source, mode).map(|_| ()),
                },
            }
            .map_err(DriverError::Source)?;
        }
        CompioTaskDriver::spawn_with_resources_and_external_handlers(
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
            let driver = CompioTaskDriver::builder(resources)
                .initial_filein("make_identity(:base)", None)
                .initial_filein_unit(
                    Symbol::intern("equipment"),
                    "make_identity(:sensor)\n\
                     make_relation(:Label, 2)\n\
                     assert Label(#sensor, include_text(\"label.txt\"))\n",
                    FileinMode::Add,
                    Some(include_loader),
                )
                .build()
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
            let driver = CompioTaskDriver::builder(resources.clone())
                .storage(DriverStorage::Fjall {
                    path: path.clone(),
                    durability: DriverDurability::Relaxed,
                })
                .initial_filein("make_identity(:persisted)", None)
                .build()
                .unwrap();

            driver.shutdown().await.unwrap();
            drop(driver);

            let reopened = CompioTaskDriver::builder(resources)
                .storage(DriverStorage::Fjall {
                    path: path.clone(),
                    durability: DriverDurability::Relaxed,
                })
                .build()
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
        let result = CompioTaskDriver::builder(resources)
            .storage(DriverStorage::Fjall {
                path: PathBuf::from("unused"),
                durability: DriverDurability::Strict,
            })
            .build();

        assert!(matches!(
            result,
            Err(DriverError::Configuration(message)) if message.contains("`fjall` feature")
        ));
    }
}
