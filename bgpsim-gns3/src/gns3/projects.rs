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

//! Module containing the main GNS3Project

use std::{collections::HashMap, fmt::Debug, path::PathBuf};

use bgpsim::types::RouterId;
use log::warn;
use rustify::blocking::clients::reqwest::Client;
use rustify_derive::Endpoint;
use serde::Deserialize;
use uuid::Uuid;

use crate::gns3::templates::{get_or_create_frr_template, get_or_create_ipterm_template};

const DEFAULT_STYLE: &str =
    "font-family: TypeWriter;font-size: 10.0;font-weight: bold;fill: #000000;fill-opacity: 1.0;";

use super::{
    links::{
        ConnectTowNodes, GetLinkInfo, Gns3LinkEndpoint, Gns3LinkFilters, Gns3LinkIndex,
        SetLinkFilters, StartCapture, StopCapture,
    },
    nodes::{
        CreateNodeFromTemplate, GetNodeInfo, Gns3Node, Gns3NodeIndex, Gns3NodeLabel, SetNodeLabel,
        SetNodeName, SetNodePos, StartNode, StopNode, TelnetHandle,
    },
    Gns3Error, LoggingClient,
};

/// This is the main datastructure to interact with the GNS3 project. It allows operation on that
/// project. As long as an instance of this is owned, the project is running on the server.
#[allow(clippy::type_complexity)]
pub struct Gns3Project {
    /// Server URL or IP address, defaults to `localhost`.
    server_url: String,
    /// Port of the server, defaults to 3080.
    server_port: u16,
    /// Name of the project
    name: String,
    /// Project UUID
    id: Uuid,
    /// the client used to communicate with the server
    client: Client,
    /// a HashMap of router templates to use for each router, if blank then just use the default FRR image
    frr_templates: HashMap<RouterId, Uuid>,
    frr_default: Uuid,
    /// The ID of the Ipterm template
    ipterm_template: Uuid,
    /// Nodes in the network
    nodes: HashMap<Gns3NodeIndex, Gns3Node>,
    /// Links in the network
    links: HashMap<Gns3LinkIndex, ((Gns3NodeIndex, usize), (Gns3NodeIndex, usize))>,
    /// Variable to control wether to delete the project after dropping
    keep_open: bool,
    /// This project's snapshot UUID.
    // TODO: extend this to hold multiple snapshots or return UUIDs from the utility function
    snapshot: Option<Uuid>,
}

impl Gns3Project {
    /// Create a new project
    pub fn new(
        name: impl Into<String>,
        server_url: Option<String>,
        server_port: Option<u16>,
        router_templates: HashMap<RouterId, (&str, &str)>,
    ) -> Result<Self, Gns3Error> {
        let name = name.into();
        let server_url = server_url.unwrap_or_else(|| String::from("localhost"));
        let server_port = server_port.unwrap_or(3080);
        let client = Client::default(&format!("http://{server_url}:{server_port}/v2"));

        // create the project
        let result = client.exec(CreateProject { name: name.clone() })?;
        let id = result.project_id;

        // open the project
        let result = client.exec(OpenProject { project_id: id })?;

        assert_eq!(result.project_id, id);
        assert!(result.status.is_open());

        // Create the templates
        let mut frr_templates = HashMap::new();
        for (r_id, template) in router_templates {
            frr_templates.insert(r_id, get_or_create_frr_template(&client, Some(template))?);
        }
        let frr_default = get_or_create_frr_template(&client, None)?;
        let ipterm_template = get_or_create_ipterm_template(&client)?;

        Ok(Self {
            server_url,
            server_port,
            name,
            id,
            client,
            frr_templates,
            frr_default,
            ipterm_template,
            nodes: Default::default(),
            links: Default::default(),
            keep_open: false,
            snapshot: None,
        })
    }

    /// Get the `TelnetHandle` for a given node
    pub fn telnet(&self, id: Gns3NodeIndex) -> TelnetHandle {
        TelnetHandle { server: self.server_url.clone(), port: self.get_node(id).port() }
    }

    /// If you call this function with the argument `true`, then the project will be kept open, and
    /// not deleted when the project is dropped.
    pub fn keep_open(&mut self, val: bool) {
        self.keep_open = val;
    }

    /// Wait for a short amount of time
    fn sleep(ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }

    /// Get the reference to a node in the Network.
    pub fn get_node(&self, id: Gns3NodeIndex) -> &Gns3Node {
        self.nodes.get(&id).unwrap()
    }

