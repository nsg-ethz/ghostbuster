use bgpsim::{prelude::*, route_map::RouteMapBuilder};
use rand::rngs::StdRng;
use std::iter::zip;

use crate::queue::{OrderedEventQueue, TriggerQueue};

/// Set up an extremely simple line network.
///```
///             :
///        e  -----   b
///             :
///  external       internal
///```
/// The return tuple is (e, b)
#[allow(dead_code)]
pub(crate) fn line_network<P, O, Q>(queue: Q) -> (Network<P, Q, O>, (RouterId, RouterId))
where
    P: Prefix,
    O: OspfImpl,
    Q: EventQueue<P>,
{
    let mut net = net! {
        Prefix = P;
        Ospf = O;
        sessions = {
            e!(1) -> b;
            // Macro will autoadd links for eBGP sessions
        };
        // Make this flexible as far as queues go
        Queue = Q;
        queue = queue;
        // No advertisements either
        return (e, b)
    };
    net.0.set_msg_limit(Some(1000));
    return net;
}

/// Set up an extremely simple line network with two internal routers.
///```
///             :
///        e  -----   b   -----  r
///             :
///  external            internal
///```
/// The return tuple is (e, b, r)
#[allow(dead_code)]
pub(crate) fn long_line_network<P, O, Q>(
    queue: Q,
) -> (Network<P, Q, O>, (RouterId, RouterId, RouterId))
where
    P: Prefix,
    O: OspfImpl,
    Q: EventQueue<P>,
{
    let mut net = net! {
        Prefix = P;
        links = {
            b -> r: 1 ;
        };
        Ospf = O;
        sessions = {
            e!(1) -> b;
            b -> r;
            // Macro will autoadd links for eBGP sessions
        };
        // Make this flexible as far as queues go
        Queue = Q;
        queue = queue;
        // No advertisements either
        return (e, b, r)
    };
    net.0.set_msg_limit(Some(1000));
    return net;
}

/// Set up an extremely simple line network with three internal routers.
///```
///           :                      :
///      e1  ---  b1 ---  r ---  b2 ---  e2
///           :                      :
///  external          internal        external
///```
/// The return tuple is ((e1,e2), (b1,b2), r)
#[allow(dead_code)]
pub(crate) fn line_reflector_network<P, O, Q>(
    queue: Q,
) -> (
    Network<P, Q, O>,
    ((RouterId, RouterId), (RouterId, RouterId), RouterId),
)
where
    P: Prefix,
    O: OspfImpl,
    Q: EventQueue<P>,
{
    let mut net = net! {
        Prefix = P;
        links = {
            b1 -> r: 1 ;
            b2 -> r: 1 ;
        };
        Ospf = O;
        sessions = {
            e1!(1) -> b1;
            e2!(2) -> b2;

            r -> b1: client;
            r -> b2: client;
            // Macro will autoadd links for eBGP sessions
        };
        // Make this flexible as far as queues go
        Queue = Q;
        queue = queue;
        // No advertisements either
        return ((e1, e2), (b1, b2), r)
    };
    net.0.set_msg_limit(Some(1000));
    return net;
}

/// Set up an extremely simple Y network with two external routers.
///```
///      e1 -----┐   
///           :  |
///           :  b ----- r
///           :  |
///      e2 -----┘
///  
///  external     internal
///```
/// The return tuple is (e1, e2, b, r)
#[allow(dead_code)]
pub(crate) fn y_network<P, O, Q>(
    queue: Q,
) -> (Network<P, Q, O>, (RouterId, RouterId, RouterId, RouterId))
where
    P: Prefix,
    O: OspfImpl,
    Q: EventQueue<P>,
{
    let mut net = net! {
        Prefix = P;
        links = {
            b -> r: 1 ;
        };
        Ospf = O;
        sessions = {
            e1!(1) -> b;
            e2!(2) -> b;
            b -> r;
            // Macro will autoadd links for eBGP sessions
        };
        // Make this flexible as far as queues go
        Queue = Q;
        queue = queue;
        // No advertisements either
        return (e1, e2, b, r)
    };
    net.0.set_msg_limit(Some(1000));
    return net;
}

