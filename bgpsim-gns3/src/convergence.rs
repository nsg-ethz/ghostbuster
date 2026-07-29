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

//! Module that waits for convergence

use bgpsim::{
    export::Addressor,
    ospf::OspfImpl,
    prelude::NetworkFormatter,
    types::{Prefix, RouterId},
};
use ipnet::Ipv4Net;
use itertools::Itertools;
use log::info;
use rand::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    net::Ipv4Addr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    thread::{sleep, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    gns3::nodes::{
        frr::{BgpPath, BgpRoute, FrrError, OspfRoute, Route},
        TelnetHandle,
    },
    Gns3Network, Gns3NetworkError,
};

/// Time in seconds, which indicates that the network has converged if nothing in any control-plane
/// table of any router has changed for this amount of seconds.
const NO_UPDATE_TIME: Duration = Duration::from_secs(10);
/// Frequency to pull data
const FREQUENCY: u64 = 2_000;
/// Frequency to pull data when also tracking the change. This is 10 times faster than the normal
/// frequency.
const FREQUENCY_TRACK: u64 = 200;

impl<'n, P: Prefix, Q, Ospf: OspfImpl> Gns3Network<'n, P, Q, Ospf> {
    /// Wait until the network has converged. This is done by monitoring the routing tables of all
    /// routers. If none of these tables have seen any change for the last 10 seconds, then we call
    /// this network converged.
    ///
    /// This function will spawn a thread for each router in the network. The threads will poll the
    /// routing table every 2 seconds. Further, the threads will be desynced by waiting a random
    /// time between 0 and 2 seconds before starting. This will reduce the spikes in performance.
    ///
    /// You can provide an argument `max_wait_time`, which is the upper limit of how long we should
    /// wait for convergence.
    pub fn wait_for_convergence(
        &mut self,
        max_wait_time: Duration,
        no_update_time: Option<Duration>,
    ) -> Result<(), Gns3NetworkError> {
        info!("Waiting for convergence...");

        let last_update = Arc::new(AtomicU64::new(0));
        let start_time = Instant::now();
        let prefixes = self
            .net
            .get_known_prefixes()
            .flat_map(|p| self.addressor.prefix_address(*p).unwrap())
            .collect::<Vec<_>>();

        // create all threads
        let threads = self
            .net
            .get_topology()
            .node_indices()
            .map(|r| {
                let handle = self.get_frr_handle(r).unwrap();
                let last_update = last_update.clone();
                let prefixes = prefixes.clone();
                let router_name = r.fmt(self.net).to_string();
                Ok(std::thread::spawn(move || {
                    wait_for_convergence(
                        handle,
                        start_time,
                        max_wait_time,
                        no_update_time.unwrap_or(NO_UPDATE_TIME),
                        last_update,
                        prefixes,
                        router_name,
                    )
                }))
            })
            .collect::<Result<Vec<JoinHandle<_>>, Gns3NetworkError>>()?;

        // join all threasd
        for thread in threads {
            thread.join().map_err(Gns3NetworkError::ThreadError)??;
        }

        info!("Network has converged after {:.1}s!", start_time.elapsed().as_secs_f64());

        Ok(())
    }

