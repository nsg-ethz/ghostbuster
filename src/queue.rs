use bgpsim::event::{BasicEventQueue, EventQueue, FmtPriority, GeoTimingModel};
use bgpsim::ospf::OspfProcess;
use bgpsim::types::{NetworkDevice, PhysicalNetwork, RouterId};
use bgpsim::{event::Event, types::Prefix};
use itertools::Itertools;
use rand::prelude::SliceRandom;
use rand::rngs::StdRng;
use serde::Serialize;
use std::collections::HashMap;
use std::collections::VecDeque;

use crate::failure::Failure;

/// Marker trait for a queue that does not apply any filtering. This means that all events that are
/// pushed are also popped without any modifications.
pub trait NotFilteringQueue {}
// External queues
impl<P: Prefix> NotFilteringQueue for BasicEventQueue<P> {}
impl<P: Prefix> NotFilteringQueue for GeoTimingModel<P> {}

// We can use the Priority type to store the ID of the event that triggered a particular event
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Default)]
pub struct TriggerId(pub Option<usize>);

impl FmtPriority for TriggerId {
    fn fmt(&self) -> String {
        match self.0 {
            Some(p) => p.to_string(),
            None => "None".to_string(),
        }
    }
}

/// Inner queue for events to mark the event that triggered them.
/// This queue uses the `Priority` field to store the ID of the event that triggered each event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TriggerQueue<P: Prefix> {
    pub(crate) events: VecDeque<Event<P, TriggerId>>,
    // The ID of the event that was last triggered.
    // *Note*: Has to be reset manually, otherwise this will keep incrementing
    last_event_id: TriggerId,
}

impl<P: Prefix> TriggerQueue<P> {
    pub fn new() -> Self {
        Self {
            events: VecDeque::new(),
            last_event_id: TriggerId(None),
        }
    }
    /// Reset the trigger id counter
    pub fn reset_trigger_id_counter(&mut self) {
        self.last_event_id = TriggerId(None);
    }
}

impl<P: Prefix> EventQueue<P> for TriggerQueue<P> {
    type Priority = TriggerId;

    fn push<Ospf: OspfProcess>(
        &mut self,
        mut event: Event<P, Self::Priority>,
        _: &HashMap<RouterId, NetworkDevice<P, Ospf>>,
        _: &PhysicalNetwork,
    ) {
        // We use the priority field to store the ID of the event that triggered the event
        // In this case, the ID of the last event that was triggered
        *event.priority_mut() = self.last_event_id;

        self.events.push_back(event)
    }

    fn pop(&mut self) -> Option<Event<P, Self::Priority>> {
        // When we pop an event it means this event will be processed. Processing will push more events to the queue
        // these events need to have the id of the event that triggered them (this one)
        match self.events.pop_front() {
            // If there is an event, update the counter
            Some(event) => {
                let event_id = match self.last_event_id.0 {
                    // If we had already been counting just increment
                    Some(id) => id + 1,
                    // If it's the first event, start at 0
                    None => 0,
                };
                self.last_event_id = TriggerId(Some(event_id));
                // Yield the event
                Some(event)
            }
            // If there is no event there is no last event ID to set
            None => None,
        }
    }

    fn peek(&self) -> Option<&Event<P, Self::Priority>> {
        self.events.front()
    }

    fn len(&self) -> usize {
        self.events.len()
    }

    fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    fn clear(&mut self) {
        self.events.clear()
    }

    fn get_time(&self) -> Option<f64> {
        None
    }

    fn update_params<Ospf: OspfProcess>(
        &mut self,
        _: &HashMap<RouterId, NetworkDevice<P, Ospf>>,
        _: &PhysicalNetwork,
    ) {
    }

    unsafe fn clone_events(&self, _: Self) -> Self {
        self.clone()
    }
}

impl<P: Prefix> NotFilteringQueue for TriggerQueue<P> {}

