//! What a device sent, decoded, and the log that keeps it.
//!
//! The payload shapes were read from `Sources/StreamProtocol` (`Input.swift`,
//! `Switcher.swift`, `Viewport.swift`, `Compatibility.swift`, `HostSettings.swift`,
//! `MacMenu.swift`, `Gesture.swift`, `Clipboard.swift`, `Remote.swift`). A field Swift's
//! `JSONDecoder` requires is required here too, so a payload the real host would drop
//! shows up as [`DeviceEvent::Undecodable`]. One difference: a whole number written with
//! a fraction (`60.0`) decodes as an `Int` in Swift and not here.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// What the host streams (`StreamSource`). On the wire Swift's synthesized coding:
/// `{"none":{}}`, `{"desktop":{}}`, `{"window":{"_0":101}}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Nothing streams.
    None,
    /// One window, by its id.
    Window(u32),
    /// The Mac's main display.
    Desktop,
}

impl Source {
    /// The wire form.
    pub fn to_json(self) -> Value {
        match self {
            Source::None => serde_json::json!({"none": {}}),
            Source::Desktop => serde_json::json!({"desktop": {}}),
            Source::Window(id) => serde_json::json!({"window": {"_0": id}}),
        }
    }

    /// Parses the wire form; `None` for anything else (exactly one case key, as Swift requires).
    pub fn from_json(v: &Value) -> Option<Source> {
        let o = v.as_object()?;
        if o.len() != 1 {
            return None;
        }
        let (k, inner) = o.iter().next()?;
        let inner = inner.as_object()?;
        match k.as_str() {
            "none" => Some(Source::None),
            "desktop" => Some(Source::Desktop),
            "window" => u32::try_from(inner.get("_0")?.as_u64()?).ok().map(Source::Window),
            _ => None,
        }
    }
}

impl Serialize for Source {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(s)
    }
}

/// A kind 8 payload (`InputEvent`). On the wire Swift's synthesized coding:
/// `{"pointer":{"_0":"move","x":0.5,"y":0.5}}`, `{"scroll":{"x":…,"y":…,"dx":…,"dy":…}}`,
/// `{"text":{"_0":"abc"}}`, `{"scrollGesture":{"_0":"began","x":…,"y":…}}`,
/// `{"key":{"hidUsage":4,"down":true,"modifiers":0}}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InputEvent {
    /// `action`: "move", "leftDown", "leftUp", "rightDown" or "rightUp"; x and y are
    /// fractions of the streamed frame.
    Pointer { action: String, x: f64, y: f64 },
    /// A scroll with the cursor at (x, y); dx and dy are fractions of the frame.
    Scroll { x: f64, y: f64, dx: f64, dy: f64 },
    /// Text from the software keyboard.
    Text { text: String },
    /// `phase`: "began", "ended", "momentumBegan" or "momentumEnded".
    #[serde(rename_all = "camelCase")]
    ScrollGesture { phase: String, x: f64, y: f64 },
    /// A key by USB HID usage, with UIKeyModifierFlags bits.
    #[serde(rename_all = "camelCase")]
    Key { hid_usage: u16, down: bool, modifiers: u64 },
}

impl InputEvent {
    /// Parses the wire form as Swift's decoder would: exactly one case key, every
    /// associated value present, the enums' raw values known.
    pub fn from_json(v: &Value) -> Option<InputEvent> {
        let o = v.as_object()?;
        if o.len() != 1 {
            return None;
        }
        let (k, inner) = o.iter().next()?;
        let f = |name: &str| inner.get(name).and_then(Value::as_f64);
        match k.as_str() {
            "pointer" => {
                let action = inner.get("_0")?.as_str()?;
                if !["move", "leftDown", "leftUp", "rightDown", "rightUp"].contains(&action) {
                    return None;
                }
                Some(InputEvent::Pointer { action: action.into(), x: f("x")?, y: f("y")? })
            }
            "scroll" => Some(InputEvent::Scroll { x: f("x")?, y: f("y")?, dx: f("dx")?, dy: f("dy")? }),
            "text" => Some(InputEvent::Text { text: inner.get("_0")?.as_str()?.into() }),
            "scrollGesture" => {
                let phase = inner.get("_0")?.as_str()?;
                if !["began", "ended", "momentumBegan", "momentumEnded"].contains(&phase) {
                    return None;
                }
                Some(InputEvent::ScrollGesture { phase: phase.into(), x: f("x")?, y: f("y")? })
            }
            "key" => Some(InputEvent::Key {
                hid_usage: u16::try_from(inner.get("hidUsage")?.as_u64()?).ok()?,
                down: inner.get("down")?.as_bool()?,
                modifiers: inner.get("modifiers")?.as_u64()?,
            }),
            _ => None,
        }
    }
}

