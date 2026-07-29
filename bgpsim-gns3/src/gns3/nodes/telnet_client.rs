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

//! Module to create the telnet client

use std::{net::ToSocketAddrs, time::Duration};

use itertools::Itertools;
use lazy_static::lazy_static;
use log::{debug, error};
use regex::Regex;
use telnet::{Event, Telnet};
use thiserror::Error;

const ESCAPE_RE: &str = r"\u{1B}\[.n";

/// Telnet client to communicate with a remote device. This struct adds convenience methods around
/// the `[telnet::Telnet]` api.
pub struct TelnetClient {
    s: Telnet,
    prompt: &'static str,
}

impl std::fmt::Debug for TelnetClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelnetClient").field("prompt", &self.prompt).finish()
    }
}

impl TelnetClient {
    /// Create a new telnet client. When setting this client up, we will wait until we got the
    /// expected propmpt.
    pub fn new(
        target: impl Into<String>,
        port: u16,
        prompt: &'static str,
    ) -> Result<Self, TelnetError> {
        let addr = (target.into(), port).to_socket_addrs().unwrap().next().unwrap();

        let mut s =
            Self { s: Telnet::connect_timeout(&addr, 4096, Duration::from_secs(10))?, prompt };

        s.s.write("\n".as_bytes())?;

        // wait for a prompt
        s.expect_prompt(Duration::from_secs(10))?;

        // now, wait until we have the last prompt received. To do that, use a low timeout of one millisecond.
        while s.expect_prompt(Duration::from_millis(1)).is_ok() {}

        Ok(s)
    }

    /// Wait until we receive a prompt, and return everything up to the last line that includes the
    /// prompt.
    pub(crate) fn expect_prompt(&mut self, timeout: Duration) -> Result<String, TelnetError> {
        let mut data_before_prompt = String::new();
        while !data_before_prompt.ends_with(self.prompt) {
            match self.s.read_timeout(timeout)? {
                Event::Data(d) => {
                    let d = String::from_utf8_lossy(&d);
                    lazy_static! {
                        static ref ESCAPE: Regex = Regex::new(ESCAPE_RE).unwrap();
                    }
                    data_before_prompt.push_str(&ESCAPE.replace_all(&d, ""));
                }
                Event::UnknownIAC(_) | Event::Negotiation(_, _) | Event::Subnegotiation(_, _) => {}
                Event::TimedOut | Event::NoData => {
                    return Err(TelnetError::NoPrompt(data_before_prompt))
                }
                Event::Error(e) => return Err(TelnetError::Telnet(e)),
            }
        }
        // there might be a second prompt in the input. If so, remove all up to the last prompt.
        let before_prompt =
            data_before_prompt.rsplit_once(self.prompt).expect("while exit condition above").0;
        let data_between_prompt =
            before_prompt.rsplit_once(self.prompt).map(|(_, x)| x).unwrap_or(before_prompt);
        let lines = data_between_prompt.lines().collect_vec();

        Ok(lines[1..lines.len() - 1].join("\n"))
    }

    /// Send a command and get the output until the next prompt line.
    pub fn send_cmd(&mut self, cmd: &str, timeout: Duration) -> Result<String, TelnetError> {
        self.s.write(cmd.as_bytes())?;
        self.s.write("\n".as_bytes())?;
        match self.expect_prompt(timeout) {
            Ok(a) => {
                debug!("{}; {}", cmd, a);
                Ok(a)
            }
            Err(e) => {
                error!("Error executing command: {}", cmd);
                Err(e)
            }
        }
    }

    /// Send a command without waiting for any result.
    pub fn send_cmd_no_wait(&mut self, cmd: &str) -> Result<(), TelnetError> {
        self.s.write(cmd.as_bytes())?;
        self.s.write("\n".as_bytes())?;
        Ok(())
    }

    /// Send a command without waiting.
    pub fn read_all(&mut self) -> Result<String, TelnetError> {
        let mut data = String::new();

        loop {
            match self.s.read_nonblocking()? {
                Event::Data(d) => data.push_str(&String::from_utf8_lossy(&d)),
                Event::UnknownIAC(_) | Event::Negotiation(_, _) | Event::Subnegotiation(_, _) => {}
                Event::TimedOut | Event::NoData => return Ok(data.lines().join("\n")),
                Event::Error(e) => return Err(TelnetError::Telnet(e)),
            }
        }
    }

    /// Send the Ctrl-c command
    pub fn send_ctrl_c(&mut self) -> Result<(), TelnetError> {
        self.s.write(&[0x03])?;
        Ok(())
    }
}

/// Errors from the Telnet Communication
#[derive(Debug, Error)]
pub enum TelnetError {
    /// IO error while communicating
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// Telnet protocol error
    #[error("Telnet Error: {0}")]
    Telnet(#[from] telnet::TelnetError),
    /// Cannot get the initial prompt
    #[error("Cannot get a prompt from the client. Data received: {0}")]
    NoPrompt(String),
}

#[cfg(test)]
mod test {
    use regex::Regex;

    use super::ESCAPE_RE;

    #[test]
    fn escape_re() {
        let re: Regex = Regex::new(ESCAPE_RE).unwrap();
        assert_eq!(
            re.replace_all("exit\r\n\r\n/ # \r\n/ # \u{1b}[6n", ""),
            "exit\r\n\r\n/ # \r\n/ # "
        );
    }
}
