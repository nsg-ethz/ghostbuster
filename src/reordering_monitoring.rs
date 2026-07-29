use core::f64;
use std::collections::HashMap;

use bgpsim::{
    bgp::{BgpEvent, BgpRibEntry, BgpRoute},
    event::Event,
    router::{BgpProcess, Router},
    types::{RouterId, SimplePrefix, SinglePrefix},
};
use itertools::Itertools;
use ordered_float::NotNan;
use petgraph::prelude::*;
use serde::{Deserialize, Serialize};

use crate::recording::MessageSequence;

use super::monitoring::{MonitoringError, MonitoringResult};

type RibArena = typed_arena::Arena<BgpRibEntry<SinglePrefix>>;
type MsgArena = typed_arena::Arena<UncommittedMessagePerSession>;

#[derive(Default)]
pub struct Arena {
    rib: RibArena,
    msg: MsgArena,
}

pub struct ReorderingMonitor<'a> {
    arena: &'a Arena,
    /// This is a copy of the router, but that always remains empty.
    /// It is used to simulate how the route is processed in the absence of other events.
    empty_router: Router<SinglePrefix>,
    /// This router applies all events that we observed. It is used to recover from a failure in
    /// one of the per-session monitors.
    failure_recovery_router: Router<SinglePrefix>,
    /// The data per session.
    pub per_session: HashMap<RouterId, ForkingMonitorPerSession<'a>>,
    /// A counter that tracks and identifies incoming messages.
    incoming_counter: usize,
    /// Whether to make progress on outgoing withdraw messages.
    process_withdraws: ProcessWithdraws,
}

impl<'a> std::fmt::Debug for ReorderingMonitor<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReorderingMonitor")
            .field("empty_router", &self.empty_router)
            .field("failure_recovery_router", &self.failure_recovery_router)
            .field("per_session", &self.per_session)
            .field("incoming_counter", &self.incoming_counter)
            .field("process_withdraws", &self.process_withdraws)
            .finish()
    }
}

/// The forking monitor might carry multiple monitors per session. Each time an error occurs within
/// one fork, the forking monitor will remove that fork. If there are no forks left, the forking
/// monitor will propagate the error.
#[derive(Debug)]
pub struct ForkingMonitorPerSession<'a> {
    forks: Vec<MonitorPerSession<'a>>,
    make_progress_on_withdraw: bool,
    has_unprocessed_withdraw: bool,
}

#[derive(Clone, Debug)]
struct MonitorPerSession<'a> {
    router_id: RouterId,
    neighbor: RouterId,
    /// This structure maintains the current state of the router.
    bgp_process: RibPerSession<'a>,
    /// The set of yet uncommitted messages. As soon as we commit a message, we apply them to
    /// `self.router`.
    uncommitted_messages: HashMap<RouterId, Vec<&'a UncommittedMessagePerSession>>,
    /// History of committed messages. Essentially, whenever messages are committed (in a batch),
    /// the entire batch is pushed back to the queue.
    commit_history: Vec<Vec<usize>>,
}

#[derive(Clone, Default)]
struct RibPerSession<'a> {
    rib_in: HashMap<RouterId, CommittedMessagePerSession<'a>>,
    rib: Option<&'a BgpRibEntry<SinglePrefix>>,
    rib_out: Option<&'a BgpRibEntry<SinglePrefix>>,
}

#[derive(Clone, Copy)]
struct CommittedMessagePerSession<'a> {
    rib_in: &'a BgpRibEntry<SinglePrefix>,
    rib_out: Option<&'a BgpRibEntry<SinglePrefix>>,
}

impl<'a> std::fmt::Debug for RibPerSession<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rib_in = self
            .rib_in
            .iter()
            .map(|(n, m)| (*n, &m.rib_in))
            .collect::<HashMap<_, _>>();
        f.debug_struct("LightweightBgpProcessPerSession")
            .field("rib_in", &rib_in)
            .field("rib", &self.rib)
            .field("rib_out", &self.rib_out)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct UncommittedMessage {
    id: usize,
    time: NotNan<f64>,
    event: BgpEvent<SinglePrefix>,
    rib_entry: Option<BgpRibEntry<SinglePrefix>>,
    outgoing_routes: HashMap<RouterId, BgpRibEntry<SinglePrefix>>,
}

#[derive(Clone, Debug)]
struct UncommittedMessagePerSession {
    id: usize,
    time: NotNan<f64>,
    event: BgpEvent<SinglePrefix>,
    rib_entry: Option<BgpRibEntry<SinglePrefix>>,
    /// The outgoing route to the neighbor we are currently considering
    outgoing_route: Option<BgpRibEntry<SinglePrefix>>,
}

const DISABLE_WITHDRAW_PROCESSING_WHEN_MORE_FORKS_THAN: usize = 128;
const ENABLE_WITHDRAW_PROCESSING_WHEN_LESS_FORKS_THAN: usize = 8;
const KILL_AFTER_EXCEEDING_MORE_FORKS_THAN: usize = 1 << 12; // 4K

#[derive(Clone, Copy, Debug, Serialize, Deserialize, clap::ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ProcessWithdraws {
    Always,
    Never,
    Adaptive,
}

impl ProcessWithdraws {
    pub fn should_disable(&self, num_forks: usize) -> bool {
        match self {
            ProcessWithdraws::Always => false,
            ProcessWithdraws::Never => true,
            ProcessWithdraws::Adaptive => {
                num_forks > DISABLE_WITHDRAW_PROCESSING_WHEN_MORE_FORKS_THAN
            }
        }
    }

    pub fn should_enable(&self, num_forks: usize) -> bool {
        match self {
            ProcessWithdraws::Always => true,
            ProcessWithdraws::Never => false,
            ProcessWithdraws::Adaptive => {
                num_forks <= ENABLE_WITHDRAW_PROCESSING_WHEN_LESS_FORKS_THAN
            }
        }
    }
}

// TODO: Remove all potential panics.
impl<'a> ReorderingMonitor<'a> {
    /// Configures the monitor to make progress on withdraw messages (on by default). If disabled,
    /// withdraw messages still considered for the timeout (`assert_messages_processed_before` and
    /// `final_check`)
    pub fn set_process_withdraw_mode(
        &mut self,
        mode: ProcessWithdraws,
    ) -> MonitoringResult<SinglePrefix> {
        self.process_withdraws = mode;
        let mut result = Ok(());
        for forking in self.per_session.values_mut() {
            let should_enable = mode.should_enable(forking.forks.len());
            if let Err(e) = forking.enable_withdraws(should_enable) {
                result = Err(e);
            }
        }
        result
    }

    /// Process an entire sequence of events and return the first error that occurs. This function
    /// does not call the final check.
    ///
    /// If `mrai` is Some, then we run `assert_messages_processed_before` before processing each message
    /// with the time set to the message's time minus the `mrai`
    pub fn ingest_message_sequence(
        &mut self,
        message_sequence: MessageSequence,
        mrai: Option<NotNan<f64>>,
    ) -> MonitoringResult<SinglePrefix> {
        log::info!("Checking trace on {}", self.router_id().index());
        for msg in message_sequence.into_iter() {
            if let Some(mrai) = mrai {
                let apply_all_before = msg.0.priority() - mrai;
                log::info!("Processing all messages received before {apply_all_before:?}");
                self.assert_messages_processed_before(apply_all_before)?;
            }
            self.process_message(msg.0)?;
        }
        Ok(())
    }
    /// Process a single message. Depending on whether the message is incoming or outging, this
    /// function will call either `process_incoming`, or `process_outgoing`.
    ///
    /// The state of the monitor can be used after this call, even if the function returns an error
    /// (the state is recovered, see the docs on `process_incoming`). The monitor can be used to
    /// check further messages.
    pub fn process_message(
        &mut self,
        event: Event<SinglePrefix, NotNan<f64>>,
    ) -> MonitoringResult<SinglePrefix> {
        let Event::Bgp { src, dst, .. } = event else {
            return Ok(());
        };
        log::info!("Applying {event:?}");
        // If this is the destination router, we just update the current router
        if dst == self.router_id() {
            Ok(self.process_incoming(event))
        } else if src == self.router_id() {
            self.process_outgoing_ok(event)
        } else {
            log::error!(
                "Router {:?} received an event not related to it: {event:?}.",
                self.failure_recovery_router.router_id(),
            );
            return Ok(());
        }
    }

