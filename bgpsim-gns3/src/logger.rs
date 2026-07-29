use bgpsim::{
    ospf::OspfImpl,
    types::{Prefix, RouterId},
};
use itertools::Itertools;
use regex::Regex;
use std::time::Duration;

use chrono::NaiveDateTime;

#[derive(Debug)]
pub struct Log {
    pub messages: Vec<LogMessage>,
}

#[derive(Debug)]
pub struct LogMessage {
    pub timestamp: f64,
    pub daemon: String,
    pub content: String,
}

impl Log {
    /// Filter the log messages according to wether or not they contain a substring
    pub fn filter_substring(self, substring: &str) -> Self {
        let messages =
            self.messages.into_iter().filter(|m| m.content.contains(substring)).collect();
        Self { messages }
    }

    /// Filter the log messages according to wether or not they were produced during a specified interval of time
    pub fn filter_interval(self, start: f64, finish: f64) -> Self {
        let messages = self
            .messages
            .into_iter()
            .filter(|m| start <= m.timestamp && m.timestamp <= finish)
            .collect();
        Self { messages }
    }
}

impl TryFrom<String> for Log {
    type Error = Gns3NetworkError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        let messages = raw.lines().map(|l| l.try_into()).try_collect()?;
        Ok(Self { messages })
    }
}

impl TryFrom<&str> for LogMessage {
    type Error = Gns3NetworkError;

    fn try_from(line: &str) -> Result<Self, Self::Error> {
        // Regex captures: timestamp, daemon, content
        let re = Regex::new(r"^(\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2}\.\d+) (\w+): (.+)$")
            .map_err(|_| FrrError::LogError)?;

        let caps = re.captures(line).ok_or(Gns3NetworkError::FrrClient(FrrError::LogError))?;
        let datetime_str = &caps[1];
        let daemon = &caps[2];
        let content = &caps[3];

        // Parse timestamp
        let dt = NaiveDateTime::parse_from_str(datetime_str, "%Y/%m/%d %H:%M:%S%.6f")
            .map_err(|_| FrrError::LogError)?;
        let timestamp = dt.and_utc().timestamp() as f64
            + dt.and_utc().timestamp_subsec_micros() as f64 / 1_000_000.0;

        Ok(LogMessage { timestamp, daemon: daemon.to_string(), content: content.to_string() })
    }
}

use crate::{gns3::nodes::frr::FrrError, Gns3Network, Gns3NetworkError};

const LOG_PATH: &'static str = "/etc/frr/bgpd";

fn log_file(router: RouterId) -> String {
    format!("{LOG_PATH}-{}.log", router.index())
}

impl<'n, P: Prefix, Q, Ospf: OspfImpl> Gns3Network<'n, P, Q, Ospf> {
    /// Enable logging for bgpd on a router on the network
    pub fn enable_log(&self, router: RouterId) -> Result<(), Gns3NetworkError> {
        let mut client = self.get_frr(router)?;
        client.configure(format!(
            "log daemon bgpd file {}\nlog timestamp precision 6",
            log_file(router)
        ))?;
        Ok(())
    }

    /// Get logs from a router on the network
    pub fn get_log(&self, router: RouterId) -> Result<Log, Gns3NetworkError> {
        let mut client = self.get_frr(router)?;
        // First, go to the shell
        client.send_cmd("exit\n", Duration::from_secs(1))?;
        // Then send the cat command
        let answer =
            client.send_cmd(&format!("cat {}", log_file(router)), Duration::from_secs(10))?;

        // Lastly, go back to the vtysh
        client.send_cmd("exit\n", Duration::from_secs(1))?;

        // Try converting the log
        answer.try_into()
    }
}