/// Slightly more complex implementation of a BasicEventQueue that enables
/// either fully deterministic or random ordering of events.
#[derive(Serialize, Clone, PartialEq, Eq, Debug)]
pub struct OrderedEventQueue<Q> {
    pub inner_queue: Q,
    // If shuffle is not `None`, the events will be shuffled before being enqueued
    #[serde(skip_serializing)]
    shuffle: Option<StdRng>,
}

impl<Q> OrderedEventQueue<Q> {
    // Init a new failure queue with a failure
    pub fn new(rng: Option<StdRng>, queue: Q) -> Self {
        Self {
            inner_queue: queue,
            shuffle: rng,
        }
    }
}

impl<P: Prefix, Q: EventQueue<P>> EventQueue<P> for OrderedEventQueue<Q> {
    // Inherit the same priority as the inner queue
    type Priority = Q::Priority;

    fn push<Ospf: OspfProcess>(
        &mut self,
        event: Event<P, Self::Priority>,
        routers: &HashMap<RouterId, NetworkDevice<P, Ospf>>,
        net: &PhysicalNetwork,
    ) {
        // We just push the event in the inner queue
        self.inner_queue.push(event, routers, net);
    }

    fn push_many<Ospf: OspfProcess>(
        &mut self,
        events: Vec<Event<P, Self::Priority>>,
        routers: &HashMap<RouterId, NetworkDevice<P, Ospf>>,
        net: &PhysicalNetwork,
    ) {
        // Assertion because the case in which more than one event bound for the same router gets generated in the
        // same run may be an unhandled case
        debug_assert_eq!(
            events
                .iter()
                .unique_by(|e| (e.router(), e.prefix()))
                .count(),
            events.len()
        );
        // We always sort the events first, no matter what
        let mut sorted_events: Vec<_> = events
            .into_iter()
            .sorted_by_key(|e| (e.router(), e.prefix()))
            .collect();

        // If shuffle is set, shuffle the events
        if let Some(rng) = &mut self.shuffle {
            sorted_events.shuffle(rng);
        }

        for event in sorted_events {
            self.push(event, routers, net);
        }
    }

    fn pop(&mut self) -> Option<Event<P, Self::Priority>> {
        // Simply pop an event from the inner queue
        self.inner_queue.pop()
    }

    fn peek(&self) -> Option<&Event<P, Self::Priority>> {
        self.inner_queue.peek()
    }

    fn len(&self) -> usize {
        self.inner_queue.len()
    }

    fn is_empty(&self) -> bool {
        self.inner_queue.is_empty()
    }

    fn clear(&mut self) {
        self.inner_queue.clear()
    }

    fn get_time(&self) -> Option<f64> {
        self.inner_queue.get_time()
    }

    fn update_params<Ospf: OspfProcess>(
        &mut self,
        routers: &HashMap<RouterId, NetworkDevice<P, Ospf>>,
        net: &PhysicalNetwork,
    ) {
        self.inner_queue.update_params(routers, net);
    }

    unsafe fn clone_events(&self, _: Self) -> Self {
        todo!("Not implemented yet")
    }
}

impl<P: Prefix> NotFilteringQueue for OrderedEventQueue<P> {}

/// Queue for applying failures to the network
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct FailureQueue<Q> {
    pub(crate) inner_queue: Q,
    // Failure that is applied to this queue (Every queue only ever has one failure)
    failure: Failure,
}

impl<Q> FailureQueue<Q> {
    // Init a new failure queue with a failure
    pub fn new(failure: Failure, queue: Q) -> Self {
        // Here I had an argument for the shuffle (, rng: Option<StdRng>)
        Self {
            inner_queue: queue,
            failure: failure,
        }
    }
}

impl<P: Prefix, Q: EventQueue<P>> EventQueue<P> for FailureQueue<Q> {
    // Inherit the same priority as the inner queue
    type Priority = Q::Priority;

