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

use function_name::named;
use std::{concat, thread::sleep, time::Duration};
use test_log::test;

use bgpsim::{
    builder::{
        equal_preferences, k_highest_degree_nodes, k_random_nodes, uniform_integer_link_weight,
        NetworkBuilder,
    },
    prelude::*,
    route_map::{RouteMapBuilder, RouteMapDirection::Incoming},
    topology_zoo::TopologyZoo,
    types::SimplePrefix as P,
};

use crate::Gns3Network;

#[allow(dead_code)]
// #[test]
#[named]
fn create() {
    let mut net = Network::build_complete_graph(BasicEventQueue::new(), 4);
    net.build_external_routers(k_highest_degree_nodes, 2).unwrap();
    net.build_link_weights(uniform_integer_link_weight, (10, 100)).unwrap();
    net.build_ibgp_full_mesh().unwrap();
    net.build_ebgp_sessions().unwrap();
    net.build_advertisements(P::from(0), equal_preferences, 2).unwrap();
    net.build_advertisements(P::from(1), equal_preferences, 2).unwrap();

    let gnet = Gns3Network::new(concat!("network::", function_name!()), &net, None, None).unwrap();

    sleep(Duration::from_secs(10000));

    let _ = gnet;
}

#[test]
#[named]
fn simple_network() {
    let mut net = Network::build_complete_graph(BasicEventQueue::new(), 4);
    net.build_external_routers(|_, _| [0.into(), 1.into()], ()).unwrap();
    net.build_link_weights(uniform_integer_link_weight, (10, 1000)).unwrap();
    net.build_ibgp_full_mesh().unwrap();
    net.build_ebgp_sessions().unwrap();
    net.build_advertisements(P::from(0), equal_preferences, 2).unwrap();
    net.build_advertisements(P::from(1), equal_preferences, 2).unwrap();

    let mut gnet =
        Gns3Network::new(concat!("network::", function_name!()), &net, None, None).unwrap();

    // wait for convergence
    gnet.wait_for_convergence(Duration::from_secs(300), None).unwrap();

    assert!(gnet.check_reachability(None).unwrap());
    assert!(gnet.equal_forwarding_state(&net).unwrap());
    assert!(gnet.equal_bgp_state(&net).unwrap());
}

#[test]
#[named]
fn ecmp_equal_behavior() {
    let mut net = Network::build_complete_graph(BasicEventQueue::new(), 4);
    net.build_external_routers(k_highest_degree_nodes, 2).unwrap();
    net.build_link_weights(uniform_integer_link_weight, (10, 11)).unwrap();
    net.build_ibgp_full_mesh().unwrap();
    net.build_ebgp_sessions().unwrap();
    net.build_advertisements(P::from(0), equal_preferences, 2).unwrap();
    net.build_advertisements(P::from(1), equal_preferences, 2).unwrap();
    for r in net.get_routers() {
        net.set_load_balancing(r, true).unwrap();
    }

    let mut gnet =
        Gns3Network::new(concat!("network::", function_name!()), &net, None, None).unwrap();

    // wait for convergence
    gnet.wait_for_convergence(Duration::from_secs(300), None).unwrap();

    assert!(gnet.check_reachability(None).unwrap());
    assert!(gnet.equal_forwarding_state(&net).unwrap());
    assert!(gnet.equal_bgp_state(&net).unwrap());
}

#[test]
#[named]
fn ecmp_rr_equal_behavior() {
    let mut net = Network::build_complete_graph(BasicEventQueue::new(), 4);
    net.build_external_routers(k_highest_degree_nodes, 2).unwrap();
    net.build_link_weights(uniform_integer_link_weight, (10, 11)).unwrap();
    net.build_ibgp_route_reflection(k_random_nodes, 2).unwrap();
    net.build_ebgp_sessions().unwrap();
    net.build_advertisements(P::from(0), equal_preferences, 2).unwrap();
    net.build_advertisements(P::from(1), equal_preferences, 2).unwrap();
    for r in net.get_routers() {
        net.set_load_balancing(r, true).unwrap();
    }

    let mut gnet =
        Gns3Network::new(concat!("network::", function_name!()), &net, None, None).unwrap();

    // wait for convergence
    gnet.wait_for_convergence(Duration::from_secs(300), None).unwrap();

    assert!(gnet.check_reachability(None).unwrap());
    assert!(gnet.equal_forwarding_state(&net).unwrap());
    assert!(gnet.equal_bgp_state(&net).unwrap());

    // sleep(Duration::from_secs(10000));
}

