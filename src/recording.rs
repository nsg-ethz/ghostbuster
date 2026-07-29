use bgpsim::{
    bgp::{BgpEvent, BgpRoute},
    event::{Event, EventQueue},
    ospf::OspfImpl,
    prelude::NetworkFormatter,
    types::{Prefix, RouterId, SinglePrefix},
};
use bgpsim_gns3::{
    parser::{message_to_events, BgpIterator},
    Gns3Network,
};
use itertools::Itertools;
use log::{info, warn};
use ordered_float::NotNan;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};

/// A recording of messages organized by prefix, router and session
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(deserialize = "P: for<'a> serde::Deserialize<'a>"))]
pub struct Recording<P: Prefix>(pub HashMap<P, RouterSequences>);
// TODO: Maybe turn this into a P::Map (Won't work as is due to the fact tha BinaryHeap does not impl PartialEq)

impl<P: Prefix> Default for Recording<P> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

impl<P: Prefix> Recording<P> {
    /// This function constructs a monitoring interval from a series of packet captures taken from a
    /// GNS3 network.  
    pub fn from_pcaps<Q, Ospf>(
        pcaps: HashMap<(RouterId, RouterId), PathBuf>,
        gns3_net: &Gns3Network<P, Q, Ospf>,
    ) -> Self
    where
        Q: EventQueue<P>,
        Ospf: OspfImpl,
    {
        let addressor = gns3_net.get_addressor();

        // Build the result using the provided structures
        let mut result = Recording::<P>::default();

        for ((a, b), pcap) in pcaps {
            let mut messages = 0;
            for bgp_message in BgpIterator::new(&pcap).unwrap() {
                messages += 1;
                let events = message_to_events(addressor, bgp_message.unwrap());
                // File the events into the correct sessions
                events
                    .into_iter()
                    .for_each(|event| result.allocate_events((&a, &b), event));
            }
            info!(
                "Pcap '{:?}' contains {} messages",
                pcap.file_name().unwrap(),
                messages
            );
        }

        // Sort the resulting recording
        result.sort_self();
        result
    }

    /// This function constructs a recording from a vector of events
    // TODO: Very experimental, as the workaround with the index shows
    pub fn from_vec<T>(events: Vec<Event<P, T>>) -> Self {
        if events.is_empty() {
            warn!("Attempting to get a recording of an empty vector")
        }
        // Workaround for events without a priority or one that is incompatible with the monitoring
        let mut i = NotNan::new(0.0).expect("I mean...");
        let mut result = events
            .into_iter()
            .fold(Recording::<P>::default(), |mut acc, event| {
                if let Event::Bgp { src, dst, e, .. } = &event {
                    let prefix = e.prefix();
                    let event = Event::Bgp {
                        p: i,
                        src: *src,
                        dst: *dst,
                        e: e.clone(),
                    };
                    i += 1.0;
                    // Insert outgoing
                    acc.insert_event(&prefix, src, event.clone());
                    // Insert incoming
                    acc.insert_event(&prefix, dst, event.clone());
                }
                acc
            });
        result.sort_self();
        result
    }

    // Sorts every message sequence by the timing/priority information
    fn sort_self(&mut self) {
        for events_by_router in self.0.values_mut() {
            for message_sequence in events_by_router.values_mut() {
                message_sequence.sort();
            }
        }
    }

    /// Helper method to insert an event into the nested structure
    fn insert_event(&mut self, prefix: &P, router: &RouterId, event: Event<P, NotNan<f64>>) {
        // Get or create RouterSequences for this prefix
        let router_messages = self
            .0
            .entry(*prefix)
            .or_insert_with(RouterSequences::default);
        // Get or create a MessageSequence for this router
        let message_sequence = router_messages
            .entry(*router)
            .or_insert_with(MessageSequence::default);

        message_sequence.push(TimestampedEvent::from(event));
    }

    /// Helper method to sort an event into the corresponding sessions based on the link it was recorded on
    fn allocate_events(&mut self, (a, b): (&RouterId, &RouterId), event: Event<P, NotNan<f64>>) {
        // Each event is allocated according to the link it was recorded on, as events can show up in a pcap while they are
        // only transiting the network
        match &event {
            Event::Bgp { p: _, src, dst, e } => {
                let prefix = e.prefix();
                if src == a || src == b {
                    // The source of the event is one of the link endpoints
                    self.insert_event(&prefix, src, event.clone());
                }
                if dst == a || dst == b {
                    // The destination of the event is one of the link endpoints
                    self.insert_event(&prefix, dst, event.clone());
                }
            }
            Event::Ospf { .. } => panic!("How did we end up trying to allocate an OSPF event?"),
        }
    }

    /// Filter for specific routers, consumes self and returns a recording of only the events we captured
    /// on those routers
    pub fn filter_routers(self, routers: &std::collections::HashSet<RouterId>) -> Self {
        Self(
            self.0
                .into_iter()
                .map(|(prefix, router_messages)| {
                    let filtered_messages: RouterSequences = router_messages
                        .into_iter()
                        .filter(|(router_id, _)| routers.contains(router_id))
                        .collect();
                    (prefix, filtered_messages)
                })
                .filter(|(_, router_messages)| !router_messages.is_empty())
                .collect(),
        )
    }

