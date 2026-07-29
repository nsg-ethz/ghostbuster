use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
    fs::OpenOptions,
    ops::Range,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, LazyLock, Mutex,
    },
    time::Instant,
};

use bgpsim::{
    bgp::BgpSessionType,
    builder::*,
    event::{BasicEventQueue, Event, EventQueue},
    network::Network,
    prelude::{InteractiveNetwork, NetworkFormatter},
    route_map::{
        RouteMap, RouteMapBuilder, RouteMapDirection, RouteMapFlow, RouteMapMatch, RouteMapState,
    },
    topology_zoo::TopologyZoo,
    types::{AsId, RouterId, SinglePrefix},
};
use clap::Parser;
use failure_extraction::{
    distance::NetworkDistance,
    monitoring::MonitoringError,
    reordering_monitoring::{ProcessWithdraws, ReorderingMonitor},
    session_queue::reordering_queue::{
        Bug, Failure, FailureLocation, ReorderingQueue, ReorderingSessionQueueInit,
    },
};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use itertools::{iproduct, Itertools};
use ordered_float::NotNan;
use rand::{rngs::StdRng, seq::SliceRandom, Rng, SeedableRng};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

type P = SinglePrefix;

// Resolved from the environment rather than from `CARGO_MANIFEST_DIR`, which would bake the
// directory the binary happened to be built in into the binary itself.
pub static RESULTS_FOLDER: LazyLock<PathBuf> =
    LazyLock::new(failure_extraction::config::results_path);
static RAW_FOLDER: LazyLock<PathBuf> = LazyLock::new(|| {
    let mut raw_folder = RESULTS_FOLDER.to_path_buf();
    raw_folder.push("raw");
    raw_folder
});
static RAW_FILE_ID: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, clap::Parser)]
struct EvalArguments {
    #[arg(long, short='t', value_parser = parse_topo)]
    topology: Option<Vec<TopologyZoo>>,
    #[arg(long, short = 'E')]
    num_external_networks: Option<Vec<usize>>,
    #[arg(long, short = 'r')]
    num_route_reflectors: Option<Vec<usize>>,
    #[arg(long, short = 'e')]
    num_external_events: Option<Vec<usize>>,
    #[arg(long, short = 'f')]
    with_failure: Option<Vec<bool>>,
    #[arg(long, short = 'm')]
    with_monitor_speaker: Option<Vec<bool>>,
    #[arg(long, short = 'R')]
    with_reordering: Option<Vec<bool>>,
    #[arg(long, short = 'd')]
    with_debouncing: Option<Vec<bool>>,
    #[arg(long, short = 'w', value_parser)]
    process_withdraws: Option<Vec<ProcessWithdraws>>,
    #[arg(long, short = 's')]
    seed: Option<Vec<u64>>,
}

fn parse_topo(s: &str) -> Result<TopologyZoo, String> {
    TopologyZoo::topologies_increasing_nodes()
        .iter()
        .find(|t| t.to_string().to_lowercase() == s.to_lowercase())
        .copied()
        .ok_or_else(|| format!("Unknown topology name: {s}"))
}

fn main() {
    let logger = pretty_env_logger::formatted_builder()
        .parse_env("RUST_LOG")
        .build();
    let multi_progress = MultiProgress::new();
    indicatif_log_bridge::LogWrapper::new(multi_progress.clone(), logger)
        .try_init()
        .unwrap();

    let args = EvalArguments::parse();

    println!("Preparing all scenarios");
    let topologies = args.topology.unwrap_or_else(|| {
        TopologyZoo::topologies_increasing_nodes()
            .iter()
            .filter(|topo| topo.num_internals() > 6)
            .filter(|topo| topo.num_internals() < 300)
            .filter(|t| {
                let n: Network<P, BasicEventQueue<P>> = t.build(Default::default());
                let mut g = n.get_topology().clone();
                for e in n.external_indices() {
                    g.remove_node(e);
                }
                let g = petgraph::graph::Graph::from(g);
                petgraph::algo::connected_components(&g) == 1
            })
            .copied()
            .collect::<Vec<_>>()
    });
    let mut scenarios = iproduct!(
        topologies,
        args.num_external_networks.unwrap_or([10, 50].into()),
        args.num_route_reflectors.unwrap_or([0, 1, 3, 5].into()),
        args.num_external_events
            .unwrap_or([1, 3, 5, 10, 30, 50, 100].into()),
        args.with_failure.unwrap_or([false, true].into()),
        args.with_monitor_speaker.unwrap_or([false, true].into()),
        args.with_reordering.unwrap_or([false, true].into()),
        args.with_debouncing.unwrap_or([false, true].into()),
        args.process_withdraws.unwrap_or(
            [
                ProcessWithdraws::Always,
                ProcessWithdraws::Never,
                ProcessWithdraws::Adaptive
            ]
            .into()
        ),
        args.seed.unwrap_or(Vec::from_iter(1..=10))
    )
    .filter(
        |(_, _, _, _, with_failure, with_monitor_speaker, _, _, process_withdraws, _)| {
            *with_monitor_speaker
                || (*with_failure && matches!(process_withdraws, ProcessWithdraws::Adaptive))
        },
    )
    .map(
        |(
            topology,
            num_externals,
            num_route_reflectors,
            num_external_events,
            with_failure,
            with_monitor_speaker,
            with_reordering,
            with_debouncing,
            process_withdraws,
            seed,
        )| {
            MeasurementInput {
                commit_hash: env!("GIT_HASH").to_string(),
                seed,
                topology,
                num_nodes: topology.num_internals(),
                num_edges: topology.num_internal_edges(),
                num_externals,
                num_route_reflectors,
                num_external_events,
                with_failure,
                with_monitor_speaker,
                with_reordering,
                with_debouncing,
                process_withdraws,
            }
        },
    )
    .collect_vec();
    scenarios.shuffle(&mut rand::thread_rng());
    run_experiments(scenarios, multi_progress)
}

