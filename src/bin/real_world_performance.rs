use std::{
    collections::{hash_map::Entry, HashMap},
    mem::drop,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::Instant,
};

use bgpkit_broker::BgpkitBroker;
use bgpkit_parser::{BgpElem, BgpkitParser};
use bgpsim::{
    event::{BasicEventQueue, Event, EventQueue},
    prelude::{InteractiveNetwork, Network, NetworkFormatter},
    topology_zoo::TopologyZoo,
    types::{AsId, RouterId, SinglePrefix},
};
use clap::Parser;
use crossbeam::channel;
use failure_extraction::{
    reordering_monitoring::{ProcessWithdraws, ReorderingMonitor},
    session_queue::reordering_queue::{ReorderingQueue, ReorderingSessionQueueInit},
};
use indicatif::{MultiProgress, ProgressBar, ProgressIterator, ProgressStyle};
use ipnet::IpNet;
use itertools::Itertools;
use logging_timer::{time, timer, Level};
use ordered_float::NotNan;
use rand::{rngs::StdRng, SeedableRng};
use serde::Serialize;

#[allow(dead_code)]
mod reordering_eval;

type P = SinglePrefix;
type QNet = Network<P, ReorderingQueue>;
type SNet = Network<P, BasicEventQueue<P>>;

#[derive(Debug, Clone, Parser)]
struct Config {
    #[arg(long, short = 'w', default_value = "1")]
    num_workers: usize,
    #[arg(long, short = 'n', default_value = "10")]
    min_num_events_to_verify: usize,
    #[arg(long, short = 'd', default_value = "1.0")]
    duration_between_events: f64,
    #[arg(long, short = 'p', default_value = "1.0")]
    assumed_processing_time: f64,
    #[arg(long, short='t', value_parser = parse_topo, default_value = "Geant2012")]
    topology: TopologyZoo,
    #[arg(long, short = 'S', default_value = "1")]
    seed: u64,
    #[arg(long, short = 's', default_value = "2025-12-01")]
    start: String,
    #[arg(long, short = 'e', default_value = "2026-01-01")]
    end: String,
    #[arg(long, short = 'c', default_value = "route-views.amsix")]
    collector: String,
    #[arg(long, default_value = "100")]
    num_externals: usize,
    #[arg(long, default_value = "3")]
    num_route_reflectors: usize,
    #[arg(long, action = clap::ArgAction::Set, default_value = "true")]
    with_monitor_speaker: bool,
    #[arg(long, action = clap::ArgAction::Set, default_value = "false")]
    with_debouncing: bool,
    #[arg(long, value_parser, default_value = "adaptive")]
    process_withdraws: ProcessWithdraws,
    #[arg(long = "all_routers", action = clap::ArgAction::Set, default_value = "false")]
    all_routers_analysis: bool,
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

    let config = Config::parse();

    let file_prefix: &'static str = Box::leak(Box::new(format!(
        "real_world_performance_{}",
        time::OffsetDateTime::now_local()
            .unwrap()
            .format(&time::macros::format_description!(
                "[year]-[month]-[day]_[hour]:[minute]:[second]"
            ))
            .unwrap()
    )))
    .as_str();
    let mut result_csv_file = reordering_eval::RESULTS_FOLDER.to_path_buf();
    std::fs::create_dir_all(&result_csv_file).expect("creating the results directory");
    result_csv_file.push(format!("{file_prefix}.csv.gz"));
    let writer = Arc::new(Mutex::new(csv::Writer::from_writer(
        flate2::write::GzEncoder::new(
            std::fs::File::create_new(&result_csv_file).expect("Creating the csv file"),
            flate2::Compression::best(),
        ),
    )));

    // prepare the network
    println!("prepare the network");
    let (empty_net, roles) = setup_network(&config).expect("Failed to setup the network");
    let empty_net = Box::leak(Box::new(empty_net)) as &_;
    let roles = Box::leak(Box::new(roles)) as &_;
    log::info!("network setup successfully");

    let (sender, receiver) = channel::bounded(config.num_workers * 10);

    let _config = config.clone();
    let _sender = sender.clone();
    let fetcher = std::thread::spawn(move || find_events_in(_config, _sender, multi_progress));

    let workers = (0..config.num_workers)
        .map(|_| {
            let config = config.clone();
            let receiver = receiver.clone();
            let writer = writer.clone();

            std::thread::spawn(move || worker(config, &empty_net, &roles, receiver, writer))
        })
        .collect::<Vec<_>>();

    let _ = (sender, receiver);

    match fetcher.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!("Error in the fetcher: {e}"),
        Err(_) => eprintln!("Failed to join the fetcher. Maybe it did panic?"),
    }
    for (i, worker) in workers.into_iter().enumerate() {
        match worker.join() {
            Ok(()) => {}
            Err(_) => eprintln!("Failed to join worker {i}. Maybe it did panic?"),
        }
    }
}

