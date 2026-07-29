use std::{
    collections::{HashMap},
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
        reconfiguration::{Gns3Config, PostConfig, PreConfig},
        run::{Run, RunConfig},
        BaselineNetworkConfig, ExperimentConfig,
    },
};
use log::error;
use rand::{
    rngs::StdRng,
    SeedableRng,
};

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

    println!("Faulty Router: {}", faulty_router.fmt(&baseline_net));
    // Get the external peer of this faulty router
    let (external, _) = baseline_net
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
        router_templates: if GROUND_TRUTH {
            HashMap::from([(faulty_router, ("FRR-LP-BUG", "frr:gns-alpine-lp-bug"))])
        } else {
            // The LP bug is fixed in upstream FRR, so the non-ground-truth baseline pins the
            // faulty router to a released image rather than falling through to `frr:latest`.
            HashMap::from([(faulty_router, ("FRR-10.2.1", "frr:10.2.1"))])
        },
        pre_config: Some(PreConfig::ToIncremental {
            router: faulty_router,
            external: *external,
        }),
        post_config: if GROUND_TRUTH {
            Some(vec![PostConfig::EnableLogging {
                router: faulty_router,
            }])
        } else {
            None
        },
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
        external_prefixes: (0..3).map(P::from).collect(),
    };

    perform_experiment(
        8,
        baseline_net,
        &config,
        &experiments_path("lp_bug"),
        if GROUND_TRUTH {
            Some(Arc::new(check_router(faulty_router)))
        } else {
            None
        },
    )?;

    Ok(())
}

fn check_router(
    router: RouterId,
) -> impl Fn(&Run, &Recording<P>, (f64, f64)) -> Option<Vec<BugReport>> {
    move |run, recording, interval| lp_ground_truth(run, recording, &router, interval)
}

fn lp_ground_truth(
    run: &Run,
    _: &Recording<P>,
    router: &RouterId,
    interval: (f64, f64),
) -> Option<Vec<BugReport>> {
    let log = run
        .gns3_net
        .get_log(*router)
        .inspect_err(|e| {
            error!("Failed to retrieve log: {}", e);
        })
        .ok()?
        .filter_substring("<<<:::BUG:::>>>");

    if log.messages.is_empty() {
        return None;
    }

    // Filter log according to the sequence interval.
    let sequence_log = log.filter_interval(interval.0, interval.1);
    sequence_log
        .messages
        .iter()
        .map(|bug_message| BugReport::maybe_from(bug_message, *router))
        .collect()
}