    /// This function assumes that a lot of time has passed---It applies all yet uncommitted
    /// messages and checks whether the currently advertised routes change. If they do, an error
    /// is returned.
    ///
    /// After calling this function, the state of all monitors is such that there are no uncommitted
    /// messages. The monitor can be used to check further messages! Upon an error, the (per-session)
    /// monitor is reset.
    pub fn final_check(&mut self) -> MonitoringResult<SinglePrefix> {
        // Dispatch the final check to all per-session monitors.
        let mut result = Ok(());
        for (neighbor, per_session_monitor) in self.per_session.iter_mut() {
            let per_session_result = per_session_monitor.final_check();
            // On an error, the per-session monitor is left without any forks, and we must recover
            // it by creating a fresh monitor. Otherwise, the next call on it panics on its
            // `assert!(!self.forks.is_empty())`.
            if per_session_result.is_err() {
                let bgp_process =
                    RibPerSession::new(&self.failure_recovery_router.bgp, *neighbor, self.arena);
                *per_session_monitor = ForkingMonitorPerSession::new(
                    self.empty_router.router_id(),
                    *neighbor,
                    bgp_process,
                    self.process_withdraws.should_enable(1),
                );
            }
            result = result.and(per_session_result);
        }
        result
    }

    /// Check whether there exists a single ordering of (past) incoming events that complies with
    /// all individual per-session monitors.
    ///
    /// The algorithm iterates over all possible combination of forks. Ideally, call this function
    /// only when there exists only one fork per monitor.
    ///
    /// After this call, all commit history is cleared (since it either passed or failed.)
    pub fn check_incoming_serializability(&mut self) -> MonitoringResult<SinglePrefix> {
        let mut result = Ok(());

        // iterate over all possible combinations of forks.
        'combinations: for fork_combination in self
            .per_session
            .values()
            .map(|mon| mon.forks.iter())
            .multi_cartesian_product()
        {
            // create an empty graph
            let mut g: petgraph::Graph<(), (), Directed, usize> = Default::default();
            (0..(self.incoming_counter)).for_each(|_| {
                g.add_node(());
            });

            // add all edges according to each monitor
            for mon in fork_combination {
                for (before, after) in mon
                    .commit_history
                    .iter()
                    .zip(mon.commit_history.iter().skip(1))
                {
                    for a in before.iter().copied() {
                        for b in after.iter().copied() {
                            g.add_edge(a.into(), b.into(), ());
                        }
                    }
                }
            }

            if petgraph::algo::is_cyclic_directed(&g) {
                // graph contains a cycle. That means that this order is not serializable.
                result = Err(MonitoringError::Unserializable(self.router_id()));
            } else {
                // Graph contains no cycles; it is serializable
                result = Ok(());
                break 'combinations;
            }
        }

        // clear the commit history
        self.per_session
            .values_mut()
            .flat_map(|x| x.forks.iter_mut())
            .for_each(|mon| mon.commit_history.clear());

        result
    }

    /// Similar to final_check. However, instad of applying all messages, it only applies those
    /// that have been observed before the argument `time`. Upon an error, the (per-session) monitor
    /// is reset.
    pub fn assert_messages_processed_before(
        &mut self,
        time: NotNan<f64>,
    ) -> MonitoringResult<SinglePrefix> {
        // Dispatch to all per-session monitors
        let mut result = Ok(());
        for (neighbor, per_session_monitor) in self.per_session.iter_mut() {
            // If the result was an error, we must recover that session and create a fresh monitor.
            match per_session_monitor
                .assert_messages_processed_before(time)
                .and_then(|_| per_session_monitor.update_enable_withdraws(self.process_withdraws))
            {
                Ok(()) => {}
                Err(e) => {
                    // store the error
                    result = Err(e);
                    // restore the monitor
                    let bgp_process = RibPerSession::new(
                        &self.failure_recovery_router.bgp,
                        *neighbor,
                        self.arena,
                    );
                    *per_session_monitor = ForkingMonitorPerSession::new(
                        self.empty_router.router_id(),
                        *neighbor,
                        bgp_process,
                        self.process_withdraws.should_enable(1),
                    );
                }
            }
        }
        result
    }

    /// Returns the currently maximum number of active forks.
    pub fn max_num_active_forks(&self) -> usize {
        self.per_session
            .values()
            .map(|x| x.forks.len())
            .max()
            .unwrap_or_default()
    }

    /// Get the monitored router id
    pub fn router_id(&self) -> RouterId {
        self.empty_router.router_id()
    }

    /// Register the current event as (potentially) uncommitted. This function never throws an
    /// error.
    fn process_incoming(&mut self, event: Event<SinglePrefix, NotNan<f64>>) {
        // Dispatch all incoming messages to all per-sesssion monitors.
        // first, process the event on the recover router
        unsafe {
            let _ = self.failure_recovery_router.trigger_event(event.clone());
        }

        // then, dispatch the event to all monitors.
        let (from, mut uncommitted_message) = self.generate_uncommitted_message(event);

        log::info!(
            "Incoming event [{:02}] {from:?} -> {:?}: {:?}",
            uncommitted_message.id,
            self.router_id(),
            uncommitted_message.event
        );

        for (neighbor, per_session_monitor) in self.per_session.iter_mut() {
            per_session_monitor.process_incoming(
                from,
                uncommitted_message.into_per_session(*neighbor, self.arena),
            )
        }
    }

    /// Check whether the outgoing event could be possibly sent out. If not, then the (per-session)
    /// monitor is recovered (see below).
    ///
    /// ## Checking Algorithm
    /// For each per-session monitor, we try to find the smallest set of events that could cause the
    /// router to send out the given `event`. If there is such a set, it applies them to its own
    /// internal state (and keeps all the other messages uncommitted).
    ///
    /// There might be multiple different such minimal sets (for example, sending a withdraw out can
    /// be triggered either by the router selecting no route, or by it selecting a route that is
    /// denied in the outgoing route-map to this neighbor). In that case, we fork the monitor (for
    /// this session only). In the future, we apply all changes in all monitors, dropping those that
    /// lead to a contradiction. If we end up with no valid monitors left, we trigger an error.
    ///
    /// ## Failure Recovery
    /// In case there was a failure, the internal state does no longer match the expected one. In
    /// such a case, we simply reset the (per-session) monitor by applying all uncommitted messages
    /// and starting fresh.
    fn process_outgoing_ok(
        &mut self,
        event: Event<SinglePrefix, NotNan<f64>>,
    ) -> MonitoringResult<SinglePrefix> {
        // dispatch the outgoing message only to the monitor for this specific session.
        // extract the relevant information
        let Event::Bgp {
            src,
            dst: neighbor,
            e,
            ..
        } = event
        else {
            log::error!(
                "Router {:?} received a non-BGP event: {event:?}.",
                self.failure_recovery_router.router_id(),
            );
            return Ok(());
        };
        log::info!("Outgoing event [  ] {src:?} -> {neighbor:?}: {e:?}");

        assert_eq!(src, self.router_id());
        let route = match e {
            BgpEvent::Withdraw(_) => None,
            BgpEvent::Update(bgp_route) => Some(bgp_route),
        };

        // Dispatch to the corresponding monitor
        let Some(per_session_monitor) = self.per_session.get_mut(&neighbor) else {
            log::error!(
                "Router {:?} does not have a BGP session with {neighbor:?}",
                self.failure_recovery_router.router_id(),
            );
            return Ok(());
        };
        // If the result was an error, we must recover that session and create a fresh monitor.
        match per_session_monitor
            .process_outgoing_ok(route)
            .and_then(|_| per_session_monitor.update_enable_withdraws(self.process_withdraws))
        {
            Ok(()) => Ok(()),
            Err(e) => {
                let bgp_process =
                    RibPerSession::new(&self.failure_recovery_router.bgp, neighbor, self.arena);
                self.per_session.insert(
                    neighbor,
                    ForkingMonitorPerSession::new(
                        self.empty_router.router_id(),
                        neighbor,
                        bgp_process,
                        self.process_withdraws.should_enable(1),
                    ),
                );
                Err(e)
            }
        }
    }

