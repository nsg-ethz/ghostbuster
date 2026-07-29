pub mod generator;
pub mod ground_truth;
pub mod reconfiguration;
pub mod run;
pub mod simulation;

use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;
use std::{
    collections::{HashMap, HashSet},
    fs,
    fs::OpenOptions,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use bgpsim::route_map::RouteMapBuilder;
use bgpsim::types::RouterId;
#[allow(dead_code)]
use bgpsim::{
    builder::{
        constant_link_weight, GaoRexfordPeerType,
    },
    event::BasicEventQueue,
    ospf::GlobalOspf as Ospf,
    prelude::{Network, NetworkBuilder, NetworkFormatter},
    topology_zoo::TopologyZoo,
    types::{NetworkError, SimplePrefix as P},
};
use bgpsim_gns3::Gns3NetworkError;
use csv::WriterBuilder;
use indicatif::{ProgressBar, ProgressStyle};
use itertools::Itertools;
use rand::{rngs::StdRng, SeedableRng};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use rayon::ThreadPoolBuilder;
use serde::Serialize;

type Q = BasicEventQueue<P>;

use log::{error, info, warn};
use run::{RunResult, SequenceResult};
use tracing::{span, Level};
use tracing_appender::non_blocking;
use tracing_log::LogTracer;
use tracing_subscriber::EnvFilter;

use crate::{
    recording::Recording,
    testbed::{
        generator::UniformEventGenerator,
        ground_truth::BugReport,
        reconfiguration::Gns3Config,
        run::{Run, RunConfig, SequenceResultSummary},
    },
};

/// Configuration for an experiment on this testbed
#[derive(Debug, Clone, Serialize)]
pub struct ExperimentConfig {
    pub seed: u64,

    /// The number of runs in this experiment
    pub runs: usize,
    /// Run specific configuration
    pub run_config: RunConfig,

    /// BgpSim specific configuration
    pub network_config: BaselineNetworkConfig,
    /// GNS3 specific configuration
    pub gns3_config: Gns3Config,

    /// The prefixes we generate events for
    pub external_prefixes: Vec<P>,
    /// The prefixes we compare our networks on and monitor for
    pub monitoring_prefixes: HashSet<P>,
    /// Optionally: inform our monitor of the MRAI value in order to be more aggressive in the reporting
    pub monitoring_mrai: Option<u16>,
}

/// All experiment results including the network topology
#[allow(dead_code)]
pub type ExperimentResults = (Network, HashMap<usize, RunResult>);

/// Perform the whole experiment sequentially
pub fn perform_experiment(
    n_threads: usize,
    baseline_net: Network,
    config: &ExperimentConfig,
    path: &Path,
    checker: Option<
        Arc<dyn Fn(&Run, &Recording<P>, (f64, f64)) -> Option<Vec<BugReport>> + Send + Sync>,
    >,
) -> Result<(), Gns3NetworkError> {
    LogTracer::init().expect("failed to set log tracer");

    ThreadPoolBuilder::new()
        .num_threads(n_threads)
        .build_global()
        .unwrap();

    // FAILSIM_RUNS caps the run count so the pipeline can be smoke-tested without waiting for a
    // full experiment.
    let runs = crate::config::runs_override().unwrap_or(config.runs).min(config.runs);
    if runs != config.runs {
        warn!("FAILSIM_RUNS is set: performing {runs} of {} runs", config.runs);
    }

    // We know how many sequences we will be looking at in total
    let bar = ProgressBar::new((runs * config.run_config.max_sequences) as u64).with_style(
        ProgressStyle::default_bar()
            .template("{elapsed_precise} {bar:40.cyan/blue} {pos}/{len} ({percent}%) {eta} {msg}")
            .unwrap()
            .progress_chars("##-"),
    );

    // Apply the pre-config modifications to the baseline network
    let baseline_net = if let Some(cfg) = &config.gns3_config.pre_config {
        cfg.apply(baseline_net)?
    } else {
        baseline_net
    };

    // Save the network and some info
    let experiment_dir = create_experiment_folder(&baseline_net, &config, &path);

    // Run experiments sequentially, save results immediately as we start finishing stuff
    let run_results: HashMap<usize, RunResult> = (0..runs)
        .into_par_iter()
        .filter_map(|number| {
            // Create a non-blocking file appender for this task
            let file_appender =
                tracing_appender::rolling::never(&experiment_dir, format!("run_{}.log", number));
            let (non_blocking, _guard) = non_blocking(file_appender);
            // Build and set the subscriber for this task
            let subscriber = tracing_subscriber::fmt()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_target(true)
                .with_env_filter(EnvFilter::new(
                    "trace,rustify=off,tracing=off,bgpkit_parser=off,bgpsim_gns3=info,reqwest=off,bgpsim=debug,failure_extraction::reordering_monitoring=warn,failure_extraction::monitoring=info",
                ))
                .finish();
            // Run everything inside a span in scope
            let run_span = span!(Level::TRACE, "run_task", run_number = number);
            run_span.in_scope(|| {
                // Set the subscriber for this scope
                let _default_guard = tracing::subscriber::set_default(subscriber);

                // Stagger run starts
                sleep(Duration::from_millis(rand::random::<u8>() as u64 * 100));
                info!("Starting run {}", number);
                bar.println(format!("Starting run {}", number));

                // Create a new event generator for this run
                // TODO: abstract away the generator, we should be able to pick one per experiment
                let event_generator = UniformEventGenerator::new(
                    StdRng::seed_from_u64(config.seed + number as u64),
                    baseline_net
                        .external_routers()
                        .sorted_by_key(|e| e.router_id())
                        .map(|e| (e.router_id(), e.as_id()))
                        .collect(),
                    config.external_prefixes.clone(),
                    // Only the prefix-list scenario is reconfigured while the run is in progress.
                    matches!(
                        config.gns3_config.pre_config,
                        Some(reconfiguration::PreConfig::Whitelist { .. })
                    ),
                );

                let arena = Default::default();
                match Run::new(
                    number,
                    &baseline_net,
                    config.clone(),
                    event_generator,
                    checker.clone(),
                    &arena,
                )
                .and_then(|mut run| run.execute(&bar))
                {
                    Ok(result) => {
                        // Save results immediately after successful completion
                        info!("Saving run results");
                        save_run_results(&experiment_dir, number, &result);
                        Some((number, result))
                    }
                    Err(e) => {
                        error!("Error in run {}: {:?}", number, e);
                        None
                    }
                }
            })
        })
        .collect();

    println!(
        "Completed {} successful runs out of {}",
        run_results.len(),
        runs
    );
    Ok(())
}

#[derive(Serialize)]
/// Information about the experiment that will be saved as a json in the experiment directory
struct ExperimentInfo {
    routers: HashMap<usize, String>,
    config: ExperimentConfig,
}

/// Create an experiment folder and save baseline network
fn create_experiment_folder(net: &Network, config: &ExperimentConfig, path: &Path) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let mut experiment_dir = PathBuf::from(path);
    experiment_dir.push(timestamp.to_string());

    fs::create_dir_all(&experiment_dir).expect("Could not create the necessary directories");

    // Save baseline network
    let mut net_path = experiment_dir.clone();
    net_path.push("baseline_network.json");
    let net_file = fs::File::create(&net_path).expect("Could not create network file");
    serde_json::to_writer(net_file, net).expect("Could not write network file");

    // Save additional information
    let info = ExperimentInfo {
        routers: net
            .devices()
            .map(|d| (d.router_id().index(), d.name().to_string()))
            .collect(),
        config: config.clone(),
    };
    let mut info_path = experiment_dir.clone();
    info_path.push("info.json");
    let info_file = fs::File::create(&info_path).expect("Could not create info file");
    serde_json::to_writer_pretty(info_file, &info).expect("Could not write info file");

    let mut results_path = experiment_dir.clone();
    results_path.push("results.csv");
    {
        // Create and write CSV header once per experiment directory
        let file = fs::File::create(&results_path).expect("Could not create a results file");
        let mut w = WriterBuilder::new().from_writer(file);
        w.write_record([
            "run",
            "prefix",
            "sequence",
            "simulated_events",
            "monitoring_errors",
            "selected_diff",
            "tables_diff",
            "bug_reports",
        ])
        .expect("Could not write CSV header");
        w.flush().expect("Could not flush CSV writer");
    }

    info!("Created experiment folder: {}", experiment_dir.display());
    info!("Saved baseline network to: {}", net_path.display());

    experiment_dir
}

