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

use std::path::PathBuf;

use getset::{CopyGetters, Getters, MutGetters, Setters};
use rustify_derive::Endpoint;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use self::{
    frr::{FrrClient, FrrError},
    ipterm::{IpTermClient, IpTermError},
    telnet_client::TelnetClient,
};

pub mod frr;
pub mod ipterm;
pub mod telnet_client;
pub use telnet_client::TelnetError;

/// Module containing all code for the GNS3 nodes.

#[derive(CopyGetters, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Gns3NodeIndex {
    #[getset(get_copy = "pub")]
    id: Uuid,
}

#[derive(Getters, MutGetters, CopyGetters, Debug)]
pub struct Gns3Node {
    /// Id of the node
    #[getset(get_copy = "pub")]
    node_id: Uuid,
    /// Name of the node
    #[getset(get = "pub")]
    name: String,
    /// Path to the node files.
    #[getset(get = "pub")]
    path: PathBuf,
    /// Port to connect to the node
    #[getset(get_copy = "pub")]
    port: u16,
    #[getset(get = "pub", get_mut = "pub(crate)")]
    interfaces: Vec<Gns3Interface>,
}

impl Gns3Node {
    pub fn index(&self) -> Gns3NodeIndex {
        Gns3NodeIndex { id: self.node_id() }
    }

    /// Get the FRR client to communicate with the node. Make sure the node is running and make
    /// sure that the node is an FRR!
    pub fn get_frr_client(&self, target: impl Into<String>) -> Result<FrrClient, FrrError> {
        FrrClient::new(target, self.port)
    }

    /// Get the IpTerm client to communicate with the node. Make sure the node is running and make
    /// sure that the node is an IpTerm!
    pub fn get_ipterm_client(
        &self,
        target: impl Into<String>,
    ) -> Result<IpTermClient, IpTermError> {
        IpTermClient::new(target, self.port)
    }

    /// Get the telnet client to communicate with the node. Make sure the node is running!
    pub fn get_client(&self, target: impl Into<String>) -> Result<TelnetClient, TelnetError> {
        TelnetClient::new(target, self.port, "# ")
    }
}

impl From<Gns3NodeRaw> for Gns3Node {
    fn from(x: Gns3NodeRaw) -> Self {
        Self {
            node_id: x.node_id,
            name: x.name,
            path: x.node_directory,
            port: x.console,
            interfaces: x.ports,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct Gns3NodeRaw {
    node_id: Uuid,
    name: String,
    node_directory: PathBuf,
    console: u16,
    ports: Vec<Gns3Interface>,
    status: String,
}

impl Gns3NodeRaw {
    pub(crate) fn is_running(&self) -> bool {
        &self.status == "started"
    }
}

#[derive(Getters, CopyGetters, Setters, Debug, Deserialize)]
pub struct Gns3Interface {
    #[getset(get = "pub")]
    link_type: String,
    #[getset(get_copy = "pub")]
    adapter_number: usize,
    #[getset(get_copy = "pub")]
    port_number: usize,
    #[getset(get = "pub")]
    name: String,
    #[serde(skip)]
    #[getset(get = "pub", set = "pub(crate)")]
    connected_to: Option<(Gns3NodeIndex, usize)>,
}

impl Gns3Interface {
    pub fn is_connected(&self) -> bool {
        self.connected_to.is_some()
    }
}

#[derive(Debug, Endpoint)]
#[endpoint(
    path = "projects/{self.project_id}/templates/{self.template_id}",
    method = "POST",
    response = "Gns3NodeRaw"
)]
pub(crate) struct CreateNodeFromTemplate {
    #[endpoint(skip)]
    pub project_id: Uuid,
    #[endpoint(skip)]
    pub template_id: Uuid,
    pub name: String,
    pub x: isize,
    pub y: isize,
}

#[derive(Debug, Endpoint)]
#[endpoint(
    path = "projects/{self.project_id}/nodes/{self.node_id}",
    method = "GET",
    response = "Gns3NodeRaw"
)]
pub(crate) struct GetNodeInfo {
    #[endpoint(skip)]
    pub project_id: Uuid,
    #[endpoint(skip)]
    pub node_id: Uuid,
}

