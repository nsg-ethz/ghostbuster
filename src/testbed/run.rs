use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    thread::sleep,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bgpsim::{
    event::BasicEventQueue as Q,
    export::LinkId,
    ospf::GlobalOspf as Ospf,
    prelude::{Network, NetworkFormatter},
    types::RouterId,
};
use bgpsim::{event::EventQueue, types::Prefix};
use bgpsim_gns3::{routing_state::BgpTableDiffs, Gns3Network, Gns3NetworkError};
use indicatif::ProgressBar;
use log::debug;
use ordered_float::NotNan;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::{
    monitoring::{Controller, MonitoringResults},
    recording::Recording,
    reordering_monitoring::Arena,
    testbed::{
        generator::UniformEventGenerator,
        ground_truth::BugReport,
        reconfiguration::{pl::apply_pl_reconfigs, PreConfig},
    },
};
use std::sync::Arc;

use super::simulation::{ExternalEvent, SimulationEvent};
use super::ExperimentConfig;
use super::P;


#[derive(Debug, Clone, Serialize)]
pub struct RunConfig {
    /// The maximum number of sequences per run. After each sequence, the resulting pcaps and the
    /// two networks will be monitored
    pub max_sequences: usize,
    /// The number of interval within each sequence.
    /// In between intervals the network will wait for convergence
    pub intervals_per_sequence: usize,
    /// The number of simualtion steps per interval
    pub steps_per_interval: usize,
    /// How long to wait in between intervals
    pub convergence_wait: Duration,
    /// Duration of a time step in the simulation
    pub simulation_step: Duration,
    /// Wether to perform an early return when the routing states differ
    pub early_return: bool,
}

/// A run is made up of different sequences
pub type RunResult = Vec<SequenceResult>;

/// A sequence contains intermediate results
#[derive(Debug, Serialize, Deserialize)]
pub struct SequenceResult {
    pub start: f64,
    pub finish: f64,
    pub simulated_events: Sequence,
    pub monitored_prefixes: HashSet<P>,
    pub recording: Recording<P>,
    pub monitoring_results: MonitoringResults<P>,
    pub tables_diff: BgpTableDiffs<P>,
    pub bug_reports: Option<Vec<BugReport>>,
}

/// A brief overview of the results in this sequence
#[derive(Debug, Serialize)]
pub struct SequenceResultSummary {
    /// For this run
    pub run: usize,
    /// For this prefix
    pub prefix: u32,
    /// For this sequence
    pub sequence: usize,
    /// How many events have we seen
    pub simulated_events: usize,
    /// How many monitoring errors have we reported for that prefix
    pub monitoring_errors: usize,
    /// How many routers have selected different routes for that prefix
    pub selected_diff: usize,
    /// How many routers have different tables for that prefix
    pub tables_diff: usize,
    /// How many bug have been reported on that prefix
    pub bug_reports: Option<usize>,
}

impl From<(usize, usize, P, &SequenceResult)> for SequenceResultSummary {
    fn from((run, sequence, prefix, result): (usize, usize, P, &SequenceResult)) -> Self {
        SequenceResultSummary {
            run,
            prefix: prefix.as_num(),
            sequence,
            simulated_events: result
                .simulated_events
                .intervals
                .iter()
                .flatten()
                .filter(|(_, sim_event)| match sim_event {
                    SimulationEvent::External(ExternalEvent { prefix: p, .. }) => *p == prefix,
                    // Always count reconfiguration events
                    SimulationEvent::Reconfiguration => true,
                })
                .count(),
            monitoring_errors: result.monitoring_results.get_errors_per_prefix(&prefix),
            selected_diff: result
                .tables_diff
                .iter()
                .filter(|(_, diffs)| {
                    diffs
                        .get(&prefix)
                        .map(|diff| !diff.selected_routes_equal())
                        .unwrap_or(false)
                })
                .count(),
            tables_diff: result
                .tables_diff
                .iter()
                .filter(|(_, diffs)| diffs.contains_key(&prefix))
                .count(),
            bug_reports: result.bug_reports.as_ref().map(|bug_reports| {
                bug_reports
                    .iter()
                    .filter(|report| report.prefix == prefix)
                    .count()
            }),
        }
    }
}

