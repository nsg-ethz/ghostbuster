//! Runtime configuration, resolved from environment variables with defaults that work for the
//! bundled docker-compose setup.
//!
//! Nothing here is required: every value has a default, so a plain `cargo run` works out of the
//! box against a GNS3 server on localhost. The variables exist so the artifact can be pointed at a
//! GNS3 server running somewhere else (a VM, another host) without recompiling.
//!
//! | Variable            | Default            | Meaning                                  |
//! |---------------------|--------------------|------------------------------------------|
//! | `GNS3_HOST`         | `localhost`        | Host running `gns3server`                 |
//! | `GNS3_PORT`         | `3080`             | Port of the GNS3 REST API                 |
//! | `FAILSIM_DATA_PATH` | `./data`           | Root for experiment output and GNS3 dumps |

use std::{env, path::PathBuf};

/// Host running the GNS3 server.
pub fn gns3_host() -> String {
    env::var("GNS3_HOST").unwrap_or_else(|_| String::from("localhost"))
}

/// Port the GNS3 REST API listens on.
pub fn gns3_port() -> u16 {
    env::var("GNS3_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3080)
}

/// Root directory for everything this tool reads or writes.
pub fn data_path() -> PathBuf {
    env::var("FAILSIM_DATA_PATH")
        .unwrap_or_else(|_| String::from("./data"))
        .into()
}

/// Directory holding the results of a given experiment, e.g. `lp_bug`.
pub fn experiments_path(experiment: &str) -> PathBuf {
    data_path().join("experiments").join(experiment)
}

/// Caps the number of runs a GNS3 bug experiment performs.
///
/// The experiments are configured with the run count used for the paper, which takes on the order
/// of an hour. Setting this to a small number is the intended way to smoke-test the pipeline
/// without waiting for a full experiment.
pub fn runs_override() -> Option<usize> {
    env::var("FAILSIM_RUNS").ok().and_then(|r| r.parse().ok())
}

/// Directory that `reordering_eval` writes its summary CSVs and raw experiment dumps into.
pub fn results_path() -> PathBuf {
    env::var("FAILSIM_RESULTS_PATH")
        .unwrap_or_else(|_| String::from("./results"))
        .into()
}

/// Local path at which the GNS3 server's `projects` directory is readable.
///
/// Packet captures are written by the GNS3 server into its own projects directory and then read
/// back from disk here, so this path has to resolve to the *same files the server writes*, not to a
/// copy. With the bundled `gns3/docker-compose.yml` that is automatic: `/opt/gns3` is bind-mounted
/// from the host, so the captures are directly visible at the default below.
///
/// When driving a GNS3 server on another machine, this must point at a mount of that server's
/// projects directory, for example:
///
/// ```sh
/// sshfs gns3@<host>:/opt/gns3/projects /mnt/gns3-projects
/// export GNS3_PROJECTS_PATH=/mnt/gns3-projects
/// ```
pub fn gns3_projects_path() -> PathBuf {
    env::var("GNS3_PROJECTS_PATH")
        .unwrap_or_else(|_| String::from("/opt/gns3/projects"))
        .into()
}