/// Kind 23 (`Hello`): every field optional, `{}` decodes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    /// CFBundleShortVersionString, what a host's floor compares.
    pub app_version: Option<String>,
    /// CFBundleVersion.
    pub build: Option<String>,
    /// `SillProtocol.current`: 1.
    pub protocol: Option<i64>,
    /// "iPad (iPad14,1)".
    pub device: Option<String>,
    /// The codecs of the Mac's sound this device plays, best first (`["aac-eld"]`).
    pub audio: Option<Vec<String>>,
    /// 1 from a device that shows a Mac's off-console view.
    pub off_console_view: Option<i64>,
}

/// Kind 9 (`Viewport`): `width` and `height` required.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Viewport {
    /// Device points.
    pub width: f64,
    pub height: f64,
    /// Device points per Mac point for text; `None` leaves the Mac window alone.
    pub scale: Option<f64>,
    pub local_cursor: Option<bool>,
    /// The rate the device wants; `None` means 60.
    pub fps: Option<i64>,
}

/// Kind 12 (`ClientStats`): the first four fields required.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientStats {
    pub fps: i64,
    pub frame_age_ms: i64,
    pub rtt_ms: i64,
    pub device: String,
    pub frame_age_max_ms: Option<i64>,
    pub rtt_max_ms: Option<i64>,
    pub audio_behind_ms: Option<i64>,
    pub audio_late: Option<i64>,
}

/// Kind 17 (`HostSettingsChange`): only the fields one control changed, plus a token.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsChange {
    pub token: Option<i64>,
    #[serde(rename = "maxFPS")]
    pub max_fps: Option<i64>,
    pub bitrate: Option<i64>,
    pub capture_scale: Option<f64>,
    pub prioritize_speed: Option<bool>,
    pub virtual_display: Option<bool>,
    pub direct_wireless: Option<bool>,
    pub send_audio: Option<bool>,
}

/// Kind 22 (`Goodbye`): why the host is about to close a session. Nil fields are left
/// out of the JSON, as the Swift host's encoder leaves them out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Goodbye {
    /// "removed", "remoteOff", "internetOff", "quit", "busy", "pairingRequired", "update",
    /// or anything a later host may say.
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconnect: Option<bool>,
}

/// One device on the host's trust list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDevice {
    /// base64url of the device key's pin (SHA-256 of its SubjectPublicKeyInfo).
    pub fingerprint: String,
    /// The name its kind 19 gave.
    pub name: String,
    pub model: Option<String>,
    /// Seconds since 1970.
    pub paired_at: f64,
    /// "qr" or "code".
    pub method: String,
}

