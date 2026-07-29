// BgpSim-GNS3: Control and interact with GNS3 from BgpSim
// Copyright (C) 2022-2023 Tibor Schneider <sctibor@ethz.ch>
// Modified 2025 by Pietro Ronchetti <pietroro@ethz.ch>: Additional functionality
//
// This program is free software; you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation; either version 2 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License along
// with this program; if not, write to the Free Software Foundation, Inc.,
// 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301 USA.

use std::{
    collections::{BTreeSet, HashMap},
    net::Ipv4Addr,
    slice::Iter,
    time::Duration,
    vec::IntoIter,
};

use bgpsim::{prelude::BgpSessionType, types::AsId};
use ipnet::Ipv4Net;
use itertools::Itertools;
use log::error;
use serde::{de::Error, Deserialize, Deserializer};
use serde_json::Value;
use thiserror::Error;
use time::{OffsetDateTime, UtcOffset};

use super::{telnet_client::TelnetClient, PingError, TelnetError};

/// CLient to interact with the FRR client
#[derive(Debug)]
pub struct FrrClient(TelnetClient);

const EXPECTED_CFG_HEADER: &str = "Building configuration...\n\nCurrent configuration:\n";
const EXPECTED_CFG_FOOTER: &str = "\n!\nend";

macro_rules! parse_json {
    ($s:ident, $c:expr, $dur:literal, $ty:ty) => {{
        let data = $s.0.send_cmd($c, Duration::from_secs($dur))?;
        match serde_json::from_str::<'_, $ty>(&data) {
            Ok(x) => x,
            Err(e) => {
                error!(
                    "Cannot parse data as {}! {}. Data:\n{}",
                    std::any::type_name::<$ty>(),
                    e,
                    data
                );
                return Err(FrrError::ParseError(e));
            }
        }
    }};
    ($x:expr, $ty:ty) => {{
        let data = $x;
        match serde_json::from_str::<'_, $ty>(&data) {
            Ok(x) => x,
            Err(e) => {
                error!(
                    "Cannot parse data as {}! {}. Data:\n{}",
                    std::any::type_name::<$ty>(),
                    e,
                    data
                );
                return Err(FrrError::ParseError(e));
            }
        }
    }};
}

impl FrrClient {
    /// Create a new ipterm client
    pub fn new(target: impl Into<String>, port: u16) -> Result<Self, FrrError> {
        Ok(Self(TelnetClient::new(target, port, "# ")?))
    }

    /// Send a command and get the output until the next prompt line.
    #[inline(always)]
    pub fn send_cmd(&mut self, cmd: &str, timeout: Duration) -> Result<String, FrrError> {
        Ok(self.0.send_cmd(cmd, timeout)?)
    }

    /// Test if a destination is reachable using ping. This function simply returns a boolean wether
    /// the destination is reachable.
    pub fn ping(&mut self, destination: Ipv4Addr) -> Result<(), PingError> {
        // first, go to the shell
        self.0.send_cmd("exit\n", Duration::from_secs(1))?;
        // prepare the success
        let mut success = false;
        // Then, send the ping command
        let answer =
            self.0.send_cmd(&format!("ping -c 1 -w 1 {destination}"), Duration::from_secs(10))?;
        for line in answer.lines() {
            if line.starts_with("1 packets transmitted, 1 packets received") {
                success = true;
            }
        }
        // go back to the vtysh
        self.0.send_cmd("exit", Duration::from_secs(1))?;
        if success {
            Ok(())
        } else {
            Err(PingError::Fail(answer))
        }
    }

    /// Apply the given configuration to the router.
    pub fn configure(&mut self, config: impl AsRef<str>) -> Result<(), FrrError> {
        // issue an empty command to clear everything that was here before
        self.0.send_cmd("", Duration::from_secs(1))?;
        // go to configuration mode
        self.send_config_cmd("configure terminal", 0)?;
        // iterate over all lines
        let mut num_lines = 0;
        for line in config.as_ref().trim().lines() {
            num_lines += 1;
            let line = line.trim();
            if !line.is_empty() {
                self.send_config_cmd(line, num_lines)?;
            }
        }
        // exit the configuration mode
        self.send_config_cmd("end", num_lines + 1)?;
        // configuration successful!
        Ok(())
    }