/// Set up a simple network shaped like an $\ni$ mathematical symbol with three external routers,
/// 3 border routers, and 1 internal router acting as a route reflector.
///```
///      e1 ----- b1 ------┐   
///           :            |
///      e2 ----- b2 ----- r
///           :            |
///      e3 ----- b3 ------┘
///  
///  external     internal
///```
/// The return tuple is (net, (\[e1, e2, e3\], \[b1, b2, b3\], r))
#[allow(dead_code)]
pub(crate) fn e_network<P, O, Q>(
    queue: Q,
) -> (Network<P, Q, O>, ([RouterId; 3], [RouterId; 3], RouterId))
where
    P: Prefix,
    O: OspfImpl,
    Q: EventQueue<P>,
{
    let (mut net, ((e1, e2, e3), (b1, b2, b3), r)) = net! {
        Prefix = P;
        links = {
            b1 -> r: 1 ;
            b2 -> r: 1 ;
            b3 -> r: 1 ;
        };
        Ospf = O;
        sessions = {
            e1!(1) -> b1;
            e2!(2) -> b2;
            e3!(3) -> b3;
            r -> b1: client;
            r -> b2: client;
            r -> b3: client;
            // Macro will autoadd links for eBGP sessions
        };
        // Make this flexible as far as queues go
        Queue = Q;
        queue = queue;
        // No advertisements either
        return ((e1, e2, e3), (b1, b2, b3), r)
    };
    net.set_msg_limit(Some(1000));
    (net, ([e1, e2, e3], [b1, b2, b3], r))
}

/// Returns an e-network already configured with certain routemaps, ready for testing.
/// All ASes have already sent out their messages. The network is in the intial state and
/// in automatic simulation mode.
///
/// The argument `rng` is used to seed the queue. Setting it to `None` will order the queue
/// deterministically.
///```
///    (200) e1 ----- b1 ------┐   
///               :            |
///    (150) e2 ----- b2 ----- r
///               :            |
///    (100) e3 ----- b3 ------┘
///  
///     external     internal
///```
/// The return tuple is (net, (\[e1, e2, e3\], \[b1, b2, b3\], r))
#[allow(dead_code)]
pub fn e_network_route_map_scenario(
    rng: Option<StdRng>,
) -> (
    Network<SinglePrefix, OrderedEventQueue<TriggerQueue<SinglePrefix>>>,
    ([RouterId; 3], [RouterId; 3], RouterId),
) {
    // Initialize a simple network
    let (mut net, (e, b, r)) =
        e_network::<SinglePrefix, GlobalOspf, OrderedEventQueue<TriggerQueue<SinglePrefix>>>(
            OrderedEventQueue::new(rng, TriggerQueue::new()),
        );
    // Add three route maps assigning local preferences to the incoming routes from the external routers
    for (lp, i) in zip([200, 150, 100], 0..3) {
        let route_map = RouteMapBuilder::new()
            .order(10)
            .allow()
            .set_local_pref(lp)
            .build();
        net.set_bgp_route_map(
            b[i],
            e[i],
            bgpsim::route_map::RouteMapDirection::Incoming,
            route_map,
        )
        .unwrap();
    }
    // Advertise the same prefix for all three
    let mut as_id = 1;
    for external in e.iter() {
        net.advertise_external_route(*external, SinglePrefix::from(0), [as_id], None, None)
            .unwrap();
        as_id += 1;
    }

    (net, (e, b, r))
}

