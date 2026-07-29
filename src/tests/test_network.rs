#[cfg(test)]
pub(crate) mod test_network {
    use bgpsim::builder::{equal_preferences, GaoRexfordPeerType};
    use bgpsim::prelude::*;
    use bgpsim::route_map::RouteMapBuilder;
    use itertools::Itertools;
    use std::collections::HashMap;
    use std::{collections::HashSet, iter::zip};

    use crate::failure::test_failure::is_destructive_failure;
    use crate::failure::*;
    use crate::queue::*;
    use crate::tests::*;

    #[test]
    fn test_line_network() {
        let (mut net, (e, b)) =
            line_network::<SinglePrefix, GlobalOspf, BasicEventQueue<SinglePrefix>>(
                BasicEventQueue::default(),
            );

        assert_forwarding!(&net, b, None);

        // Propagate a single advertisment from the external router to the border one
        net.advertise_external_route(e.into(), SinglePrefix::from(0), [1, 2, 3], None, None)
            .unwrap();

        assert_forwarding!(&net, b, Some(e));
    }

    #[test]
    fn test_long_line_network() {
        let (mut net, (e, b, r)) =
            long_line_network::<SinglePrefix, GlobalOspf, BasicEventQueue<SinglePrefix>>(
                BasicEventQueue::default(),
            );

        assert_forwarding!(&net, b, None);

        // Propagate a single advertisment from the external router to the border one
        net.advertise_external_route(e.into(), SinglePrefix::from(0), [1, 2, 3], None, None)
            .unwrap();

        assert_forwarding!(&net, b, Some(e));
        assert_forwarding!(&net, r, Some(b));
    }

    #[test]
    fn test_y_network() {
        let (mut net, (e1, e2, b, _r)) =
            y_network::<SinglePrefix, GlobalOspf, BasicEventQueue<SinglePrefix>>(
                BasicEventQueue::default(),
            );

        assert_forwarding!(net, b, None);

        // Propagate a single advertisment from the e1 to the border router
        net.advertise_external_route(e1.into(), SinglePrefix::from(0), [1, 2, 3], None, None)
            .unwrap();
        assert_forwarding!(net, b, Some(e1));

        // Propagate a single advertisment from the e2 to the border router
        net.advertise_external_route(e2.into(), SinglePrefix::from(0), [1, 2, 3], None, None)
            .unwrap();
        // This new advertisement should NOT replace the previous one
        // The tiebreaker for route selection in this case is the router id, and according to the net!() macro,
        // the router id of e1 is less than the router id of e2
        assert_forwarding!(net, b, Some(e1));
    }

    #[test]
    fn test_line_network_failure_queue() {
        let (mut net, (e, b)) = line_network::<
            SinglePrefix,
            GlobalOspf,
            OrderedEventQueue<BasicEventQueue<SinglePrefix>>,
        >(OrderedEventQueue::new(None, BasicEventQueue::new()));

        assert_forwarding!(net, b, None);

        // Propagate a single advertisment from the external router to the border one
        net.advertise_external_route(e.into(), SinglePrefix::from(0), [1, 2, 3], None, None)
            .unwrap();

        assert_forwarding!(net, b, Some(e));
    }

    #[test]
    fn test_line_interactive_network() {
        let (mut net, (e, b)) = line_network::<
            SinglePrefix,
            GlobalOspf,
            OrderedEventQueue<BasicEventQueue<SinglePrefix>>,
        >(OrderedEventQueue::new(None, BasicEventQueue::new()));
        // Put the network in manual mode
        net.manual_simulation();
        // Propagate a single advertisment from the external router to the border one
        net.advertise_external_route(e, SinglePrefix::from(0), [1, 2, 3], None, None)
            .unwrap();
        let mut net_clone = net.clone();
        // Check that the cloned network also has one in the chamber
        assert_eq!(net_clone.queue().inner_queue.len(), 1);

        // Check that nothing has propagated yet
        assert_forwarding!(net, b, None);
        assert_eq!(net, net_clone);
        // Now we enable automatic simulation on the original network
        net.auto_simulation();
        // ALWAYS call simulate() after toggling back to automatic simulation
        net.simulate().unwrap();
        assert_forwarding!(net, b, Some(e));
        assert_ne!(net, net_clone);
        net_clone.auto_simulation();
        net_clone.simulate().unwrap();

        // Withdraw the advertisement from both
        net.withdraw_external_route(e, SinglePrefix::from(0))
            .unwrap();
        net_clone
            .withdraw_external_route(e, SinglePrefix::from(0))
            .unwrap();
        assert_forwarding!(net, b, None);
        assert_eq!(net, net_clone);
    }

