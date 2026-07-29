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

use std::{net::Ipv4Addr, time::Duration};

use lazy_static::lazy_static;
use regex::Regex;
use thiserror::Error;

use super::{telnet_client::TelnetClient, PingError, TelnetError};

/// CLient to interact with the IPTerm client
#[derive(Debug)]
pub struct IpTermClient(TelnetClient);

impl IpTermClient {
    /// Create a new ipterm client
    pub fn new(target: impl Into<String>, port: u16) -> Result<Self, IpTermError> {
        Ok(Self(TelnetClient::new(target, port, "# ")?))
    }

    /// Set the IP address, the network mask and the default gateway of the client
    pub fn configure_ip(
        &mut self,
        ip: Ipv4Addr,
        mask: Ipv4Addr,
        gateway: Ipv4Addr,
    ) -> Result<(), IpTermError> {
        let timeout = Duration::from_secs(1);
        self.0.send_cmd(&format!("ifconfig eth0 {ip} netmask {mask} up"), timeout)?;
        self.0.send_cmd(&format!("route add default gw {gateway}"), timeout)?;
        Ok(())
    }

    /// Test if a destination is reachable using ping. This function simply returns a boolean wether
    /// the destination is reachable.
    pub fn ping(&mut self, destination: Ipv4Addr) -> Result<(), PingError> {
        let answer =
            self.0.send_cmd(&format!("ping -c 1 -w 1 {destination}"), Duration::from_secs(10))?;
        for line in answer.lines() {
            if line.starts_with("1 packets transmitted, 1 received") {
                return Ok(());
            }
        }
        Err(PingError::Fail(answer))
    }

    /// Start an iperf3 server on this device. Make sure that no server is already running.`
    pub fn iperf3_server(&mut self) -> Result<(), IpTermError> {
        let answer = self.0.send_cmd("iperf3 -s -D", Duration::from_secs(1))?;
        if answer.is_empty() {
            Ok(())
        } else {
            Err(IpTermError::CannotStartIperf3Server(answer))
        }
    }

    /// Get a buyilder to start an Iperf3 client.
    pub fn iperf3_client(&mut self, destination: Ipv4Addr) -> IpTermIperf3ClientBuilder<'_> {
        IpTermIperf3ClientBuilder::new(self, destination)
    }

    /// send pings using socat (without waiting for any answer), and send them with the given
    /// interval. The process can be stopped again using `Self::stop_process`.
    pub fn socat_ping(
        &mut self,
        destination: Ipv4Addr,
        interval: Duration,
    ) -> Result<IpTermProcess, IpTermError> {
        let ping_packet = "\
00000000: 0800 7291 0050 0001 7f03 4d63 0000 0000
00000010: efe3 0a00 0000 0000 1011 1213 1415 1617
00000020: 1819 1a1b 1c1d 1e1f 2021 2223 2425 2627
00000030: 2829 2a2b 2c2d 2e2f 3031 3233 3435 3637\
";
        // first, create the ping file
        self.0
            .send_cmd(&format!("echo \"{ping_packet}\" | xxd -r >ping"), Duration::from_secs(1))?;
        // then, start the command
        let answer = self.0.send_cmd(
            &format!(
                "while :; do cat ping; sleep {}; done | socat - IP:{}:1 > socat_log &",
                interval.as_secs_f32(),
                destination
            ),
            Duration::from_secs(1),
        )?;
        // split the result by spaces once and take the second half.
        answer
            .trim()
            .split_once(' ')
            .and_then(|(_, x)| x.parse::<u32>().ok())
            .map(IpTermProcess)
            .ok_or(IpTermError::CannotStartProcess(answer))
    }

    /// Stop a background process.
    pub fn stop_process(&mut self, process: IpTermProcess) -> Result<(), IpTermError> {
        self.0.send_cmd(&format!("kill {}", process.0), Duration::from_secs(1))?;
        Ok(())
    }
}

/// A process running in an IpTerm client (in the background).
#[derive(Debug)]
pub struct IpTermProcess(u32);

