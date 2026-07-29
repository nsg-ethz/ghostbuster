//! capturing.

use core::f64;
use std::{
    cell::RefCell,
    collections::{BinaryHeap, HashMap, VecDeque},
    sync::{atomic::AtomicUsize, Arc},
};

use bgpsim::{
    bgp::BgpEvent,
    event::{Event, EventQueue},
    types::{Prefix, RouterId, SinglePrefix},
};
use ordered_float::NotNan;
use rand::{rngs::StdRng, Rng, RngCore, SeedableRng};
use serde::{Deserialize, Serialize};

use crate::session_queue::{
    FilteredSessionQueue as Filter, Message, Poll, QueueInit, SessionFilter, SessionQueue,
    SessionQueueSequence as Sequence,
};

// This module only works for single prefix.
type P = SinglePrefix;

// FilterOutPre --> Debounce --> FilterOutPost --> MonitorOut --> RandomQueue --> MonitorIn --> RandomQueue --> FilterIn
pub type ReorderingSessionQueue = Sequence<
    Filter<(), RandomDebouncer, Failures>,
    Sequence<Filter<Monitor, RandomQueue, Monitor>, Filter<Failures, RandomQueue, ()>>,
>;

/// Record all events and store them in a vector.
#[derive(Clone, Debug)]
pub struct Monitor {
    pub log: Vec<Message<P>>,
    time: Arc<AtomicUsize>,
    use_msg_time: bool,
    /// When false, `apply` skips recording into `log`. Used to avoid building measurement traces
    /// on sessions never actually monitored.
    enabled: bool,
}

#[derive(Clone, Debug)]
pub struct RandomQueue {
    queue: VecDeque<Message<P>>,
    /// Whether to randomization, or whether to rely on the fixed time of the packet.
    fixed_delay: Option<NotNan<f64>>,
    /// We use a RefCell here to get interior mutablility.
    rng: RefCell<StdRng>,
}

#[derive(Clone, Debug)]
pub struct RandomDebouncer {
    inner: RandomQueue,
    last_dequeued: Option<BgpEvent<P>>,
    enable: bool,
    rng: StdRng,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NextQueue {
    time: NotNan<f64>,
    session: (RouterId, RouterId),
}

impl Ord for NextQueue {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.time
            .cmp(&other.time)
            .reverse()
            .then_with(|| self.session.cmp(&other.session))
    }
}