    #[test]
    fn test_e_interactive_network() {
        let (mut net, (e, _, _)) = e_network_route_map_scenario(None);
        // Put the network in manual mode
        net.manual_simulation();
        // Propagate a single advertisment from the external router to the border one
        net.withdraw_external_route(e[0], SinglePrefix::from(0))
            .unwrap();

        net.simulate_hooked(|net, event, result| {
            if result.is_none() {
                println!("\n\n{}", event.fmt(&net))
            }
        })
        .unwrap();
    }

    #[test]
    fn test_e_network_failure_extraction() {
        let (mut net, (e, b, r)) =
            e_network::<SinglePrefix, GlobalOspf, BasicEventQueue<SinglePrefix>>(
                BasicEventQueue::default(),
            );
        // Add three route maps assigning local preferences to the incoming routes from the external routers
        for (lp, i) in zip([200, 150, 100], 0..3) {
            let route_map = RouteMapBuilder::new()
                .order(10)
                .allow()
                .set_local_pref(lp)
                .build();
            net.set_bgp_route_map(
                b[i],
                e[i],
                bgpsim::route_map::RouteMapDirection::Incoming,
                route_map,
            )
            .unwrap();
        }
        // Advertise the same prefix for all three
        e.iter().for_each(|external| {
            net.advertise_external_route(*external, SinglePrefix::from(0), [1, 2, 3], None, None)
                .unwrap()
        });
        // Ensure that every internal router is forwarding to the correct border router
        assert_forwarding!(net, r, Some(b[0]));
        assert_forwarding!(net, b[0], Some(e[0]));
        assert_forwarding!(net, b[1], Some(b[0]));
        assert_forwarding!(net, b[2], Some(b[0]));
        // Extract the local preference failures
        let mut failure_builder = FailureSetBuilder::new();
        failure_builder.ingest_network(&net);
        // Check that the correct local preference failures are present
        let failures = failure_builder.build();
        // There are 8 failures with router localities (4 (src,*) + 4 (*,dst))
        assert_eq!(
            failures
                .iter()
                .fold(HashSet::new(), |mut set, f| {
                    let loc = f.get_locality();
                    if loc.0.is_none() || loc.1.is_none() {
                        // Add to the set
                        set.insert(loc.clone());
                    }
                    set
                })
                .len(),
            8
        );
        // There are 9 failures whose localities are session based (3 sessions for each border router)
        assert_eq!(
            failures
                .iter()
                .fold(HashSet::new(), |mut set, f| {
                    let loc = f.get_locality();
                    if loc.0.is_some() && loc.1.is_some() {
                        // Add to the set
                        set.insert(loc.clone());
                    }
                    set
                })
                .len(),
            9
        );
        // For each locality, there should be only 2 failures (destructive ones) and 7 (transformative ones)
        // The 7 come from the fact there are 3 possible local preferences in all (we consider values in between) and no communities
        assert_eq!(failures.len(), (9 + 8) * (2 + 7))
    }