#[derive(Debug, Endpoint)]
#[endpoint(
    path = "projects/{self.project_id}/nodes/{self.node_id}",
    method = "PUT",
    response = "Gns3NodeRaw"
)]
pub(crate) struct SetNodeName {
    #[endpoint(skip)]
    pub project_id: Uuid,
    #[endpoint(skip)]
    pub node_id: Uuid,
    pub name: String,
}

#[derive(Debug, Endpoint)]
#[endpoint(
    path = "projects/{self.project_id}/nodes/{self.node_id}",
    method = "PUT",
    response = "Gns3NodeRaw"
)]
pub(crate) struct SetNodePos {
    #[endpoint(skip)]
    pub project_id: Uuid,
    #[endpoint(skip)]
    pub node_id: Uuid,
    pub x: isize,
    pub y: isize,
}

#[derive(Debug, Endpoint)]
#[endpoint(
    path = "projects/{self.project_id}/nodes/{self.node_id}",
    method = "PUT",
    response = "Gns3NodeRaw"
)]
pub(crate) struct SetNodeLabel {
    #[endpoint(skip)]
    pub project_id: Uuid,
    #[endpoint(skip)]
    pub node_id: Uuid,
    pub label: Gns3NodeLabel,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Gns3NodeLabel {
    pub text: String,
    pub x: Option<isize>,
    pub y: Option<isize>,
    pub rotation: Option<isize>,
    pub style: Option<String>,
}

#[derive(Debug, Endpoint)]
#[endpoint(
    path = "projects/{self.project_id}/nodes/{self.node_id}/start",
    method = "POST",
    response = "Gns3NodeRaw"
)]
pub(crate) struct StartNode {
    #[endpoint(skip)]
    pub project_id: Uuid,
    #[endpoint(skip)]
    pub node_id: Uuid,
}

#[derive(Debug, Endpoint)]
#[endpoint(
    path = "projects/{self.project_id}/nodes/{self.node_id}/stop",
    method = "POST",
    response = "Gns3NodeRaw"
)]
pub(crate) struct StopNode {
    #[endpoint(skip)]
    pub project_id: Uuid,
    #[endpoint(skip)]
    pub node_id: Uuid,
}

/// Error thrown by trying to ping a destination
#[derive(Debug, Error)]
pub enum PingError {
    /// Telnet errors
    #[error("Telnet Error: {0}")]
    Telnet(#[from] TelnetError),
    /// Ping was unsuccessful.
    #[error("Ping was unsuccessful:\n{0}")]
    Fail(String),
}

/// Trait that allows us to extend `Result<(), TelnetError>`.
pub trait PingResult {
    fn success(self) -> Result<bool, TelnetError>;
}

impl PingResult for Result<(), PingError> {
    fn success(self) -> Result<bool, TelnetError> {
        match self {
            Ok(()) => Ok(true),
            Err(PingError::Fail(_)) => Ok(false),
            Err(PingError::Telnet(e)) => Err(e),
        }
    }
}

/// Handle that can be used to open the telnet connection. Since `TelnetClient` (and with that also
/// `FrrClient` and `IpTermClient`) do not implement `Sync` and `Send`, this handle exists that can
/// be used to send across thread boundaries before opening the actual telnet session.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TelnetHandle {
    pub(crate) server: String,
    pub(crate) port: u16,
}

impl TelnetHandle {
    /// Create a normal telnet client
    pub fn open(self, prompt: &'static str) -> Result<TelnetClient, TelnetError> {
        TelnetClient::new(self.server, self.port, prompt)
    }

    /// Create a normal telnet client
    pub fn open_frr(self) -> Result<FrrClient, FrrError> {
        FrrClient::new(self.server, self.port)
    }

    /// Create a normal telnet client
    pub fn open_ipterm(self) -> Result<IpTermClient, IpTermError> {
        IpTermClient::new(self.server, self.port)
    }
}