/// A sequence is made up of intervals
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sequence {
    pub intervals: Vec<Interval>,
}
/// Timestamp of when each SimulationEvent was triggered
type Interval = Vec<(f64, SimulationEvent)>;

impl Sequence {
    /// Filter for specific prefixes, consumes self and returns a sequence of only the events we simulated
    /// on those prefixes. Always returns reconfiguration events as well
    pub fn filter_prefixes(self, prefixes: &HashSet<P>) -> Self {
        self.filter(|ev| match ev {
            SimulationEvent::External(e) => prefixes.contains(&e.prefix),
            // Always return true for reconfiguration events
            _ => true,
        })
    }

    fn filter<F>(self, keep: F) -> Self
    where
        F: Fn(&SimulationEvent) -> bool,
    {
        Self {
            intervals: self
                .intervals
                .into_iter()
                .filter_map(|interval| {
                    let i: Interval = interval.into_iter().filter(|(_, ev)| keep(ev)).collect();
                    (!i.is_empty()).then_some(i)
                })
                .collect(),
        }
    }
}
impl<'a, PR, Q, Ospf> NetworkFormatter<'a, PR, Q, Ospf> for Sequence
where
    PR: Prefix,
    Q: EventQueue<PR>,
    Ospf: bgpsim::ospf::OspfImpl,
{
    fn fmt(&self, net: &'a bgpsim::network::Network<PR, Q, Ospf>) -> String {
        let mut result = String::from("Sequence [\n");
        for interval in &self.intervals {
            for (timestamp, event) in interval {
                result.push_str(&format!(
                    "  {timestamp:<w$} : {},\n",
                    event.fmt(net),
                    w = 18
                ));
            }
            result.push_str(&format!("  --- Pausing between intervals ---,\n"));
        }
        result.push_str("]");
        result
    }
}

pub struct Run<'a> {
    id: usize,
    config: ExperimentConfig,

    // Things we need to keep track of during the run
    /// The GNS3 network we are executing this run on
    pub gns3_net: Gns3Network<'a, P, Q<P>, Ospf>,
    sim_net: Network,
    controller: Controller<'a, P>,
    /// A way to generate SimulationEvents
    event_generator: UniformEventGenerator,
    /// A function that can be called on self in order
    /// to check wether or not a bug was triggered in
    /// a given interval
    checker: Option<
        Arc<dyn Fn(&Self, &Recording<P>, (f64, f64)) -> Option<Vec<BugReport>> + Send + Sync>,
    >,
}

impl<'a> Run<'a> {
    /// Create a new run for an experiment
    pub fn new(
        id: usize,
        net_baseline: &'a Network,
        config: ExperimentConfig,
        event_generator: UniformEventGenerator,
        checker: Option<
            Arc<dyn Fn(&Self, &Recording<P>, (f64, f64)) -> Option<Vec<BugReport>> + Send + Sync>,
        >,
        arena: &'a Arena,
    ) -> Result<Self, Gns3NetworkError> {
        // Create the GNS3 network
        let mut gns3_net = Gns3Network::new(
            format!("testbed_{}", id),
            net_baseline,
            Some(crate::config::gns3_host()),
            Some(crate::config::gns3_port()),
            false,
            config.gns3_config.router_templates.clone(),
        )?;
        sleep(Duration::from_secs(20));

        // Apply post-initialization reconfiguration
        if let Some(steps) = &config.gns3_config.post_config {
            info!("Applying reconfiguration");
            for reconfiguration in steps {
                reconfiguration.apply(&mut gns3_net)?;
            }
            sleep(Duration::from_secs(5));
        }

        // Create the BGPSim network for simulation
        let sim_net = net_baseline.clone();
        let mut controller =
            Controller::new_for_prefixes(&sim_net, &config.monitoring_prefixes, arena);
        controller.mrai = config.monitoring_mrai.map(|u| {
            info!("Setting MRAI value on controller");
            NotNan::new((u * 2) as f64).expect("Cannot be NaN")
        });

        Ok(Run {
            id,
            config,
            gns3_net,
            sim_net,
            controller,
            event_generator,
            checker,
        })
    }