struct Task {
    prefix: IpNet,
    start_timestamp: f64,
    end_timestamp: f64,
    messages: Vec<AbstractMessage>,
}

impl Task {
    fn new(elem: BgpElem, peer_id: usize) -> Self {
        Self {
            prefix: elem.prefix.prefix,
            start_timestamp: elem.timestamp,
            end_timestamp: elem.timestamp,
            messages: vec![AbstractMessage::new(elem, peer_id)],
        }
    }

    fn update(
        &mut self,
        elem: BgpElem,
        peer_id: usize,
        duration_between_events: f64,
    ) -> Option<Task> {
        if self.end_timestamp + duration_between_events > elem.timestamp {
            self.end_timestamp = elem.timestamp;
            self.messages.push(AbstractMessage::new(elem, peer_id));
            None
        } else {
            let mut finished = std::mem::replace(self, Self::new(elem, peer_id));
            finished.messages = finished
                .messages
                .into_iter()
                .sorted_by_key(|m| (m.peer, NotNan::new(m.timestamp).unwrap()))
                .dedup()
                .sorted_by_key(|m| NotNan::new(m.timestamp).unwrap())
                .collect();
            Some(finished)
        }
    }
}

struct AbstractMessage {
    peer: usize,
    path: Option<Vec<AsId>>,
    timestamp: f64,
}

impl PartialEq for AbstractMessage {
    fn eq(&self, other: &Self) -> bool {
        self.peer == other.peer && self.path == other.path
    }
}

impl AbstractMessage {
    fn new(elem: BgpElem, peer_id: usize) -> Self {
        Self {
            peer: peer_id,
            path: elem
                .as_path
                .and_then(|x| x.to_u32_vec_opt(false))
                .map(|x| x.into_iter().map(AsId::from).collect()),
            timestamp: elem.timestamp,
        }
    }

    fn trigger(&self, net: &mut QNet, external_lookup: &[RouterId]) {
        let ext = external_lookup[self.peer % external_lookup.len()];
        log::debug!("trigger {}: {:?}", ext.fmt(net), self.path);
        match self.path.clone() {
            Some(as_path) => net
                .advertise_external_route(ext, SinglePrefix, as_path, None, None)
                .unwrap(),
            None => net.withdraw_external_route(ext, SinglePrefix).unwrap(),
        }
    }
}

fn worker<W: std::io::Write>(
    config: Config,
    empty_net: &'static SNet,
    roles: &'static HashMap<RouterId, String>,
    receiver: channel::Receiver<Task>,
    csv_writer: Arc<Mutex<csv::Writer<W>>>,
) {
    let external_lookup = empty_net.external_indices().collect::<Vec<_>>();
    let router_to_consider: Option<RouterId> = if !config.all_routers_analysis {
        empty_net
            .internal_routers()
            .max_by_key(|r| r.bgp.get_sessions().len())
            .map(|x| x.router_id())
    } else {
        None
    };
    let mut net = setup_queue(empty_net.clone(), &config);
    // Only the trace of `router_to_consider` is read, so skip building measurement logs on every
    // other session — that per-session log push is the dominant per-task allocation.
    if let Some(router) = router_to_consider {
        net.queue_mut().restrict_monitoring_to(router);
    }

    loop {
        let Ok(task) = receiver.recv() else { return };

        match run_experiment(
            &mut net,
            router_to_consider,
            &external_lookup,
            task,
            roles,
            &config,
            &csv_writer,
        ) {
            Ok(()) => {}
            Err(e) => {
                log::error!("Error while running an experiment: {e}");
            }
        }
    }
}

