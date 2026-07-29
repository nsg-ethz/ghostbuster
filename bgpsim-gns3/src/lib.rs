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

#![deny(missing_debug_implementations)]
#![doc(html_logo_url = "https://iospf.tibors.ch/images/bgpsim/dark_only.svg")]

//! # GNS3 interface for `bgpsim`.
//!
//! This library creates a replica of a [`bgpsim::network::Network`] inside of GNS3, and allows you
//! to perform operations on that gns3 network.
//!
//! Make sure to have `gns3-server` installed, and make sure that `gns3server` is running locally,
//! on port 3080. Also, make sure that the Docker Daemon is running.
//!
//! Finally, install the frr image as follows:
//!
//! ```sh
//! docker build -t frr docker-frr
//! ```

use std::{any::Any, collections::HashMap, net::Ipv4Addr};

use bgpsim::{
    export::{CiscoFrrCfgGen, DefaultAddressor, DefaultAddressorBuilder, ExportError, LinkId},
    prelude::*,
};
use gns3::{
    links::Gns3LinkIndex,
    nodes::{frr::FrrError, ipterm::IpTermError, Gns3NodeIndex, TelnetError},
    Gns3Error, Gns3Project,
};
use thiserror::Error;

pub mod convergence;
mod forwarding_state;
mod geo;
pub mod gns3;
mod initialize;
#[cfg(feature = "logging")]
pub mod logger;
#[cfg(feature = "parsing")]
pub mod parser;
mod reachability;
pub mod routing_state;
#[cfg(test)]
mod test;
mod updates;
mod utils;

/// This is the main datastructure of this crate. It represents a Gns3 Network that was created form
/// a [`bgpsim::network::Network`].
#[derive(Debug)]
pub struct Gns3Network<'n, P: Prefix, Q, Ospf: OspfImpl> {
    /// The reference to the original network, used to keep track of things.
    net: &'n Network<P, Q, Ospf>,
    /// The owned GNS3 project. As soon as this Gns3 Network goes out of scope, the project is also
    /// destroyed.
    project: Gns3Project,
    /// Addressor that assigns IP addresses and interface indices to every node.
    addressor: DefaultAddressor<'n, P, Q, Ospf>,
    /// Datastructure storing the GNS3 ID and the config generator for each router
    routers: HashMap<RouterId, (Gns3NodeIndex, CiscoFrrCfgGen<P>)>,
    /// Datastructure storing all clients. This structure also stores the link that connects that
    /// client to its router, along with the IP of the client, and the interface index of the router.
    clients: HashMap<RouterId, (Gns3NodeIndex, Gns3LinkIndex, Ipv4Addr, usize, Ipv4Addr)>,
    /// Datastructure remembering all links
    links: HashMap<LinkId, Gns3LinkIndex>,
    /// server_url
    server_url: String,
}

impl<'n, P: Prefix, Q, Ospf: OspfImpl> Gns3Network<'n, P, Q, Ospf> {
    /// Generate a new GNS3 network, setting up the GNS3 project, adding all necessary routers and
    /// links, and configuring the network properly.
    /// One can specify specific router images to use for certain routers using the templates HashMap.
    /// The format is RouterId: (template_name, image_name)
    pub fn new(
        name: impl Into<String>,
        net: &'n Network<P, Q, Ospf>,
        server_url: Option<String>,
        server_port: Option<u16>,
        with_hosts: bool,
        router_templates: HashMap<RouterId, (&str, &str)>,
    ) -> Result<Self, Gns3NetworkError> {
        let mut s = Self {
            net,
            project: Gns3Project::new(name, server_url.clone(), server_port, router_templates)?,
            addressor: DefaultAddressorBuilder {
                internal_ip_range: "11.0.0.0/8".parse().unwrap(),
                external_ip_range: "22.0.0.0/8".parse().unwrap(),
                ..Default::default()
            }
            .build(net)?,
            routers: Default::default(),
            links: Default::default(),
            clients: Default::default(),
            server_url: server_url.unwrap_or_else(|| String::from("localhost")),
        };

        s.initialize(with_hosts)?;

        Ok(s)
    }
}

/// Error kind thrown by the `bgpsim-gns3` crate.
#[derive(Debug, Error)]
pub enum Gns3NetworkError {
    /// Error from GNS3, or from the communication with it.
    #[error("Gns3 Error: {0}")]
    Gns3(#[from] Gns3Error),
    /// Error from exporting things from `bgpsim` into actual configurations.
    #[error("Export Error: {0}")]
    Export(#[from] ExportError),
    /// Error from bgpsim itself
    #[error("Bgpsim Error: {0}")]
    Network(#[from] NetworkError),
    /// Error while communicating with an IpTerm client
    #[error("Error while communicating with an IpTerm client: {0}")]
    IpTermClient(#[from] IpTermError),
    /// Error while communicating with an FRR client
    #[error("Error while communicating with an FRR client: {0}")]
    FrrClient(#[from] FrrError),
    /// Telnet Error
    #[error("TelnetError: {0}")]
    Telnet(#[from] TelnetError),
    /// Network did not converge.
    #[error("Network did not converge!")]
    NoConvergence,
    /// Error while joining threasd
    #[error("Error while joining threads: {0:?}")]
    ThreadError(Box<dyn Any + Send + 'static>),
}