    /// Get the running configuration
    pub fn get_running_config(&mut self) -> Result<String, FrrError> {
        self.0.send_cmd("", Duration::from_secs(1))?;
        let cfg = self.0.send_cmd("show running-config", Duration::from_secs(10))?;
        if cfg.starts_with(EXPECTED_CFG_HEADER) && cfg.ends_with(EXPECTED_CFG_FOOTER) {
            Ok(cfg
                .trim_start_matches(EXPECTED_CFG_HEADER)
                .trim_end_matches(EXPECTED_CFG_FOOTER)
                .to_string())
        } else {
            Err(FrrError::ShowConfigUnexpectedAnswer(cfg))
        }
    }

    /// Send a configuration command, except only the prompt to be returned. If not, properly create
    /// the configuration error.
    fn send_config_cmd(&mut self, cmd: &str, line: usize) -> Result<(), FrrError> {
        let result = self.0.send_cmd(cmd, Duration::from_secs(10))?;
        if !result.is_empty() {
            Err(FrrError::ConfigurationError(line, cmd.to_string(), result))
        } else {
            Ok(())
        }
    }

    /// Get all routes known to the router
    pub fn get_all_routes(&mut self) -> Result<HashMap<Ipv4Net, Vec<Route>>, FrrError> {
        Ok(parse_json!(self, "show ip route json", 1, HashMap<Ipv4Net, Vec<Route>>))
    }

    /// Get the route matching a given prefix
    pub fn get_route_for_prefix(&mut self, prefix: Ipv4Net) -> Result<Option<Route>, FrrError> {
        let mut routes = parse_json!(
            self, &format!("show ip route {prefix} json"), 1, HashMap<Ipv4Net, Vec<Route>>
        );
        Ok(routes.remove(&prefix).into_iter().flatten().find(|x| x.is_valid()))
    }

    /// Get the route matching a given prefix
    pub fn get_route_for_address(&mut self, address: Ipv4Addr) -> Result<Option<Route>, FrrError> {
        let routes = parse_json!(
            self, &format!("show ip route {address} json"), 1, HashMap<Ipv4Net, Vec<Route>>
        );
        Ok(routes.into_iter().flat_map(|(_, x)| x).find(|x| x.is_valid()))
    }

    /// Get all current ospf routes
    pub fn get_ospf_routes(&mut self) -> Result<HashMap<Ipv4Net, OspfRoute>, FrrError> {
        Ok(parse_json!(self, "show ip ospf route json", 1, HashMap<Ipv4Net, OspfRoute>))
    }

    /// Get the BGP route and paths for the given prefix.
    pub fn get_bgp_routes_for_prefix(
        &mut self,
        prefix: Ipv4Net,
    ) -> Result<Option<BgpRoute>, FrrError> {
        let answer =
            self.0.send_cmd(&format!("show ip bgp {prefix} json"), Duration::from_secs(1))?;
        if answer.lines().map(|x| x.trim()).filter(|l| !l.starts_with("\"warning\":")).join("")
            == "{}"
        {
            Ok(None)
        } else {
            Ok(Some(parse_json!(answer, BgpRoute)))
        }
    }

    /// Get all BGP neighbor statistics.
    pub fn get_bgp_neighbors(&mut self) -> Result<HashMap<Ipv4Addr, BgpNeighbor>, FrrError> {
        Ok(serde_json::from_str(
            &self.0.send_cmd("show bgp neighbors json", Duration::from_secs(1))?,
        )?)
    }

    /// Get the BGP neighbor statistics for a specific neighbor.
    pub fn get_bgp_neighbor(
        &mut self,
        neighbor: Ipv4Addr,
    ) -> Result<Option<BgpNeighbor>, FrrError> {
        let mut result = parse_json!(
            self, &format!("show bgp neighbors {neighbor} json"), 1, HashMap<Ipv4Addr, BgpNeighbor>
        );
        Ok(result.remove(&neighbor))
    }

