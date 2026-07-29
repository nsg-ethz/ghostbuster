//! Integration tests that drive a real GNS3 server.
//!
//! There is a single GNS3 server, and each of these tests builds a whole emulated network on it, so
//! they cannot run at the same time as one another. Every test is therefore marked `#[serial]`,
//! which serialises them against each other while leaving the rest of the test suite free to run in
//! parallel. Without it they interfere and fail in ways that look like real bugs.
#[cfg(test)]
pub(crate) mod test_gns3 {
    use bgpsim::builder::GaoRexfordPeerType;
    use bgpsim::route_map::RouteMapBuilder;
    use ipnet::Ipv4Net;
    use log::{error, info, warn};
    use rayon::iter::{IntoParallelIterator, ParallelIterator};
    use rayon::ThreadPoolBuilder;
    use std::collections::{HashMap, HashSet};
    use std::io;
    use std::path::PathBuf;
    use std::thread::sleep;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use serial_test::serial;
    use test_log::test;

    use crate::testbed::reconfiguration::PreConfig;
    use crate::{monitoring::Controller, tests::*};
    use bgpsim::export::{Addressor, LinkId};
    use bgpsim_gns3::{Gns3Network, Gns3NetworkError};

    use crate::recording::Recording;

    fn pause() {
        info!("Pausing! Press Enter to continue...");
        let mut buffer = String::new();
        io::stdin()
            .read_line(&mut buffer) // Wait for user input (Enter key)
            .expect("Failed to read line");
    }

    fn timestamp() -> f64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");

