// BgpSim-GNS3: Control and interact with GNS3 from BgpSim
// Copyright (C) 2022-2023 Tibor Schneider <sctibor@ethz.ch>
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

use function_name::named;
use std::{concat, time::Duration};
use test_log::test;

use bgpsim::{
    builder::{equal_preferences, uniform_integer_link_weight, NetworkBuilder},
    config::{
        ConfigExpr::{BgpRouteMap, BgpSession, IgpLinkWeight},
        ConfigModifier, NetworkConfig,
    },
    prelude::*,
    route_map::{RouteMapBuilder, RouteMapDirection::Incoming},
    types::SimplePrefix as P,
};

use crate::Gns3Network;

macro_rules! cmp_net {
    ($gnet: ident, $net: ident) => {
        $gnet.wait_for_convergence(Duration::from_secs(300), None).unwrap();
        // std::thread::sleep(Duration::from_secs(20));
        assert!($gnet.equal_forwarding_state(&$net).unwrap());
        assert!($gnet.equal_bgp_state(&$net).unwrap());
    };
}

#[test]
#[named]
fn update_link_weight() {
    let mut net = Network::build_complete_graph(BasicEventQueue::new(), 4);
    net.build_external_routers(|_, _| [0.into(), 1.into()], ()).unwrap();
    net.build_link_weights(uniform_integer_link_weight, (10, 20)).unwrap();
    net.set_link_weight(2.into(), 0.into(), 1000.0).unwrap();
    net.build_ibgp_full_mesh().unwrap();
    net.build_ebgp_sessions().unwrap();
    net.build_advertisements(P::from(0), equal_preferences, 2).unwrap();
    net.build_advertisements(P::from(1), equal_preferences, 2).unwrap();

    let net_copy = net.clone();
    let mut gnet =
        Gns3Network::new(concat!("updates::", function_name!()), &net_copy, None, None).unwrap();

    cmp_net!(gnet, net);

    // change the link weight to something lower
    let cmd = ConfigModifier::Update {
        from: IgpLinkWeight { source: 2.into(), target: 0.into(), weight: 1000.0 },
        to: IgpLinkWeight { source: 2.into(), target: 0.into(), weight: 1.0 },
    };
    net.apply_modifier(&cmd).unwrap();
    gnet.apply_modifier(&cmd).unwrap();

    cmp_net!(gnet, net);

    // change the link weight to something lower
    let cmd =
        ConfigModifier::Remove(IgpLinkWeight { source: 2.into(), target: 0.into(), weight: 1.0 });
    net.apply_modifier(&cmd).unwrap();
    gnet.apply_modifier(&cmd).unwrap();
    cmp_net!(gnet, net);
}

#[test]
#[named]
fn update_bgp_session() {
    let mut net = Network::build_complete_graph(BasicEventQueue::new(), 4);
    net.build_external_routers(|_, _| [0.into(), 1.into()], ()).unwrap();
    net.build_link_weights(uniform_integer_link_weight, (10, 20)).unwrap();
    net.build_ibgp_full_mesh().unwrap();
    net.build_ebgp_sessions().unwrap();
    net.build_advertisements(P::from(0), equal_preferences, 2).unwrap();
    net.build_advertisements(P::from(1), equal_preferences, 2).unwrap();

    let net_copy = net.clone();
    let mut gnet =
        Gns3Network::new(concat!("updates::", function_name!()), &net_copy, None, None).unwrap();

    cmp_net!(gnet, net);

    // change the link weight to something lower
    let cmd = ConfigModifier::Update {
        from: BgpSession {
            source: 2.into(),
            target: 0.into(),
            session_type: BgpSessionType::IBgpPeer,
        },
        to: BgpSession {
            source: 2.into(),
            target: 0.into(),
            session_type: BgpSessionType::IBgpClient,
        },
    };
    net.apply_modifier(&cmd).unwrap();
    gnet.apply_modifier(&cmd).unwrap();
    cmp_net!(gnet, net);

    // change the link weight to something lower
    let cmd = ConfigModifier::Update {
        from: BgpSession {
            source: 2.into(),
            target: 1.into(),
            session_type: BgpSessionType::IBgpPeer,
        },
        to: BgpSession {
            source: 2.into(),
            target: 1.into(),
            session_type: BgpSessionType::IBgpClient,
        },
    };
    net.apply_modifier(&cmd).unwrap();
    gnet.apply_modifier(&cmd).unwrap();
    cmp_net!(gnet, net);

    // change the link weight to something lower
    let cmd = ConfigModifier::Update {
        from: BgpSession {
            source: 2.into(),
            target: 3.into(),
            session_type: BgpSessionType::IBgpPeer,
        },
        to: BgpSession {
            source: 2.into(),
            target: 3.into(),
            session_type: BgpSessionType::IBgpClient,
        },
    };
    net.apply_modifier(&cmd).unwrap();
    gnet.apply_modifier(&cmd).unwrap();
    cmp_net!(gnet, net);

    // change the link weight to something lower
    let cmd = ConfigModifier::Remove(BgpSession {
        source: 0.into(),
        target: 1.into(),
        session_type: BgpSessionType::IBgpPeer,
    });
    net.apply_modifier(&cmd).unwrap();
    gnet.apply_modifier(&cmd).unwrap();
    cmp_net!(gnet, net);

    // change the link weight to something lower
    let cmd = ConfigModifier::Remove(BgpSession {
        source: 0.into(),
        target: 3.into(),
        session_type: BgpSessionType::IBgpPeer,
    });
    net.apply_modifier(&cmd).unwrap();
    gnet.apply_modifier(&cmd).unwrap();
    cmp_net!(gnet, net);

    // change the link weight to something lower
    let cmd = ConfigModifier::Remove(BgpSession {
        source: 1.into(),
        target: 3.into(),
        session_type: BgpSessionType::IBgpPeer,
    });
    net.apply_modifier(&cmd).unwrap();
    gnet.apply_modifier(&cmd).unwrap();
    cmp_net!(gnet, net);
}

