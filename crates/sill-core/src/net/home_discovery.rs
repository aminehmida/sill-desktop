//! The network browser at home (`StreamClient`'s Bonjour half, on the `mdns-sd` crate): every Mac
//! running Sill on this network, as `_sill._tcp` lists it.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use super::saved_macs::Endpoint;
use super::session::monotonic;

/// The home door's Bonjour service type.
pub const SERVICE_TYPE: &str = "_sill._tcp";

/// `HomeDoorTXT`: how the door speaks, by its TXT record's `p`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HomeDoor {
    /// No `p`: plain TCP and no pairing (a Sill from before pairing at home). Never dialed.
    Plain,
    /// `p=1`, or any value but "0": TLS, paired devices only.
    PairingRequired,
    /// `p=0`: TLS, any device.
    Open,
}

impl HomeDoor {
    /// The door a TXT record describes: `p` is its value, `None` when the record has no `p`.
    pub fn of(p: Option<&str>) -> HomeDoor {
        match p {
            None => HomeDoor::Plain,
            Some("0") => HomeDoor::Open,
            Some(_) => HomeDoor::PairingRequired,
        }
    }
}

/// A Mac's home door as Bonjour lists it on this network: its name, where it answers, and its TXT
/// record's `p` (how the door speaks) and `r` (the recognition tag that names a saved Mac).
#[derive(Debug, Clone, PartialEq)]
pub struct HomeMac {
    pub service_name: String,
    pub host: String,
    pub port: u16,
    pub door: HomeDoor,
    /// The TXT record's `r`.
    pub tag: Option<String>,
    /// When this listing of the name began (seconds, monotonic), the same for as long as the name
    /// stays listed; a new value is the Mac listed afresh, which the automatic reconnect dials at
    /// once.
    pub since: f64,
}

impl HomeMac {
    pub fn endpoint(&self) -> Endpoint {
        Endpoint::new(self.host.clone(), self.port)
    }
}

type OnChange = Arc<dyn Fn(Vec<HomeMac>) + Send + Sync>;

struct Running {
    daemon: ServiceDaemon,
    generation: u64,
}

/// Browses for home doors while started. `on_change` gets the whole list at every change, on the
/// browser's own thread.
pub struct HomeDiscovery {
    service_type: String,
    endpoint_override: Option<Endpoint>,
    on_change: OnChange,
    running: Mutex<Option<Running>>,
    generation: Arc<Mutex<u64>>,
}

impl HomeDiscovery {
    /// `service_type`: [`SERVICE_TYPE`], or a test type. `endpoint_override`: tests only, what the
    /// browser lists is reached there instead.
    pub fn new(service_type: &str, endpoint_override: Option<Endpoint>, on_change: impl Fn(Vec<HomeMac>) + Send + Sync + 'static) -> HomeDiscovery {
        HomeDiscovery {
            service_type: service_type.trim_end_matches('.').trim_end_matches(".local").to_string(),
            endpoint_override,
            on_change: Arc::new(on_change),
            running: Mutex::new(None),
            generation: Arc::new(Mutex::new(0)),
        }
    }

    fn full_type(&self) -> String {
        format!("{}.local.", self.service_type)
    }

    /// Starts browsing; nothing when already started. An error is the mDNS daemon's (no usable
    /// interface, say).
    pub fn start(&self) -> Result<(), String> {
        let mut running = self.running.lock().unwrap();
        if running.is_some() {
            return Ok(());
        }
        let daemon = ServiceDaemon::new().map_err(|e| e.to_string())?;
        let ty = self.full_type();
        let receiver = daemon.browse(&ty).map_err(|e| e.to_string())?;
        let generation = {
            let mut g = self.generation.lock().unwrap();
            *g += 1;
            *g
        };
        let (current, on_change, over) = (self.generation.clone(), self.on_change.clone(), self.endpoint_override.clone());
        std::thread::Builder::new()
            .name("sill-mdns".into())
            .spawn(move || {
                let mut found: BTreeMap<String, HomeMac> = BTreeMap::new();
                while let Ok(event) = receiver.recv() {
                    if *current.lock().unwrap() != generation {
                        break;
                    }
                    let changed = match event {
                        ServiceEvent::ServiceResolved(info) => match home_mac(&info, &ty, over.as_ref(), found.get(info.get_fullname())) {
                            Some(mac) => found.insert(info.get_fullname().to_string(), mac.clone()) != Some(mac),
                            None => false,
                        },
                        ServiceEvent::ServiceRemoved(_, fullname) => found.remove(&fullname).is_some(),
                        ServiceEvent::SearchStopped(_) => break,
                        _ => false,
                    };
                    if changed {
                        on_change(found.values().cloned().collect());
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        *running = Some(Running { daemon, generation });
        Ok(())
    }

    /// Stops browsing and empties the list.
    pub fn stop(&self) {
        let Some(r) = self.running.lock().unwrap().take() else { return };
        {
            let mut g = self.generation.lock().unwrap();
            if *g == r.generation {
                *g += 1;
            }
        }
        let _ = r.daemon.stop_browse(&self.full_type());
        let _ = r.daemon.shutdown();
        (self.on_change)(Vec::new());
    }

    pub fn is_running(&self) -> bool {
        self.running.lock().unwrap().is_some()
    }
}

impl Drop for HomeDiscovery {
    fn drop(&mut self) {
        if let Some(r) = self.running.lock().unwrap().take() {
            *self.generation.lock().unwrap() += 1;
            let _ = r.daemon.shutdown();
        }
    }
}

/// The instance name of "Name._sill._tcp.local.".
fn instance_name(fullname: &str, ty: &str) -> String {
    fullname.strip_suffix(ty).map(|n| n.trim_end_matches('.')).unwrap_or(fullname).to_string()
}

fn home_mac(info: &ServiceInfo, ty: &str, over: Option<&Endpoint>, was: Option<&HomeMac>) -> Option<HomeMac> {
    // IPv4 first; a link-local IPv6 address without its scope names nothing.
    let addresses = info.get_addresses();
    // A LAN address before a VPN's (a Mac with Tailscale announces both): home is the LAN.
    let address = addresses
        .iter()
        .find(|a| a.is_ipv4() && !Endpoint::is_tailscale(&a.to_string()))
        .or_else(|| addresses.iter().find(|a| a.is_ipv4()))
        .or_else(|| addresses.iter().find(|a| matches!(a, IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) != 0xfe80)))?;
    let p = info.get_property("p").map(|p| p.val_str().to_string());
    let tag = info.get_property_val_str("r").map(str::to_string).filter(|t| !t.is_empty());
    Some(HomeMac {
        service_name: instance_name(info.get_fullname(), ty),
        host: over.map(|o| o.host.clone()).unwrap_or_else(|| address.to_string()),
        port: over.map(|o| o.port).unwrap_or_else(|| info.get_port()),
        door: HomeDoor::of(p.as_deref()),
        tag,
        since: was.map(|w| w.since).unwrap_or_else(monotonic),
    })
}
