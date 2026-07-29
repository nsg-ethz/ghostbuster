//! This module should eventually be migrated into bgpsim

pub mod failure_queue;
pub mod geo_distances;
pub mod reordering_queue;

use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    ops::RangeInclusive,
};

use bgpsim::{
    bgp::BgpEvent,
    event::{Event, EventQueue},
    types::{Prefix, RouterId},
};
use ordered_float::NotNan;
use rand::{thread_rng, Rng};

/// Trait to initialize a specific queue.
pub trait QueueInit<SQ> {
    fn init(&mut self, src: RouterId, dst: RouterId) -> SQ;

    fn update_params<P: Prefix, Ospf: bgpsim::prelude::OspfProcess>(
        &mut self,
        routers: &HashMap<RouterId, bgpsim::types::NetworkDevice<P, Ospf>>,
        net: &bgpsim::types::PhysicalNetwork,
        bgp_queues: &mut HashMap<(RouterId, RouterId), SQ>,
    );
}

pub struct DispatchQueue<P: Prefix, SQ, F> {
    queue_init: F,
    ospf_delay: NotNan<f64>,
    bgp_queues: HashMap<(RouterId, RouterId), SQ>,
    ospf_queue: VecDeque<Event<P, NotNan<f64>>>,
    next_to_pop: BTreeSet<NextQueue>,
    current_time: NotNan<f64>,
}

impl<P: Prefix, SQ, F> DispatchQueue<P, SQ, F> {
    pub fn new(queue_init: F, ospf_delay: NotNan<f64>) -> Self {
        Self {
            queue_init,
            ospf_delay,
            bgp_queues: HashMap::new(),
            ospf_queue: VecDeque::new(),
            next_to_pop: BTreeSet::new(),
            current_time: NotNan::default(),
        }
    }
}

fn advance_time(current_time: &mut NotNan<f64>, next_time: NotNan<f64>, next_queue_t: NotNan<f64>) {
    assert_eq!(
        next_time, next_queue_t,
        "Time of poll must match the time in the next_to_pop queue"
    );
    assert!(next_time >= *current_time, "Time must move forward");
    *current_time = next_time;
}