fn run_experiments(scenarios: Vec<MeasurementInput>, multi_progress: MultiProgress) {
    // create the output directory and file
    let file_prefix: &'static str = Box::leak(Box::new(format!(
        "reordering_{}",
        time::OffsetDateTime::now_local()
            .unwrap()
            .format(&time::macros::format_description!(
                "[year]-[month]-[day]_[hour]:[minute]:[second]"
            ))
            .unwrap()
    )))
    .as_str();

    let mut result_csv_file = RESULTS_FOLDER.to_path_buf();
    std::fs::create_dir_all(&result_csv_file).expect("creating the results directory");
    result_csv_file.push(format!("{file_prefix}.csv.gz"));
    let writer = Arc::new(Mutex::new(csv::Writer::from_writer(
        flate2::write::GzEncoder::new(
            std::fs::File::create_new(&result_csv_file).expect("Creating the csv file"),
            flate2::Compression::best(),
        ),
    )));
    result_csv_file.pop();
    result_csv_file.push(format!("{file_prefix}_verif_time.csv.gz"));
    let verif_writer = Arc::new(Mutex::new(csv::Writer::from_writer(
        flate2::write::GzEncoder::new(
            std::fs::File::create_new(&result_csv_file).expect("Creating the csv file"),
            flate2::Compression::best(),
        ),
    )));

    let progress = multi_progress.add(
        ProgressBar::new(scenarios.len() as u64).with_style(
            ProgressStyle::default_bar()
                .template(
                    "{elapsed_precise} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {eta} {msg}",
                )
                .unwrap()
                .progress_chars("=> "),
        ),
    );

    scenarios
        // .into_iter()
        // .for_each(|input| {
        .into_par_iter()
        .for_each_with(
            (writer.clone(), verif_writer.clone()),
            |(writer, verif_writer), input| {
                progress.inc(1);
                let setup = match input.setup_experiment() {
                    Ok(setup) => setup,
                    Err(e) => {
                        log::error!("Error while setting up the experiment:\n{e}");
                        return;
                    }
                };

                let results = match setup.run(file_prefix, verif_writer) {
                    Ok(result) => result,
                    Err(e) => {
                        log::error!("Error running the experiment:\n{e}");
                        return;
                    }
                };

                match writer
                    .lock()
                    .context("Cannot lock the reordering csv writer")
                    .and_then(|mut w| {
                        w.serialize(&results)
                            .context("Writing the reordering result")?;
                        w.flush().context("Flushing the reordering result writer")?;
                        Ok(())
                    }) {
                    Ok(()) => {}
                    Err(e) => {
                        log::error!("Error writing the results:\n{e}");
                        return;
                    }
                }
            },
        );
}

const PROB_DESTRUCTIVE_FAILURE: f64 = 0.5;
const PROB_TRANSFORMATIVE_FAILURE: f64 = 0.25;
const PROB_EGRESS_BUG: f64 = 0.66;
const PROB_TRANSFORM_LP: f64 = 0.5;
const PROB_DROP_WITHDRAW: f64 = 0.5;
const PROB_UPDATE_TO_WITHDRAW: f64 = 0.5;
const PROB_BUG_APPLY_RANGE: Range<f64> = 0.1..1.0;
const BUG_POSSIBLE_LOCAL_PREFS: [u32; 7] = [20, 50, 80, 100, 150, 200, 250];
const BUG_POSSIBLE_COMMUNITIES: [u32; 4] = [501, 502, 503, 500];
const BUG_SPECIAL_ROUTE_MAP_ORDER: i16 = 0;

const LINK_WEIGHT_RANGE: (usize, usize) = (5, 20);
// Equal probability to have any of the peer types.
const GAO_REXFORD_PEER_TYPE_PROBS: (f64, f64) = (0.333, 0.666);
// Parameters that control the event generation
const AS_PATH_LEN_RANGE: Range<usize> = 1..4;
const STEPS_AFTER_EVENT_RANGE: Range<usize> = 2..20;

impl MeasurementInput {
    fn setup_experiment(&self) -> Result<Experiment, String> {
        let mut rng = StdRng::seed_from_u64(self.seed);
        let mut network: Network<P, BasicEventQueue<P>> = self.topology.build(Default::default());

        // configure the thing
        let ConfigureNetworkResult {
            internals,
            reflectors,
            borders,
            externals,
            monitor,
            roles,
        } = configure_network(
            &mut rng,
            &mut network,
            self.num_externals,
            self.num_route_reflectors,
            self.with_monitor_speaker,
        )
        .context("configure_network")?;

        // build the set of events and already emulate it
        let failure = self
            .gen_failure(
                &mut rng,
                &network,
                &internals,
                &borders,
                &reflectors,
                &externals,
            )
            .context("MeasurementInput::gen_failure")?;

        let (setup, external_events) = self
            .gen_event_sequence(&mut rng, &network, &externals)
            .context("MeasurementInput::gen_event_sequence")?;

        // setup the network route-map for the failure (if necessary)
        if let Some(failure) = &failure {
            failure_setup_route_map_in_healthy(failure, &mut network)
                .context("failure_setup_route_map_in_healthy")?;
        }

        // apply the setup
        for e in setup.iter() {
            e.apply(&mut network).context("Apply the setup")?;
        }

        Ok(Experiment {
            inputs: self.clone(),
            network,
            setup,
            external_events,
            failure,
            traces: Default::default(),
            roles,
            monitor,
        })
    }