#[test]
#[named]
fn fw_state_equal_path() {
    let mut net = Network::build_complete_graph(BasicEventQueue::new(), 4);
    net.build_external_routers(k_highest_degree_nodes, 2).unwrap();
    net.build_link_weights(uniform_integer_link_weight, (10, 1000)).unwrap();
    net.build_ibgp_full_mesh().unwrap();
    net.build_ebgp_sessions().unwrap();
    net.build_advertisements(P::from(0), equal_preferences, 2).unwrap();
    net.build_advertisements(P::from(1), equal_preferences, 2).unwrap();

    let mut gnet =
        Gns3Network::new(concat!("network::", function_name!()), &net, None, None).unwrap();

    // wait for convergence
    gnet.wait_for_convergence(Duration::from_secs(300), None).unwrap();

    assert!(gnet.check_reachability(None).unwrap());
    assert!(gnet.equal_forwarding_state(&net).unwrap());
    assert!(gnet.equal_bgp_state(&net).unwrap());

    // sleep(Duration::from_secs(10000));
}

#[test]
#[named]
fn route_maps() {
    let mut net = Network::build_complete_graph(BasicEventQueue::new(), 4);
    net.build_external_routers(k_highest_degree_nodes, 2).unwrap();
    net.build_link_weights(uniform_integer_link_weight, (10, 1000)).unwrap();
    net.build_ibgp_full_mesh().unwrap();
    net.build_ebgp_sessions().unwrap();
    net.build_advertisements(P::from(0), equal_preferences, 2).unwrap();
    let ext = net.build_advertisements(P::from(1), equal_preferences, 2).unwrap()[0][0];
    let int = *net.get_device(ext).unwrap_external().get_bgp_sessions().iter().next().unwrap();
    net.set_bgp_route_map(
        int,
        ext,
        Incoming,
        RouteMapBuilder::new().allow().set_local_pref(200).order(10).build(),
    )
    .unwrap();

    let mut gnet =
        Gns3Network::new(concat!("network::", function_name!()), &net, None, None).unwrap();

    // wait for convergence
    gnet.wait_for_convergence(Duration::from_secs(300), None).unwrap();

    assert!(gnet.check_reachability(None).unwrap());
    assert!(gnet.equal_forwarding_state(&net).unwrap());
    assert!(gnet.equal_bgp_state(&net).unwrap());
}

#[test]
#[named]
fn abilene_full_mesh() {
    let mut net = TopologyZoo::Abilene.build(BasicEventQueue::new());
    net.build_external_routers(k_highest_degree_nodes, 2).unwrap();
    net.build_link_weights(uniform_integer_link_weight, (10, 100)).unwrap();
    net.build_ibgp_full_mesh().unwrap();
    net.build_ebgp_sessions().unwrap();
    net.build_advertisements(P::from(0), equal_preferences, 2).unwrap();
    net.build_advertisements(P::from(1), equal_preferences, 2).unwrap();

    let mut gnet =
        Gns3Network::new(concat!("network::", function_name!()), &net, None, None).unwrap();

    gnet.set_geo_delay(&TopologyZoo::Abilene.geo_location()).unwrap();

    // wait for convergence
    gnet.wait_for_convergence(Duration::from_secs(300), None).unwrap();

    let reachable = gnet.check_reachability(None).unwrap();
    let fw_state = gnet.equal_forwarding_state(&net).unwrap();
    let bgp_state = gnet.equal_bgp_state(&net).unwrap();

    if !(reachable && fw_state && bgp_state) {
        sleep(Duration::from_secs(10000))
    }

    assert!(gnet.check_reachability(None).unwrap());
    assert!(gnet.equal_forwarding_state(&net).unwrap());
    assert!(gnet.equal_bgp_state(&net).unwrap());
}

#[test]
#[named]
fn abilene_rr() {
    let mut net = TopologyZoo::Abilene.build(BasicEventQueue::new());
    net.build_external_routers(k_highest_degree_nodes, 2).unwrap();
    net.build_link_weights(uniform_integer_link_weight, (10, 100)).unwrap();
    net.build_ibgp_route_reflection(k_random_nodes, 2).unwrap();
    net.build_ebgp_sessions().unwrap();
    net.build_advertisements(P::from(0), equal_preferences, 2).unwrap();
    net.build_advertisements(P::from(1), equal_preferences, 2).unwrap();

    let mut gnet =
        Gns3Network::new(concat!("network::", function_name!()), &net, None, None).unwrap();

    gnet.set_geo_delay(&TopologyZoo::Abilene.geo_location()).unwrap();

    // wait for convergence
    gnet.wait_for_convergence(Duration::from_secs(300), None).unwrap();
    assert!(gnet.check_reachability(None).unwrap());
    assert!(gnet.equal_forwarding_state(&net).unwrap());
    assert!(gnet.equal_bgp_state(&net).unwrap());
}
