use core::panic;
// TRIX: Inference of Transient Violation Times from Logged Routing Events or Collected BGP Messages
// Copyright (C) 2024-2025 Roland Schmid <roschmi@ethz.ch> and Tibor Schneider <sctibor@ethz.ch>
// Modified 2025 by Pietro Ronchetti <pietroro@ethz.ch>: Removed TCP sequence deduplication logic
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.
use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::BufReader,
    net::{IpAddr, Ipv4Addr},
    panic::{catch_unwind, AssertUnwindSafe},
    path::Path,
};

use bgpkit_parser::{
    bgp::parse_bgp_message,
    models::{AsnLength, Attributes, BgpMessage, BgpUpdateMessage, Community, MetaCommunity},
};
use bgpsim::{
    bgp::{BgpEvent, BgpRoute},
    event::Event,
    export::Addressor,
    types::{AsId, Prefix},
};
use byteorder::{BigEndian, ByteOrder};
use bytes::{Buf, Bytes, BytesMut};
use etherparse::{InternetSlice, LinkSlice, SlicedPacket, TcpSlice, TransportSlice};
use ipnet::{IpNet, Ipv4Net};
use log::debug;
use mac_address::MacAddress;
use ordered_float::NotNan;
use pcap_file::pcap::PcapReader;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Pcap error: {0}")]
    Pcap(#[from] pcap_file::PcapError),
    #[error("BGP parser error: {0}")]
    Parser(#[from] bgpkit_parser::error::ParserError),
    #[error("The TCP stream is missing a marker.")]
    MissingMarker,
    #[error("Could not deliver all BGP messges in order.")]
    CouldNotDeliverPackets,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Msg {
    pub time: f64,
    pub src_mac: MacAddress,
    pub dst_mac: MacAddress,
    pub src_ip: Ipv4Addr,
    pub dst_ip: Ipv4Addr,
    pub msg: BgpUpdateMessage,
}
#[derive(Debug)]
struct TmpMsg {
    time: f64,
    src_mac: [u8; 6],
    dst_mac: [u8; 6],
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    flow: Flow,
    data: Bytes,
}

// TODO: we might want to implement some kind of filtering (a BGP message can be seen traversing multiple links)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Flow {
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
}
#[derive(Debug)]
pub struct FlowData {
    flow: Flow,
    next_expected: Option<u32>,
    open_bytes: Bytes,
    buffered: BTreeMap<u32, Bytes>,
}

impl FlowData {
    fn recv<'a>(&'a mut self, tcp: &'a TcpSlice<'_>) -> Option<Bytes> {
        let mut seq = tcp.sequence_number();
        if self.next_expected.is_none() {
            self.next_expected = Some(seq);
        }
        let len = tcp.payload().len();
        if len == 0 {
            return None;
        }
        let exp = self.next_expected.as_mut().unwrap();

        let mut payload = Bytes::from(tcp.payload().to_vec());

        match partial_deliver(&mut seq, &mut payload, *exp) {
            DeliverOutcome::CanDeliver => {}
            DeliverOutcome::Enqueue => {
                self.buffered.insert(seq, payload);
                return None;
            }
            DeliverOutcome::Ignore => return None,
        }

        // if we get here, then the current packet is the one we expected
        *exp = seq.wrapping_add(len as u32);

        let mut bytes = BytesMut::from(std::mem::take(&mut self.open_bytes));
        bytes.extend_from_slice(&payload);

        // we must repeat that thing here twice, because the sequence number may overflow (we go
        // through that thing in order)
        for _ in 0..2 {
            // try to deliver each packet in the queue.
            for (mut buffered_seq, mut buffered_pkt) in std::mem::take(&mut self.buffered) {
                match partial_deliver(&mut buffered_seq, &mut buffered_pkt, *exp) {
                    DeliverOutcome::CanDeliver => {
                        *exp = buffered_seq.wrapping_add(buffered_pkt.len() as u32);
                        bytes.extend_from_slice(&buffered_pkt);
                    }
                    DeliverOutcome::Enqueue => {
                        self.buffered.insert(buffered_seq, buffered_pkt);
                    }
                    DeliverOutcome::Ignore => {}
                }
            }
        }

        Some(bytes.into())
    }
}

const MID: u32 = 1 << 31;

enum DeliverOutcome {
    CanDeliver,
    Enqueue,
    Ignore,
}