    /// Filter for specific prefixes, consumes self and returns a recording of only the events we captured
    /// on those prefixes
    pub fn filter_prefixes(self, prefixes: &std::collections::HashSet<P>) -> Self {
        Self(
            self.0
                .into_iter()
                .filter(|(prefix, _)| prefixes.contains(&prefix))
                .collect(),
        )
    }

    /// Filter out specific prefixes, consumes self and returns a recording with no events for those prefixes
    pub fn filter_out_prefixes(self, prefixes: &std::collections::HashSet<P>) -> Self {
        Self(
            self.0
                .into_iter()
                .filter(|(prefix, _)| !prefixes.contains(&prefix))
                .collect(),
        )
    }

    /// Get the recorded sequence of events for a specific prefix and router
    /// Returns events in chronological order (earliest timestamp first)
    pub fn get_events(&self, prefix: P, router: RouterId) -> Option<Vec<Event<P, NotNan<f64>>>> {
        self.0
            .get(&prefix)
            .and_then(|router_messages| router_messages.get(&router))
            .map(|message_sequence| {
                message_sequence
                    .iter()
                    .map(|t_e| t_e.0.clone().with_prefix(prefix))
                    .collect_vec()
            })
    }
}

impl<P: Prefix> IntoIterator for Recording<P> {
    type Item = (P, RouterSequences);
    type IntoIter = std::collections::hash_map::IntoIter<P, RouterSequences>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, P, Q, Ospf> NetworkFormatter<'a, P, Q, Ospf> for Recording<P>
where
    P: Prefix,
    Q: EventQueue<P>,
    Ospf: bgpsim::ospf::OspfImpl,
{
    fn fmt(&self, net: &'a bgpsim::network::Network<P, Q, Ospf>) -> String {
        let mut result = String::from("Recording {");
        for (prefix, router_messages) in &self.0 {
            result.push_str(&format!(" {}: {{", prefix));
            for (router_id, message_sequence) in router_messages {
                result.push_str(&format!(" {}: [", router_id.fmt(net)));
                // Iterate forward through sorted events (earliest first)
                let mut first = true;
                for event in message_sequence.iter() {
                    if !first {
                        result.push_str(", ");
                    }
                    first = false;
                    match &event.0 {
                        Event::Bgp { p, src, dst, e } => result.push_str(&format!(
                            "{}: ({})->({}) {}",
                            p,
                            src.fmt(&net),
                            dst.fmt(&net),
                            e.clone().with_prefix(*prefix).fmt(&net)
                        )),
                        _ => (),
                    }
                }
                result.push_str("],");
            }
            result.push_str(" },");
        }
        result.push_str(" }");
        result
    }

    fn fmt_multiline_indent(
        &self,
        net: &'a bgpsim::network::Network<P, Q, Ospf>,
        indent: usize,
    ) -> String {
        let spc = " ".repeat(indent);
        let mut result = String::from("Recording {\n");
        for (prefix, router_messages) in &self.0 {
            result.push_str(&format!("{spc}  {}: {{\n", prefix));
            for (router_id, message_sequence) in router_messages {
                result.push_str(&format!("{spc}    {}: [\n", router_id.fmt(net)));

                // First pass: compute max widths for this block
                let mut max_p = 0usize;
                let mut max_edge = 0usize;
                for event in message_sequence.iter() {
                    if let Event::Bgp { p, src, dst, .. } = &event.0 {
                        max_p = max_p.max(format!("{}", p).len());
                        let edge = format!("({})->({})", src.fmt(net), dst.fmt(net));
                        max_edge = max_edge.max(edge.len());
                    }
                }

                // Second pass: format events using computed widths (in chronological order)
                for event in message_sequence.iter() {
                    if let Event::Bgp { p, src, dst, e } = &event.0 {
                        let edge = format!("({})->({})", src.fmt(net), dst.fmt(net));

                        result.push_str(&format!(
                            "{spc}      {p:<p_w$} : {edge:<edge_w$} {event},\n",
                            p = p,
                            p_w = max_p,
                            edge = edge,
                            edge_w = max_edge,
                            event = e.clone().with_prefix(*prefix).fmt(&net),
                        ));
                    }
                }
                result.push_str(&format!("{spc}    ],\n"));
            }
            result.push_str(&format!("{spc}  }},\n"));
        }
        result.push_str(&format!("{spc}}}"));
        result
    }
}

/// Stores messages organized by router (for all routers)
pub(crate) type RouterSequences = HashMap<RouterId, MessageSequence>;
/// Stores a sequence of timestamped BGP events, ordered by time
pub(crate) type MessageSequence = Vec<TimestampedEvent>;

/// A BGPSim event with an associated timestamp
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimestampedEvent(pub Event<SinglePrefix, NotNan<f64>>);

impl<P: Prefix> From<Event<P, NotNan<f64>>> for TimestampedEvent {
    fn from(event: Event<P, NotNan<f64>>) -> Self {
        let Event::Bgp { p, src, dst, e } = event else {
            panic!("How did we end up trying to allocate an OSPF event?");
        };

        let e = match e {
            BgpEvent::Withdraw(_) => BgpEvent::Withdraw(SinglePrefix::default()),
            BgpEvent::Update(r) => BgpEvent::Update(BgpRoute {
                prefix: SinglePrefix::default(),
                as_path: r.as_path,
                next_hop: r.next_hop,
                local_pref: r.local_pref,
                med: r.med,
                community: r.community,
                originator_id: r.originator_id,
                cluster_list: r.cluster_list,
            }),
        };

        Self(Event::Bgp { p, src, dst, e })
    }
}

impl PartialEq for TimestampedEvent {
    fn eq(&self, other: &Self) -> bool {
        self.0.priority() == other.0.priority()
    }
}

impl Eq for TimestampedEvent {}

impl PartialOrd for TimestampedEvent {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimestampedEvent {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.priority().cmp(&other.0.priority())
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::Write;

