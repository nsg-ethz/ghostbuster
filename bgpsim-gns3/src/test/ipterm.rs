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
use net_parser_rs::{
    layer2::Ethernet,
    layer3::{IPv4, InternetProtocolId},
    CaptureFile,
};
use pretty_hex::*;
use std::{
    net::{IpAddr, Ipv4Addr},
    thread::sleep,
    time::Duration,
};
use test_log::test;

use crate::{
    e,
    gns3::{nodes::PingError, Gns3Project},
};

#[test]
#[named]
fn communicate() {
    let mut p = e!(Gns3Project::new(concat!("ipterm::", function_name!()), None, None));
    let a = e!(p.create_ipterm_node("a"));
    let b = e!(p.create_ipterm_node("b"));
    let (link_id, a_iface, b_iface) = e!(p.connect_nodes(a, b));
    assert_eq!(a_iface, 0);
    assert_eq!(b_iface, 0);
    assert_eq!(p.get_links_connecting(a, b), vec![link_id]);
    e!(p.start_node(a));
    e!(p.start_node(b));
    let mut a_term = p.get_node(a).get_ipterm_client("localhost".to_string()).unwrap();
    let mut b_term = p.get_node(b).get_ipterm_client("localhost".to_string()).unwrap();
    // check that the ping is not working
    assert!(matches!(a_term.ping(Ipv4Addr::new(192, 168, 1, 3)), Err(PingError::Fail(_))));
    assert!(matches!(b_term.ping(Ipv4Addr::new(192, 168, 1, 2)), Err(PingError::Fail(_))));
    // set the IP addr
    a_term
        .configure_ip(
            Ipv4Addr::new(192, 168, 1, 2),
            Ipv4Addr::new(255, 255, 255, 0),
            Ipv4Addr::new(192, 168, 1, 1),
        )
        .unwrap();
    b_term
        .configure_ip(
            Ipv4Addr::new(192, 168, 1, 3),
            Ipv4Addr::new(255, 255, 255, 0),
            Ipv4Addr::new(192, 168, 1, 1),
        )
        .unwrap();
    a_term.ping(Ipv4Addr::new(192, 168, 1, 3)).unwrap();
    b_term.ping(Ipv4Addr::new(192, 168, 1, 2)).unwrap();
}

#[test]
#[named]
fn iperf3() {
    let mut p = e!(Gns3Project::new(concat!("ipterm::", function_name!()), None, None));
    let a = e!(p.create_ipterm_node("a"));
    let b = e!(p.create_ipterm_node("b"));
    let (link_id, a_iface, b_iface) = e!(p.connect_nodes(a, b));
    assert_eq!(a_iface, 0);
    assert_eq!(b_iface, 0);
    assert_eq!(p.get_links_connecting(a, b), vec![link_id]);
    e!(p.start_node(a));
    e!(p.start_node(b));
    let mut a_term = p.get_node(a).get_ipterm_client("localhost".to_string()).unwrap();
    let mut b_term = p.get_node(b).get_ipterm_client("localhost".to_string()).unwrap();
    // set the IP addr
    a_term
        .configure_ip(
            Ipv4Addr::new(192, 168, 1, 2),
            Ipv4Addr::new(255, 255, 255, 0),
            Ipv4Addr::new(192, 168, 1, 1),
        )
        .unwrap();
    b_term
        .configure_ip(
            Ipv4Addr::new(192, 168, 1, 3),
            Ipv4Addr::new(255, 255, 255, 0),
            Ipv4Addr::new(192, 168, 1, 1),
        )
        .unwrap();
    a_term.iperf3_server().unwrap();
    let iperf3_client = b_term.iperf3_client(Ipv4Addr::new(192, 168, 1, 2)).start().unwrap();
    sleep(std::time::Duration::from_secs_f64(1.1));
    let iperf3_data = iperf3_client.stop().unwrap();
    assert!(iperf3_data.len() >= 9);
    assert!(iperf3_data.len() <= 12);
}

#[test]
#[named]
fn netcat_capture() {
    let mut p = e!(Gns3Project::new(concat!("ipterm::", function_name!()), None, None));
    let a = e!(p.create_ipterm_node("a"));
    let b = e!(p.create_ipterm_node("b"));
    let (link_id, a_iface, b_iface) = e!(p.connect_nodes(a, b));
    assert_eq!(a_iface, 0);
    assert_eq!(b_iface, 0);
    assert_eq!(p.get_links_connecting(a, b), vec![link_id]);
    e!(p.start_node(a));
    e!(p.start_node(b));
    let mut a_term = p.get_node(a).get_ipterm_client("localhost".to_string()).unwrap();
    let mut b_term = p.get_node(b).get_ipterm_client("localhost".to_string()).unwrap();
    // set the IP addr
    a_term
        .configure_ip(
            Ipv4Addr::new(192, 168, 1, 2),
            Ipv4Addr::new(255, 255, 255, 0),
            Ipv4Addr::new(192, 168, 1, 1),
        )
        .unwrap();
    b_term
        .configure_ip(
            Ipv4Addr::new(192, 168, 1, 3),
            Ipv4Addr::new(255, 255, 255, 0),
            Ipv4Addr::new(192, 168, 1, 1),
        )
        .unwrap();

    // start the capture
    let pcap_file = p.start_capture(link_id).unwrap();

    // start the socat command
    let socat_process =
        a_term.socat_ping(Ipv4Addr::new(192, 168, 1, 3), Duration::from_millis(100)).unwrap();

    // wait for 1 second
    sleep(Duration::from_millis(1010));

    // stop the process
    a_term.stop_process(socat_process).unwrap();

    // wait some time to check that the process really stopped
    sleep(Duration::from_millis(200));

    // stop the capture
    p.stop_capture(link_id).unwrap();

    // read the pcap file
    let pcap_content = std::fs::read(pcap_file).unwrap();

    // parse the pcap file
    let (_, records) = CaptureFile::parse(&pcap_content).unwrap();
    let records = records.records.into_inner();

    let mut num_pings = 0;

    for record in records {
        // dump the record
        eprintln!("New packet:\n{:?}", record.payload.hex_dump());
        // find the ping packets from 192.168.1.2 to 192.168.1.3
        match Ethernet::parse(record.payload).and_then(|(_, eth)| IPv4::parse(eth.payload)) {
            Ok((_, ip_pkt)) => {
                eprintln!("got IP packet: {ip_pkt:?}");
                if ip_pkt.protocol == InternetProtocolId::ICMP
                    && ip_pkt.src_ip == IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2))
                    && ip_pkt.dst_ip == IpAddr::V4(Ipv4Addr::new(192, 168, 1, 3))
                {
                    num_pings += 1;
                }
            }
            Err(e) => eprintln!("cannot parse: {e}"),
        }
        eprintln!("\n\n");
    }

    assert_eq!(num_pings, 10)
}