impl<P: Prefix, SQ: SessionQueue<P> + Clone, F: QueueInit<SQ>> EventQueue<P>
    for DispatchQueue<P, SQ, F>
{
    type Priority = NotNan<f64>;

    fn push<Ospf: bgpsim::prelude::OspfProcess>(
        &mut self,
        mut event: Event<P, Self::Priority>,
        _: &HashMap<RouterId, bgpsim::types::NetworkDevice<P, Ospf>>,
        _: &bgpsim::types::PhysicalNetwork,
    ) {
        let next_queue = match &event {
            Event::Bgp { src, dst, .. } => NextQueue {
                key: NextQueueKey::Bgp(*src, *dst),
                t: self.current_time,
            },
            Event::Ospf { .. } => NextQueue {
                key: NextQueueKey::Ospf,
                t: self.current_time + self.ospf_delay,
            },
        };
        *event.priority_mut() = next_queue.t;

        match next_queue.key {
            NextQueueKey::Bgp(src, dst) => {
                let queue = self
                    .bgp_queues
                    .entry((src, dst))
                    .or_insert_with(|| self.queue_init.init(src, dst));

                // remember what the old next_t was
                let old_next_t = queue.next_t();

                // push the message into the queue
                queue.push(event.into());

                // compute the new next_t
                let new_next_t = queue.next_t();

                // If the new next_t is larger than the old one, we must update the value in the
                // next_to_pop queue
                match (old_next_t, new_next_t) {
                    (None, None) => {} // nothing to do
                    (None, Some(t)) => {
                        // remove the old entry
                        self.next_to_pop.remove(&NextQueue {
                            key: next_queue.key,
                            t,
                        });
                    }
                    (Some(t), None) => {
                        // put the new time into the queue
                        self.next_to_pop.insert(NextQueue {
                            key: next_queue.key,
                            t,
                        });
                    }
                    (Some(old), Some(new)) if new == old => {} // next trigger time did not change.
                    (Some(old), Some(new)) => {
                        // next triggwer time changed. replace the old with the new entry
                        self.next_to_pop.remove(&NextQueue {
                            key: next_queue.key,
                            t: old,
                        });
                        self.next_to_pop.insert(NextQueue {
                            key: next_queue.key,
                            t: new,
                        });
                    }
                }

                // att the enw time to next_to_pop
                if let Some(t) = queue.next_t() {
                    self.next_to_pop.insert(NextQueue {
                        key: next_queue.key,
                        t,
                    });
                }
            }
            NextQueueKey::Ospf => {
                // update the next queue priority list only if the ospf queue is currently empty.
                if self.ospf_queue.is_empty() {
                    self.next_to_pop.insert(next_queue);
                }
                self.ospf_queue.push_back(event);
            }
        }
    }

    fn pop(&mut self) -> Option<Event<P, Self::Priority>> {
        'repeat: loop {
            let next_queue = self.next_to_pop.pop_first()?;
            self.current_time = next_queue.t;

            match next_queue.key {
                NextQueueKey::Bgp(src, dst) => {
                    let queue = self.bgp_queues.get_mut(&(src, dst)).unwrap();
                    match queue.poll() {
                        Poll::Msg(msg) => {
                            advance_time(&mut self.current_time, msg.t, next_queue.t);
                            // update the next_to_pop
                            if let Some(t) = queue.next_t() {
                                self.next_to_pop.insert(NextQueue {
                                    key: next_queue.key,
                                    t,
                                });
                            }
                            // return the message
                            return Some(msg.into_event(src, dst));
                        }
                        Poll::NotReadyYet(t) => {
                            advance_time(&mut self.current_time, t, next_queue.t);
                            continue 'repeat;
                        }
                        Poll::Empty => continue 'repeat,
                    }
                }
                NextQueueKey::Ospf => {
                    let Some(event) = self.ospf_queue.pop_front() else {
                        continue 'repeat;
                    };
                    // assert that the time matches
                    assert_eq!(next_queue.t, *event.priority());

                    // update next_to_pop if there are still events in that queue
                    if let Some(next) = self.ospf_queue.front() {
                        self.next_to_pop.insert(NextQueue {
                            key: next_queue.key,
                            t: *next.priority(),
                        });
                    }
                    return Some(event);
                }
            }
        }
    }

    fn peek(&self) -> Option<&Event<P, Self::Priority>> {
        unimplemented!("Peeking does not work on the session queue, unfortunately.")
    }

    fn len(&self) -> usize {
        self.ospf_queue.len()
            + self
                .bgp_queues
                .values()
                .map(SessionQueue::len)
                .sum::<usize>()
    }

    fn clear(&mut self) {
        self.ospf_queue.clear();
        self.next_to_pop.clear();
        self.bgp_queues.values_mut().for_each(SessionQueue::clear);
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
        conquered.ospf_queue = self.ospf_queue.clone();
        conquered.next_to_pop = self.next_to_pop.clone();
        conquered
    }
}

/// A mesasge contains an event and a time when that event is dequeued.
///
/// The message can have an empty event. This is used to indicate that something internal in the
/// in the queue changed, and it needs to be polled again. This also means that the implementation
/// is far from optimized, but is designed to be as general as possible.
#[derive(Clone, Debug)]
pub struct Message<P: Prefix> {
    pub e: BgpEvent<P>,
    pub t: NotNan<f64>,
}

#[derive(Debug)]
pub enum Poll<P: Prefix> {
    Msg(Message<P>),
    NotReadyYet(NotNan<f64>),
    Empty,
}

impl<P: Prefix> Message<P> {
    pub fn into_event(self, src: RouterId, dst: RouterId) -> Event<P, NotNan<f64>> {
        Event::Bgp {
            p: self.t,
            src,
            dst,
            e: self.e,
        }
    }
}

impl<P: Prefix> From<Event<P, NotNan<f64>>> for Message<P> {
    fn from(value: Event<P, NotNan<f64>>) -> Self {
        let Event::Bgp { p, e, .. } = value else {
            panic!("Cannot convert a Non-BGP Event to a BGP Message.");
        };
        Self { e, t: p }
    }
}

/// A session queue is a queue on a single BGP session. It implements only a subset of all functions
/// of the Event queue.
pub trait SessionQueue<P: Prefix> {
    /// Push a new event into the queue
    fn push(&mut self, msg: Message<P>);

    /// Do work on the queue. This function either produces a new message, updates something
    /// internally (and returns `Poll::NotReadyYet`), or returns `Poll::Empty` indicating that no
    /// work was done and the queue is completely empty.
    fn poll(&mut self) -> Poll<P>;

    /// Get the time at which the next work on the queue will have to be done. If it returns none,
    /// then the queue is empty.
    fn next_t(&self) -> Option<NotNan<f64>>;

    /// Get the number of messages in the queue. This is an overestimation.
    fn len(&self) -> usize;

    /// Clear all messages in the queue
    fn clear(&mut self);
}