    /// This function process the event and extracts all the RIB entries as it traverses the router.
    /// The function returns the neighbor that sent the event, and the processed message.
    fn generate_uncommitted_message(
        &mut self,
        event: Event<SinglePrefix, NotNan<f64>>,
    ) -> (RouterId, UncommittedMessage) {
        let Event::Bgp {
            p: time,
            src: neighbor,
            dst,
            e,
        } = event
        else {
            unreachable!("Cannot process a non-BGP event")
        };
        assert_eq!(dst, self.router_id());

        let id = self.incoming_counter;
        self.incoming_counter += 1;

        // withdraw events do not need to be processed
        let BgpEvent::Update(route) = e else {
            return (neighbor, UncommittedMessage::withdraw(id, time));
        };

        let mut r = self.empty_router.clone();
        // safety: We discard r when exiting this function.
        unsafe {
            let _ = r.trigger_event(Event::Bgp {
                p: (),
                src: neighbor,
                dst,
                e: BgpEvent::Update(route.clone()),
            });
        }

        // now, fill in the uncommitted message by reading the resulting RIB from the router
        let rib_entry = r.bgp.get_rib().0.clone();
        let outgoing_routes = r.bgp.get_rib_out().0.clone().unwrap_or_default();

        (
            neighbor,
            UncommittedMessage {
                id,
                time,
                event: BgpEvent::Update(route),
                rib_entry,
                outgoing_routes,
            },
        )
    }
}

impl<'a> ForkingMonitorPerSession<'a> {
    fn new(
        router_id: RouterId,
        neighbor: RouterId,
        bgp_process: RibPerSession<'a>,
        make_progress_on_withdraw: bool,
    ) -> Self {
        Self {
            forks: vec![MonitorPerSession {
                router_id,
                neighbor,
                bgp_process,
                uncommitted_messages: Default::default(),
                commit_history: Vec::new(),
            }],
            make_progress_on_withdraw,
            has_unprocessed_withdraw: false,
        }
    }

    /// Configures the monitor to make progress on withdraw messages (on by default). If disabled,
    /// withdraw messages still considered for the timeout (`assert_messages_processed_before` and
    /// `final_check`)
    pub fn enable_withdraws(&mut self, enable: bool) -> MonitoringResult<SinglePrefix> {
        if self.make_progress_on_withdraw == enable {
            // nothing to do here, no value changes.
            return Ok(());
        }

        // if we now enable it, we might need to process an unprocessed withdraw.
        if enable {
            // re-enable the progress, and maybe execute the last withdraw.
            self.make_progress_on_withdraw = true;
            if self.has_unprocessed_withdraw {
                self.has_unprocessed_withdraw = false;
                log::info!("Process an unprocessed withdraw upon re-enabling withdraw progress.");
                self.process_outgoing_ok(None)?
            }
        } else {
            // disable it
            self.make_progress_on_withdraw = false;
            self.has_unprocessed_withdraw = false;
        }
        Ok(())
    }

    /// Incoming messages are propagated to all
    fn process_incoming(&mut self, from: RouterId, message: &'a UncommittedMessagePerSession) {
        // now, forward this to all the forks
        for fork in self.forks.iter_mut() {
            fork.process_incoming(from, message)
        }
    }

    /// As a message is coming out of the router, I check what messages do need to be processed in
    /// order for this message to be observed.
    ///
    /// This function will remove all the forks that do not comply with the message. In case of an
    /// error, this structure is empty and must be recovered by the caller.
    fn process_outgoing_ok(
        &mut self,
        route: Option<BgpRoute<SinglePrefix>>,
    ) -> MonitoringResult<SinglePrefix> {
        assert!(!self.forks.is_empty());

        if !self.make_progress_on_withdraw {
            if route.is_none() {
                log::debug!("Skip processing withdraw because it is disabled.");
                self.has_unprocessed_withdraw = true;
                return Ok(());
            }
        }
        // set the unprocessed flag to false, as we are currently processing the latest outgoing
        // message.
        self.has_unprocessed_withdraw = false;

        log::debug!(
            "Per-session monitor has {} forks with {} uncommitted messages",
            self.forks.len(),
            self.forks
                .iter()
                .map(|mon| mon
                    .uncommitted_messages
                    .values()
                    .map(|x| x.len())
                    .sum::<usize>())
                .join("|")
        );

        let mut result = MonitoringResult::Ok(());

        // Pull out all old forks and replace the current set of forks by an empty vector.
        let old_forks = std::mem::take(&mut self.forks);

        for fork in old_forks {
            match fork.process_outgoing_ok(route.as_ref()) {
                Ok(new_forks) => {
                    // Extend the forks by all new forks that were found.
                    self.extend_forks(new_forks)?;
                }
                Err(e) => {
                    // Error occured. Do not push that fork to the list, but remember the error
                    result = Err(e);
                }
            }
        }

        // If there are no forks left, return the error. Otherwise, everything is still fine.
        if self.forks.is_empty() {
            assert!(result.is_err());
            result
        } else {
            Ok(())
        }
    }

    /// Check if there exists one monitor that is in a valid final state. If so, make make that
    /// monitor remain, and remove all others.
    fn final_check(&mut self) -> MonitoringResult<SinglePrefix> {
        self.assert_messages_processed_before(NotNan::new(f64::INFINITY).unwrap())
    }

    /// Apply all messages that have been observed before `time` and check that the last seen
    /// outgoing message matches the one that would be sent after applying those messages.
    ///
    /// This function will remove all the forks that do not comply with the assert. In case of an
    /// error, this structure is empty and must be recovered by the caller.
    fn assert_messages_processed_before(
        &mut self,
        time: NotNan<f64>,
    ) -> MonitoringResult<SinglePrefix> {
        assert!(!self.forks.is_empty());
        let num_forks = self.forks.len();

        // if there are unprocessed withdraw messages, we must process them now!
        if (!self.make_progress_on_withdraw) && self.has_unprocessed_withdraw {
            // However, we skip this step if doing a final check, i.e., if time is infinite.
            // In that case, we just change the current rib out to None.
            if time.is_finite() {
                log::debug!("Process withdraw before doing the check");
                // temporarily disable it
                self.make_progress_on_withdraw = true;
                let result = self.process_outgoing_ok(None);
                self.make_progress_on_withdraw = false;
                self.has_unprocessed_withdraw = false;
                // handle the result
                result?;
            }
        }

        // for each fork, we first call `process_uncommitted_messages_before`, and then advance the
        // monitor to observe the current route (as in before the applied messages).
        // Note, that this call might create (or remove) forks.
        let mut result = MonitoringResult::Ok(());

        // Pull out all old forks and replace the current set of forks by an empty vector.
        let old_forks = std::mem::take(&mut self.forks);

        for (i, mut fork) in old_forks.into_iter().enumerate() {
            log::debug!(
                "Assert messages processed before {time} on fork {}/{num_forks}",
                i + 1
            );
            fork.debug_log_state();

            // remember what we currently think the rib out should be. This should not change!
            let mut cur_rib_out = fork.bgp_process.rib_out;
            if self.has_unprocessed_withdraw {
                // in case we still have an unprocessed withdraw (which can only be in the final check),
                // we know that the last outgoing message was a withdraw. So instead, we just set
                // cur_rib_out to None.
                cur_rib_out = None;
            }

            // advance the state by processing mesasges.
            fork.apply_uncommitted_messages_before(time);

            // store the current rib after processing these messages.
            let expected_rib_out = fork.bgp_process.rib_out;

            // The call above might change the cur_rib_out. So now, we want to see if there are
            // still uncommitted messages that could lead us to selecting `cur_rib_out`, i.e.,
            // the one before everything happened.
            match fork.process_outgoing_ok(cur_rib_out.map(|rib| &rib.route)) {
                Ok(new_forks) => {
                    // These fork are fine. put it back onto the result.
                    self.extend_forks(new_forks)?;
                }
                // Error occured. Do not push that fork to the list, but remember the error
                Err(MonitoringError::UnexpectedEvent {
                    router,
                    session,
                    event,
                }) => {
                    // change the error type
                    result = Err(MonitoringError::MissingOutgoingEvent {
                        router,
                        neighbor: session,
                        current_event: event,
                        expected_event: expected_rib_out
                            .map(|x| BgpEvent::Update(x.route.clone()))
                            .unwrap_or(BgpEvent::Withdraw(SinglePrefix)),
                    });
                }
                Err(e) => {
                    result = Err(e);
                }
            }
        }

        // at this point, there might be a way that has_unprocessed_withdraw is still true:
        // - called on the final state, i.e., with infinite time
        // - it had an unprocessed withdraw before
        // In that case, we know that we applied all events, and thus, we can safely set this
        // flag back to false
        self.has_unprocessed_withdraw = false;

        // If there are no forks left, return the error. Otherwise, everything is still fine.
        if self.forks.is_empty() {
            assert!(result.is_err());
            log::info!("{result:?}");
            result
        } else {
            Ok(())
        }
    }

