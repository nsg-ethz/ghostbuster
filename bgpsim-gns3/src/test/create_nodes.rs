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

use std::thread::sleep;
use std::time::Duration;

use crate::e;
use crate::gns3::Gns3Project;

use function_name::named;
use test_log::test;

#[test]
#[named]
fn add_frr_node() {
    let mut p = e!(Gns3Project::new(concat!("create_nodes::", function_name!()), None, None));
    let id = e!(p.create_frr_node("test"));
    let node = p.get_node(id);
    assert_eq!(node.name(), "test");
    assert_eq!(node.interfaces().len(), 8);
}

#[test]
#[named]
fn add_ipterm_node() {
    let mut p = e!(Gns3Project::new(concat!("create_nodes::", function_name!()), None, None));
    let id = e!(p.create_ipterm_node("test"));
    let node = p.get_node(id);
    assert_eq!(node.name(), "test");
    assert_eq!(node.interfaces().len(), 1);
}

#[test]
#[named]
fn start_node() {
    let mut p = e!(Gns3Project::new(concat!("create_nodes::", function_name!()), None, None));
    let id = e!(p.create_frr_node("test"));
    assert!(!e!(p.node_running(id)));
    e!(p.start_node(id));
    assert!(e!(p.node_running(id)));
    e!(p.start_node(id));
    assert!(e!(p.node_running(id)));
    e!(p.stop_node(id));
    assert!(!e!(p.node_running(id)));
}

#[test]
#[named]
fn setup_telnet_frr() {
    let mut p = e!(Gns3Project::new(concat!("create_nodes::", function_name!()), None, None));
    let id = e!(p.create_frr_node("test"));
    let node = p.get_node(id);
    e!(p.start_node(id));
    let mut c = node.get_client("localhost".to_string()).unwrap();
    let version = c.send_cmd("show version", Duration::from_secs(1)).unwrap();
    assert!(version.starts_with("FRRouting"));
    assert_eq!(version.lines().count(), 4);
}

#[test]
#[named]
fn setup_telnet_ipterm() {
    let mut p = e!(Gns3Project::new(concat!("create_nodes::", function_name!()), None, None));
    let id = e!(p.create_ipterm_node("test"));
    let node = p.get_node(id);
    e!(p.start_node(id));
    sleep(Duration::from_secs(5));
    let mut c = node.get_client("localhost".to_string()).unwrap();
    let whoami = c.send_cmd("whoami", Duration::from_secs(1)).unwrap();
    assert_eq!(whoami, "root");
}