    /// Get the links connecting two nodes
    pub fn get_links_connecting(&self, a: Gns3NodeIndex, b: Gns3NodeIndex) -> Vec<Gns3LinkIndex> {
        self.links
            .iter()
            .filter(|(_, ((ax, _), (bx, _)))| *ax == a && *bx == b || *ax == b && *bx == a)
            .map(|(id, _)| id)
            .copied()
            .collect()
    }

    /// Create an FRR router under the given name
    pub fn create_frr_node(
        &mut self,
        name: impl Into<String>,
        router_id: &RouterId,
    ) -> Result<Gns3NodeIndex, Gns3Error> {
        let name = name.into();
        let template_id = *self.frr_templates.get(router_id).unwrap_or(&self.frr_default);
        let node: Gns3Node = self
            .client
            .exec(CreateNodeFromTemplate {
                project_id: self.id,
                template_id,
                name: name.clone(),
                x: 0,
                y: 0,
            })?
            .into();

        Self::sleep(1);

        let node: Gns3Node = self
            .client
            .exec(SetNodeName { project_id: self.id, node_id: node.node_id(), name })?
            .into();

        let index = node.index();
        self.nodes.insert(index, node);
        Ok(index)
    }

    /// Create an IPTerm client under the given name
    pub fn create_ipterm_node(
        &mut self,
        name: impl Into<String>,
    ) -> Result<Gns3NodeIndex, Gns3Error> {
        let name = name.into();
        let node: Gns3Node = self
            .client
            .exec(CreateNodeFromTemplate {
                project_id: self.id,
                template_id: self.ipterm_template,
                name: name.clone(),
                x: 0,
                y: 0,
            })?
            .into();

        Self::sleep(1);

        let node: Gns3Node = self
            .client
            .exec(SetNodeName { project_id: self.id, node_id: node.node_id(), name })?
            .into();

        let index = node.index();
        self.nodes.insert(index, node);
        Ok(index)
    }

    /// Connect two nodes together, and return the Link index, together with interface index of `a`
    /// and `b`.
    pub fn connect_nodes(
        &mut self,
        a: Gns3NodeIndex,
        b: Gns3NodeIndex,
    ) -> Result<(Gns3LinkIndex, usize, usize), Gns3Error> {
        let a_node = self.nodes.get(&a).unwrap();
        let b_node = self.nodes.get(&b).unwrap();

        let a_iface = a_node
            .interfaces()
            .iter()
            .enumerate()
            .find(|(_, x)| !x.is_connected())
            .ok_or(Gns3Error::NotEnoughInterfaces)?;

        let b_iface = b_node
            .interfaces()
            .iter()
            .enumerate()
            .find(|(_, x)| !x.is_connected())
            .ok_or(Gns3Error::NotEnoughInterfaces)?;

        // send the command
        let link = self.client.exec(ConnectTowNodes {
            project_id: self.id,
            nodes: vec![
                Gns3LinkEndpoint {
                    node_id: a_node.node_id(),
                    adapter_number: a_iface.1.adapter_number(),
                    port_number: a_iface.1.port_number(),
                },
                Gns3LinkEndpoint {
                    node_id: b_node.node_id(),
                    adapter_number: b_iface.1.adapter_number(),
                    port_number: b_iface.1.port_number(),
                },
            ],
        })?;

        let a_iface_idx = a_iface.0;
        let b_iface_idx = b_iface.0;

        // update the interface stuff
        self.nodes
            .get_mut(&a)
            .unwrap()
            .interfaces_mut()
            .get_mut(a_iface_idx)
            .unwrap()
            .set_connected_to(Some((b, b_iface_idx)));
        self.nodes
            .get_mut(&b)
            .unwrap()
            .interfaces_mut()
            .get_mut(b_iface_idx)
            .unwrap()
            .set_connected_to(Some((a, a_iface_idx)));

        // add the link to the structure
        self.links.insert(link.index(), ((a, a_iface_idx), (b, b_iface_idx)));
        Ok((link.index(), a_iface_idx, b_iface_idx))
    }

    /// Get all link filters
    pub fn get_link_filters(&self, link: Gns3LinkIndex) -> Result<Gns3LinkFilters, Gns3Error> {
        Ok(self
            .client
            .exec(GetLinkInfo { project_id: self.id, link_id: link.id() })?
            .filters
            .unwrap_or_default())
    }

    /// Get all link filters
    pub fn set_link_filters(
        &self,
        link: Gns3LinkIndex,
        filters: Gns3LinkFilters,
    ) -> Result<(), Gns3Error> {
        self.client.exec(SetLinkFilters { project_id: self.id, link_id: link.id(), filters })?;
        Ok(())
    }

