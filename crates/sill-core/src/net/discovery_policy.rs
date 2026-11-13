//! After the background, and a Mac whose row stays listed (iOSClient/DiscoveryPolicy.swift).
//! Pure: times are seconds on one monotonic clock.

/// Whether to act now, or when to look again.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Due {
    pub now: bool,
    pub recheck_at: Option<f64>,
}

pub struct DiscoveryPolicy;

impl DiscoveryPolicy {
    /// A Mac on the LAN answers mDNS well within this.
    pub const NETWORK_FIRST: f64 = 3.0;
    /// The waits before a browser that failed is made again, after 1, 2, 3… failures in a row.
    pub const BROWSER_RETRY: [f64; 6] = [1.0, 2.0, 4.0, 8.0, 16.0, 30.0];
    /// A browser that ran this long before it failed counts as the first failure in a row.
    pub const BROWSER_STABLE: f64 = 30.0;
    /// How long a browser made again shows the results of the browser it replaced beside its own.
    pub const REBUILD_SETTLE: f64 = Self::NETWORK_FIRST;
    /// The waits between the automatic reconnect's dials of one listing of its Mac's row that do
    /// not connect, counted from the last dial's start: 5, 10, 20, then 30 s.
    pub const RECONNECT_RETRY: [f64; 4] = [5.0, 10.0, 20.0, 30.0];
    /// After this many dials of a listing that did not connect, the status line says the Mac may
    /// be asleep.
    pub const RECONNECT_QUIET_AFTER: u32 = 3;
    /// After this many the dials of that listing stop (about 4.6 minutes).
    pub const RECONNECT_DIALS: u32 = 12;
    /// After a session ends by itself, automatic remote dials stop this long after the loss.
    pub const REDIAL_WINDOW: f64 = 120.0;
    /// Nor within this long of the network last listing that Mac.
    pub const NETWORK_GRACE: f64 = 10.0;
    /// A failed automatic remote dial is tried again after 2, 4, 8, then every 10 s.
    pub const REMOTE_RETRY: [f64; 4] = [2.0, 4.0, 8.0, 10.0];

    /// The wait before a browser is made again after `failures` failures in a row.
    pub fn browser_retry_delay(failures: i32) -> f64 {
        Self::BROWSER_RETRY[(failures.clamp(1, Self::BROWSER_RETRY.len() as i32) - 1) as usize]
    }

    /// Failures in a row once a browser started at `started_at` fails at `now`, `previous` before it.
    pub fn browser_failures(previous: i32, started_at: f64, now: f64) -> i32 {
        if now - started_at >= Self::BROWSER_STABLE { 1 } else { previous.max(0) + 1 }
    }

    /// Whether to make a browser again now, or when to look again. At once on the app's return to
    /// the screen, failed or not. One that failed once `browser_retry_delay` has passed since, and
    /// only in the foreground. One that runs is left alone.
    pub fn rebuild_browser(failed_at: Option<f64>, failures: i32, came_forward: bool, foreground: bool, now: f64) -> Due {
        if came_forward {
            return Due { now: true, recheck_at: None };
        }
        let (true, Some(failed_at)) = (foreground, failed_at) else { return Due { now: false, recheck_at: None } };
        let due = failed_at + Self::browser_retry_delay(failures);
        if now >= due { Due { now: true, recheck_at: None } } else { Due { now: false, recheck_at: Some(due) } }
    }

    /// What the rows are read from: while a browser made again settles (before `settle_ends`), its
    /// own results (`fresh`), then those of the browser it replaced (`kept`) whose names it does
    /// not list yet; after that, its own alone.
    pub fn browser_results<R: Clone>(kept: &[R], fresh: &[R], name: impl Fn(&R) -> String, settle_ends: Option<f64>, now: f64) -> Vec<R> {
        let mut out = fresh.to_vec();
        match settle_ends {
            Some(ends) if now < ends => {
                let listed: Vec<String> = fresh.iter().map(&name).collect();
                out.extend(kept.iter().filter(|k| !listed.contains(&name(k))).cloned());
                out
            }
            _ => out,
        }
    }

