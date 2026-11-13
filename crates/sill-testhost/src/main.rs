//! `sill-testhost`: a synthetic Sill host on a TCP port, for a client to be tried against.
//!
//! ```text
//! sill-testhost [--port N] [--bind ADDR] [--dir STATE_DIR] [--code DIGITS] [--name NAME]
//!               [--mdns] [--input-log] [--audio] [--remote-door] [--open]
//! ```
//!
//! At start it prints, one per line: `PAIRING_CODE=…`, `ADDRESS=host:port`, `MAC_ID=…`,
//! `FINGERPRINT=<hex of the SHA-256 of the host key's SubjectPublicKeyInfo>` and
//! `PAIR_LINK=sill://pair?…`. A later pairing window (after a device paired) prints its
//! `PAIRING_CODE=…` again. With `--input-log` every device event is one JSON line.

use sill_testhost::{Door, HostConfig, TestHost};
use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "usage: sill-testhost [--port N] [--bind ADDR] [--dir STATE_DIR] [--code DIGITS] [--name NAME]
                     [--mdns] [--input-log] [--audio] [--remote-door] [--open]

  --port N        listen on this port (default: an ephemeral one)
  --bind ADDR     listen on this address (default 127.0.0.1; 0.0.0.0 for every interface)
  --dir DIR       keep the identity and the paired devices here (default: in memory)
  --code DIGITS   a fixed pairing code: 12 digits with a valid Damm check digit
  --name NAME     the Mac's name (default \"Test Mac\")
  --mdns          advertise the home door as _sill._tcp with the TXT records r and p
  --input-log     print each device event as one JSON line
  --audio         Send Audio on from the start
  --remote-door   behave as the remote door (default: the home door)
  --open          Require pairing off: any device gets a session (home door)";

fn parse() -> Result<(HostConfig, bool), String> {
    let mut config = HostConfig::default();
    let mut input_log = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--port" => config.port = value("--port")?.parse().map_err(|_| "--port: a number from 0 to 65535".to_string())?,
            "--bind" => config.bind = value("--bind")?.parse().map_err(|_| "--bind: an IP address".to_string())?,
            "--dir" => config.dir = Some(value("--dir")?.into()),
            "--code" => config.code = Some(value("--code")?),
            "--name" => config.name = value("--name")?,
            "--mdns" => config.mdns = true,
            "--input-log" => input_log = true,
            "--audio" => config.send_audio = true,
            "--remote-door" => config.door = Door::Remote,
            "--open" => config.require_pairing = false,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok((config, input_log))
}

fn main() -> ExitCode {
    let (config, input_log) = match parse() {
        Ok(parsed) => parsed,
        Err(why) => {
            if !why.is_empty() {
                eprintln!("sill-testhost: {why}");
            }
            eprintln!("{USAGE}");
            return if why.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(2) };
        }
    };
    let host = match TestHost::start(config) {
        Ok(host) => host,
        Err(e) => {
            eprintln!("sill-testhost: {e}");
            return ExitCode::from(2);
        }
    };
    let mut code = host.pairing_code();
    let fingerprint: String = host.fingerprint().iter().map(|b| format!("{b:02x}")).collect();
    println!("PAIRING_CODE={code}");
    println!("ADDRESS={}", host.address());
    println!("MAC_ID={}", host.mac_id());
    println!("FINGERPRINT={fingerprint}");
    if let Some(link) = host.pair_link() {
        println!("PAIR_LINK={link}");
    }
    let _ = std::io::stdout().flush();

    let mut seen = 0u64;
    loop {
        let (events, count) = host.next_events(seen, Duration::from_secs(1));
        seen = count;
        let mut out = std::io::stdout().lock();
        if input_log {
            for event in &events {
                if let Ok(line) = serde_json::to_string(event) {
                    let _ = writeln!(out, "{line}");
                }
            }
        }
        // A fresh window after a pairing: its code, for whoever drives this host.
        let now = host.pairing_code();
        if now != code && !now.is_empty() {
            let _ = writeln!(out, "PAIRING_CODE={now}");
        }
        code = now;
        if out.flush().is_err() {
            return ExitCode::SUCCESS; // nobody reads any more
        }
    }
}