    /// Extend the forks with the new forks, but only if they are not subsumed by an existing one.
    fn extend_forks(
        &mut self,
        forks: Vec<MonitorPerSession<'a>>,
    ) -> MonitoringResult<SinglePrefix> {
        assert!(!forks.is_empty());
        'fork: for fork in forks {
            for parent in self.forks.iter() {
                if parent.subsumes(&fork) {
                    continue 'fork;
                }
            }
            self.forks.push(fork);
            // handle the killing of too many forks
            if self.forks.len() > KILL_AFTER_EXCEEDING_MORE_FORKS_THAN {
                let first = self.forks.get(0).unwrap();
                log::warn!(
                    "Fork limit exceeded with {} > {} forks.",
                    self.forks.len(),
                    KILL_AFTER_EXCEEDING_MORE_FORKS_THAN
                );
                return Err(MonitoringError::Killed {
                    router: first.router_id,
                    neighbor: first.neighbor,
                    prefix: SinglePrefix,
                    num_forks: self.forks.len(),
                });
            }
        }
        assert!(!self.forks.is_empty());
        Ok(())
    }

    fn update_enable_withdraws(
        &mut self,
        process_withdraws: ProcessWithdraws,
    ) -> Result<(), MonitoringError<SinglePrefix>> {
        // maybe enable or disable the withdraw processing
        let num_forks = self.forks.len();
        if self.make_progress_on_withdraw && process_withdraws.should_disable(num_forks) {
            log::info!("Disable processing withdraws, as the monitor has {num_forks} forks.");
            self.enable_withdraws(false)?;
        } else if !self.make_progress_on_withdraw && process_withdraws.should_enable(num_forks) {
            log::info!("Enable processing withdraws, as the monitor has {num_forks} forks.");
            self.enable_withdraws(true)?;
        }
        Ok(())
    }
}

impl<'a> MonitorPerSession<'a> {
    fn debug_log_state(&self) {
        if log::log_enabled!(log::Level::Trace) {
            log::debug!(
                "State on {:?} -- {:?} on self",
                self.router_id,
                self.neighbor,
            );
            log::trace!("current RIB-IN (processed)");
            for m in self.bgp_process.rib_in.values() {
                log::trace!("  {:?}", m.rib_in);
            }
            for (neighbor, msgs) in self.uncommitted_messages.iter() {
                if msgs.is_empty() {
                    continue;
                }
                log::trace!("uncommitted from {neighbor:?}");
                for msg in msgs {
                    log::trace!("  [{:02}] {:?}", msg.id, msg.event);
                    log::trace!("  -> rib: {:?}", msg.rib_entry);
                    log::trace!("  -> out: {:?}", msg.outgoing_route);
                }
            }
        }
    }

    /// As a message is incoming, we only add it to the set of uncommitted events.
    fn process_incoming(&mut self, from: RouterId, message: &'a UncommittedMessagePerSession) {
        self.uncommitted_messages
            .entry(from)
            .or_default()
            .push(message);
    }

    /// As a message is coming out of the router, I check what messages do need to be processed in
    /// order for this message to be observed.
    ///
    /// This function might return a set of new monitors; In this case the monitor forked itself as
    /// there are multiple possibilities to observe the given message.
    fn process_outgoing_ok(
        self,
        route: Option<&BgpRoute<SinglePrefix>>,
    ) -> Result<Vec<Self>, MonitoringError<SinglePrefix>> {
        // Do nothing if the outgoing event is the same as the current rib-out one.
        let current_rib_out = self.bgp_process.rib_out;
        if current_rib_out.map(|x| &x.route) == route {
            return Ok(vec![self]);
        }

        // Find out which event would have caused this here. Note, that it could either be the
        // result of one (or multiple) withdrawals, or one update, or a combination of the two.
        self.find_minimal_messages_to_observe(route)
    }

    /// Apply all events that have been observed before `time`.
    ///
    /// *Note*: You most likely want to get the current rib *before* calling this function, and
    /// then check (or advancce) the state to observe that message. Use process_outgoing_ok. We are
    /// doing that in `ForkingMonitorPerSession::assert_messages_processed_before` for how to call
    /// me.
    fn apply_uncommitted_messages_before(&mut self, time: NotNan<f64>) {
        // apply all remaining events
        let uncommitted = std::mem::take(&mut self.uncommitted_messages);
        let mut applied_events = Vec::new();
        for (neighbor, msgs) in uncommitted {
            for msg in msgs {
                if msg.time > time {
                    // put that message back onto the uncommitted messages
                    self.uncommitted_messages
                        .entry(neighbor)
                        .or_default()
                        .push(msg);
                    continue;
                }
                applied_events.push((neighbor, msg.event.clone()));
                log::trace!(
                    "executing event [{}] at {}: {:?}",
                    msg.id,
                    msg.time,
                    msg.event
                );
                self.bgp_process.commit(msg, neighbor);
            }
        }
    }

    /// This function tries to find the minimal set of messages that, when executed on the router,
    /// will generate the given BGP event.
    ///
    /// This function might fork if there are multiple ways to observe this route.
    fn find_minimal_messages_to_observe(
        self,
        route: Option<&BgpRoute<SinglePrefix>>,
    ) -> Result<Vec<Self>, MonitoringError<SinglePrefix>> {
        // find all ways in which this update can be observed
        let mut possibilities = Vec::<EventsToExecute>::new();
        let rib_in_processed = self
            .bgp_process
            .rib_in
            .values()
            .map(|x| x.rib_in)
            .collect::<Vec<_>>();

        self.debug_log_state();

        // There are three possibilities:
        // - In the current RIB, there already exists an entry that would send out the given
        //   `route`, but there are others in the RIB that are more preferred which must be
        //   withdrawn first.
        // - Withdraw all routes in the rib (only for withdraw)
        // - One (or more) updates (from the same neighbor) plus (potentially zero) withdraws
        //   from others.

        // Possibility one: current RIB already contains the route, we just need to withdraw
        // those that are higher preferred.
        for msg in self.bgp_process.rib_in.values() {
            let rib_in = msg.rib_in;
            let rib_out = msg.rib_out;
            if rib_out.as_ref().map(|x| &x.route) != route {
                // this entry currently in the rib does not result in this outgoing message. skip!
                continue;
            }
            log::debug!("Step 1: Trying to select {rib_in:?}");
            log::debug!("  -> so we observe {rib_out:?}");
            // this is a possibility. We mus therefore withdraw all others that are more preferred.
            if let Some(possibility) =
                self.construct_events_by_updating_better_to_worse(&rib_in_processed, rib_in)
            {
                log::debug!(
                    "P{}: Event can be observed when withdrawing better routes than from {:?}",
                    possibilities.len(),
                    rib_in.from_id
                );
                possibilities.push(possibility);
            }
        }

        // Possibility two: all routes are withdrawn (for withdraw only)
        if route.is_none() {
            if let Some(possibility) = self.construct_events_by_withdrawing_all(&rib_in_processed) {
                log::debug!(
                    "P{}: Event can be observed when withdrawing routes in the processed rib-in.",
                    possibilities.len(),
                );
                possibilities.push(possibility);
            }
        }

        // Possibility three: multiple update messages (plus some withdraws)
        for (from_neighbor, uncommitted_messages) in self.uncommitted_messages.iter() {
            'uncommitted: for (position, msg) in uncommitted_messages.iter().enumerate() {
                // If it is an effective withdraw, it should be handled in step 2 (withdraw all)!
                if msg.is_effective_withdraw() {
                    continue 'uncommitted; // go ot the next message
                }
                // We only consider those that actually produce the outgoing event that we observed
                if msg.outgoing_route.as_ref().map(|x| &x.route) != route {
                    continue 'uncommitted;
                }
                let target_rib = msg
                    .rib_entry
                    .as_ref()
                    .expect("Filtering for effective withdraw above");
                // We want to be able to select this route by updating other and better routes to wrose routes.
                let Some(mut possibility) = self
                    .construct_events_by_updating_better_to_worse(&rib_in_processed, target_rib)
                else {
                    continue 'uncommitted;
                };
                // found the first message that would be selectable and produce the same outcome.
                log::debug!("P{}: Event can be observed when receiving (and selecting) a route from {from_neighbor:?}", possibilities.len());
                // also, push those of this neighbor up to the position.
                possibility.insert(*from_neighbor, position);
                possibilities.push(possibility);
                // no need to search for later events that do the same.
                // break 'uncommitted;
            }
        }