/// Builder to start an iperf client.
#[derive(Debug)]
pub struct IpTermIperf3ClientBuilder<'a> {
    client: &'a mut IpTermClient,
    destination: Ipv4Addr,
    /// bandwidth to use, in bits/second. Default is unlimited for TCP and 1Mbps for UDP.
    bandwidth: Option<u64>,
    /// Interval in seconds when to update the throughput. Defaults to 0.1s
    interval: f64,
}

impl<'a> IpTermIperf3ClientBuilder<'a> {
    fn new(client: &'a mut IpTermClient, destination: Ipv4Addr) -> Self {
        Self { client, destination, bandwidth: None, interval: 0.1 }
    }

    /// Set the bandwidth to a specific number of bits per second. For instance, use `1_000_000` to
    /// set the bandwidth to 1Mbps.
    pub fn bandwidth(mut self, bandwidth: u64) -> Self {
        self.bandwidth = Some(bandwidth);
        self
    }

    /// Set unlimited bandwidth (default)
    pub fn unlimited_bandwidth(mut self) -> Self {
        self.bandwidth = None;
        self
    }

    /// Set the interval of how often to update the throughput, defaults to 0.1
    pub fn interval(mut self, interval: f64) -> Self {
        self.interval = interval;
        self
    }

    /// Start the Iperf3 client.
    pub fn start(self) -> Result<IpTermIperf3Client<'a>, IpTermError> {
        let cmd = format!(
            "iperf3 -c {} -i {}{}",
            self.destination,
            self.interval,
            self.bandwidth.map(|x| format!(" -b {x}")).unwrap_or_default()
        );
        self.client.0.send_cmd_no_wait(&cmd)?;
        Ok(IpTermIperf3Client { client: self.client, data: Vec::new() })
    }
}

#[derive(Debug)]
pub struct IpTermIperf3Client<'a> {
    client: &'a mut IpTermClient,
    data: Vec<f64>,
}

impl<'a> IpTermIperf3Client<'a> {
    /// Stop the Iperf3 client and return all data samples.
    pub fn stop(mut self) -> Result<Vec<f64>, IpTermError> {
        self.update()?;
        self.client.0.send_ctrl_c()?;
        self.client.0.expect_prompt(Duration::from_secs(1))?;
        Ok(self.data)
    }

    /// Get the current throughput
    pub fn current_throughput(&mut self) -> Result<f64, IpTermError> {
        self.update()?;
        Ok(self.data.last().copied().unwrap_or(0.0))
    }

    /// Update the throughput data
    fn update(&mut self) -> Result<(), IpTermError> {
        let s = self.client.0.read_all()?;
        // expect a line like this:
        // [  4]   0.00-0.10   sec  59.1 MBytes  4.96 Gbits/sec   46   1.04 MBytes
        lazy_static! {
            static ref THROUGHPUT_RE: Regex = Regex::new(
                r"^\[[ 0-9]+\] +[0-9\.-]+ +[a-zA-Z]+ +([0-9\.]+) +(K|M|G|T)?Bytes +([0-9\.]+) +(K|M|G|T)?bits/sec +[0-9]+ +[0-9\.]+ +.?Bytes *$"
            ).unwrap();
        }
        for line in s.lines() {
            if let Some(cap) = THROUGHPUT_RE.captures(line) {
                // let transfer_num = cap.get(1).unwqrap();
                // let transfer_unit = cap.get(2).unwqrap();
                let speed_num = cap.get(3).unwrap().as_str();
                let speed_unit = cap.get(4).map_or("", |m| m.as_str());
                let speed: f64 = speed_num.parse::<f64>().unwrap_or(0.0)
                    * match speed_unit {
                        "" => 1.0,
                        "K" => 1024.0,
                        "M" => 1024.0 * 1024.0,
                        "G" => 1024.0 * 1024.0 * 1024.0,
                        "T" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
                        _ => unreachable!(),
                    };
                self.data.push(speed)
            }
        }
        Ok(())
    }
}

/// IpTerm communiation errors
#[derive(Debug, Error)]
pub enum IpTermError {
    /// Telnet errors
    #[error("Telnet Error: {0}")]
    Telnet(#[from] TelnetError),
    /// Could not start iperf3 `server.
    #[error("Could not start iperf3 server: {0}")]
    CannotStartIperf3Server(String),
    /// Could not start the process.
    #[error("Could not start the process: {0}")]
    CannotStartProcess(String),
}