    fn push<Ospf: OspfProcess>(
        &mut self,
        event: Event<P, Self::Priority>,
        routers: &HashMap<RouterId, NetworkDevice<P, Ospf>>,
        net: &PhysicalNetwork,
    ) {
        // We just push the event in the inner queue
        self.inner_queue.push(event, routers, net);
    }

    /// We need to define this for the failure queue, otherwise the default implementation will be used.
    /// This will enqueue the events in the inner queue individually and will break any underlying ordering
    /// that the queue may have.
    fn push_many<Ospf: OspfProcess>(
        &mut self,
        events: Vec<Event<P, Self::Priority>>,
        routers: &HashMap<RouterId, NetworkDevice<P, Ospf>>,
        net: &PhysicalNetwork,
    ) {
        self.inner_queue.push_many(events, routers, net);
    }

    /// We apply the failures on pop. This is due to the fact that a failure can be added on
    /// top of an existing `EventQueue` which may still have events in it.
    /// We want all events to be affected by the failure as soon as we wrap the queue with the failure,
    /// therefore the failure is only applied when events are popped.
    fn pop(&mut self) -> Option<Event<P, Self::Priority>> {
        // Pop an event from the inner queue. This event will be unaffected by the failure and will be
        // `None` iff there are no more events in the queue
        while let Some(inner_event) = self.inner_queue.pop() {
            // Apply the failure to the event that is being popped
            match self.failure.apply(inner_event) {
                // If applying the failure either leaves the event unchanged or modifies it, we return the event
                Some(event) => return Some(event),
                // If applying the failure causes the event to be dropped we pop the next one
                None => (),
            }
        }
        // If we get here it means there are no more events in the queue
        None
    }

    fn peek(&self) -> Option<&Event<P, Self::Priority>> {
        self.inner_queue.peek()
    }

    fn len(&self) -> usize {
        self.inner_queue.len()
    }

    fn is_empty(&self) -> bool {
        self.inner_queue.is_empty()
    }

    fn clear(&mut self) {
        self.inner_queue.clear();
    }

    fn get_time(&self) -> Option<f64> {
        self.inner_queue.get_time()
    }

    fn update_params<Ospf: OspfProcess>(
        &mut self,
        routers: &HashMap<RouterId, NetworkDevice<P, Ospf>>,
        net: &PhysicalNetwork,
    ) {
        self.inner_queue.update_params(routers, net);
    }

    unsafe fn clone_events(&self, _: Self) -> Self {
        todo!("Not implemented yet")
    }
}

#[cfg(test)]
pub(crate) mod test_failure_queue {
    use crate::assert_forwarding;
    use crate::failure::*;
    use crate::queue::OrderedEventQueue;
    use crate::queue::TriggerId;
    use crate::queue::TriggerQueue;
    use crate::tests::*;
    use bgpsim::bgp::BgpEvent;
    use bgpsim::builder::*;
    use bgpsim::event::*;
    use bgpsim::prelude::*;
    use bgpsim::topology_zoo::TopologyZoo;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    use super::FailureQueue;

    #[test]
    fn failure_queue_drop_updates() {
        let (mut net, (e, b, r)) = long_line_network::<
            SinglePrefix,
            GlobalOspf,
            OrderedEventQueue<BasicEventQueue<SinglePrefix>>,
        >(OrderedEventQueue::new(None, BasicEventQueue::new()));
        // Put the network in manual mode
        net.manual_simulation();

        // Propagate a single advertisment from the external router to the border one
        net.advertise_external_route(e.into(), SinglePrefix::from(0), [1, 2, 3], None, None)
            .unwrap();

        net.simulate_hooked(|net, event, result| {
            // We only consider hooks that get called before the event is processed
            if result.is_none() {
                println!("{}", event.fmt(&net));
            }
        })
        .unwrap();
        assert_forwarding!(net, r, Some(b));

        // Add a failure to the queue, drop all withdrawals from b
        let inner_queue = net.queue().clone();
        let mut net = net.swap_queue(FailureQueue::new(
            Failure::BGPDropWithdraw((Some(b), None)),
            inner_queue,
        ));
        // Propagate a withdrawal from the border router to the internal one
        net.withdraw_external_route(e.into(), SinglePrefix::from(0))
            .unwrap();

        net.simulate_hooked(|net, event, result| {
            // There should be no withdraw events being exchanged between the border and internal routers
            if result.is_some() {
                println!("{}", event.fmt(&net));
                match event {
                    Event::Bgp {
                        src,
                        e: BgpEvent::Withdraw(..),
                        ..
                    } => {
                        assert_ne!(*src, b);
                    }
                    _ => {}
                }
            }
        })
        .unwrap();
        // Since r has not received the withdraw it should still be forwarding to b
        assert_forwarding!(net, r, Some(b));
    }

