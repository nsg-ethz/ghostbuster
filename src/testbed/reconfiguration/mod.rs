use std::collections::{HashMap, HashSet};

use bgpsim::{
    bgp::BgpSessionType,
    prelude::{Network, NetworkFormatter},
    types::{NetworkError, RouterId},
};
use bgpsim_gns3::{Gns3Network, Gns3NetworkError};
use serde::Serialize;
use tracing::info;

use super::{Ospf, P, Q};

pub mod lp;
mod mrai;
pub mod pl;

/// A struct of GNS3 specific configuration settings
#[derive(Debug, Clone, Serialize)]
pub struct Gns3Config {
    /// Router templates to use in GNS3. If a router does not have a specified template
    /// We will pick the latest FRR image available
    pub router_templates: HashMap<RouterId, (&'static str, &'static str)>,
    /// A configuration to apply to the baseline network before conversion to GNS3
    pub pre_config: Option<PreConfig>,
    /// A configuration to apply after the gns3 network has been initialized
    pub post_config: Option<Vec<PostConfig>>,
}

/// Reconfigurations are applied to the GNS3 network
#[derive(Debug, Clone, Serialize)]
pub enum PostConfig {
    /// Set a specified MRAI value on every internal session of the specified routers.
    /// The provided routers must be border routers (internal routers with eBGP sessions)
    SetMrai {
        mrai: u16,
        routers: HashSet<RouterId>,
    },
    /// Quickly change a prefix list on a target router from `deny all` to `permit one` and back
    /// to `deny all` in quick succession
    PrefixList { router: RouterId },
    /// Enable logging on one specific router
    EnableLogging { router: RouterId },
}

impl PostConfig {
    pub fn apply(&self, gns3_net: &mut Gns3Network<P, Q, Ospf>) -> Result<(), Gns3NetworkError> {
        match self {
            PostConfig::SetMrai { mrai, routers } => {
                mrai::configure_mrai_timers(gns3_net, *mrai, routers)
            }
            PostConfig::PrefixList { router } => pl::apply_pl_reconfigs(gns3_net, *router),
            PostConfig::EnableLogging { router } => gns3_net.enable_log(*router),
        }
    }
}

/// Get the border routers of a network
fn get_border_routers(net: &Network) -> HashSet<RouterId> {
    net.internal_routers()
        .filter(|internal| {
            internal
                .bgp
                .get_sessions()
                .iter()
                .any(|s| *s.1 == BgpSessionType::EBgp)
        })
        .map(|r| r.router_id())
        .collect()
}

/// These modifications are applied to the BGPSim network immediately after initial construction and before conversion
/// into GNS3
#[derive(Debug, Clone, Serialize)]
pub enum PreConfig {
    /// Whitelist a specific prefix on a border router. This configuration adds a route map
    /// that only permits a specific prefix to be redistributed on the internal neighbours
    /// of the border router
    Whitelist { prefix: P, router: RouterId },
    /// Convert all routemaps on an eBGP session to work incrementally instead of setting absolute values
    ToIncremental {
        router: RouterId,
        external: RouterId,
    },
}

impl PreConfig {
    pub fn apply(&self, mut net: Network) -> Result<Network, NetworkError> {
        info!("Applying modification to {}", self.router().fmt(&net));
        match self {
            PreConfig::Whitelist { prefix, router } => {
                pl::whitelist_prefix(&mut net, *router, *prefix)?;
            }
            PreConfig::ToIncremental { router, external } => {
                lp::to_incremental(&mut net, *router, *external)?;
            }
        }
        Ok(net)
    }

    pub fn router(&self) -> RouterId {
        match self {
            PreConfig::Whitelist { router, .. } | PreConfig::ToIncremental { router, .. } => {
                *router
            }
        }
    }
}