/// Set up a simple network shaped like a fan. It has three external routers,
/// each connected to a single border router, which in turn is connected to a single internal route.
///```
///       e1 ------┐   
///            :   |         
///       e2 ----- b ----- r
///            :   |         
///       e3 ------┘
///            :
///  external  :  internal
///```
/// The return tuple is (net, (\[e1, e2, e3\], b, r))
#[allow(dead_code)]
pub(crate) fn fan_network<P, O, Q>(
    queue: Q,
) -> (Network<P, Q, O>, ([RouterId; 3], RouterId, RouterId))
where
    P: Prefix,
    O: OspfImpl,
    Q: EventQueue<P>,
{
    let (mut net, ((e1, e2, e3), b, r)) = net! {
        Prefix = P;
        links = {
            b -> r: 1 ;
        };
        Ospf = O;
        sessions = {
            e1!(1) -> b;
            e2!(2) -> b;
            e3!(3) -> b;
            r -> b: peer;
            // Macro will autoadd links for eBGP sessions
        };
        // Make this flexible as far as queues go
        Queue = Q;
        queue = queue;
        // No advertisements either
        return ((e1, e2, e3), b, r)
    };
    net.set_msg_limit(Some(1000));
    (net, ([e1, e2, e3], b, r))
}

/// Set up a network used for timing experiments. It has 5 route reflectors used to normalize for timing
/// 1 border routers, and 1 internal router.
///```
///            :     r1     :
///            :   / :  \   :
///       e1 -----b--r3--i----- e2
///            :   \ :  /   :
///            :     r5     :
///  
///  external     internal
///```
/// The return tuple is (net, (\[e1, e2\], b, \[r1, .., r5\], i))
#[allow(dead_code)]
pub(crate) fn timing_network<P, O, Q>(
    queue: Q,
) -> (
    Network<P, Q, O>,
    ([RouterId; 2], RouterId, [RouterId; 5], RouterId),
)
where
    P: Prefix,
    O: OspfImpl,
    Q: EventQueue<P>,
{
    let (mut net, ((e1, e2), b, r, i)) = net! {
        Prefix = P;
        links = {
            b -> r1: 1 ;
            b -> r2: 1 ;
            b -> r3: 1 ;
            b -> r4: 1 ;
            b -> r5: 1 ;
            r1-> i:  1 ;
            r2-> i:  1 ;
            r3-> i:  1 ;
            r4-> i:  1 ;
            r5-> i:  1 ;
        };
        Ospf = O;
        sessions = {
            e1!(1) -> b;
            e2!(2) -> i;
            r1 -> b: client;
            r2 -> b: client;
            r3 -> b: client;
            r4 -> b: client;
            r5 -> b: client;
            r1 -> i: client;
            r2 -> i: client;
            r3 -> i: client;
            r4 -> i: client;
            r5 -> i: client;
            // Macro will autoadd links for eBGP sessions
        };
        // Make this flexible as far as queues go
        Queue = Q;
        queue = queue;
        // No advertisements either
        return ((e1, e2), b, (r1, r2, r3, r4, r5), i)
    };
    let r_arr = [r.0, r.1, r.2, r.3, r.4];
    // Add five route maps assigning local preferences to the incoming routes
    for (lp, i) in zip([200, 175, 150, 125, 100], 0..5) {
        let route_map = RouteMapBuilder::new()
            .order(10)
            .allow()
            .set_local_pref(lp)
            .build();
        net.set_bgp_route_map(
            b,
            r_arr[i],
            bgpsim::route_map::RouteMapDirection::Outgoing,
            route_map,
        )
        .unwrap();
    }

    net.set_msg_limit(Some(1000));
    (net, ([e1, e2], b, r_arr, i))
}

/// Set up a simple network shaped like a triangle with three external routers and
/// 3 border routers, configured to run full mesh.
///```
///               e1   
///               |
///               b1
///              /  \
///             /    \
///     e2 -- b2 ---- b3 -- e3
///```
/// The return tuple is (net, (\[e1, e2, e3\], \[b1, b2, b3\]))
#[allow(dead_code)]
pub(crate) fn triangle_network<P, O, Q>(
    queue: Q,
) -> (Network<P, Q, O>, ([RouterId; 3], [RouterId; 3]))
where
    P: Prefix,
    O: OspfImpl,
    Q: EventQueue<P>,
{
    let (mut net, ((e1, e2, e3), (b1, b2, b3))) = net! {
        Prefix = P;
        links = {
            b1 -> b2: 1 ;
            b2 -> b3: 1 ;
            b3 -> b1: 1 ;
        };
        Ospf = O;
        sessions = {
            e1!(1) -> b1;
            e2!(2) -> b2;
            e3!(3) -> b3;
            b1 -> b2: peer;
            b2 -> b3: peer;
            b3 -> b1: peer;
            // Macro will autoadd links for eBGP sessions
        };
        // Make this flexible as far as queues go
        Queue = Q;
        queue = queue;
        // No advertisements either
        return ((e1, e2, e3), (b1, b2, b3))
    };
    net.set_msg_limit(Some(1000));
    (net, ([e1, e2, e3], [b1, b2, b3]))
}

