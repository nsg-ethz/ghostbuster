use bgpsim::{
    prelude::Network,
    route_map::RouteMapBuilder,
    types::{NetworkError, RouterId},
};
use bgpsim_gns3::{Gns3Network, Gns3NetworkError};
use itertools::Itertools;
use std::{thread::sleep, time::Duration};

use tracing::warn;

use super::{Ospf, P, Q};

/// Modify the normal Gao_Rexford forwarding behaviour for a specific router.
/// In this scenario, we only permit a specific prefix to be advertised within the network.
/// All other prefixes are denied.
pub(crate) fn whitelist_prefix(
    net: &mut Network,
    router: RouterId,
    prefix: P,
) -> Result<(), NetworkError> {
    let border_routers = super::get_border_routers(&net);
    assert!(
        border_routers.contains(&router),
        "Can only configure MRAI on border routers"
    );

    let internal_neighbours = net
        .get_internal_router(router)
        .expect("Trying to whitelist on a non-existing router")
        .bgp
        .get_sessions()
        .iter()
        .filter(|(_, s)| s.is_ibgp())
        .map(|(n, _)| *n)
        .collect_vec();

    let deny_all_rm = RouteMapBuilder::new().order(20).deny().build();
    let allow_one_rm = RouteMapBuilder::new()
        .order(10)
        .allow()
        .match_prefix(prefix)
        .exit()
        .build();

    for neighbour in internal_neighbours {
        net.set_bgp_route_map(
            router,
            neighbour,
            bgpsim::route_map::RouteMapDirection::Outgoing,
            allow_one_rm.clone(),
        )?;
        net.set_bgp_route_map(
            router,
            neighbour,
            bgpsim::route_map::RouteMapDirection::Outgoing,
            deny_all_rm.clone(),
        )?;
    }
    Ok(())
}

/// Apply a series of changes to a prefix list on a router that should turn a (permit one) PL into a (permit all) one
pub(crate) fn apply_pl_reconfigs(
    gns3_net: &mut Gns3Network<P, Q, Ospf>,
    target: RouterId,
) -> Result<(), Gns3NetworkError> {
    let mut target_client = gns3_net
        .get_frr(target)
        .expect("Could not get a telnet connection to the target router");
    let prefix_lists = target_client
        .get_prefix_lists()
        .expect("Could not get the prefix lists");

    // TODO: decide on which prefix lists we apply the bug
    // todo!("Add a way to sanity check the application of this");
    // assert!(
    //     prefix_lists.len() == 1,
    //     "There should be one prefix list on the target router, instead there are {}",
    //     prefix_lists.len()
    // );

    let (name, pl) = prefix_lists.iter().next().unwrap();
    assert!(pl.entries.len() == 1, "More than one entry in prefix list");
    let entry = pl.entries.first().unwrap();

    warn!("Triggering bug on prefix list: '{name}'");
    target_client.configure(format!(
        "ip prefix-list {name} seq {} deny any",
        entry.sequence_number
    ))?;
    sleep(Duration::from_millis(500));
    target_client.configure(format!(
        "ip prefix-list {name} seq {} permit {}",
        entry.sequence_number, entry.prefix
    ))?;
    sleep(Duration::from_millis(500));
    Ok(())
}
