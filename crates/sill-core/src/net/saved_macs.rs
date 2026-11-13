//! The Macs this device paired with (SavedMacs.swift), as a JSON file in the config directory,
//! and how they are dialed.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{json, Map, Value};

use crate::crypto::keystore::write_private;
use crate::crypto::{base64url, mac_id};
use crate::protocol::json::J;
use crate::protocol::MacAddress;

/// A Mac this device paired with: its pin, its name, how to reach it. The record is refused
/// unless its Mac ID is its fingerprint's, so a damaged store can never pin a stranger.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedMac {
    pub mac_id: String,
    /// SHA-256 of the Mac's SubjectPublicKeyInfo, 32 bytes.
    pub fingerprint: Vec<u8>,
    pub name: String,
    /// base64url, for the Bonjour recognition tag.
    pub recognition_key: Option<String>,
    pub remote_port: u16,
    /// From the Mac's signed kind 18, in its dial order.
    pub addresses: Vec<MacAddress>,
    /// What the person typed when pairing ("mac.tail1234.ts.net", "100.101.102.103:7455").
    pub typed_addresses: Vec<String>,
    pub info_issued_at: f64,
    /// "host|port" of the last address a session reached.
    pub last_worked: Option<String>,
    pub paired_at: f64,
    pub last_connected_at: Option<f64>,
    /// The Mac removed this device (goodbye `removed`, or its key refused).
    pub revoked: bool,
}

impl SavedMac {
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("macID".into(), json!(self.mac_id));
        o.insert("fingerprint".into(), json!(base64url::encode(&self.fingerprint)));
        o.insert("name".into(), json!(self.name));
        if let Some(k) = &self.recognition_key {
            o.insert("recognitionKey".into(), json!(k));
        }
        o.insert("remotePort".into(), json!(self.remote_port));
        let addresses: Vec<Value> = self
            .addresses
            .iter()
            .map(|a| {
                let mut m = json!({"host": a.host, "kind": a.kind, "via": a.via});
                if let Some(p) = a.port {
                    m["port"] = json!(p);
                }
                m
            })
            .collect();
        o.insert("addresses".into(), Value::Array(addresses));
        o.insert("typedAddresses".into(), json!(self.typed_addresses));
        o.insert("infoIssuedAt".into(), json!(self.info_issued_at));
        if let Some(l) = &self.last_worked {
            o.insert("lastWorked".into(), json!(l));
        }
        o.insert("pairedAt".into(), json!(self.paired_at));
        if let Some(l) = self.last_connected_at {
            o.insert("lastConnectedAt".into(), json!(l));
        }
        o.insert("revoked".into(), json!(self.revoked));
        Value::Object(o)
    }

    pub fn from_json(v: &Value) -> Option<SavedMac> {
        let o = J(v.as_object()?);
        let fingerprint = base64url::decode(&o.string_req("fingerprint")?)?;
        let id = o.string_req("macID")?;
        if fingerprint.len() != 32 || mac_id::make(&fingerprint) != id {
            return None;
        }
        let mut addresses = Vec::new();
        for a in o.array("addresses").map(|a| a.as_slice()).unwrap_or(&[]) {
            let Some(a) = a.as_object() else { continue };
            let a = J(a);
            addresses.push(MacAddress {
                host: a.string_req("host")?,
                port: a.int_opt("port").and_then(|p| u16::try_from(p).ok()),
                kind: a.string("kind"),
                via: a.string("via"),
            });
        }
        let mut typed = Vec::new();
        for t in o.array("typedAddresses").map(|a| a.as_slice()).unwrap_or(&[]) {
            typed.push(t.as_str()?.to_string());
        }
        let non_empty = |k: &str| o.string_opt(k).filter(|s| !s.is_empty() && s != "null");
        Some(SavedMac {
            mac_id: id,
            fingerprint,
            name: o.string_or("name", "Mac"),
            recognition_key: non_empty("recognitionKey"),
            remote_port: u16::try_from(o.int_or("remotePort", 7455)).ok().filter(|p| *p > 0).unwrap_or(7455),
            addresses,
            typed_addresses: typed,
            info_issued_at: o.double_or("infoIssuedAt", 0.0),
            last_worked: non_empty("lastWorked"),
            paired_at: o.double_or("pairedAt", 0.0),
            last_connected_at: o.double_opt("lastConnectedAt").filter(|d| *d > 0.0),
            revoked: o.bool("revoked"),
        })
    }
}

