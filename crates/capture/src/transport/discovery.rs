//! LAN discovery over mDNS: receivers advertise `_relay._udp.local.` with
//! their signalling TCP port; senders browse and pick a peer by name. Zero
//! configuration — no addresses typed anywhere.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

pub const SERVICE_TYPE: &str = "_relay._udp.local.";

/// Keeps the advertisement alive; dropping unregisters.
pub struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertisement {
    /// Advertise this host as a Relay receiver on `port` (signalling TCP).
    pub fn start(instance: &str, port: u16) -> Result<Self> {
        let daemon = ServiceDaemon::new().context("mDNS daemon")?;
        let host = hostname();
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            instance,
            &format!("{host}.local."),
            (), // let mdns-sd resolve our addresses
            port,
            None,
        )?
        .enable_addr_auto();
        let fullname = info.get_fullname().to_string();
        daemon.register(info).context("mDNS register")?;
        Ok(Self { daemon, fullname })
    }
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Discovered {
    pub name: String,
    pub addr: std::net::IpAddr,
    pub port: u16,
}

/// Browse for receivers for `timeout`. Returns unique instances.
pub fn browse(timeout: Duration) -> Result<Vec<Discovered>> {
    let daemon = ServiceDaemon::new().context("mDNS daemon")?;
    let rx = daemon.browse(SERVICE_TYPE).context("mDNS browse")?;
    let deadline = std::time::Instant::now() + timeout;
    let mut found: HashMap<String, Discovered> = HashMap::new();
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let name = info
                    .get_fullname()
                    .strip_suffix(&format!(".{SERVICE_TYPE}"))
                    .unwrap_or(info.get_fullname())
                    .to_string();
                // IPv4 only for signalling; every LAN we target has it.
                if let Some(addr) = info.get_addresses_v4().into_iter().next() {
                    found.insert(
                        name.clone(),
                        Discovered { name, addr: addr.into(), port: info.get_port() },
                    );
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = daemon.shutdown();
    Ok(found.into_values().collect())
}

pub fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "relay".into()).to_lowercase()
}
