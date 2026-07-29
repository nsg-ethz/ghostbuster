// BgpSim-GNS3: Control and interact with GNS3 from BgpSim
// Copyright (C) 2022-2023 Tibor Schneider <sctibor@ethz.ch>
//
// This program is free software; you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation; either version 2 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License along
// with this program; if not, write to the Free Software Foundation, Inc.,
// 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301 USA.

//! This module is used to interact with GNS3 via the Rest API.

pub mod links;
pub mod nodes;
pub mod projects;
pub mod templates;
pub use projects::Gns3Project;

use log::{debug, error, warn};
use rustify::{blocking::clients::reqwest::Client, errors::ClientError, Endpoint};
use std::{
    fmt::Debug,
    sync::{LazyLock, Mutex},
};
use thiserror::Error;

/// Error thrown by Gns3
#[derive(Debug, Error)]
pub enum Gns3Error {
    /// Error from communicating with the server.
    #[error("{0}")]
    ApiError(#[from] ClientError),
    /// An FRR template already exists but is invalid
    #[error("An FRR template already exists with the name '{0}' already exists, but its associated image is '{1}', not '{2}'!")]
    InvalidFrrTemplate(String, String, String),
    /// An invalid IpTerm template already exists
    #[error("An invalid IpTErm template already exists with the same name!")]
    InvalidIptermTemplate,
    /// There are not enough interfaces on a node to create the links.
    #[error("There are not enough interfaces on a node to create a new link!")]
    NotEnoughInterfaces,
    /// No link was found connecting the two devices.
    #[error("No link between {0} and {1} found in the GNS3 network!")]
    NoLinkFound(String, String),
    /// No snapshot of this network exists.
    #[error("This network has no snapshot!")]
    NoSnapshotFound,
}

trait LoggingClient {
    fn exec<E>(&self, endpoint: E) -> Result<E::Response, Gns3Error>
    where
        E: Endpoint + Debug,
        E::Response: Debug;

    fn exec_no_content<E>(&self, endpoint: E) -> Result<(), Gns3Error>
    where
        E: Endpoint + Debug;
}

static GNS3_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

impl LoggingClient for Client {
    fn exec<E>(&self, endpoint: E) -> Result<E::Response, Gns3Error>
    where
        E: Endpoint + Debug,
        E::Response: Debug,
    {
        let l = GNS3_LOCK.lock();
        debug!("request {:?}: {}\n{:#?}", endpoint.method(), endpoint.path(), endpoint);
        let response = endpoint.exec_block(self);
        match response {
            Ok(r) => {
                if r.response.status().as_u16() == 204 {
                    debug!("response status: {} (success)", r.response.status(),);
                } else if r.response.status().is_success() {
                    debug!(
                        "response status: {} (success), content:\n{}",
                        r.response.status(),
                        String::from_utf8_lossy(r.response.body())
                    );
                } else {
                    error!(
                        "response status: {} (error), content:\n{}",
                        r.response.status(),
                        String::from_utf8_lossy(r.response.body())
                    );
                }
                std::mem::drop(l);
                Ok(r.parse()?)
            }
            Err(e) => {
                error!("Could not perform the request: \n{}\n{:#?}", e, endpoint);
                std::mem::drop(l);
                Err(e.into())
            }
        }
    }

    fn exec_no_content<E>(&self, endpoint: E) -> Result<(), Gns3Error>
    where
        E: Endpoint + Debug,
    {
        let l = GNS3_LOCK.lock();
        debug!("request {:?}: {}\n{:#?}", endpoint.method(), endpoint.path(), endpoint);
        let response = endpoint.exec_block(self);
        match response {
            Ok(r) => {
                if r.response.status().as_u16() == 204 {
                    debug!("response status: {} (success)", r.response.status(),);
                } else if r.response.status().is_success() {
                    warn!(
                        "Expected 204 No Content, found: {}\n{}",
                        r.response.status(),
                        String::from_utf8_lossy(r.response.body())
                    );
                } else {
                    error!(
                        "response status: {} (error), content:\n{}",
                        r.response.status(),
                        String::from_utf8_lossy(r.response.body())
                    );
                }
                std::mem::drop(l);
                Ok(())
            }
            Err(e) => {
                error!("Could not perform the request: \n{}\n{:#?}", e, endpoint);
                std::mem::drop(l);
                Err(e.into())
            }
        }
    }
}
