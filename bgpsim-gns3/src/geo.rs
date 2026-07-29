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

//! Module for setting the delay on links based on information from `bgpsim::topology_zoo`.

use std::collections::HashMap;

use bgpsim::prelude::*;
use geoutils::Location;
use graph_layout::{fruchterman_reingold, P2d};
use itertools::Itertools;
use log::debug;
use ordered_float::OrderedFloat as OrdF;
use rand::prelude::*;

use crate::{gns3::links::Gns3LinkFilters, Gns3Network, Gns3NetworkError};

// Light speed in [m/s]
const LIGHT_SPEED: f64 = 299792458.0;
const INV_LIGHT_SPEED: f64 = 1.0 / LIGHT_SPEED;
const JITTER_FACTOR: f64 = 0.05;

const MIN_X: isize = -1000;
const MAX_X: isize = 1000;
const MIN_Y: isize = 1000;
const MAX_Y: isize = -1000;
const X_A: f64 = (MAX_X - MIN_X) as f64;
const X_M: f64 = MIN_X as f64;
const Y_A: f64 = (MAX_Y - MIN_Y) as f64;
const Y_M: f64 = MIN_Y as f64;
const RAND_OFFSET: f64 = 0.05;
const CLIENT_OFFSET_X: isize = 0;
const CLIENT_OFFSET_Y: isize = 80;

impl<'n, P: Prefix, Q, Ospf: OspfImpl> Gns3Network<'n, P, Q, Ospf> {
    /// Set the delay on links based on the provided geo-information. At the same time, position the
    /// nodes according to the geo information in GNS3.
    pub fn set_geo_delay(
        &mut self,
        geo: &HashMap<RouterId, Location>,
    ) -> Result<(), Gns3NetworkError> {
        // also set the geo layout
        let mut geo = geo.clone();
        self.geo_layout(&mut geo)?;

        for e in self.net.get_topology().edge_indices() {
            let (a, b) = self.net.get_topology().edge_endpoints(e).unwrap();
            if a > b {
                continue;
            }

            // get the distance between a and b
            let zero = Location::new(0, 0);
            let a_loc = *geo.get(&a).unwrap_or(&zero);
            let b_loc = *geo.get(&b).unwrap_or(&zero);

            // check if either a_loc or b_loc is pointing to zero.
            if a_loc == zero || b_loc == zero {
                continue;
            }

            let distance = a_loc
                .distance_to(&b_loc)
                .unwrap_or_else(|_| a_loc.haversine_distance_to(&b_loc))
                .meters();
            let delay = distance * INV_LIGHT_SPEED;
            let jitter = delay * JITTER_FACTOR;

            self.set_link_delay(a, b, (delay * 1000.0) as u64, (jitter * 1000.0) as u64)?;
        }

        Ok(())
    }

    /// Set the delay \[ms\] and jitter \[ms\] on a specific link
    pub fn set_link_delay(
        &self,
        a: RouterId,
        b: RouterId,
        delay: u64,
        jitter: u64,
    ) -> Result<(), Gns3NetworkError> {
        let link_id = self.links.get(&(a, b).into()).unwrap();

        debug!("setting delay of {} ms between {} and {}", delay, a.fmt(self.net), b.fmt(self.net),);

        self.project.set_link_filters(
            *link_id,
            Gns3LinkFilters { delay: Some((delay, jitter)), ..Default::default() },
        )?;

        Ok(())
    }

    /// Layout all nodes in GNS3 according to their geo-information. This function will create a
    /// cylindrical projection, using latitude and longitude values directly. In the process, modify
    /// the geo positions and fill in the missing values.
    pub fn geo_layout(
        &mut self,
        geo: &mut HashMap<RouterId, Location>,
    ) -> Result<(), Gns3NetworkError> {
        let mut rng = thread_rng();

        // remove all geo locations with zero zero
        geo.retain(|_, p| !(p.latitude() == 0.0 && p.longitude() == 0.0));

        // compute statistics
        let lat_min =
            Iterator::min(geo.values().map(|x| x.latitude()).map(OrdF)).unwrap_or_default();
        let lat_max =
            Iterator::max(geo.values().map(|x| x.latitude()).map(OrdF)).unwrap_or_default();
        let lon_min =
            Iterator::min(geo.values().map(|x| x.longitude()).map(OrdF)).unwrap_or_default();
        let lon_max =
            Iterator::max(geo.values().map(|x| x.longitude()).map(OrdF)).unwrap_or_default();

        // exit if no information is given.
        if lat_min == lat_max || lon_min == lon_max {
            return Ok(());
        }

        // create scale and offset factors
        let lat_diff = lat_max.0 - lat_min.0;
        let lon_diff = lon_max.0 - lon_min.0;

        // extend the geo model with all routers by averaging them, and adding an offset away from
        // the center.
        for r in self.routers.keys() {
            if geo.contains_key(r) {
                continue;
            }
            let n = self.net.get_topology().neighbors(*r).filter_map(|x| geo.get(&x)).collect_vec();
            geo.insert(
                *r,
                if n.is_empty() {
                    Location::new(
                        rng.gen_range(lat_min.0..lat_max.0),
                        rng.gen_range(lon_min.0..lon_max.0),
                    )
                } else {
                    let new_lat = n.iter().map(|x| x.latitude()).sum::<f64>() / n.len() as f64;
                    let new_lon = n.iter().map(|x| x.longitude()).sum::<f64>() / n.len() as f64;
                    Location::new(
                        new_lat + rng.gen_range(-RAND_OFFSET..RAND_OFFSET) * lat_diff,
                        new_lon + rng.gen_range(-RAND_OFFSET..RAND_OFFSET) * lon_diff,
                    )
                },
            );
        }

        let layout = geo.iter().map(|(r, p)| (*r, (p.longitude(), p.latitude()))).collect();
        self.apply_layout(&layout)
    }