    /// Get all active route maps.
    pub fn get_route_maps(&mut self) -> Result<HashMap<String, BgpRouteMap>, FrrError> {
        // Only grab the route maps under "bgpd"
        let v: Value =
            serde_json::from_str(&self.0.send_cmd("show route-map json", Duration::from_secs(1))?)?;
        let bgpd = v.get("bgpd").ok_or_else(|| {
            FrrError::ParseError(serde_json::Error::custom("No 'bgpd' key present for route maps"))
        })?;
        Ok(serde_json::from_value(bgpd.clone())?)
    }

    /// Get all active prefix lists.
    pub fn get_prefix_lists(&mut self) -> Result<HashMap<String, IpPrefixList>, FrrError> {
        let raw = self.0.send_cmd("show ip prefix-list json", Duration::from_secs(2))?;

        let mut merged = serde_json::Map::new();

        // This function is slightly more involved, as it needs to work with older versions of FRR as well.
        // - Newer versions: a single JSON object containing all daemons
        // - Older versions: multiple JSON objects concatenated
        let stream = serde_json::Deserializer::from_str(&raw).into_iter::<Value>();
        for v in stream {
            if let Value::Object(map) = v? {
                merged.extend(map);
            }
        }

        // Older versions use "BGP", newer versions use "bgpd"
        let bgp = merged.get("BGP").or_else(|| merged.get("bgpd")).ok_or_else(|| {
            FrrError::ParseError(serde_json::Error::custom(
                "No 'BGP' or 'bgpd' key present for ip prefix lists",
            ))
        })?;

        Ok(serde_json::from_value(bgp.clone())?)
    }

    /// Setup the minimum route advertisement interval(mrai) for a specific peer.
    pub fn set_advertisement_interval(
        &mut self,
        duration: u16,
        neighbor: Ipv4Addr,
    ) -> Result<(), FrrError> {
        self.configure(format!(
            "router bgp\nneighbor {} advertisement-interval {}",
            neighbor, duration
        ))
    }

    /// Set the `suppress-fib-pending` knob on FRR. If applied, all bgp instances will wait for fib installation
    /// before announcing routes.
    pub fn set_suppress_fib_pending(&mut self) -> Result<(), FrrError> {
        self.configure("bgp suppress-fib-pending")
    }
}

/// Route information as revealed by `show ip route`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Route {
    /// The prefix for the route
    pub prefix: Ipv4Net,
    /// Prefix length used
    #[serde(alias = "prefixLen")]
    pub prefix_len: u8,
    /// The protocol being used
    pub protocol: Protocol,
    /// Wether this route was selected or not.
    #[serde(default)]
    pub selected: bool,
    /// The administrative distance of that route
    pub distance: u8,
    /// The metric, used in the respective protocol
    pub metric: u32,
    /// Wether the route is installed into the FIB
    #[serde(default)]
    pub installed: bool,
    /// List of next-hops
    #[serde(alias = "nexthops")]
    next_hops: Vec<RouteNextHop>,
}

impl Route {
    /// Returns `true` if and only if this route is selected and it is installed.
    pub fn is_valid(&self) -> bool {
        self.selected && self.installed
    }

    /// Returns the interface used by this route
    pub fn interfaces(&self) -> Vec<String> {
        self.next_hops().iter().filter_map(|nh| nh.interface.clone()).collect()
    }

    /// Returns the next-hop that is used for this route. This function will return `None` if there
    /// is no next-hop that is valid, installed in the FIB, and not recursive.
    pub fn next_hops(&self) -> Vec<&RouteNextHop> {
        self.next_hops
            .iter()
            .filter(|nh| {
                nh.active
                    && !nh.recursive
                    && !nh.blackhole
                    && !nh.unreachable
                    && nh.fib
                    && nh.interface.is_some()
            })
            .collect()
    }

