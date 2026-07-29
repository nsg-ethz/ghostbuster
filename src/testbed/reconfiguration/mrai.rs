use std::collections::HashSet;

use bgpsim::{bgp::BgpSessionType, export::Addressor, prelude::NetworkFormatter, types::RouterId};
use bgpsim_gns3::{Gns3Network, Gns3NetworkError};
use tracing::debug;

use super::{Ospf, P, Q};

pub fn configure_mrai_timers(
    gns3_net: &mut Gns3Network<P, Q, Ospf>,
    mrai: u16,
    routers: &HashSet<RouterId>,
) -> Result<(), Gns3NetworkError> {
    let net = gns3_net.get_net();
    let border_routers = super::get_border_routers(&net);

    for router_id in routers {
        assert!(
            border_routers.contains(&router_id),
            "Can only configure MRAI on border routers"
        );

        let router = net.get_internal_router(*router_id).unwrap();
        let mut frr_client = gns3_net.get_frr(*router_id).unwrap();
        for (neighbor_id, session_type) in router.bgp.get_sessions() {
            if *session_type != BgpSessionType::EBgp {
                let neighbor_addr = gns3_net
                    .get_addressor()
                    .try_get_router_address(*neighbor_id)
                    .expect("Should have an address");
                frr_client.set_advertisement_interval(mrai, neighbor_addr)?;
                debug!(
                    "Set an MRAI value of {}s on the ({})-({}) session",
                    mrai,
                    router_id.fmt(&net),
                    neighbor_id.fmt(&net)
                )
            }
        }
    }

    Ok(())
}
