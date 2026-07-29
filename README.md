# GhostBuster: runtime verification of BGP routers

This artifact contains the simulator, the runtime verifier, and the GNS3 testbed used to find and
reproduce BGP convergence bugs in real FRR routers.

It has three independent parts, and you can evaluate them separately:

| Part | What it needs | Disk |
|---|---|---|
| **1. Simulation and verification** | a Rust toolchain — no GNS3, no Docker, no root | ~5 GB |
| **2. The GNS3 testbed** | Docker, with privileged containers and host networking | ~5 GB |
| **3. The Lean proof** | a Lean toolchain (`elan`) | ~10 GB |

Budget **~25 GB free disk** for all three. Most of that is the prebuilt Mathlib for the proof and the
Rust build directory, not the artifact itself.

## Prerequisites

On a clean Ubuntu 22.04/24.04 machine:

```sh
# System packages. build-essential, pkg-config and libssl-dev are for the Rust build;
# git-lfs must be installed *before* cloning (see below).
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libssl-dev git-lfs curl

# Rust (Parts 1 and 2) and Lean (Part 3). Both per-user, no root needed.
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
curl https://elan.lean-lang.org/elan-init.sh -sSf | sh

# Docker, for Part 2 only.
sudo apt-get install -y docker.io docker-compose-v2
sudo usermod -aG docker "$USER"
```

The `usermod` only takes effect on a new login. Either log out and back in, or prefix commands with
`sg docker -c "..."` in the current session — otherwise every Docker command fails with
`permission denied … /var/run/docker.sock`.

If you use Nix, `flake.nix` provides a shell with the Rust toolchain and OpenSSL, covering the build
for Parts 1 and 2. Docker still has to come from the host.

## Getting the repository

`bgpsim` is a submodule and the router image archive is stored with Git LFS, so both need pulling in.
**Install `git-lfs` before cloning**: without it you silently get a 130-byte pointer file instead of
the 550 MB image archive:

```sh
git lfs install
git clone --recurse-submodules https://github.com/nsg-ethz/ghostbuster.git
```

Already cloned without them? `git submodule update --init` and `git lfs pull`.

## Layout

```
src/                         simulator, verifier, and GNS3 testbed driver
  config.rs                  all runtime configuration (see below)
  reordering_monitoring.rs   the verifier
  testbed/                   GNS3 experiment orchestration
  bin/                       experiment entry points
bgpsim/                      submodule: BGP simulator
bgpsim-gns3/                 vendored: GNS3 bindings and the FRR Dockerfile
gns3/                        containerised GNS3 server, and load-images.sh
images/                      router image archive (Git LFS), and README listing what is in it
proof/                       the Lean proof
```

## How long everything takes

The GNS3 experiments are the only genuinely long ones, and each has a documented way to run a
fraction of it.

| Step | Time |
|---|---|
| `cargo build --release` from a fresh clone | <5 min |
| `lake exe cache get` — downloading prebuilt Mathlib | several GB; often the longest single step |
| Checking the Lean proof, after `lake exe cache get` | ~30 s |
| `reordering_eval`, the pinned example below | a few seconds |
| `real_world_performance`, one collector file | 2-3 min |
| One bug experiment, `FAILSIM_RUNS=2` | 11–15 min |
| One bug experiment, full 64 runs | ~90 min |

## Part 1 - Simulation and verification

Requires only a Rust toolchain (stable).

```sh
cargo build --release
```

The main entry point sweeps simulated scenarios and checks each run against the verifier:

```sh
# a small sweep: one topology, a few seeds
./target/release/reordering_eval \
    --topology Abilene \
    --num-external-networks 3 \
    --num-external-events 10 \
    --with-reordering true \
    --seed 0 --seed 1 --seed 2
```

Every argument accepts multiple values and the tool takes the cross product. The command above pins
five of them, which leaves 168 scenarios and runs in a few seconds. Omitting an argument sweeps its
full range instead — that is where a full sweep's minutes go.

Output goes to `$FAILSIM_RESULTS_PATH` (default `./results`):

- `reordering_<timestamp>.csv.gz` — one row per scenario, with the true/false positive and negative
  counts, blast radius, verification time and bug classification. This is the evaluation data.
