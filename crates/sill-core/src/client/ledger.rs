//! The Mac's settings as this device shows them (iOSClient/HostSettingsLedger.swift): the last
//! kind 16 on this connection with this device's unanswered picks laid over it. A pick sends only
//! what changes what is shown; only the answer carrying a pick's token clears it; a value the
//! answer does not hold was refused; a pick unanswered after max(4 s, 4 × the round trip) goes
//! back. Nothing is sent before the connection's first kind 16. Pure: no clock or socket.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::protocol::{HostSettingsState, SettingsChange, StreamSettings};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SettingsField {
    MaxFps,
    Bitrate,
    CaptureScale,
    PrioritizeSpeed,
    VirtualDisplay,
    DirectWireless,
    SendAudio,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Val {
    Int(i64),
    Double(f64),
    Bool(bool),
}

#[derive(Debug, Clone, Copy)]
struct Pick {
    token: i64,
    value: Val,
    at: u64,
}

/// Tokens increase for the life of the process, never reset.
static NEXT_TOKEN: AtomicI64 = AtomicI64::new(1);

#[derive(Debug, Default)]
pub struct SettingsLedger {
    host: Option<HostSettingsState>,
    picks: BTreeMap<SettingsField, Pick>,
}

impl SettingsLedger {
    pub fn new() -> SettingsLedger {
        SettingsLedger::default()
    }

    /// The last kind 16.
    pub fn host(&self) -> Option<&HostSettingsState> {
        self.host.as_ref()
    }

    /// The Mac's settings with this device's unanswered picks over them.
    pub fn shown(&self) -> Option<StreamSettings> {
        let mut s = self.host.as_ref()?.settings.clone();
        for (f, p) in &self.picks {
            with(*f, p.value, &mut s);
        }
        Some(s)
    }

    pub fn is_pending(&self, field: SettingsField) -> bool {
        self.picks.contains_key(&field)
    }

    pub fn any_pending(&self) -> bool {
        !self.picks.is_empty()
    }

    /// The kind 17 to send for this pick, with its token, or `None` when it changes nothing shown.
    /// `now`: milliseconds on a monotonic clock.
    pub fn pick(&mut self, change: &SettingsChange, now: u64) -> Option<(i64, SettingsChange)> {
        let s = self.shown()?;
        let wanted = change.applied(&s);
        let mut out = SettingsChange::default();
        let mut fields: Vec<(SettingsField, Val)> = Vec::new();
        if wanted.max_fps != s.max_fps {
            out.max_fps = Some(wanted.max_fps);
            fields.push((SettingsField::MaxFps, Val::Int(wanted.max_fps)));
        }
        if wanted.bitrate != s.bitrate {
            out.bitrate = Some(wanted.bitrate);
            fields.push((SettingsField::Bitrate, Val::Int(wanted.bitrate)));
        }
        if wanted.capture_scale != s.capture_scale {
            out.capture_scale = Some(wanted.capture_scale);
            fields.push((SettingsField::CaptureScale, Val::Double(wanted.capture_scale)));
        }
        if wanted.prioritize_speed != s.prioritize_speed {
            out.prioritize_speed = Some(wanted.prioritize_speed);
            fields.push((SettingsField::PrioritizeSpeed, Val::Bool(wanted.prioritize_speed)));
        }
        if wanted.virtual_display != s.virtual_display {
            out.virtual_display = Some(wanted.virtual_display);
            fields.push((SettingsField::VirtualDisplay, Val::Bool(wanted.virtual_display)));
        }
        // A field the Mac reported as nil (an older Mac) is never sent.
        if let (Some(_), Some(w)) = (s.direct_wireless, wanted.direct_wireless) {
            if Some(w) != s.direct_wireless {
                out.direct_wireless = Some(w);
                fields.push((SettingsField::DirectWireless, Val::Bool(w)));
            }
        }
        if let (Some(_), Some(w)) = (s.send_audio, wanted.send_audio) {
            if Some(w) != s.send_audio {
                out.send_audio = Some(w);
                fields.push((SettingsField::SendAudio, Val::Bool(w)));
            }
        }
        if out.is_empty() {
            return None;
        }
        let token = NEXT_TOKEN.fetch_add(1, Ordering::SeqCst);
        for (f, v) in fields {
            self.picks.insert(f, Pick { token, value: v, at: now });
        }
        Some((token, out))
    }

    /// A kind 16. Returns the fields the Mac refused (an answer that holds another value).
    pub fn receive(&mut self, state: HostSettingsState) -> Vec<SettingsField> {
        let answering = state.answering;
        let settings = state.settings.clone();
        self.host = Some(state);
        let Some(answering) = answering else { return Vec::new() };
        let mut refused = Vec::new();
        self.picks.retain(|f, p| {
            if p.token != answering {
                return true;
            }
            if value_of(*f, &settings) != Some(p.value) {
                refused.push(*f);
            }
            false
        });
        refused
    }

    /// Picks unanswered for longer than `timeout_ms` go back; whether any did.
    pub fn expire(&mut self, now: u64, timeout_ms: u64) -> bool {
        let before = self.picks.len();
        self.picks.retain(|_, p| now.saturating_sub(p.at) <= timeout_ms);
        self.picks.len() != before
    }

    pub fn oldest_pick_at(&self) -> Option<u64> {
        self.picks.values().map(|p| p.at).min()
    }

    pub fn reset(&mut self) {
        self.host = None;
        self.picks.clear();
    }
}

fn value_of(f: SettingsField, s: &StreamSettings) -> Option<Val> {
    match f {
        SettingsField::MaxFps => Some(Val::Int(s.max_fps)),
        SettingsField::Bitrate => Some(Val::Int(s.bitrate)),
        SettingsField::CaptureScale => Some(Val::Double(s.capture_scale)),
        SettingsField::PrioritizeSpeed => Some(Val::Bool(s.prioritize_speed)),
        SettingsField::VirtualDisplay => Some(Val::Bool(s.virtual_display)),
        SettingsField::DirectWireless => s.direct_wireless.map(Val::Bool),
        SettingsField::SendAudio => s.send_audio.map(Val::Bool),
    }
}

fn with(f: SettingsField, v: Val, s: &mut StreamSettings) {
    match (f, v) {
        (SettingsField::MaxFps, Val::Int(v)) => s.max_fps = v,
        (SettingsField::Bitrate, Val::Int(v)) => s.bitrate = v,
        (SettingsField::CaptureScale, Val::Double(v)) => s.capture_scale = v,
        (SettingsField::PrioritizeSpeed, Val::Bool(v)) => s.prioritize_speed = v,
        (SettingsField::VirtualDisplay, Val::Bool(v)) => s.virtual_display = v,
        (SettingsField::DirectWireless, Val::Bool(v)) => s.direct_wireless = Some(v),
        (SettingsField::SendAudio, Val::Bool(v)) => s.send_audio = Some(v),
        _ => {}
    }
}