    /// Returns the raw next-hops that is used for this route. This function will also return any
    /// next-hop which is not vaild, not installed, or is recursive.
    pub fn next_hops_raw(&self) -> &[RouteNextHop] {
        &self.next_hops
    }
}

/// Enumeration of all available protocols
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
pub enum Protocol {
    #[serde(alias = "ospf")]
    Ospf,
    #[serde(alias = "bgp")]
    Bgp,
    #[serde(alias = "static")]
    StaticRoute,
    #[serde(alias = "connected")]
    #[serde(alias = "local")]
    Connected,
}

/// The next hop of a route in FRR.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct RouteNextHop {
    /// Wether this next-hop is written into the FIB.
    pub fib: bool,
    /// IP of the next-hop
    pub ip: Option<Ipv4Addr>,
    /// The interface name
    #[serde(alias = "interfaceName")]
    pub interface: Option<String>,
    /// Wether this next-hop is active
    pub active: bool,
    /// Wether this next-hop is recursive
    pub recursive: bool,
    /// Wether this route is a black-hole
    pub blackhole: bool,
    /// Wether the next-hop is unreachable
    pub unreachable: bool,
}

/// A datastructure to parse OSPF route information from FRR
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct OspfRoute {
    /// Cost to reach this destination.
    pub cost: u32,
    /// OSPF Area in which this route was distributed
    pub area: Ipv4Addr,
    /// The next hops used.
    #[serde(alias = "nexthops", deserialize_with = "deserialize_ospf_next_hops")]
    pub next_hops: Vec<OspfNextHop>,
}

impl OspfRoute {
    /// Returns `true` if the route was learned from any of its neighbors.
    pub fn is_learned(&self) -> bool {
        self.next_hops.iter().any(|x| !x.direct)
    }

    /// Returns `true` if the route is known because the router is directly connected to it.
    pub fn is_direct(&self) -> bool {
        self.next_hops.iter().all(|x| x.direct)
    }

    /// Returns the interface of the first next-hop.
    pub fn get_next_hop(&self) -> Option<&String> {
        self.next_hops.iter().map(|nh| &nh.interface).next()
    }
}

/// Ospf next-hop object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OspfNextHop {
    /// IP Address of the next-hop.
    pub ip: Option<Ipv4Addr>,
    /// The interface name to send traffic on
    pub interface: String,
    /// Wether this route is learned via OSPF (`direct = false`), or the interface is directly
    /// connected (`direct = true`).
    pub direct: bool,
}

fn deserialize_ospf_next_hops<'de, D>(deserializer: D) -> Result<Vec<OspfNextHop>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct OspfNextHopRaw {
        #[serde(default)]
        ip: String,
        via: Option<String>,
        #[serde(alias = "directly attached to")]
        direct: Option<String>,
    }
    let x: Vec<OspfNextHopRaw> = Deserialize::deserialize(deserializer)?;
    Ok(x.into_iter()
        .filter_map(|x| {
            let direct = x.direct.is_some();
            let interface = x.direct.or(x.via)?;
            let ip = x.ip.trim().parse().ok();
            Some(OspfNextHop { ip, interface, direct })
        })
        .collect())
}

/// BGP ROute Object from FRR
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct BgpRoute {
    pub prefix: Ipv4Net,
    pub paths: Vec<BgpPath>,
    #[serde(default, alias = "advertisedTo", deserialize_with = "deserialize_advertised_to")]
    pub advertised_to: Vec<Ipv4Addr>,
}

impl IntoIterator for BgpRoute {
    type Item = BgpPath;
    type IntoIter = IntoIter<BgpPath>;

    fn into_iter(self) -> Self::IntoIter {
        self.paths.into_iter()
    }
}

impl<'a> IntoIterator for &'a BgpRoute {
    type Item = &'a BgpPath;
    type IntoIter = Iter<'a, BgpPath>;

    fn into_iter(self) -> Self::IntoIter {
        self.paths.iter()
    }
}