- `reordering_<timestamp>_verif_time.csv.gz` — per-trace verification timings.
- `raw/reordering_<timestamp>_raw_<n>.json.gz` — the full serialised experiment for each scenario.

`Killed monitor X -> Y after creating 4097 forks` messages during the run are expected: they are the
verifier hitting its fork limit on a trace, and are counted in the `num_monitors_killed` column.

### Replaying real BGP data

`real_world_performance` fetches BGP updates from a public collector via BGPKit and replays them
through the verifier. It needs network access.

The defaults cover a whole month of `route-views.amsix` (around 2900 MRT files, which takes a very
long time). Narrow the window to a single file to try it out:

```sh
./target/release/real_world_performance \
    --start 2025-12-01T00:00:00Z \
    --end   2025-12-01T00:10:00Z \
    --collector route-views.amsix \
    --num-workers 4
```

That is one single 5.6 MB update file, two to three minutes, producing ~8400 verified prefixes in
`$FAILSIM_RESULTS_PATH`.

## Part 2 - The GNS3 testbed

This runs actual FRR routers as Docker containers wired into a topology by GNS3, captures their BGP
traffic, and feeds it to the verifier.

### Requirements

- ~5 GB free disk and port 3080 free on the host.
- A host where you can run a container that is **privileged**, bind-mounts the host Docker socket,
  and shares the host **PID and network namespaces**. All three are needed (see below) and all three
  are things some managed or hardened environments forbid outright.

### Get the router images

The archive comes with the repository over Git LFS, so there is nothing extra to download. Load the
images into your Docker daemon:

```sh
./gns3/load-images.sh     # loads the 7 FRR images and verifies they are all present
```

If it complains that the archive is tiny, LFS did not fetch it: run `git lfs pull`.

`images/README.md` lists every image and which experiment uses it. Two of them
(`frr:gns-alpine-lp-bug`, `frr:gns-alpine-mrai-bug`) are patched FRR builds that cannot be rebuilt
from this repository — the archive is the only copy. The changes they carry are documented in the
branches of https://github.com/tiborschneider/frr.

### Start GNS3

`/opt/gns3` has to exist on the host before the server starts:

```sh
sudo mkdir -p /opt/gns3 && sudo chown "$USER:$USER" /opt/gns3

cd gns3
docker compose up -d                      # on older installs: docker-compose up -d
curl http://localhost:3080/v2/version     # should report exactly: "version": "2.2.55"
```

If the `curl` returns nothing, something else already holds port 3080 (`ss -tlnp | grep 3080`);
`docker compose up -d` reports success either way. The container reproduces the VM the results were
produced on: Ubuntu 20.04, `gns3-server` 2.2.55, `ubridge` 0.9.19.

> `/opt/gns3` must stay bind-mounted from the host at that exact path. GNS3 does not copy files into
> the routers, it asks Docker to mount them, so the path has to mean the same thing to the daemon as
> it does to GNS3. A named volume breaks router startup.

The compose file's comments explain its three unusual settings.

### Run the bug experiments

Each of these builds a topology, injects the relevant reconfiguration, and records whether the
verifier flags the resulting behaviour:

```sh
./target/release/lp_bug      # local-preference bug
./target/release/mrai_bug    # MRAI bug
./target/release/pl_bug      # prefix-list bug
```

Each experiment is configured with the 64 runs used for the paper, and every run brings up a full
emulated network and waits for BGP to converge — about **90 minutes per experiment**. To check that
the pipeline works without waiting that long, cap the run count:

```sh
FAILSIM_RUNS=2 ./target/release/lp_bug     # 11-15 minutes
```

The other knobs are compile-time constants in `src/bin/*_bug.rs` — `runs`, `max_sequences`,
`early_return`, and `GROUND_TRUTH` — so changing them means editing the file and rebuilding.

`GROUND_TRUTH` (default `true`) controls whether the run has an independent ground truth to compare
the verifier against. **All three bugs are still present in released, unmodified FRR images**.

For `lp_bug` and `mrai_bug` the ground truth comes from a patched FRR image that logs a `<<<:::BUG:::>>>`
marker at the moment the bug fires; the checker greps for it. That instrumentation is the *only*
reason those two images exist. Setting `GROUND_TRUTH = false` runs the faulty router on the plain
released `frr:10.2.1` instead, which still exhibits the bug but says nothing about it: there is
nothing to grep and the checker is skipped. That configuration is the stronger demonstration, since
nothing about the router has been touched; you just lose the independent confirmation.

