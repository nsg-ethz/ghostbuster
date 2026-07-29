use super::P;
use bgpsim::{
    prelude::NetworkFormatter,
    types::{AsId, Prefix, RouterId},
};
use serde::{Deserialize, Serialize};

/// At each time step the Simulation will attempt to apply multiple events in quick succession
pub type SimulationTimeStep = Vec<SimulationEvent>;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Possible events we can apply to the simulation
pub enum SimulationEvent {
    /// A sequence of possible updates/withdrawals from external neighbours to apply to the simulation
    External(ExternalEvent),
    /// A reconfiguration task to perform
    Reconfiguration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalEvent {
    /// Where did this event originate?
    pub external_neighbor: RouterId,
    /// What prefix was this event for?
    pub prefix: P,
    /// Is this event an update or a withdrawal? If the route is None, it is a withdrawal
    pub route: Option<Vec<AsId>>,
}

impl<'a, PR, Q, Ospf> NetworkFormatter<'a, PR, Q, Ospf> for SimulationEvent
where
    PR: Prefix,
    Q: bgpsim::event::EventQueue<PR>,
    Ospf: bgpsim::ospf::OspfImpl,
{
    fn fmt(&self, net: &'a bgpsim::network::Network<PR, Q, Ospf>) -> String {
        match self {
            SimulationEvent::External(ExternalEvent {
                external_neighbor,
                prefix,
                route,
            }) => {
                if let Some(as_path) = route {
                    format!(
                        "Advertising {prefix} from {} with AS path {:?}",
                        external_neighbor.fmt(&net),
                        as_path
                    )
                } else {
                    format!("Withdrawing {prefix} from {}", external_neighbor.fmt(&net))
                }
            }
            SimulationEvent::Reconfiguration => "Reconfiguring".to_string(),
        }
    }
}