/// Save results for a single run
fn save_run_results(experiment_dir: &Path, run_number: usize, results: &Vec<SequenceResult>) {
    let mut result_path = experiment_dir.to_path_buf();
    result_path.push(format!("run_{}.json", run_number));

    let result_file = fs::File::create(&result_path).expect("Could not open run file");
    serde_json::to_writer(result_file, results).expect("Could not write to run file");

    info!(
        "Saved run {} results to: {}",
        run_number,
        result_path.display()
    );

    // Append an overview of each sequence in the run to results.csv
    let mut csv_path = experiment_dir.to_path_buf();
    csv_path.push("results.csv");
    let file = OpenOptions::new()
        .append(true)
        .open(&csv_path)
        .expect("Could not open results.csv for appending");
    let mut rows: Vec<SequenceResultSummary> = Vec::new();
    for (sequence_number, sequence_result) in results.iter().enumerate() {
        for prefix in &sequence_result.monitored_prefixes {
            rows.push((run_number, sequence_number, prefix.clone(), sequence_result).into());
        }
    }
    // sort by run_number, then prefix, then sequence_number
    rows.sort_by_key(|r| (r.run, r.prefix, r.sequence));
    let mut w = WriterBuilder::new().has_headers(false).from_writer(file);
    for row in rows {
        w.serialize(row)
            .expect("Could not write sequence summary to CSV");
    }
    w.flush().expect("Could not flush CSV writer");
}