/// A simple deterministic session queue that delays all packets with a deterministic, constant
/// delay.
#[derive(Clone, Debug)]
pub struct DeterministicSessionQueue<P: Prefix> {
    queue: VecDeque<Message<P>>,
    pub delay: NotNan<f64>,
}

impl<P: Prefix> DeterministicSessionQueue<P> {
    /// Create a new queue with the given delay
    pub fn new(delay: NotNan<f64>) -> Self {
        Self {
            queue: Default::default(),
            delay,
        }
    }
}

impl<P: Prefix> SessionQueue<P> for DeterministicSessionQueue<P> {
    fn push(&mut self, mut msg: Message<P>) {
        msg.t += self.delay;
        self.queue.push_back(msg);
    }

    fn poll(&mut self) -> Poll<P> {
        match self.queue.pop_front() {
            Some(m) => Poll::Msg(m),
            None => Poll::Empty,
        }
    }

    fn next_t(&self) -> Option<NotNan<f64>> {
        self.queue.get(0).map(|m| m.t)
    }

    fn len(&self) -> usize {
        self.queue.len()
    }

    fn clear(&mut self) {
        self.queue.clear()
    }
}

/// A randomized session queue that delays all packets with a delay sampled uniformly within the
/// given range. The queue ensures that all packets are dequeued in FIFO order, (as TCP would
/// ensure).
#[derive(Clone, Debug)]
pub struct RandomizedFifoSessionQueue<P: Prefix> {
    queue: VecDeque<Message<P>>,
    pub delay: RangeInclusive<f64>,
}

impl<P: Prefix> RandomizedFifoSessionQueue<P> {
    /// Create a new queue with the given delay
    pub fn new(delay_min: NotNan<f64>, delay_max: NotNan<f64>) -> Self {
        Self {
            queue: Default::default(),
            delay: RangeInclusive::new(delay_min.into_inner(), delay_max.into_inner()),
        }
    }
}

impl<P: Prefix> SessionQueue<P> for RandomizedFifoSessionQueue<P> {
    fn push(&mut self, mut msg: Message<P>) {
        let mut rng = thread_rng();
        let delay = rng.gen_range(self.delay.clone());
        msg.t += NotNan::new(delay).unwrap();

        // make sure the time is at least as high as the last enqueued message
        if !self.queue.is_empty() {
            let last_t = self.queue.get(self.queue.len() - 1).unwrap().t;
            if last_t > msg.t {
                msg.t = last_t;
            }
        }

        self.queue.push_back(msg);
    }

    fn poll(&mut self) -> Poll<P> {
        match self.queue.pop_front() {
            Some(m) => Poll::Msg(m),
            None => Poll::Empty,
        }
    }

    fn next_t(&self) -> Option<NotNan<f64>> {
        self.queue.get(0).map(|m| m.t)
    }

    fn len(&self) -> usize {
        self.queue.len()
    }

    fn clear(&mut self) {
        self.queue.clear()
    }
}

#[derive(Clone, Debug)]
pub struct SessionQueueSequence<A, B> {
    pub a: A,
    pub b: B,
}

impl<P: Prefix, A: SessionQueue<P>, B: SessionQueue<P>> SessionQueue<P>
    for SessionQueueSequence<A, B>
{
    fn push(&mut self, msg: Message<P>) {
        self.a.push(msg)
    }

    fn poll(&mut self) -> Poll<P> {
        // determine which one is first
        match (self.a.next_t(), self.b.next_t()) {
            (None, None) => Poll::Empty,
            // in the following case, we must make progress on queue b
            (None, Some(_)) => self.b.poll(),
            (Some(a), Some(b)) if b <= a => self.b.poll(),
            // in any other case, we make progress on a.
            _ => {
                match self.a.poll() {
                    Poll::Msg(msg) => {
                        let t = msg.t;
                        self.b.push(msg);
                        Poll::NotReadyYet(t)
                    },
                    Poll::NotReadyYet(t) => Poll::NotReadyYet(t),
                    Poll::Empty => panic!("Queue yielded `Poll::Empty`, while a preceeding call to `next_t` yielded `Some(t)`."),
                }
            }
        }
    }

    fn next_t(&self) -> Option<NotNan<f64>> {
        match (self.a.next_t(), self.b.next_t()) {
            (None, None) => None,
            // in the following case, we must make progress on queue b
            (None, Some(b)) => Some(b),
            (Some(a), Some(b)) if b <= a => Some(b),
            // in any other case, we make progress on a.
            (Some(a), _) => Some(a),
        }
    }

    fn len(&self) -> usize {
        self.a.len() + self.b.len()
    }

    fn clear(&mut self) {
        self.a.clear();
        self.b.clear();
    }
}