impl BgpRoute {
    /// Get an iterator over all paths.
    pub fn iter(&self) -> Iter<'_, BgpPath> {
        self.paths.iter()
    }

    /// Get the selected BGP path.
    pub fn selected(&self) -> &BgpPath {
        self.iter().find(|x| x.best_path).unwrap()
    }
}

/// A path object of a BGP Route in FRR. This is essentially a BGP Route.
#[derive(Debug, Clone, Deserialize, Eq)]
pub struct BgpPath {
    /// The AS_Path. The left-most element is the most recent AS number, the right-most element is
    /// the originator.
    #[serde(alias = "aspath", deserialize_with = "deserialize_as_path")]
    pub as_path: Vec<AsId>,
    /// Med value
    #[serde(alias = "metric")]
    pub med: Option<u32>,
    /// the local preference
    #[serde(alias = "locPrf")]
    pub local_pref: Option<u32>,
    /// Local weight, which is not propagated
    pub weight: Option<u16>,
    /// Set of BGP Communities setting
    #[serde(alias = "community", default, deserialize_with = "deserialize_community")]
    pub communities: BTreeSet<(AsId, u32)>,
    /// The router that has originally received the route
    #[serde(alias = "originatorId")]
    pub originator_id: Option<Ipv4Addr>,
    /// Cluster list, which is a list of route reflectors it traversed.
    #[serde(alias = "clusterList", default, deserialize_with = "deserialize_cluster_list")]
    pub cluster_list: Vec<Ipv4Addr>,
    /// Whether this path is valid
    pub valid: bool,
    /// Wether this route is a local route
    #[serde(default)]
    pub local: bool,
    /// Wether this path is the best one
    #[serde(default, alias = "bestpath", deserialize_with = "deserialize_best_path")]
    pub best_path: bool,
    /// When was the last update
    #[serde(alias = "lastUpdate", deserialize_with = "deserialize_epoch")]
    pub last_update: OffsetDateTime,
    /// The next hops.
    #[serde(alias = "nexthops", deserialize_with = "deserialize_bgp_next_hops")]
    pub next_hop: BgpNextHop,
    /// The peer from which theis route was received (its router-id).
    pub peer: BgpPeer,
}

impl PartialEq for BgpPath {
    fn eq(&self, other: &Self) -> bool {
        self.as_path == other.as_path
            && self.med == other.med
            && self.local_pref == other.local_pref
            && self.weight == other.weight
            && self.communities == other.communities
            && self.originator_id == other.originator_id
            && self.cluster_list == other.cluster_list
            && self.valid == other.valid
            && self.local == other.local
            && self.best_path == other.best_path
            && self.next_hop == other.next_hop
            && self.peer == other.peer
    }
}

/// The next-hops field in BGP
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct BgpNextHop {
    /// The IP address of the next-hop
    pub ip: Ipv4Addr,
    /// The host-name of the next-hop
    #[serde(default)]
    pub hostname: Option<String>,
    /// Wether this next-hop is accessible
    pub accessible: bool,
    /// Wether this next-hop is used
    pub used: bool,
    /// The IGP cost towards the next-hop.
    #[serde(alias = "metric")]
    pub igp_cost: Option<u32>,
}

impl BgpNextHop {
    /// Wether the next-hop is valid. A next-hop is invalid if the IP address is `0.0.0.0`.
    pub fn valid(&self) -> bool {
        self.ip == Ipv4Addr::new(0, 0, 0, 0)
    }
}

/// The peer information in BGP. The router-id field will always be filled!
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct BgpPeer {
    #[serde(alias = "peerId")]
    pub peer_id: Ipv4Addr,
    #[serde(alias = "routerId")]
    pub router_id: Ipv4Addr,
    #[serde(default)]
    pub hostname: String,
    #[serde(default, alias = "type")]
    pub peer_type: BgpPeerType,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash)]
pub enum BgpPeerType {
    #[serde(alias = "internal")]
    Internal,
    #[serde(alias = "external")]
    External,
    #[serde(alias = "local")]
    Local,
}

