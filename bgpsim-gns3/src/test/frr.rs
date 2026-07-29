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

use bgpsim::export::cisco_frr_generators::{Interface, Target};
use pretty_assertions::assert_eq;
use std::{thread::sleep, time::Duration};

use function_name::named;
use test_log::test;

use crate::{
    e,
    gns3::{nodes::frr::Protocol, Gns3Project},
};

#[test]
#[named]
fn get_running_config() {
    let mut p = e!(Gns3Project::new(concat!("frr::", function_name!()), None, None));
    let node = e!(p.create_frr_node("router"));
    // start the router
    e!(p.start_node(node));
    // wait a bit
    sleep(Duration::from_secs(1));
    // get the shell
    let mut term = p.get_node(node).get_frr_client("localhost".to_string()).unwrap();
    // get the config and compare it
    assert_eq!(
        term.get_running_config().unwrap(),
        "!
frr version 8.1_git
frr defaults traditional
hostname router
no ipv6 forwarding
!
segment-routing
 traffic-eng
 exit
exit"
    )
}

#[test]
#[named]
fn configure_lo() {
    let mut p = e!(Gns3Project::new(concat!("frr::", function_name!()), None, None));
    let node = e!(p.create_frr_node("router"));
    // start the router
    e!(p.start_node(node));
    // wait a bit
    sleep(Duration::from_secs(1));
    // get the shell
    let mut term = p.get_node(node).get_frr_client("localhost".to_string()).unwrap();
    // configure the IP address
    term.configure(
        Interface::new("lo")
            .no_shutdown()
            .ip_address("10.0.0.1/8".parse().unwrap())
            .build(Target::Frr),
    )
    .unwrap();
    // get the config and compare it
    assert_eq!(
        term.get_running_config().unwrap(),
        "!
frr version 8.1_git
frr defaults traditional
hostname router
no ipv6 forwarding
!
interface lo
 ip address 10.0.0.1/8
exit
!
segment-routing
 traffic-eng
 exit
exit"
    )
}

#[test]
#[named]
fn ping() {
    let mut p = e!(Gns3Project::new(concat!("frr::", function_name!()), None, None));
    let a = e!(p.create_frr_node("router_a"));
    let b = e!(p.create_frr_node("router_b"));
    // connect the two nodes
    e!(p.connect_nodes(a, b));
    // start the routers
    e!(p.start_node(a));
    e!(p.start_node(b));
    // wait a bit
    sleep(Duration::from_secs(1));
    // get the shell
    let mut a_term = p.get_node(a).get_frr_client("localhost".to_string()).unwrap();
    let mut b_term = p.get_node(b).get_frr_client("localhost".to_string()).unwrap();
    // configure the IP address
    a_term
        .configure(
            Interface::new("eth0")
                .no_shutdown()
                .ip_address("10.0.0.1/8".parse().unwrap())
                .build(Target::Frr),
        )
        .unwrap();
    // configure the IP address
    b_term
        .configure(
            Interface::new("eth0")
                .no_shutdown()
                .ip_address("10.0.0.2/8".parse().unwrap())
                .build(Target::Frr),
        )
        .unwrap();

    // wait a bit
    sleep(Duration::from_secs(1));

    b_term.ping("10.0.0.1".parse().unwrap()).unwrap();
    a_term.ping("10.0.0.2".parse().unwrap()).unwrap();
}

#[test]
#[named]
fn next_hop() {
    let mut p = e!(Gns3Project::new(concat!("frr::", function_name!()), None, None));
    let a = e!(p.create_frr_node("router_a"));
    let b = e!(p.create_frr_node("router_b"));
    // connect the two nodes
    e!(p.connect_nodes(a, b));
    // start the routers
    e!(p.start_node(a));
    e!(p.start_node(b));
    // wait a bit
    sleep(Duration::from_secs(1));
    // get the shell
    let mut a_term = p.get_node(a).get_frr_client("localhost".to_string()).unwrap();
    let mut b_term = p.get_node(b).get_frr_client("localhost".to_string()).unwrap();
    // check that there is currently no active route
    assert!(a_term.get_route_for_address("10.0.0.2".parse().unwrap()).unwrap().is_none());
    assert!(b_term.get_route_for_address("10.0.0.1".parse().unwrap()).unwrap().is_none());
    // configure the IP address
    a_term
        .configure(
            Interface::new("eth0")
                .no_shutdown()
                .ip_address("10.0.0.1/8".parse().unwrap())
                .build(Target::Frr),
        )
        .unwrap();
    // configure the IP address
    b_term
        .configure(
            Interface::new("eth0")
                .no_shutdown()
                .ip_address("10.0.0.2/8".parse().unwrap())
                .build(Target::Frr),
        )
        .unwrap();

    // wait a bit
    sleep(Duration::from_secs(1));
    // check that there is now a route
    let a_route = a_term.get_route_for_address("10.0.0.2".parse().unwrap()).unwrap().unwrap();
    let b_route = b_term.get_route_for_address("10.0.0.1".parse().unwrap()).unwrap().unwrap();

    // check the prefix
    assert_eq!(a_route.prefix, "10.0.0.0/8".parse().unwrap());
    assert_eq!(b_route.prefix, "10.0.0.0/8".parse().unwrap());
    // check the prefix length
    assert_eq!(a_route.prefix_len, 8);
    assert_eq!(b_route.prefix_len, 8);
    // check check the protocol
    assert_eq!(a_route.protocol, Protocol::Connected);
    assert_eq!(b_route.protocol, Protocol::Connected);
    // check the next-hop
    assert_eq!(a_route.next_hops()[0].interface.as_deref(), Some("eth0"));
    assert_eq!(b_route.next_hops()[0].interface.as_deref(), Some("eth0"));
    assert!(a_route.next_hops()[0].ip.is_none());
    assert!(b_route.next_hops()[0].ip.is_none());
    // chheck that the call for the prefix and the address are the same
    assert_eq!(
        a_term.get_route_for_address("10.0.0.2".parse().unwrap()).unwrap(),
        a_term.get_route_for_prefix("10.0.0.0/8".parse().unwrap()).unwrap(),
    );
    assert_eq!(
        b_term.get_route_for_address("10.0.0.1".parse().unwrap()).unwrap(),
        b_term.get_route_for_prefix("10.0.0.0/8".parse().unwrap()).unwrap(),
    );
}