`pl_bug` needs no instrumentation: its ground truth is derived from the recording itself, by spotting
a non-whitelisted prefix leaving the faulty router. So it runs stock `frr:8.4.2` either way, and
`GROUND_TRUTH` only decides whether that check runs.

Results land in `$FAILSIM_DATA_PATH/experiments/<name>/<timestamp>/`:

- `results.csv` — one row per (run, prefix, sequence) with `monitoring_errors` (how many errors the
  verifier raised), `bug_reports` (how many the ground-truth checker confirmed), and the
  routing-state diffs between the emulated and simulated networks. This is the evaluation data.
- `run_<n>.json` / `run_<n>.log` — the full result and trace log for each run.
- `baseline_network.json`, `info.json` — the topology and the configuration used.

#### What to expect

These experiments drive real FRR routers in an emulated network, so **the numbers are not
deterministic.** Row and detection counts shift between machines and between runs on the same
machine, with core count, host load, how quickly BGP converges, and where MRAI timers happen to fall
relative to the captures.

The row count in particular is not a fixed quantity. All three binaries set `early_return: true`: once 
a prefix's tables diverge, that prefix stops being monitored, and the run ends early when all of them have.
With `runs=2`, `max_sequences=10` and 3 prefixes the ceiling is 60 rows, and **the sooner the bug manifests,
the fewer rows you get**.

So treat these as the right order of magnitude rather than as expected output. The ranges below come
from `FAILSIM_RUNS=2` runs on a few different machines:

| Experiment | Result rows | Rows with a bug confirmed |
|---|---|---|
| `lp_bug` | 20–30 | nearly all |
| `mrai_bug` | 40–55 | most |
| `pl_bug` | 30–40 | roughly two thirds |

The check is qualitative. With `GROUND_TRUTH` at its default, all of these should hold:

1. The run finishes without a panic, and the tool reports `Completed 2 successful runs out of 2`.
2. A clear majority of rows have a non-zero `bug_reports` — the ground-truth checker confirming the
   bug really occurred.
3. `monitoring_errors` is non-zero on at least one row: the verifier caught the bug from the message
   trace alone. Expect this column to be far sparser than `bug_reports`. For `mrai_bug` a single flagged
   row out of ~50 is normal.
4. No *run* has a non-zero `monitoring_errors` while `bug_reports` is zero. That is the direction worth
   checking: the verifier should not flag runs in which the bug never manifested.

Rebuilding with `GROUND_TRUTH = false` leaves the `bug_reports` column empty — nothing is checking it
— while `monitoring_errors` should still fire. That run shows the verifier working against a stock,
unmodified FRR release.

## Part 3 - The Lean proof

`proof/` holds the machine-checked version of the paper's correctness argument. Lean is installed
through `elan`, which reads `proof/lean-toolchain` and fetches the exact compiler the proof was
written against:

```sh
cd proof
lake exe cache get   # prebuilt Mathlib: a several-GB download, and 7.3 GB on disk once unpacked.
                     # Without it, Mathlib is compiled from source, which takes hours.
lake build
```

`lake build` reporting success is the check: Lean accepts the file only if every proof in it is
complete.

## Configuration

Everything is environment-driven, with defaults that work for the setup above. Nothing needs
editing to run locally.

| Variable | Default | Meaning |
|---|---|---|
| `GNS3_HOST` | `localhost` | Host running `gns3server` |
| `GNS3_PORT` | `3080` | GNS3 REST API port |
| `GNS3_PROJECTS_PATH` | `/opt/gns3/projects` | Where the GNS3 server's project files are readable. The tools read packet captures from disk, so this has to be the server's own directory rather than a copy — which the bundled container arranges by bind-mounting `/opt/gns3` from the host. |
| `FAILSIM_DATA_PATH` | `./data` | Root for GNS3 experiment output |
| `FAILSIM_RESULTS_PATH` | `./results` | Where `reordering_eval` writes its CSVs and raw dumps |
| `FAILSIM_RUNS` | *(unset)* | Caps the run count of a GNS3 bug experiment, for smoke tests |