    fn gen_failure(
        &self,
        rng: &mut StdRng,
        network: &Network<P, BasicEventQueue<P>>,
        internals: &[RouterId],
        borders: &[RouterId],
        reflectors: &[RouterId],
        externals: &[RouterId],
    ) -> Result<Option<Failure>, String> {
        if !self.with_failure {
            return Ok(None);
        }

        let egress_bug = rng.gen_bool(PROB_EGRESS_BUG);

        // Get an interesting router, given the kind of egress bug.
        let r_id = if egress_bug {
            // egress bugs should only ever be applied to border routers and route reflectors.
            let choices = borders
                .iter()
                .chain(reflectors)
                .copied()
                .collect::<Vec<_>>();
            *choices.choose(rng).unwrap()
        } else {
            // ingress bugs can be any internal router
            *internals.choose(rng).unwrap()
        };

        let r = network
            .get_internal_router(r_id)
            .context("Network::get_internal_router")?;

        let neighbors = r.bgp.get_sessions().keys().copied().sorted();
        let neighbors = if egress_bug {
            neighbors.collect::<Vec<_>>()
        } else {
            // for ingress bugs, only take externals, borders and reflector neighbors.
            neighbors
                .filter(|n| borders.contains(n) || reflectors.contains(n) || externals.contains(n))
                .collect::<Vec<_>>()
        };
        let neighbor = *neighbors.choose(rng).unwrap();

        let bug = Self::gen_bug(rng, &BUG_POSSIBLE_LOCAL_PREFS, &BUG_POSSIBLE_COMMUNITIES);
        let location = if egress_bug {
            FailureLocation::Egress
        } else {
            FailureLocation::Ingress
        };

        Ok(Some(Failure {
            router: r_id,
            neighbor,
            location,
            bug,
        }))
    }

    fn gen_bug(rng: &mut StdRng, lp_values: &[u32], communities: &[u32]) -> Bug {
        let p = rng.gen_range(PROB_BUG_APPLY_RANGE);
        let x = rng.gen_range(0.0..1.0);
        if x < PROB_DESTRUCTIVE_FAILURE {
            // destructive
            if rng.gen_bool(PROB_DROP_WITHDRAW) {
                Bug::DropWithdraws { p }
            } else {
                Bug::DropUpdates { p }
            }
        } else if x - PROB_DESTRUCTIVE_FAILURE < PROB_TRANSFORMATIVE_FAILURE {
            // transformative
            if rng.gen_bool(PROB_TRANSFORM_LP) {
                let new_val = *lp_values.choose(rng).unwrap();
                Bug::TransformLocalPref { p, new_val }
            } else {
                let swap = *communities.choose(rng).unwrap();
                Bug::TransformCommunity { p, swap }
            }
        } else {
            // constructive
            if rng.gen_bool(PROB_UPDATE_TO_WITHDRAW) {
                Bug::UpdateToWithdraw { p }
            } else {
                Bug::WithdrawToUpdate { p }
            }
        }
    }

    fn gen_event_sequence(
        &self,
        rng: &mut StdRng,
        network: &Network<P, BasicEventQueue<P>>,
        externals: &[RouterId],
    ) -> Result<(Vec<ExternalEvent>, Vec<ExternalEvent>), String> {
        let mut already_advertised = HashSet::new();
        let mut random_event = || -> Result<_, String> {
            let router = *externals.choose(rng).unwrap();
            let update = if already_advertised.contains(&router) {
                rng.gen_bool(0.5)
            } else {
                true
            };
            let route = if update {
                already_advertised.insert(router);
                let as_path_len = rng.gen_range(AS_PATH_LEN_RANGE);
                let as_path = std::iter::repeat(
                    network
                        .get_external_router(router)
                        .context("Network::get_external_router")?
                        .as_id(),
                )
                .take(as_path_len)
                .collect::<Vec<_>>();
                Some(as_path)
            } else {
                already_advertised.remove(&router);
                None
            };
            Ok(ExternalEvent {
                source: router,
                route,
                num_steps_after: rng.gen_range(STEPS_AFTER_EVENT_RANGE),
            })
        };

        let setup = (0..self.num_externals)
            .map(|_| random_event())
            .collect::<Result<Vec<_>, _>>()?;
        let sequence = (0..self.num_external_events)
            .map(|_| random_event())
            .collect::<Result<Vec<_>, _>>()?;
        Ok((setup, sequence))
    }
}

#[derive(Serialize, Deserialize)]
pub struct Experiment {
    pub inputs: MeasurementInput,
    pub network: Network<P, BasicEventQueue<P>>,
    pub setup: Vec<ExternalEvent>,
    pub external_events: Vec<ExternalEvent>,
    pub failure: Option<Failure>,
    pub traces: HashMap<RouterId, Vec<Event<P, NotNan<f64>>>>,
    pub roles: HashMap<RouterId, String>,
    pub monitor: Option<RouterId>,
}

impl std::fmt::Debug for Experiment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Experiment")
            .field("inputs", &self.inputs)
            .field("external_events", &self.external_events)
            .field("failures", &self.failure)
            .finish()
    }
}

