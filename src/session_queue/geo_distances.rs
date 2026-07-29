use std::{
    collections::{HashMap, HashSet},
    iter::zip,
};

use bgpsim::{
    ospf::OspfProcess,
    types::{NetworkDevice, Prefix, RouterId},
};
use geoutils::Location;
use itertools::Itertools;
use ordered_float::NotNan;
use serde::{Deserialize, Serialize};

/// The geo-model computes the propagation delays between two end-hosts.
///
/// The delay of a message from `a` to `b` is computed by constructing the path from `a` to `b`,
/// summing up the distance, and dividing that by the speed of light.
///
/// The delay on a specific link is just the distance of the two endpoints divided by the speed of
/// light.
///
/// This structure is intended to be used when designing event queues.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoDistance {
    default_delay: NotNan<f64>,
    paths: HashMap<(RouterId, RouterId), (NotNan<f64>, usize)>,
    distances: HashMap<(RouterId, RouterId), NotNan<f64>>,
    current_time: NotNan<f64>,
}

/// This is used when there is no location configured on that router.
const GEO_DISTANCE_F_LIGHT_SPEED: f64 = 1.0 / 299792458.0;

impl GeoDistance {
    /// Create a new, empty model queue with given default parameters
    pub fn new(geo_location: &HashMap<RouterId, Location>, default_delay: NotNan<f64>) -> Self {
        // compute the distance between all pairs of routers.
        let distances = geo_location
            .iter()
            .flat_map(|l1| geo_location.iter().map(move |l2| (l1, l2)))
            .map(|((r1, p1), (r2, p2))| {
                (
                    (*r1, *r2),
                    NotNan::new(
                        p1.distance_to(p2)
                            .unwrap_or_else(|_| p1.haversine_distance_to(p2))
                            .meters(),
                    )
                    .unwrap(),
                )
            })
            .collect();

        Self {
            paths: HashMap::new(),
            distances,
            current_time: NotNan::default(),
            default_delay,
        }
    }

    /// Get the distance between two routers
    pub fn get_link_distance(&self, src: RouterId, dst: RouterId) -> Option<NotNan<f64>> {
        self.distances.get(&(src, dst)).copied()
    }

    /// Get the distance and number of hops for the path from src to dst.
    pub fn get_path_distance(&self, src: RouterId, dst: RouterId) -> Option<(NotNan<f64>, usize)> {
        self.paths.get(&(src, dst)).copied()
    }

    /// Set the distance between two nodes in light seconds
    pub fn set_distance(&mut self, src: RouterId, dst: RouterId, dist: f64) {
        let dist = NotNan::new(dist).unwrap();
        self.distances.insert((src, dst), dist);
        self.distances.insert((dst, src), dist);
    }

    /// Recursively update the paths of the routers.
    ///
    /// **TODO**: this function needs improvements!
    fn recursive_compute_paths<P: Prefix, Ospf: OspfProcess>(
        &mut self,
        router: RouterId,
        target: RouterId,
        loop_protection: &mut HashSet<RouterId>,
        routers: &HashMap<RouterId, NetworkDevice<P, Ospf>>,
        path_cache: &mut HashMap<(RouterId, RouterId), Option<Vec<RouterId>>>,
    ) {
        if router == target {
            path_cache.insert((router, target), Some(vec![router]));
            self.paths.insert((router, target), (NotNan::default(), 0));
            return;
        }

        if !loop_protection.insert(router) {
            // router was already present in the loop protection.
            path_cache.insert((router, target), None);
            return;
        }

        // get the next-hop of that router
        let new_path = if let Some(nh) = routers
            .get(&router)
            .and_then(|r| r.as_ref().internal())
            .map(|r| r.ospf.get(target))
            .and_then(|nhs| nhs.first().copied())
        {
            // next-hop is known
            if !path_cache.contains_key(&(nh, target)) {
                // cache the result
                self.recursive_compute_paths(nh, target, loop_protection, routers, path_cache);
            }
            path_cache.get(&(nh, target)).unwrap().as_ref().map(|path| {
                std::iter::once(router)
                    .chain(path.iter().copied())
                    .collect_vec()
            })
        } else {
            // next-hop is unknown.
            None
        };

        if let Some(path) = new_path {
            // compute the delay
            let delay: NotNan<f64> = zip(&path[0..path.len() - 1], &path[1..path.len()])
                .map(|(a, b)| {
                    self.distances
                        .get(&(*a, *b))
                        .copied()
                        .unwrap_or(self.default_delay)
                        * GEO_DISTANCE_F_LIGHT_SPEED
                })
                .sum();
            self.paths.insert((router, target), (delay, path.len()));
            path_cache.insert((router, target), Some(path));
        } else {
            path_cache.insert((router, target), None);
        }

        // remove the router from the loop protection
        loop_protection.remove(&router);
    }

    pub fn update_params<P: Prefix, Ospf: OspfProcess>(
        &mut self,
        routers: &HashMap<RouterId, NetworkDevice<P, Ospf>>,
    ) {
        self.paths.clear();
        // update all paths
        for src in routers.keys() {
            for dst in routers.keys() {
                self.recursive_compute_paths(
                    *src,
                    *dst,
                    &mut HashSet::new(),
                    routers,
                    &mut HashMap::new(),
                );
            }
        }
    }
}
