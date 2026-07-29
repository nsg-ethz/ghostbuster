use std::str::FromStr;

use bgpsim::{
    event::EventQueue,
    prelude::NetworkFormatter,
    types::{Prefix, RouterId},
};
use bgpsim_gns3::logger::LogMessage;
use serde::{Deserialize, Serialize};
use tracing::trace;

use super::P;

#[derive(Debug, Serialize, Deserialize)]
pub struct BugReport {
    pub timestamp: f64,
    pub router: RouterId,
    pub prefix: P,
}

impl BugReport {
    /// Try to parse a log message into a bug report
    pub fn maybe_from(msg: &LogMessage, router: RouterId) -> Option<Self> {
        // Extract the relevant prefix
        let prefix_str = msg.content.split(" ").last()?;
        trace!("Extracted prefix string '{}'", prefix_str);
        let prefix = P::from_str(prefix_str).ok()?;
        trace!("Successful conversion into '{}'", prefix);

        Some(Self {
            timestamp: msg.timestamp,
            router,
            prefix,
        })
    }
}

impl<'a, PR, Q, Ospf> NetworkFormatter<'a, PR, Q, Ospf> for BugReport
where
    PR: Prefix,
    Q: EventQueue<PR>,
    Ospf: bgpsim::ospf::OspfImpl,
{
    fn fmt(&self, net: &'a bgpsim::network::Network<PR, Q, Ospf>) -> String {
        format!(
            "{:<w$} : Reported bug on {} for prefix {}",
            self.timestamp,
            self.router.fmt(&net),
            self.prefix,
            w = 24,
        )
    }
}
