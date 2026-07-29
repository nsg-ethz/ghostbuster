use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::u32;

use bgpsim::bgp::BgpEvent;
use bgpsim::bgp::BgpRibEntry;
use bgpsim::bgp::BgpSessionType;
use bgpsim::event::Event;
use bgpsim::event::EventQueue;
use bgpsim::network::Network;
use bgpsim::ospf::OspfImpl;
use bgpsim::prelude::NetworkFormatter;
use bgpsim::route_map::RouteMap;
use bgpsim::route_map::RouteMapDirection;
use bgpsim::route_map::{RouteMapMatch, RouteMapSet};
use bgpsim::router::BgpProcess;
use bgpsim::types::PrefixMap;
use bgpsim::types::{Prefix, RouterId};
use itertools::Itertools;
use serde::Serialize;
use strum_macros::EnumDiscriminants;

/// A locality defines a spot on the network in which a failure can occur
/// There are two types of localities, [router]- and [session]-specific ones.
/// Router specific ones represent malfunctioning ingress or egress pipelines of a router r. Either all the messages
/// destined to router r or all the messages sourced from router r are affected by such failures. These
/// are represented by localities of the form `(None, Some(r))` or `(Some(r), None)`, respectively.
/// Session specific ones represent issues which are tied to a specific session. Only the messages which travel on
/// this exact session will be affected. These localities are in the form `(Some(src), Some(dst))`
pub type Locality = (Option<RouterId>, Option<RouterId>);

/// Failure modeling
// In order to exhaustively model failures, we need to model both constructive and destructive failures
// Constructive failures are modeled by injecting random messages into the queue (not considered here)
// Destructive failures are modeled by filtering events from the queue
// Transformative failures are modeled by modifying the events in the queue

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, EnumDiscriminants, Serialize)]
pub enum Failure {
    // [Destructive Failures] Drops for both update and withdraw messages
    BGPDropUpdate(Locality),
    BGPDropWithdraw(Locality),

    // [Transformative Failures] Associated with the modification of a
    // The local preferences get overwritten to a single value
    BGPChangeLocalPref(Locality, u32),
    // The communities get either added or removed, depending on the sign of the value
    BGPChangeCommunity(Locality, i32),
}

impl Failure {
    // This function applies a failure to an event
    pub fn apply<P: Prefix, T: Clone>(&self, event: Event<P, T>) -> Option<Event<P, T>> {
        if !self.filter(&event) {
            return Some(event);
        }
        // Copy the priority (in our particular case it encodes a reference to the event
        // that triggered this one) of the event
        let p = event.priority().clone();
        match event {
            Event::Bgp { src, dst, e, .. } => {
                // Match on both self and the event to keep things clean
                match (self, e) {
                    // For the destructive failures, we simply drop the event
                    (Failure::BGPDropUpdate(_), BgpEvent::Update(_))
                    | (Failure::BGPDropWithdraw(_), BgpEvent::Withdraw(_)) => None,

                    // Change the local preference of the route only on updates
                    (Failure::BGPChangeLocalPref(_, lp), BgpEvent::Update(route)) => {
                        let mut new_route = route.clone();
                        new_route.local_pref = Some(*lp);
                        Some(Event::Bgp {
                            p,
                            src,
                            dst,
                            e: BgpEvent::Update(new_route),
                        })
                    }

                    // Change the community values of the route only on updates
                    (Failure::BGPChangeCommunity(_, c), BgpEvent::Update(route)) => {
                        let mut new_route = route.clone();
                        if *c < 0 {
                            // Remove the community value if it exists
                            new_route.community.retain(|&x| x != (-*c) as u32);
                        } else {
                            // Add the community value, even if it already exists
                            new_route.community.insert(*c as u32);
                        }
                        Some(Event::Bgp {
                            p,
                            src,
                            dst,
                            e: BgpEvent::Update(new_route),
                        })
                    }

                    // For any other combination, return the original event.
                    (_, e) => Some(Event::Bgp { p, src, dst, e }),
                }
            }
            // For non-BGP events, return them unchanged.
            _ => Some(event),
        }
    }

    // Helper funtion to filter by locality
    // Returns true if we get a match on the locality
    fn filter<P: Prefix, T>(&self, event: &Event<P, T>) -> bool {
        match event {
            Event::Bgp { src, dst, .. } => self.matches_locality(*src, *dst),
            // We never match on other kinds of events
            _ => false,
        }
    }