        // if there are no possibilities, we are not expecting this event.
        if possibilities.len() == 0 {
            log::debug!("Nothing can explain the observed event; removing that fork.");
            Err(MonitoringError::UnexpectedEvent {
                router: self.router_id,
                session: self.neighbor,
                event: match route {
                    Some(route) => BgpEvent::Update(route.clone()),
                    None => BgpEvent::Withdraw(SinglePrefix),
                },
            })?
        } else {
            Ok(self.apply_all_possibilities(possibilities, route))
        }
    }

    /// Try to apply all possibilities (creating forks in the process). Any possibility that could
    /// not be applied because it results in an invalid RIB will not be returned.
    fn apply_all_possibilities(
        mut self,
        mut possibilities: Vec<EventsToExecute>,
        want_rib_out: Option<&BgpRoute<SinglePrefix>>,
    ) -> Vec<Self> {
        // if there is only one possibility, we don't clone, but we modify self.
        if possibilities.len() == 1 {
            log::debug!("Applying P0:");
            self.apply_single_possibility(possibilities.pop().unwrap(), want_rib_out);
            return vec![self];
        }

        let mut new_forks = Vec::new();
        for (i, events_to_execute) in possibilities.into_iter().enumerate() {
            log::debug!("Applying P{i}:");
            let mut fork = self.clone();
            fork.apply_single_possibility(events_to_execute, want_rib_out);
            new_forks.push(fork)
        }
        new_forks
    }

    /// Apply the events to execute. This function checks and asserts that the resulting rib_out is
    /// the one we expect. If not, this function panics. It indicates that there is a logic error in
    /// the monitor.
    fn apply_single_possibility(
        &mut self,
        events_to_execute: EventsToExecute,
        want_rib_out: Option<&BgpRoute<SinglePrefix>>,
    ) {
        // record all events that have been processed
        let mut batch = Vec::new();
        for (neighbor, position) in events_to_execute {
            let msgs = self
                .uncommitted_messages
                .get_mut(&neighbor)
                .expect("Message must be in the set")
                .take_start(position);
            // apply all these messages on the router
            // safety: This router is in isolation
            for msg in msgs {
                log::trace!(
                    "    committing [{:02}] {:?} -> {:?}: {:?}",
                    msg.id,
                    neighbor,
                    self.router_id,
                    msg.event
                );
                batch.push(msg.id);
                self.bgp_process.commit(msg, neighbor);
            }
        }
        self.commit_history.push(batch);

        assert_eq!(
            self.bgp_process.rib_out.map(|x| &x.route),
            want_rib_out,
            "Predicted RIB out does not match the simulated one. This is likely a bug in the monitor"
        );
    }

    /// Construct the minimal set of events such all routes in the rib that are better than
    /// target_rib are updated to be now worse than target_rib
    fn construct_events_by_updating_better_to_worse(
        &self,
        rib_in_processed: &[&'a BgpRibEntry<SinglePrefix>],
        target_rib: &BgpRibEntry<SinglePrefix>,
    ) -> Option<EventsToExecute> {
        let mut events = EventsToExecute::new();
        let neighbors_that_are_better = rib_in_processed
            .iter()
            .filter(|rib| rib.from_id != target_rib.from_id && **rib > target_rib)
            .map(|rib| rib.from_id);
        for neighbor in neighbors_that_are_better {
            let position = self
                .uncommitted_messages
                .get(&neighbor)
                .into_iter()
                .flatten()
                // Either, the `msg.rib` is None (thus, we can withdraw it completely) or `msg.rib` is worse than `target_rib`.
                .find_position(|msg| {
                    msg.rib_entry
                        .as_ref()
                        .map(|rib| rib < target_rib)
                        .unwrap_or(true)
                })
                .map(|(pos, _)| pos)?;
            events.insert(neighbor, position);
        }
        Some(events)
    }

    /// Construct the minimal set of events such that all neighbors (effectively) withdraw a route.
    fn construct_events_by_withdrawing_all(
        &self,
        rib_in_processed: &[&'a BgpRibEntry<SinglePrefix>],
    ) -> Option<EventsToExecute> {
        let mut events = EventsToExecute::new();
        let neighbors_from_which_to_withdraw = rib_in_processed.iter().map(|x| x.from_id);
        for neighbor in neighbors_from_which_to_withdraw {
            let position = self
                .uncommitted_messages
                .get(&neighbor)
                .into_iter()
                .flatten()
                .find_position(|msg| msg.is_effective_withdraw())
                .map(|(pos, _)| pos)?;
            events.insert(neighbor, position);
        }
        Some(events)
    }

    /// `self` subsumes `other` if `self` contains all (and more) uncommitted messages as `other`.
    fn subsumes(&self, other: &Self) -> bool {
        for (neighbor, other_msgs) in other.uncommitted_messages.iter() {
            let other_msg_count = other_msgs.len();
            let self_msg_count = self
                .uncommitted_messages
                .get(neighbor)
                .map(|x| x.len())
                .unwrap_or(0);
            // self must have at least as many uncommitted messages from that neighbor as other
            // does. If not, self does not subsume other.
            if other_msg_count > self_msg_count {
                return false;
            }
        }
        // if we reach this point, then all the checks pass and self subsumes other.
        true
    }
}

impl<'a> RibPerSession<'a> {
    fn new(bgp_process: &BgpProcess<SinglePrefix>, neighbor: RouterId, arena: &'a Arena) -> Self {
        let mut s = Self::default();

        for rib in bgp_process.rib_in_processed() {
            let from = rib.from_id;
            let rib_in = arena.alloc_rib(rib);
            let rib_out = bgp_process
                .process_route_from_rib_to_rib_out(rib_in, neighbor)
                .map(|x| arena.alloc_rib(x));
            s.rib_in
                .insert(from, CommittedMessagePerSession { rib_in, rib_out });
        }

        // run the decision process
        s.run_decision_process();

        s
    }

    fn commit(&mut self, msg: &'a UncommittedMessagePerSession, from: RouterId) {
        match msg.committed() {
            Some(msg) => self.rib_in.insert(from, msg),
            None => self.rib_in.remove(&from),
        };

        self.run_decision_process()
    }

    fn run_decision_process(&mut self) {
        // TODO: this is to replicate the bug from bgpsim. In the newest version of BGPsim, that bug is fixed.
        match self.rib_in.values().max_by_key(|x| x.rib_in) {
            Some(m) => {
                self.rib = Some(m.rib_in);
                self.rib_out = m.rib_out;
            }
            None => {
                self.rib = None;
                self.rib_out = None;
            }
        }
    }
}

impl UncommittedMessage {
    /// Create an uncommitted message that represents a withdraw, at the given time.
    fn withdraw(id: usize, time: NotNan<f64>) -> Self {
        Self {
            id,
            time,
            event: BgpEvent::Withdraw(SinglePrefix),
            rib_entry: None,
            outgoing_routes: Default::default(),
        }
    }

    fn into_per_session<'a>(
        &mut self,
        neighbor: RouterId,
        arena: &'a Arena,
    ) -> &'a UncommittedMessagePerSession {
        arena.alloc_msg(UncommittedMessagePerSession {
            id: self.id,
            time: self.time,
            event: self.event.clone(),
            rib_entry: self.rib_entry.clone(),
            outgoing_route: self.outgoing_routes.remove(&neighbor),
        })
    }
}

