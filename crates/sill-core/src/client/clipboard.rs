//! The clipboard's wire and rules (kind 32), ported from `Sources/StreamProtocol`'s Clipboard.swift
//! and ClipboardPolicy.swift. Pure. The device's rules are here whole, and the host's reading of a
//! device's message (`from_device`, `host_receives`), which the tests hold this device's messages
//! to.

use base64::Engine;
use serde_json::{Map, Value};

/// Kind 32, both ways (Clipboard.swift). Text in this generation (`WindowList.clipboard` 1).
///
/// - Device → host, only to a host whose list says `clipboard` 1 or more: any kind 32 subscribes
///   the connection to the Mac's copies (`{}` is what a device sends while its Share Clipboard is
///   on); `text` also becomes the Mac's clipboard; `stop: true` ends the subscription and wins
///   over the rest.
/// - Host → device, only to a connection that subscribed: `text`, copied on the Mac or sent by
///   another device.
///
/// Every field optional and left out when nil, so `{}` is a subscription; unknown keys are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Clipboard {
    /// At most `MAX_TEXT_BYTES` of UTF-8, never empty when sent.
    pub text: Option<String>,
    /// Reserved for a later generation: base64 on the wire. Never sent here.
    pub image: Option<Vec<u8>>,
    /// Device → host: this device's Share Clipboard went off. Never from a host.
    pub stop: Option<bool>,
}

impl Clipboard {
    /// The most text either way: 256 KB of UTF-8.
    pub const MAX_TEXT_BYTES: usize = 256 * 1024;

    pub fn encode(&self) -> Vec<u8> {
        let mut o = Map::new();
        if let Some(t) = &self.text {
            o.insert("text".into(), Value::String(t.clone()));
        }
        if let Some(i) = &self.image {
            o.insert("image".into(), Value::String(base64::engine::general_purpose::STANDARD.encode(i)));
        }
        if let Some(s) = self.stop {
            o.insert("stop".into(), Value::Bool(s));
        }
        Value::Object(o).to_string().into_bytes()
    }

    /// Swift's `JSONDecoder` of Clipboard: one JSON object in valid UTF-8; `text` a string,
    /// `image` a base64 string, `stop` a boolean, each when present and not null; anything else
    /// `None`.
    pub fn decode(bytes: &[u8]) -> Option<Clipboard> {
        let Value::Object(o) = serde_json::from_slice::<Value>(bytes).ok()? else { return None };
        let field = |k: &str| o.get(k).filter(|v| !v.is_null());
        let text = match field("text") {
            None => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return None,
        };
        let image = match field("image") {
            None => None,
            // Swift's Data(base64Encoded:): padded, nothing but the alphabet.
            Some(Value::String(s)) => Some(base64::engine::general_purpose::STANDARD.decode(s).ok()?),
            Some(_) => return None,
        };
        let stop = match field("stop") {
            None => None,
            Some(Value::Bool(b)) => Some(*b),
            Some(_) => return None,
        };
        Some(Clipboard { text, image, stop })
    }
}

/// Why a host refuses a device's kind 32.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Undecodable,
    TooLong,
    Rate,
}

impl Refusal {
    pub fn wire(self) -> &'static str {
        match self {
            Refusal::Undecodable => "undecodable",
            Refusal::TooLong => "tooLong",
            Refusal::Rate => "rate",
        }
    }
}

/// What a device's kind 32 asks of a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromDevice {
    /// Send this connection the Mac's copies from now on.
    Subscribe,
    /// The same, and this text becomes the Mac's clipboard.
    Set(String),
    /// Send this connection nothing more.
    Stop,
    Refuse(Refusal),
}

/// What a window list makes a device send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subscription {
    Subscribe,
    Stop,
    None,
}

/// What this device sent last, and its clipboard's change count right after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub text: String,
    pub change_count: i64,
}

/// What a Paste does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceSend {
    Send(Vec<u8>),
    /// Longer than the cap: nothing sent, the person told.
    TooLong,
    /// Nothing to send (no text, not connected, the Mac does not take it, the switch off).
    Nothing,
}