impl Experiment {
    fn run<W: std::io::Write>(
        mut self,
        file_prefix: &str,
        verif_writer: &Arc<Mutex<csv::Writer<W>>>,
    ) -> Result<Measurement, String> {
        let mut result = self.empty_measurement();

        let mut healthy = self.network.clone();

        // First, create the device under test
        let mut queue = ReorderingQueue::new(ReorderingSessionQueueInit::new(self.inputs.seed));
        // fill the queue in a deterministic ordering
        for r in self.network.device_indices().sorted() {
            for n in self
                .network
                .get_device(r)
                .unwrap()
                .bgp_neighbors()
                .into_iter()
                .sorted()
            {
                queue.init_session(
                    r,
                    n,
                    self.inputs.with_reordering,
                    self.inputs.with_debouncing,
                    1.0,
                );
            }
        }
        let mut dut = healthy.clone();
        if let Some(failure) = self.failure.clone() {
            // enable the route map bug if necessary
            failure_enable_bug_in_route_map(&failure, &mut dut)
                .context("failure_enable_bug_in_route_map")?;
            queue.add_failure(failure);
        }
        let mut dut = dut.swap_queue(queue);
        dut.manual_simulation();

        // next, evaluate all events on the healthy network, and to the dut.
        for event in self.external_events.iter() {
            event.apply(&mut healthy).context("ExternalEvent::apply")?;
            event
                .apply_and_simulate_n(&mut dut)
                .context("ExternalEvent::apply_and_simulate_n")?;
        }

        // finally, ensure that the DUT has converged. If not, directly return that the measurement failed
        dut.set_msg_limit(Some(10000));
        if dut.simulate().is_err() {
            self.write_json(&mut result, file_prefix)
                .context("Experiment::finish")?;
            return Ok(result);
        }
        result.has_converged = true;

        // collect all traces into one.
        //
        // `all_measurement_traces` keys the traces by both endpoints of every BGP session, which
        // includes the external routers of eBGP sessions. Only internal routers are verified: a
        // ReorderingMonitor is built from an internal `Router`, so `verify_traces` (and
        // `reordering_eval_rerun`, which reads these traces back out of the experiment JSON) fails
        // with "cannot be an external router" if they are left in.
        let internal: HashSet<RouterId> = dut.internal_indices().collect();
        self.traces = dut.queue_mut().all_measurement_traces();
        self.traces.retain(|rid, _| internal.contains(rid));

        // get the number of failure triggers
        result.num_failures_triggered = if let Some(failure) = self.failure.as_ref() {
            failure_count_num_triggered(failure, &dut).context("failure_count_num_triggered")?
        } else {
            0
        };

        // compare the final state
        compare_bgp_state(&mut result, &healthy, &dut);
        result.blast_radius = healthy.distance(&dut);

        // write the results to json
        self.write_json(&mut result, file_prefix)
            .context("Experiment::finish")?;

        // verify all traces
        self.verify_traces(&mut result, verif_writer)
            .context("Experiment::verify_traces")?;

        Ok(result)
    }

    fn verify_traces<W: std::io::Write>(
        &self,
        result: &mut Measurement,
        verif_writer: &Arc<Mutex<csv::Writer<W>>>,
    ) -> Result<(), String> {
        // verify all traces
        for (rid, trace) in self.traces.clone() {
            if Some(rid) == self.monitor {
                // skip verifying the monitor
                continue;
            }

            let trace_len = trace.len();
            let bug_triggered = self
                .failure
                .as_ref()
                .map(|x| x.router == rid && result.num_failures_triggered > 0)
                .unwrap_or(false);
            let router = self
                .network
                .get_internal_router(rid)
                .context("Network::get_internal_router")?
                .clone();
            let arena = Default::default();
            let mut monitor = ReorderingMonitor::new(router, &arena);
            monitor
                .set_process_withdraw_mode(self.inputs.process_withdraws)
                .unwrap();
            let start = Instant::now();
            let (num_forks, sequence_ok, was_killed, max_heap_size) =
                test_trace(&self.network, &mut monitor, trace, None);
            let elapsed = start.elapsed();

            // update the results
            if was_killed {
                result.num_monitors_killed += 1;
            } else {
                match (bug_triggered, sequence_ok) {
                    (false, true) => result.num_true_negatives += 1,
                    (false, false) => result.num_false_positives += 1,
                    (true, true) => result.num_false_negatives += 1,
                    (true, false) => result.num_true_positives += 1,
                }
            }

            result.total_verif_time += elapsed.as_secs_f64();
            result.total_num_events += trace_len;
            result.total_max_num_forks = result.total_max_num_forks.max(num_forks);

            // serialize the performance file
            let mut w = verif_writer.lock().context("Locking the CSV writer")?;
            w.serialize(PerformanceMeasurement {
                commit_hash: result.commit_hash.clone(),
                raw_json: result.raw_json.clone(),
                seed: result.seed,
                topology: result.topology,
                num_nodes: result.num_nodes,
                num_edges: result.num_edges,
                num_externals: result.num_externals,
                num_route_reflectors: result.num_route_reflectors,
                num_external_events: result.num_external_events,
                with_failure: result.with_failure,
                with_monitor_speaker: result.with_monitor_speaker,
                with_reordering: result.with_reordering,
                with_debouncing: result.with_debouncing,
                process_withdraws: result.process_withdraws,
                router: rid.index(),
                router_name: rid.fmt(&self.network),
                router_role: self
                    .roles
                    .get(&rid)
                    .map(|x| x.as_str())
                    .unwrap_or("?")
                    .to_string(),
                num_events: trace_len,
                verif_time: elapsed.as_secs_f64(),
                max_num_forks: num_forks,
                max_heap_size,
                was_killed,
                bug_triggered,
                found_bug: !sequence_ok,
            })
            .context("Writing the performance result")?;
            w.flush()
                .context("Flushing the performance result writer")?;
        }

        Ok(())
    }

    fn empty_measurement(&self) -> Measurement {
        Measurement {
            commit_hash: self.inputs.commit_hash.clone(),
            raw_json: String::new(),
            seed: self.inputs.seed,
            topology: self.inputs.topology,
            num_nodes: self.inputs.num_nodes,
            num_edges: self.inputs.num_edges,
            num_externals: self.inputs.num_externals,
            num_route_reflectors: self.inputs.num_route_reflectors,
            num_external_events: self.inputs.num_external_events,
            with_failure: self.inputs.with_failure,
            with_monitor_speaker: self.inputs.with_monitor_speaker,
            with_reordering: self.inputs.with_reordering,
            with_debouncing: self.inputs.with_debouncing,
            process_withdraws: self.inputs.process_withdraws,
            bug_class: self.failure.map(|x| bug_class(&x.bug)),
            bug_affected_msg: self.failure.map(|x| bug_affected_msg(&x.bug)),
            bug_args: self.failure.map(|x| bug_args(&x.bug)),
            bug_pipeline: self.failure.map(|x| location_pipeline(&x.location)),
            bug_router: self.failure.map(|x| x.router.fmt(&self.network)),
            bug_neighbor: self.failure.map(|x| x.neighbor.fmt(&self.network)),
            bug_router_roles: self.failure.map(|x| self.roles[&x.router].clone()),
            bug_neighbor_roles: self.failure.map(|x| self.roles[&x.neighbor].clone()),
            bug_probability: self.failure.map(|x| bug_probability(&x.bug)),
            has_converged: false,
            rib_in_mismatch: false,
            rib_in_processed_mismatch: false,
            rib_selected_mismatch: false,
            rib_out_mismatch: false,
            blast_radius: 0,
            num_true_positives: 0,
            num_true_negatives: 0,
            num_false_positives: 0,
            num_false_negatives: 0,
            num_failures_triggered: 0,
            total_num_events: 0,
            total_verif_time: 0.0,
            total_max_num_forks: 1,
            num_monitors_killed: 0,
        }
    }