/// Returns a triangle-network already configured with certain routemaps, ready for testing.
/// All ASes have already sent out their messages. The network is in the intial state and
/// in automatic simulation mode.
///
/// The argument `rng` is used to seed the queue. Setting it to `None` will order the queue
/// deterministically.
///```
///           (200) e1   
///                 |
///                 b1
///                /  \
///  (150)        /    \        (100)
///       e2 -- b2 ---- b3 -- e3
///```
/// The return tuple is (net, (\[e1, e2, e3\], \[b1, b2, b3\]))
#[allow(dead_code)]
pub(crate) fn triangle_network_route_map_scenario(
    rng: Option<StdRng>,
) -> (
    Network<SinglePrefix, OrderedEventQueue<TriggerQueue<SinglePrefix>>>,
    ([RouterId; 3], [RouterId; 3]),
) {
    // Initialize a simple network
    let (mut net, (e, b)) = triangle_network::<
        SinglePrefix,
        GlobalOspf,
        OrderedEventQueue<TriggerQueue<SinglePrefix>>,
    >(OrderedEventQueue::new(rng, TriggerQueue::new()));
    // Add three route maps assigning local preferences to the incoming routes from the external routers
    for (lp, i) in zip([200, 150, 100], 0..3) {
        let route_map = RouteMapBuilder::new()
            .order(10)
            .allow()
            .set_local_pref(lp)
            .build();
        net.set_bgp_route_map(
            b[i],
            e[i],
            bgpsim::route_map::RouteMapDirection::Incoming,
            route_map,
        )
        .unwrap();
    }
    // Advertise the same prefix for all three
    for external in e.iter() {
        net.advertise_external_route(*external, SinglePrefix::from(0), [1, 2, 3], None, None)
            .unwrap();
    }

    (net, (e, b))
}

/// This macro checks the forwarding behavior of a router in a network.
///
/// # Syntax
///
/// This macro asserts that the router identified by `$router` in the network `$net`
/// has a next hop equal to `$next_hop` for the route to the prefix `0`. If `$next_hop`
/// is `None`, it asserts that no route is present for the prefix `0`.
///
/// - `$net`: The network object containing the routers.
/// - `$router`: The identifier of the router to check.
/// - `$next_hop`: The expected next hop for the router. If `None`, it checks that no route is present.
///
/// # Example
///
/// ```rust
/// assert_forwarding!(network, router_id, Some(expected_next_hop));
/// assert_forwarding!(network, router_id, None);
/// ```
#[macro_export]
macro_rules! assert_forwarding {
    ($net:expr, $router:expr, $next_hop:expr) => {
        let Ok(internal_router) = $net.get_internal_router($router) else {
            panic!("Router {} not found in network", $router.fmt(&$net));
        };
        let route = internal_router.bgp.get_route(SinglePrefix::from(0));

        if let Some(next_hop) = $next_hop {
            assert!(
                route.is_some(),
                "Router {} does not have a route and is supposed to have one through",
                $router.fmt(&$net)
            );
            assert_eq!(
                route.unwrap().route.next_hop,
                next_hop,
                "Router {} has wrong next hop: {}",
                $router.fmt(&$net),
                route.unwrap().route.next_hop.fmt(&$net)
            );
        } else {
            assert!(
                route.is_none(),
                "Router {} has a route, but is not supposed to have any",
                $router.fmt(&$net)
            );
        }
    };
}

mod test_gns3;
mod test_network;