    #[test]
    fn test_e_network_non_convergence() {
        let (mut net, ([e1, _, _], [_, _, b3], r)) = e_network_route_map_scenario(None);
        net.manual_simulation();

        let non_converging_failures = vec![
            Failure::BGPChangeLocalPref((Some(b3), None), 175),
            Failure::BGPChangeLocalPref((Some(b3), None), 200),
            Failure::BGPChangeLocalPref((Some(b3), None), 400),
            Failure::BGPChangeLocalPref((Some(b3), Some(r)), 175),
            Failure::BGPChangeLocalPref((Some(b3), Some(r)), 200),
            Failure::BGPChangeLocalPref((Some(b3), Some(r)), 400),
        ];

        for failure in non_converging_failures {
            // Apply a failure we know will cause non-convergence
            let mut failed_net = net
                .clone()
                .swap_queue(FailureQueue::new(failure.clone(), net.queue().clone()));
            // Add the external event
            failed_net
                .withdraw_external_route(e1, SinglePrefix::from(0))
                .unwrap();

            // Test the automatic simulation
            let mut failed_net_auto = failed_net.clone();
            failed_net_auto.auto_simulation();
            assert_eq!(
                failed_net_auto.simulate().unwrap_err(),
                NetworkError::NoConvergence
            );

            // Test the step-by-step simulation
            let mut events = Vec::new();
            assert_eq!(
                failed_net
                    .simulate_hooked(|_, event, result| {
                        // We only consider hooks that get called before the event is processed
                        if result.is_none() {
                            println!("Network Event: {}", event.fmt(&net));
                            events.push(event.clone());
                        }
                    })
                    .unwrap_err(),
                NetworkError::NoConvergence
            );
        }
    }

    #[test]
    fn test_e_network_gao_rexford() {
        // Initialize a simple scenario
        let (mut net, ([e1, e2, e3], [b1, b2, b3], r)) = e_network::<
            SinglePrefix,
            GlobalOspf,
            OrderedEventQueue<BasicEventQueue<SinglePrefix>>,
        >(OrderedEventQueue::new(
            None,
            BasicEventQueue::default(),
        ));
        let lut: HashMap<RouterId, GaoRexfordPeerType> = HashMap::from([
            (e1, GaoRexfordPeerType::Customer),
            (e2, GaoRexfordPeerType::Peer),
            (e3, GaoRexfordPeerType::Provider),
        ]);
        net.build_gao_rexford_policies(GaoRexfordPeerType::lookup, &lut)
            .unwrap();
        // Build advertisements from all three
        net.build_advertisements(SinglePrefix::from(0), equal_preferences, 3)
            .unwrap();
        // Check that all external routers are advertising the same thing
        [e1, e2, e3].windows(2).for_each(|w| {
            assert!(
                net.get_external_router(w[0])
                    .unwrap()
                    .advertised_prefixes()
                    .zip(net.get_external_router(w[1]).unwrap().advertised_prefixes())
                    .all(|(a, b)| a == b),
                "{}",
                w.fmt(&net)
            )
        });
        // Check that all routers are forwarding to e1, the customer
        assert_forwarding!(&net, b1, Some(e1));
        assert_forwarding!(&net, b2, Some(b1));
        assert_forwarding!(&net, b3, Some(b1));
        assert_forwarding!(&net, r, Some(b1));

        // Put the network in manual mode
        net.manual_simulation();
        let mut failure_builder = FailureSetBuilder::new();
        failure_builder.ingest_network(&net);

        let mut events = Vec::new();
        let mut failures = HashSet::new();

        // Propagate a single advertisment from the e2 to the border router
        net.withdraw_external_route(e1, SinglePrefix::from(0))
            .unwrap();

        net.simulate_hooked(|net, event, result| {
            // We only consider hooks that get called before the event is processed
            if result.is_none() {
                println!("{}", event.fmt(&net));
                failures.extend(failure_builder.build_from_event(&event).unwrap());
                events.push(event.clone());
            }
        })
        .unwrap();

        // Make sure the failure extraction has worked
        // Differentiate between communities and local preferences
        let (mut communities, local_preferences): (HashSet<i32>, HashSet<u32>) = failures
            .iter()
            .filter(|f| !is_destructive_failure(f))
            .partition_map(|f| match f {
                Failure::BGPChangeCommunity(_, c) => itertools::Either::Left(c),
                &Failure::BGPChangeLocalPref(_, lp) => itertools::Either::Right(lp),
                _ => panic!(),
            });
        // Check the communities
        [501, 502, 503].iter().for_each(|c| {
            assert!(communities.remove(c));
            assert!(communities.remove(&-c));
        });
        assert!(communities.is_empty());
        // Check the local preferences
        assert_eq!(local_preferences.len(), 3 * 2 + 1);

        // Apply a failure that ensures something will change
        //todo!()
    }
}
