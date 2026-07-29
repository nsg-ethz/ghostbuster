use bgpsim::{bgp::BgpEvent, prelude::*, router::Router};
use itertools::Itertools;
use ordered_float::NotNan;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use strum_macros::EnumDiscriminants;
use thiserror::Error;

use crate::{
    recording::{Recording, RouterSequences},
    reordering_monitoring::{Arena, ReorderingMonitor},
};

pub type MonitoringResult<P> = Result<(), MonitoringError<P>>;
// The monitoring result contains detailed information about the result of the monitoring
#[derive(Clone, Debug, EnumDiscriminants, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(deserialize = "P: for<'a> serde::Deserialize<'a>"))]
pub enum MonitoringError<P: Prefix> {
    // There was an event output by a router that we cannot reconcile with any of the possible router states
    UnexpectedEvent {
        router: RouterId,
        session: RouterId,
        event: BgpEvent<P>,
    },
    // There are no events observed in the network, but we should have seen at least one.
    // WARN: Disabled for now, but we might reintroduce later
    // NoEvents,
    // When checking the final state of the network, we found that there should have been events that have been sent out
    UnsentEvents {
        router: RouterId,
        session: RouterId,
        events: Vec<BgpEvent<P>>,
    },
    MissingOutgoingEvent {
        router: RouterId,
        neighbor: RouterId,
        current_event: BgpEvent<P>,
        expected_event: BgpEvent<P>,
    },
    Unserializable(RouterId),
    Killed {
        router: RouterId,
        neighbor: RouterId,
        prefix: P,
        num_forks: usize,
    },
}

impl<P: Prefix> MonitoringError<P> {
    /// Return the router id of the router that triggered this error
    pub fn router(&self) -> RouterId {
        match self {
            MonitoringError::UnexpectedEvent { router, .. } => *router,
            MonitoringError::UnsentEvents { router, .. } => *router,
            MonitoringError::MissingOutgoingEvent { router, .. } => *router,
            MonitoringError::Unserializable(node_index) => *node_index,
            MonitoringError::Killed { router, .. } => *router,
        }
    }
    /// Return a set of prefixes that this error is related to
    pub fn prefixes(&self) -> HashSet<P> {
        match self {
            MonitoringError::UnexpectedEvent { event, .. }
            | MonitoringError::MissingOutgoingEvent {
                current_event: event,
                ..
            } => HashSet::from([event.prefix()]),
            MonitoringError::UnsentEvents { events, .. } => {
                events.iter().map(|e| e.prefix()).collect()
            }
            MonitoringError::Unserializable(_) => HashSet::new(),
            MonitoringError::Killed { prefix, .. } => HashSet::from([*prefix]),
        }
    }
}

// TODO: Not super clean, needed to convert from a singleprefix back into whatever we had.
//       Maybe split this error up further, but am I missing something?
impl<P1: Prefix, P2: Prefix> From<(MonitoringError<P1>, P2)> for MonitoringError<P2> {
    fn from((error, prefix): (MonitoringError<P1>, P2)) -> Self {
        match error {
            MonitoringError::UnexpectedEvent {
                router,
                session,
                event,
            } => Self::UnexpectedEvent {
                router,
                session,
                event: event.with_prefix(prefix),
            },

            MonitoringError::UnsentEvents {
                router,
                session,
                events,
            } => Self::UnsentEvents {
                router,
                session,
                events: events.into_iter().map(|e| e.with_prefix(prefix)).collect(),
            },

            MonitoringError::MissingOutgoingEvent {
                router,
                neighbor,
                current_event,
                expected_event,
            } => Self::MissingOutgoingEvent {
                router,
                neighbor,
                current_event: current_event.with_prefix(prefix),
                expected_event: expected_event.with_prefix(prefix),
            },
            MonitoringError::Unserializable(r) => Self::Unserializable(r),
            MonitoringError::Killed {
                router,
                neighbor,
                num_forks,
                ..
            } => Self::Killed {
                router,
                neighbor,
                prefix,
                num_forks,
            },
        }
    }
}