        let timestamp = now.as_secs() as f64 + now.subsec_nanos() as f64 * 1e-9;
        timestamp
    }

    /// Checks that the filename of a given pcap and the associated router IDs are consistent
    fn captures_consistent<P, Q, Ospf>(
        net: &Network<P, Q, Ospf>,
        captures: &HashMap<(RouterId, RouterId), PathBuf>,
    ) -> bool
    where
        P: Prefix,
        Q: EventQueue<P>,
        Ospf: OspfImpl,
    {
        for ((a, b), path) in captures {
            // Filenames are, for example, b3_1-0_to_e3_0-0.pcap
            let (a_fname, b_fname) = path
                .file_name()
                .and_then(|f| f.to_str())
                .and_then(|fname| fname.split_once("_to_"))
                .map(|(a_s, b_s)| {
                    let a = a_s.split('_').next().unwrap();
                    let b = b_s.split('_').next().unwrap();
                    (a, b)
                })
                .unwrap();
            // Format node names
            let (a_nname, b_nname) = (a.fmt(&net), b.fmt(&net));
            let (a_nname, b_nname) = (a_nname.as_str(), b_nname.as_str());
            // Check both orders
            if !((a_nname, b_nname) == (a_fname, b_fname)
                || (a_nname, b_nname) == (b_fname, a_fname))
            {
                return false;
            }
        }

        true
    }

    /// Stop a running capture and get a path to the local pcap data
    fn stop_capture_to_local_file<P, Q, Ospf>(
        gns3_net: &mut Gns3Network<P, Q, Ospf>,
        a: RouterId,
        b: RouterId,
    ) -> Result<PathBuf, Gns3NetworkError>
    where
        P: Prefix,
        Q: EventQueue<P>,
        Ospf: OspfImpl,
    {
        let remote_path = gns3_net.stop_captures(a, b)?;

        Ok(crate::config::gns3_projects_path().join(
            remote_path
                .first()
                .unwrap()
                .as_ref()
                .unwrap()
                .strip_prefix("/opt/gns3/projects")
                .unwrap(),
        ))
    }

    #[test]
    #[serial]
    fn test_gns3_line_network() {
        let (net, (e, b)) = line_network::<SinglePrefix, GlobalOspf, BasicEventQueue<SinglePrefix>>(
            BasicEventQueue::default(),
        );

        let p = SinglePrefix::from(0);

        // Map this BGPSim network to a GNS3 one (should be on localhost)
        let mut gns3_net = Gns3Network::new(
            "line_network",
            &net,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            HashMap::default(),
        )
        .unwrap();

        // Check that the border router has no route installed
        let nh = gns3_net.get_next_hops(b, p);
        assert!(nh.is_ok_and(|list| list.is_empty()));

        // Propagate a single advertisment from the external router to the border one
        info!("Advertising");
        gns3_net
            .advertise_external_route(e, p, None, None, None)
            .unwrap();
        info!("Waiting for convergence...");
        gns3_net
            .wait_for_convergence(Duration::from_secs(20), None)
            .unwrap();
        // No route map should have been added
        let route_maps = gns3_net.get_frr(e).unwrap().get_route_maps().unwrap();
        assert!(route_maps.iter().all(|(_, rm)| rm
            .rules
            .iter()
            .all(|rule| rule.match_clauses.is_empty() && rule.set_clauses.is_empty())));
        // Check the next hops
        let nh = gns3_net.get_next_hops(b, p).unwrap();
        info!(
            "The next hop to {} from {} is {}",
            p.fmt(&net),
            b.fmt(&net),
            nh.iter().cloned().fmt_list(&net)
        );
        assert!(
            !nh.is_empty(),
            "B uses {} to get to {}",
            e.fmt(&net),
            p.fmt(&net)
        );

        info!("Withdrawing");
        gns3_net.withdraw_external_route(e, p).unwrap();
        gns3_net
            .wait_for_convergence(Duration::from_secs(20), None)
            .unwrap();
        let nh = gns3_net.get_next_hops(b, p).unwrap();
        assert!(
            nh.is_empty(),
            "B uses {} to get to {}",
            e.fmt(&net),
            p.fmt(&net)
        );
    }

    #[test]
    #[serial]
    fn test_gns3_snapshot() {
        let (net, (_, b, r)) =
            long_line_network::<SinglePrefix, GlobalOspf, BasicEventQueue<SinglePrefix>>(
                BasicEventQueue::default(),
            );

        let _ = SinglePrefix::default();

        // Map this BGPSim network to a GNS3 one (should be on localhost)
        let mut gns3_net = Gns3Network::new(
            "snapshot_test",
            &net,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            HashMap::default(),
        )
        .unwrap();
        gns3_net
            .wait_for_convergence(Duration::from_secs(20), None)
            .unwrap();

        // Take a snapshot of the converged network
        gns3_net.take_snapshot().unwrap();
        sleep(Duration::from_secs(2));
        // Set an MRAI interval on a router
        gns3_net
            .get_frr(b)
            .unwrap()
            .set_advertisement_interval(
                2,
                gns3_net.get_addressor().try_get_router_address(r).unwrap(),
            )
            .unwrap();
        assert!(gns3_net
            .get_frr(b)
            .unwrap()
            .get_bgp_neighbor(gns3_net.get_addressor().try_get_router_address(r).unwrap())
            .unwrap()
            .is_some_and(|neighbor| neighbor.advertisement_interval == 2000));
        sleep(Duration::from_secs(1));
        gns3_net.restore_snapshot().unwrap();
        sleep(Duration::from_secs(10));
        assert!(gns3_net
            .get_frr(b)
            .unwrap()
            .get_bgp_neighbor(gns3_net.get_addressor().try_get_router_address(r).unwrap())
            .unwrap()
            .is_some_and(|neighbor| neighbor.advertisement_interval == 0));
    }

    #[test]
    #[serial]
    fn test_gns3_comparison() {
        let (mut net, (e, b, _)) =
            long_line_network::<SimplePrefix, GlobalOspf, BasicEventQueue<SimplePrefix>>(
                BasicEventQueue::default(),
            );
        let net_baseline = net.clone();
        let p1 = SimplePrefix::from(1);
        let p2 = SimplePrefix::from(2);
        let prefixes = HashSet::from([p1, p2]);

        // Map this BGPSim network to a GNS3 one (should be on localhost)
        let mut gns3_net = Gns3Network::new(
            "comparison_test",
            &net_baseline,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            HashMap::default(),
        )
        .unwrap();
        gns3_net
            .wait_for_convergence(Duration::from_secs(20), None)
            .unwrap();

        gns3_net
            .advertise_external_route(e, p1, None, None, None)
            .unwrap();
        net.advertise_external_route(e, p1, vec![net.get_device(e).unwrap().as_id()], None, None)
            .unwrap();
        sleep(Duration::from_secs(2));
        gns3_net
            .advertise_external_route(e, p2, None, None, None)
            .unwrap();
        sleep(Duration::from_secs(5));
        warn!(
            "{}",
            net.get_internal_router(b)
                .unwrap()
                .bgp
                .fmt_prefix_table(&net, p2)
        );
        let table_diff = gns3_net.compare_bgp_tables(&net, &prefixes).unwrap();
        assert!(!table_diff.is_empty());
        warn!("{}", table_diff.fmt_multiline(&net));
        net.advertise_external_route(e, p2, vec![net.get_device(e).unwrap().as_id()], None, None)
            .unwrap();
        assert!(gns3_net
            .compare_bgp_tables(&net, &prefixes)
            .unwrap()
            .is_empty());
        warn!("Tables now match");
    }

    #[test]
    #[serial]
    fn test_gns3_different_routers() {
        let (net, (e, b)) = line_network::<SinglePrefix, GlobalOspf, BasicEventQueue<SinglePrefix>>(
            BasicEventQueue::default(),
        );

        let map = HashMap::from([
            (e, ("FRR-8.5.1", "frr:8.5.1")),
            (b, ("FRR-docker", "frr:latest")),
        ]);

        let gns3_net_init = Gns3Network::new(
            "line_network",
            &net,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            map,
        );

        gns3_net_init.unwrap_or_else(|e| panic!("{}", e));
    }

    #[test]
    #[serial]
    fn test_gns3_e_network_with_routemaps() {
        let (net, (e, [b1, b2, _], r)) = e_network_route_map_scenario(None);

        let p = SinglePrefix::from(0);

        // Map this BGPSim network to a GNS3 one (should be on localhost)
        let mut gns3_net = Gns3Network::new(
            "e_network",
            &net,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            HashMap::default(),
        )
        .unwrap();

        // Check the correct routes were selected by the route reflector
        info!("Waiting for convergence...");
        gns3_net
            .wait_for_convergence(Duration::from_secs(20), None)
            .unwrap();
        assert!(gns3_net.equal_forwarding_state(&net).unwrap());
        // Sanity check
        assert_forwarding!(&net, r, Some(b1));
        assert_eq!(*gns3_net.get_next_hops(r, p).unwrap().first().unwrap(), b1);
        info!("Forwarding state checks passed");
        // Check that the route maps were placed on the external routers.
        //
        // Every external router is configured with an inbound and an outbound route map. Note that
        // here they only hold the catch-all permit entry: bgpsim emits a rule carrying match and
        // set clauses only for routes that actually need AS path, community or MED manipulation
        // (see `advertise_route` in bgpsim's cisco_frr export). In this scenario e[i] advertises
        // the prefix with an AS path of just its own AS, so there is nothing to prepend.
        let route_maps = gns3_net.get_frr(e[0]).unwrap().get_route_maps().unwrap();
        for name in ["neighbor-in", "neighbor-out"] {
            assert!(
                route_maps.contains_key(name),
                "expected route map '{name}' on the external router, got {route_maps:?}"
            );
        }

        info!("Capturing messages on link between r and b2");
        let _ = gns3_net.start_captures(r, b2).unwrap();
        sleep(Duration::from_secs(1));

        info!("Withdrawing best route");
        gns3_net.withdraw_external_route(e[0], p).unwrap();
        info!("Waiting for convergence...");
        gns3_net
            .wait_for_convergence(Duration::from_secs(20), None)
            .unwrap();

        let path_bufs = gns3_net.stop_captures(r, b2).unwrap();
        info!("Stopped {} captures", path_bufs.len());

        // Ensure that all the routes have actually converged to the new network
        let nh = gns3_net.get_next_hops(b1, p).unwrap();
        assert_eq!(
            nh,
            vec![r],
            "Wrong next hop, we got {} and should have gotten {}",
            nh.fmt(&net),
            r.fmt(&net)
        );

        // Since we have withdrawn the route from e[0] the route maps should have been cleared
        let route_maps = gns3_net.get_frr(e[0]).unwrap().get_route_maps().unwrap();
        assert!(route_maps.iter().all(|(_, rm)| rm
            .rules
            .iter()
            .all(|rule| rule.match_clauses.is_empty() && rule.set_clauses.is_empty())));
    }

    #[test]
    #[serial]
    fn test_gns3_parse_pcap() {
        let (net, (e, _, _)) = e_network_route_map_scenario(None);

        let p = SinglePrefix::from(0);

        // Map this BGPSim network to a GNS3 one (should be on localhost)
        let mut gns3_net = Gns3Network::new(
            "e_network",
            &net,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            HashMap::default(),
        )
        .unwrap();
        info!("Waiting for initial convergence...");
        std::thread::sleep(Duration::from_secs(20));

        gns3_net.get_links().iter().for_each(|(&LinkId(a, b), _)| {
            gns3_net.start_captures(a, b).unwrap();
        });
        // Then withdraw the best external route
        gns3_net.withdraw_external_route(e[0], p).unwrap();
        info!("Waiting for convergence after withdrawal...");
        gns3_net
            .wait_for_convergence(Duration::from_secs(20), None)
            .unwrap();
        let link_captures_local: HashMap<_, PathBuf> = gns3_net
            .get_links()
            .keys()
            .map(|&LinkId(a, b)| {
                let path = stop_capture_to_local_file(&mut gns3_net, a, b).unwrap();
                ((a, b), path)
            })
            .collect();

        assert!(captures_consistent(&net, &link_captures_local));

        let recording = Recording::from_pcaps(link_captures_local, &gns3_net);
        info!("{}", recording.fmt_multiline(&net));
    }

    #[test]
    #[serial]
    fn test_gns3_monitoring_line() {
        let (net, (e, _, _)) =
            long_line_network::<SinglePrefix, GlobalOspf, BasicEventQueue<SinglePrefix>>(
                BasicEventQueue::default(),
            );

        let p = SinglePrefix::from(0);
        let arena = Default::default();
        let mut controller = Controller::new(&net, &arena);

        // Map this BGPSim network to a GNS3 one (should be on localhost)
        let mut gns3_net = Gns3Network::new(
            "line_network",
            &net,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            HashMap::default(),
        )
        .unwrap();
        info!("Waiting for initial convergence...");
        std::thread::sleep(Duration::from_secs(20));

        gns3_net.get_links().iter().for_each(|(&LinkId(a, b), _)| {
            gns3_net.start_captures(a, b).unwrap();
        });
        gns3_net
            .advertise_external_route(e, p, None, None, None)
            .unwrap();
        sleep(Duration::from_secs(1));
        gns3_net.withdraw_external_route(e, p).unwrap();

        info!("Waiting for convergence after withdrawal...");
        gns3_net
            .wait_for_convergence(Duration::from_secs(20), None)
            .unwrap();
        let link_captures_local: HashMap<_, PathBuf> = gns3_net
            .get_links()
            .keys()
            .map(|&LinkId(a, b)| {
                let path = stop_capture_to_local_file(&mut gns3_net, a, b).unwrap();
                ((a, b), path)
            })
            .collect();

        assert!(captures_consistent(&net, &link_captures_local));

        let recording = Recording::from_pcaps(link_captures_local, &gns3_net)
            .filter_routers(&net.internal_indices().collect());
        info!("{}", recording.fmt_multiline(&net));
        let monitoring_errors = controller.monitor_recording(recording).unwrap();
        assert!(monitoring_errors.is_empty());
    }

    #[test]
    #[serial]
    fn test_gns3_monitoring_route_maps() {
        let (net, (e, _, _)) = e_network_route_map_scenario(None);

        let p = SinglePrefix::from(0);
        let arena = Default::default();
        let mut controller = Controller::new(&net, &arena);

        // Map this BGPSim network to a GNS3 one (should be on localhost)
        let mut gns3_net = Gns3Network::new(
            "e_network",
            &net,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            HashMap::default(),
        )
        .unwrap();
        info!("Waiting for initial convergence...");
        std::thread::sleep(Duration::from_secs(20));

        gns3_net.get_links().iter().for_each(|(&LinkId(a, b), _)| {
            gns3_net.start_captures(a, b).unwrap();
        });
        // Withdraw the best external route
        gns3_net.withdraw_external_route(e[0], p).unwrap();
        info!("Waiting for convergence after withdrawal...");
        std::thread::sleep(Duration::from_secs(20));
        let link_captures_local: HashMap<_, PathBuf> = gns3_net
            .get_links()
            .keys()
            .map(|&LinkId(a, b)| {
                let path = stop_capture_to_local_file(&mut gns3_net, a, b).unwrap();
                ((a, b), path)
            })
            .collect();

        let recording = Recording::from_pcaps(link_captures_local, &gns3_net)
            .filter_routers(&net.internal_indices().collect());
        info!("{}", recording.fmt_multiline(&net));
        let monitoring_errors = controller.monitor_recording(recording).unwrap();
        warn!("{}", monitoring_errors.fmt_multiline(&net));
        assert!(monitoring_errors.is_empty());
    }

    /// This function replicates and verifies FRR bug [#11341](https://github.com/FRRouting/frr/issues/11341)
    #[test]
    #[serial]
    fn test_gns3_lp_bug() {
        // We want to replicate this bug with a simple long line network (and a simple prefix)
        let (mut net, (e, b, _)) =
            line_reflector_network::<SinglePrefix, GlobalOspf, BasicEventQueue<SinglePrefix>>(
                BasicEventQueue::default(),
            );

        // Set an incoming route map to increase the local preference of one of the two
        let route_map = RouteMapBuilder::new()
            .order(10)
            .allow()
            .set_local_pref_delta(10)
            .build();
        net.set_bgp_route_map(
            b.0,
            e.0,
            bgpsim::route_map::RouteMapDirection::Incoming,
            route_map,
        )
        .unwrap();

        // Get a controller for this network
        let arena = Default::default();
        let mut controller = Controller::new(&net, &arena);

        // let router_images = HashMap::from([(b.0, ("FRR-8.3-dev", "frr:gns-alpine-23a1220847"))]);
        let router_images = HashMap::new();
        // WARNING: we are getting the same issue when we consider images that "should" have been patched

        // Map this BGPSim network to a GNS3 one (should be on localhost)
        let mut gns3_net = Gns3Network::new(
            "lp_bug_network",
            &net,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            router_images,
        )
        .unwrap();
        info!("Waiting for initial convergence...");

        // Start a pcap on both links
        gns3_net.get_links().iter().for_each(|(&LinkId(x, y), _)| {
            gns3_net.start_captures(x, y).unwrap();
        });

        gns3_net
            .advertise_external_route(e.0, SinglePrefix::default(), None, None, None)
            .unwrap();
        sleep(Duration::from_millis(1000));
        gns3_net
            .advertise_external_route(e.1, SinglePrefix::default(), None, None, None)
            .unwrap();
        sleep(Duration::from_secs(20));

        // Extract the BGP messages from the pcap
        let link_captures_local: HashMap<_, PathBuf> = gns3_net
            .get_links()
            .keys()
            .map(|&LinkId(x, y)| {
                let path = stop_capture_to_local_file(&mut gns3_net, x, y).unwrap();
                ((x, y), path)
            })
            .collect();

        // Assign the pcap messages to each router
        let recording = Recording::from_pcaps(link_captures_local, &gns3_net)
            .filter_routers(&net.internal_indices().collect());
        info!("{}", recording.fmt_multiline(&net));
        let monitoring_errors = controller.monitor_recording(recording).unwrap();
        warn!("Monitoring errors: {}", monitoring_errors.fmt(&net));
        pause();
        assert!(!monitoring_errors.is_empty());
    }

    /// This function replicates and verifies FRR bug [#18098](https://github.com/FRRouting/frr/issues/18098)
    #[test]
    #[serial]
    fn test_gns3_mrai_bug() {
        // We want to replicate this bug with a simple long line network (and a simple prefix)
        let (net, (e, b, r)) =
            long_line_network::<SimplePrefix, GlobalOspf, BasicEventQueue<SimplePrefix>>(
                BasicEventQueue::default(),
            );

        let router_images = HashMap::from([
            (b, ("FRR-10.2.1", "frr:10.2.1")),
            (r, ("FRR-10.2.1", "frr:10.2.1")),
        ]);
        // Map this BGPSim network to a GNS3 one (should be on localhost)
        let mut gns3_net = Gns3Network::new(
            "long_line_network",
            &net,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            router_images,
        )
        .unwrap();
        info!("Waiting for initial convergence...");
        sleep(Duration::from_secs(20));

        // Set the MRAI on the border router (2) and on the internal router (5)
        // WARN: The MRAI on the internal router is actually unnecessary
        gns3_net
            .get_frr(b)
            .unwrap()
            .set_advertisement_interval(
                2,
                gns3_net.get_addressor().try_get_router_address(r).unwrap(),
            )
            .unwrap();
        assert!(gns3_net
            .get_frr(b)
            .unwrap()
            .get_bgp_neighbor(gns3_net.get_addressor().try_get_router_address(r).unwrap())
            .unwrap()
            .is_some_and(|neighbor| neighbor.advertisement_interval == 2000));
        gns3_net
            .get_frr(r)
            .unwrap()
            .set_advertisement_interval(
                5,
                gns3_net.get_addressor().try_get_router_address(b).unwrap(),
            )
            .unwrap();
        info!("Set MRAI timers on both routers");

        // Start a pcap on both links
        gns3_net.get_links().iter().for_each(|(&LinkId(x, y), _)| {
            gns3_net.start_captures(x, y).unwrap();
        });

        let p1 = SimplePrefix::from(0);
        let p2 = SimplePrefix::from(1);
        // Get a controller for this network and these prefixes
        let arena = Default::default();
        let mut controller =
            Controller::new_for_prefixes(&net, &[p1, p2].into_iter().collect(), &arena);

        // Advertisement sequence
        info!("Advertising sequence");
        gns3_net
            .advertise_external_route(e, p1, None, None, None)
            .unwrap();
        sleep(Duration::from_millis(1000));
        gns3_net
            .advertise_external_route(e, p2, None, None, None)
            .unwrap();
        sleep(Duration::from_millis(250));
        gns3_net.withdraw_external_route(e, p1).unwrap();
        sleep(Duration::from_millis(250));
        gns3_net
            .advertise_external_route(e, p1, None, None, None)
            .unwrap();

        info!("Waiting for final convergence...");
        sleep(Duration::from_secs(20));

        // Extract the BGP messages from the pcap
        let link_captures_local: HashMap<_, PathBuf> = gns3_net
            .get_links()
            .keys()
            .map(|&LinkId(x, y)| {
                let path = stop_capture_to_local_file(&mut gns3_net, x, y).unwrap();
                ((x, y), path)
            })
            .collect();

        // Assign the pcap messages to each router
        let recording = Recording::from_pcaps(link_captures_local, &gns3_net)
            .filter_routers(&net.internal_indices().collect());
        info!("{}", recording.fmt_multiline(&net));
        let monitoring_errors = controller.monitor_recording(recording).unwrap();
        warn!("Monitoring errors: {}", monitoring_errors.fmt(&net));
        assert!(!monitoring_errors.is_empty());
    }

    /// This function replicates and verifies FRR bug [#13007](https://github.com/FRRouting/frr/issues/13007)
    #[test]
    #[serial]
    fn test_gns3_pl_bug() {
        // We want to replicate this bug with a line reflector network (and a simple prefix)
        let (mut net, (e, b, r)) =
            line_reflector_network::<SimplePrefix, GlobalOspf, BasicEventQueue<SimplePrefix>>(
                BasicEventQueue::default(),
            );

        let p1 = SimplePrefix::from(0);
        let p2 = SimplePrefix::from(1);
        // Add a route map to the border router to explicitly permit the first prefix
        let route_map = RouteMapBuilder::new()
            .order(10)
            .allow()
            .match_prefix(p1)
            .exit()
            .build();
        net.set_bgp_route_map(
            b.0,
            r,
            bgpsim::route_map::RouteMapDirection::Outgoing,
            route_map,
        )
        .unwrap();
        // We need to add an explicit deny to the route-map in order to circumvent the BGPSim default
        // behaviour which allows all routes
        let route_map = RouteMapBuilder::new().order(20).deny().build();
        net.set_bgp_route_map(
            b.0,
            r,
            bgpsim::route_map::RouteMapDirection::Outgoing,
            route_map,
        )
        .unwrap();

        // Get a controller for this network and these prefixes
        let arena = Default::default();
        let mut controller =
            Controller::new_for_prefixes(&net, &[p1, p2].into_iter().collect(), &arena);

        let router_images = HashMap::from([(b.0, ("FRR-8.4.2", "frr:8.4.2"))]);
        // Map this BGPSim network to a GNS3 one (should be on localhost)
        let mut gns3_net = Gns3Network::new(
            "pl_bug_network",
            &net,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            router_images,
        )
        .unwrap();
        info!("Waiting for initial convergence...");
        sleep(Duration::from_secs(20));

        info!("First round of advertisements");
        gns3_net.get_links().iter().for_each(|(&LinkId(x, y), _)| {
            gns3_net.start_captures(x, y).unwrap();
        });
        gns3_net
            .advertise_external_route(e.0, p1, None, None, None)
            .unwrap();
        gns3_net
            .advertise_external_route(e.0, p2, None, None, None)
            .unwrap();
        sleep(Duration::from_millis(5000));
        let first_captures: HashMap<_, PathBuf> = gns3_net
            .get_links()
            .keys()
            .map(|&LinkId(x, y)| {
                let path = stop_capture_to_local_file(&mut gns3_net, x, y).unwrap();
                ((x, y), path)
            })
            .collect();
        let first_recording = Recording::from_pcaps(first_captures.clone(), &gns3_net)
            .filter_routers(&net.internal_indices().collect());

        // Do the same in the simulation to compare
        let mut sim_net = net.clone();
        sim_net.manual_simulation();
        sim_net
            .advertise_external_route(e.0, p1, vec![1], None, None)
            .unwrap();
        sim_net
            .advertise_external_route(e.0, p2, vec![1], None, None)
            .unwrap();
        println!("Simulation events:");
        sim_net
            .simulate_hooked(|net, event, result| {
                // We only consider hooks that get called before the event is processed
                if result.is_none() {
                    println!("\t{}", event.fmt(&net));
                }
            })
            .unwrap();

        // Check that p2 was not propagated to r
        assert!(!gns3_net
            .get_frr(r)
            .unwrap()
            .get_all_routes()
            .unwrap()
            .contains_key(&Ipv4Net::from(p2.into_ipv4_prefix())));
        // Change the prefix lists in the border router to trigger the bug
        let prefix_lists = gns3_net.get_frr(b.0).unwrap().get_prefix_lists().unwrap();
        let (name, pl) = prefix_lists.iter().next().unwrap();
        let entry = pl.entries.first().unwrap();
        info!("Triggering bug");
        gns3_net
            .get_frr(b.0)
            .unwrap()
            .configure(format!(
                "ip prefix-list {name} seq {} deny any",
                entry.sequence_number
            ))
            .unwrap();
        sleep(Duration::from_millis(1000));
        gns3_net
            .get_frr(b.0)
            .unwrap()
            .configure(format!(
                "ip prefix-list {name} seq {} permit {}",
                entry.sequence_number, entry.prefix
            ))
            .unwrap();
        info!("Waiting for convergence after bug...");
        sleep(Duration::from_secs(20));
        // Check that p2 is now being propagated to r
        assert!(gns3_net
            .get_frr(r)
            .unwrap()
            .get_all_routes()
            .unwrap()
            .contains_key(&Ipv4Net::from(p2.into_ipv4_prefix())));

        info!("Second round of advertisements");
        gns3_net.get_links().iter().for_each(|(&LinkId(x, y), _)| {
            gns3_net.start_captures(x, y).unwrap();
        });
        gns3_net
            .advertise_external_route(e.1, p2, None, None, None)
            .unwrap();
        sleep(Duration::from_millis(5000));
        let second_captures: HashMap<_, PathBuf> = gns3_net
            .get_links()
            .keys()
            .map(|&LinkId(x, y)| {
                let path = stop_capture_to_local_file(&mut gns3_net, x, y).unwrap();
                ((x, y), path)
            })
            .collect();
        let second_recording = Recording::from_pcaps(second_captures, &gns3_net)
            .filter_routers(&net.internal_indices().collect());

        // Do the same in the simulation to compare
        sim_net
            .advertise_external_route(e.1, p2, vec![1], None, None)
            .unwrap();
        println!("Simulation events:");
        sim_net
            .simulate_hooked(|net, event, result| {
                // We only consider hooks that get called before the event is processed
                if result.is_none() {
                    println!("\t{}", event.fmt(&net));
                }
            })
            .unwrap();

        println!(
            "First recording:\n{}\nSecond recording:\n{}",
            first_recording.fmt_multiline(&net),
            second_recording.fmt_multiline(&net)
        );

        // Monitor the first half
        let monitoring_errors = controller.monitor_recording(first_recording).unwrap();
        warn!(
            "First part monitoring errors: {}",
            monitoring_errors.fmt(&net)
        );
        assert!(monitoring_errors.is_empty());
        // Then the second half
        let monitoring_errors = controller.monitor_recording(second_recording).unwrap();
        warn!(
            "Second part monitoring errors: {}",
            monitoring_errors.fmt(&net)
        );
        assert!(!monitoring_errors.is_empty());
    }

    /// This function aims to monitor for the Prefix List bug without the need for stopping the collection
    #[test]
    #[serial]
    fn test_gns3_pl_bug_monitoring() {
        // We want to replicate this bug with a line reflector network (and a simple prefix)
        let (mut net, (e, b, r)) =
            long_line_network::<SimplePrefix, GlobalOspf, BasicEventQueue<SimplePrefix>>(
                BasicEventQueue::default(),
            );
        let as_e = net.get_device(e).unwrap().as_id();

        let p0 = SimplePrefix::from(0);
        let p1 = SimplePrefix::from(1);
        // Add a route map to the border router to explicitly permit the first prefix
        let route_map = RouteMapBuilder::new()
            .order(10)
            .allow()
            .match_prefix(p0)
            .exit()
            .build();
        net.set_bgp_route_map(
            b,
            r,
            bgpsim::route_map::RouteMapDirection::Outgoing,
            route_map,
        )
        .unwrap();
        // We need to add an explicit deny to the route-map in order to circumvent the BGPSim default
        // behaviour which allows all routes
        let route_map = RouteMapBuilder::new().order(20).deny().build();
        net.set_bgp_route_map(
            b,
            r,
            bgpsim::route_map::RouteMapDirection::Outgoing,
            route_map,
        )
        .unwrap();

        // Get a controller for this network and these prefixes
        let arena = Default::default();
        let mut controller =
            Controller::new_for_prefixes(&net, &[p0, p1].into_iter().collect(), &arena);

        let router_images = HashMap::from([(b, ("FRR-8.4.2", "frr:8.4.2"))]);
        // Map this BGPSim network to a GNS3 one (should be on localhost)
        let mut gns3_net = Gns3Network::new(
            "pl_bug_network",
            &net,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            router_images,
        )
        .unwrap();
        info!("Waiting for initial convergence...");
        sleep(Duration::from_secs(20));

        // Start the monitoring
        gns3_net.get_links().iter().for_each(|(&LinkId(x, y), _)| {
            gns3_net.start_captures(x, y).unwrap();
        });
        info!("First round of advertisements at {:.6}", timestamp());
        gns3_net
            .advertise_external_route(e, p0, None, None, None)
            .unwrap();
        gns3_net
            .advertise_external_route(e, p1, None, None, None)
            .unwrap();
        sleep(Duration::from_millis(5000));

        // Do the same in the simulation to compare
        let mut sim_net = net.clone();
        sim_net.manual_simulation();
        sim_net
            .advertise_external_route(e, p0, [as_e], None, None)
            .unwrap();
        sim_net
            .advertise_external_route(e, p1, [as_e], None, None)
            .unwrap();
        info!("Simulation events:");
        sim_net
            .simulate_hooked(|net, event, result| {
                // We only consider hooks that get called before the event is processed
                if result.is_none() {
                    info!("\t{}", event.fmt(&net));
                }
            })
            .unwrap();

        // Check that p1 was not propagated to r
        assert!(!gns3_net
            .get_frr(r)
            .unwrap()
            .get_all_routes()
            .unwrap()
            .contains_key(&Ipv4Net::from(p1.into_ipv4_prefix())));
        // Change the prefix lists in the border router to trigger the bug
        let prefix_lists = gns3_net.get_frr(b).unwrap().get_prefix_lists().unwrap();
        let (name, pl) = prefix_lists.iter().next().unwrap();
        let entry = pl.entries.first().unwrap();
        info!("Triggering bug at {:.6}", timestamp());
        gns3_net
            .get_frr(b)
            .unwrap()
            .configure(format!(
                "ip prefix-list {name} seq {} deny any",
                entry.sequence_number
            ))
            .unwrap();
        sleep(Duration::from_millis(1000));
        gns3_net
            .get_frr(b)
            .unwrap()
            .configure(format!(
                "ip prefix-list {name} seq {} permit {}",
                entry.sequence_number, entry.prefix
            ))
            .unwrap();
        info!("Waiting for convergence after bug...");
        sleep(Duration::from_secs(20));
        // Check that p1 is now being propagated to r
        assert!(gns3_net
            .get_frr(r)
            .unwrap()
            .get_all_routes()
            .unwrap()
            .contains_key(&Ipv4Net::from(p1.into_ipv4_prefix())));
        let captures: HashMap<_, PathBuf> = gns3_net
            .get_links()
            .keys()
            .map(|&LinkId(x, y)| {
                let path = stop_capture_to_local_file(&mut gns3_net, x, y).unwrap();
                ((x, y), path)
            })
            .collect();
        let recording = Recording::from_pcaps(captures, &gns3_net)
            .filter_routers(&net.internal_indices().collect());
        // Filter out the sacrificial prefix
        let recording_p1 = recording.filter_prefixes(&HashSet::from([p1]));

        info!(
            "Recording for {}:\n{}",
            p1.fmt(&net),
            recording_p1.fmt_multiline(&net),
        );

        let monitoring_errors = controller.monitor_recording(recording_p1).unwrap();
        warn!("Monitoring errors: {}", monitoring_errors.fmt(&net));
        assert!(!monitoring_errors.is_empty());
        let table_diff = gns3_net
            .compare_bgp_tables(&net, &HashSet::from([p1]))
            .unwrap();
        warn!("{}", table_diff.fmt(&net));
        assert!(!table_diff.is_empty());
        pause();
    }

    /// This test checks how important the timing of the messages is, mainly to motivate a need to be robust
    #[test]
    #[serial]
    fn test_gns3_timing() {
        let (net, (e, _, _, _)) =
            timing_network::<SinglePrefix, GlobalOspf, BasicEventQueue<SinglePrefix>>(
                BasicEventQueue::default(),
            );

        // Get a controller for this network
        let arena = Default::default();
        let mut controller = Controller::new(&net, &arena);

        let mut gns3_net = Gns3Network::new(
            "timing_network",
            &net,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            HashMap::new(),
        )
        .unwrap();

        gns3_net.get_links().iter().for_each(|(&LinkId(x, y), _)| {
            gns3_net.start_captures(x, y).unwrap();
        });
        // for interval in 0..20 {
        //     info!("Starting interval: {}", interval);
        gns3_net
            .advertise_external_route(e[0], SinglePrefix::default(), None, None, None)
            .unwrap();
        sleep(Duration::from_secs(5));
        gns3_net
            .withdraw_external_route(e[0], SinglePrefix::default())
            .unwrap();
        sleep(Duration::from_secs(5));
        // }
        let link_captures_local: HashMap<_, PathBuf> = gns3_net
            .get_links()
            .keys()
            .map(|&LinkId(x, y)| {
                let path = stop_capture_to_local_file(&mut gns3_net, x, y).unwrap();
                ((x, y), path)
            })
            .collect();
        let recording = Recording::from_pcaps(link_captures_local, &gns3_net)
            .filter_routers(&net.internal_indices().collect());

        info!("{}", recording.fmt_multiline(&net));
        let monitoring_errors = controller.monitor_recording(recording).unwrap();
        warn!("Monitoring errors: {}", monitoring_errors.fmt(&net));
        pause();
    }

    /// Test to reproduce concurrent telnet connection issues when multiple GNS3
    /// networks are created in parallel - mimics the actual mrai_bug.rs conditions
    #[test]
    #[serial]
    fn test_gns3_concurrent_telnet_bug() {
        use bgpsim::builder::{
            extend_to_k_external_routers_seeded, k_random_nodes_seeded, GaoRexfordPeerType,
        };
        use bgpsim::topology_zoo::TopologyZoo;
        use rand::{rngs::StdRng, SeedableRng};

        // Build the same network as mrai_bug.rs
        let topology = TopologyZoo::Abilene;
        let mut net = topology.build::<SimplePrefix, BasicEventQueue<SimplePrefix>, GlobalOspf>(
            BasicEventQueue::default(),
        );
        let mut rng = StdRng::seed_from_u64(42);
        net.build_external_routers(extend_to_k_external_routers_seeded, (&mut rng, 4))
            .unwrap();
        net.build_ibgp_route_reflection(k_random_nodes_seeded, (&mut rng, 2))
            .unwrap();
        net.build_ebgp_sessions().unwrap();
        net.build_link_weights(bgpsim::builder::constant_link_weight, 10.0)
            .unwrap();
        net.build_gao_rexford_policies_seeded(
            &mut rng,
            GaoRexfordPeerType::random_seeded,
            (0.2, 0.3),
        )
        .unwrap();

        warn!(
            "Built network with {} internal routers, {} external routers",
            net.internal_indices().count(),
            net.external_indices().count()
        );

        ThreadPoolBuilder::new()
            .num_threads(2)
            .build_global()
            .unwrap();

        let result: Result<Vec<()>, Gns3NetworkError> = (0..4)
            .into_par_iter()
            .map(|number| {
                let _gns3_net = Gns3Network::new(
                    format!("concurrent_test_{}", number),
                    &net,
                    Some(crate::config::gns3_host()),
                    Some(crate::config::gns3_port()),
                    false,
                    HashMap::default(),
                )?;
                Ok(())
            })
            .collect();

        match &result {
            Ok(_) => warn!("All networks created successfully"),
            Err(e) => error!("Failed to create networks: {:?}", e),
        }

        result.unwrap();
    }

    /// Test to verify the forwarding behaviour of FRR routers when receiving the same route from two route reflectors
    #[test]
    #[serial]
    fn test_gns3_forwarding_behaviour() {
        // The point is: how does GNS3 handle a withdrawal from route reflectors, does it still have one or no?
        let (net, ((e1, _), _, (r1, _), i)) = net! {
            Prefix = SinglePrefix;
            links = {
                b -> r1: 1 ;
                b -> r2: 1 ;
                r1-> i:  1 ;
                r2-> i:  1 ;
            };
            Ospf = GlobalOspf;
            sessions = {
                e1!(1) -> b;
                e2!(2) -> i;
                r1 -> b: client;
                r2 -> b: client;
                r1 -> i: client;
                r2 -> i: client;
                // Macro will autoadd links for eBGP sessions
            };
            // Make this flexible as far as queues go
            Queue = BasicEventQueue<SinglePrefix>;
            queue = BasicEventQueue::default();
            // No advertisements either
            return ((e1, e2), b, (r1, r2), i)
        };

        let mut gns3_net = Gns3Network::new(
            "two_rr_network",
            &net,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            HashMap::new(),
        )
        .unwrap();

        gns3_net.get_links().iter().for_each(|(&LinkId(x, y), _)| {
            gns3_net.start_captures(x, y).unwrap();
        });
        gns3_net
            .advertise_external_route(e1, SinglePrefix::default(), None, None, None)
            .unwrap();
        sleep(Duration::from_secs(3));
        gns3_net.set_link_delay(r1, i, 10000, 0).unwrap();
        sleep(Duration::from_secs(3));
        gns3_net
            .withdraw_external_route(e1, SinglePrefix::default())
            .unwrap();
        sleep(Duration::from_secs(20));

        let link_captures_local: HashMap<_, PathBuf> = gns3_net
            .get_links()
            .keys()
            .map(|&LinkId(x, y)| {
                let path = stop_capture_to_local_file(&mut gns3_net, x, y).unwrap();
                ((x, y), path)
            })
            .collect();
        let recording = Recording::from_pcaps(link_captures_local, &gns3_net)
            .filter_routers(&net.internal_indices().collect());

        info!("{}", recording.fmt_multiline(&net));
    }

    /// Test to check logging and bug message recovery from logs
    #[test]
    #[serial]
    fn test_gns3_logging() {
        const BUG_STRING: &'static str = "<<<:::BUG:::>>>";

        let (mut net, (e, b)) =
            line_network::<SimplePrefix, GlobalOspf, BasicEventQueue<SimplePrefix>>(
                BasicEventQueue::default(),
            );
        net.build_gao_rexford_policies(GaoRexfordPeerType::random, (0.3, 0.3))
            .unwrap();
        let prefix = SimplePrefix::from(0);

        let pre_conf = PreConfig::ToIncremental {
            router: b,
            external: e,
        };
        let net = pre_conf.apply(net).unwrap();

        // Map this BGPSim network to a GNS3 one (should be on localhost)
        let mut gns3_net = Gns3Network::new(
            "line_network",
            &net,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            HashMap::from([(b, ("FRR-LP-BUG", "frr:gns-alpine-lp-bug"))]),
        )
        .unwrap();

        info!("Enabling logging");
        gns3_net.enable_log(b).unwrap();

        sleep(Duration::from_secs(5));
        gns3_net
            .advertise_external_route(e, prefix, None, None, None)
            .unwrap();
        sleep(Duration::from_secs(5));

        let log = gns3_net.get_log(b).unwrap().filter_substring(BUG_STRING);
        info!("Logs from router {}:\n{:?}", b.fmt(&net), log);
        pause();
    }
}
