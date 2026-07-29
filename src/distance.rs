use std::iter::zip;

// Distance metric
// Defines the distance between two networks
use bgpsim::{
    policies::{FwPolicy, Policy, PolicyError},
    prelude::*,
};

//
// TODO: eventually I want to add more than just one type of distance
//

pub trait NetworkDistance<P, Q, Ospf>
where
    P: Prefix,
    Q: EventQueue<P>,
    Ospf: OspfImpl,
{
    fn distance(&self, other: &Network<P, Q, Ospf>) -> u32;
}

impl<Q1, Q2, Ospf> NetworkDistance<SinglePrefix, Q2, Ospf> for Network<SinglePrefix, Q1, Ospf>
where
    Q1: EventQueue<SinglePrefix>,
    Q2: EventQueue<SinglePrefix>,
    Ospf: OspfImpl,
{
    fn distance(&self, other: &Network<SinglePrefix, Q2, Ospf>) -> u32 {
        // Define how the distance is computed
        // For now we only look at the forwarding states of each network's router and compare the differences
        zip(self.internal_routers(), other.internal_routers()).fold(0, |sum, (ours, theirs)| {
            sum + if ours.bgp.get(0.into()) != theirs.bgp.get(0.into()) {
                1
            } else {
                0
            }
        })
    }
}

pub trait NetworkPolicies {
    fn blackholes(&self) -> u32;
    fn forwarding_loops(&self) -> u32;
}

impl<Q, Ospf> NetworkPolicies for Network<SinglePrefix, Q, Ospf>
where
    Q: EventQueue<SinglePrefix>,
    Ospf: OspfImpl,
{
    fn blackholes(&self) -> u32 {
        self.internal_indices()
            .map(|r| {
                match FwPolicy::Reachable(r, 0.into()).check(&mut self.get_forwarding_state()) {
                    // We can grab more info if we need it
                    Err(PolicyError::BlackHole { .. }) => 1,
                    _ => 0,
                }
            })
            .sum()
    }

    fn forwarding_loops(&self) -> u32 {
        self.internal_indices()
            .map(|r| {
                match FwPolicy::Reachable(r, 0.into()).check(&mut self.get_forwarding_state()) {
                    // We can grab more info if we need it
                    Err(PolicyError::ForwardingLoop { .. }) => 1,
                    _ => 0,
                }
            })
            .sum()
    }
}

#[cfg(test)]
pub(crate) mod test_distance {
    // Test the distance metric with the line network
    use crate::{distance::NetworkDistance, tests::*};
    use bgpsim::prelude::*;

    #[test]
    fn test_distance() {
        let (net1, _) = line_network::<SinglePrefix, GlobalOspf, BasicEventQueue<SinglePrefix>>(
            BasicEventQueue::default(),
        );
        let (mut net2, (e, _)) =
            line_network::<SinglePrefix, GlobalOspf, BasicEventQueue<SinglePrefix>>(
                BasicEventQueue::default(),
            );

        // The networks are the same, so the distance should be 0
        assert_eq!(net1.distance(&net2), 0);

        // Now lets only advertise a route in one of the networks
        net2.advertise_external_route(e, SinglePrefix::from(0), [1, 2, 3], None, None)
            .unwrap();

        // Now let's change the forwarding state of one of the routers
        assert_eq!(net1.distance(&net2), 1);
    }
}