impl From<BgpPeerType> for BgpSessionType {
    fn from(x: BgpPeerType) -> Self {
        match x {
            BgpPeerType::Internal => BgpSessionType::IBgpPeer,
            BgpPeerType::External | BgpPeerType::Local => BgpSessionType::EBgp,
        }
    }
}

impl Default for BgpPeerType {
    fn default() -> Self {
        Self::Local
    }
}

fn deserialize_advertised_to<'de, D>(deserializer: D) -> Result<Vec<Ipv4Addr>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct IgnoreObject {}
    let advertised_to: HashMap<Ipv4Addr, IgnoreObject> = Deserialize::deserialize(deserializer)?;
    Ok(advertised_to.into_keys().sorted().collect())
}

fn deserialize_community<'de, D>(deserializer: D) -> Result<BTreeSet<(AsId, u32)>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct BgpCommunity {
        list: Vec<String>,
    }
    let community: BgpCommunity = Deserialize::deserialize(deserializer)?;
    Ok(community
        .list
        .iter()
        .filter_map(|s| s.split_once(':'))
        .filter_map(|(as_id, c)| Some((AsId(as_id.parse::<u32>().ok()?), c.parse::<u32>().ok()?)))
        .collect())
}

fn deserialize_cluster_list<'de, D>(deserializer: D) -> Result<Vec<Ipv4Addr>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct BgpClusterList {
        list: Vec<Ipv4Addr>,
    }
    Ok(BgpClusterList::deserialize(deserializer)?.list)
}

fn deserialize_as_path<'de, D>(deserializer: D) -> Result<Vec<AsId>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct BgpAsPath {
        string: String,
    }
    let path: BgpAsPath = Deserialize::deserialize(deserializer)?;
    Ok(path.string.split_whitespace().filter_map(|x| x.parse::<u32>().ok()).map(AsId).collect())
}

fn deserialize_epoch<'de, D>(deserializer: D) -> Result<OffsetDateTime, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct FrrTime {
        #[serde(deserialize_with = "time::serde::timestamp::deserialize")]
        epoch: OffsetDateTime,
    }
    let time: FrrTime = Deserialize::deserialize(deserializer)?;
    if let Ok(offset) = UtcOffset::current_local_offset() {
        Ok(time.epoch.to_offset(offset))
    } else {
        Ok(time.epoch)
    }
}

fn deserialize_best_path<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct BgpBestPath {
        overall: bool,
    }
    Ok(BgpBestPath::deserialize(deserializer)?.overall)
}

fn deserialize_bgp_next_hops<'de, D>(deserializer: D) -> Result<BgpNextHop, D::Error>
where
    D: Deserializer<'de>,
{
    let next_hops: Vec<BgpNextHop> = Deserialize::deserialize(deserializer)?;
    Ok(next_hops.into_iter().find(|nh| nh.used).unwrap_or_else(|| BgpNextHop {
        ip: Ipv4Addr::new(0, 0, 0, 0),
        hostname: None,
        accessible: false,
        used: true,
        igp_cost: None,
    }))
}

/// Information about a single BGP neighbor
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BgpNeighbor {
    /// The remote AS Id
    #[serde(alias = "remoteAs", deserialize_with = "deserialize_as_id")]
    pub remote_as: AsId,
    /// The remote router ID
    #[serde(alias = "remoteRouterId")]
    pub remote_router_id: Ipv4Addr,
    /// The local AS Id
    #[serde(alias = "localAs", deserialize_with = "deserialize_as_id")]
    pub local_as: AsId,
    /// The local router ID
    #[serde(alias = "localRouterId")]
    pub local_router_id: Ipv4Addr,
    /// The State of the BGP Session
    #[serde(alias = "bgpState")]
    pub state: BgpNeighborState,
    /// The statistics of BGP messages exchanged.
    #[serde(alias = "messageStats")]
    pub statistics: BgpNeighborStatistics,
    /// The minimum route advertisement interval (MRAI) in [ms]
    #[serde(alias = "minBtwnAdvertisementRunsTimerMsecs")]
    pub advertisement_interval: u32,
}

