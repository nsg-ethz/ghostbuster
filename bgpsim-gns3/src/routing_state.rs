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

//! This module contains implementations to get the routing state of the network.

use std::collections::{BTreeSet, HashMap, HashSet};

use bgpsim::{
    bgp::BgpRibEntry,
    export::{Addressor, ExportError},
    prelude::*,
    types::PrefixMap,
};
use ipnet::Ipv4Net;
use itertools::Itertools;
use log::{debug, info};
use ordered_float::NotNan;
use serde::{Deserialize, Serialize};

use crate::{gns3::nodes::frr::BgpPath, Gns3Network, Gns3NetworkError};

impl<'n, P: Prefix, Q, Ospf: OspfImpl> Gns3Network<'n, P, Q, Ospf> {
    /// Get all OSPF routes of a single router. The resulting hashmap contains, for each router, a
    /// set of next-hops to route traffic towards it.
    pub fn get_ospf_routes(
        &mut self,
        router: RouterId,
    ) -> Result<HashMap<RouterId, Vec<RouterId>>, Gns3NetworkError> {
        let (r_id, gen) = self.routers.get(&router).ok_or(NetworkError::DeviceNotFound(router))?;
        let mut client = self.project.get_node(*r_id).get_frr_client(&self.server_url)?;

        let mut result: HashMap<RouterId, BTreeSet<RouterId>> = HashMap::new();

        let internal_routers_net = self.addressor.subnet_for_internal_routers();
        let external_links_net = self.addressor.subnet_for_external_links();

        for (net, route) in client.get_ospf_routes()?.into_iter() {
            // only take those networks within the internal network, or the external links
            if !(internal_routers_net.contains(&net) || external_links_net.contains(&net)) {
                continue;
            }

            // what is left are either networks of internal routers, or of links to external ones
            let target = self.addressor.find_address(net)?;
            // get the next-hops
            let nhs = result.entry(target).or_default();
            for iface in route.next_hops.iter().map(|x| &x.interface) {
                nhs.insert(self.addressor.find_neighbor(router, gen.iface_idx(iface)?)?);
            }
        }

        Ok(result.into_iter().map(|(r, nhs)| (r, Vec::from_iter(nhs))).collect())
    }