impl UncommittedMessagePerSession {
    /// Returns true if the event is a withdraw, or if the incoming message was rejected by a route
    /// map.
    fn is_effective_withdraw(&self) -> bool {
        self.rib_entry.is_none()
    }

    fn committed<'a>(&'a self) -> Option<CommittedMessagePerSession<'a>> {
        Some(CommittedMessagePerSession {
            rib_in: self.rib_entry.as_ref()?,
            rib_out: self.outgoing_route.as_ref(),
        })
    }
}

// There are two possible ways to create a ReorderingMonitor, either from a SinglePrefix Router or from a
// SimplePrefix router and the corresponding prefix plane we want to isolate
impl<'a> ReorderingMonitor<'a> {
    pub fn new(router: Router<SinglePrefix>, arena: &'a Arena) -> Self {
        // clear all state in the router by withdrawing all routes.
        let mut empty_router = router.clone();
        let r = router.router_id();
        let mut per_session = HashMap::new();
        for neighbor in router.bgp.get_sessions().keys().copied() {
            // Safety: router is not running inside a network.
            unsafe {
                empty_router
                    .trigger_event(Event::Bgp {
                        p: (),
                        src: neighbor,
                        dst: r,
                        e: BgpEvent::Withdraw(SinglePrefix),
                    })
                    .unwrap();
            }

            let bgp_process = RibPerSession::new(&router.bgp, neighbor, arena);

            // populate the per-session monitor.
            per_session.insert(
                neighbor,
                ForkingMonitorPerSession::new(router.router_id(), neighbor, bgp_process, true),
            );
        }

        Self {
            arena,
            empty_router,
            failure_recovery_router: router.clone(),
            per_session,
            incoming_counter: 0,
            process_withdraws: ProcessWithdraws::Always,
        }
    }

    pub fn new_for_prefix(
        router: Router<SimplePrefix>,
        prefix: &SimplePrefix,
        arena: &'a Arena,
    ) -> Self {
        let router = router.to_single_prefix(prefix);
        Self::new(router, arena)
    }
}

/// How many events from each neighbor must be processed to observe this outcome.
type EventsToExecute = HashMap<RouterId, usize>;

/// Extension trait for convenient access to the corresponding rib entries.
trait RouterExt {
    fn rib_in_processed(&self) -> Vec<BgpRibEntry<SinglePrefix>>;
    fn process_route_from_rib_to_rib_out(
        &self,
        r: &BgpRibEntry<SinglePrefix>,
        peer: RouterId,
    ) -> Option<BgpRibEntry<SinglePrefix>>;
}

impl RouterExt for BgpProcess<SinglePrefix> {
    fn rib_in_processed(&self) -> Vec<BgpRibEntry<SinglePrefix>> {
        self.get_processed_rib_in()
            .0
            .into_iter()
            .flatten()
            .map(|(rib, _)| rib)
            .collect()
    }

    fn process_route_from_rib_to_rib_out(
        &self,
        r: &BgpRibEntry<SinglePrefix>,
        peer: RouterId,
    ) -> Option<BgpRibEntry<SinglePrefix>> {
        let peer_type = self.get_sessions().get(&peer)?;
        if !self.should_export_route(r.from_id, r.from_type, peer, *peer_type) {
            return None;
        }
        self.process_rib_out_route(r.clone(), peer)
            .expect("Error whild processing route-maps")
    }
}

impl RouterExt for Router<SinglePrefix> {
    fn rib_in_processed(&self) -> Vec<BgpRibEntry<SinglePrefix>> {
        self.bgp.rib_in_processed()
    }

    fn process_route_from_rib_to_rib_out(
        &self,
        r: &BgpRibEntry<SinglePrefix>,
        peer: RouterId,
    ) -> Option<BgpRibEntry<SinglePrefix>> {
        self.bgp.process_route_from_rib_to_rib_out(r, peer)
    }
}

trait VecExt {
    fn take_start(&mut self, up_to: usize) -> Self;
}

impl<T> VecExt for Vec<T> {
    #[track_caller]
    fn take_start(&mut self, up_to: usize) -> Self {
        assert!(up_to <= self.len(), "Message must be in the set.");
        let mut rest: Vec<T> = self.split_off(up_to + 1);
        std::mem::swap(&mut rest, self);
        rest
    }
}

impl<'a> ReorderingMonitor<'a> {
    pub fn heap_size(&self) -> usize {
        self.arena.heap_size()
            + hash_map_heap_size(&self.per_session)
            + self
                .per_session
                .values()
                .map(|x| x.heap_size())
                .sum::<usize>()
    }
}

impl<'a> ForkingMonitorPerSession<'a> {
    fn heap_size(&self) -> usize {
        vec_heap_size(&self.forks) + self.forks.iter().map(|x| x.heap_size()).sum::<usize>()
    }
}

impl<'a> MonitorPerSession<'a> {
    fn heap_size(&self) -> usize {
        self.bgp_process.heap_size()
            + hash_map_heap_size(&self.uncommitted_messages)
            + vec_heap_size(&self.commit_history)
            + self.commit_history.iter().map(vec_heap_size).sum::<usize>()
    }
}

impl<'a> RibPerSession<'a> {
    fn heap_size(&self) -> usize {
        hash_map_heap_size(&self.rib_in)
    }
}

fn vec_heap_size<T>(vec: &Vec<T>) -> usize {
    vec.capacity() * std::mem::size_of::<T>()
}

fn hash_map_heap_size<K, V>(map: &HashMap<K, V>) -> usize {
    map.capacity() * (std::mem::size_of::<K>() + std::mem::size_of::<V>())
}

impl Arena {
    fn heap_size(&self) -> usize {
        self.rib.len() * std::mem::size_of::<BgpRibEntry<SinglePrefix>>()
            + self.msg.len() * std::mem::size_of::<UncommittedMessagePerSession>()
    }

    fn alloc_rib<'a>(&'a self, rib: BgpRibEntry<SinglePrefix>) -> &'a BgpRibEntry<SinglePrefix> {
        self.rib.alloc(rib)
    }

    fn alloc_msg<'a>(
        &'a self,
        msg: UncommittedMessagePerSession,
    ) -> &'a UncommittedMessagePerSession {
        self.msg.alloc(msg)
    }
}

// Momentarily suppressed monitoring tests
#[cfg(test)]
pub(crate) mod test_monitoring {
    use std::fmt::Write;

    use super::ReorderingMonitor;
    use bgpsim::{
        bgp::BgpEvent,
        event::Event,
        prelude::*,
        route_map::{RouteMapBuilder, RouteMapDirection::Outgoing},
    };
    use itertools::Itertools;
    use ordered_float::NotNan;

    #[test]
    fn single_reply_all() {
        sequence(
            [
                (I(IA(true)), 0.0, None, (1, 1, 1)),
                (I(IB(true)), 1.0, None, (1, 1, 1)),
                (I(IC(true)), 2.0, None, (1, 1, 1)),
                (I(IC(false)), 3.0, None, (1, 1, 1)),
                (I(IB(false)), 4.0, None, (1, 1, 1)),
                (O(OA(Some(a()))), 5.0, None, (1, 1, 1)),
                (O(OA(Some(b()))), 6.0, None, (1, 1, 1)),
                (O(OA(Some(c()))), 7.0, None, (1, 1, 1)),
                (O(OA(Some(b()))), 8.0, None, (1, 1, 1)),
                (O(OA(Some(a()))), 9.0, None, (1, 1, 1)),
                // make the final state valid for neighbors b and c
                (O(OB(Some(a()))), 10.0, None, (1, 1, 1)),
                (O(OC(Some(a()))), 10.0, None, (1, 1, 1)),
            ],
            test_network(),
            true,
            true,
            true,
            true,
        );
    }

