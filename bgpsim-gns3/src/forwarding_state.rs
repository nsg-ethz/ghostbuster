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

//! Module to extract the current forwarding state from the network.

use bgpsim::{
    export::{Addressor, ExportError},
    prelude::*,
};
use log::{debug, info};

use crate::{Gns3Network, Gns3NetworkError};

impl<'n, P: Prefix, Q, Ospf: OspfImpl> Gns3Network<'n, P, Q, Ospf> {
    /// Get the next-hop(s) for a single router and a prefix. The resulting list of next-hops will
    /// be sorted.
    pub fn get_next_hops(
        &mut self,
        router: RouterId,
        prefix: P,
    ) -> Result<Vec<RouterId>, Gns3NetworkError> {
        let (r_id, gen) = self.routers.get(&router).ok_or(NetworkError::DeviceNotFound(router))?;
        let mut client = self.project.get_node(*r_id).get_frr_client(&self.server_url)?;
        let prefix = self.addressor.prefix(prefix)?.unwrap_single();
        let nh_ifaces =
            client.get_route_for_prefix(prefix)?.map(|x| x.interfaces()).unwrap_or_default();
        Ok(nh_ifaces
            .into_iter()
            .map(|x| gen.iface_idx(x).and_then(|idx| self.addressor.find_neighbor(router, idx)))
            .collect::<Result<Vec<RouterId>, ExportError>>()?)
    }

    /// Check if the forwarding state matches the provided network. This function takes a rerference
    /// to a different network, against which it will be compared.
    pub fn equal_forwarding_state(
        &mut self,
        net: &Network<P, Q>,
    ) -> Result<bool, Gns3NetworkError> {
        let fw_state = net.get_forwarding_state();
        for router in net.internal_indices() {
            for prefix in net.get_known_prefixes().chain(self.net.get_known_prefixes()).copied() {
                let exp = fw_state.get_next_hops(router, prefix);
                let acq = self.get_next_hops(router, prefix)?;
                if exp == acq {
                    debug!("Correct next-hop for {} and {}!", router.fmt(net), prefix);
                } else {
                    info!(
                        "Forwarding state of {} and {} is not as expected!\nExpected: {}\nAcquired: {:#?}",
                        router.fmt(net),
                        prefix,
                        exp.fmt(net),
                        acq.fmt(net)
                    )
                }
            }
        }

        Ok(true)
    }
}