    /// Wait until the network has converged. This is done by monitoring the routing tables of all
    /// routers. If none of these tables have seen any change for the last 10 seconds, then we call
    /// this network converged.
    ///
    /// This function will spawn a thread for each router in the network. The threads will poll the
    /// routing table every 0.2 seconds. Further, the threads will be desynced by waiting a random
    /// time between 0 and 0.2 seconds before starting. This will reduce the spikes in performance.
    ///
    /// You can provide an argument `max_wait_time`, which is the upper limit of how long we should
    /// wait for convergence.
    ///
    /// During this time, the function will collect all changes in state, that is, OSPF, BGP and
    /// general routing state. The function will return a structure that contains all changes.
    pub fn wait_for_convergence_track_changes(
        &mut self,
        max_wait_time: Duration,
        no_update_time: Option<Duration>,
    ) -> Result<HashMap<RouterId, Vec<RouterStateDelta>>, Gns3NetworkError> {
        info!("Waiting for convergence...");

        let last_update = Arc::new(AtomicU64::new(0));
        let start_time = Instant::now();
        let prefixes = self
            .net
            .get_known_prefixes()
            .flat_map(|p| self.addressor.prefix_address(*p).unwrap())
            .collect::<Vec<_>>();

        // create all threads
        let threads = self
            .net
            .get_topology()
            .node_indices()
            .map(|r| {
                let handle = self.get_frr_handle(r).unwrap();
                let last_update = last_update.clone();
                let prefixes = prefixes.clone();
                let router_name = r.fmt(self.net).to_string();
                Ok((
                    r,
                    std::thread::spawn(move || {
                        wait_for_convergence_track_changes(
                            handle,
                            start_time,
                            max_wait_time,
                            no_update_time.unwrap_or(NO_UPDATE_TIME),
                            last_update,
                            prefixes,
                            router_name,
                        )
                    }),
                ))
            })
            .collect::<Result<Vec<(RouterId, JoinHandle<_>)>, Gns3NetworkError>>()?;

        let mut results = HashMap::new();
        // join all threasd
        for (r, thread) in threads {
            let delta = thread.join().map_err(Gns3NetworkError::ThreadError)??;
            results.insert(r, delta);
        }

        info!("Network has converged after {:.1}s!", start_time.elapsed().as_secs_f64());

        Ok(results)
    }
}

/// Process that waits until the the last_update happened more than 30 seconds ago. This function
/// can be spawned as a separate thread. If convergence was reached before `max_wait_time`, then
/// return normally. Otherwise, return `Err(Gns3NetworkError::NoCovergence)`.
fn wait_for_convergence(
    handle: TelnetHandle,
    start_time: Instant,
    max_wait_time: Duration,
    no_update_time: Duration,
    last_update: Arc<AtomicU64>,
    prefixes: Vec<Ipv4Net>,
    router_name: String,
) -> Result<(), Gns3NetworkError> {
    let mut client = handle.open_frr()?;
    // sleep for a random amount of time, to desync the threads
    let random_sleep_time = thread_rng().gen_range(0..FREQUENCY);
    sleep(Duration::from_millis(random_sleep_time));

    let mut ospf_state = client.get_ospf_routes()?;
    let mut bgp_state = prefixes
        .iter()
        .map(|p| client.get_bgp_routes_for_prefix(*p))
        .collect::<Result<Vec<_>, _>>()?;
    let mut routing_state = client.get_all_routes()?;

    while start_time.elapsed() < max_wait_time {
        sleep(Duration::from_millis(FREQUENCY));
        let new_ospf_state = client.get_ospf_routes()?;
        let new_bgp_state = prefixes
            .iter()
            .map(|p| client.get_bgp_routes_for_prefix(*p))
            .collect::<Result<Vec<_>, _>>()?;
        let new_routing_state = client.get_all_routes()?;
        let elapsed = start_time.elapsed().as_secs();
        let ospf_update = new_ospf_state != ospf_state;
        let bgp_update = new_bgp_state != bgp_state;
        let routing_update = new_routing_state != routing_state;
        if ospf_update || bgp_update || routing_update {
            info!(
                "{} has updated its {} table(s)",
                router_name,
                [(routing_update, "routing"), (ospf_update, "ospf"), (bgp_update, "bgp")]
                    .into_iter()
                    .filter(|(x, _)| *x)
                    .map(|(_, x)| x)
                    .join(" and ")
            );
            ospf_state = new_ospf_state;
            bgp_state = new_bgp_state;
            routing_state = new_routing_state;
            last_update.store(elapsed, Ordering::Relaxed);
        } else if last_update.load(Ordering::Relaxed) + no_update_time.as_secs() < elapsed {
            return Ok(());
        }
    }

    Err(Gns3NetworkError::NoConvergence)
}