    /// Start all nodes in the project.
    pub fn start_all_nodes(&self) -> Result<(), Gns3Error> {
        self.client.exec_no_content(StartAllNodes { project_id: self.id })?;
        Ok(())
    }

    /// Start all nodes in the project.
    pub fn stop_all_nodes(&self) -> Result<(), Gns3Error> {
        self.client.exec_no_content(StopAllNodes { project_id: self.id })?;
        Ok(())
    }

    /// Start a node in Gns3
    pub fn start_node(&self, node: Gns3NodeIndex) -> Result<(), Gns3Error> {
        self.client.exec(StartNode { project_id: self.id, node_id: node.id() })?;
        Ok(())
    }

    /// stop a node in Gns3
    pub fn stop_node(&self, node: Gns3NodeIndex) -> Result<(), Gns3Error> {
        self.client.exec(StopNode { project_id: self.id, node_id: node.id() })?;
        Ok(())
    }

    pub fn node_running(&self, node: Gns3NodeIndex) -> Result<bool, Gns3Error> {
        let info = self.client.exec(GetNodeInfo { project_id: self.id, node_id: node.id() })?;
        Ok(info.is_running())
    }

    /// Start the capture on a specific link and return the path to the capture file (`pcap`). If a
    /// capture is already running, then don't do anything and simply return the path to the capture
    /// file.
    ///
    /// For the capture to work, at least one of the endpoint nodes must be started.
    pub fn start_capture(&self, link: Gns3LinkIndex) -> Result<PathBuf, Gns3Error> {
        let info = self.client.exec(GetLinkInfo { project_id: self.id, link_id: link.id() })?;
        if info.capturing {
            if let Some(path) = info.capture_file_path {
                Ok(path)
            } else {
                Err(Gns3Error::ApiError(rustify::errors::ClientError::ResponseError {
                    source: anyhow::anyhow!("Capture is running but there is no file path"),
                }))
            }
        } else {
            let info =
                self.client.exec(StartCapture { project_id: self.id, link_id: link.id() })?;
            Ok(info.capture_file_path.unwrap())
        }
    }

    /// Stop the capture on a specific link. If the link was being captured before calling this function,
    /// then this function will return `Ok(Some(PATH_TO_PCAP))`. Otherwise, it will do nothing and
    /// return `Ok(None)`.
    pub fn stop_capture(&self, link: Gns3LinkIndex) -> Result<Option<PathBuf>, Gns3Error> {
        let info = self.client.exec(GetLinkInfo { project_id: self.id, link_id: link.id() })?;
        if info.capturing {
            if let Some(path) = info.capture_file_path {
                self.client.exec(StopCapture { project_id: self.id, link_id: link.id() })?;
                Ok(Some(path))
            } else {
                Err(Gns3Error::ApiError(rustify::errors::ClientError::ResponseError {
                    source: anyhow::anyhow!("Capture is running but there is no file path"),
                }))
            }
        } else {
            Ok(None)
        }
    }

    /// Set the position of a specific node in the network.
    pub fn set_node_pos(
        &mut self,
        node: Gns3NodeIndex,
        x: isize,
        y: isize,
    ) -> Result<(), Gns3Error> {
        let data: Gns3Node =
            self.client.exec(SetNodePos { project_id: self.id, node_id: node.id(), x, y })?.into();
        self.nodes.insert(node, data);
        Ok(())
    }

    /// Set the position of a specific node in the network.
    pub fn hide_node_label(&mut self, node: Gns3NodeIndex) -> Result<(), Gns3Error> {
        let name = self.nodes.get(&node).map(|x| x.name().clone()).unwrap_or_default();
        let data: Gns3Node = self
            .client
            .exec(SetNodeLabel {
                project_id: self.id,
                node_id: node.id(),
                label: Gns3NodeLabel {
                    text: name,
                    y: Some(10000),
                    x: Some(10000),
                    rotation: Some(0),
                    style: Some(DEFAULT_STYLE.to_string()),
                },
            })?
            .into();
        self.nodes.insert(node, data);
        Ok(())
    }

    /// Take a snapshot of the current network.
    /// Only one snapshot at a time is supported for now, taking another one will overwrite the existing one.
    pub fn take_snapshot(&mut self) -> Result<(), Gns3Error> {
        let data: CreateSnapshotData = self
            .client
            .exec(CreateSnapshot { project_id: self.id, name: "snapshot".to_string() })?
            .into();
        if self.snapshot.is_some() {
            warn!("Overwriting existing snapshot");
        }
        self.snapshot = Some(data.snapshot_id);
        Ok(())
    }

