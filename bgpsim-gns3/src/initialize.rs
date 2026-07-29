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

//! This module contains the initialization functions for the `Gns3Network`.

use std::{
    thread::sleep,
    time::{Duration, Instant},
};

use bgpsim::{
    export::{
        cisco_frr_generators::{Interface, Target},
        Addressor, CiscoFrrCfgGen, ExportError, ExternalCfgGen, InternalCfgGen,
    },
    prelude::*,
};
use ipnet::Ipv4Net;
use itertools::Itertools;
use log::{debug, info, warn};

use crate::{Gns3Network, Gns3NetworkError};

impl<'n, P: Prefix, Q, Ospf: OspfImpl> Gns3Network<'n, P, Q, Ospf> {
    /// Initialize the network by performing the following actions:
    ///
    /// - Create all routers in GNS3 with the specified templates (or the default ones)
    /// - Create all hosts in GNS3 if the user requires it
    /// - Create all links in GNS3
    /// - Start all routers, hosts and links
    /// - Configure all routers and hosts
    pub(crate) fn initialize(&mut self, with_hosts: bool) -> Result<(), Gns3NetworkError> {
        // assign ip addresses in increasing order
        for r in self.net.get_topology().node_indices() {
            self.addressor.router(r)?;
        }

        self.create_routers()?;
        self.create_links()?;
        if with_hosts {
            self.create_hosts()?;
        }
        self.spring_layout()?;
        self.start_all_devices()?;
        sleep(Duration::from_secs(5));
        self.configure_routers()?;
        self.configure_hosts()?;
        sleep(Duration::from_secs(1));
        self.check_adjacent_networks()?;
        self.wait_ospf()?;
        self.wait_bgp()?;

        Ok(())
    }

    /// Create all nodes in GNS3, and populate `self.routers`
    fn create_routers(&mut self) -> Result<(), Gns3NetworkError> {
        info!("Creating routers.");

        for r in self.net.get_topology().node_indices() {
            let node_index = self.project.create_frr_node(r.fmt(self.net), &r)?;
            let info = self.project.get_node(node_index);
            let ifaces = info.interfaces().iter().map(|i| i.name().clone()).collect();
            let gen = CiscoFrrCfgGen::new(self.net, r, Target::Frr, ifaces)?;
            self.routers.insert(r, (node_index, gen));
        }

        Ok(())
    }

    /// Connect all routers according to `self.net`, and populate `self.links`
    fn create_links(&mut self) -> Result<(), Gns3NetworkError> {
        info!("Adding links to connect routers.");

        let topo = self.net.get_topology();

        // iterate over all links in the network
        for e in topo.edge_indices() {
            let (a, b) = topo.edge_endpoints(e).unwrap();
            let link_id = (a, b).into();
            // skip if that link was already created
            if self.links.contains_key(&link_id) {
                continue;
            }
            // get the interface indices for a and b, to assert they are equal. This will also
            // populate the entries in `self.addressor`.
            let iface_a = self.addressor.iface_index(a, b)?;
            let iface_b = self.addressor.iface_index(b, a)?;
            // create the link
            let (gns3_id, iface_a_acq, iface_b_acq) =
                self.project.connect_nodes(self.routers[&a].0, self.routers[&b].0)?;
            // assert that the expected interface index is equal to the acquired one.
            assert_eq!((iface_a, iface_b), (iface_a_acq, iface_b_acq));
            // push the link into the datastructure
            self.links.insert(link_id, gns3_id);
        }

        Ok(())
    }

    /// Create all hosts of every router, and populate `self.hosts`. This function will also create
    /// links connecting the new host to its router.
    fn create_hosts(&mut self) -> Result<(), Gns3NetworkError> {
        info!("Creating hosts.");

        for (r, r_id) in self.routers.iter() {
            let client_id =
                self.project.create_ipterm_node(format!("{}_client", r.fmt(self.net)))?;
            let (link_id, iface_idx, _) = self.project.connect_nodes(r_id.0, client_id)?;
            let net = self.addressor.router_network(*r)?;
            let ip = net.hosts().nth(2).ok_or(ExportError::NotEnoughAddresses)?;
            let gw = net.hosts().nth(1).ok_or(ExportError::NotEnoughAddresses)?;
            self.clients.insert(*r, (client_id, link_id, ip, iface_idx, gw));
        }

        Ok(())
    }

    /// Start all devices
    fn start_all_devices(&mut self) -> Result<(), Gns3NetworkError> {
        info!("Starting devices.");

        self.project.start_all_nodes()?;

        Ok(())
    }

