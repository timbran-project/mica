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

use mica_driver::CompioTaskDriver;
use mica_var::Identity;
use std::sync::Arc;

pub mod codec;

pub mod auth;
pub mod metrics;
mod request;
mod response;
mod server;
mod sync;

pub use server::{serve, serve_in_process};

pub const DEFAULT_BIND: &str = "127.0.0.1:8080";

#[derive(Clone, Debug)]
pub struct RequestBinding {
    pub principal: Identity,
    pub actor: Option<Identity>,
}

pub struct InProcessWebHost {
    pub(crate) driver: Arc<CompioTaskDriver>,
    pub(crate) sync: sync::InProcessSyncHost,
    pub(crate) auth: Option<Arc<auth::AuthSubsystem>>,
}

impl InProcessWebHost {
    pub fn new(driver: CompioTaskDriver) -> Self {
        let driver = Arc::new(driver);
        Self {
            sync: sync::InProcessSyncHost::new(driver.clone()),
            driver,
            auth: None,
        }
    }

    pub fn with_auth(mut self, auth: auth::AuthSubsystem) -> Self {
        self.auth = Some(Arc::new(auth));
        self
    }

    pub(crate) fn allocate_endpoint(&self) -> Result<Identity, String> {
        self.driver
            .allocate_ephemeral_identity()
            .map_err(|error| self.driver.format_error(&error))
    }

    pub(crate) fn allocate_request(&self) -> Result<Identity, String> {
        self.driver
            .allocate_ephemeral_identity()
            .map_err(|error| self.driver.format_error(&error))
    }
}

pub(crate) fn format_driver_error(
    driver: &mica_driver::CompioTaskDriver,
    error: mica_driver::DriverError,
) -> String {
    format!("error: {}", driver.format_error(&error))
}