impl<'a, P, Q, Ospf> NetworkFormatter<'a, P, Q, Ospf> for MonitoringError<P>
where
    P: Prefix,
    Q: EventQueue<P>,
    Ospf: bgpsim::ospf::OspfImpl,
{
    fn fmt(&self, net: &Network<P, Q, Ospf>) -> String {
        match self {
            MonitoringError::UnexpectedEvent { router, session, event } => format!(
                "Router {} has sent out an event {} on its session to {} that we cannot reconcile with any of the possible router states",
                router.fmt(net),
                event.fmt(net),
                session.fmt(net)
            ),
            MonitoringError::UnsentEvents { router, session, events } => format!(
                "Router {} has not sent out all the messages it was supposed to on session {}: {}",
                router.fmt(net),
                session.fmt(net),
                events.fmt_list(net)
            ),
            MonitoringError::MissingOutgoingEvent { router, neighbor, current_event, expected_event , ..} => format!{
                "Router {} has not sent a message to {}:\n      last message: {}\n  expected message: {}",
                router.fmt(net),
                neighbor.fmt(net),
                current_event.fmt(net),
                expected_event.fmt(net),
            },
            MonitoringError::Unserializable(r) => format!("Per-session monitors for router {} rely on an order of incoming messages that contradict eachother.", r.fmt(net)),
            MonitoringError::Killed {
                router,
                neighbor,
                prefix,
                num_forks,
            } => {
                format!("Per-session monitor from {} to {} for {prefix} was killed, as it maintained {num_forks} forks.", router.fmt(net), neighbor.fmt(net))}
        }
    }
}

#[derive(Error, Debug)]
pub enum ControllerError<P: Prefix> {
    #[error("Controller is not built to ingest messages with prefix: {0}")]
    NoPrefix(P),
    #[error("Could not find a router with id {0:?} in one of the planes")]
    NoRouter(RouterId),
}

/// A PrefixPlane is a component of the controller responsible for monitoring the correctness of the processing of a single prefix
type PrefixPlane<'a> = HashMap<RouterId, ReorderingMonitor<'a>>;
pub struct Controller<'a, P: Prefix> {
    // The controller keeps a copy of the network plane that controls a certain prefix for each
    // prefix
    net_planes: HashMap<P, PrefixPlane<'a>>,
    pub mrai: Option<NotNan<f64>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(bound(deserialize = "P: for<'a> serde::Deserialize<'a>"))]
/// We store the sequence of monitoring outcomes for every prefix and router
pub struct MonitoringResults<P: Prefix>(
    pub HashMap<P, HashMap<RouterId, Vec<(MonitoringResult<P>, f64)>>>,
);

impl<P: Prefix> MonitoringResults<P> {
    fn new() -> Self {
        MonitoringResults(HashMap::new())
    }

    /// Get the number of errors per prefix
    pub fn get_errors_per_prefix(&self, prefix: &P) -> usize {
        self.0
            .get(prefix)
            .map(|plane| {
                plane
                    .values()
                    .flat_map(|results| results.iter())
                    .filter(|(result, _)| result.is_err())
                    .count()
            })
            .unwrap_or(0)
    }

    /// Only get a list of errors from these results
    pub fn get_errors(&self) -> Vec<MonitoringError<P>> {
        self.0
            .iter()
            .flat_map(|(_, plane)| {
                plane.iter().flat_map(|(_, results)| {
                    results.iter().filter_map(|result| result.0.clone().err())
                })
            })
            .collect_vec()
    }

    /// TODO: this is just glue code and the name of this function should be changed in the future
    pub fn is_empty(&self) -> bool {
        self.get_errors().is_empty()
    }
}