    pub fn matches_locality(&self, src: RouterId, dst: RouterId) -> bool {
        // Get the locality of the failure
        let locality = self.get_locality();

        match (locality.0, locality.1) {
            // Session specific localities
            (Some(a), Some(b)) => src == a && dst == b,
            // Router specific localities
            (Some(a), None) => src == a,
            (None, Some(b)) => dst == b,
            (None, None) => panic!("Invalid locality, both are None"),
        }
    }

    // Get the locality of an event
    pub(crate) fn get_locality(&self) -> Locality {
        match self {
            Failure::BGPDropUpdate(loc) => *loc,
            Failure::BGPDropWithdraw(loc) => *loc,
            Failure::BGPChangeLocalPref(loc, _) => *loc,
            Failure::BGPChangeCommunity(loc, _) => *loc,
        }
    }
}

impl<'n, P: Prefix, Q, Ospf: OspfImpl> NetworkFormatter<'n, P, Q, Ospf> for Failure {
    fn fmt(&self, net: &'n Network<P, Q, Ospf>) -> String {
        // Helper function to format the localities
        fn fmt_locality<P: Prefix, Q, Ospf: OspfImpl>(
            loc: Locality,
            net: &Network<P, Q, Ospf>,
        ) -> String {
            match loc {
                (None, Some(r)) => format!("(*, Some({}))", r.fmt(&net)),
                (Some(r), None) => format!("(Some({}), *)", r.fmt(&net)),
                (Some(src), Some(dst)) => {
                    format!("(Some({}), Some({}))", src.fmt(&net), dst.fmt(&net))
                }
                _ => panic!("Invalid locality"),
            }
        }

        match self {
            Failure::BGPDropUpdate(loc) => {
                format!("BGPDropUpdate({})", fmt_locality(*loc, &net))
            }
            Failure::BGPDropWithdraw(loc) => {
                format!("BGPDropWithdraw({})", fmt_locality(*loc, &net))
            }
            Failure::BGPChangeLocalPref(loc, lp) => {
                format!("BGPChangeLocalPref({}, {})", fmt_locality(*loc, &net), lp)
            }
            Failure::BGPChangeCommunity(loc, c) => {
                format!(
                    "BGPChangeCommunity({}, {}{})",
                    fmt_locality(*loc, &net),
                    if c < &0 { "" } else { "+" },
                    c
                )
            }
        }
    }
}

/// A builder for a set of failures
///
/// we have this here because we want to be able to build up a set of failures in a more controlled manner
pub struct FailureSetBuilder {
    // Recording events allows us to restrict ourselves to failures that will actually have an impact on the network.
    // We keep track of a list of localities we have seen used by the advertisements. This set will be populated by the
    // events we ingest.
    // When receiving an event whose localities we have already explored, we do not need to explore the failures at those
    // localities again.
    localities: HashSet<Locality>,
    // We also keep track of all relevant localities to make sure we don't extract failures on, say, sessions which come in
    // from an external router or that modify all outgoing messages from the external router.
    // The reasoning here is that we do not control the external routers, so we cannot model failures on them.
    pub(crate) all_relevant_localities: HashSet<Locality>,
    // Attributes
    local_preferences: BTreeSet<u32>,
    communities: BTreeSet<u32>,
    // We keep track of the internal sessions in the network. The format is {from: {to_i: session_i}}
    // This is needed to extract the values of all relevant LPs and communities from the RIBs and route maps
    internal_sessions: HashMap<RouterId, HashMap<RouterId, BgpSessionType>>,
}
impl FailureSetBuilder {
    pub fn new() -> Self {
        Self {
            localities: HashSet::new(),
            all_relevant_localities: HashSet::new(),
            local_preferences: BTreeSet::new(),
            communities: BTreeSet::new(),
            internal_sessions: HashMap::new(),
        }
    }