/// The saved Macs, newest pairing first, at most 16, in `saved-macs.json` of the directory given.
/// Every change is on disk before the call returns, so a Mac just paired stays saved when the app
/// is killed right after.
pub struct SavedMacs {
    path: Option<PathBuf>,
    list: Mutex<Vec<SavedMac>>,
}

impl SavedMacs {
    pub const FILE_NAME: &'static str = "saved-macs.json";
    pub const LIMIT: usize = 16;

    /// The store in `dir` (created when a Mac is first saved).
    pub fn open(dir: impl Into<PathBuf>) -> SavedMacs {
        let path = dir.into().join(Self::FILE_NAME);
        let list = std::fs::read(&path).ok().map(|d| Self::decode(&d)).unwrap_or_default();
        SavedMacs { path: Some(path), list: Mutex::new(list) }
    }

    /// A store that is never written anywhere, for tests.
    pub fn in_memory() -> SavedMacs {
        SavedMacs { path: None, list: Mutex::new(Vec::new()) }
    }

    /// The records of a store's JSON; those that do not read are left out.
    pub fn decode(bytes: &[u8]) -> Vec<SavedMac> {
        match serde_json::from_slice::<Value>(bytes) {
            Ok(Value::Array(arr)) => arr.iter().filter_map(SavedMac::from_json).collect(),
            _ => Vec::new(),
        }
    }

    pub fn encode(list: &[SavedMac]) -> Vec<u8> {
        Value::Array(list.iter().map(SavedMac::to_json).collect()).to_string().into_bytes()
    }

    pub fn all(&self) -> Vec<SavedMac> {
        self.list.lock().unwrap().clone()
    }

    pub fn by_id(&self, mac_id: &str) -> Option<SavedMac> {
        self.list.lock().unwrap().iter().find(|m| m.mac_id == mac_id).cloned()
    }

    pub fn upsert(&self, mac: SavedMac) {
        let mut list = self.list.lock().unwrap();
        list.retain(|m| m.mac_id != mac.mac_id);
        list.insert(0, mac);
        list.truncate(Self::LIMIT);
        self.save(&list);
    }

    pub fn update(&self, mac_id: &str, change: impl FnOnce(&mut SavedMac)) {
        let mut list = self.list.lock().unwrap();
        if let Some(m) = list.iter_mut().find(|m| m.mac_id == mac_id) {
            change(m);
            self.save(&list);
        }
    }

    pub fn remove(&self, mac_id: &str) {
        let mut list = self.list.lock().unwrap();
        list.retain(|m| m.mac_id != mac_id);
        self.save(&list);
    }

    pub fn clear(&self) {
        let mut list = self.list.lock().unwrap();
        list.clear();
        self.save(&list);
    }

    fn save(&self, list: &[SavedMac]) {
        let Some(path) = &self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // The recognition keys are secrets of a kind: the owner's alone.
        let _ = write_private(path, &Self::encode(list));
    }

    /// Whether an address is worth saving for a later dial (`SavedMacs.keepable`): never a
    /// link-local one (169.254/16, fe80::/10, or one with a scope), which names a Mac on one link
    /// only, nor loopback (allowed for tests against a host on the same computer). The parsed
    /// host is classified, never a prefix a DNS name shares ("fearless.ts.net").
    pub fn keepable(host: &str, allow_loopback: bool) -> bool {
        let h = host.trim();
        let h = h.strip_prefix('[').unwrap_or(h);
        let h = h.strip_suffix(']').unwrap_or(h).to_lowercase();
        if h.is_empty() || h.contains('%') {
            return false;
        }
        let v4: Vec<&str> = h.split('.').collect();
        if v4.len() == 4 && v4.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|c| c.is_ascii_digit()) && p.parse::<u32>().unwrap() <= 255) {
            if v4[0] == "169" && v4[1] == "254" {
                return false;
            }
            if v4[0] == "127" {
                return allow_loopback;
            }
            return true;
        }
        if h.contains(':') {
            if h == "::1" {
                return allow_loopback;
            }
            let last = h.rsplit(':').next().unwrap_or("");
            // IPv4-mapped (::ffff:a.b.c.d): its IPv4 address decides.
            if h.starts_with("::ffff:") && last.contains('.') {
                return Self::keepable(last, allow_loopback);
            }
            let first = h.split(':').next().unwrap_or("");
            if first.is_empty() {
                return true; // "::…", not link-local
            }
            return match u32::from_str_radix(first, 16) {
                Ok(word) => word & 0xffc0 != 0xfe80,
                Err(_) => false,
            };
        }
        if h == "localhost" || h == "localhost." {
            return allow_loopback;
        }
        true
    }
}