/// Something a device did, in the order the host saw it. `conn` numbers the TCP
/// connection (from 1, per host).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum DeviceEvent {
    /// A TLS handshake the door refused or that failed: an unpaired key on `sill/1`, no
    /// client certificate, a key that is not P-256, no protocol in common, not TLS 1.3.
    #[serde(rename_all = "camelCase")]
    HandshakeRefused { conn: u64, alpn: Option<String>, fingerprint: Option<String>, reason: String },
    /// A session was admitted (`sill/1`). `fingerprint`: base64url of the device's pin.
    #[serde(rename_all = "camelCase")]
    SessionStarted { conn: u64, fingerprint: String, paired: bool },
    /// A session ended (either side closed it).
    #[serde(rename_all = "camelCase")]
    SessionEnded { conn: u64 },
    /// A session the host closed before serving it, with this goodbye: the device gate's
    /// "update", or the remote door's "remoteOff".
    #[serde(rename_all = "camelCase")]
    SessionRefused { conn: u64, reason: String },
    /// Kind 19 on a pairing connection, and the reason of the host's answer ("ok", "code",
    /// "busy", "closed", "expired", "stopped", "shown", "openOnMac", "locked").
    #[serde(rename_all = "camelCase")]
    PairRequest {
        conn: u64,
        fingerprint: String,
        v: i64,
        method: String,
        name: String,
        model: Option<String>,
        cable: Option<bool>,
        answer: String,
        tries_left: Option<u32>,
    },
    /// A pairing connection that ended without a decodable kind 19.
    #[serde(rename_all = "camelCase")]
    PairingAbandoned { conn: u64, reason: String },
    /// Kind 23.
    #[serde(rename_all = "camelCase")]
    Hello { conn: u64, hello: Hello },
    /// Kind 6.
    #[serde(rename_all = "camelCase")]
    SelectSource { conn: u64, source: Source },
    /// Kind 7.
    #[serde(rename_all = "camelCase")]
    LaunchApp { conn: u64, bundle_id: String },
    /// Kind 8.
    #[serde(rename_all = "camelCase")]
    Input { conn: u64, input: InputEvent },
    /// Kind 9. `refused`: why the host took nothing of it (`ViewportBounds.refusal`).
    #[serde(rename_all = "camelCase")]
    Viewport { conn: u64, viewport: Viewport, refused: Option<String> },
    /// Kind 10: its payload (8 bytes for an ordinary ping, 16 for a Paste's receipt) and
    /// the header's time stamp, both echoed in the pong.
    #[serde(rename_all = "camelCase")]
    Ping { conn: u64, payload: Vec<u8>, timestamp: f64 },
    /// Kind 12.
    #[serde(rename_all = "camelCase")]
    ClientStats { conn: u64, stats: ClientStats },
    /// Kind 15. `action`: "close", "minimize" or "fullScreen".
    #[serde(rename_all = "camelCase")]
    WindowCommand { conn: u64, id: u32, action: String },
    /// Kind 17. `refused`: the fields the host did not take, in its log's words.
    #[serde(rename_all = "camelCase")]
    ChangeSettings { conn: u64, change: SettingsChange, refused: Vec<String> },
    /// Kind 21.
    #[serde(rename_all = "camelCase")]
    PairingWanted { conn: u64 },
    /// Kind 27. Without an `id`: the subscription to the top level.
    #[serde(rename_all = "camelCase")]
    FetchMenu { conn: u64, version: Option<i64>, id: Option<String>, title: Option<String>, token: Option<i64> },
    /// Kind 25, and whether the host "pressed" the item.
    #[serde(rename_all = "camelCase")]
    PressMenuItem {
        conn: u64,
        version: Option<i64>,
        id: Option<String>,
        title: Option<String>,
        token: Option<i64>,
        pressed: bool,
    },
    /// Kind 28.
    #[serde(rename_all = "camelCase")]
    Gesture { conn: u64, gesture: String, fingers: Option<i64> },
    /// Kind 32: a subscription (`{}`), a text for the Mac's clipboard, or a stop.
    #[serde(rename_all = "camelCase")]
    Clipboard { conn: u64, text: Option<String>, stop: bool },
    /// Kind 44. `honoured`: a keyframe followed (false: nothing streams, or more than one a second).
    #[serde(rename_all = "camelCase")]
    KeyframeRequest { conn: u64, honoured: bool },
    /// The host took a message and did nothing with it: the Mac is locked ("pick",
    /// "launch", "windowCommand", "menus", "gesture", "clipboard", "input"), or a rate
    /// limit ("rate"). Follows the event of the message itself.
    #[serde(rename_all = "camelCase")]
    Refused { conn: u64, kind: u8, reason: String },
    /// A known kind whose payload the real host's decoder would drop.
    #[serde(rename_all = "camelCase")]
    Undecodable { conn: u64, kind: u8, len: usize },
    /// A kind this host does not take from a device (skipped, as the real host skips it).
    #[serde(rename_all = "camelCase")]
    Unknown { conn: u64, kind: u8, len: usize },
}

/// The shared, inspectable log of [`DeviceEvent`]s.
#[derive(Default)]
pub(crate) struct EventLog {
    events: Mutex<(Vec<DeviceEvent>, u64)>,
    changed: Condvar,
}

impl EventLog {
    pub fn push(&self, e: DeviceEvent) {
        let mut g = self.events.lock().unwrap();
        g.0.push(e);
        g.1 += 1;
        self.changed.notify_all();
    }