    /// Configure all routers in the network
    fn configure_routers(&mut self) -> Result<(), Gns3NetworkError> {
        info!("Configuring routers.");
        for (r, (r_id, gen)) in self.routers.iter_mut() {
            let cfg = if self.net.get_device(*r).unwrap().is_internal() {
                InternalCfgGen::generate_config(gen, self.net, &mut self.addressor)?
            } else {
                ExternalCfgGen::generate_config(gen, self.net, &mut self.addressor)?
            };
            let mut client = self.project.get_node(*r_id).get_frr_client(&self.server_url)?;
            client.configure(cfg)?;
        }
        // write the config into startup config
        self.send_cmd_all("copy running-config startup-config", Duration::from_secs(5))?;

        Ok(())
    }

    /// Configure all hosts in the network.
    fn configure_hosts(&mut self) -> Result<(), Gns3NetworkError> {
        info!("Configuring {} hosts", self.clients.len());

        for (r, (c_id, _, ip, r_iface_idx, gateway)) in self.clients.iter() {
            let mut client = self.project.get_node(*c_id).get_ipterm_client(&self.server_url)?;
            let mask = self.addressor.router_network(*r)?.netmask();

            client.configure_ip(*ip, mask, *gateway)?;

            // configure the interface on the router
            let (r_id, r_gen) = self.routers.get(r).unwrap();
            let r_iface = r_gen.iface_name(*r_iface_idx)?;
            let link_net = self.addressor.router_network(*r)?;
            let router_ip = Ipv4Net::new(*gateway, link_net.prefix_len()).unwrap();
            let mut iface_cfg = Interface::new(r_iface);
            iface_cfg.ip_address(router_ip);
            if let Some(area) = r_gen.local_area() {
                iface_cfg.cost(1.0);
                iface_cfg.area(area);
            }
            iface_cfg.no_shutdown();

            // push the configuration
            let mut r_client = self.project.get_node(*r_id).get_frr_client(&self.server_url)?;
            r_client.configure(iface_cfg.build(Target::Frr))?;
        }

        Ok(())
    }

    /// Check that all FRR routers see their adjacent networks. Otherwise, they need to be restarted
    fn check_adjacent_networks(&mut self) -> Result<(), Gns3NetworkError> {
        for (r, (r_id, _)) in self.routers.iter() {
            let c_id = self.clients.get(r).and_then(|host| Some(host.0));
            // repeat until the router is started properly
            'inner: loop {
                let mut client = self.project.get_node(*r_id).get_frr_client(&self.server_url)?;
                // get all routes
                let routes = client.get_all_routes()?;
                // drop the client
                drop(client);
                // check that all interfaces of that router are present in the set of routes.
                if !self
                    .addressor
                    .list_ifaces(*r)
                    .iter()
                    .all(|(_, _, net, _)| routes.contains_key(net))
                {
                    // The router does not know the adjacent network. Restart the router
                    warn!(
                        "Router {} has an error in FRR services! Restarting the router...",
                        r.fmt(self.net)
                    );
                    c_id.map(|id| self.project.stop_node(id)).transpose()?;
                    self.project.stop_node(*r_id)?;
                    sleep(Duration::from_millis(500));
                    self.project.start_node(*r_id)?;
                    c_id.map(|id| self.project.start_node(id)).transpose()?;
                    sleep(Duration::from_secs(5));

                    // reconfigure the client if there is one
                    if let Some((c_id, _, ip, _, gw)) = self.clients.get(r) {
                        let mask = self.addressor.router_network(*r)?.netmask();
                        self.project
                            .get_node(*c_id)
                            .get_ipterm_client(&self.server_url)?
                            .configure_ip(*ip, mask, *gw)?;
                    }

                    continue 'inner;
                } else {
                    debug!("FRR services of router {} are running properly.", r.fmt(self.net));
                    break 'inner;
                }
            }
        }