fn partial_deliver(seq: &mut u32, payload: &mut Bytes, exp: u32) -> DeliverOutcome {
    match cmp_seq(*seq, exp) {
        Ordering::Greater => return DeliverOutcome::Enqueue,
        Ordering::Equal => return DeliverOutcome::CanDeliver,
        Ordering::Less => {
            // we need to check if we can partially deliver it!
        }
    }
    let len = payload.len();
    let end = seq.wrapping_add(len as u32);
    if cmp_seq(exp, end).is_ge() {
        // ignore that packet, the entire packet is before the expected sequence number
        return DeliverOutcome::Ignore;
    }
    // some parts of the packet are within the range.
    let remove_len =
        if *seq < exp { (exp - *seq) as usize } else { payload.len() - (end - exp) as usize };
    assert!(remove_len < len);
    // remove the beginning of the packet
    *seq = exp;
    payload.advance(remove_len);
    assert_eq!(seq.wrapping_add(payload.len() as u32), end);

    DeliverOutcome::CanDeliver
}

fn cmp_seq(a: u32, b: u32) -> Ordering {
    // check if both have the same MSB
    if (a & MID) == (b & MID) {
        a.cmp(&b)
    } else {
        // they don't have the same MSB.
        // Turn one of the numbers, and then compare again (but this time in reverse).
        a.wrapping_add(MID).cmp(&b).reverse()
    }
}

/// An iterator over all BGP messages observed from a PCAP file.
#[derive(Debug)]
pub struct BgpIterator {
    cap: PcapReader<BufReader<File>>,
    sessions: HashMap<Flow, FlowData>,
    current_pkt: Option<TmpMsg>,
    parsed_first_event: HashSet<(Ipv4Addr, Ipv4Addr)>,
}

impl BgpIterator {
    /// Create a new BGP iterator from a recorded PCAP file. The `filename` must be the path to an uncompressed
    /// pcap file.
    ///
    /// The BGP parser will yield individual BGP messages! If multiple messages are packed into a
    /// single IP packet, then the iterator will yield multiple messages for this packet. If a
    /// parsing error occurred, then the iterator will return an Error, and advance the TCP stream
    /// up to the next BGP marker, so parsing can continue.
    pub fn new(
        filename: impl AsRef<Path>,
        // watchlist: impl IntoIterator<Item = Watcher>, ------ We might want to implement some kind of filtering
    ) -> Result<Self, Error> {
        let file = File::open(filename)?;
        let bufreader = BufReader::new(file);
        let cap = PcapReader::new(bufreader)?;
        Ok(Self {
            cap,
            sessions: Default::default(),
            current_pkt: None,
            parsed_first_event: HashSet::default(),
        })
    }

    /// Check that all buffers are empty.
    pub fn check_buffers(&self) -> Result<(), Error> {
        let mut found_buffered = false;
        for session in self.sessions.values() {
            if !session.buffered.is_empty() {
                log::error!(
                    "Buffered packets remain on BGP session from {} to {}:\nPackets in the queue: {:?}",
                    session.flow.src_ip,
                    session.flow.dst_ip,
                    session.buffered.keys(),
                );
                found_buffered = true;
            }
        }
        if found_buffered {
            Err(Error::CouldNotDeliverPackets)
        } else {
            Ok(())
        }
    }

    fn next_relevant_packet(&mut self) -> Option<TmpMsg> {
        while let Some(Ok(packet)) = self.cap.next_packet() {
            // try to parse the packet as ethernet
            let Ok(pkt) = SlicedPacket::from_ethernet(&packet.data) else {
                continue;
            };
            let Some(LinkSlice::Ethernet2(eth)) = pkt.link else {
                continue;
            };
            let Some(InternetSlice::Ipv4(ip)) = pkt.net else {
                continue;
            };
            let Some(TransportSlice::Tcp(tcp)) = pkt.transport else {
                continue;
            };
            if tcp.destination_port() != 179 && tcp.source_port() != 179 {
                continue;
            }

            let time = packet.timestamp.as_secs_f64();

            let src_mac = eth.source();
            let dst_mac = eth.destination();
            let src_ip = ip.header().source_addr();
            let dst_ip = ip.header().destination_addr();

            // TODO: maybe we want to implement some kind of filtering

            let flow = Flow { src_ip, dst_ip };

            // get the session
            let session = self.sessions.entry(flow).or_insert_with(|| FlowData {
                flow,
                next_expected: None,
                open_bytes: Bytes::new(),
                buffered: Default::default(),
            });

            let Some(msg) = session.recv(&tcp) else {
                continue;
            };

            return Some(TmpMsg { time, src_mac, dst_mac, src_ip, dst_ip, flow, data: msg });
        }

        None
    }
}

fn next_bgp_msg(data: &mut Bytes) -> Result<Option<BgpMessage>, Error> {
    if data.len() < 19 {
        return Ok(None);
    }
    if data[..16].iter().any(|x| *x != 0xff) {
        return Err(Error::MissingMarker);
    }
    let len = BigEndian::read_u16(&data[16..18]) as usize;

    if data.len() < len {
        return Ok(None);
    }

    let mut bgp_data = data.split_to(len);
    Ok(Some(parse_bgp_message(&mut bgp_data, false, &AsnLength::Bits32)?))
}

