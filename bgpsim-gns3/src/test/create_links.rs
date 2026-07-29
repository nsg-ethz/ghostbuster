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

use crate::gns3::{links::Gns3LinkFilters, Gns3Project};

use crate::e;

use function_name::named;
use test_log::test;

#[test]
#[named]
fn add_link() {
    let mut p = e!(Gns3Project::new(concat!("create_links::", function_name!()), None, None));
    let a = e!(p.create_frr_node("a"));
    let b = e!(p.create_frr_node("b"));
    let (link_id, a_iface, b_iface) = e!(p.connect_nodes(a, b));
    assert_eq!(a_iface, 0);
    assert_eq!(b_iface, 0);
    assert_eq!(p.get_links_connecting(a, b), vec![link_id]);
}

#[test]
#[named]
fn capture() {
    let mut p = e!(Gns3Project::new(concat!("create_links::", function_name!()), None, None));
    let a = e!(p.create_frr_node("a"));
    let b = e!(p.create_frr_node("b"));
    let (link, _, _) = e!(p.connect_nodes(a, b));
    e!(p.start_node(a));
    e!(p.start_node(b));
    let pcap_path = e!(p.start_capture(link));
    assert!(pcap_path.to_string_lossy().ends_with(".pcap"));
    assert!(pcap_path.exists());
    let same_path = e!(p.start_capture(link));
    assert_eq!(pcap_path, same_path);
    assert!(pcap_path.exists());
    let same_path = e!(p.stop_capture(link)).unwrap();
    assert_eq!(pcap_path, same_path);
    assert!(pcap_path.exists());
}

#[test]
#[named]
fn add_triangle() {
    let mut p = e!(Gns3Project::new(concat!("create_links::", function_name!()), None, None));
    let a = e!(p.create_frr_node("a"));
    let b = e!(p.create_frr_node("b"));
    let c = e!(p.create_frr_node("c"));
    let (link_ab, ab_iface, ba_iface) = e!(p.connect_nodes(a, b));
    let (link_bc, bc_iface, cb_iface) = e!(p.connect_nodes(b, c));
    let (link_ca, ca_iface, ac_iface) = e!(p.connect_nodes(c, a));
    assert_eq!(ab_iface, 0);
    assert_eq!(ac_iface, 1);
    assert_eq!(ba_iface, 0);
    assert_eq!(bc_iface, 1);
    assert_eq!(ca_iface, 1);
    assert_eq!(cb_iface, 0);
    assert_eq!(p.get_links_connecting(a, b), vec![link_ab]);
    assert_eq!(p.get_links_connecting(b, c), vec![link_bc]);
    assert_eq!(p.get_links_connecting(c, a), vec![link_ca]);
    assert_eq!(
        e!(p.get_link_filters(link_ab)),
        Gns3LinkFilters { delay: None, corrupt: None, frequency_drop: None, packet_loss: None }
    );
    e!(p.set_link_filters(
        link_ab,
        Gns3LinkFilters {
            delay: Some((1, 0)),
            corrupt: None,
            frequency_drop: None,
            packet_loss: None,
        },
    ));
    assert_eq!(
        e!(p.get_link_filters(link_ab)),
        Gns3LinkFilters {
            delay: Some((1, 0)),
            corrupt: None,
            frequency_drop: None,
            packet_loss: None,
        }
    );
}