    /// Whether the automatic reconnect dials its Mac's listed row now, or when to look again,
    /// after `failed` dials of this listing that did not connect, the last started at
    /// `last_dial_at`: the first at once; each next `RECONNECT_RETRY` after the last started; none
    /// after `RECONNECT_DIALS`; none in the background.
    pub fn redial_row(failed: u32, last_dial_at: Option<f64>, foreground: bool, now: f64) -> Due {
        if !foreground || failed >= Self::RECONNECT_DIALS {
            return Due { now: false, recheck_at: None };
        }
        let (true, Some(last)) = (failed > 0, last_dial_at) else { return Due { now: true, recheck_at: None } };
        let due = last + Self::RECONNECT_RETRY[(failed as usize).min(Self::RECONNECT_RETRY.len()) - 1];
        if now >= due { Due { now: true, recheck_at: None } } else { Due { now: false, recheck_at: Some(due) } }
    }

    /// The status line while the automatic reconnect dials its Mac's listed row.
    pub fn reconnect_line(mac: &str, failed: u32) -> String {
        if failed >= Self::RECONNECT_DIALS {
            Self::stopped_line(mac)
        } else if failed >= Self::RECONNECT_QUIET_AFTER {
            format!("{mac} isn’t answering. It may be asleep. Sill keeps trying, or tap it to try now.")
        } else {
            format!("Reconnecting to {mac}…")
        }
    }

    pub fn stopped_line(mac: &str) -> String {
        format!("Stopped trying to reach {mac}. Tap it to try again.")
    }

    pub fn remote_retry_delay(failures: i32) -> f64 {
        Self::REMOTE_RETRY[(failures.clamp(1, Self::REMOTE_RETRY.len() as i32) - 1) as usize]
    }

    /// Another key answered the automatic reconnect of a Mac this device was connected to without
    /// pairing.
    pub fn other_key_line(mac: &str, device: &str) -> String {
        format!("{mac} answered with another key, so this {device} didn’t reconnect. Tap it to connect.")
    }
}

/// What the reconnect does next.
#[derive(Debug, Clone, PartialEq)]
pub enum ReconnectStep {
    /// Dial now, with this status line.
    Dial { line: String },
    /// Nothing now: show the line, and look again at `at` (`None`: at the next event only).
    Wait { at: Option<f64>, line: String },
}

/// The automatic reconnect of one Mac (`StreamClient`'s `Reconnect` and `reconnectIfListed`):
/// when its row is listed at home, its dials of that listing (`DiscoveryPolicy::redial_row`); when
/// not, its remote dials (2, 4, 8, then every 10 s, for `REDIAL_WINDOW` after the loss). It lives
/// until a session connects, a tap or Disconnect, or an answer that ends it. Pure: the client asks
/// it at every loss, failed dial, discovery change, return to the screen and the time it said to
/// look again, and does what it says.
#[derive(Debug, Clone)]
pub struct ReconnectClock {
    pub mac_id: String,
    pub name: String,
    lost_at: f64,
    on_stream: bool,
    row_listing: Option<f64>,
    row_dials: u32,
    last_row_dial_at: Option<f64>,
    remote_failures: i32,
    next_remote_at: f64,
    remote_allowed: bool,
    row_left_at: Option<f64>,
    last_was_row: bool,
    dialing: bool,
}

impl ReconnectClock {
    /// `on_stream`: the stream screen stays up while the reconnect is still a blip (a session
    /// that was streaming).
    pub fn new(mac_id: impl Into<String>, name: impl Into<String>, lost_at: f64, on_stream: bool) -> ReconnectClock {
        ReconnectClock {
            mac_id: mac_id.into(),
            name: name.into(),
            lost_at,
            on_stream,
            row_listing: None,
            row_dials: 0,
            last_row_dial_at: None,
            remote_failures: 0,
            next_remote_at: lost_at,
            remote_allowed: true,
            row_left_at: None,
            last_was_row: false,
            dialing: false,
        }
    }