#[time("info")]
fn run_experiment<W: std::io::Write>(
    net: &mut QNet,
    router_to_consider: Option<RouterId>,
    external_lookup: &[RouterId],
    task: Task,
    roles: &HashMap<RouterId, String>,
    config: &Config,
    writer: &Arc<Mutex<csv::Writer<W>>>,
) -> Result<(), String> {
    // simulate to obtain the trace
    let traces = simulate(net, external_lookup, &task, router_to_consider)?;
    // reset the network while keeping all allocations.
    net.clear_bgp();
    net.queue_mut().clear();

    for (rid, trace) in traces {
        let timer = timer!(Level::Info; "prepare verification");
        let router_name = rid.fmt(&net);
        if router_name.as_str() == "Monitor" {
            // skip verifying the monitor
            continue;
        }

        let trace_len = trace.len();
        let router = net
            .get_internal_router(rid)
            .context("Network::get_internal_router")?
            .clone();
        let arena = Default::default();
        let mut monitor = ReorderingMonitor::new(router, &arena);
        monitor
            .set_process_withdraw_mode(config.process_withdraws)
            .unwrap();
        drop(timer);

        let timer = timer!(Level::Info; "run verification");
        let start = Instant::now();
        let (num_forks, sequence_ok, was_killed, max_heap_size) = reordering_eval::test_trace(
            &net,
            &mut monitor,
            trace,
            Some(config.assumed_processing_time),
        );
        let elapsed = start.elapsed();
        drop(timer);

        if max_heap_size > 1024 * 1024 * 512 {
            log::warn!(
                "Monitor used a lot of memor: {} mb",
                max_heap_size / 1024 / 1024
            );
        }

        let mut w = writer.lock().context("Locking the CSV writer")?;
        w.serialize(ExperimentResult {
            commit_hash: env!("GIT_HASH"),
            seed: config.seed,
            duration_between_events: config.duration_between_events,
            assumed_processing_time: config.assumed_processing_time,
            topology: config.topology,
            num_nodes: config.topology.num_internals(),
            num_edges: config.topology.num_internal_edges(),
            num_externals: config.num_externals,
            num_route_reflectors: config.num_route_reflectors,
            process_withdraws: config.process_withdraws,
            with_monitor_speaker: config.with_monitor_speaker,
            prefix: task.prefix,
            num_external_events: task.messages.len(),
            external_event_duration: task.end_timestamp - task.start_timestamp
                + config.duration_between_events,
            router: rid.index(),
            router_name,
            router_role: roles
                .get(&rid)
                .map(|x| x.as_str())
                .unwrap_or("?")
                .to_string(),
            num_events: trace_len,
            verif_time: elapsed.as_secs_f64(),
            max_num_forks: num_forks,
            max_heap_size,
            was_killed,
            found_bug: !sequence_ok,
        })
        .context("Failed to write the results to csv")?;
        w.flush().context("Failed to flush the csv file")?;
    }

    Ok(())
}

#[time("info")]
fn simulate(
    net: &mut QNet,
    external_lookup: &[RouterId],
    task: &Task,
    router_to_consider: Option<RouterId>,
) -> Result<HashMap<RouterId, Vec<Event<P, NotNan<f64>>>>, String> {
    log::trace!("new simulation");

    net.manual_simulation();

    let mut events = task.messages.iter().peekable();

    // trigger the first event
    let event = events.next().ok_or_else(|| "Empty task".to_string())?;
    net.queue_mut().current_time = NotNan::new(event.timestamp).unwrap();
    event.trigger(net, external_lookup);

    let mut time_before = NotNan::default();

    let timer = timer!(Level::Info; "simulation loop");
    'simulation: loop {
        debug_assert!(
            time_before <= net.queue().current_time,
            "Time went backward! queue time = {}, last queue time = {}",
            net.queue().current_time,
            time_before,
        );
        time_before = net.queue().current_time;

        // pop the next event from the queue
        let Some(next) = net.queue_mut().poll() else {
            // if the queue is empty, then push the next event onto the queue, advancing the virtual time
            let Some(event) = events.next() else {
                // we are finished
                break 'simulation;
            };
            log::trace!("queue empty, but external messages is not");
            // advance the time
            assert!(
                net.queue().current_time.into_inner() <= event.timestamp,
                "Time went backward! queue time = {}, event time = {}\nRemaining: {:?}",
                net.queue().current_time,
                event.timestamp,
                task.messages
                    .iter()
                    .map(|x| x.timestamp)
                    .collect::<Vec<_>>(),
            );
            net.queue_mut().current_time = NotNan::new(event.timestamp).unwrap();
            event.trigger(net, external_lookup);
            continue 'simulation;
        };
        let time = net.queue().current_time;

        // now, check if we should first simulate the next event before executing this one
        while time.into_inner() >= events.peek().map(|x| x.timestamp).unwrap_or(f64::INFINITY) {
            log::trace!("external event should be earlier");
            // we must execute the next event first, but no need to update the time
            events.next().unwrap().trigger(net, external_lookup);
        }

        // finally, we can process this event, if the poll yielded an actual event.
        if let Some(next) = next {
            log::trace!("trigger internal event {next:?}");
            let (_, events) = unsafe { net.trigger_event(prepare(next)).context("trigger_event")? };
            for event in events {
                unsafe { net.enqueue_event(event) };
            }
        }
    }
    assert!(net.queue_mut().pop().is_none());
    drop(timer);

    let timer = timer!(Level::Info; "Post processing");
    let result = if let Some(router) = router_to_consider {
        let trace = net.queue_mut().measurement_trace(router);
        [(router, trace)].into_iter().collect()
    } else {
        net.queue_mut().all_measurement_traces()
    };
    drop(timer);

    Ok(result)
}