impl<'a, P, Q, Ospf> NetworkFormatter<'a, P, Q, Ospf> for MonitoringResults<P>
where
    P: Prefix,
    Q: EventQueue<P>,
    Ospf: bgpsim::ospf::OspfImpl,
{
    /// TODO: this is just glue code and the name of this function should be changed in the future
    fn fmt(&self, net: &'a Network<P, Q, Ospf>) -> String {
        // We only format the errors
        self.get_errors().fmt_multiline(&net)
    }
}

impl<'a, P: Prefix> Controller<'a, P> {
    // Monitor a recorded trace for the controller
    pub fn monitor_recording(
        &mut self,
        recording: Recording<P>,
    ) -> Result<MonitoringResults<P>, ControllerError<P>> {
        let mut results = MonitoringResults::new();
        // Store the time at which we are supposed to perform the final check on
        let final_check_times: HashMap<(P, RouterId), f64> =
            recording
                .0
                .iter()
                .fold(HashMap::new(), |mut acc, (prefix, routers)| {
                    for (router, messages) in routers {
                        if let Some(last_message) = messages.last() {
                            // Get the last message for each prefix/router monitoring unit
                            let final_message_time = **last_message.0.priority();
                            // Add the eventual delay in case MRAI values are set
                            let final_check_time =
                                final_message_time + *self.mrai.unwrap_or_default();
                            acc.insert((*prefix, *router), final_check_time);
                        }
                    }
                    acc
                });

        // Check the messages "at runtime"
        for (prefix, routers) in recording {
            let prefix_errors = self.monitor_prefix(prefix, routers)?;
            results.0.insert(prefix, prefix_errors);
        }

        // Early return if there are runtime errors
        if !results.is_empty() {
            return Ok(results);
        }

        // Check the final state
        log::warn!("Checking final state");
        self.check_final_state(&mut results, final_check_times);

        Ok(results)
    }

    // Monitor the events exchanged within the network for a specific prefix
    fn monitor_prefix(
        &mut self,
        prefix: P,
        routers: RouterSequences,
    ) -> Result<HashMap<RouterId, Vec<(MonitoringResult<P>, f64)>>, ControllerError<P>> {
        let mut results: HashMap<RouterId, Vec<(MonitoringResult<P>, f64)>> = HashMap::new();
        log::info!("Monitoring prefix: {}", prefix);
        let plane = self
            .net_planes
            .get_mut(&prefix)
            .ok_or(ControllerError::NoPrefix(prefix))?;

        for (router, message_sequence) in routers.into_iter() {
            // Get the monitor for this router
            let router_monitor = plane
                .get_mut(&router)
                .ok_or(ControllerError::NoRouter(router))?;

            // Run every message through the monitor
            log::info!("Checking trace on {}", router.index());
            for msg in message_sequence.into_iter() {
                let mut monitor_result = Ok(());
                let mut msg_timestamp = msg.0.priority().clone();

                // If there is an mrai configured we can check the validity of all applied events before
                if let Some(mrai) = self.mrai {
                    let apply_all_before = msg_timestamp - mrai;
                    log::debug!("Processing all messages received before {apply_all_before:?}");
                    monitor_result =
                        router_monitor.assert_messages_processed_before(apply_all_before);
                    // If the error is due to messages expiring we only catch it now
                    if monitor_result.is_err() {
                        msg_timestamp += mrai
                    }
                }
                monitor_result = monitor_result.and_then(|_| router_monitor.process_message(msg.0));

                if monitor_result.is_err() {
                    log::debug!(
                        "Found error {:?} at timestamp {}",
                        monitor_result,
                        msg_timestamp.into_inner()
                    )
                }

                // Record the result
                results.entry(router).or_default().push((
                    monitor_result.map_err(|e| (e, prefix).into()),
                    msg_timestamp.into_inner(),
                ));
            }
        }
        Ok(results)
    }