/// An address as typed or as the Mac lists it: host and port, IPv6 without brackets.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.host.contains(':') { write!(f, "[{}]:{}", self.host, self.port) } else { write!(f, "{}:{}", self.host, self.port) }
    }
}

fn parse_port(s: &str) -> Option<u16> {
    s.parse::<u32>().ok().filter(|p| (1..=65535).contains(p)).map(|p| p as u16)
}

impl Endpoint {
    pub fn new(host: impl Into<String>, port: u16) -> Endpoint {
        Endpoint { host: host.into(), port }
    }

    /// "host|port": how the last address that worked is remembered.
    pub fn key(&self) -> String {
        format!("{}|{}", self.host, self.port)
    }

    /// "host", "host:port", "[v6]:port" or a bare IPv6 address; `None` for anything else.
    pub fn parse(raw: &str, default_port: u16) -> Option<Endpoint> {
        let t = raw.trim();
        if t.is_empty() || t.starts_with("sill://") {
            return None;
        }
        if let Some(rest) = t.strip_prefix('[') {
            let close = rest.find(']')?;
            let host = &rest[..close];
            let after = &rest[close + 1..];
            let port = match after.strip_prefix(':') {
                Some(p) => parse_port(p)?,
                None => default_port,
            };
            return if port >= 1 && !host.is_empty() { Some(Endpoint::new(host, port)) } else { None };
        }
        let colons = t.matches(':').count();
        if colons > 1 {
            return Some(Endpoint::new(t, default_port)); // a bare IPv6 address
        }
        if colons == 1 {
            let (host, port) = t.split_once(':')?;
            let port = parse_port(port)?;
            return if !host.is_empty() && !host.chars().any(char::is_whitespace) { Some(Endpoint::new(host, port)) } else { None };
        }
        if t.chars().any(|c| c.is_whitespace() || c == '/') { None } else { Some(Endpoint::new(t, default_port)) }
    }

    pub fn is_tailscale(host: &str) -> bool {
        let lower = host.to_lowercase();
        if lower.ends_with(".ts.net") || lower.starts_with("fd7a:115c:a1e0:") {
            return true;
        }
        let parts: Vec<Option<u32>> = host.split('.').map(|p| p.parse::<u32>().ok().filter(|v| *v <= 255)).collect();
        if parts.len() == 4 && parts.iter().all(Option::is_some) {
            let (a, b) = (parts[0].unwrap(), parts[1].unwrap());
            return a == 100 && (64..=127).contains(&b);
        }
        false
    }

    pub fn is_loopback(host: &str) -> bool {
        host == "127.0.0.1" || host == "::1" || host.eq_ignore_ascii_case("localhost")
    }
}

/// The order a saved Mac is dialed in (`RemoteDialPolicy`): loopback, then VPN names and
/// addresses (Tailscale's), then the rest as the Mac listed them, then what was typed; the address
/// that last worked goes first; duplicates once.
pub struct DialOrder;

impl DialOrder {
    pub fn endpoints(mac: &SavedMac) -> Vec<Endpoint> {
        fn rank(e: &Endpoint, kind: Option<&str>) -> u8 {
            if Endpoint::is_loopback(&e.host) {
                0
            } else if kind == Some("vpn") || Endpoint::is_tailscale(&e.host) {
                if e.host.chars().any(char::is_alphabetic) && !e.host.contains(':') { 1 } else { 2 }
            } else if kind == Some("lan") {
                3
            } else if kind == Some("internet") {
                4
            } else {
                5
            }
        }
        let mut all: Vec<(Endpoint, Option<&str>)> = mac
            .addresses
            .iter()
            .map(|a| (Endpoint::new(a.host.clone(), a.port.unwrap_or(mac.remote_port)), Some(a.kind.as_str())))
            .collect();
        all.extend(mac.typed_addresses.iter().filter_map(|t| Endpoint::parse(t, mac.remote_port)).map(|e| (e, None)));
        all.sort_by_key(|(e, k)| rank(e, *k)); // stable
        let mut ordered: Vec<Endpoint> = Vec::new();
        if let Some(lw) = &mac.last_worked {
            if let Some((host, port)) = lw.rsplit_once('|') {
                if let (false, Some(port)) = (host.is_empty(), parse_port(port)) {
                    ordered.push(Endpoint::new(host, port));
                }
            }
        }
        for (e, _) in all {
            if !ordered.contains(&e) {
                ordered.push(e);
            }
        }
        ordered
    }
}