    /// Get the list of available BGP routes for a specific `router` and `perfix`. This function
    /// only works on internal routers! For each actual network address in the prefix equivalence
    /// class of `prefix`, this function will return the known routes and wether or not they are selected
    pub fn get_bgp_routes_for_prefix(
        &mut self,
        router: RouterId,
        prefix: P,
    ) -> Result<Vec<(BgpRibEntry<P>, bool)>, Gns3NetworkError> {
        self.net.get_device(router)?.internal_or_err()?;
        let mut client = self.get_frr(router)?;
        let net = self.addressor.prefix(prefix)?.unwrap_single();
        Ok(if let Some(route) = client.get_bgp_routes_for_prefix(net)? {
            route
                .iter()
                .map(|path| {
                    self.transform_bgp_route(route.prefix, path)
                        .map(|entry| (entry, path.best_path))
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        })
    }

    /// Get the selected BGP route for a specific `router` and `perfix`. This function only works on
    /// internal routers! This function will find the selected route for each network address that
    /// is associated with that prefix equivalence class.
    pub fn get_selected_bgp_route_for_prefix(
        &mut self,
        router: RouterId,
        prefix: P,
    ) -> Result<Option<BgpRibEntry<P>>, Gns3NetworkError> {
        self.net.get_device(router)?.internal_or_err()?;
        let mut client = self.get_frr(router)?;
        let net = self.addressor.prefix(prefix)?.unwrap_single();
        Ok(if let Some(route) = client.get_bgp_routes_for_prefix(net)? {
            Some(self.transform_bgp_route(route.prefix, route.selected())?)
        } else {
            None
        })
    }

    /// Transform a BGP Path to a `bgpsim::bgp::BgpRoute`.
    pub fn transform_bgp_route(
        &self,
        prefix: Ipv4Net,
        route: &BgpPath,
    ) -> Result<BgpRibEntry<P>, Gns3NetworkError> {
        Ok(BgpRibEntry {
            route: bgpsim::bgp::BgpRoute {
                prefix: P::from(prefix),
                as_path: route.as_path.clone(),
                next_hop: self.addressor.find_address(route.next_hop.ip)?,
                local_pref: route.local_pref,
                med: route.med,
                community: route.communities.iter().map(|(_, x)| *x).collect(),
                originator_id: route
                    .originator_id
                    .map(|x| self.addressor.find_address(x))
                    .transpose()?,
                cluster_list: route
                    .cluster_list
                    .iter()
                    .map(|x| self.addressor.find_address(*x))
                    .collect::<Result<_, ExportError>>()?,
            },
            from_type: route.peer.peer_type.into(),
            from_id: self.addressor.find_address(route.peer.router_id)?,
            to_id: None,
            igp_cost: route.next_hop.igp_cost.map(|x| NotNan::new(x as f64).unwrap()),
            weight: route.weight.map(|x| x as u32).unwrap_or(100),
        })
    }

    /// Assert that all selected routes are as described in `net`.
    pub fn equal_bgp_state(&mut self, net: &Network<P, Q>) -> Result<bool, Gns3NetworkError> {
        for router in self.net.internal_indices() {
            for prefix in self.net.get_known_prefixes().chain(net.get_known_prefixes()).copied() {
                let exp = net.get_device(router)?.unwrap_internal().bgp.get_exact(prefix);
                let acq = self.get_selected_bgp_route_for_prefix(router, prefix)?;
                if exp == acq.as_ref() {
                    debug!(
                        "The selected BGP route for {} and {} is as expected!",
                        router.fmt(net),
                        prefix
                    );
                } else {
                    info!(
                        "Expected a different selected BGP route of {} and {}:\nexp: {}\nacq: {}",
                        router.fmt(net),
                        prefix,
                        exp.map(|x| x.fmt(net)).unwrap_or_default(),
                        acq.map(|x| x.fmt(net)).unwrap_or_default()
                    );
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Assert that, for every router in both networks, the two RIB-Ins contain the same entries for a given set of prefixes.
    /// Furthermore, both RIBs have selected the same best route.
    ///
    /// **WARNING**: This function has a bit of a hack in it. The IGP of the GNS3 routes is manually reduced by one
    pub fn compare_bgp_tables(
        &mut self,
        net: &Network<P, Q>,
        prefixes: &HashSet<P>,
    ) -> Result<BgpTableDiffs<P>, Gns3NetworkError> {
        let mut diffs = HashMap::new();

        for router in self.net.internal_indices() {
            let rib_in = net.get_device(router)?.unwrap_internal().bgp.get_processed_rib_in();

            let mut router_diffs = HashMap::new();
            for prefix in prefixes {
                // Get the expected rib entries for this prefix
                let exp_rib_in = rib_in.get(&prefix).map(Vec::as_slice).unwrap_or(&[]);

                // Get the GNS3 rib entries for this prefix
                let mut acq_rib_in = self.get_bgp_routes_for_prefix(router, *prefix)?;
                // WARN: This is where the hack is applied
                for (entry, _) in acq_rib_in.iter_mut() {
                    // Skip the reduction for externally learned routes
                    if entry.from_type == BgpSessionType::EBgp {
                        continue;
                    }
                    if let Some(cost) = entry.igp_cost.as_mut() {
                        *cost = NotNan::new(cost.into_inner() - 1.0).unwrap();
                    }
                }

                if !equal_bgp_tables(exp_rib_in.iter(), acq_rib_in.iter()) {
                    router_diffs.insert(
                        *prefix,
                        BgpTableDiff {
                            bgpsim_rib: exp_rib_in.iter().cloned().sorted().collect(),
                            gns3_rib: acq_rib_in.into_iter().sorted().collect(),
                        },
                    );
                } else {
                    debug!(
                        "The RIB entries of {} for {} are as expected!",
                        router.fmt(net),
                        prefix
                    );
                }
            }
            // Only insert this router if there are any diffs
            if !router_diffs.is_empty() {
                diffs.insert(router, router_diffs);
            }
        }
        Ok(diffs)
    }
}

/// Compares the entries of the RIB-Ins and the selected routes for two BGP tables.
fn equal_bgp_tables<'a, P, E>(this_rib: E, other_rib: E) -> bool
where
    E: Iterator<Item = &'a (BgpRibEntry<P>, bool)> + ExactSizeIterator,
    P: Prefix,
{
    // Get the set of RIB in entries but only the selected route
    let (t_rib_in, t_selected) = to_rib_in_split(this_rib);
    let (o_rib_in, o_selected) = to_rib_in_split(other_rib);
    // First, compare the two RIB-Ins, they should have the same entries.
    // Then, compare the two selected routes, they should be the same.
    // The selected routes can come from different entries
    t_rib_in == o_rib_in && t_selected == o_selected
}

/// Helper function to convert the content of a RIB-In for easier comparison
fn to_rib_in_split<'a, P, E>(rib: E) -> (HashSet<&'a BgpRibEntry<P>>, Option<&'a BgpRoute<P>>)
where
    E: Iterator<Item = &'a (BgpRibEntry<P>, bool)> + ExactSizeIterator,
    P: Prefix,
{
    let expected_len = rib.len();

    let (set, selected) = rib.fold((HashSet::new(), None), |(mut set, selected), entry| {
        set.insert(&entry.0);
        (set, selected.or_else(|| if entry.1 { Some(&entry.0.route) } else { None }))
    });

    assert_eq!(expected_len, set.len(), "We lost some entries in converting");
    (set, selected)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(bound(deserialize = "P: for<'a> serde::Deserialize<'a>"))]
pub struct BgpTableDiff<P: Prefix> {
    pub gns3_rib: Vec<(BgpRibEntry<P>, bool)>,
    pub bgpsim_rib: Vec<(BgpRibEntry<P>, bool)>,
}

impl<P: Prefix> BgpTableDiff<P> {
    /// Are the selected routes equal
    pub fn selected_routes_equal(&self) -> bool {
        // If both are empty, they are equal
        if self.gns3_rib.is_empty() && self.bgpsim_rib.is_empty() {
            return true;
        }

        let gns3_selected_route = match self.gns3_rib.iter().filter(|e| e.1).exactly_one() {
            Ok(entry) => &entry.0.route,
            Err(_) => return false,
        };

        let bgpsim_selected_route = match self.bgpsim_rib.iter().filter(|e| e.1).exactly_one() {
            Ok(entry) => &entry.0.route,
            Err(_) => return false,
        };

        bgpsim_selected_route == gns3_selected_route
    }
}

impl<'a, P, Q, Ospf> NetworkFormatter<'a, P, Q, Ospf> for BgpTableDiff<P>
where
    P: Prefix,
    Q: EventQueue<P>,
    Ospf: bgpsim::ospf::OspfImpl,
{
    fn fmt(&self, net: &'a Network<P, Q, Ospf>) -> String {
        format!(
            "bgpsim: {}  |  gns3: {}",
            (!self.bgpsim_rib.is_empty())
                .then(|| self.bgpsim_rib.iter().map(|e| e.fmt_ext(net)).join("  "))
                .unwrap_or_else(|| "Empty".to_string()),
            (!self.gns3_rib.is_empty())
                .then(|| self.gns3_rib.iter().map(|e| e.fmt_ext(net)).join("  "))
                .unwrap_or_else(|| "Empty".to_string())
        )
    }

    fn fmt_multiline_indent(&self, net: &'a Network<P, Q, Ospf>, indent: usize) -> String {
        let spc = " ".repeat(indent + 2);
        let join_str = "\n        ".to_owned() + &spc;
        format!(
            "\n{spc}bgpsim: {}\n{spc}gns3:   {}",
            (!self.bgpsim_rib.is_empty())
                .then(|| self.bgpsim_rib.iter().map(|e| e.fmt_ext(net)).join(&join_str))
                .unwrap_or_else(|| "  Empty".to_string()),
            (!self.gns3_rib.is_empty())
                .then(|| self.gns3_rib.iter().map(|e| e.fmt_ext(net)).join(&join_str))
                .unwrap_or_else(|| "  Empty".to_string())
        )
    }
}

pub type BgpTableDiffs<P> = HashMap<RouterId, HashMap<P, BgpTableDiff<P>>>;
