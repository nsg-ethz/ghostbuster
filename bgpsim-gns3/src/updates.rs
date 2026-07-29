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

//! Implementation to handle configuration updates and external routing updates.

use bgpsim::{
    config::{ConfigExprKey, ConfigModifier},
    export::{ExternalCfgGen, InternalCfgGen},
    prelude::*,
};

use crate::{Gns3Network, Gns3NetworkError};

impl<'n, P: Prefix, Q, Ospf: OspfImpl> Gns3Network<'n, P, Q, Ospf> {
    /// Apply a configuration modification to the network.
    pub fn apply_modifier(&mut self, expr: &ConfigModifier<P>) -> Result<(), Gns3NetworkError> {
        for r in expr.routers() {
            let err = || Gns3NetworkError::Network(NetworkError::DeviceNotFound(r));
            let (r_id, gen) = self.routers.get_mut(&r).ok_or_else(err)?;
            let commands = if self.net.get_device(r)?.is_internal() {
                InternalCfgGen::generate_command(gen, self.net, &mut self.addressor, expr.clone())?
            } else if let Some(ConfigExprKey::BgpSession { speaker_a: a, speaker_b: b }) =
                expr.key()
            {
                let neighbor = if a == r { b } else { a };
                if let ConfigModifier::Insert(_) = expr {
                    ExternalCfgGen::establish_ebgp_session(
                        gen,
                        self.net,
                        &mut self.addressor,
                        neighbor,
                    )?
                } else if let ConfigModifier::Remove(_) = expr {
                    ExternalCfgGen::teardown_ebgp_session(
                        gen,
                        self.net,
                        &mut self.addressor,
                        neighbor,
                    )?
                } else {
                    unreachable!("Cannot modify the kind of session with an external router!")
                }
            } else {
                // skip this router, as the configuration does not affect the router!
                continue;
            };
            self.project.get_node(*r_id).get_frr_client(&self.server_url)?.configure(commands)?;
        }
        Ok(())
    }

    /// Advertise a new external route. The `router` must be an external router. If no AS path is provided,
    /// we will assume the external network originates the prefix.
    pub fn advertise_external_route<C>(
        &mut self,
        router: RouterId,
        prefix: P,
        as_path: Option<Vec<AsId>>,
        med: Option<u32>,
        community: C,
    ) -> Result<(), Gns3NetworkError>
    where
        C: IntoIterator<Item = u32>,
    {
        let external_as_id = self.net.get_device(router)?.external_or_err()?.as_id();

        let route =
            BgpRoute::new(router, prefix, as_path.unwrap_or(vec![external_as_id]), med, community);
        let (r_id, gen) =
            self.routers.get_mut(&router).ok_or(NetworkError::DeviceNotFound(router))?;
        let commands = gen.advertise_route(self.net, &mut self.addressor, &route)?;
        self.project.get_node(*r_id).get_frr_client(&self.server_url)?.configure(commands)?;
        Ok(())
    }

    /// Withdraw an external route. The `router` must be an external router.
    pub fn withdraw_external_route(
        &mut self,
        router: RouterId,
        prefix: P,
    ) -> Result<(), Gns3NetworkError> {
        if !self.net.get_device(router)?.is_external() {
            return Err(Gns3NetworkError::Network(NetworkError::DeviceIsInternalRouter(router)));
        }
        let (r_id, gen) =
            self.routers.get_mut(&router).ok_or(NetworkError::DeviceNotFound(router))?;
        let commands = gen.withdraw_route(self.net, &mut self.addressor, prefix)?;
        self.project.get_node(*r_id).get_frr_client(&self.server_url)?.configure(commands)?;
        Ok(())
    }
}
