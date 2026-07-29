use std::collections::{HashMap, HashSet};

use bgpsim::types::{AsId, RouterId};
use itertools::Itertools;
use rand::{seq::SliceRandom, Rng};

use super::{
    simulation::{ExternalEvent, SimulationEvent, SimulationTimeStep},
    StdRng, P,
};

/// Generates sequences of events for all routers.
pub struct UniformEventGenerator {
    rng: StdRng,
    prefixes: Vec<P>,
    routers: HashMap<RouterId, AsId>,
    /// Keep track of which routers are advertising which prefixes
    advertised_prefixes: HashSet<(RouterId, P)>,
    /// Whether a reconfiguration event is still to be emitted. Set to `false` up front for
    /// experiments that do not reconfigure anything at runtime, so that none is ever produced.
    reconfig: bool,
}

impl UniformEventGenerator {
    /// Create a generator for an experiment.
    ///
    /// `with_reconfiguration` controls whether a single [`SimulationEvent::Reconfiguration`] is
    /// emitted somewhere during the run. Only the prefix-list scenario reconfigures the network
    /// while it runs; the other scenarios set their bug up before the run starts, and emitting a
    /// reconfiguration event for them would both trigger a reconfiguration they never asked for and
    /// record an event that did not happen into their trace.
    pub fn new(
        rng: StdRng,
        external_routers: HashMap<RouterId, AsId>,
        prefixes: Vec<P>,
        with_reconfiguration: bool,
    ) -> Self {
        UniformEventGenerator {
            rng,
            prefixes,
            routers: external_routers,
            advertised_prefixes: HashSet::new(),
            reconfig: with_reconfiguration,
        }
    }

    /// For a key to the set of advertised prefixes, generate the corresponding event and update the
    /// internal tracking of those advertisements
    fn get_external_event(&mut self, (external_neighbor, prefix): (RouterId, P)) -> ExternalEvent {
        if self
            .advertised_prefixes
            .remove(&(external_neighbor, prefix))
        {
            // The value was present and has been removed, signal the route to be withdrawn
            ExternalEvent {
                external_neighbor,
                prefix,
                route: None,
            }
        } else {
            // The value was not present, track the advertisement and signal the update
            self.advertised_prefixes.insert((external_neighbor, prefix));
            ExternalEvent {
                external_neighbor,
                prefix,
                route: Some(vec![self.routers[&external_neighbor]]),
            }
        }
    }
}

impl Iterator for UniformEventGenerator {
    type Item = SimulationTimeStep;

    fn next(&mut self) -> Option<Self::Item> {
        // Pick, at random, which routers and prefixes will be involved during this simulation step
        let mut candidates = Vec::new();

        // Generate a reconfiguration event
        if self.reconfig && self.rng.gen_bool(0.005) {
            // Only one per run
            self.reconfig = false;
            return Some(vec![SimulationEvent::Reconfiguration]);
        }

        for router in self.routers.keys().sorted() {
            if self.rng.gen_bool(0.5) {
                // If we selected this router, assign it random prefixes
                let prefix = *self
                    .prefixes
                    .choose(&mut self.rng)
                    .expect("Prefix list should not be empty");

                candidates.push((*router, prefix));
            }
        }

        // Map those candidates to actual network operations
        let external_events = candidates
            .into_iter()
            .map(|candidate| SimulationEvent::External(self.get_external_event(candidate)))
            .collect();
        Some(external_events)
    }
}

// /// Generates sequences of events for all routers. Every time next is called there is a probability that the event
// /// will be a reconfiguration event. *Only one* is ever issued *per* generator
// pub struct UniformReconfigurationEventGenerator<G: Iterator<Item = SimulationTimeStep>> {
//     rng: StdRng,
//     reconfiguration_prob: Vec<P>,
//     /// Internal generator
//     generator: G,
// }

// impl UniformReconfigurationEventGenerator<G>

#[cfg(test)]
mod test_generator {
    use super::*;
    use bgpsim::types::{AsId, Prefix as PrefixTrait, RouterId, SimplePrefix as Prefix};
    use rand::SeedableRng;