pub struct ClipboardPolicy;

impl ClipboardPolicy {
    /// `WindowList.clipboard` from a host that takes and sends text.
    pub const GENERATION: i64 = 1;
    /// The most a kind 32's JSON may be, either way (1 MiB).
    pub const MAX_PAYLOAD: usize = 1 << 20;
    /// A device's subscriptions and texts a second, per connection, that a host takes.
    pub const PER_SECOND: usize = 4;
    /// How often the host looks at its pasteboard while a device subscribes (s).
    pub const LOOK_INTERVAL: f64 = 0.25;
    /// How long the Mac's text stays on this device's clipboard (s).
    pub const DEVICE_EXPIRY: f64 = 600.0;
    /// How long the chip stays at most (s).
    pub const CHIP_LIFETIME: f64 = 20.0;
    /// A session whose first list comes this soon after the app became active still gets that look (s).
    pub const ACTIVATION_WINDOW: f64 = 15.0;
    /// How long "Sent to…", "Pasted on…", "Too long…" or "Didn’t reach…" stays (s).
    pub const NOTE_LIFETIME: f64 = 3.0;
    /// A Paste's receipt: the payload of the ping sent after its text, this many random bytes (a
    /// ping's own is 8).
    pub const RECEIPT_LENGTH: usize = 16;
    /// The key ⌘V presses: V's HID usage.
    pub const PASTE_KEY: u16 = 0x19;
    /// How long after the receipt ⌘V goes (s): the Mac writes the text on a queue of its own.
    pub const PASTE_DELAY: f64 = 0.05;

    /// A kind 32 that sets the other side's clipboard to `text`: its JSON, or `None` when the text
    /// is empty, longer than `MAX_TEXT_BYTES` of UTF-8, or its JSON longer than `MAX_PAYLOAD`.
    pub fn payload(text: &str) -> Option<Vec<u8>> {
        if text.is_empty() || !Self::within_cap(text) {
            return None;
        }
        let data = Clipboard { text: Some(text.to_string()), ..Default::default() }.encode();
        if data.is_empty() || data.len() > Self::MAX_PAYLOAD { None } else { Some(data) }
    }

    /// A device's subscription: `{}`.
    pub fn subscription() -> Vec<u8> {
        Clipboard::default().encode()
    }

    /// A device's switch going off: `{"stop":true}`.
    pub fn stopping() -> Vec<u8> {
        Clipboard { stop: Some(true), ..Default::default() }.encode()
    }

    /// Whether `text` is at most `MAX_TEXT_BYTES` of UTF-8.
    pub fn within_cap(text: &str) -> bool {
        text.len() <= Clipboard::MAX_TEXT_BYTES
    }

    /// `stop` wins over anything with it; text that is not empty sets; anything else subscribes.
    pub fn from_device(payload: &[u8]) -> FromDevice {
        let Some(clip) = Clipboard::decode(payload) else { return FromDevice::Refuse(Refusal::Undecodable) };
        if clip.stop == Some(true) {
            return FromDevice::Stop;
        }
        match clip.text {
            None => FromDevice::Subscribe,
            Some(t) if t.is_empty() => FromDevice::Subscribe,
            Some(t) if !Self::within_cap(&t) => FromDevice::Refuse(Refusal::TooLong),
            Some(t) => FromDevice::Set(t),
        }
    }

    /// Who a device's text goes to: every subscriber but its sender, in the order given.
    pub fn forward<T: PartialEq + Clone>(to: &[T], from: &T) -> Vec<T> {
        to.iter().filter(|t| *t != from).cloned().collect()
    }

    /// A device's kind 32 at `now`, as the host takes it: what it asks, and the arrivals to keep.
    /// A subscription or text at most `PER_SECOND` in any second; a stop always, counting nothing.
    pub fn host_receives(payload: &[u8], arrivals: &[f64], now: f64) -> (FromDevice, Vec<f64>) {
        let ask = Self::from_device(payload);
        if !Self::counts_against_rate(&ask) {
            return (ask, arrivals.to_vec());
        }
        let (taken, kept) = Self::admits(arrivals, now);
        (if taken { ask } else { FromDevice::Refuse(Refusal::Rate) }, kept)
    }