    pub fn lost_at(&self) -> f64 {
        self.lost_at
    }
    pub fn row_listing(&self) -> Option<f64> {
        self.row_listing
    }
    pub fn row_dials(&self) -> u32 {
        self.row_dials
    }
    pub fn last_row_dial_at(&self) -> Option<f64> {
        self.last_row_dial_at
    }
    pub fn remote_failures(&self) -> i32 {
        self.remote_failures
    }
    pub fn next_remote_at(&self) -> f64 {
        self.next_remote_at
    }
    pub fn remote_allowed(&self) -> bool {
        self.remote_allowed
    }
    pub fn row_left_at(&self) -> Option<f64> {
        self.row_left_at
    }

    /// The Mac looks asleep or out of reach: the connect screen, with the line.
    pub fn quiet(&self) -> bool {
        self.row_dials.saturating_sub(u32::from(self.dialing && self.last_was_row)) >= DiscoveryPolicy::RECONNECT_QUIET_AFTER
            || !self.remote_allowed
    }

    /// Whether the stream screen stays up meanwhile.
    pub fn shows_stream(&self) -> bool {
        self.on_stream && !self.quiet()
    }

    /// `listed_since`: when the Mac's home row now listed was first listed; `None` when no row
    /// lists it.
    pub fn next(&mut self, listed_since: Option<f64>, foreground: bool, now: f64) -> ReconnectStep {
        if let Some(since) = listed_since {
            if self.row_listing != Some(since) {
                self.row_listing = Some(since);
                self.row_dials = 0;
                self.last_row_dial_at = None;
            }
            let line = DiscoveryPolicy::reconnect_line(&self.name, self.row_dials);
            let due = DiscoveryPolicy::redial_row(self.row_dials, self.last_row_dial_at, foreground, now);
            if !due.now {
                return ReconnectStep::Wait { at: due.recheck_at, line };
            }
            self.row_dials += 1;
            self.last_row_dial_at = Some(now);
            self.last_was_row = true;
            self.dialing = true;
            return ReconnectStep::Dial { line };
        }
        if self.row_listing.is_some() {
            // The row went: a listing that comes back is a new one, dialed at once.
            self.row_listing = None;
            self.row_left_at = Some(now);
        }
        if !self.remote_allowed || now >= self.lost_at + DiscoveryPolicy::REDIAL_WINDOW {
            self.remote_allowed = false;
            return ReconnectStep::Wait { at: None, line: DiscoveryPolicy::stopped_line(&self.name) };
        }
        let line = format!("Reconnecting to {}…", self.name);
        if !foreground {
            return ReconnectStep::Wait { at: None, line };
        }
        let due = match self.row_left_at {
            Some(left) => self.next_remote_at.max(left + DiscoveryPolicy::NETWORK_GRACE),
            None => self.next_remote_at,
        };
        if now < due {
            return ReconnectStep::Wait { at: Some(due.min(self.lost_at + DiscoveryPolicy::REDIAL_WINDOW)), line };
        }
        self.last_was_row = false;
        self.dialing = true;
        ReconnectStep::Dial { line }
    }

    /// The dial `next` asked for did not connect. A row's was counted when it started; a remote
    /// one waits its back-off.
    pub fn dial_failed(&mut self, now: f64) {
        self.dialing = false;
        if self.last_was_row {
            return;
        }
        self.remote_failures += 1;
        self.next_remote_at = now + DiscoveryPolicy::remote_retry_delay(self.remote_failures);
    }

    /// The Mac answered for good (it refused this device's key): no more remote dials; a listed
    /// row still counts.
    pub fn answered_for_good(&mut self) {
        self.remote_allowed = false;
    }

    /// Back on the screen: the person is back, so a listed row is dialed at once, and the remote
    /// dials get their window again from now.
    pub fn came_forward(&mut self, now: f64) {
        self.row_dials = 0;
        self.last_row_dial_at = None;
        self.lost_at = now;
        self.remote_failures = 0;
        self.next_remote_at = now;
        self.remote_allowed = true;
        self.row_left_at = None;
    }
}
