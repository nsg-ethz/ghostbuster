use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use bgpsim::{
    bgp::BgpSessionType,
    prelude::NetworkFormatter,
    types::{RouterId, SimplePrefix as P},
};
use bgpsim_gns3::Gns3NetworkError;
use failure_extraction::{
    recording::Recording,
    testbed::{
        build_test_network,
        ground_truth::BugReport,
        perform_experiment,
        reconfiguration::{Gns3Config, PreConfig},
        run::{Run, RunConfig},
        BaselineNetworkConfig, ExperimentConfig,
    },
};
use rand::{rngs::StdRng, SeedableRng};

use failure_extraction::config::experiments_path;
const GROUND_TRUTH: bool = true;

fn main() -> Result<(), Gns3NetworkError> {
    let network_config = BaselineNetworkConfig {
        n_external: 4,
        with_monitor_speaker: false,
    };

    const SEED: u64 = 0;
    // Build the baseline network
    let mut rng = StdRng::seed_from_u64(SEED);
    let baseline_net = build_test_network(&network_config, &mut rng)?;
    let faulty_router = RouterId::from(2);
    let prefix = P::from(3);

    println!("Faulty Router: {}", faulty_router.fmt(&baseline_net));
    // Get the external peer of this faulty router
    let baseline_net_clone = baseline_net.clone();
    let (external, _) = baseline_net_clone
        .get_internal_router(faulty_router)
        .unwrap()
        .bgp
        .get_sessions()
        .iter()
        .filter(|s| *s.1 == BgpSessionType::EBgp)
        .next()
        .unwrap();
    println!("External router: {}", external.fmt(&baseline_net));

    let gns3_config = Gns3Config {
        router_templates: HashMap::from([(faulty_router, ("FRR-8.4.2", "frr:8.4.2"))]),
        pre_config: Some(PreConfig::Whitelist {
            prefix,
            router: faulty_router,
        }),
        post_config: None,
    };

    let run_config = RunConfig {
        early_return: true,
        max_sequences: 10,
        intervals_per_sequence: 3,
        steps_per_interval: 10,
        convergence_wait: Duration::from_secs(5),
        simulation_step: Duration::from_secs(1),
    };

    let config = ExperimentConfig {
        seed: SEED,
        runs: 64,
        network_config,
        gns3_config,
        run_config,
        monitoring_prefixes: (0..3).map(P::from).collect(),
        monitoring_mrai: Some(5),
        external_prefixes: (0..4).map(P::from).collect(),
    };

    let sacrificial_prefix: HashSet<_> = config
        .external_prefixes
        .clone()
        .into_iter()
        .collect::<HashSet<_>>()
        .difference(&config.monitoring_prefixes)
        .copied()
        .collect();
    assert!(
        sacrificial_prefix.len() == 1 && *sacrificial_prefix.iter().next().unwrap() == prefix,
        "There should be only one sacrificial prefix and it should be {}",
        prefix
    );

    perform_experiment(
        8,
        baseline_net,
        &config,
        &experiments_path("pl_bug"),
        if GROUND_TRUTH {
            Some(Arc::new(check_router(faulty_router, *external)))
        } else {
            None
        },
    )?;

    Ok(())
}

fn check_router(
    router: RouterId,
    external: RouterId,
) -> impl Fn(&Run, &Recording<P>, (f64, f64)) -> Option<Vec<BugReport>> {
    move |_, recording, _| pl_ground_truth(recording, &router, &external)
}

fn pl_ground_truth(
    recording: &Recording<P>,
    router: &RouterId,
    external: &RouterId,
) -> Option<Vec<BugReport>> {
    // We say a bug has been triggered every time we see a non-whitelisted prefix going through
    // the connections
    let reports: Vec<BugReport> = recording
        .0
        .iter()
        .flat_map(move |(current_prefix, plane)| {
            let current_prefix = *current_prefix;
            plane.get(router).into_iter().flat_map(move |r| {
                r.iter().filter_map(move |e| match &e.0 {
                    bgpsim::event::Event::Bgp { p, src, dst, .. } => {
                        if src == router && dst != external {
                            Some((p, current_prefix))
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
            })
        })
        .map(|(t, current_prefix)| BugReport {
            timestamp: t.into_inner(),
            router: *router,
            prefix: current_prefix,
        })
        .collect();

    if reports.is_empty() {
        None
    } else {
        Some(reports)
    }
}