    use bgpsim::{
        bgp::BgpEvent,
        event::{BasicEventQueue, Event},
        ospf::GlobalOspf,
        prelude::NetworkFormatter,
        types::SimplePrefix,
    };
    use bgpsim_gns3::Gns3Network;
    use log::info;

    use crate::tests::line_network;
    use serial_test::serial;
    use test_log::test;

    // TODO: need to add an explicit test for the case in which we generate multiple updates in the same message
    //
    // This test builds a network on the GNS3 server, so it has to be serialised against the tests in
    // `tests::test_gns3` that do the same.
    #[test]
    #[serial]
    fn test_stable_order() {
        // Explicit test for the case in which we generate multiple updates in the same message
        // We need to make sure that we insert the messages into the recording in a correct order

        // Dummy gns3 network
        let (net, (e, b)) = line_network::<SimplePrefix, GlobalOspf, BasicEventQueue<SimplePrefix>>(
            BasicEventQueue::default(),
        );
        let gns3_net = Gns3Network::new(
            "line_network",
            &net,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            HashMap::new(),
        )
        .unwrap();

        let bytes = vec![
            0xd4, 0xc3, 0xb2, 0xa1, 0x02, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0xff, 0xff, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x1b, 0xa4, 0x70, 0x69,
            0x03, 0x87, 0x0d, 0x00, 0x96, 0x00, 0x00, 0x00, 0x96, 0x00, 0x00, 0x00, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0x0a, 0x74, 0xa7, 0xa0, 0x66, 0x61, 0x08, 0x00, 0x45, 0x00,
            0x00, 0x88, 0x00, 0x01, 0x00, 0x00, 0x01, 0x06, 0xa1, 0xed, 0x0b, 0xc0, 0x00, 0x02,
            0x0b, 0xc0, 0x00, 0x01, 0x00, 0xb3, 0x00, 0xb3, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
            0x00, 0x01, 0x50, 0x18, 0x20, 0x00, 0xf9, 0x1b, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x30,
            0x02, 0x00, 0x00, 0x00, 0x15, 0x40, 0x01, 0x01, 0x00, 0x40, 0x02, 0x00, 0x40, 0x03,
            0x04, 0x0b, 0xc0, 0x00, 0x02, 0x40, 0x05, 0x04, 0x00, 0x00, 0x00, 0x32, 0x18, 0x64,
            0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0x00, 0x30, 0x02, 0x00, 0x00, 0x00, 0x15, 0x40, 0x01, 0x01,
            0x00, 0x40, 0x02, 0x00, 0x40, 0x03, 0x04, 0x0b, 0xc0, 0x00, 0x02, 0x40, 0x05, 0x04,
            0x00, 0x00, 0x00, 0x64, 0x18, 0x64, 0x00, 0x00,
        ];

        // Pack the bytes in a temporary pcap file that can be read by the recording
        let pcap_path =
            std::env::temp_dir().join(format!("test_stable_order_{}.pcap", std::process::id()));
        let mut file = File::create(&pcap_path).unwrap();
        file.write_all(&bytes).unwrap();
        file.flush().unwrap();

        // Ingest the bytes as being from that pcap file and read it using from_pcaps
        let mut pcaps = HashMap::new();
        pcaps.insert((e, b), pcap_path.clone());

        let recording = super::Recording::from_pcaps(pcaps, &gns3_net)
            .filter_routers(&net.internal_indices().collect());

        info!("{}", recording.fmt_multiline(&net));
        let sequence = recording
            .0
            .get(&SimplePrefix::from(0))
            .unwrap()
            .get(&b)
            .unwrap();
        let mut prev_lp = 0;
        for msg in sequence.iter() {
            match &msg.0 {
                Event::Bgp { e, .. } => match e {
                    BgpEvent::Withdraw(_) => panic!("Not a WITHDRAW message"),
                    BgpEvent::Update(bgp_route) => {
                        assert!(prev_lp < bgp_route.local_pref.unwrap());
                        prev_lp = bgp_route.local_pref.unwrap();
                    }
                },
                Event::Ospf { .. } => panic!("Not an OSPF message"),
            }
        }

        // Clean up temporary file
        let _ = std::fs::remove_file(&pcap_path);
    }
}