#[test]
#[named]
fn update_route_map() {
    let mut net = Network::build_complete_graph(BasicEventQueue::new(), 4);
    net.build_external_routers(|_, _| [0.into(), 1.into()], ()).unwrap();
    net.build_link_weights(uniform_integer_link_weight, (10, 20)).unwrap();
    net.build_ibgp_full_mesh().unwrap();
    net.build_ebgp_sessions().unwrap();
    net.build_advertisements(P::from(0), equal_preferences, 2).unwrap();
    net.build_advertisements(P::from(1), equal_preferences, 2).unwrap();

    let net_copy = net.clone();
    let mut gnet =
        Gns3Network::new(concat!("updates::", function_name!()), &net_copy, None, None).unwrap();

    cmp_net!(gnet, net);

    // add the first route-map to increase the local pref.
    let cmd = ConfigModifier::Insert(BgpRouteMap {
        router: 0.into(),
        neighbor: 4.into(),
        direction: Incoming,
        map: RouteMapBuilder::new().allow().order(10).set_local_pref(200).build(),
    });
    net.apply_modifier(&cmd).unwrap();
    gnet.apply_modifier(&cmd).unwrap();
    cmp_net!(gnet, net);

    // add the second route-map to set the community value.
    let cmd = ConfigModifier::Insert(BgpRouteMap {
        router: 0.into(),
        neighbor: 4.into(),
        direction: Incoming,
        map: RouteMapBuilder::new()
            .allow()
            .order(20)
            .match_prefix(P::from(0))
            .set_community(20)
            .build(),
    });
    net.apply_modifier(&cmd).unwrap();
    gnet.apply_modifier(&cmd).unwrap();
    cmp_net!(gnet, net);

    // update router 1 to decrease the local pref if community 20 is set
    let cmd = ConfigModifier::Insert(BgpRouteMap {
        router: 1.into(),
        neighbor: 0.into(),
        direction: Incoming,
        map: RouteMapBuilder::new()
            .allow()
            .order(10)
            .match_community(20)
            .set_local_pref(50)
            .build(),
    });
    net.apply_modifier(&cmd).unwrap();
    gnet.apply_modifier(&cmd).unwrap();
    cmp_net!(gnet, net);

    // update the first route map such that it will only affect prefix 1
    let cmd = ConfigModifier::Update {
        from: BgpRouteMap {
            router: 0.into(),
            neighbor: 4.into(),
            direction: Incoming,
            map: RouteMapBuilder::new().allow().order(10).set_local_pref(100).build(),
        },
        to: BgpRouteMap {
            router: 0.into(),
            neighbor: 4.into(),
            direction: Incoming,
            map: RouteMapBuilder::new()
                .allow()
                .order(10)
                .match_prefix(P::from(1))
                .set_local_pref(200)
                .build(),
        },
    };
    net.apply_modifier(&cmd).unwrap();
    gnet.apply_modifier(&cmd).unwrap();
    cmp_net!(gnet, net);

    // remove that route-map again
    let cmd = ConfigModifier::Remove(BgpRouteMap {
        router: 0.into(),
        neighbor: 4.into(),
        direction: Incoming,
        map: RouteMapBuilder::new()
            .allow()
            .order(10)
            .match_prefix(P::from(1))
            .set_local_pref(200)
            .build(),
    });
    net.apply_modifier(&cmd).unwrap();
    gnet.apply_modifier(&cmd).unwrap();
    cmp_net!(gnet, net);
}
