use bgpsim::{
    prelude::{Network, NetworkFormatter},
    route_map::{RouteMapDirection, RouteMapSet},
    types::{NetworkError, RouterId},
};
use itertools::Itertools;
use tracing::warn;

/// Modify all the routemaps on a session to set the local preference relatively to the default value
/// instead of absolutely
pub(crate) fn to_incremental(
    net: &mut Network,
    router: RouterId,
    external: RouterId,
) -> Result<(), NetworkError> {
    assert!(
        net.external_indices().contains(&external),
        "Specified external router is not external"
    );
    let border_router = net
        .get_internal_router_mut(router)
        .expect("This should be an internal router");
    assert!(
        border_router.bgp.get_sessions().contains_key(&external),
        "The specified border router has no session to the external router"
    );

    // Get all the route maps that set the local preference
    let rm_orders_with_lp_set = border_router
        .bgp
        .get_route_maps(external, RouteMapDirection::Incoming)
        .iter()
        .filter(|rm| {
            rm.set
                .iter()
                .any(|s| matches!(s, RouteMapSet::LocalPref(_)))
        })
        .map(|rm| rm.order)
        .collect_vec();

    // Convert those absolute sets into relative ones
    for order in rm_orders_with_lp_set.iter() {
        // SAFETY: there should be no routes at this point
        unsafe {
            let rm = border_router
                .bgp
                .get_route_map_mut(external, RouteMapDirection::Incoming, *order)
                .expect("Route-map disappeared unexpectedly");

            for set in &mut rm.set {
                if let RouteMapSet::LocalPref(Some(x)) = *set {
                    *set = RouteMapSet::LocalPrefDelta(
                        i32::try_from(x).expect("Could not convert") - 100,
                    );
                }
            }
        }
    }

    if rm_orders_with_lp_set.is_empty() {
        warn!("There are no route maps that set the local preference on incoming sessions from ({}) to ({})", external.fmt(&net),  router.fmt(&net))
    }

    Ok(())
}

#[cfg(test)]
pub(crate) mod lp {
    use bgpsim::builder::GaoRexfordPeerType;
    use bgpsim::prelude::{NetworkBuilder, NetworkFormatter};
    use log::info;

    use crate::testbed::reconfiguration::PreConfig;
    use crate::testbed::{Ospf, P, Q};
    use crate::tests::e_network;

    use test_log::test;

    #[test]
    fn test_lp_routemaps() {
        let (mut net, (e, b, _)) = e_network::<P, Ospf, Q>(Q::default());

        net.build_gao_rexford_policies(GaoRexfordPeerType::random, (0.3, 0.3))
            .unwrap();

        for (&external, router) in e.iter().zip(b) {
            info!(
                "Before modification, the route map between {} and {} is: {}",
                router.fmt(&net),
                external.fmt(&net),
                net.get_internal_router(router)
                    .unwrap()
                    .bgp
                    .get_route_maps(external, bgpsim::route_map::RouteMapDirection::Incoming)
                    .fmt_multiline(&net)
            );
            let modification = PreConfig::ToIncremental { router, external };
            net = modification.apply(net).unwrap();
        }

        for (&external, router) in e.iter().zip(b) {
            // Print the route maps
            info!(
                "After modification: {}",
                net.get_internal_router(router)
                    .unwrap()
                    .bgp
                    .get_route_maps(external, bgpsim::route_map::RouteMapDirection::Incoming)
                    .fmt_multiline(&net)
            )
        }
    }
}