    pub fn snapshot(&self) -> Vec<DeviceEvent> {
        self.events.lock().unwrap().0.clone()
    }

    pub fn clear(&self) {
        self.events.lock().unwrap().0.clear();
    }

    /// The first event `predicate` accepts, waiting for it up to `timeout` on a condition
    /// variable (no polling).
    pub fn wait_for(&self, predicate: impl Fn(&DeviceEvent) -> bool, timeout: Duration) -> Option<DeviceEvent> {
        let deadline = Instant::now() + timeout;
        let mut g = self.events.lock().unwrap();
        let mut seen = 0usize;
        loop {
            // After a clear the log is shorter than what was looked at: start over.
            if seen > g.0.len() {
                seen = 0;
            }
            if let Some(e) = g.0[seen..].iter().find(|e| predicate(e)) {
                return Some(e.clone());
            }
            seen = g.0.len();
            let left = deadline.checked_duration_since(Instant::now())?;
            let (next, result) = self.changed.wait_timeout(g, left).unwrap();
            g = next;
            if result.timed_out() && g.0.len() == seen {
                return None;
            }
        }
    }

    /// Every event pushed after the first `from` (counted since the host started, clears
    /// ignored), waiting up to `timeout` for at least one; with the new count.
    pub fn wait_since(&self, from: u64, timeout: Duration) -> (Vec<DeviceEvent>, u64) {
        let deadline = Instant::now() + timeout;
        let mut g = self.events.lock().unwrap();
        loop {
            if g.1 > from {
                let fresh = ((g.1 - from) as usize).min(g.0.len());
                return (g.0[g.0.len() - fresh..].to_vec(), g.1);
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return (Vec::new(), g.1);
            };
            g = self.changed.wait_timeout(g, left).unwrap().0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sources() {
        for s in [Source::None, Source::Desktop, Source::Window(7)] {
            assert_eq!(Source::from_json(&s.to_json()), Some(s));
        }
        assert_eq!(Source::from_json(&json!({"window": {}})), None);
        assert_eq!(Source::from_json(&json!("desktop")), None);
        assert_eq!(Source::from_json(&json!({"desktop": {}, "none": {}})), None);
    }

    #[test]
    fn inputs() {
        assert_eq!(
            InputEvent::from_json(&json!({"pointer": {"_0": "move", "x": 0.5, "y": 1}})),
            Some(InputEvent::Pointer { action: "move".into(), x: 0.5, y: 1.0 })
        );
        assert_eq!(InputEvent::from_json(&json!({"pointer": {"_0": "hover", "x": 0.5, "y": 1}})), None);
        assert_eq!(
            InputEvent::from_json(&json!({"key": {"hidUsage": 4, "down": true, "modifiers": 1048576}})),
            Some(InputEvent::Key { hid_usage: 4, down: true, modifiers: 1_048_576 })
        );
        assert_eq!(InputEvent::from_json(&json!({"key": {"hidUsage": 4, "down": true}})), None);
        assert_eq!(InputEvent::from_json(&json!({"text": {"_0": "hi"}})), Some(InputEvent::Text { text: "hi".into() }));
        assert_eq!(
            InputEvent::from_json(&json!({"scrollGesture": {"_0": "began", "x": 0.1, "y": 0.2}})),
            Some(InputEvent::ScrollGesture { phase: "began".into(), x: 0.1, y: 0.2 })
        );
    }

    #[test]
    fn log_waits_without_spinning() {
        let log = std::sync::Arc::new(EventLog::default());
        assert!(log.wait_for(|_| true, Duration::from_millis(20)).is_none());
        let l2 = log.clone();
        let t = std::thread::spawn(move || l2.wait_for(|e| matches!(e, DeviceEvent::PairingWanted { conn: 2 }), Duration::from_secs(5)));
        log.push(DeviceEvent::PairingWanted { conn: 1 });
        log.push(DeviceEvent::PairingWanted { conn: 2 });
        assert_eq!(t.join().unwrap(), Some(DeviceEvent::PairingWanted { conn: 2 }));
        let (fresh, n) = log.wait_since(1, Duration::from_millis(10));
        assert_eq!((fresh.len(), n), (1, 2));
        log.clear();
        assert!(log.snapshot().is_empty());
    }
}