/// Convert a bgpkit_parser `BgpUpdateMessage` to a sequence of bgpsim `BgpEvent`s.
/// Implicitly filter out all events pertaining prefixes that are not compatible with the BGPSim `Network`'s
/// `Prefix` type
fn update_message_to_bgp_events<P, A>(
    addressor: &A,
    update_msg: BgpUpdateMessage,
) -> Vec<BgpEvent<P>>
where
    P: Prefix,
    A: Addressor<P>,
{
    let mut bgp_events: Vec<BgpEvent<P>> = Vec::new();
    let BgpUpdateMessage { withdrawn_prefixes, attributes, announced_prefixes } = update_msg;
    // First withdraw the withdrawals
    bgp_events.extend(withdrawn_prefixes.iter().filter_map(|withdrawal| match withdrawal.prefix {
        IpNet::V4(v4_prefix) => try_collapse_prefix::<P>(v4_prefix).map(|p| BgpEvent::Withdraw(p)),
        IpNet::V6(_) => {
            panic!("I tried converting an IPv6 network, how did we get here?")
        }
    }));

    // Then update the updates
    bgp_events.extend(announced_prefixes.iter().filter_map(|announce| {
        match announce.prefix {
            IpNet::V4(v4_prefix) => try_collapse_prefix::<P>(v4_prefix)
                .map(|p| BgpEvent::Update(build_route(addressor, p, attributes.clone()))),
            IpNet::V6(_) => {
                panic!("I tried converting an IPv6 network, how did we get here?")
            }
        }
    }));
    bgp_events
}

/// This function checks wether or not an Ipv4 prefix can be converted into a prefix of type P
fn try_collapse_prefix<P>(v4_prefix: Ipv4Net) -> Option<P>
where
    P: Prefix,
{
    // Try to convert the prefix and catch panics
    let p = catch_unwind(AssertUnwindSafe(|| P::from(v4_prefix))).ok()?;
    // Check that the prefix was converted to the correct one
    if Into::<Ipv4Net>::into(p) == v4_prefix {
        Some(p)
    } else {
        debug!(
            "Prefix {} is not addressable, as Prefix is of type {}",
            v4_prefix.to_string(),
            std::any::type_name::<P>()
        );
        None
    }
}

/// Convert a parsed message to a sequence of bgpsim `Event`s.
/// The priority here is used to be the timestamp of the device.
pub fn message_to_events<P, A>(addressor: &A, msg: Msg) -> Vec<Event<P, NotNan<f64>>>
where
    P: Prefix,
    A: Addressor<P>,
{
    let bgp_events = update_message_to_bgp_events(addressor, msg.msg);

    let src = addressor.find_address(msg.src_ip).unwrap();
    let dst = addressor.find_address(msg.dst_ip).unwrap();
    let p = NotNan::new(msg.time).expect("timestamp should not be NaN");

    bgp_events.iter().map(|bgp_event| Event::Bgp { p, src, dst, e: bgp_event.clone() }).collect()
}

/// Parse a bgpkit parser `Attributes` field to build BGPSim `BgpRoute` structs
fn build_route<P, A>(addressor: &A, prefix: P, attr: Attributes) -> BgpRoute<P>
where
    P: Prefix,
    A: Addressor<P>,
{
    // Flatten the segments of the AS path
    let as_path: Vec<AsId> = attr
        .as_path()
        .unwrap()
        .segments
        .iter()
        .flat_map(|seg| {
            seg.to_u32_vec_opt(false).unwrap().iter().map(AsId::from).collect::<Vec<AsId>>()
        })
        .collect();
    // Get the NodeIndex of the next hop from the GNS3 network
    let next_hop_addr = match attr.next_hop().unwrap() {
        IpAddr::V4(addr) => addr,
        _ => panic!("The next hop attribute is supposed to have an IPv4 address"),
    };
    let next_hop = addressor.find_address(next_hop_addr).unwrap();
    // Extract the communities
    let community = attr
        .iter_communities()
        .map(|meta_community| match meta_community {
            MetaCommunity::Plain(comm) => match comm {
                Community::Custom(_, c) => c.into(),
                _ => panic!("The type of plain community is supposed to be custom"),
            },
            _ => panic!("The type of metacommunity is supposed to be Plain"),
        })
        .collect();
    // Extract the originator ID
    let originator_id =
        attr.origin_id().and_then(|originator_ip| addressor.find_address(originator_ip).ok());
    // Extract the cluster list
    let cluster_list = attr
        .clusters()
        .unwrap_or(&[])
        .iter()
        .map(|&c| {
            let address = Ipv4Addr::from(c);
            addressor.find_address(address).unwrap()
        })
        .collect();
    BgpRoute {
        prefix,
        as_path,
        next_hop,
        local_pref: attr.local_preference(),
        med: attr.multi_exit_discriminator(),
        community,
        originator_id,
        cluster_list,
    }
}