fn prepare(event: Event<P, NotNan<f64>>) -> Event<P, ()> {
    match event {
        Event::Bgp { src, dst, e, .. } => Event::Bgp { src, dst, e, p: () },
        Event::Ospf {
            src, dst, area, e, ..
        } => Event::Ospf {
            src,
            dst,
            area,
            e,
            p: (),
        },
    }
}

fn setup_network(config: &Config) -> Result<(SNet, HashMap<RouterId, String>), String> {
    let mut rng = StdRng::seed_from_u64(config.seed);
    let mut network: Network<SinglePrefix, BasicEventQueue<SinglePrefix>> =
        config.topology.build(Default::default());

    let conf_settings = reordering_eval::configure_network(
        &mut rng,
        &mut network,
        config.num_externals,
        config.num_route_reflectors,
        config.with_monitor_speaker,
    )
    .context("configure_network")?;

    let roles = conf_settings.roles;

    Ok((network, roles))
}

fn setup_queue(net: SNet, config: &Config) -> QNet {
    let mut queue = ReorderingQueue::new(ReorderingSessionQueueInit::new(config.seed));
    // fill the queue in a deterministic ordering
    for r in net.device_indices().sorted() {
        for n in net
            .get_device(r)
            .unwrap()
            .bgp_neighbors()
            .into_iter()
            .sorted()
        {
            queue.init_session(r, n, false, config.with_debouncing, 0.01);
        }
    }
    log::info!("queues set up");
    net.swap_queue(queue)
}

fn find_events_in(
    config: Config,
    sender: channel::Sender<Task>,
    multi_progress: MultiProgress,
) -> Result<(), String> {
    println!("Preparing files to download");
    let files = BgpkitBroker::new()
        .ts_start(config.start)
        .ts_end(config.end)
        .data_type("updates")
        .collector_id(config.collector)
        .into_iter()
        .collect::<Vec<_>>();

    let mut data = HashMap::<IpNet, Task>::new();
    let mut peer_ids = HashMap::<IpAddr, usize>::new();

    let progress = multi_progress.add(ProgressBar::new(files.len() as u64).with_style(
        ProgressStyle::default_bar()
            .template(
                "[FILES]    {elapsed_precise} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {eta} left, {msg}",
            )
            .unwrap()
            .progress_chars("=> "),
    ));

    for file in files.into_iter().progress_with(progress) {
        let elems = BgpkitParser::new(&file.url)
            .context("Failed to fetch BMP file")?
            .into_iter()
            .collect::<Vec<_>>();

        let sub_progress = multi_progress.add(ProgressBar::new(elems.len() as u64).with_style(
            ProgressStyle::default_bar()
                .template(
                    "[MESSAGES] {elapsed_precise} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {eta} left, {msg}",
                )
                .unwrap()
                .progress_chars("=> "),
        ));
        for elem in elems.into_iter().progress_with(sub_progress) {
            let next_peer_id = peer_ids.len();
            let peer_id = *peer_ids.entry(elem.peer_ip).or_insert(next_peer_id);

            match data.entry(elem.prefix.prefix) {
                Entry::Occupied(mut e) => {
                    if let Some(finished) =
                        e.get_mut()
                            .update(elem, peer_id, config.duration_between_events)
                    {
                        // only process if there are enough messages
                        if finished.messages.len() > config.min_num_events_to_verify {
                            // push it to the queue
                            sender.send(finished).context("Failed to schedule a job")?;
                        }
                    }
                }
                Entry::Vacant(e) => {
                    e.insert(Task::new(elem, peer_id));
                }
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize)]
struct ExperimentResult<'a> {
    commit_hash: &'a str,
    seed: u64,
    duration_between_events: f64,
    assumed_processing_time: f64,
    // initial configuration
    topology: TopologyZoo,
    num_nodes: usize,
    num_edges: usize,
    num_externals: usize,
    num_route_reflectors: usize,
    process_withdraws: ProcessWithdraws,
    with_monitor_speaker: bool,
    // event configuration
    prefix: IpNet,
    num_external_events: usize,
    external_event_duration: f64,
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
    /// Whether the process found a bug (should be false)
    found_bug: bool,
}

pub trait ResultExt<T, E> {
    fn context(self, context: &'static str) -> Result<T, String>;
}

impl<T, E: std::fmt::Display> ResultExt<T, E> for Result<T, E> {
    #[track_caller]
    fn context(self, context: &'static str) -> Result<T, String> {
        let location = std::panic::Location::caller();
        self.map_err(|e| format!("{context}: {e}\nin: {location}"))
    }
}