impl BgpNeighbor {
    /// Returns `true` only if the session to the neighbor is established
    pub fn is_established(&self) -> bool {
        self.state.is_established()
    }
}

/// The state of a Bgp Neighbor
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, Deserialize)]
pub enum BgpNeighborState {
    Idle,
    Connect,
    OpenSent,
    OpenConfirm,
    Active,
    Clearing,
    Established,
}

impl BgpNeighborState {
    /// Returns `true` only if `self` is `BgpNeighborState::Established`.
    pub fn is_established(&self) -> bool {
        matches!(self, BgpNeighborState::Established)
    }
}

/// BPG Statistics of how many messages and routes have been exchanged.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BgpNeighborStatistics {
    /// Number of BGP open messages sent to the neighbor.
    #[serde(alias = "opensSent")]
    pub opens_sent: u64,
    /// Number of BGP open messages recewived
    #[serde(alias = "opensRecv")]
    pub opens_recv: u64,
    /// Number of BGP notification messages sent
    #[serde(alias = "notificationsSent")]
    pub notifications_sent: u64,
    /// Number of BGP notification messages received
    #[serde(alias = "notificationsRecv")]
    pub notifications_recv: u64,
    /// Number of BGP update messages sent
    #[serde(alias = "updatesSent")]
    pub updates_sent: u64,
    /// Number of BGP update messages received
    #[serde(alias = "updatesRecv")]
    pub updates_recv: u64,
    /// Number of BGP keepalive messages sent
    #[serde(alias = "keepalivesSent")]
    pub keepalives_sent: u64,
    /// Number of BGP keepalive messages received
    #[serde(alias = "keepalivesRecv")]
    pub keepalives_recv: u64,
}

fn deserialize_as_id<'de, D>(deserializer: D) -> Result<AsId, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(AsId(u32::deserialize(deserializer)?))
}

/// BGP Route Map Object from FRR
#[derive(Deserialize, Debug)]
pub struct BgpRouteMap {
    #[serde(alias = "invoked")]
    _invoked: u32,
    #[serde(alias = "disabledOptimization")]
    _disabled_optimization: bool,
    #[serde(alias = "processedChange")]
    _processed_change: bool,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
pub struct Rule {
    #[serde(alias = "sequenceNumber")]
    pub sequence_number: u32,
    #[serde(rename = "type")]
    pub rule_type: String,
    #[serde(alias = "invoked")]
    _invoked: u32,
    #[serde(alias = "matchClauses")]
    pub match_clauses: Vec<String>,
    #[serde(alias = "setClauses")]
    pub set_clauses: Vec<String>,
    pub action: String,
}

/// IP Prefix List Object from FRR
#[derive(Deserialize, Debug)]
pub struct IpPrefixList {
    #[serde(alias = "addressFamily")]
    _address_family: String,
    pub entries: Vec<PrefixListEntry>,
}

#[derive(Debug, Deserialize)]
pub struct PrefixListEntry {
    #[serde(alias = "sequenceNumber")]
    pub sequence_number: u32,
    #[serde(alias = "type")]
    pub state: String,
    pub prefix: Ipv4Net,
}

/// IpTerm communiation errors
#[derive(Debug, Error)]
pub enum FrrError {
    /// Telnet errors
    #[error("Telnet Error: {0}")]
    Telnet(#[from] TelnetError),
    /// Error while performing configuration
    #[error("Configuration error at line {0}: {1}. Message: {2}")]
    ConfigurationError(usize, String, String),
    /// Unexpected answer while showing the current configuration
    #[error("Unexpected answer for `show running-config`: {0}")]
    ShowConfigUnexpectedAnswer(String),
    /// Cannot parse the received json
    #[error("Cannot parse the json: {0}")]
    ParseError(#[from] serde_json::Error),
    /// Could not parse logs
    #[cfg(feature = "logging")]
    #[error("Could not parse logs")]
    LogError,
}