    pub fn counts_against_rate(ask: &FromDevice) -> bool {
        matches!(ask, FromDevice::Subscribe | FromDevice::Set(_))
    }

    /// Whether one more is taken at `now`, at most `PER_SECOND` in any second, and the arrivals to keep.
    pub fn admits(arrivals: &[f64], now: f64) -> (bool, Vec<f64>) {
        let mut recent: Vec<f64> = arrivals.iter().copied().filter(|a| now - a < 1.0).collect();
        if recent.len() >= Self::PER_SECOND {
            return (false, recent);
        }
        recent.push(now);
        (true, recent)
    }

    /// Whether the Mac's list says it takes and sends the clipboard.
    pub fn host_takes(host_clipboard: Option<i64>) -> bool {
        host_clipboard.unwrap_or(0) >= Self::GENERATION
    }

    /// A list's word on the clipboard as this device takes it: a locked Mac shares nothing.
    pub fn host_clipboard(clipboard: Option<i64>, locked: Option<bool>) -> Option<i64> {
        if locked == Some(true) { None } else { clipboard }
    }

    /// Subscribe while the Mac takes the clipboard, the switch is on and this connection has not;
    /// stop when it has and the switch went off. A Mac that stopped saying it forgot every
    /// subscription. What to send, and whether the connection is subscribed after it.
    pub fn device_subscription(host_clipboard: Option<i64>, switch_on: bool, subscribed: bool) -> (Subscription, bool) {
        if !Self::host_takes(host_clipboard) {
            return (Subscription::None, false);
        }
        if switch_on && !subscribed {
            return (Subscription::Subscribe, true);
        }
        if !switch_on && subscribed {
            return (Subscription::Stop, false);
        }
        (Subscription::None, subscribed)
    }

    /// A kind 32 from the Mac: the text the device writes to its clipboard, or `None`. Only with
    /// the switch on, from a Mac that takes the clipboard, text not empty and within the cap, and
    /// never the text this device sent while its clipboard still holds it (the Mac's echo).
    pub fn device_writes(payload: &[u8], switch_on: bool, host_clipboard: Option<i64>, last_sent: Option<&Sent>, change_count: i64) -> Option<String> {
        if !switch_on || !Self::host_takes(host_clipboard) {
            return None;
        }
        let text = Clipboard::decode(payload)?.text?;
        if text.is_empty() || !Self::within_cap(&text) {
            return None;
        }
        if let Some(sent) = last_sent {
            if sent.text == text && sent.change_count == change_count {
                return None;
            }
        }
        Some(text)
    }

    /// The chip, "Paste on ‹Mac›": connected to a Mac that takes the clipboard, the switch on, the
    /// device's clipboard has text, and its change is not the one Sill last handled.
    pub fn offers_chip(connected: bool, host_clipboard: Option<i64>, switch_on: bool, has_strings: bool, change_count: i64, handled: Option<i64>) -> bool {
        connected && Self::host_takes(host_clipboard) && switch_on && has_strings && Some(change_count) != handled
    }

    /// The chip's `CHIP_LIFETIME` runs only while it is on screen, the app is in front and no
    /// screen reader is on.
    pub fn chip_counts(on_screen: bool, active: bool, voice_over: bool) -> bool {
        on_screen && active && !voice_over
    }

    /// A session's first list that says the Mac takes the clipboard: whether the app's activation
    /// owes it the chip's look.
    pub fn looks_at_session_start(activated_at: Option<f64>, now: f64) -> bool {
        matches!(activated_at, Some(at) if now >= at && now - at <= Self::ACTIVATION_WINDOW)
    }

    /// What a Paste read (one string per item): the first that is not empty goes, to a connected
    /// Mac that takes it, with the switch on.
    pub fn device_sends(strings: &[String], connected: bool, host_clipboard: Option<i64>, switch_on: bool) -> DeviceSend {
        if !connected || !Self::host_takes(host_clipboard) || !switch_on {
            return DeviceSend::Nothing;
        }
        let Some(text) = strings.iter().find(|s| !s.is_empty()) else { return DeviceSend::Nothing };
        match Self::payload(text) {
            Some(data) => DeviceSend::Send(data),
            None => DeviceSend::TooLong,
        }
    }

