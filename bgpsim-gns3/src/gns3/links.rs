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

//! This module contains all code to create and manage Links in GNS3

use std::path::PathBuf;

use getset::CopyGetters;
use rustify_derive::Endpoint;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(CopyGetters, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Gns3LinkIndex {
    #[getset(get_copy = "pub")]
    id: Uuid,
}

#[derive(Debug, Endpoint)]
#[endpoint(path = "projects/{self.project_id}/links", method = "POST", response = "Gns3Link")]
pub struct ConnectTowNodes {
    #[endpoint(skip)]
    pub project_id: Uuid,
    pub nodes: Vec<Gns3LinkEndpoint>,
}

#[derive(Debug, Endpoint)]
#[endpoint(
    path = "projects/{self.project_id}/links/{self.link_id}",
    method = "PUT",
    response = "Gns3Link"
)]
pub struct SetLinkFilters {
    #[endpoint(skip)]
    pub project_id: Uuid,
    #[endpoint(skip)]
    pub link_id: Uuid,
    pub filters: Gns3LinkFilters,
}

#[derive(Debug, Endpoint)]
#[endpoint(
    path = "projects/{self.project_id}/links/{self.link_id}",
    method = "GET",
    response = "Gns3Link"
)]
pub struct GetLinkInfo {
    #[endpoint(skip)]
    pub project_id: Uuid,
    #[endpoint(skip)]
    pub link_id: Uuid,
}

#[derive(Debug, Endpoint)]
#[endpoint(
    path = "projects/{self.project_id}/links/{self.link_id}/start_capture",
    method = "POST",
    response = "Gns3Link"
)]
pub struct StartCapture {
    #[endpoint(skip)]
    pub project_id: Uuid,
    #[endpoint(skip)]
    pub link_id: Uuid,
}

#[derive(Debug, Endpoint)]
#[endpoint(
    path = "projects/{self.project_id}/links/{self.link_id}/stop_capture",
    method = "POST",
    response = "Gns3Link"
)]
pub struct StopCapture {
    #[endpoint(skip)]
    pub project_id: Uuid,
    #[endpoint(skip)]
    pub link_id: Uuid,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Gns3LinkEndpoint {
    pub node_id: Uuid,
    pub adapter_number: usize,
    pub port_number: usize,
}

#[derive(Debug, Deserialize)]
pub struct Gns3Link {
    pub link_id: Uuid,
    pub project_id: Uuid,
    pub capture_compute_id: Option<String>,
    pub capture_file_name: Option<String>,
    pub capture_file_path: Option<PathBuf>,
    pub capturing: bool,
    pub nodes: (Gns3LinkEndpoint, Gns3LinkEndpoint),
    pub filters: Option<Gns3LinkFilters>,
}

impl Gns3Link {
    pub fn index(&self) -> Gns3LinkIndex {
        Gns3LinkIndex { id: self.link_id }
    }
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Gns3LinkFilters {
    /// Delay in milliseconds, where the first element is the expected delay, and the second one is
    /// the variance (jitter).
    pub delay: Option<(u64, u64)>,
    /// Percentage (0 to 100) to corrupt a packet
    pub corrupt: Option<u64>,
    /// Drop every nth packet
    pub frequency_drop: Option<u64>,
    /// Packet loss as a percentage (0 to 100)
    pub packet_loss: Option<u64>,
}