impl PartialOrd for NextQueue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug)]
pub struct ScramblingQueue<SQ, F> {
    queue_init: F,
    bgp_queues: HashMap<(RouterId, RouterId), SQ>,
    bgp_queue_heap: BinaryHeap<NextQueue>,
    base_queue: VecDeque<Event<P, ()>>,
    pub current_time: NotNan<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Failure {
    pub router: RouterId,
    pub neighbor: RouterId,
    pub location: FailureLocation,
    pub bug: Bug,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum FailureLocation {
    Ingress,
    Egress,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Bug {
    DropUpdates {
        p: f64,
    },
    DropWithdraws {
        p: f64,
    },
    TransformCommunity {
        p: f64,
        swap: u32,
    },
    TransformLocalPref {
        p: f64,
        new_val: u32,
    },
    /// this will not to anything! It is just here to model the failure.
    /// TODO remove me.
    WithdrawToUpdate {
        p: f64,
    },
    /// this will not to anything! It is just here to model the failure.
    /// TODO remove me.
    UpdateToWithdraw {
        p: f64,
    },
}

#[derive(Clone, Debug)]
pub struct Failures {
    rng: StdRng,
    pub bugs: Vec<Bug>,
    pub num_triggered: usize,
}

impl<SQ: SessionQueue<P> + Clone, F: QueueInit<SQ>> EventQueue<P> for ScramblingQueue<SQ, F> {
    type Priority = ();

    fn push<Ospf: bgpsim::prelude::OspfProcess>(
        &mut self,
        event: Event<P, ()>,
        _: &HashMap<RouterId, bgpsim::types::NetworkDevice<P, Ospf>>,
        _: &bgpsim::types::PhysicalNetwork,
    ) {
        match event {
            Event::Bgp { src, dst, e, .. } => {
                let queue = self
                    .bgp_queues
                    .entry((src, dst))
                    .or_insert_with(|| self.queue_init.init(src, dst));
                // check if we will need to update the time.
                let update_time = queue.next_t().is_none();
                queue.push(Message {
                    e,
                    t: self.current_time,
                });
                // maybe update the next_queue
                if update_time {
                    if let Some(time) = queue.next_t() {
                        self.bgp_queue_heap.push(NextQueue {
                            time,
                            session: (src, dst),
                        });
                    }
                }
            }
            other => self.base_queue.push_back(other),
        }
    }

    fn pop(&mut self) -> Option<Event<P, Self::Priority>> {
        // first, pop the others
        if let Some(e) = self.base_queue.pop_front() {
            return Some(e);
        };
        loop {
            let Some(NextQueue { session, .. }) = self.bgp_queue_heap.pop() else {
                return None;
            };
            let next_queue = self.bgp_queues.get_mut(&session).unwrap();
            let poll = next_queue.poll();
            if let Some(time) = next_queue.next_t() {
                self.bgp_queue_heap.push(NextQueue { session, time });
            }
            match poll {
                Poll::Msg(message) => {
                    self.current_time = message.t;
                    return Some(Event::Bgp {
                        p: (),
                        src: session.0,
                        dst: session.1,
                        e: message.e,
                    });
                }
                Poll::NotReadyYet(t) => {
                    self.current_time = t;
                }
                Poll::Empty => {}
            }
        }
    }

    fn peek(&self) -> Option<&Event<P, Self::Priority>> {
        unimplemented!("Peeking does not work on the session queue, unfortunately.")
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn len(&self) -> usize {
        self.base_queue.len()
            + self
                .bgp_queues
                .values()
                .map(SessionQueue::len)
                .sum::<usize>()
    }

    fn clear(&mut self) {
        self.base_queue.clear();
        self.bgp_queues.values_mut().for_each(SessionQueue::clear);
        self.bgp_queue_heap.clear();
    }

    fn update_params<Ospf: bgpsim::prelude::OspfProcess>(
        &mut self,
        routers: &HashMap<RouterId, bgpsim::types::NetworkDevice<P, Ospf>>,
        net: &bgpsim::types::PhysicalNetwork,
    ) {
        self.queue_init
            .update_params(routers, net, &mut self.bgp_queues)
    }

    fn get_time(&self) -> Option<f64> {
        Some(self.current_time.into_inner())
    }

    unsafe fn clone_events(&self, mut conquered: Self) -> Self {
        conquered.bgp_queues = self.bgp_queues.clone();
        conquered.base_queue = self.base_queue.clone();
        conquered
    }
}

impl SessionFilter<P> for Monitor {
    fn apply(&mut self, msg: Message<P>) -> Option<Message<P>> {
        if !self.enabled {
            return Some(msg);
        }
        let time = if self.use_msg_time {
            msg.t
        } else {
            NotNan::new(self.time.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as f64)
                .unwrap()
        };
        let mut log_msg = msg.clone();
        log_msg.t = time;
        self.log.push(log_msg);
        Some(msg)
    }

    fn clear(&mut self) {
        self.log = Vec::new();
    }
}

impl SessionQueue<P> for RandomQueue {
    fn push(&mut self, mut msg: Message<P>) {
        if let Some(t) = self.fixed_delay {
            msg.t += t;
        }
        self.queue.push_back(msg);
    }

    fn poll(&mut self) -> Poll<P> {
        match self.queue.pop_front() {
            Some(e) => Poll::Msg(e),
            None => Poll::Empty,
        }
    }

    fn next_t(&self) -> Option<NotNan<f64>> {
        if self.queue.is_empty() {
            None
        } else if self.fixed_delay.is_some() {
            // without randomness
            self.queue.front().map(|x| x.t)
        } else {
            Some(NotNan::new(self.rng.borrow_mut().gen_range(0.0..=1.0)).unwrap())
        }
    }

    fn len(&self) -> usize {
        self.queue.len()
    }

    fn clear(&mut self) {
        self.queue.clear();
    }
}

impl SessionQueue<P> for RandomDebouncer {
    fn push(&mut self, msg: Message<P>) {
        self.inner.push(msg);
    }

    fn poll(&mut self) -> Poll<P> {
        if self.enable {
            let last_equal = self.inner.queue.front().map(|x| &x.e) == self.last_dequeued.as_ref();
            let min_num_to_pop = 1;
            let max_num_to_pop = self.inner.queue.len() + if last_equal { 1 } else { 0 };
            let to_pop = self.rng.gen_range(min_num_to_pop..=max_num_to_pop);
            (0..to_pop)
                .map(|_| self.inner.poll())
                .last()
                .unwrap_or(Poll::Empty)
        } else {
            self.inner.poll()
        }
    }

    fn next_t(&self) -> Option<NotNan<f64>> {
        self.inner.next_t()
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn clear(&mut self) {
        self.inner.clear();
    }
}

impl SessionFilter<P> for Failures {
    fn apply(&mut self, mut msg: Message<P>) -> Option<Message<P>> {
        for bug in &self.bugs {
            match (bug, &mut msg.e) {
                (Bug::DropUpdates { p }, BgpEvent::Update(_))
                | (Bug::DropWithdraws { p }, BgpEvent::Withdraw(_)) => {
                    if self.rng.gen_bool(*p) {
                        // trigger bug
                        self.num_triggered += 1;
                        return None;
                    }
                }
                (Bug::TransformCommunity { p, swap }, BgpEvent::Update(bgp_route)) => {
                    if self.rng.gen_bool(*p) {
                        if bgp_route.community.contains(swap) {
                            bgp_route.community.remove(swap);
                        } else {
                            bgp_route.community.insert(*swap);
                        }
                        self.num_triggered += 1;
                    }
                }
                (Bug::TransformLocalPref { p, new_val }, BgpEvent::Update(bgp_route)) => {
                    if self.rng.gen_bool(*p) {
                        if bgp_route.local_pref.unwrap_or(100) != *new_val {
                            bgp_route.local_pref = Some(*new_val);
                            self.num_triggered += 1;
                        }
                    }
                }
                _ => {}
            };
        }
        Some(msg)
    }

    fn clear(&mut self) {
        self.num_triggered = 0;
    }
}

pub type ReorderingQueue = ScramblingQueue<ReorderingSessionQueue, ReorderingSessionQueueInit>;

impl ReorderingQueue {
    pub fn init_session(
        &mut self,
        src: RouterId,
        dst: RouterId,
        with_reordering: bool,
        with_debouncing: bool,
        time_scale: f64,
    ) {
        let q = self
            .bgp_queues
            .entry((src, dst))
            .or_insert_with(|| self.queue_init.init(src, dst));
        // disable reordering
        if !with_reordering {
            q.a.queue.use_fixed_timing(time_scale);
            q.b.a.queue.use_fixed_timing(time_scale);
            q.b.b.queue.use_fixed_timing(time_scale);
            q.b.a.pre.use_msg_time = true;
            q.b.a.post.use_msg_time = true;
        }
        // disable debouncing
        if !with_debouncing {
            q.a.queue.disable();
        }
    }

    /// Only record measurement traces on sessions incident to `router`: the outgoing (`pre`)
    /// monitor where `router` is the source, and the incoming (`post`) monitor where `router` is
    /// the destination. Every other session skips `Monitor::apply`'s log push entirely — this is
    /// the dominant per-task allocation when only a single router's trace is collected.
    pub fn restrict_monitoring_to(&mut self, router: RouterId) {
        for ((src, dst), q) in self.bgp_queues.iter_mut() {
            q.b.a.pre.enabled = *src == router;
            q.b.a.post.enabled = *dst == router;
        }
    }

    pub fn add_failure(&mut self, failure: Failure) {
        let (src, dst) = match failure.location {
            FailureLocation::Ingress => (failure.neighbor, failure.router),
            FailureLocation::Egress => (failure.router, failure.neighbor),
        };
        let q = self
            .bgp_queues
            .entry((src, dst))
            .or_insert_with(|| self.queue_init.init(src, dst));
        match failure.location {
            FailureLocation::Ingress => q.b.b.pre.bugs.push(failure.bug),
            FailureLocation::Egress => q.a.post.bugs.push(failure.bug),
        }
    }

    pub fn get_global_time(&self) -> Arc<AtomicUsize> {
        self.queue_init.global_time.clone()
    }

    /// Get the number of times a failure was triggered.
    pub fn num_failures_triggered(&self) -> usize {
        self.bgp_queues
            .values()
            .map(|q| q.a.post.num_triggered + q.b.b.pre.num_triggered)
            .sum()
    }

    /// Get the number of times a failure was triggered on the given router.
    pub fn num_failures_for_router(&self, router: RouterId) -> usize {
        // first count the outgoing ones.
        let out_failures = self
            .bgp_queues
            .iter()
            .filter(|((src, _), _)| *src == router)
            .map(|(_, q)| q.a.post.num_triggered)
            .sum::<usize>();
        // then count the incoming ones as well
        let in_failures = self
            .bgp_queues
            .iter()
            .filter(|((_, dst), _)| *dst == router)
            .map(|(_, q)| q.b.b.pre.num_triggered)
            .sum::<usize>();

        out_failures + in_failures
    }

    /// Collect the measured traces of all routers
    pub fn all_measurement_traces(&mut self) -> HashMap<RouterId, Vec<Event<P, NotNan<f64>>>> {
        let mut traces: HashMap<RouterId, Vec<_>> = HashMap::new();
        for ((src, dst), queue) in self.bgp_queues.iter_mut() {
            let (src, dst) = (*src, *dst);
            let ev = |msg: Message<_>| Event::Bgp {
                p: msg.t,
                src,
                dst,
                e: msg.e,
            };

            // for the source, take the measurement pre network delay
            traces
                .entry(src)
                .or_default()
                .extend(std::mem::take(&mut queue.b.a.pre.log).into_iter().map(ev));
            // for the destination, take the measurement post network delay
            traces
                .entry(dst)
                .or_default()
                .extend(std::mem::take(&mut queue.b.a.post.log).into_iter().map(ev));
        }

        // sort all traces
        traces
            .values_mut()
            .for_each(|x| x.sort_by_key(|e| *e.priority()));
        traces
    }

    /// Collect the measured trace at the given router.
    pub fn measurement_trace(&mut self, router: RouterId) -> Vec<Event<P, NotNan<f64>>> {
        let mut messages: Vec<Event<P, NotNan<f64>>> = Vec::new();
        for ((src, dst), queue) in self.bgp_queues.iter_mut() {
            if *src == router {
                // outgoing direction
                messages.extend(
                    std::mem::take(&mut queue.b.a.pre.log)
                        .into_iter()
                        .map(|msg| Event::Bgp {
                            p: msg.t,
                            src: *src,
                            dst: *dst,
                            e: msg.e,
                        }),
                );
            } else if *dst == router {
                // incoming direction
                messages.extend(
                    std::mem::take(&mut queue.b.a.post.log)
                        .into_iter()
                        .map(|msg| Event::Bgp {
                            p: msg.t,
                            src: *src,
                            dst: *dst,
                            e: msg.e,
                        }),
                );
            }
        }
        messages.sort_by_key(|e| *e.priority());
        messages
    }

    /// Manually poll the queues. If it returns Some(None), then there are still messages left, but non yet was produced.
    pub fn poll(&mut self) -> Option<Option<Event<P, NotNan<f64>>>> {
        // first, pop the others
        if let Some(e) = self.base_queue.pop_front() {
            return Some(Some(match e {
                Event::Bgp { src, dst, e, .. } => Event::Bgp {
                    src,
                    dst,
                    e,
                    p: self.current_time,
                },
                Event::Ospf {
                    src, dst, area, e, ..
                } => Event::Ospf {
                    src,
                    dst,
                    area,
                    e,
                    p: self.current_time,
                },
            }));
        };

        let Some(NextQueue { session, .. }) = self.bgp_queue_heap.pop() else {
            return None;
        };
        let next_queue = self.bgp_queues.get_mut(&session).unwrap();
        let poll = next_queue.poll();
        if let Some(time) = next_queue.next_t() {
            self.bgp_queue_heap.push(NextQueue { session, time });
        }
        match poll {
            Poll::Msg(message) => {
                self.current_time = message.t;
                return Some(Some(Event::Bgp {
                    p: self.current_time,
                    src: session.0,
                    dst: session.1,
                    e: message.e,
                }));
            }
            Poll::NotReadyYet(t) => {
                self.current_time = t;
                Some(None)
            }
            Poll::Empty => None,
        }
    }

    pub fn heap_size(&self) -> usize {
        self.base_queue.capacity() * std::mem::size_of::<Event<P, ()>>()
            + self.bgp_queue_heap.capacity() * std::mem::size_of::<NextQueue>()
            + self.bgp_queues.capacity()
                * (std::mem::size_of::<(RouterId, RouterId)>()
                    + std::mem::size_of::<ReorderingSessionQueue>())
            + self
                .bgp_queues
                .values()
                .map(|x| x.heap_size())
                .sum::<usize>()
    }
}

impl ReorderingSessionQueue {
    fn heap_size(&self) -> usize {
        self.a.queue.inner.heap_size()
            + self.a.post.heap_size()
            + self.b.a.pre.heap_size()
            + self.b.a.queue.heap_size()
            + self.b.a.post.heap_size()
            + self.b.b.pre.heap_size()
            + self.b.b.queue.heap_size()
    }
}

impl Monitor {
    fn heap_size(&self) -> usize {
        self.log.capacity() * std::mem::size_of::<Message<P>>()
    }
}

impl Failures {
    fn heap_size(&self) -> usize {
        self.bugs.capacity() * std::mem::size_of::<Bug>()
    }
}

impl RandomQueue {
    fn heap_size(&self) -> usize {
        self.queue.capacity() * std::mem::size_of::<Message<P>>()
    }
}

impl<SQ, F> ScramblingQueue<SQ, F> {
    pub fn new(queue_init: F) -> Self {
        Self {
            queue_init,
            bgp_queues: HashMap::new(),
            bgp_queue_heap: BinaryHeap::new(),
            base_queue: VecDeque::new(),
            current_time: NotNan::default(),
        }
    }
}

impl<SQ, F: QueueInit<SQ>> ScramblingQueue<SQ, F> {
    pub fn session_queue(&mut self, src: RouterId, dst: RouterId) -> &mut SQ {
        self.bgp_queues
            .entry((src, dst))
            .or_insert_with(|| self.queue_init.init(src, dst))
    }
}

impl Monitor {
    fn new(time: Arc<AtomicUsize>) -> Self {
        Self {
            log: Default::default(),
            time,
            use_msg_time: false,
            enabled: true,
        }
    }
}

impl RandomQueue {
    fn new<R: RngCore>(init_rng: &mut R) -> Self {
        Self {
            queue: Default::default(),
            fixed_delay: None,
            rng: RefCell::new(StdRng::from_rng(init_rng).unwrap()),
        }
    }

    fn use_fixed_timing(&mut self, time_scale: f64) {
        self.fixed_delay =
            Some(NotNan::new(time_scale * self.rng.borrow_mut().gen_range(0.0..1.0)).unwrap());
    }
}

impl RandomDebouncer {
    fn new<R: RngCore>(init_rng: &mut R) -> Self {
        Self {
            inner: RandomQueue::new(init_rng),
            last_dequeued: None,
            enable: true,
            rng: StdRng::from_rng(init_rng).unwrap(),
        }
    }

    fn disable(&mut self) {
        self.enable = false;
    }

    fn use_fixed_timing(&mut self, time_scale: f64) {
        self.inner.fixed_delay = Some(NotNan::new(2.0 * time_scale).unwrap());
    }
}

impl Failures {
    fn new<R: RngCore>(init_rng: &mut R) -> Self {
        Self {
            rng: StdRng::from_rng(init_rng).unwrap(),
            bugs: Vec::new(),
            num_triggered: 0,
        }
    }
}

#[derive(Clone)]
pub struct ReorderingSessionQueueInit {
    pub init_rng: StdRng,
    pub global_time: Arc<AtomicUsize>,
}

impl ReorderingSessionQueueInit {
    pub fn new(seed: u64) -> Self {
        Self {
            init_rng: StdRng::seed_from_u64(seed),
            global_time: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl QueueInit<ReorderingSessionQueue> for ReorderingSessionQueueInit {
    fn init(&mut self, _src: RouterId, _dst: RouterId) -> ReorderingSessionQueue {
        Sequence {
            a: Filter {
                pre: (),
                queue: RandomDebouncer::new(&mut self.init_rng),
                post: Failures::new(&mut self.init_rng),
            },
            b: Sequence {
                a: Filter {
                    pre: Monitor::new(self.global_time.clone()),
                    queue: RandomQueue::new(&mut self.init_rng),
                    post: Monitor::new(self.global_time.clone()),
                },
                b: Filter {
                    pre: Failures::new(&mut self.init_rng),
                    queue: RandomQueue::new(&mut self.init_rng),
                    post: (),
                },
            },
        }
    }

    fn update_params<PP: Prefix, Ospf: bgpsim::prelude::OspfProcess>(
        &mut self,
        _routers: &HashMap<RouterId, bgpsim::types::NetworkDevice<PP, Ospf>>,
        _net: &bgpsim::types::PhysicalNetwork,
        _bgp_queues: &mut HashMap<(RouterId, RouterId), ReorderingSessionQueue>,
    ) {
    }
}

#[cfg(test)]
mod test_debouncing {
    use super::{ReorderingQueue, ReorderingSessionQueueInit, P};
    use crate::tests::e_network_route_map_scenario;
    use bgpsim::{network::Network, prelude::InteractiveNetwork, types::RouterId};
    use itertools::Itertools;

    const SEED: u64 = 42;

    /// Build the route-map scenario on a `ReorderingQueue`, with debouncing on or off.
    ///
    /// Reordering is disabled in both cases, so debouncing is the only thing that differs between
    /// the two networks and the comparison below isolates its effect.
    fn scenario(with_reordering: bool, with_debouncing: bool) -> (Network<P, ReorderingQueue>, [RouterId; 3]) {
        let (net, (e, _, _)) = e_network_route_map_scenario(None);

        let mut queue = ReorderingQueue::new(ReorderingSessionQueueInit::new(SEED));
        for r in net.device_indices().sorted() {
            for n in net
                .get_device(r)
                .unwrap()
                .bgp_neighbors()
                .into_iter()
                .sorted()
            {
                queue.init_session(r, n, with_reordering, with_debouncing, 1.0);
            }
        }

        (net.swap_queue(queue), e)
    }

    /// Withdraw the best route and count the BGP messages that crossed the network on the way to
    /// convergence. The queue's own monitors do the counting, since this queue cannot be peeked.
    fn withdraw_and_count_messages(with_reordering: bool, with_debouncing: bool) -> (Network<P, ReorderingQueue>, usize) {
        let (mut net, e) = scenario(with_reordering, with_debouncing);
        // Queue several events before simulating any of them. The debouncer collapses messages
        // that pile up on one session, so it can only do anything when the network is still
        // reacting to one event as the next arrives.
        net.manual_simulation();
        net.withdraw_external_route(e[0], P::from(0)).unwrap();
        net.withdraw_external_route(e[1], P::from(0)).unwrap();
        net.advertise_external_route(e[0], P::from(0), [1], None, None)
            .unwrap();
        net.withdraw_external_route(e[2], P::from(0)).unwrap();
        net.simulate().unwrap();
        let messages = net
            .queue_mut()
            .all_measurement_traces()
            .values()
            .map(|trace| trace.len())
            .sum();
        (net, messages)
    }

    /// Withdrawing the best route makes the border routers re-advertise, and the debouncer is what
    /// stops every intermediate step from being sent on. Debouncing must therefore cut the number
    /// of messages down without changing where traffic ends up.
    #[test]
    fn debouncing_reduces_messages_without_changing_the_outcome() {
        let (plain, plain_messages) = withdraw_and_count_messages(false, false);
        let (debounced, debounced_messages) = withdraw_and_count_messages(false, true);

        assert_eq!(
            plain.get_forwarding_state(),
            debounced.get_forwarding_state(),
            "debouncing must not change the forwarding state the network settles on"
        );
        assert!(
            debounced_messages < plain_messages,
            "debouncing should suppress messages: {debounced_messages} with, {plain_messages} without"
        );
    }
}