    /// How long a Paste waits for its receipt (s): four times the worst recent round trip, at
    /// least 4 s.
    pub fn receipt_wait(worst_rtt_ms: Option<i32>) -> f64 {
        4f64.max(4.0 * f64::from(worst_rtt_ms.unwrap_or(0)) / 1000.0)
    }

    /// Paste right away: a receipt also presses ⌘V on the Mac only with the switch on, for a
    /// receipt in time, while something streams, while the Mac's last list still shares its
    /// clipboard, never while a tour keeps input from the Mac.
    pub fn pastes_on_receipt(right_away: bool, late: bool, streaming: bool, host_clipboard: Option<i64>, tour_shows: bool) -> bool {
        right_away && !late && streaming && Self::host_takes(host_clipboard) && !tour_shows
    }

    /// The note a receipt brings, written and spoken.
    pub fn receipt_note(pasted: bool, mac: &str) -> (String, String) {
        if pasted { (copy::pasted(mac), copy::pasted(mac)) } else { (copy::sent(mac), copy::sent_spoken(mac)) }
    }

    /// The Mac's text, written at `written_at_ms` (wall clock), is cleared from this device's
    /// clipboard at a look `DEVICE_EXPIRY` later, if still there.
    pub fn own_text_expired(written_at_ms: i64, now_ms: i64) -> bool {
        now_ms - written_at_ms >= (Self::DEVICE_EXPIRY * 1000.0) as i64
    }
}

/// The clipboard's words.
pub mod copy {
    use super::Clipboard;

    /// "256 KB".
    pub fn cap() -> String {
        format!("{} KB", Clipboard::MAX_TEXT_BYTES / 1024)
    }
    pub fn chip_title(mac: &str) -> String {
        format!("Paste on {mac}")
    }
    pub const CHIP_SUBTITLE: &str = "Then press ⌘V there.";
    pub fn chip_subtitle(right_away: bool) -> &'static str {
        if right_away { "Where the cursor is." } else { CHIP_SUBTITLE }
    }
    pub fn paste_hint(mac: &str, device: &str, right_away: bool) -> String {
        if right_away {
            format!("Pastes the text you copied on this {device} where {mac}’s cursor is.")
        } else {
            format!("Sends the text you copied on this {device} to {mac}’s clipboard.")
        }
    }
    pub fn sent(mac: &str) -> String {
        format!("Sent to {mac}’s clipboard. Press ⌘V there.")
    }
    pub fn sent_spoken(mac: &str) -> String {
        format!("Sent to {mac}’s clipboard. Press Command-V there.")
    }
    pub fn too_long(mac: &str) -> String {
        format!("Too long to send: {mac} takes up to {} of text.", cap())
    }
    pub fn not_sent(mac: &str) -> String {
        format!("Didn’t reach {mac}. Try Paste again.")
    }
    pub fn pasted(mac: &str) -> String {
        format!("Pasted on {mac}.")
    }
    pub const SWITCH_TITLE: &str = "Share Clipboard";
    pub fn row_title(mac: &str) -> String {
        format!("Paste on {mac}")
    }
    pub fn right_away_title(mac: &str) -> String {
        format!("Paste on {mac} Right Away")
    }
    pub fn footnote(on: bool, right_away: bool, mac: &str, device: &str) -> String {
        if !on {
            format!("Nothing you copy goes between this {device} and {mac} while this is off.")
        } else if right_away {
            format!("Text you copy on {mac} is ready to paste on this {device}. To paste on {mac}, copy text here and tap Paste: it goes where {mac}’s cursor is. With Paste on {mac} Right Away off, Paste only puts it on {mac}’s clipboard.")
        } else {
            format!("Text you copy on {mac} is ready to paste on this {device}. To paste on {mac}, copy text here, tap Paste, then press ⌘V there.")
        }
    }
}
