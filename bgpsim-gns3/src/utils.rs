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

//! This module contains utilities for the `Gns3Network`.

use std::{collections::HashMap, path::PathBuf, thread::JoinHandle, time::Duration};

use bgpsim::{
    export::{CiscoFrrCfgGen, DefaultAddressor, LinkId},
    prelude::*,
};
use log::debug;

use crate::{
    gns3::{
        links::Gns3LinkIndex,
        nodes::{frr::FrrClient, ipterm::IpTermClient, TelnetHandle},
        Gns3Error,
    },
    Gns3Network, Gns3NetworkError,
};

impl<'n, P: Prefix, Q, Ospf: OspfImpl> Gns3Network<'n, P, Q, Ospf> {
    /// Execute a command on all routers in parallel.
    pub(crate) fn send_cmd_all(
        &mut self,
        cmd: impl Into<String>,
        timeout: Duration,
    ) -> Result<(), Gns3NetworkError> {
        let cmd = cmd.into();
        debug!("Send command to all routers: {}", cmd);

        // create all threads
        let threads = self
            .net
            .internal_indices()
            .map(|r| {
                let handle = self.get_frr_handle(r).unwrap();
                let cmd = cmd.clone();
                Ok(std::thread::spawn(move || apply_cmd(handle, cmd, timeout)))
            })
            .collect::<Result<Vec<JoinHandle<_>>, Gns3NetworkError>>()?;

        // join all threasd
        for thread in threads {
            thread.join().map_err(Gns3NetworkError::ThreadError)??;
        }

        Ok(())
    }

    /// If you call this function with the argument `true`, then the project will be kept open, and
    /// not deleted when the project is dropped.
    pub fn keep_open(&mut self, val: bool) {
        self.project.keep_open(val);
    }

    /// Get the FRR telnet handle for a given router. This handle can be used to spawn an FRR
    /// client. `gnet.get_frr_handle(x).open_frr()` is equivalent to `gnet.get_frr(x)`.
    pub fn get_frr_handle(&self, router: RouterId) -> Result<TelnetHandle, Gns3NetworkError> {
        let r_id = self.routers.get(&router).ok_or(NetworkError::DeviceNotFound(router))?.0;
        Ok(self.project.telnet(r_id))
    }

    /// Get the FRR telnet connection for a given router.
    pub fn get_frr(&self, router: RouterId) -> Result<FrrClient, Gns3NetworkError> {
        let r_id = self.routers.get(&router).ok_or(NetworkError::DeviceNotFound(router))?.0;
        Ok(self.project.get_node(r_id).get_frr_client(&self.server_url)?)
    }

    /// Get the IpTerm handle for the client of a given router. This handle can be used to spawn an
    /// IpTerm client. `gnet.get_client_handle(x).open_ipterm()` is equivalent to
    /// `gnet.get_client(x)`.
    pub fn get_client_handle(&self, router: RouterId) -> Result<TelnetHandle, Gns3NetworkError> {
        let c_id = self.clients.get(&router).ok_or(NetworkError::DeviceNotFound(router))?.0;
        Ok(self.project.telnet(c_id))
    }

    /// Get the IpTerm connection for the client of a given router.
    pub fn get_client(&self, router: RouterId) -> Result<IpTermClient, Gns3NetworkError> {
        let c_id = self.clients.get(&router).ok_or(NetworkError::DeviceNotFound(router))?.0;
        Ok(self.project.get_node(c_id).get_ipterm_client(&self.server_url)?)
    }

    /// Get the config generator for the given router. Also return the addressor, which will most
    /// likely be needed when using the config generator.
    pub fn get_generator(
        &mut self,
        router: RouterId,
    ) -> Result<(&mut CiscoFrrCfgGen<P>, &mut DefaultAddressor<'n, P, Q, Ospf>), Gns3NetworkError>
    {
        Ok(self
            .routers
            .get_mut(&router)
            .map(|(_, x)| (x, &mut self.addressor))
            .ok_or(NetworkError::DeviceNotFound(router))?)
    }

    /// Get a reference to the addressor.
    pub fn get_addressor(&self) -> &DefaultAddressor<'n, P, Q, Ospf> {
        &self.addressor
    }

    /// Get a reference to the inner network. This is the network that was used to create the
    /// original network. Any later modifications **are not reflected** in this network.
    pub fn get_net(&self) -> &'n Network<P, Q, Ospf> {
        self.net
    }

    /// Get a copy of the internal mapping of BgpSim and Gns3 links
    pub fn get_links(&self) -> HashMap<LinkId, Gns3LinkIndex> {
        self.links.clone()
    }

    /// Start a capture on a link connecting two routers (or multiple if the two routers are
    /// connected on multiple links).
    pub fn start_captures(
        &mut self,
        a: RouterId,
        b: RouterId,
    ) -> Result<Vec<PathBuf>, Gns3NetworkError> {
        let a_id = self.routers.get(&a).ok_or(NetworkError::DeviceNotFound(a))?.0;
        let b_id = self.routers.get(&b).ok_or(NetworkError::DeviceNotFound(b))?.0;
        let links_connecting = self.project.get_links_connecting(a_id, b_id);
        if links_connecting.is_empty() {
            Err(Gns3NetworkError::Gns3(Gns3Error::NoLinkFound(a.fmt(&self.net), b.fmt(&self.net))))
        } else {
            Ok(links_connecting
                .into_iter()
                .map(|x| self.project.start_capture(x))
                .collect::<Result<Vec<PathBuf>, Gns3Error>>()?)
        }
    }

    /// Stop a capture on a link connecting two routers (or multiple if the two routers are
    /// connected on multiple links).
    pub fn stop_captures(
        &mut self,
        a: RouterId,
        b: RouterId,
    ) -> Result<Vec<Option<PathBuf>>, Gns3NetworkError> {
        let a_id = self.routers.get(&a).ok_or(NetworkError::DeviceNotFound(a))?.0;
        let b_id = self.routers.get(&b).ok_or(NetworkError::DeviceNotFound(b))?.0;
        let links_connecting = self.project.get_links_connecting(a_id, b_id);
        if links_connecting.is_empty() {
            Err(Gns3NetworkError::Gns3(Gns3Error::NoLinkFound(a.fmt(&self.net), b.fmt(&self.net))))
        } else {
            Ok(links_connecting
                .into_iter()
                .map(|x| self.project.stop_capture(x))
                .collect::<Result<Vec<Option<PathBuf>>, Gns3Error>>()?)
        }
    }

    /// Take a snapshot of the current network
    pub fn take_snapshot(&mut self) -> Result<(), Gns3NetworkError> {
        self.project.stop_all_nodes()?;
        self.project.take_snapshot()?;
        self.project.start_all_nodes()?;
        Ok(())
    }

    /// Restore the network to an existing snapshot
    pub fn restore_snapshot(&mut self) -> Result<(), Gns3NetworkError> {
        self.project.stop_all_nodes()?;
        self.project.restore_snapshot()?;
        self.project.start_all_nodes()?;
        Ok(())
    }
}

/// Process that waits until the the last_update happened more than 30 seconds ago. This function
/// can be spawned as a separate thread. If convergence was reached before `max_wait_time`, then
/// return normally. Otherwise, return `Err(Gns3NetworkError::NoCovergence)`.
fn apply_cmd(handle: TelnetHandle, cmd: String, timeout: Duration) -> Result<(), Gns3NetworkError> {
    handle.open_frr()?.send_cmd(&cmd, timeout)?;
    Ok(())
}