    /// Execute this run
    pub fn execute(&mut self, bar: &ProgressBar) -> Result<RunResult, Gns3NetworkError> {
        let max_sequences = self.config.run_config.max_sequences;
        // Track the sequence results
        let mut sequence_results = Vec::new();
        'sequence: for i in 0..max_sequences {
            let result = match self.execute_sequence() {
                Ok(r) => r,
                Err(e) => {
                    // Fill remaining progress for this thread
                    bar.inc((max_sequences - i) as u64);
                    return Err(e);
                }
            };

            // Collect all unequal prefixes for an eventual early return
            let unequal_prefixes: HashSet<P> = result
                .tables_diff
                .iter()
                .flat_map(|(_, diffs)| {
                    diffs
                        .iter()
                        // By unequal we count the amount of routes that have selected unequal routes
                        .filter(|(_, diff)| !diff.selected_routes_equal())
                        .map(|(p, _)| *p)
                })
                .collect();
            sequence_results.push(result);
            bar.inc(1);

            if self.config.run_config.early_return {
                for prefix in unequal_prefixes {
                    warn!("Table diffs for prefix {} detected", prefix);
                    // Stop monitoring that prefix, it is marked as problematic
                    self.config.monitoring_prefixes.remove(&prefix);
                    // If there are no more "unaffected" prefixes remaining, return
                    if self.config.monitoring_prefixes.len() == 0 {
                        warn!("Early return at iteration {i}");
                        // Fill remaining progress as done
                        bar.inc((max_sequences - (i + 1)) as u64);
                        break 'sequence;
                    }
                }
            }
        }

        let all_equal_tables = sequence_results.iter().all(|r| r.tables_diff.is_empty());
        let total_errors: usize = sequence_results
            .iter()
            .map(|r| r.monitoring_results.get_errors().len())
            .sum();

        bar.println(format!(
            "Completed run {}. Always equal BGP tables: {} | Total monitoring errors: {}",
            self.id, all_equal_tables, total_errors,
        ));