    pub fn build_from_event<P: Prefix, T>(
        &mut self,
        event: &Event<P, T>,
    ) -> Option<HashSet<Failure>> {
        // We build a set of failures from a single event, taking into account the localities we have seen so far
        match event {
            // We only consider BGP events
            Event::Bgp { src, dst, .. } => {
                // Extract the localities from this event (every event affects three)
                let event_localities: HashSet<Locality> = HashSet::from([
                    (Some(*src), None),
                    (None, Some(*dst)),
                    (Some(*src), Some(*dst)),
                ]);
                // The localitites that we have not seen yet are the difference between the event's ones and the ones we have already seen.
                // Get the failures on the unseen ones
                // Additionally filter on only the localities we consider relevant
                let failures: HashSet<Failure> = event_localities
                    .difference(&self.localities)
                    .filter(|locality| self.all_relevant_localities.contains(locality))
                    .flat_map(|locality| self.failures_from_locality(*locality))
                    .collect();
                // Add the localities to the ones we have seen
                self.localities.extend(event_localities);
                Some(failures)
            }
            _ => None,
        }
    }

    pub fn ingest_network<P: Prefix, Q: EventQueue<P>, Ospf: OspfImpl>(
        &mut self,
        net: &Network<P, Q, Ospf>,
    ) {
        // Extract all the sessions of the internal indices
        self.internal_sessions = net
            .internal_indices()
            .map(|id| {
                let sessions: HashMap<RouterId, BgpSessionType> = net
                    .get_internal_router(id)
                    .unwrap()
                    .bgp
                    .get_sessions()
                    .clone();
                (id, sessions)
            })
            .collect();

        // We extract the information relevant for our failures only from the internal routers
        for internal_router in net.internal_routers() {
            let bgp_process: &BgpProcess<P> = &internal_router.bgp;

            // RIB information
            // Grab the relevant ribs from this router.
            // TODO: Some of these may be irrelevant
            bgp_process.get_rib_in().values().for_each(|table| {
                self.ingest_rib(table.values());
            });
            bgp_process.get_rib_out().values().for_each(|table| {
                self.ingest_rib(table.values());
            });
            self.ingest_rib(bgp_process.get_rib().values());

            // Route map information
            // Grab all existing route maps on this router
            let route_maps = bgp_process
                .get_sessions()
                .keys()
                .cartesian_product([RouteMapDirection::Incoming, RouteMapDirection::Outgoing])
                .flat_map(|(id, dir)| bgp_process.get_route_maps(*id, dir));
            // Extract the relevant attributes from the route maps
            route_maps.for_each(|route_map: &RouteMap<P>| {
                // Match statements
                route_map
                    .conds()
                    .iter()
                    .for_each(|rm_match: &RouteMapMatch<P>| {
                        // For each match statement, record what we match on
                        match rm_match {
                            RouteMapMatch::DenyCommunity(c) => self.communities.insert(*c),
                            RouteMapMatch::Community(c) => self.communities.insert(*c),
                            _ => false,
                        };
                    });
                // Set statements
                route_map.actions().iter().for_each(|rm_set: &RouteMapSet| {
                    // For each set statement, record what we set
                    match rm_set {
                        RouteMapSet::DelCommunity(c) => self.communities.insert(*c),
                        RouteMapSet::SetCommunity(c) => self.communities.insert(*c),
                        RouteMapSet::LocalPref(lp) => {
                            self.local_preferences.insert(lp.unwrap_or(100))
                        }
                        _ => false,
                    };
                });
            });
        }

        // Extract all possible relevant localities after we have populated the other fields
        self.all_relevant_localities = self.get_all_relevant_localities();
    }

    /// TODO: This build function does not take into account the events which have been ingested and thus doesn't
    /// care about the localities. This is a brute force implementation, but it will change soon
    pub fn build(&self) -> HashSet<Failure> {
        // Simply brute force all the localities
        self.all_relevant_localities
            .iter()
            .flat_map(|locality| self.failures_from_locality(*locality))
            .collect()
    }

    // Helper function to return an iterator of every possible failure at a specific locality
    fn failures_from_locality(&self, loc: Locality) -> Vec<Failure> {
        vec![
            // Destructive failures
            Failure::BGPDropUpdate(loc),
            Failure::BGPDropWithdraw(loc),
        ]
        .into_iter()
        .chain(
            // Transformative failures: Communities
            self.communities.iter().flat_map(|c| {
                let c = *c as i32;
                vec![
                    Failure::BGPChangeCommunity(loc, c),
                    Failure::BGPChangeCommunity(loc, -(c)),
                ]
            }),
        )
        .chain(
            // Transformative failures: Local Preferences
            // They require a bit more thought. In order to consider all possible options, we make sure to
            // consider the local preferences that fall in between the already observed ones
            self.get_all_relevant_local_preferences()
                .iter()
                .map(|lp| Failure::BGPChangeLocalPref(loc, *lp)),
        )
        .collect()
    }