    /// Check the final state of the network for consistency, updating the results for each router
    /// Also provide a timestamp for when each prefix/router monitoring unit is "technically" supposed to perform this final check
    pub fn check_final_state(
        &mut self,
        results: &mut MonitoringResults<P>,
        final_check_times: HashMap<(P, RouterId), f64>,
    ) {
        // No more events are coming. Are we in a consistent state?
        // Check if all the messages we sent out have actually been sent out
        self.net_planes.iter_mut().for_each(|(&prefix, plane)| {
            for (&router, state) in plane.iter_mut() {
                if let Some(final_check_time) = final_check_times.get(&(prefix, router)) {
                    // Perform a final check on this router only if there were messages for it in the recording
                    // At this point in time, everything is fine according to the monitor, add a timestamp to log this
                    let result = state.final_check().map_err(|e| (e, prefix).into());
                    results.0.entry(prefix).or_default().entry(router).or_default().push((
                    result,
                    *final_check_time,
                ));
                } else {
                    log::warn!(
                        "Skipping final state check for router {:?} and prefix {}, because we didn't have any events for it in the recording!\n\
                        This might be due to the fact that we are not monitoring for this prefix anymore", router, prefix
                    );
                }
            }
        });
    }
}

impl<'a> Controller<'a, SinglePrefix> {
    pub fn new<Q: EventQueue<SinglePrefix>>(
        net: &Network<SinglePrefix, Q>,
        arena: &'a Arena,
    ) -> Self {
        Self {
            net_planes: HashMap::from_iter([(
                SinglePrefix,
                net.internal_routers()
                    .map(|r: &Router<SinglePrefix>| {
                        (r.router_id(), ReorderingMonitor::new(r.clone(), arena))
                    })
                    .collect(),
            )]),
            mrai: None,
        }
    }
}

impl<'a> Controller<'a, SimplePrefix> {
    pub fn new_for_prefixes<Q: EventQueue<SimplePrefix>>(
        net: &Network<SimplePrefix, Q>,
        prefix_set: &HashSet<SimplePrefix>,
        arena: &'a Arena,
    ) -> Self {
        // Extract copies of the routers for every prefix
        fn states_for_prefix<'a, Q: EventQueue<SimplePrefix>>(
            net: &Network<SimplePrefix, Q>,
            p: &SimplePrefix,
            arena: &'a Arena,
        ) -> HashMap<RouterId, ReorderingMonitor<'a>> {
            net.internal_routers()
                .map(|r| {
                    (
                        r.router_id(),
                        ReorderingMonitor::new_for_prefix(r.clone(), p, arena),
                    )
                })
                .collect()
        }

        Self {
            net_planes: prefix_set
                .into_iter()
                .map(|p| (*p, states_for_prefix(net, p, arena)))
                .collect(),
            mrai: None,
        }
    }
}

#[cfg(test)]
pub(crate) mod test_monitoring {
    use bgpsim::{
        event::EventQueue,
        prelude::{InteractiveNetwork, NetworkFormatter},
        types::SinglePrefix,
    };
    use log::{debug, info};
    use test_log::test;

    use crate::{
        assert_forwarding,
        failure::Failure,
        monitoring::{Controller, MonitoringErrorDiscriminants},
        queue::FailureQueue,
        recording::Recording,
        tests::e_network_route_map_scenario,
    };