    /// Restore the network to an existing snapshot
    pub fn restore_snapshot(&mut self) -> Result<(), Gns3Error> {
        if let Some(snapshot_id) = self.snapshot {
            self.client.exec(RestoreSnapshot { project_id: self.id, snapshot_id })?;
        } else {
            return Err(Gns3Error::NoSnapshotFound);
        }

        Ok(())
    }
}

impl Drop for Gns3Project {
    fn drop(&mut self) {
        if !self.keep_open {
            // close the project and ignore all attributes.
            let _ = self.client.exec_no_content(CloseProject { project_id: self.id });
            let _ = self.client.exec_no_content(DeleteProject { project_id: self.id });
        }
    }
}

impl std::fmt::Debug for Gns3Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gns3Project")
            .field("server_url", &self.server_url)
            .field("server_port", &self.server_port)
            .field("name", &self.name)
            .field("id", &self.id)
            .finish()
    }
}

#[derive(Debug, Endpoint)]
#[endpoint(path = "projects", method = "POST", response = "CreateProjectData")]
pub struct CreateProject {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateProjectData {
    pub name: String,
    pub project_id: Uuid,
}

#[derive(Debug, Endpoint)]
#[endpoint(path = "projects/{self.project_id}/open", method = "POST", response = "ProjectInfo")]
pub struct OpenProject {
    #[endpoint(skip)]
    pub project_id: Uuid,
}

#[derive(Debug, Endpoint)]
#[endpoint(path = "projects/{self.project_id}/close", method = "POST", response = "ProjectInfo")]
pub struct CloseProject {
    #[endpoint(skip)]
    pub project_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct ProjectInfo {
    /// Project auto close when client cut off the notifications feed
    pub auto_close: bool,
    /// Project open when GNS3 start
    pub auto_open: bool,
    /// Project start when opened
    pub auto_start: bool,
    /// Grid size for the drawing area for drawings
    pub drawing_grid_size: isize,
    /// Project filename
    pub filename: String,
    /// Grid size for the drawing area for nodes
    pub grid_size: isize,
    /// Project name
    pub name: String,
    /// Project directory
    pub path: String,
    /// Project UUID
    pub project_id: Uuid,
    /// Height of the drawing area
    pub scene_height: isize,
    /// Width of the drawing area
    pub scene_width: isize,
    /// Show the grid on the drawing area
    pub show_grid: bool,
    /// Show interface labels on the drawing area
    pub show_interface_labels: bool,
    /// Show layers on the drawing area
    pub show_layers: bool,
    /// Snap to grid on the drawing area
    pub snap_to_grid: bool,
    /// Possible values: opened, closed
    pub status: ProjectStatus,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    Opened,
    Closed,
}

impl ProjectStatus {
    pub fn is_open(&self) -> bool {
        match self {
            ProjectStatus::Opened => true,
            ProjectStatus::Closed => false,
        }
    }
}

#[derive(Debug, Endpoint)]
#[endpoint(path = "projects/{self.project_id}", method = "DELETE")]
pub struct DeleteProject {
    #[endpoint(skip)]
    pub project_id: Uuid,
}

#[derive(Debug, Endpoint)]
#[endpoint(path = "projects/{self.project_id}/nodes/start", method = "POST")]
pub struct StartAllNodes {
    #[endpoint(skip)]
    pub project_id: Uuid,
}

#[derive(Debug, Endpoint)]
#[endpoint(path = "projects/{self.project_id}/nodes/stop", method = "POST")]
pub struct StopAllNodes {
    #[endpoint(skip)]
    pub project_id: Uuid,
}

#[derive(Debug, Endpoint)]
#[endpoint(
    path = "projects/{self.project_id}/snapshots",
    method = "POST",
    response = "CreateSnapshotData"
)]
pub struct CreateSnapshot {
    #[endpoint(skip)]
    pub project_id: Uuid,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateSnapshotData {
    pub created_at: isize,
    pub name: String,
    pub project_id: Uuid,
    pub snapshot_id: Uuid,
}

#[derive(Debug, Endpoint)]
#[endpoint(
    path = "projects/{self.project_id}/snapshots/{self.snapshot_id}/restore",
    method = "POST",
    response = "ProjectInfo"
)]
pub struct RestoreSnapshot {
    #[endpoint(skip)]
    pub project_id: Uuid,
    #[endpoint(skip)]
    pub snapshot_id: Uuid,
}