    fn write_json(&self, result: &mut Measurement, file_prefix: &str) -> Result<(), String> {
        if cfg!(feature = "skip_export_experiments") {
            // do not export
            return Ok(());
        }
        let mut raw_file = RAW_FOLDER.to_path_buf();
        std::fs::create_dir_all(&raw_file).context("std::fs::create_dir_all")?;
        let idx = RAW_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let filename = format!("{file_prefix}_raw_{idx}.json.gz");
        raw_file.push(&filename);
        result.raw_json = format!("./raw/{filename}");
        serde_json::to_writer(
            flate2::write::GzEncoder::new(
                OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(raw_file)
                    .context("open raw file")?,
                flate2::Compression::new(8),
            ),
            &self,
        )
        .context("serde_json::to_writer")?;
        Ok(())
    }
}

pub fn test_trace<Q: EventQueue<P>>(
    net: &Network<P, Q>,
    monitor: &mut ReorderingMonitor,
    trace: Vec<Event<P, NotNan<f64>>>,
    assumed_processing_time: Option<f64>,
) -> (usize, bool, bool, usize) {
    let mut max_forks = 1;
    if log::log_enabled!(log::Level::Info) {
        log::info!("Testing trace for {}", monitor.router_id().fmt(net));
        for event in &trace {
            log::info!("  [{}] {}", event.priority(), event.fmt(net));
        }
    }
    let mut max_heap = 0;
    for event in trace {
        let result = if let Some(delay) = assumed_processing_time {
            monitor
                .assert_messages_processed_before(*event.priority() - delay)
                .and_then(|_| monitor.process_message(event))
        } else {
            monitor.process_message(event)
        };
        max_heap = max_heap.max(monitor.heap_size());
        match result {
            Ok(()) => {}
            Err(MonitoringError::Killed {
                router,
                neighbor,
                num_forks,
                ..
            }) => {
                log::error!(
                    "Killed monitor {} -> {} after creating {num_forks} forks",
                    router.fmt(net),
                    neighbor.fmt(net)
                );
                return (num_forks, true, true, max_heap);
            }
            Err(e) => {
                log::warn!("Error: {}", e.fmt(net));
                return (max_forks, false, false, max_heap);
            }
        }
        max_forks = max_forks.max(monitor.max_num_active_forks());
    }
    if let Err(e) = monitor.final_check() {
        log::warn!("final check: {}", e.fmt(net));
        return (max_forks, false, false, max_heap);
    }
    (max_forks, true, false, max_heap)
}