    #[test]
    fn test_monitoring_basic() {
        let (net_base, ([e1, _, _], [b1, b2, b3], r)) = e_network_route_map_scenario(None);
        // Set up some failure scenarios
        let failures = vec![
            (
                Failure::BGPDropWithdraw((Some(b1), Some(r))),
                Some(MonitoringErrorDiscriminants::MissingOutgoingEvent),
            ),
            // The following failure should be ignored due to the possibility of debouncing
            (Failure::BGPDropWithdraw((Some(r), Some(b2))), None),
            (
                Failure::BGPDropWithdraw((Some(r), None)),
                Some(MonitoringErrorDiscriminants::MissingOutgoingEvent),
            ),
            (
                Failure::BGPChangeLocalPref((Some(b3), Some(r)), 175),
                Some(MonitoringErrorDiscriminants::UnexpectedEvent),
            ),
            // Skipping due to the fact that we can and should catch this elsewhere
            // (
            //     Failure::BGPDropWithdraw((None, Some(b1))),
            //     MonitoringErrorDiscriminants::NoEvents,
            // ),
        ];

        // Check the healthy network
        let mut net = net_base.clone();
        // Make sure the forwarding state is what we expect
        assert_forwarding!(net, b1, Some(e1));
        assert_forwarding!(net, b2, Some(b1));
        assert_forwarding!(net, b3, Some(b1));
        // Trigger a withdrawal event
        net.withdraw_external_route(e1, SinglePrefix::default())
            .unwrap();
        // Make sure the forwarding state is what we expect
        assert_forwarding!(net, b1, Some(b2));

        // Check the failures
        for (failure, should_error) in failures {
            info!("Testing failure scenario: {}", failure.fmt(&net_base));
            assert!(net_base.queue().is_empty());
            // Clone the network for each failure scenario, get a SimpleTiming model
            let mut net = net_base
                .clone()
                .swap_queue(FailureQueue::new(failure.clone(), net_base.queue().clone()));

            // Make sure the forwarding state is what we expect
            assert_forwarding!(net, b1, Some(e1));
            assert_forwarding!(net, b2, Some(b1));
            assert_forwarding!(net, b3, Some(b1));

            // Create a controller attached to this network
            let arena = Default::default();
            let mut controller = Controller::new(&net, &arena);
            // Make a recording of the withdrawal
            let mut events = Vec::new();
            // Put the network in manual mode
            net.manual_simulation();
            // Run the closure on the network
            net.withdraw_external_route(e1, SinglePrefix::default())
                .unwrap();
            let _ = net.simulate_hooked(|_, event, result| {
                // We only consider hooks that get called after the event has been popped from the queue
                if result.is_some() {
                    events.push(event.clone());
                }
            });

            let recording =
                Recording::from_vec(events).filter_routers(&net.internal_indices().collect());
            debug!("{}", recording.fmt_multiline(&net));
            // There should not be any controller errors
            let monitoring_errors = controller.monitor_recording(recording).unwrap();

            if let Some(expected_error) = should_error {
                assert!(
                    monitoring_errors
                        .get_errors()
                        .iter()
                        .any(|error| MonitoringErrorDiscriminants::from(error.clone())
                            == expected_error),
                    "No error was found, but we expected one of the type {:?}",
                    expected_error
                );
                info!("Found errors: {}", monitoring_errors.fmt_multiline(&net));
            } else {
                assert!(monitoring_errors.is_empty());
                info!("Found no errors");
            }
        }
    }

    /// Removing a link tears the BGP session down with it, so a later withdrawal cannot produce a
    /// recording that mentions a session the network no longer has.
    ///
    /// This used to be asserted the other way round: bgpsim left the session in place and refused
    /// the withdrawal with `InconsistentBgpSession`. It now cleans the session up instead, which is
    /// the better behaviour, and the monitor depends on it — so it is worth pinning down.
    #[test]
    fn removing_a_link_also_removes_the_bgp_session() {
        let (mut net, ([e1, _, _], [b1, _, _], _)) = e_network_route_map_scenario(None);
        assert!(net.get_device(b1).unwrap().bgp_neighbors().contains(&e1));

        net.remove_link(b1, e1).unwrap();

        assert!(
            !net.get_device(b1).unwrap().bgp_neighbors().contains(&e1),
            "removing the link must tear down the eBGP session with it"
        );
        assert_eq!(net.withdraw_external_route(e1, SinglePrefix::default()), Ok(()));
    }
}
