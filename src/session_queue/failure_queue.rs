//! BGP Queue implementation that does propagation delays, debouncing, failure simulation, and
//! capturing.

use std::collections::HashMap;

use bgpsim::{
    bgp::BgpEvent,
    types::{Prefix, RouterId},
};
use geoutils::Location;
use ordered_float::NotNan;

use crate::{
    failure::Failure,
    session_queue::{
        DispatchQueue, FilteredSessionQueue, Message, Poll, QueueInit, RandomizedFifoSessionQueue,
        SessionFilter, SessionQueue, SessionQueueSequence,
    },
};

use super::geo_distances::GeoDistance;

pub type BgpSessionQueue<P> = SessionQueueSequence<
    DebounceQueue<P>,
    FilteredSessionQueue<(Vec<Failure>,), RandomizedFifoSessionQueue<P>, ()>,
>;
pub type BgpQueue<P> = DispatchQueue<P, BgpSessionQueue<P>, BgpSessionQueueInit>;

pub struct BgpSessionQueueInit {
    geo_distance: GeoDistance,
    variation: NotNan<f64>,
    // default_delay: NotNan<f64>,
    debounce_time: NotNan<f64>,
    failures: Vec<Failure>,
}

impl BgpSessionQueueInit {
    pub fn new(
        default_delay: NotNan<f64>,
        variation: NotNan<f64>,
        debounce_time: NotNan<f64>,
        geo_location: &HashMap<RouterId, Location>,
        failures: Vec<Failure>,
    ) -> Self {
        Self {
            geo_distance: GeoDistance::new(geo_location, default_delay),
            variation,
            // default_delay,
            debounce_time,
            failures,
        }
    }
}

impl<P: Prefix> QueueInit<BgpSessionQueue<P>> for BgpSessionQueueInit {
    fn init(&mut self, src: RouterId, dst: RouterId) -> BgpSessionQueue<P> {
        let delay = self
            .geo_distance
            .get_path_distance(src, dst)
            .map(|(x, _)| x)
            .unwrap_or(NotNan::new(1.0).unwrap());

        let failures = self
            .failures
            .iter()
            .filter(|f| f.matches_locality(src, dst))
            .copied()
            .collect();

        SessionQueueSequence {
            a: DebounceQueue::new(self.debounce_time),
            b: FilteredSessionQueue {
                pre: (failures,),
                queue: RandomizedFifoSessionQueue::new(delay, delay + self.variation),
                post: (),
            },
        }
    }

    fn update_params<PP: Prefix, Ospf: bgpsim::prelude::OspfProcess>(
        &mut self,
        routers: &HashMap<RouterId, bgpsim::types::NetworkDevice<PP, Ospf>>,
        _net: &bgpsim::types::PhysicalNetwork,
        bgp_queues: &mut HashMap<(RouterId, RouterId), BgpSessionQueue<P>>,
    ) {
        self.geo_distance.update_params(routers);
        // update all the propagation delays
        for ((src, dst), queue) in bgp_queues.iter_mut() {
            let delay = self
                .geo_distance
                .get_path_distance(*src, *dst)
                .map(|(x, _)| x)
                .unwrap_or(NotNan::new(1.0).unwrap())
                .into_inner();
            queue.b.queue.delay = delay..=(delay + self.variation.into_inner());
        }
    }
}

#[derive(Clone, Debug)]
pub struct DebounceQueue<P: Prefix> {
    queue: Option<Message<P>>,
    last_dequeued: Option<BgpEvent<P>>,
    pub debounce_time: NotNan<f64>,
}

impl<P: Prefix> DebounceQueue<P> {
    pub fn new(debounce_time: NotNan<f64>) -> Self {
        Self {
            queue: None,
            last_dequeued: None,
            debounce_time,
        }
    }
}

impl<P: Prefix> SessionQueue<P> for DebounceQueue<P> {
    fn push(&mut self, mut msg: Message<P>) {
        if Some(&msg.e) == self.last_dequeued.as_ref() {
            // We are trying to deliver the same message that is already delivered. Ignore that
            // message and clear the queue
            self.queue = None;
            return;
        }
        // if we reach this point, we must replace the queue with the message.
        msg.t += self.debounce_time;
        self.queue = Some(msg);
    }

    fn poll(&mut self) -> Poll<P> {
        if let Some(msg) = self.queue.take() {
            self.last_dequeued = Some(msg.e.clone());
            Poll::Msg(msg)
        } else {
            Poll::Empty
        }
    }

    fn next_t(&self) -> Option<NotNan<f64>> {
        self.queue.as_ref().map(|x| x.t)
    }

    fn len(&self) -> usize {
        if self.queue.is_some() {
            1
        } else {
            0
        }
    }

    fn clear(&mut self) {
        self.queue = None;
    }
}

/// Capture all messages exchanged on the session.
#[derive(Clone, Debug)]
pub struct Capture<P: Prefix> {
    pub stream: Vec<Message<P>>,
}

impl<P: Prefix> SessionFilter<P> for Capture<P> {
    fn apply(&mut self, msg: Message<P>) -> Option<Message<P>> {
        self.stream.push(msg.clone());
        Some(msg)
    }

    fn clear(&mut self) {
        self.stream.clear()
    }
}

impl<P: Prefix> SessionFilter<P> for Failure {
    fn apply(&mut self, msg: Message<P>) -> Option<Message<P>> {
        let new_e = match (self, msg.e) {
            (Failure::BGPDropUpdate(_), BgpEvent::Update(_)) => None,
            (Failure::BGPDropUpdate(_), BgpEvent::Withdraw(p)) => Some(BgpEvent::Withdraw(p)),
            (Failure::BGPDropWithdraw(_), BgpEvent::Update(r)) => Some(BgpEvent::Update(r)),
            (Failure::BGPDropWithdraw(_), BgpEvent::Withdraw(_)) => None,
            (Failure::BGPChangeLocalPref(_, x), BgpEvent::Update(mut r)) => {
                r.local_pref = Some(*x);
                Some(BgpEvent::Update(r))
            }
            (Failure::BGPChangeLocalPref(_, _), e) => Some(e),
            (Failure::BGPChangeCommunity(_, c), BgpEvent::Update(mut r)) => {
                let cc = c.abs() as u32;
                if *c < 0 {
                    r.community.remove(&cc);
                } else {
                    r.community.insert(cc);
                }
                Some(BgpEvent::Update(r))
            }
            (Failure::BGPChangeCommunity(_, _), e) => Some(e),
        }?;

        Some(Message { e: new_e, t: msg.t })
    }

    fn clear(&mut self) {}
}