        Ok(sequence_results)
    }

    /// Run a sequence of intervals.
    /// Between each interval we give the network a bit of time to "breathe", giving it a chance to converge
    /// Throughout the entire sequence we collect the exchanged events. At the end of it we monitor and
    /// compare our two networks.
    fn execute_sequence(&mut self) -> Result<SequenceResult, Gns3NetworkError> {
        sleep(Duration::from_secs(1));
        // Get start time of the sequence
        let start = now();
        debug!("Starting pcaps");
        // Start PCAP on every link
        for &LinkId(x, y) in self.gns3_net.get_links().keys() {
            self.gns3_net.start_captures(x, y)?;
        }
        sleep(Duration::from_secs(5));

        // Execute each interval in the sequence and gather the results
        let mut simulated_events = Sequence {
            intervals: Vec::new(),
        };
        for _ in 0..self.config.run_config.intervals_per_sequence {
            let interval_events = self.execute_interval()?;
            simulated_events.intervals.push(interval_events);
            // Wait for convergence for a specified amount of time
            sleep(self.config.run_config.convergence_wait);
        }
        sleep(Duration::from_secs(20));

        debug!("Stopping pcaps");
        // Stop and extract BGP messages from PCAP
        let link_captures_local: HashMap<_, PathBuf> = self
            .gns3_net
            .get_links()
            .keys()
            .map(|&LinkId(x, y)| {
                let path = self.stop_capture_to_local_file(x, y).unwrap();
                ((x, y), path)
            })
            .collect();
        let recording = Recording::from_pcaps(link_captures_local, &self.gns3_net)
            .filter_routers(
                &self
                    .gns3_net
                    .get_net()
                    .internal_routers()
                    .filter(|r| r.name() != "Monitor")
                    .map(|r| r.router_id())
                    .collect(),
            )
            .filter_prefixes(&self.config.monitoring_prefixes);
        // Get final time of the sequence
        let finish = now();

        // Monitor and compare the BGP tables
        let monitoring_results = self
            .controller
            .monitor_recording(recording.clone())
            .unwrap();
        info!(
            "Monitoring errors: {}",
            monitoring_results.fmt_multiline(&self.sim_net)
        );
        let tables_diff = self
            .gns3_net
            .compare_bgp_tables(&self.sim_net, &self.config.monitoring_prefixes)?;
        info!("Tables diff: {}", tables_diff.fmt_multiline(&self.sim_net));
        // Extract the ground truth, if any, from the recording and the logs
        let bug_reports = self
            .checker
            .as_ref()
            .and_then(|checker| checker(self, &recording, (start, finish)));
        info!("Bug reports: {}", bug_reports.fmt_multiline(&self.sim_net));

        Ok(SequenceResult {
            start,
            finish,
            simulated_events,
            monitored_prefixes: self.config.monitoring_prefixes.clone(),
            recording,
            monitoring_results,
            tables_diff,
            bug_reports,
        })
    }

    /// Execute all events in an interval
    fn execute_interval(&mut self) -> Result<Interval, Gns3NetworkError> {
        let mut interval_events = Vec::new();
        for _ in 0..self.config.run_config.steps_per_interval {
            let step_events = self.event_generator.next().unwrap();
            let step_length = step_events.len();

            // Advance time by one time step
            sleep(self.config.run_config.simulation_step);
            info!(
                "Generated events for timestep: {}",
                step_events.fmt(&self.sim_net)
            );

            let step_start = SystemTime::now();
            for event in step_events {
                // Apply all the events in the time step
                if let SimulationEvent::External(external) = &event {
                    self.apply_external_event(external.clone())?;
                } else {
                    // THIS IS A RECONFIGURATION EVENT
                    //
                    // Flipping a prefix list is what triggers the prefix-list bug, so it only
                    // applies to the scenario that installed one. The other scenarios trigger
                    // their bug through the pre/post configuration instead and have no prefix
                    // list on the faulty router, which used to panic here.
                    match &self.config.gns3_config.pre_config {
                        Some(PreConfig::Whitelist { router, .. }) => {
                            warn!("Reconfiguration event triggered");
                            apply_pl_reconfigs(&mut self.gns3_net, *router)?;
                        }
                        _ => debug!("Reconfiguration event ignored: no prefix list configured"),
                    }
                }

                // Grab the timestamp for this event
                let timestamp = now();

                // Sleep a bit in between events
                sleep(Duration::from_millis(100));

                interval_events.push((timestamp, event));
            }
            let step_duration = SystemTime::now()
                .duration_since(step_start)
                .expect("What happened here?");
            info!(
                "This simulation step was {step_length} events long and took {} seconds",
                step_duration.as_secs_f32()
            );
        }
        Ok(interval_events)
    }

    /// Apply an external simulation event on both GNS3 and simulation networks
    fn apply_external_event(
        &mut self,
        ExternalEvent {
            external_neighbor,
            prefix,
            route,
        }: ExternalEvent,
    ) -> Result<(), Gns3NetworkError> {
        if let Some(path) = route {
            // We are supposed to advertise a new prefix
            debug!(
                "Advertising an external route for {} from {}",
                prefix,
                external_neighbor.fmt(&self.sim_net)
            );
            self.gns3_net
                .advertise_external_route(external_neighbor, prefix, None, None, None)?;
            self.sim_net
                .advertise_external_route(external_neighbor, prefix, path, None, None)?;
        } else {
            // We are supposed to withdraw a new prefix
            debug!(
                "Withdrawing an external route for {} from {}",
                prefix,
                external_neighbor.fmt(&self.sim_net)
            );
            self.gns3_net
                .withdraw_external_route(external_neighbor, prefix)?;
            self.sim_net
                .withdraw_external_route(external_neighbor, prefix)?;
        }
        Ok(())
    }

    /// Stop a running capture and get a path to the local pcap data
    fn stop_capture_to_local_file(
        &mut self,
        a: RouterId,
        b: RouterId,
    ) -> Result<PathBuf, Gns3NetworkError> {
        let remote_path = self.gns3_net.stop_captures(a, b)?;

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
}

// Helper function to get current time as f64
fn now() -> f64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
    now.as_secs() as f64 + now.subsec_nanos() as f64 * 1e-9
}
