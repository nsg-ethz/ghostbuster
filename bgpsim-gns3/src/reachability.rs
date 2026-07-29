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

//! This module contains the implementation for checking reachability. It also provides functions to
//! wait until the network has reachability.

use std::{
    thread::sleep,
    time::{Duration, Instant},
};

use crate::{gns3::nodes::PingResult, Gns3Network, Gns3NetworkError};

use bgpsim::{ospf::OspfImpl, prelude::NetworkFormatter, types::Prefix};
use itertools::Itertools;
use log::{debug, info};

impl<'n, P: Prefix, Q, Ospf: OspfImpl> Gns3Network<'n, P, Q, Ospf> {
    /// Check for reachability. This checks that every pair of routers, every pair of clients, and
    /// every direct link can ping eachother. If `wait_duration` is set, then this function will
    /// wait at most `wait_duration` if a ping is unreachable.
    pub fn check_reachability(
        &mut self,
        wait_duration: Option<Duration>,
    ) -> Result<bool, Gns3NetworkError> {
        let wait_duration = wait_duration.unwrap_or_default();
        let now = Instant::now();
        let sleep_time = Duration::from_secs(1);

        for (r, (c_id, _, _, _, _)) in self.clients.iter().sorted_by_key(|(x, _)| *x) {
            let mut client = self.project.get_node(*c_id).get_ipterm_client(&self.server_url)?;
            for (r_other, (_, _, ip, _, _)) in self.clients.iter().sorted_by_key(|(x, _)| *x) {
                debug!("{}-client tries to ping {}-client", r.fmt(self.net), r_other.fmt(self.net));
                while !client.ping(*ip).success()? {
                    if now.elapsed() > wait_duration {
                        info!(
                            "{}-client cannot ping {}-client",
                            r.fmt(self.net),
                            r_other.fmt(self.net)
                        );
                        return Ok(false);
                    }
                    sleep(sleep_time);
                }
                debug!("{}-client can ping {}-client", r.fmt(self.net), r_other.fmt(self.net));
            }
        }

        Ok(true)
    }
}