    #[test]
    fn failure_queue_trigger_id_single() {
        // Test if the trigger id of the events is correctly set
        let (mut net, (e, b, r)) = long_line_network::<
            SinglePrefix,
            GlobalOspf,
            OrderedEventQueue<TriggerQueue<SinglePrefix>>,
        >(OrderedEventQueue::new(None, TriggerQueue::new()));
        // Put the network in manual mode
        net.manual_simulation();

        // Propagate a single advertisment from the external router to the border one
        net.advertise_external_route(e.into(), SinglePrefix::from(0), [1, 2, 3], None, None)
            .unwrap();

        // Trigger id
        let mut previously_triggered: Vec<Event<SinglePrefix, TriggerId>> =
            net.queue().inner_queue.events.clone().into();
        net.simulate_hooked(|net, event, result| {
            match result {
                None => {
                    // These are the callback that happens before the event is processed
                    println!("{}", event.fmt(&net));
                    // Check that the events triggered in the previous event processing have been enqueued
                    // and have the correct trigger id
                    println!("               Queue: {:?}", net.queue().inner_queue.events);
                    println!("Previously triggered: {:?}", previously_triggered);
                    for e in &previously_triggered {
                        // Find the event in the queue, it should be there
                        // This equality simultaneously checks that the trigger id is set correctly
                        assert!(net.queue().inner_queue.events.contains(e));
                    }
                }
                Some((_, triggered_events)) => {
                    previously_triggered = triggered_events
                        .iter()
                        .map(|e| {
                            let mut event = e.clone();
                            // Artificially set the trigger id, as these events have not been pushed to the queue yet
                            *event.priority_mut() = net.queue().inner_queue.last_event_id;
                            event
                        })
                        .collect();
                }
            }
        })
        .unwrap();
        assert_forwarding!(net, r, Some(b));

        net.queue_mut().inner_queue.reset_trigger_id_counter();
        // Check that the trigger id is reset after the queue is empty
        assert_eq!(net.queue().inner_queue.last_event_id, TriggerId(None));

        // Add a failure to the queue, drop all withdrawals from b
        let inner_queue = net.queue().clone();
        let mut net = net.swap_queue(FailureQueue::new(
            Failure::BGPDropWithdraw((Some(b), None)),
            inner_queue,
        ));
        // Propagate a withdrawal from the border router to the internal one
        net.withdraw_external_route(e.into(), SinglePrefix::from(0))
            .unwrap();

        // Trigger id
        let mut previously_triggered: Vec<Event<SinglePrefix, TriggerId>> =
            net.queue().inner_queue.inner_queue.events.clone().into();
        net.simulate_hooked(|net, event, result| {
            match result {
                None => {
                    // These are the callback that happens before the event is processed
                    println!("{}", event.fmt(&net));
                    // Check that the events triggered in the previous event processing have been enqueued
                    // and have the correct trigger id
                    println!(
                        "               Queue: {:?}",
                        net.queue().inner_queue.inner_queue.events
                    );
                    println!("Previously triggered: {:?}", previously_triggered);
                    for e in &previously_triggered {
                        // Find the event in the queue, it should be there
                        // This equality simultaneously checks that the trigger id is set correctly
                        assert!(net.queue().inner_queue.inner_queue.events.contains(e));
                    }
                }
                Some((_, triggered_events)) => {
                    previously_triggered = triggered_events
                        .iter()
                        .map(|e| {
                            let mut event = e.clone();
                            // Artificially set the trigger id, as these events have not been pushed to the queue yet
                            *event.priority_mut() =
                                net.queue().inner_queue.inner_queue.last_event_id;
                            event
                        })
                        .collect();
                }
            }
        })
        .unwrap();
        // Since r has not received the withdraw it should still be forwarding to b
        assert_forwarding!(net, r, Some(b));
    }