    /// Apply a precomputed layout, Any missing router will default to the center.
    pub fn apply_layout(
        &mut self,
        layout: &HashMap<RouterId, (f64, f64)>,
    ) -> Result<(), Gns3NetworkError> {
        let x_min = Iterator::min(layout.values().map(|(x, _)| *x).map(OrdF)).unwrap_or_default().0;
        let x_max = Iterator::max(layout.values().map(|(x, _)| *x).map(OrdF)).unwrap_or_default().0;
        let y_min = Iterator::min(layout.values().map(|(_, y)| *y).map(OrdF)).unwrap_or_default().0;
        let y_max = Iterator::max(layout.values().map(|(_, y)| *y).map(OrdF)).unwrap_or_default().0;

        let center = ((x_min + x_max) / 2f64, (y_min + y_max) / 2f64);

        // create scale and offset factors
        let (x_m, x_a) = (x_min, x_max - x_min);
        let (y_m, y_a) = (y_min, y_max - y_min);

        let translate = |p: &(f64, f64)| -> (isize, isize) {
            let x = ((p.0 - x_m) / x_a) * X_A + X_M;
            let y = ((p.1 - y_m) / y_a) * Y_A + Y_M;
            (x as isize, y as isize)
        };

        // set the position of each and every node
        for (r, (r_id, _)) in self.routers.iter() {
            let pos = layout.get(r).unwrap_or(&center);
            let (x, y) = translate(pos);
            self.project.set_node_pos(*r_id, x, y)?;
        }

        for (r, (c_id, _, _, _, _)) in self.clients.iter() {
            let pos = layout.get(r).unwrap_or(&center);
            let (x, y) = translate(pos);
            self.project.set_node_pos(*c_id, x + CLIENT_OFFSET_X, y + CLIENT_OFFSET_Y)?;
            self.project.hide_node_label(*c_id)?;
        }

        Ok(())
    }

    /// Automatically layout the network using a spring layout.
    pub fn spring_layout(&mut self) -> Result<(), Gns3NetworkError> {
        let layout = get_spring_layout(self.net);
        self.apply_layout(&layout)
    }
}

fn get_spring_layout<P: Prefix, Q, Ospf: OspfImpl>(
    net: &Network<P, Q, Ospf>,
) -> HashMap<RouterId, (f64, f64)> {
    let mut rng = thread_rng();
    let g = net.get_topology();
    let n = g.node_count();
    let n_f = n as f32;

    let mut positions: Vec<P2d> = g.node_indices().map(|_| P2d(rng.gen(), rng.gen())).collect();

    let neighbors: Vec<Vec<usize>> =
        g.node_indices().map(|x| g.neighbors(x).map(|x| x.index()).collect()).collect();

    const MAX_ITER: usize = 300;
    const EPS: f32 = 0.0001;
    const OPT_PAIR_SQR_DIST_SCALE: f32 = 0.3;

    let min_pos = P2d(0.0, 0.0);
    let max_pos = P2d(1.0, 1.0);
    let step_fn = |iter| 1.0 / (1.0 + (iter as f32) * 0.1);

    let k_r: f32 = OPT_PAIR_SQR_DIST_SCALE * 1.0 / n_f;
    let k_s = k_r.sqrt();

    fruchterman_reingold(
        step_fn,
        MAX_ITER,
        EPS,
        k_r,
        k_s,
        &min_pos,
        &max_pos,
        &mut positions,
        &neighbors,
    );

    positions
        .into_iter()
        .enumerate()
        .map(|(i, p)| ((i as u32).into(), (p.0 as f64, p.1 as f64)))
        .collect()
}