    // Helper function to ingest the failure-relevant information from the entries in a rib
    fn ingest_rib<'a, I, P>(&mut self, rib_values: I)
    where
        I: IntoIterator<Item = &'a BgpRibEntry<P>>,
        P: 'a + Prefix, // Ensure `P` lives at least as long as `'a`
    {
        rib_values.into_iter().for_each(|entry: &BgpRibEntry<P>| {
            self.local_preferences
                .insert(entry.route.local_pref.unwrap_or(100));
            self.communities.extend(entry.route.community.clone());
        });
    }

    // Helper function to intersperse the local preferences with values in between them
    fn get_all_relevant_local_preferences(&self) -> Vec<u32> {
        if self.local_preferences.is_empty() {
            // println!(
            //     "[WARNING] Attempting to intersperse local preferences without any being stored"
            // );
            return Vec::new();
        }

        let mut extended_list = BTreeSet::new();
        let mut prev = None;
        for &value in &self.local_preferences {
            if let Some(prev_value) = prev {
                let step = (value - prev_value) / 2;
                extended_list.insert(prev_value + step);
            }
            extended_list.insert(value);
            prev = Some(value);
        }

        // Add lower and upper bounds
        let min = *extended_list.iter().next().unwrap();
        let max = *extended_list.iter().last().unwrap(); // Technically unchecked, but who is going to set LP to 2^31+1?
        extended_list.extend([min / 2, max * 2]);

        return extended_list.into_iter().collect();
    }

    // Helper function to get all the localities from the stored internal indices
    fn get_all_relevant_localities(&self) -> HashSet<Locality> {
        // Get all sessions
        let mut localities = HashSet::new();
        // Remember that the src router is always an internal one...
        for (src, sessions) in &self.internal_sessions {
            for (dst, session_type) in sessions {
                // Add session-specific localities
                localities.insert((Some(*src), Some(*dst)));
                if *session_type == BgpSessionType::EBgp {
                    // ...which means we have to double these edges, as the external router will never be a source
                    // WARNING: uncommenting the line below will cause us to consider failures on sessions where the external
                    //          router is the destination. Just be aware that this is a decision we make
                    //localities.insert((Some(*dst), Some(*src)));
                }
            }
            // The src routers are just internal, which means we can add both router-specific localities here
            localities.insert((None, Some(*src)));
            localities.insert((Some(*src), None));
        }
        localities
    }
}

#[cfg(test)]
pub(crate) mod test_failure {
    use std::{hash::Hash, vec};

    use super::*;
    use crate::tests::*;
    use bgpsim::{
        ospf::{local::OspfEvent, OspfArea},
        prelude::*,
    };

    // Helper function to get an Event out of a BgpEvent
    fn get_event(src: u32, dst: u32, event: BgpEvent<SinglePrefix>) -> Event<SinglePrefix, ()> {
        Event::Bgp {
            p: (),
            src: src.into(),
            dst: dst.into(),
            e: event,
        }
    }
    // Helper function to get a BgpRoute with specific characteristics
    fn get_default_route() -> BgpRoute<SinglePrefix> {
        let mut route = BgpRoute::new(0.into(), 0, vec![1, 2, 3], None, vec![1, 2, 3]);
        route.local_pref = Some(100);
        route.community = vec![1, 2, 3].into_iter().collect();
        route
    }

    // Helper function to disciminate the category of failure
    pub fn is_destructive_failure(failure: &Failure) -> bool {
        return (FailureDiscriminants::from(failure) == FailureDiscriminants::BGPDropUpdate)
            || (FailureDiscriminants::from(failure) == FailureDiscriminants::BGPDropWithdraw);
    }

    #[test]
    fn failure_filtering() {
        // Test all cases of failure filtering
        // Random BgpEvent to test with
        let event = BgpEvent::Update(get_default_route());
        // For the events we assign the `NodeIndex`es a: 0, b: 1, c: 2 ...
        let a_b = get_event(0, 1, event.clone());
        let a_c = get_event(0, 2, event.clone());
        let b_a = get_event(1, 0, event.clone());
        let b_c = get_event(1, 2, event.clone());
        let events = vec![a_b, a_c, b_a, b_c];
        // Test three localities
        let loc_a_b: Locality = (Some(0.into()), Some(1.into()));
        let loc_a_x: Locality = (Some(0.into()), None);
        let loc_x_a: Locality = (None, Some(0.into()));

        // Helper function to test the failure filtering on multiple events
        fn test_events(events: Vec<Event<SinglePrefix, ()>>, failure: Failure) -> Vec<bool> {
            events.iter().map(|e| failure.filter(e)).collect()
        }

        assert_eq!(
            test_events(events.clone(), Failure::BGPDropUpdate(loc_a_b)),
            vec![true, false, false, false],
            "Session specific locality filtering failed"
        );
        assert_eq!(
            test_events(events.clone(), Failure::BGPDropUpdate(loc_a_x)),
            vec![true, true, false, false],
            "Router specific locality (outgoing) filtering failed"
        );
        assert_eq!(
            test_events(events, Failure::BGPDropUpdate(loc_x_a)),
            vec![false, false, true, false],
            "Router specific locality (incoming) filtering failed"
        );
    }

    #[test]
    fn failure_application() {
        // Test all cases of failure application (they should always hit the filter, we only test for the )
        // --: Events
        // ----: Control Ospf Event (should not be touched by the failures)
        let ospf_event = Event::<SinglePrefix, ()>::ospf(
            (),
            0.into(),
            1.into(),
            OspfArea::BACKBONE,
            OspfEvent::LinkStateRequest { headers: vec![] },
        );
        // ----: Bgp Events
        let bgp_withdraw = BgpEvent::<SinglePrefix>::Withdraw(0.into());
        let bgp_update = BgpEvent::<SinglePrefix>::Update(get_default_route());

        // Helper function to test the failure application on multiple events
        let events = vec![
            ospf_event,
            get_event(0, 1, bgp_update),
            get_event(0, 1, bgp_withdraw),
        ];
        fn test_events(
            events: Vec<Event<SinglePrefix, ()>>,
            failure: Failure,
        ) -> Vec<Option<Event<SinglePrefix, ()>>>
        where
            SinglePrefix: Hash,
        {
            events.iter().map(|e| failure.apply(e.clone())).collect()
        }

        // --: Failures
        // ----: Destructive failures
        let drop_update = Failure::BGPDropUpdate((Some(0.into()), None));
        assert_eq!(
            test_events(events.clone(), drop_update),
            vec![Some(events[0].clone()), None, Some(events[2].clone())],
            "BGPDropUpdate failure application failed"
        );
        let drop_withdraw = Failure::BGPDropWithdraw((Some(0.into()), None));
        assert_eq!(
            test_events(events.clone(), drop_withdraw),
            vec![Some(events[0].clone()), Some(events[1].clone()), None],
            "BGPDropWithdraw failure application failed"
        );
        // ----: Transformative failures
        let change_local_pref = Failure::BGPChangeLocalPref((Some(0.into()), None), 200);
        assert_eq!(
            test_events(events.clone(), change_local_pref),
            vec![
                Some(events[0].clone()),
                Some(get_event(
                    0,
                    1,
                    BgpEvent::Update(BgpRoute {
                        local_pref: Some(200),
                        ..get_default_route()
                    })
                )),
                Some(events[2].clone())
            ],
            "BGPChangeLocalPref failure application failed"
        );
        let change_community_add = Failure::BGPChangeCommunity((Some(0.into()), None), 3);
        let change_community_remove = Failure::BGPChangeCommunity((Some(0.into()), None), -3);
        assert_eq!(
            test_events(events.clone(), change_community_add),
            vec![
                Some(events[0].clone()),
                Some(events[1].clone()),
                Some(events[2].clone())
            ],
            "BGPChangeCommunity failure application (add) failed"
        );
        assert_eq!(
            test_events(events.clone(), change_community_remove),
            vec![
                Some(events[0].clone()),
                Some(get_event(
                    0,
                    1,
                    BgpEvent::Update(BgpRoute {
                        community: vec![1, 2].into_iter().collect(),
                        ..get_default_route()
                    })
                )),
                Some(events[2].clone())
            ],
            "BGPChangeCommunity failure application (remove) failed"
        );
    }

    #[test]
    fn failure_formatting() {
        let (ll_net, (_, b, r)) =
            long_line_network::<SinglePrefix, GlobalOspf, BasicEventQueue<SinglePrefix>>(
                BasicEventQueue::default(),
            );
        assert_eq!(
            Failure::BGPDropUpdate((None, Some(b))).fmt(&ll_net),
            "BGPDropUpdate((*, Some(b)))"
        );
        assert_eq!(
            Failure::BGPDropWithdraw((Some(b), None)).fmt(&ll_net),
            "BGPDropWithdraw((Some(b), *))"
        );
        assert_eq!(
            Failure::BGPChangeLocalPref((Some(b), Some(r)), 100).fmt(&ll_net),
            "BGPChangeLocalPref((Some(b), Some(r)), 100)"
        );
        assert_eq!(
            Failure::BGPChangeCommunity((Some(r), Some(b)), 3).fmt(&ll_net),
            "BGPChangeCommunity((Some(r), Some(b)), +3)"
        );
        assert_eq!(
            Failure::BGPChangeCommunity((Some(r), None), -3).fmt(&ll_net),
            "BGPChangeCommunity((Some(r), *), -3)"
        );

        let failure_set: HashSet<Failure> =
            HashSet::from_iter(vec![Failure::BGPDropUpdate((None, Some(b)))]);
        assert_eq!(failure_set.fmt(&ll_net), "{BGPDropUpdate((*, Some(b)))}");
        //println!("{}", failure_set.fmt_multiline(&ll_net));
    }

    #[test]
    fn test_premature_intersperse() {
        let builder: FailureSetBuilder = FailureSetBuilder::new();
        assert_eq!(builder.get_all_relevant_local_preferences().len(), 0);
    }

    #[test]
    fn test_local_pref_intersperse() {
        let mut builder: FailureSetBuilder = FailureSetBuilder::new();
        builder.local_preferences.insert(100);
        // Test the case in which there is only one local preference
        assert_eq!(
            builder.get_all_relevant_local_preferences(),
            vec![50, 100, 200]
        );
        builder.local_preferences.insert(200);
        builder.local_preferences.insert(300);
        // Multiple local preferences
        assert_eq!(
            builder.get_all_relevant_local_preferences(),
            vec![50, 100, 150, 200, 250, 300, 600]
        );
    }

    #[test]
    fn check_locality_extraction() {
        let (ll_net, _) =
            long_line_network::<SinglePrefix, GlobalOspf, BasicEventQueue<SinglePrefix>>(
                BasicEventQueue::default(),
            );

        let mut builder: FailureSetBuilder = FailureSetBuilder::new();
        builder.ingest_network(&ll_net);
        let localities = builder.get_all_relevant_localities();
        // A long line network should have 2*2 sessions in total (minus one which has an external router as a source).
        // The two internal routers means we should have 2+2 more localities. The two from the external don't count
        assert_eq!(
            localities.len(),
            2 * 2 - 1 + (2 + 2),
            "The locality extraction did not work on the long line network"
        );
        // Check that none of the router-specific localities are centered on external indices
        assert!(localities.iter().all(|&(router1, router2)| {
            match (router1, router2) {
                (None, Some(r)) | (Some(r), None) => !ll_net.external_indices().contains(&r),
                _ => true, // If neither or both are `None`, the condition is satisfied
            }
        }));

        let (y_net, _) = y_network::<SinglePrefix, GlobalOspf, BasicEventQueue<SinglePrefix>>(
            BasicEventQueue::default(),
        );
        // Same routine
        let mut builder: FailureSetBuilder = FailureSetBuilder::new();
        builder.ingest_network(&y_net);
        let localities = builder.get_all_relevant_localities();
        // A y network should have 3*2 sessions in total (minus two which have an external router as a source).
        // The two internal routers means we should have 2+2 more localities. The two from the external don't count
        assert_eq!(
            localities.len(),
            (3 * 2) - 2 + (2 + 2),
            "The locality extraction did not work on the y network"
        );
        // Check that none of the router-specific localities are centered on external indices
        assert!(localities.iter().all(|&(router1, router2)| {
            match (router1, router2) {
                (None, Some(r)) | (Some(r), None) => !y_net.external_indices().contains(&r),
                _ => true, // If neither or both are `None`, the condition is satisfied
            }
        }));
    }
}
