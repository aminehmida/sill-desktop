//! Which addresses are this computer's own: a Mac that runs Sill and this app would otherwise list
//! itself, and a session with oneself is a picture of a picture, with the pointer jumping between.

use std::collections::HashSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::time::{Duration, Instant};

pub struct ThisComputer {
    addresses: HashSet<IpAddr>,
    names: HashSet<String>,
    read: Instant,
    /// The bench's host is on loopback: there, loopback is another Mac.
    pub loopback_is_other: bool,
}

/// "host", "host:port", "[v6]:port", "fe80::1%en0" → the host alone, lowercased, no trailing dot.
fn bare(host: &str) -> String {
    let h = host.trim();
    let h = match h.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest),
        None if h.matches(':').count() == 1 => h.split(':').next().unwrap_or(h),
        None => h,
    };
    h.split('%').next().unwrap_or(h).trim_end_matches('.').to_lowercase()
}

impl ThisComputer {
    pub fn new() -> Self {
        let mut me = Self { addresses: HashSet::new(), names: HashSet::new(), read: Instant::now(), loopback_is_other: false };
        me.read();
        me
    }

    fn read(&mut self) {
        self.read = Instant::now();
        self.addresses = if_addrs::get_if_addrs().map(|list| list.into_iter().map(|i| i.ip()).filter(|ip| !ip.is_loopback()).collect()).unwrap_or_default();
        let name = gethostname::gethostname().to_string_lossy().trim_end_matches('.').to_lowercase();
        let short = name.trim_end_matches(".local").to_string();
        self.names = [name, format!("{short}.local"), short].into_iter().filter(|n| !n.is_empty()).collect();
    }

    /// Interfaces come and go (Wi-Fi, a VPN): read again now and then.
    pub fn refresh(&mut self) {
        if self.read.elapsed() > Duration::from_secs(10) {
            self.read();
        }
    }

    /// Whether `host` is this computer, by what is known without asking the network.
    pub fn is(&self, host: &str) -> bool {
        let h = bare(host);
        match h.parse::<IpAddr>() {
            Ok(ip) => (ip.is_loopback() && !self.loopback_is_other) || self.addresses.contains(&ip),
            Err(_) => (h == "localhost" && !self.loopback_is_other) || self.names.contains(&h),
        }
    }

    /// The same for an address a person typed, looking a name up (a VPN's name for this computer).
    pub fn is_typed(&self, address: &str) -> bool {
        if self.is(address) {
            return true;
        }
        let h = bare(address);
        if h.is_empty() || h.parse::<IpAddr>().is_ok() {
            return false;
        }
        (h.as_str(), 0).to_socket_addrs().map(|mut found| found.any(|a| (a.ip().is_loopback() && !self.loopback_is_other) || self.addresses.contains(&a.ip()))).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(addresses: &[&str], names: &[&str]) -> ThisComputer {
        ThisComputer {
            addresses: addresses.iter().map(|a| a.parse().unwrap()).collect(),
            names: names.iter().map(|n| n.to_string()).collect(),
            read: Instant::now(),
            loopback_is_other: false,
        }
    }

    #[test]
    fn its_own_addresses_in_every_spelling() {
        let me = with(&["192.168.1.20", "100.101.102.103", "fe80::1"], &["sams-macbook-pro.local", "sams-macbook-pro"]);
        assert!(me.is("192.168.1.20"));
        assert!(me.is("100.101.102.103:47455"));
        assert!(me.is("[fe80::1]:47455"));
        assert!(me.is("fe80::1%en0"));
        assert!(me.is("Sams-MacBook-Pro.local."));
        assert!(me.is("127.0.0.1"));
        assert!(me.is("[::1]:47455"));
        assert!(me.is("localhost"));
        assert!(!me.is("192.168.1.21"));
        assert!(!me.is("mac-mini.local"));
        assert!(!me.is(""));
    }
}