/// A trait that allows you to apply a filter on a message, or to capture all messages on that
/// session. See the `FilteredSessionQueue` on how to use this feature.
pub trait SessionFilter<P: Prefix> {
    /// Apply the filter or transformation to the message
    fn apply(&mut self, msg: Message<P>) -> Option<Message<P>>;

    fn clear(&mut self);
}

impl<P: Prefix> SessionFilter<P> for () {
    fn apply(&mut self, msg: Message<P>) -> Option<Message<P>> {
        Some(msg)
    }

    fn clear(&mut self) {}
}

impl<P: Prefix, F1: SessionFilter<P>> SessionFilter<P> for (F1,) {
    fn apply(&mut self, msg: Message<P>) -> Option<Message<P>> {
        let msg = self.0.apply(msg)?;
        Some(msg)
    }
    fn clear(&mut self) {
        self.0.clear()
    }
}

impl<P: Prefix, F1: SessionFilter<P>, F2: SessionFilter<P>> SessionFilter<P> for (F1, F2) {
    fn apply(&mut self, msg: Message<P>) -> Option<Message<P>> {
        let msg = self.0.apply(msg)?;
        let msg = self.1.apply(msg)?;
        Some(msg)
    }

    fn clear(&mut self) {
        self.0.clear();
        self.1.clear();
    }
}

impl<P: Prefix, F1: SessionFilter<P>, F2: SessionFilter<P>, F3: SessionFilter<P>> SessionFilter<P>
    for (F1, F2, F3)
{
    fn apply(&mut self, msg: Message<P>) -> Option<Message<P>> {
        let msg = self.0.apply(msg)?;
        let msg = self.1.apply(msg)?;
        let msg = self.2.apply(msg)?;
        Some(msg)
    }

    fn clear(&mut self) {
        self.0.clear();
        self.1.clear();
        self.2.clear();
    }
}

impl<
        P: Prefix,
        F1: SessionFilter<P>,
        F2: SessionFilter<P>,
        F3: SessionFilter<P>,
        F4: SessionFilter<P>,
    > SessionFilter<P> for (F1, F2, F3, F4)
{
    fn apply(&mut self, msg: Message<P>) -> Option<Message<P>> {
        let msg = self.0.apply(msg)?;
        let msg = self.1.apply(msg)?;
        let msg = self.2.apply(msg)?;
        let msg = self.3.apply(msg)?;
        Some(msg)
    }

    fn clear(&mut self) {
        self.0.clear();
        self.1.clear();
        self.2.clear();
        self.3.clear();
    }
}

impl<P: Prefix, F: SessionFilter<P>> SessionFilter<P> for Vec<F> {
    fn apply(&mut self, msg: Message<P>) -> Option<Message<P>> {
        self.iter_mut()
            .fold(Some(msg), |msg, f| msg.and_then(|m| f.apply(m)))
    }

    fn clear(&mut self) {
        self.iter_mut().for_each(|x| x.clear())
    }
}

/// A queue that applies a filter before and after
#[derive(Clone, Debug)]
pub struct FilteredSessionQueue<Pre, Q, Post> {
    pub pre: Pre,
    pub queue: Q,
    pub post: Post,
}

impl<P: Prefix, Pre: SessionFilter<P>, Q: SessionQueue<P>, Post: SessionFilter<P>> SessionQueue<P>
    for FilteredSessionQueue<Pre, Q, Post>
{
    fn push(&mut self, msg: Message<P>) {
        if let Some(msg) = self.pre.apply(msg) {
            self.queue.push(msg)
        }
    }

    fn poll(&mut self) -> Poll<P> {
        match self.queue.poll() {
            Poll::Msg(msg) => {
                let t = msg.t;
                match self.post.apply(msg) {
                    Some(msg) => Poll::Msg(msg),
                    None => Poll::NotReadyYet(t),
                }
            }
            p => p,
        }
    }

    fn next_t(&self) -> Option<NotNan<f64>> {
        self.queue.next_t()
    }

    fn len(&self) -> usize {
        self.queue.len()
    }

    fn clear(&mut self) {
        self.queue.clear();
        self.pre.clear();
        self.post.clear();
    }
}

///////////////////////////////////////////////////////////
// Private Utility data structures needed for the queue //
///////////////////////////////////////////////////////////

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum NextQueueKey {
    Bgp(RouterId, RouterId),
    Ospf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct NextQueue {
    key: NextQueueKey,
    t: NotNan<f64>,
}

impl Ord for NextQueue {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.t.cmp(&other.t)
    }
}

impl PartialOrd for NextQueue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