/// Load experiment results from a folder
pub fn load_experiment_results(
    dir: Option<&str>,
    run_numbers: Option<HashSet<usize>>,
    path: &Path,
) -> ExperimentResults {
    let experiment_dir = dir.map(|d| path.to_path_buf().join(d)).unwrap_or_else(|| {
        let most_recent = fs::read_dir(path)
            .expect("Could not read experiments directory")
            .sorted_by_key(|p| p.as_ref().unwrap().file_name())
            .rev()
            .next()
            .expect("No experiments")
            .unwrap()
            .path();

        warn!(
            "Getting most recent experiment at: {:?}",
            most_recent.file_name()
        );
        most_recent
    });

    let network: Network = {
        let path = experiment_dir.join("baseline_network.json");
        let file = fs::File::open(&path).expect("Could not open network file");
        serde_json::from_reader(file).expect("Could not read network file")
    };

    let run_files: HashMap<usize, PathBuf> = fs::read_dir(&experiment_dir)
        .expect("Could not read experiment directory")
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();

            let name = path.file_name()?.to_str()?;

            let idx = name
                .strip_prefix("run_")?
                .strip_suffix(".json")?
                .parse::<usize>()
                .ok()?;
            Some((idx, path))
        })
        .collect();

    info!("Run files detected: {:?}", run_files);

    let runs: HashMap<usize, RunResult> = run_files
        .into_par_iter() // Use parallel iterator
        .filter(|(idx, _)| {
            run_numbers
                .as_ref()
                .map_or(true, |numbers| numbers.contains(idx))
        })
        .filter_map(|(idx, path)| {
            info!("Reading file for run: {}", idx);
            // Use BufReader for buffered I/O
            let file = fs::File::open(path).ok()?;
            let reader = std::io::BufReader::with_capacity(8 * 1024 * 1024, file); // 8MB buffer
            let results = serde_json::from_reader(reader).ok()?;
            Some((idx, results))
        })
        .collect();

    info!(
        "Loaded experiment with {} runs from: {}",
        runs.len(),
        experiment_dir.display()
    );

    (network, runs)
}

#[derive(Debug, Clone, Serialize)]
pub struct BaselineNetworkConfig {
    /// The number of external routers in our network
    pub n_external: usize,
    /// Wether to use a monitor speaker or not
    pub with_monitor_speaker: bool,
}

/// Build a test network from TopologyZoo with the given configuration
pub fn build_test_network(
    config: &BaselineNetworkConfig,
    _rng: &mut StdRng,
) -> Result<Network, NetworkError> {
    let topology = TopologyZoo::Abilene;
    let mut net = topology.build::<P, Q, Ospf>(Q::default());
    net.set_msg_limit(Some(200));
    // Clear vestigial external routers
    net.external_indices()
        .detach()
        .for_each(|r| net.remove_router(r).unwrap());

    // External routers
    let border_routers = vec![
        RouterId::from(0),
        RouterId::from(2),
        RouterId::from(3),
        RouterId::from(8),
    ];
    net.build_external_routers(|_, _| border_routers, ())?;
    let rr =
        net.build_ibgp_route_reflection(|_, _| vec![RouterId::from(1), RouterId::from(2)], ())?;
    net.build_ebgp_sessions()?;
    net.build_link_weights(constant_link_weight, 10.0)?;
    let peer_types = vec![
        GaoRexfordPeerType::Customer,
        GaoRexfordPeerType::Peer,
        GaoRexfordPeerType::Peer,
        GaoRexfordPeerType::Provider,
    ];
    let gao_rexford = net.build_gao_rexford_policies(
        GaoRexfordPeerType::lookup,
        &net.external_indices().sorted().zip(peer_types).collect(),
    )?;

    // Add a monitor speaker if required:
    let internals = net.internal_indices().collect_vec();
    if config.with_monitor_speaker {
        let mon = net.add_router("Monitor");
        net.add_link(mon, internals[0])?;
        // add a BGP session from each internal router to this one
        for int in &internals {
            net.set_bgp_session(*int, mon, Some(bgpsim::bgp::BgpSessionType::IBgpClient))?;
            net.set_bgp_route_map(
                mon,
                *int,
                bgpsim::route_map::RouteMapDirection::Outgoing,
                RouteMapBuilder::new().deny().order(10).build(),
            )?;
        }
    }

    println!("Route reflectors: {}", rr.fmt(&net));
    println!("External routers: {}", gao_rexford.fmt(&net));

    Ok(net)
}