/// Process that waits until the the last_update happened more than 30 seconds ago. This function
/// can be spawned as a separate thread. If convergence was reached before `max_wait_time`, then
/// return normally. Otherwise, return `Err(Gns3NetworkError::NoCovergence)`.
///
/// During this time, track all changes happening to any table and return that trace.
fn wait_for_convergence_track_changes(
    handle: TelnetHandle,
    start_time: Instant,
    max_wait_time: Duration,
    no_update_time: Duration,
    last_update: Arc<AtomicU64>,
    prefixes: Vec<Ipv4Net>,
    router_name: String,
) -> Result<Vec<RouterStateDelta>, Gns3NetworkError> {
    let mut result = Vec::new();

    let mut client = handle.open_frr()?;

    let mut ospf_state = client.get_ospf_routes()?;
    let mut bgp_state: HashMap<Ipv4Net, Option<BgpRoute>> = prefixes
        .iter()
        .map(|p| Ok((*p, client.get_bgp_routes_for_prefix(*p)?)))
        .collect::<Result<_, FrrError>>()?;
    let mut routing_state = client.get_all_routes()?;

    // sleep for a random amount of time, to desync the threads, but only after getting the initial
    // state.
    let random_sleep_time = thread_rng().gen_range(0..FREQUENCY_TRACK);
    sleep(Duration::from_millis(random_sleep_time));

    while start_time.elapsed() < max_wait_time {
        sleep(Duration::from_millis(FREQUENCY_TRACK));
        let new_ospf_state = client.get_ospf_routes()?;
        let new_bgp_state: HashMap<Ipv4Net, Option<BgpRoute>> = prefixes
            .iter()
            .map(|p| Ok((*p, client.get_bgp_routes_for_prefix(*p)?)))
            .collect::<Result<_, FrrError>>()?;
        let new_routing_state = client.get_all_routes()?;
        let elapsed = start_time.elapsed().as_secs();
        let mut ospf_update = RouterStateDelta::from_ospf(&ospf_state, &new_ospf_state);
        let mut bgp_update = RouterStateDelta::from_bgp(&bgp_state, &new_bgp_state);
        let mut routing_update = RouterStateDelta::from_routes(&routing_state, &new_routing_state);
        if !ospf_update.is_empty() || !bgp_update.is_empty() || !routing_update.is_empty() {
            info!(
                "{} has updated its {} table(s)",
                router_name,
                [
                    (routing_update.is_empty(), "routing"),
                    (ospf_update.is_empty(), "ospf"),
                    (bgp_update.is_empty(), "bgp")
                ]
                .into_iter()
                .filter(|(x, _)| !*x)
                .map(|(_, x)| x)
                .join(" and ")
            );
            ospf_state = new_ospf_state;
            bgp_state = new_bgp_state;
            routing_state = new_routing_state;
            result.append(&mut ospf_update);
            result.append(&mut bgp_update);
            result.append(&mut routing_update);
            last_update.store(elapsed, Ordering::Relaxed);
        } else if last_update.load(Ordering::Relaxed) + no_update_time.as_secs() < elapsed {
            return Ok(result);
        }
    }

    Err(Gns3NetworkError::NoConvergence)
}

/// Structure that captures all router state changes
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouterStateDelta {
    /// The OSPF state has changed
    Ospf {
        /// The network of the OSPF route
        prefix: Ipv4Net,
        /// Old OSPF Route
        old: Box<Option<OspfRoute>>,
        /// New OSPF Route
        new: Box<Option<OspfRoute>>,
    },
    /// The BGP state changed to whom the routes are advertised to.
    BgpAdvertisedTo {
        /// The prefix of the route
        prefix: Ipv4Net,
        /// The peer two whom the route was advertised to.
        peer: Ipv4Addr,
        /// Either the router now advertises the route to that peer (`true`) or it no longer
        /// advertises it to that peer (`false).
        new_state: bool,
    },
    /// The BGP Path from a specific peer has changed
    BgpPath {
        /// The prefix of the route
        prefix: Ipv4Net,
        /// The peer from whom the route was learned
        peer: Ipv4Addr,
        /// The old route
        old: Box<Option<BgpPath>>,
        /// The new route
        new: Box<Option<BgpPath>>,
    },
    /// The route towards some destination has changed
    Route {
        /// The prefix for the route
        prefix: Ipv4Net,
        /// The old route
        old: Box<Option<Route>>,
        /// The new route
        new: Box<Option<Route>>,
    },
}

