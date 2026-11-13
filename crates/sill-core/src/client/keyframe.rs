//! When this device asks the Mac for a keyframe after its picture was interrupted (iOSClient's
//! KeyframeRecovery.swift): the decoder was rebuilt or fell behind and waits for a keyframe, and a
//! still window on the Mac sends none, so the device asks (kind 44), which the Mac answers by
//! encoding its last frame again.
//!
//! - One ask per interruption while its keyframe is due; an ask unanswered within the answer wait
//!   is owed again, as a repeat (a Mac from before kind 44 skips it).
//! - Only while the app is on screen and something streams.
//! - At least `SPACING` after the last ask; repeats double the spacing up to `LONGEST_SPACING`. A
//!   picture back for `CALM`, or a trip to the background, starts the spacing at 2 s again.
//!
//! Pure: times are seconds on a monotonic clock.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeyframeDecision {
    Nothing,
    AskNow,
    AskAt(f64),
    KeyframeDue(f64),
}

#[derive(Debug, Clone, Default)]
pub struct KeyframeRecovery {
    open: bool,
    asked: bool,
    repeats: u32,
    last_ask: Option<f64>,
    back_at: Option<f64>,
    went_away: bool,
}

impl KeyframeRecovery {
    pub const SPACING: f64 = 2.0;
    pub const LONGEST_SPACING: f64 = 32.0;
    pub const CALM: f64 = 10.0;
    pub const SHORTEST_WAIT: f64 = 4.0;

    pub fn new() -> KeyframeRecovery {
        KeyframeRecovery::default()
    }

    /// 4 s, or four of the worst recent round trips on a slow link.
    pub fn answer_wait(worst_rtt_ms: Option<i32>) -> f64 {
        Self::SHORTEST_WAIT.max(4.0 * f64::from(worst_rtt_ms.unwrap_or(0).max(0)) / 1000.0)
    }

    pub fn open(&self) -> bool {
        self.open
    }
    pub fn asked(&self) -> bool {
        self.asked
    }
    pub fn repeats(&self) -> u32 {
        self.repeats
    }
    pub fn last_ask(&self) -> Option<f64> {
        self.last_ask
    }

    pub fn spacing_now(&self) -> f64 {
        (Self::SPACING * 2f64.powi(self.repeats.min(30) as i32)).min(Self::LONGEST_SPACING)
    }

    pub fn interrupted(&mut self, now: f64) {
        if self.open {
            return;
        }
        self.open = true;
        self.asked = false;
        self.repeats = match self.back_at {
            Some(back) if !self.went_away && now >= back && now - back < Self::CALM => self.repeats + 1,
            _ => 0,
        };
    }

    pub fn picture_back(&mut self, now: f64, active: bool) {
        if !self.open {
            return;
        }
        self.open = false;
        self.back_at = Some(now);
        if active {
            self.went_away = false;
        }
    }

    pub fn left_foreground(&mut self) {
        self.went_away = true;
        self.asked = false;
        self.repeats = 0;
    }

    pub fn consider(&mut self, now: f64, active: bool, streaming: bool, wait: f64) -> KeyframeDecision {
        if !self.open || !active || !streaming {
            return KeyframeDecision::Nothing;
        }
        if self.asked {
            if let Some(last) = self.last_ask {
                if now >= last && now < last + wait {
                    return KeyframeDecision::KeyframeDue(last + wait);
                }
            }
            self.asked = false;
            self.repeats += 1;
        }
        if let Some(last) = self.last_ask {
            if now >= last && now < last + self.spacing_now() {
                return KeyframeDecision::AskAt(last + self.spacing_now());
            }
        }
        self.asked = true;
        self.last_ask = Some(now);
        KeyframeDecision::AskNow
    }

    pub fn reset(&mut self) {
        *self = KeyframeRecovery::default();
    }
}

/// When the device holds its screen on (iOSClient's DeviceIdlePolicy.swift): while the app is on
/// screen and a source streams, until `IDLE_LIMIT_S` after the person last used the session.
pub struct DeviceIdlePolicy;

impl DeviceIdlePolicy {
    pub const IDLE_LIMIT_S: f64 = 600.0;

    /// When the hold ends, or `None` when the screen is not held on at `now`.
    pub fn hold_ends(foreground: bool, streaming: bool, last_use: Option<f64>, now: f64, limit: f64) -> Option<f64> {
        if !foreground || !streaming {
            return None;
        }
        let end = last_use? + limit;
        if end > now { Some(end) } else { None }
    }

    /// A session's start as use: one the person started counts from now; the automatic reconnect
    /// keeps the last one's.
    pub fn session_start(previous: Option<f64>, reconnected: bool, now: f64) -> Option<f64> {
        if reconnected { previous } else { Some(now) }
    }
}