impl Iterator for BgpIterator {
    type Item = Result<Msg, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let TmpMsg { time, src_mac, dst_mac, src_ip, dst_ip, flow, mut data } =
                self.current_pkt.take().or_else(|| self.next_relevant_packet())?;

            // if data is empty, go back and try again
            if data.is_empty() {
                continue;
            }

            let len_before = data.len();

            match next_bgp_msg(&mut data) {
                Ok(Some(msg)) => {
                    // parsed a BGP message! Remember that we have parsed one message on that
                    // session
                    self.parsed_first_event.insert((src_ip, dst_ip));
                    // put the data back if there are more bytes to parse
                    if !data.is_empty() {
                        self.current_pkt =
                            Some(TmpMsg { time, src_mac, dst_mac, src_ip, dst_ip, flow, data });
                    }
                    match msg {
                        BgpMessage::Update(msg) => {
                            return Some(Ok(Msg {
                                time,
                                src_mac: src_mac.into(),
                                dst_mac: src_mac.into(),
                                src_ip,
                                dst_ip,
                                msg,
                            }));
                        }
                        _ => {
                            // ignore these packets
                        }
                    }
                }
                Ok(None) => {
                    // put the remaining bytes into the session
                    if !data.is_empty() {
                        self.sessions.get_mut(&flow).unwrap().open_bytes = data;
                    }
                }
                Err(e) => {
                    // advance to the next marker if possible
                    let len_after = if advance_to_next_marker(&mut data) {
                        let len_after = data.len();
                        self.current_pkt =
                            Some(TmpMsg { time, src_mac, dst_mac, src_ip, dst_ip, flow, data });
                        len_after
                    } else {
                        // otherwise, discard all data.
                        0
                    };
                    // log the message, but only if we already parsed a message on that session
                    if self.parsed_first_event.contains(&(src_ip, dst_ip)) {
                        log::error!(
                            "Parsing error occurred on session {src_ip} --> {dst_ip}. Lost {} bytes. Error: {e}",
                            len_before - len_after
                        );

                        return Some(Err(e));
                    }
                    // we have never before parsed a BGP message on that session. The first BGP
                    // message may not be parseable, because we start monitoring somewhere in
                    // between. Do nothing, which means go to the next packet in the PCAP!
                }
            }
        }
    }
}

fn advance_to_next_marker(data: &mut Bytes) -> bool {
    if let Some(pos) = data.windows(16).position(|x| x.iter().all(|b| *b == 0xff)) {
        data.advance(pos);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_cmp_seq() {
        assert_eq!(cmp_seq(0x8100_0000, 0x8100_0000), Ordering::Equal);

        assert_eq!(cmp_seq(0x8100_0000, 0x8010_0000), Ordering::Greater);
        assert_eq!(cmp_seq(0x8100_0000, 0x8110_0000), Ordering::Less);

        assert_eq!(cmp_seq(0x8100_0000, 0x0110_0000), Ordering::Greater);
        assert_eq!(cmp_seq(0x8100_0000, 0x0010_0000), Ordering::Less);

        assert_eq!(cmp_seq(0x0100_0000, 0x8110_0000), Ordering::Greater);
        assert_eq!(cmp_seq(0x0100_0000, 0x8010_0000), Ordering::Less);
    }

    #[test]
    fn test_advance() {
        let mut data: Vec<u8> = vec![1, 2, 3, 4];
        data.extend(std::iter::repeat(0xff).take(16));
        data.push(5);
        data.push(6);
        data.push(7);

        let mut bytes = Bytes::from(data);

        assert!(advance_to_next_marker(&mut bytes));
        assert_eq!(&bytes[..16], &[0xff; 16]);
        assert!(advance_to_next_marker(&mut bytes));
        bytes.advance(1);
        assert!(!advance_to_next_marker(&mut bytes));
    }

    #[test]
    fn parse() {
        let mut bytes = Bytes::from(vec![
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0x00, 0x13, 0x04, 0xde, 0xad,
        ]);
        let msg = next_bgp_msg(&mut bytes).unwrap().unwrap();
        assert!(matches!(msg, BgpMessage::KeepAlive));
        assert_eq!(&bytes[..], &[0xde, 0xad]);
    }
}