impl RouterStateDelta {
    /// Create a delta from two routing tables. If the resulting vector is empty, then the two
    /// states are equivalent. This will only compare the selected route.
    fn from_routes(
        old: &HashMap<Ipv4Net, Vec<Route>>,
        new: &HashMap<Ipv4Net, Vec<Route>>,
    ) -> Vec<Self> {
        let mut result = Vec::new();
        for net in old.keys().chain(new.keys()).unique() {
            let old_route = old.get(net).and_then(|x| x.iter().find(|r| r.is_valid()));
            let new_route = new.get(net).and_then(|x| x.iter().find(|r| r.is_valid()));
            if old_route != new_route {
                result.push(Self::Route {
                    prefix: *net,
                    old: Box::new(old_route.cloned()),
                    new: Box::new(new_route.cloned()),
                });
            }
        }

        result
    }

    /// Create a delta from two ospf tables. If the resulting vector is empty, then the two states
    /// are equivalent.
    fn from_ospf(
        old: &HashMap<Ipv4Net, OspfRoute>,
        new: &HashMap<Ipv4Net, OspfRoute>,
    ) -> Vec<Self> {
        let mut result = Vec::new();
        for net in old.keys().chain(new.keys()).unique() {
            let old_route = old.get(net);
            let new_route = new.get(net);
            if old_route != new_route {
                result.push(Self::Ospf {
                    prefix: *net,
                    old: Box::new(old_route.cloned()),
                    new: Box::new(new_route.cloned()),
                });
            }
        }

        result
    }

    /// Create a delta from two bgp tables. If the resulting vector is empty, then the two states
    /// are equivalent.
    fn from_bgp(
        old: &HashMap<Ipv4Net, Option<BgpRoute>>,
        new: &HashMap<Ipv4Net, Option<BgpRoute>>,
    ) -> Vec<Self> {
        let mut result = Vec::new();
        for net in old.keys().chain(new.keys()).unique() {
            let old_route = old.get(net).and_then(|x| x.as_ref());
            let new_route = new.get(net).and_then(|x| x.as_ref());
            result.append(&mut Self::from_single_bgp(*net, old_route, new_route))
        }

        result
    }

    /// Create a delta from two bgp entries. If the resulting vector is empty, then the two states
    /// are equivalent.
    fn from_single_bgp(
        prefix: Ipv4Net,
        old: Option<&BgpRoute>,
        new: Option<&BgpRoute>,
    ) -> Vec<Self> {
        let mut result = Vec::new();

        let old_ads = old
            .map(|x| x.advertised_to.iter().copied().collect::<HashSet<_>>())
            .unwrap_or_default();
        let new_ads = new
            .map(|x| x.advertised_to.iter().copied().collect::<HashSet<_>>())
            .unwrap_or_default();

        let mut old_paths: HashMap<Ipv4Addr, &BgpPath> =
            old.map(|x| x.paths.iter().map(|p| (p.peer.peer_id, p)).collect()).unwrap_or_default();
        let mut new_paths: HashMap<Ipv4Addr, &BgpPath> =
            new.map(|x| x.paths.iter().map(|p| (p.peer.peer_id, p)).collect()).unwrap_or_default();

        for additional_ads in new_ads.difference(&old_ads) {
            result.push(Self::BgpAdvertisedTo { prefix, peer: *additional_ads, new_state: true });
        }
        for missing_ads in old_ads.difference(&new_ads) {
            result.push(Self::BgpAdvertisedTo { prefix, peer: *missing_ads, new_state: false });
        }

        for peer in old_paths.keys().chain(new_paths.keys()).copied().unique().collect_vec() {
            let old_path = old_paths.remove(&peer);
            let new_path = new_paths.remove(&peer);
            if old_path != new_path {
                result.push(Self::BgpPath {
                    prefix,
                    peer,
                    old: Box::new(old_path.cloned()),
                    new: Box::new(new_path.cloned()),
                });
            }
        }

        result
    }

    /// Get the prefix of this delta.
    pub fn prefix(&self) -> Ipv4Net {
        match self {
            Self::Ospf { prefix, .. }
            | Self::BgpAdvertisedTo { prefix, .. }
            | Self::BgpPath { prefix, .. }
            | Self::Route { prefix, .. } => *prefix,
        }
    }
}