fn compare_bgp_state<Q1, Q2>(
    result: &mut Measurement,
    healthy: &Network<P, Q1>,
    dut: &Network<P, Q2>,
) {
    for router in healthy
        .internal_indices()
        .chain(dut.internal_indices())
        .unique()
    {
        let healthy_rib_in = healthy
            .get_internal_router(router)
            .ok()
            .and_then(|r| r.bgp.get_rib_in().0.as_ref())
            .into_iter()
            .flatten()
            .sorted_by_key(|(_, rib)| rib.from_id)
            .map(|(_, rib)| &rib.route)
            .collect_vec();
        let dut_rib_in = dut
            .get_internal_router(router)
            .ok()
            .and_then(|r| r.bgp.get_rib_in().0.as_ref())
            .into_iter()
            .flatten()
            .sorted_by_key(|(_, rib)| rib.from_id)
            .map(|(_, rib)| &rib.route)
            .collect_vec();
        if healthy_rib_in != dut_rib_in {
            result.rib_in_mismatch = true;
        }

        let healthy_rib_in_processed = healthy
            .get_internal_router(router)
            .ok()
            .and_then(|r| r.bgp.get_processed_rib_in().0)
            .unwrap_or_default()
            .into_iter()
            .sorted_by_key(|(rib, _)| rib.from_id)
            .map(|(rib, _)| rib.route)
            .collect_vec();
        let dut_rib_in_processed = dut
            .get_internal_router(router)
            .ok()
            .and_then(|r| r.bgp.get_processed_rib_in().0)
            .unwrap_or_default()
            .into_iter()
            .sorted_by_key(|(rib, _)| rib.from_id)
            .map(|(rib, _)| rib.route)
            .collect_vec();
        if healthy_rib_in_processed != dut_rib_in_processed {
            result.rib_in_processed_mismatch = true;
        }

        let healthy_rib = healthy
            .get_internal_router(router)
            .ok()
            .and_then(|x| x.bgp.get_rib().0.as_ref())
            .map(|x| &x.route);
        let dut_rib = dut
            .get_internal_router(router)
            .ok()
            .and_then(|x| x.bgp.get_rib().0.as_ref())
            .map(|x| &x.route);
        if healthy_rib != dut_rib {
            result.rib_selected_mismatch = true;
        }

        let healthy_rib_out = healthy
            .get_internal_router(router)
            .ok()
            .and_then(|r| r.bgp.get_rib_out().0.as_ref())
            .into_iter()
            .flatten()
            .sorted_by_key(|(_, rib)| rib.from_id)
            .map(|(_, rib)| &rib.route)
            .collect_vec();
        let dut_rib_out = dut
            .get_internal_router(router)
            .ok()
            .and_then(|r| r.bgp.get_rib_out().0.as_ref())
            .into_iter()
            .flatten()
            .sorted_by_key(|(_, rib)| rib.from_id)
            .map(|(_, rib)| &rib.route)
            .collect_vec();
        if healthy_rib_out != dut_rib_out {
            result.rib_out_mismatch = true;
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExternalEvent {
    /// External router to advertise the route
    source: RouterId,
    /// Whether to advertise a route or not.
    route: Option<Vec<AsId>>,
    /// How many events to evaluated after triggering this event before executing the next event.
    num_steps_after: usize,
}

impl ExternalEvent {
    fn apply<Q: EventQueue<P>>(&self, net: &mut Network<P, Q>) -> Result<(), String> {
        match &self.route {
            Some(as_path) => net
                .advertise_external_route(self.source, SinglePrefix, as_path.clone(), None, None)
                .context("Network::advertise_external_route"),
            None => net
                .withdraw_external_route(self.source, SinglePrefix)
                .context("Network::withdraw_external_event"),
        }
    }
    fn apply_and_simulate_n<Q: EventQueue<P>>(
        &self,
        net: &mut Network<P, Q>,
    ) -> Result<(), String> {
        self.apply(net).context("ExternalEvent::apply")?;

        for _ in 0..self.num_steps_after {
            net.simulate_step().context("Network::simulate_step")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeasurementInput {
    commit_hash: String,
    seed: u64,
    // initial configuration
    topology: TopologyZoo,
    num_nodes: usize,
    num_edges: usize,
    num_externals: usize,
    num_route_reflectors: usize,
    // event configuration
    num_external_events: usize,
    with_failure: bool,
    with_monitor_speaker: bool,
    with_reordering: bool,
    with_debouncing: bool,
    pub process_withdraws: ProcessWithdraws,
}

#[derive(Debug, Clone, Serialize)]
struct Measurement {
    commit_hash: String,
    raw_json: String,
    seed: u64,
    // initial configuration
    topology: TopologyZoo,
    num_nodes: usize,
    num_edges: usize,
    num_externals: usize,
    num_route_reflectors: usize,
    // event configuration
    num_external_events: usize,
    with_failure: bool,
    with_monitor_speaker: bool,
    with_reordering: bool,
    with_debouncing: bool,
    process_withdraws: ProcessWithdraws,
    // bug details
    bug_class: Option<&'static str>,
    bug_affected_msg: Option<&'static str>,
    bug_args: Option<String>,
    bug_pipeline: Option<&'static str>,
    bug_router: Option<String>,
    bug_neighbor: Option<String>,
    bug_router_roles: Option<String>,
    bug_neighbor_roles: Option<String>,
    bug_probability: Option<f64>,
    // result
    /// Whether the network converged. If not, all values below are default.
    has_converged: bool,
    rib_in_mismatch: bool,
    rib_in_processed_mismatch: bool,
    rib_selected_mismatch: bool,
    rib_out_mismatch: bool,
    /// How large the blast radius was
    blast_radius: u32,
    /// How many routers did we correctly classify as having a bug.
    num_true_positives: usize,
    /// How many routers did we correctly classify as working as expected.
    num_true_negatives: usize,
    /// How many routers did we falsely accuse of having a bug, but they actually were fine.
    num_false_positives: usize,
    /// How many routers did we not detect that they had a bug
    num_false_negatives: usize,
    /// How many times was any of the failures triggered,
    num_failures_triggered: usize,
    /// All events on all routers added together.
    total_num_events: usize,
    /// The time to verify all events together, one router at a time.
    total_verif_time: f64,
    /// Maximum number of forks during the entire time.
    total_max_num_forks: usize,
    /// How many monitors needed to be killed.
    num_monitors_killed: usize,
}

#[derive(Debug, Clone, Serialize)]
struct PerformanceMeasurement {
    commit_hash: String,
    raw_json: String,
    seed: u64,
    // initial configuration
    topology: TopologyZoo,
    num_nodes: usize,
    num_edges: usize,
    num_externals: usize,
    num_route_reflectors: usize,
    process_withdraws: ProcessWithdraws,
    // event configuration
    num_external_events: usize,
    with_failure: bool,
    with_monitor_speaker: bool,
    with_reordering: bool,
    with_debouncing: bool,
    router: usize,
    router_name: String,
    router_role: String,
    /// All events on all routers added together.
    num_events: usize,
    /// The time to verify all events together, one router at a time.
    verif_time: f64,
    /// Maximum number of forks during the entire time.
    max_num_forks: usize,
    /// Maximum number of bytes allocated on the heap.
    max_heap_size: usize,
    /// Whether the monitor was killed.
    was_killed: bool,
    /// Whether a bug was triggered
    bug_triggered: bool,
    /// Whether the process found a bug
    found_bug: bool,
}

pub trait ResultExt<T, E> {
    fn context(self, context: &'static str) -> Result<T, String>;
}

impl<T, E: Display> ResultExt<T, E> for Result<T, E> {
    #[track_caller]
    fn context(self, context: &'static str) -> Result<T, String> {
        let location = std::panic::Location::caller();
        self.map_err(|e| format!("{context}: {e}\nin: {location}"))
    }
}

pub struct ConfigureNetworkResult {
    pub internals: Vec<RouterId>,
    pub reflectors: Vec<RouterId>,
    pub borders: Vec<RouterId>,
    pub externals: Vec<RouterId>,
    pub monitor: Option<RouterId>,
    pub roles: HashMap<RouterId, String>,
}

pub fn configure_network<Q: EventQueue<SinglePrefix>>(
    rng: &mut StdRng,
    network: &mut Network<SinglePrefix, Q>,
    num_externals: usize,
    num_route_reflectors: usize,
    with_monitor_speaker: bool,
) -> Result<ConfigureNetworkResult, String> {
    // delete all existing external routers and create theem again
    log::info!("delete existing external routers");
    for e in network.external_indices().detach() {
        network.remove_router(e).context("Network::remove_router")?;
    }

    log::info!("setup external routers");
    let internals = network.internal_indices().sorted().collect::<Vec<_>>();
    let mut externals = network
        .build_external_routers(k_random_nodes_seeded, (rng, num_externals))
        .context("NetworkBuilder::build_external_routers")?;
    externals.sort();

    log::info!("setting link weights");
    network
        .build_link_weights_seeded(rng, uniform_integer_link_weight_seeded, LINK_WEIGHT_RANGE)
        .context("NetworkBuilder::build_link_weights_seeded")?;
    log::info!("setup ebgp sessions");
    network
        .build_ebgp_sessions()
        .context("NetworkBuilder::build_ebgp_sessions")?;
    let reflectors: Vec<RouterId> = if num_route_reflectors == 0 {
        // full-mesh
        log::info!("setup iBGP full-mesh");
        network
            .build_ibgp_full_mesh()
            .context("NetworkBuilder::build_ibgp_full_mesh")?;
        Vec::new()
    } else {
        // route-reflection
        log::info!("setup iBGP route relfection");
        let reflectors = network
            .build_ibgp_route_reflection(k_random_nodes_seeded, (rng, num_route_reflectors))
            .context("NetworkBuilder::build_ibgp_route_reflection")?
            .into_iter()
            .sorted()
            .collect::<Vec<_>>();

        // add incoming route maps to tag the source of all routes
        for reflector in &reflectors {
            let sessions = network
                .get_internal_router(*reflector)
                .context("Route reflector does not exist")?
                .bgp
                .get_sessions()
                .clone();
            for (neighbor, _) in sessions {
                network
                    .set_bgp_route_map(
                        *reflector,
                        neighbor,
                        RouteMapDirection::Outgoing,
                        RouteMapBuilder::new()
                            .order(1)
                            .allow()
                            .set_community(reflector.index() as u32 + 1000u32)
                            .build(),
                    )
                    .context("Cannot tag route relfector")?;
            }
        }

        reflectors
    };

    log::info!("configure gao-rexford");
    network
        .build_gao_rexford_policies_seeded(
            rng,
            GaoRexfordPeerType::random_seeded,
            GAO_REXFORD_PEER_TYPE_PROBS,
        )
        .context("NetworkBuilder::build_gao_rexford_policies_seeded")?;

    // get all border routers
    log::info!("collect border routers");
    let borders = externals
        .iter()
        .map(|e| {
            *network
                .get_external_router(*e)
                .unwrap()
                .get_bgp_sessions()
                .iter()
                .next()
                .unwrap()
        })
        .sorted()
        .collect::<Vec<_>>();

    // add the monitor speaker
    log::info!("add the monitoring speaker");
    let monitor = if with_monitor_speaker {
        let mon = network.add_router("Monitor");
        log::info!("  adding the link");
        network
            .add_link(mon, internals[0])
            .context("Network::add_link")?;
        log::info!("  configuring iBGP");
        // add a BGP session from each internal router to this one
        let sessions = internals
            .iter()
            .map(|int| (*int, mon, Some(BgpSessionType::IBgpClient)));
        network
            .set_bgp_session_from(sessions)
            .context("Network::set_bgp_session")?;
        for int in &internals {
            network
                .set_bgp_route_map(
                    mon,
                    *int,
                    RouteMapDirection::Outgoing,
                    RouteMapBuilder::new().deny().order(10).build(),
                )
                .context("Network::set_bgp_route_map")?;
        }
        log::info!("  simulating");
        Some(mon)
    } else {
        None
    };

    log::info!("compute roles");
    let roles = network
        .device_indices()
        .map(|r| {
            (
                r,
                match (
                    borders.contains(&r),
                    reflectors.contains(&r),
                    externals.contains(&r),
                ) {
                    (_, _, true) => "external".to_string(),
                    (true, true, _) => "reflector+egress".to_string(),
                    (false, true, _) => "reflector".to_string(),
                    (true, false, _) => "egress".to_string(),
                    (false, false, _) => "internal".to_string(),
                },
            )
        })
        .collect();

    log::info!("done");

    Ok(ConfigureNetworkResult {
        internals,
        reflectors,
        borders,
        externals,
        monitor,
        roles,
    })
}

fn failure_setup_route_map_in_healthy<Q: EventQueue<P>>(
    failure: &Failure,
    network: &mut Network<P, Q>,
) -> Result<(), String> {
    match &failure.bug {
        Bug::DropUpdates { .. }
        | Bug::DropWithdraws { .. }
        | Bug::TransformCommunity { .. }
        | Bug::TransformLocalPref { .. } => Ok(()),
        Bug::WithdrawToUpdate { .. } | Bug::UpdateToWithdraw { .. } => {
            // the state of the route-map in the healthy network (with match probability of 1)
            let deny_probability = if matches!(failure.bug, Bug::WithdrawToUpdate { .. }) {
                // healthy behavior is to always deny, so 100% probability
                1.0
            } else {
                // healthy behavior is send an update, so 0% probability to drop
                0.0
            };

            network
                .set_bgp_route_map(
                    failure.router,
                    failure.neighbor,
                    match failure.location {
                        FailureLocation::Ingress => RouteMapDirection::Incoming,
                        FailureLocation::Egress => RouteMapDirection::Outgoing,
                    },
                    RouteMap {
                        order: BUG_SPECIAL_ROUTE_MAP_ORDER,
                        state: RouteMapState::Deny,
                        conds: vec![RouteMapMatch::Probabilistic {
                            p: NotNan::new(deny_probability).unwrap(),
                            num_matched: AtomicUsize::new(0),
                            num_not_matched: AtomicUsize::new(0),
                        }],
                        set: Vec::new(),
                        flow: RouteMapFlow::Continue,
                    },
                )
                .context("Network::set_bgp_route_map")
                .map(|_| ())
        }
    }
}

fn failure_enable_bug_in_route_map<Q: EventQueue<P>>(
    failure: &Failure,
    network: &mut Network<P, Q>,
) -> Result<(), String> {
    match &failure.bug {
        Bug::DropUpdates { .. }
        | Bug::DropWithdraws { .. }
        | Bug::TransformCommunity { .. }
        | Bug::TransformLocalPref { .. } => Ok(()),
        Bug::WithdrawToUpdate { p: match_prob } | Bug::UpdateToWithdraw { p: match_prob } => {
            // change the route-map to match only with the given probability.
            // safety: This is NOT safe, but that's precisely the point
            let route_map = unsafe {
                network
                    .get_internal_router_mut(failure.router)
                    .context("Router does not exist")?
                    .bgp
                    .get_route_map_mut(
                        failure.neighbor,
                        match failure.location {
                            FailureLocation::Ingress => RouteMapDirection::Incoming,
                            FailureLocation::Egress => RouteMapDirection::Outgoing,
                        },
                        BUG_SPECIAL_ROUTE_MAP_ORDER,
                    )
                    .ok_or("Failure was not already prepared!")?
            };

            let Some(RouteMapMatch::Probabilistic {
                p,
                num_matched,
                num_not_matched,
            }) = route_map.conds.get_mut(0)
            else {
                return Err(String::from(
                    "The failure route map was established incorrectly",
                ));
            };

            if matches!(failure.bug, Bug::WithdrawToUpdate { .. }) {
                // withdraw to update is when we do not deny with probability p.
                // Thus, the deny probability becomes 1 - p
                *p = NotNan::new(1.0).unwrap() - *match_prob;
            } else {
                // Update to withdraw is when we do deny with probability p.
                *p = NotNan::new(*match_prob).unwrap();
            }

            // reset the counters
            *num_matched = AtomicUsize::new(0);
            *num_not_matched = AtomicUsize::new(0);

            Ok(())
        }
    }
}

fn failure_count_num_triggered(
    failure: &Failure,
    network: &Network<P, ReorderingQueue>,
) -> Result<usize, String> {
    match failure.bug {
        Bug::DropUpdates { .. }
        | Bug::DropWithdraws { .. }
        | Bug::TransformCommunity { .. }
        | Bug::TransformLocalPref { .. } => {
            Ok(network.queue().num_failures_for_router(failure.router))
        }
        Bug::WithdrawToUpdate { .. } | Bug::UpdateToWithdraw { .. } => {
            let route_map = network
                .get_internal_router(failure.router)
                .context("Router does not exist")?
                .bgp
                .get_route_map(
                    failure.neighbor,
                    match failure.location {
                        FailureLocation::Ingress => RouteMapDirection::Incoming,
                        FailureLocation::Egress => RouteMapDirection::Outgoing,
                    },
                    BUG_SPECIAL_ROUTE_MAP_ORDER,
                )
                .ok_or("Failure was not already prepared!")?;

            let Some(RouteMapMatch::Probabilistic {
                num_matched,
                num_not_matched,
                ..
            }) = route_map.conds.get(0)
            else {
                return Err(String::from(
                    "The failure route map was established incorrectly",
                ));
            };

            let counter = if matches!(failure.bug, Bug::WithdrawToUpdate { .. }) {
                // in Withdraw to update, the bug happens if we do not deny, meaning we do not match
                num_not_matched
            } else {
                // in update to withdraw, the bug happens if we do deny, meaning we do match
                num_matched
            };

            Ok(counter.load(std::sync::atomic::Ordering::Relaxed))
        }
    }
}

fn bug_class(bug: &Bug) -> &'static str {
    match bug {
        Bug::DropUpdates { .. } | Bug::DropWithdraws { .. } => "destructive",
        Bug::TransformCommunity { .. } | Bug::TransformLocalPref { .. } => "transformative",
        Bug::WithdrawToUpdate { .. } | Bug::UpdateToWithdraw { .. } => "swap_type",
    }
}
fn bug_affected_msg(bug: &Bug) -> &'static str {
    match bug {
        Bug::DropWithdraws { .. } | Bug::WithdrawToUpdate { .. } => "withdraw",
        Bug::DropUpdates { .. }
        | Bug::TransformCommunity { .. }
        | Bug::TransformLocalPref { .. }
        | Bug::UpdateToWithdraw { .. } => "update",
    }
}
fn bug_args(bug: &Bug) -> String {
    match bug {
        Bug::DropUpdates { .. } => String::new(),
        Bug::DropWithdraws { .. } => String::new(),
        Bug::TransformCommunity { swap, .. } => format!("swap {swap}"),
        Bug::TransformLocalPref { new_val, .. } => format!("set {new_val}"),
        Bug::WithdrawToUpdate { .. } => String::new(),
        Bug::UpdateToWithdraw { .. } => String::new(),
    }
}
fn bug_probability(bug: &Bug) -> f64 {
    match bug {
        Bug::DropUpdates { p, .. }
        | Bug::DropWithdraws { p, .. }
        | Bug::TransformCommunity { p, .. }
        | Bug::TransformLocalPref { p, .. }
        | Bug::WithdrawToUpdate { p, .. }
        | Bug::UpdateToWithdraw { p, .. } => *p,
    }
}
fn location_pipeline(l: &FailureLocation) -> &'static str {
    match l {
        FailureLocation::Ingress => "ingress",
        FailureLocation::Egress => "egress",
    }
}