    #[test]
    fn failure_queue_around_timing() {
        use bgpsim::types::SinglePrefix as P;

        // Define the network type
        type Net = Network<P, BasicEventQueue<P>, GlobalOspf>;

        // create the network with the basic event queue
        let mut net: Net = TopologyZoo::Abilene.build(BasicEventQueue::<P>::new());
        let mut rng = StdRng::seed_from_u64(42);
        let prefix = P::from(0);

        // Build the configuration for the network
        let _external_routers = net
            .build_external_routers(extend_to_k_external_routers_seeded, (&mut rng, 3))
            .unwrap();
        let route_reflectors = net
            .build_ibgp_route_reflection(k_highest_degree_nodes, 2)
            .unwrap();
        println!("Route reflectors: {}", route_reflectors.fmt(&net));
        net.build_ebgp_sessions().unwrap();
        net.build_link_weights(constant_link_weight, 20.0).unwrap();

        let seattle = net.get_router_id("Seattle").unwrap();
        let indianapolis = net.get_router_id("Indianapolis").unwrap();
        let sunnyvale = net.get_router_id("Sunnyvale").unwrap();
        let seattle_e = net.get_router_id("Seattle_ext_12").unwrap();
        let indianapolis_e = net.get_router_id("Indianapolis_ext_13").unwrap();
        let kansas_city_e = net.get_router_id("KansasCity_ext_11").unwrap();

        net.build_advertisements(
            prefix,
            |_, _| vec![vec![seattle_e], vec![indianapolis_e], vec![kansas_city_e]],
            (),
        )
        .unwrap();

        // With a seed of 42 the external routers are   [KansasCity_ext_11, Seattle_ext_12, Indianapolis_ext_13]
        // and the route reflectors are                 {KansasCity, Indianapolis}

        for router in net.internal_routers() {
            println!("{:?}: {}", router.router_id(), router.name());
        }

        println!(
            "Sunnyvale:\n{}",
            net.get_internal_router(sunnyvale)
                .unwrap()
                .bgp
                .fmt_prefix_table(&net, prefix)
        );

        // Make sure that Seattle is forwarding its traffic to the external router
        assert_forwarding!(net, seattle, Some(seattle_e));
        assert_forwarding!(net, indianapolis, Some(seattle));
        assert_forwarding!(net, sunnyvale, Some(seattle));

        // Swap out the queue for a `GeoTimingModel` one wrapped in failure. There are no events in the queue yet
        // so we can just swap the queue without worrying about the events
        let mut net = net.swap_queue(FailureQueue::new(
            // Isolate the Sunnyvale router
            Failure::BGPDropUpdate((None, Some(sunnyvale))),
            GeoTimingModel::new(
                ModelParams::new(0.1, 0.1, 2.0, 5.0, 0.01),
                ModelParams::new(0.000_1, 0.000_1, 2.0, 5.0, 0.0),
                &TopologyZoo::Abilene.geo_location(),
            ),
        ));

        // execute the event and measure the time
        net.withdraw_external_route(seattle_e, prefix).unwrap();
        // Seattle now has to go through indianapolis
        assert_forwarding!(net, seattle, Some(indianapolis));
        assert_forwarding!(net, indianapolis, Some(indianapolis_e));
        // But sunnyvale should still be forwarding to seattle, it did not receive any of the update messages
        assert_forwarding!(net, sunnyvale, Some(seattle));
    }
}