    #[test]
    fn single_reply_last() {
        sequence(
            [
                (I(IA(true)), 0.0, None, (1, 1, 1)),
                (I(IB(true)), 1.0, None, (1, 1, 1)),
                (I(IC(true)), 2.0, None, (1, 1, 1)),
                (I(IC(false)), 3.0, None, (1, 1, 1)),
                (I(IB(false)), 4.0, None, (1, 1, 1)),
                (O(OA(Some(a()))), 5.0, None, (1, 1, 1)),
                // make the final state valid for neighbors b and c
                (O(OB(Some(a()))), 10.0, None, (1, 1, 1)),
                (O(OC(Some(a()))), 10.0, None, (1, 1, 1)),
            ],
            test_network(),
            true,
            true,
            true,
            true,
        );
    }

    #[test]
    fn two_reply_inconsistent_order() {
        sequence(
            [
                (I(IA(true)), 0.0, None, (1, 1, 1)),
                (I(IB(true)), 1.0, None, (1, 1, 1)),
                (I(IC(true)), 2.0, None, (1, 1, 1)),
                (I(IC(false)), 3.0, None, (1, 1, 1)),
                (I(IB(false)), 4.0, None, (1, 1, 1)),
                (O(OA(Some(a()))), 5.0, None, (1, 1, 1)),
                (O(OB(Some(b()))), 5.0, None, (1, 1, 1)),
                (O(OA(Some(b()))), 6.0, None, (1, 1, 1)),
                (O(OB(None)), 6.0, None, (1, 1, 1)),
                (O(OA(Some(c()))), 7.0, None, (1, 1, 1)),
                (O(OB(Some(c()))), 7.0, None, (1, 1, 1)),
                (O(OA(Some(b()))), 8.0, None, (1, 1, 1)),
                (O(OB(None)), 8.0, None, (1, 1, 1)),
                (O(OA(Some(a()))), 9.0, None, (1, 1, 1)),
                (O(OB(Some(a()))), 9.0, None, (1, 1, 1)),
                // make the final state valid for neighbor c
                (O(OC(Some(a()))), 10.0, None, (1, 1, 1)),
            ],
            test_network(),
            true,
            true,
            true,
            false,
        );
    }

    #[test]
    fn invalid_message() {
        sequence(
            [
                (I(IA(true)), 0.0, None, (1, 1, 1)),
                (I(IB(true)), 1.0, None, (1, 1, 1)),
                (I(IC(true)), 2.0, None, (1, 1, 1)),
                (I(IC(false)), 3.0, None, (1, 1, 1)),
                (I(IB(false)), 4.0, None, (1, 1, 1)),
                (O(OA(Some(c()))), 5.0, None, (1, 1, 1)),
                (O(OA(Some(b()))), 6.0, None, (1, 1, 1)),
                (O(OA(Some(c()))), 7.0, None, (1, 1, 1)),
            ],
            test_network(),
            false,
            true,
            true,
            true,
        );
    }

    #[test]
    fn invalid_final_state() {
        sequence(
            [
                (I(IA(true)), 0.0, None, (1, 1, 1)),
                (I(IB(true)), 1.0, None, (1, 1, 1)),
                (I(IC(true)), 2.0, None, (1, 1, 1)),
                (I(IC(false)), 3.0, None, (1, 1, 1)),
                (I(IB(false)), 4.0, None, (1, 1, 1)),
                (O(OA(Some(a()))), 5.0, None, (1, 1, 1)),
                (O(OA(Some(b()))), 6.0, None, (1, 1, 1)),
                (O(OA(Some(c()))), 7.0, None, (1, 1, 1)),
                (O(OA(Some(b()))), 8.0, None, (1, 1, 1)),
                // no valid final state for a, but valid for b and c.
                (O(OB(Some(a()))), 10.0, None, (1, 1, 1)),
                (O(OC(Some(a()))), 10.0, None, (1, 1, 1)),
            ],
            test_network(),
            true,
            true,
            false,
            true,
        );
    }

    /// The monitor is reused across recordings (the testbed runs several sequences against the
    /// same controller). A failing `final_check` leaves every per-session monitor without forks,
    /// so it must recover them, exactly like `process_outgoing_ok` and
    /// `assert_messages_processed_before` do. Otherwise the next call panics.
    #[test]
    fn reuse_after_failed_final_check() {
        let net = test_network();
        let arena = Default::default();
        let mut mon = test_monitor(&net, &arena);

        // Same scenario as `invalid_final_state`: no valid final state for a.
        for (msg, time) in [
            (I(IA(true)), 0.0),
            (I(IB(true)), 1.0),
            (I(IC(true)), 2.0),
            (I(IC(false)), 3.0),
            (I(IB(false)), 4.0),
            (O(OA(Some(a()))), 5.0),
            (O(OA(Some(b()))), 6.0),
            (O(OA(Some(c()))), 7.0),
            (O(OA(Some(b()))), 8.0),
            (O(OB(Some(a()))), 10.0),
            (O(OC(Some(a()))), 10.0),
        ] {
            mon.process_message(msg.message(NotNan::new(time).unwrap()))
                .unwrap();
        }

        // The final check reports the error...
        mon.final_check().unwrap_err();

        // ...and every per-session monitor must have been recovered with a single fork.
        for (neighbor, per_session) in mon.per_session.iter() {
            assert!(
                !per_session.forks.is_empty(),
                "session to {} was left without forks after a failed final check",
                neighbor.fmt(&net)
            );
        }

        // Therefore the monitor stays usable for the next recording instead of panicking.
        mon.assert_messages_processed_before(NotNan::new(11.0).unwrap())
            .unwrap();
        mon.final_check().unwrap();
    }

    #[test]
    fn forking() {
        let mut net = test_network();
        // add route-maps to deny routes from c to all neighbors (outgoing)
        let rm = RouteMapBuilder::new()
            .order(10)
            .deny()
            .match_as_path_contains(3.into())
            .build();
        for n in [a(), b(), c()] {
            net.set_bgp_route_map(r(), n, Outgoing, rm.clone()).unwrap();
        }

        sequence(
            [
                (I(IA(true)), 0.0, None, (1, 1, 1)),
                (I(IA(false)), 0.1, None, (1, 1, 1)),
                (I(IA(true)), 0.2, None, (1, 1, 1)),
                (I(IB(true)), 1.0, None, (1, 1, 1)),
                (I(IB(false)), 1.1, None, (1, 1, 1)),
                (I(IC(true)), 2.0, None, (1, 1, 1)),
                (I(IC(false)), 2.1, None, (1, 1, 1)),
                // make r-->a observe +a [a] +c [0] +b -c [b] -b [a] -a [0] +a [a]
                (O(OA(Some(a()))), 3.0, None, (1, 1, 1)),
                (O(OA(None)), 3.1, None, (2, 1, 1)),
                (O(OA(Some(b()))), 3.2, None, (2, 1, 1)),
                (O(OA(Some(a()))), 3.3, None, (2, 1, 1)),
                (O(OA(None)), 3.4, None, (2, 1, 1)),
                (O(OA(Some(a()))), 3.5, None, (1, 1, 1)),
                // make r-->b observe +a [a] -a [0] +a [a] +b [b] +c [0] -b -c [a]
                (O(OB(Some(a()))), 4.0, None, (1, 1, 1)),
                (O(OB(None)), 4.1, None, (1, 2, 1)),
                (O(OB(Some(a()))), 4.2, None, (1, 2, 1)),
                (O(OB(Some(b()))), 4.3, None, (1, 2, 1)),
                (O(OB(None)), 4.4, None, (1, 2, 1)),
                (O(OB(Some(a()))), 4.5, None, (1, 1, 1)),
                // valid final state for b and c
                (O(OC(Some(a()))), 10.0, None, (1, 1, 1)),
            ],
            net,
            true,
            true,
            true,
            false,
        );
    }

