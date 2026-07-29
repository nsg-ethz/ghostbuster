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

use crate::gns3::Gns3Project;

use function_name::named;
use test_log::test;

#[test]
#[named]
fn create_empty_project() {
    let _ = Gns3Project::new(concat!("create_project::", function_name!()), None, None).unwrap();
}

#[test]
#[named]
fn start_all_nodes() {
    let mut p =
        Gns3Project::new(concat!("create_project::", function_name!()), None, None).unwrap();
    let a = p.create_frr_node("test_a").unwrap();
    let b = p.create_frr_node("test_b").unwrap();

    p.start_all_nodes().unwrap();

    // check if we can get the client for a and b
    let _ = p.get_node(a).get_frr_client("localhost").unwrap();
    let _ = p.get_node(b).get_frr_client("localhost").unwrap();
}