    // Normalize a timestep into comparable tuples: (router_index, prefix_num, is_advertise)
    fn normalize_step(step: SimulationTimeStep) -> Vec<(u32, u32, bool)> {
        let mut v = step
            .into_iter()
            .filter_map(|ev| match ev {
                SimulationEvent::External(ExternalEvent {
                    external_neighbor,
                    prefix,
                    route,
                }) => Some((
                    external_neighbor.index() as u32,
                    prefix.as_num(),
                    route.is_some(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        v.sort_unstable();
        v
    }

    #[test]
    fn deterministic_same_seed_same_inputs() {
        let rng1 = StdRng::seed_from_u64(42);
        let rng2 = StdRng::seed_from_u64(42);

        let mut routers_a = HashMap::new();
        routers_a.insert(RouterId::new(0), AsId::from(100));
        routers_a.insert(RouterId::new(1), AsId::from(200));
        routers_a.insert(RouterId::new(2), AsId::from(300));

        let mut routers_b = HashMap::new();
        // Different insertion order
        routers_b.insert(RouterId::new(2), AsId::from(300));
        routers_b.insert(RouterId::new(0), AsId::from(100));
        routers_b.insert(RouterId::new(1), AsId::from(200));

        let prefixes = vec![Prefix::from(0), Prefix::from(1), Prefix::from(2)];

        let mut gen1 = UniformEventGenerator::new(rng1, routers_a, prefixes.clone(), true);
        let mut gen2 = UniformEventGenerator::new(rng2, routers_b, prefixes.clone(), true);

        for _ in 0..100 {
            let s1 = normalize_step(gen1.next().unwrap());
            let s2 = normalize_step(gen2.next().unwrap());
            assert_eq!(s1, s2);
        }
    }

    /// Scenarios that set their bug up before the run starts must never see a reconfiguration
    /// event: it would trigger a reconfiguration they never asked for, and would record an event
    /// into their trace that never happened.
    #[test]
    fn no_reconfiguration_event_when_disabled() {
        let mut routers = HashMap::new();
        routers.insert(RouterId::new(0), AsId::from(100));
        routers.insert(RouterId::new(1), AsId::from(200));
        let prefixes = vec![Prefix::from(0), Prefix::from(1)];

        let mut gen =
            UniformEventGenerator::new(StdRng::seed_from_u64(42), routers, prefixes, false);

        for _ in 0..10_000 {
            assert!(
                !gen.next()
                    .unwrap()
                    .iter()
                    .any(|ev| matches!(ev, SimulationEvent::Reconfiguration)),
                "generator emitted a reconfiguration event despite being disabled"
            );
        }
    }

    /// ... whereas the prefix-list scenario relies on getting exactly one.
    #[test]
    fn exactly_one_reconfiguration_event_when_enabled() {
        let mut routers = HashMap::new();
        routers.insert(RouterId::new(0), AsId::from(100));
        routers.insert(RouterId::new(1), AsId::from(200));
        let prefixes = vec![Prefix::from(0), Prefix::from(1)];

        let mut gen = UniformEventGenerator::new(StdRng::seed_from_u64(42), routers, prefixes, true);

        let reconfigs = (0..10_000)
            .filter(|_| {
                gen.next()
                    .unwrap()
                    .iter()
                    .any(|ev| matches!(ev, SimulationEvent::Reconfiguration))
            })
            .count();
        assert_eq!(reconfigs, 1, "expected exactly one reconfiguration event");
    }

    #[test]
    fn deterministic_for_repeated_runs() {
        let mut routers = HashMap::new();
        routers.insert(RouterId::new(10), AsId::from(65010));
        routers.insert(RouterId::new(11), AsId::from(65011));
        routers.insert(RouterId::new(12), AsId::from(65012));

        let prefixes = vec![Prefix::from(10), Prefix::from(11)];

        let mut gen1 =
            UniformEventGenerator::new(StdRng::seed_from_u64(7), routers.clone(), prefixes.clone(), true);
        let mut gen2 =
            UniformEventGenerator::new(StdRng::seed_from_u64(7), routers.clone(), prefixes.clone(), true);

        for _ in 0..200 {
            let s1 = normalize_step(gen1.next().unwrap());
            let s2 = normalize_step(gen2.next().unwrap());
            assert_eq!(s1, s2);
        }
    }
}
