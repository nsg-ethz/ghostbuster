use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bgpsim::prelude::NetworkFormatter;
use bgpsim::types::{RouterId, SimplePrefix as P};
use bgpsim_gns3::Gns3NetworkError;
use failure_extraction::recording::Recording;
use failure_extraction::testbed::ground_truth::BugReport;
use failure_extraction::testbed::reconfiguration::{Gns3Config, PostConfig};
use failure_extraction::testbed::run::{Run, RunConfig};
use log::error;
use rand::{rngs::StdRng, SeedableRng};

use failure_extraction::testbed::{
    build_test_network, perform_experiment, BaselineNetworkConfig, ExperimentConfig,
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

    let border_routers = vec![
        RouterId::from(0),
        RouterId::from(2),
        RouterId::from(3),
        RouterId::from(8),
    ];
    let faulty_router = RouterId::from(2);
    println!("Faulty Router: {}", faulty_router.fmt(&baseline_net));

    // Build the reconfiguration
    const MRAI: u16 = 5;
    // Fail a router
    let gns3_config = Gns3Config {
        router_templates: if GROUND_TRUTH {
            HashMap::from([(faulty_router, ("FRR-MRAI-BUG", "frr:gns-alpine-mrai-bug"))])
        } else {
            HashMap::from([(faulty_router, ("FRR-10.2.1", "frr:10.2.1"))])
        },
        pre_config: None,
        post_config: if GROUND_TRUTH {
            Some(vec![
                PostConfig::EnableLogging {
                    router: faulty_router,
                },
                PostConfig::SetMrai {
                    mrai: MRAI,
                    routers: border_routers.into_iter().collect(),
                },
            ])
        } else {
            Some(vec![PostConfig::SetMrai {
                mrai: MRAI,
                routers: border_routers.into_iter().collect(),
            }])
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
        &experiments_path("mrai_bug"),
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