    #[test]
    fn time_dependent() {
        sequence(
            [
                (I(IA(true)), 0.0, None, (1, 1, 1)),
                (I(IB(true)), 0.0, None, (1, 1, 1)),
                (I(IC(true)), 0.0, None, (1, 1, 1)),
                (I(IC(false)), 1.0, None, (1, 1, 1)),
                (I(IB(false)), 1.0, None, (1, 1, 1)),
                (O(OA(Some(c()))), 2.0, None, (1, 1, 1)),
                (O(OA(None)), 2.0, Some(0.5), (1, 1, 1)),
            ],
            test_network(),
            true,
            false,
            true,
            true,
        );
    }

    #[track_caller]
    fn sequence<const N: usize>(
        seq: [(Message, f64, Option<f64>, (usize, usize, usize)); N],
        net: Network<SinglePrefix, BasicEventQueue<SinglePrefix>>,
        last_msg_correct: bool,
        last_timeout_correct: bool,
        final_state_correct: bool,
        is_serializable: bool,
    ) {
        let arena = Default::default();
        let timeout_triggers = seq
            .iter()
            .map(|(_, _, t, _)| t.map(|x| NotNan::new(x).unwrap()))
            .collect::<Vec<_>>();
        let num_forks = seq.iter().map(|(_, _, _, f)| *f).collect::<Vec<_>>();
        let events = seq.map(|(m, time, _, _)| m.message(NotNan::new(time).unwrap()));
        let mut mon = test_monitor(&net, &arena);
        for (i, ((event, timeout_trigger), (a_forks, b_forks, c_forks))) in events
            .into_iter()
            .zip(timeout_triggers)
            .zip(num_forks)
            .enumerate()
        {
            println!("Message {i}: {}", event.fmt(&net));
            let last = i + 1 == N;

            // process the message and check the result
            let process_result = mon.process_message(event);
            if last && !last_msg_correct {
                process_result.unwrap_err();
                return;
            } else {
                process_result.unwrap();
            }

            // check the number of forks
            let mon_a = &mon.per_session[&a()];
            let mon_b = &mon.per_session[&b()];
            let mon_c = &mon.per_session[&c()];
            assert_eq!(mon_a.forks.len(), a_forks, "{}", fork_info(mon_a, &net));
            assert_eq!(mon_b.forks.len(), b_forks, "{}", fork_info(mon_b, &net));
            assert_eq!(mon_c.forks.len(), c_forks, "{}", fork_info(mon_c, &net));

            // check for timeout
            if let Some(time) = timeout_trigger {
                let timeout_result = mon.assert_messages_processed_before(time);
                if last && !last_timeout_correct {
                    timeout_result.unwrap_err();
                    return;
                } else {
                    timeout_result.unwrap();
                }
            }
        }
        let final_result = mon.final_check();
        if final_state_correct {
            final_result.unwrap();
        } else {
            final_result.unwrap_err();
            return;
        }

        let serializable_result = mon.check_incoming_serializability();
        if is_serializable {
            serializable_result.unwrap();
        } else {
            serializable_result.unwrap_err();
        }
    }

    fn fork_info(
        forking_mon: &super::ForkingMonitorPerSession,
        net: &Network<SinglePrefix, BasicEventQueue<SinglePrefix>>,
    ) -> String {
        let mut s = String::from("Forking Monitor {\n");
        for mon in forking_mon.forks.iter() {
            write!(&mut s, "  {{").unwrap();
            for (i, (neighbor, msgs)) in mon
                .uncommitted_messages
                .iter()
                .filter(|(_, msgs)| !msgs.is_empty())
                .sorted_by_key(|(neighbor, _)| *neighbor)
                .enumerate()
            {
                if i > 0 {
                    write!(&mut s, ", ").unwrap();
                }
                write!(&mut s, "{} from {}", msgs.len(), neighbor.fmt(net)).unwrap();
            }
            writeln!(&mut s, " }}").unwrap();
        }
        write!(&mut s, "}}").unwrap();
        s
    }

    fn test_network() -> Network<SinglePrefix, BasicEventQueue<SinglePrefix>> {
        net! {
            sessions = {
                r -> a!(1);
                r -> b!(2);
                r -> c!(3);
            };
        }
    }

    fn test_monitor<'a, Q>(
        net: &Network<SinglePrefix, Q>,
        arena: &'a super::Arena,
    ) -> ReorderingMonitor<'a> {
        ReorderingMonitor::new(net.get_internal_router(r()).unwrap().clone(), arena)
    }

    fn r() -> RouterId {
        RouterId::new(0)
    }

    fn a() -> RouterId {
        RouterId::new(1)
    }

    fn b() -> RouterId {
        RouterId::new(2)
    }

    fn c() -> RouterId {
        RouterId::new(3)
    }

    // Internal enums needed for convenience.
    #[derive(Debug, Clone, Copy)]
    enum Message {
        I(InId),
        O(OutId),
    }
    use Message::*;
    impl Message {
        fn message(&self, time: NotNan<f64>) -> Event<SinglePrefix, NotNan<f64>> {
            match self {
                I(in_id) => in_id.message(time),
                O(out_id) => out_id.message(time),
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum InId {
        IA(bool),
        IB(bool),
        IC(bool),
    }
    use InId::*;

    impl InId {
        fn bgp_event(&self) -> BgpEvent<SinglePrefix> {
            match self {
                IA(true) => BgpEvent::Update(BgpRoute {
                    prefix: SinglePrefix,
                    as_path: vec![1.into(), 1.into(), 1.into()],
                    next_hop: a(),
                    local_pref: None,
                    med: None,
                    community: Default::default(),
                    originator_id: Default::default(),
                    cluster_list: Default::default(),
                }),
                IA(false) => BgpEvent::Withdraw(SinglePrefix),
                IB(true) => BgpEvent::Update(BgpRoute {
                    prefix: SinglePrefix,
                    as_path: vec![2.into(), 2.into()],
                    next_hop: b(),
                    local_pref: None,
                    med: None,
                    community: Default::default(),
                    originator_id: Default::default(),
                    cluster_list: Default::default(),
                }),
                IB(false) => BgpEvent::Withdraw(SinglePrefix),
                IC(true) => BgpEvent::Update(BgpRoute {
                    prefix: SinglePrefix,
                    as_path: vec![3.into()],
                    next_hop: c(),
                    local_pref: None,
                    med: None,
                    community: Default::default(),
                    originator_id: Default::default(),
                    cluster_list: Default::default(),
                }),
                IC(false) => BgpEvent::Withdraw(SinglePrefix),
            }
        }

        fn message(&self, time: NotNan<f64>) -> Event<SinglePrefix, NotNan<f64>> {
            let e = self.bgp_event();
            match self {
                IA(_) => Event::Bgp {
                    p: time,
                    src: a(),
                    dst: r(),
                    e,
                },
                IB(_) => Event::Bgp {
                    p: time,
                    src: b(),
                    dst: r(),
                    e,
                },
                IC(_) => Event::Bgp {
                    p: time,
                    src: c(),
                    dst: r(),
                    e,
                },
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum OutId {
        OA(Option<RouterId>),
        OB(Option<RouterId>),
        OC(Option<RouterId>),
    }
    use OutId::*;

    impl OutId {
        fn message(&self, time: NotNan<f64>) -> Event<SinglePrefix, NotNan<f64>> {
            let mut e = self.bgp_event();
            match &mut e {
                BgpEvent::Withdraw(_) => {}
                BgpEvent::Update(route) => {
                    route.as_path.insert(0, 10.into());
                    route.next_hop = r();
                }
            }
            match self {
                OA(_) => Event::Bgp {
                    p: time,
                    src: r(),
                    dst: a(),
                    e,
                },
                OB(_) => Event::Bgp {
                    p: time,
                    src: r(),
                    dst: b(),
                    e,
                },
                OC(_) => Event::Bgp {
                    p: time,
                    src: r(),
                    dst: c(),
                    e,
                },
            }
        }

        fn bgp_event(&self) -> BgpEvent<SinglePrefix> {
            match match self {
                OA(i) | OB(i) | OC(i) => *i,
            } {
                None => BgpEvent::Withdraw(SinglePrefix),
                Some(x) => {
                    if x == a() {
                        IA(true).bgp_event()
                    } else if x == b() {
                        IB(true).bgp_event()
                    } else {
                        IC(true).bgp_event()
                    }
                }
            }
        }
    }
}