        Ok(())
    }

    const OSPF_CONVERGENCE_TIMEOUT: u64 = 100;

    /// Reset the ospf instance on all routers and wait until we have received some routes.
    fn wait_ospf(&mut self) -> Result<(), Gns3NetworkError> {
        // early exit if ospf is not needed.
        if self.net.internal_indices().count() < 2 {
            return Ok(());
        }

        info!("waiting for OSPF to start communicating...");

        let start_time = Instant::now();

        let internal: Vec<_> = self.net.internal_indices().collect();

        // Loopback address of every internal router, resolved once.
        let mut loopbacks = std::collections::HashMap::new();
        for r in internal.iter().copied() {
            match self.addressor.try_get_router_address(r) {
                Some(addr) => {
                    loopbacks.insert(r, Ipv4Net::new(addr, 32).unwrap());
                }
                None => log::error!("No loopback address found for {}", r.fmt(self.net)),
            }
        }

        while start_time.elapsed().as_secs() < Self::OSPF_CONVERGENCE_TIMEOUT {
            // Collect every router's OSPF route table once per round.
            let mut tables = std::collections::HashMap::new();
            for r in internal.iter().copied() {
                let r_id = self.routers.get(&r).unwrap().0;
                let mut client = self.project.get_node(r_id).get_frr_client(&self.server_url)?;
                tables.insert(r, client.get_ospf_routes()?);
            }

            // The cost a router reports for reaching its *own* loopback. This is not constant
            // across FRR releases: 10.x reports 1, whereas 8.4.2 reports 0. Assuming a fixed offset
            // of 1 makes every route towards an 8.4.2 router look permanently off by one, so the
            // check below never converges. Reading the offset back from each destination keeps this
            // working regardless of which release a given router runs.
            let mut self_cost = std::collections::HashMap::new();
            for r in internal.iter().copied() {
                if let Some(cost) =
                    loopbacks.get(&r).and_then(|lo| tables.get(&r)?.get(lo)).map(|route| route.cost)
                {
                    self_cost.insert(r, cost);
                }
            }

            let mut all_correct = true;

            // check if the OSPF distances have converged.
            'routers: for r in internal.iter().copied() {
                let routes = tables.get(&r).unwrap();

                for other in internal.iter().copied().filter(|x| *x != r) {
                    let Some(lo) = loopbacks.get(&other) else {
                        continue;
                    };
                    // Until the destination advertises its own loopback we cannot know the offset,
                    // which simply means OSPF has not settled yet.
                    let Some(offset) = self_cost.get(&other).copied() else {
                        log::debug!(
                            "{} does not yet advertise its own loopback ({lo})",
                            other.fmt(self.net)
                        );
                        all_correct = false;
                        break 'routers;
                    };
                    let want = self
                        .net
                        .get_internal_router(r)
                        .unwrap()
                        .ospf
                        .get_cost(other)
                        .map(|x| x.round() as u32 + offset);
                    let got = routes.get(lo).map(|x| x.cost);
                    if want != got {
                        log::debug!("IGP distance from {} to {} ({lo}) is not yet as expected: got {got:?}, want: {want:?}", r.fmt(self.net), other.fmt(self.net));
                        all_correct = false;
                        break 'routers;
                    }
                }
            }

            if all_correct {
                return Ok(());
            }

            std::thread::sleep(Duration::from_secs(1))
        }

        log::error!("OSPF tables are not equal after {} seconds.", Self::OSPF_CONVERGENCE_TIMEOUT);
        Err(Gns3NetworkError::NoConvergence)
    }

    /// Reset the bgp instance on all routers and wait until we have received some routes.
    fn wait_bgp(&mut self) -> Result<(), Gns3NetworkError> {
        info!("resetting the BGP instance.");
        self.send_cmd_all("clear bgp *", Duration::from_secs(1))?;

        // wait until all BGP sessions are established
        info!("wait until all BGP sessions are established");
        for r in self.net.internal_indices() {
            let r_id = self.routers.get(&r).unwrap().0;
            let mut client = self.project.get_node(r_id).get_frr_client(&self.server_url)?;
            let mut counter = 0;
            'inner: loop {
                let sessions = client.get_bgp_neighbors()?;
                if sessions.values().all(|s| s.is_established()) {
                    break 'inner;
                }
                counter += 1;
                if counter % 10 == 0 {
                    info!(
                        "Router {} cannot establish sessions towards {}.",
                        r.fmt(self.net),
                        sessions
                            .iter()
                            .filter(|(_, s)| !s.is_established())
                            .map(|(n, _)| self
                                .addressor
                                .find_address(*n)
                                .map(|x| x.fmt(self.net).to_string())
                                .unwrap_or_else(|_| n.to_string()))
                            .join(", ")
                    );
                    client.send_cmd("clear bgp *", Duration::from_secs(1))?;
                }
                sleep(Duration::from_secs(1))
            }
            if counter > 10 {
                info!(
                    "All BGP sessions of {} are established after restarting BGP!",
                    r.fmt(self.net)
                );
            } else {
                debug!("All BGP sessions of {} are established!", r.fmt(self.net));
            }
        }

        Ok(())
    }
}
