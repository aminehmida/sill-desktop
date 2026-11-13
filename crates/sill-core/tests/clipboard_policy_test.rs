//! The clipboard's wire and rules (Tests/checks/clipboard of Sill v0.6.1), ported from
//! ClipboardPolicyTest.kt. Left out with the code they test: the chip's corner and its spoken
//! words (Android's UI), and Java's UTF-16 length counting (Rust strings are UTF-8).

#![allow(clippy::assertions_on_constants)]

use serde_json::Value;
use sill_core::client::clipboard::{copy, Clipboard, ClipboardPolicy as P, DeviceSend, FromDevice, Refusal, Sent, Subscription};
use sill_core::protocol::{self, payloads, WindowList};

fn text(of: Option<Vec<u8>>) -> Option<String> {
    of.and_then(|d| Clipboard::decode(&d)).and_then(|c| c.text)
}
fn at_cap() -> String {
    "a".repeat(Clipboard::MAX_TEXT_BYTES)
}
fn clip(text: &str) -> Clipboard {
    Clipboard { text: Some(text.to_string()), ..Default::default() }
}
fn s(b: Vec<u8>) -> String {
    String::from_utf8(b).unwrap()
}

// MARK: the window list and Clipboard's JSON

#[test]
fn window_list_clipboard() {
    let list = |j: &str| WindowList::parse(j.as_bytes()).unwrap();
    assert_eq!(Some(1), list(r#"{"macName":"M","windows":[],"active":{"desktop":{}},"clipboard":1}"#).clipboard);
    assert_eq!(None, list(r#"{"macName":"M","windows":[],"active":{"desktop":{}},"gestures":1}"#).clipboard);
    assert_eq!(Some(2), list(r#"{"macName":"M","windows":[],"active":{"none":{}},"clipboard":2}"#).clipboard);
    assert_eq!(1, P::GENERATION);
    // A locked Mac's list carries no clipboard; one that did would still share nothing.
    assert_eq!(None, P::host_clipboard(Some(1), Some(true)));
    assert_eq!(Some(1), P::host_clipboard(Some(1), Some(false)));
    assert_eq!(Some(1), P::host_clipboard(Some(1), None));
    assert_eq!(None, P::host_clipboard(None, None));
}

#[test]
fn clipboard_json() {
    assert_eq!("{}", s(Clipboard::default().encode()));
    assert_eq!("{}", s(P::subscription()));
    assert_eq!(r#"{"stop":true}"#, s(P::stopping()));
    assert_eq!(r#"{"text":"hi"}"#, s(clip("hi").encode()));
    let with_image: Value = serde_json::from_slice(&Clipboard { text: Some("a".into()), image: Some(vec![1, 2, 3]), stop: None }.encode()).unwrap();
    assert_eq!(("AQID", "a"), (with_image["image"].as_str().unwrap(), with_image["text"].as_str().unwrap()));
    assert_eq!(
        Some(Clipboard { text: Some("a".into()), image: Some(vec![0x89, 0x50, 0x4E, 0x47]), stop: None }),
        Clipboard::decode(br#"{"text":"a","image":"iVBORw==","later":[1]}"#)
    );
    assert_eq!(Some(Clipboard::default()), Clipboard::decode(b"{}"));
    assert_eq!(Some(Clipboard::default()), Clipboard::decode(br#"{"text":null,"stop":null}"#));
    assert_eq!(None, Clipboard::decode(br#"{"text":5}"#), "a number as the text");
    assert_eq!(None, Clipboard::decode(br#"{"stop":"yes"}"#), "a string as stop");
    assert_eq!(None, Clipboard::decode(br#"{"image":"iVBORw"}"#), "an image not base64");
    assert_eq!(None, Clipboard::decode(b"[]"), "not an object");
    assert_eq!(None, Clipboard::decode(b""), "nothing");
    assert_eq!(None, Clipboard::decode(b"not json"), "not JSON");
    assert_eq!(None, Clipboard::decode(br#"{"text":"a"} x"#), "something after the object");
    assert_eq!(None, Clipboard::decode(&[b'{', b'"', 0xFF, b'"', b'}']), "not UTF-8");
    assert_eq!(262_144, Clipboard::MAX_TEXT_BYTES);
    assert_eq!(1_048_576, P::MAX_PAYLOAD);
    assert_eq!(protocol::MAX_CLIENT_PAYLOAD, P::MAX_PAYLOAD);
    assert!(P::MAX_PAYLOAD <= protocol::MAX_OTHER_HOST_PAYLOAD);
}

// MARK: payload(text:)

#[test]
fn payload() {
    assert_eq!(None, P::payload(""), "empty text: nothing to send");
    assert_eq!(Some("Hello, Mac".to_string()), text(P::payload("Hello, Mac")));
    let odd = "line 1\nline\t2 \u{202e} “é” 😀 </script> \\ \"";
    assert_eq!(Some(odd.to_string()), text(P::payload(odd)), "newlines, tabs, bidi, emoji, slashes and quotes kept");
    assert_eq!(Some(Clipboard::MAX_TEXT_BYTES + 11), P::payload(&at_cap()).map(|p| p.len()), "exactly 256 KB goes (11 bytes of JSON around it)");
    assert_eq!(None, P::payload(&(at_cap() + "a")), "one byte more does not");
    let euros = "€".repeat(Clipboard::MAX_TEXT_BYTES / 3);
    assert!(P::payload(&euros).is_some());
    assert!(P::payload(&(euros + "€")).is_none(), "counted in UTF-8 bytes (€ is 3)");
    let emoji = "😀".repeat(Clipboard::MAX_TEXT_BYTES / 4);
    assert!(P::payload(&emoji).is_some());
    assert!(P::payload(&(emoji + "😀")).is_none(), "😀 is 4 bytes");
    // A control character is six bytes of JSON: within 256 KB of text, over 1 MiB of JSON.
    let controls = "\u{1}".repeat(200_000);
    assert!(clip(&controls).encode().len() > P::MAX_PAYLOAD);
    assert_eq!(None, P::payload(&controls), "text within the cap whose JSON is over 1 MiB is not sent");
    let fits = "\u{1}".repeat((P::MAX_PAYLOAD - 11) / 6);
    assert_eq!(Some(fits.clone()), text(P::payload(&fits)), "its largest that fits goes");
    let one_over = "\u{1}".repeat((P::MAX_PAYLOAD - 11) / 6 + 1);
    assert!(P::within_cap(&one_over));
    assert_eq!(None, P::payload(&one_over), "one control character more does not");
}

// MARK: the host's reading of a device's message

#[test]
fn from_device() {
    assert_eq!(FromDevice::Subscribe, P::from_device(b"{}"));
    assert_eq!(FromDevice::Set("abc".into()), P::from_device(br#"{"text":"abc"}"#));
    assert_eq!(FromDevice::Subscribe, P::from_device(br#"{"text":""}"#), "empty text: subscribes");
    assert_eq!(FromDevice::Stop, P::from_device(br#"{"stop":true}"#));
    assert_eq!(FromDevice::Stop, P::from_device(br#"{"stop":true,"text":"abc"}"#), "stop wins over text");
    assert_eq!(FromDevice::Subscribe, P::from_device(br#"{"stop":false}"#), "stop false is no stop");
    assert_eq!(FromDevice::Subscribe, P::from_device(br#"{"image":"AQID"}"#), "an image alone subscribes");
    assert_eq!(FromDevice::Set("t".into()), P::from_device(br#"{"text":"t","image":"AQID"}"#));
    assert_eq!(FromDevice::Set(at_cap()), P::from_device(&clip(&at_cap()).encode()));
    assert_eq!(FromDevice::Refuse(Refusal::TooLong), P::from_device(&clip(&(at_cap() + "a")).encode()));
    for junk in [&b"not json"[..], br#"{"text":7}"#, b""] {
        assert_eq!(FromDevice::Refuse(Refusal::Undecodable), P::from_device(junk));
    }
    assert_eq!(FromDevice::Set("x".into()), P::from_device(br#"{"text":"x","later":{"a":1}}"#), "a later field is skipped");
    // What this device sends reads as meant.
    assert_eq!(FromDevice::Subscribe, P::from_device(&P::subscription()));
    assert_eq!(FromDevice::Stop, P::from_device(&P::stopping()));
    assert_eq!(FromDevice::Set("pasted".into()), P::from_device(&P::payload("pasted").unwrap()));
    assert_eq!(["rate", "tooLong", "undecodable"], [Refusal::Rate.wire(), Refusal::TooLong.wire(), Refusal::Undecodable.wire()]);
}

fn receives(messages: &[(f64, &str)]) -> Vec<FromDevice> {
    let mut kept: Vec<f64> = Vec::new();
    messages
        .iter()
        .map(|(t, json)| {
            let (ask, k) = P::host_receives(json.as_bytes(), &kept, *t);
            kept = k;
            ask
        })
        .collect()
}

#[test]
fn forward_and_rate() {
    assert_eq!(vec![1, 3], P::forward(&[1, 2, 3], &2));
    assert_eq!(vec![1, 2, 3], P::forward(&[1, 2, 3], &4));
    assert!(P::forward(&[] as &[i32], &1).is_empty() && P::forward(&[1], &1).is_empty());
    assert_eq!(vec![3, 2], P::forward(&[3, 1, 2], &1));
    let mut arrivals: Vec<f64> = Vec::new();
    let mut taken = Vec::new();
    for t in [0.0, 0.1, 0.2, 0.3, 0.4, 0.99, 1.05, 1.11] {
        let (ok, kept) = P::admits(&arrivals, t);
        taken.push(ok);
        arrivals = kept;
    }
    assert_eq!(vec![true, true, true, true, false, false, true, true], taken);
    assert_eq!((true, vec![0.5, 0.9, 0.95, 1.0]), P::admits(&[0.0, 0.5, 0.9, 0.95], 1.0));
    assert_eq!((false, vec![0.2, 0.5, 0.9, 0.95]), P::admits(&[0.2, 0.5, 0.9, 0.95], 1.0));
    assert_eq!(4, P::PER_SECOND);
    let stop = r#"{"stop":true}"#;
    use FromDevice::{Stop, Subscribe};
    assert_eq!(
        vec![Subscribe, Stop, Subscribe, Stop, Subscribe, Stop],
        receives(&[(0.0, "{}"), (2.0, stop), (2.1, "{}"), (2.2, stop), (2.3, "{}"), (2.4, stop)]),
        "every stop taken"
    );
    assert_eq!(Some(&Stop), receives(&[(3.0, "{}"), (3.1, "{}"), (3.2, "{}"), (3.3, "{}"), (3.4, stop)]).last());
    assert_eq!(
        Some(&FromDevice::Refuse(Refusal::Rate)),
        receives(&[(3.0, "{}"), (3.1, "{}"), (3.2, "{}"), (3.3, "{}"), (3.4, "{}")]).last()
    );
    let texts: Vec<String> = ["a", "b", "c", "d", "e"].iter().map(|t| format!(r#"{{"text":"{t}"}}"#)).collect();
    let timed: Vec<(f64, &str)> = texts.iter().enumerate().map(|(i, t)| (i as f64 * 0.1, t.as_str())).collect();
    let set = |t: &str| FromDevice::Set(t.to_string());
    assert_eq!(vec![set("a"), set("b"), set("c"), set("d"), FromDevice::Refuse(Refusal::Rate)], receives(&timed));
    let mut m: Vec<(f64, &str)> = (0..5).map(|i| (f64::from(i) * 0.1, stop)).collect();
    m.extend((0..4).map(|i| (0.5 + f64::from(i) * 0.1, "{}")));
    assert_eq!(vec![Subscribe; 4], receives(&m)[5..], "stops count nothing");
    let mut m: Vec<(f64, &str)> = (0..4).map(|i| (f64::from(i) * 0.1, "junk")).collect();
    m.push((0.4, "{}"));
    assert_eq!(Some(&Subscribe), receives(&m).last(), "refusals count nothing");
    let six: Vec<(f64, &str)> = (0..6).map(|i| (f64::from(i) * 0.1, "{}")).collect();
    assert_eq!(vec![FromDevice::Refuse(Refusal::Rate); 2], receives(&six)[4..]);
    let mut m: Vec<(f64, &str)> = (0..4).map(|i| (f64::from(i) * 0.1, "{}")).collect();
    m.push((1.05, "{}"));
    assert_eq!(Some(&Subscribe), receives(&m).last());
}

// MARK: the device

#[test]
fn subscription() {
    assert!(!P::host_takes(None) && !P::host_takes(Some(0)) && P::host_takes(Some(1)) && P::host_takes(Some(2)));
    for host in [None, Some(0), Some(1), Some(2)] {
        for on in [false, true] {
            for subscribed in [false, true] {
                let takes = host.unwrap_or(0) >= 1;
                let want = if !takes {
                    (Subscription::None, false)
                } else if on && !subscribed {
                    (Subscription::Subscribe, true)
                } else if !on && subscribed {
                    (Subscription::Stop, false)
                } else {
                    (Subscription::None, subscribed)
                };
                assert_eq!(want, P::device_subscription(host, on, subscribed), "host {host:?}, switch {on}, subscribed {subscribed}");
            }
        }
    }
}

#[test]
fn mac_text() {
    let from_mac = clip("from the Mac").encode();
    assert_eq!(Some("from the Mac".to_string()), P::device_writes(&from_mac, true, Some(1), None, 3));
    assert_eq!(None, P::device_writes(&from_mac, false, Some(1), None, 3), "the switch off");
    assert_eq!(None, P::device_writes(&from_mac, true, None, None, 3), "a Mac that does not say it");
    assert_eq!(None, P::device_writes(br#"{"text":""}"#, true, Some(1), None, 3), "empty");
    assert_eq!(None, P::device_writes(br#"{"stop":true}"#, true, Some(1), None, 3), "no text");
    assert_eq!(None, P::device_writes(b"garbage", true, Some(1), None, 3), "undecodable");
    assert_eq!(None, P::device_writes(&clip(&(at_cap() + "a")).encode(), true, Some(1), None, 3), "over the cap");
    assert_eq!(Some(at_cap()), P::device_writes(&clip(&at_cap()).encode(), true, Some(1), None, 3));
    let sent = Sent { text: "mine".into(), change_count: 41 };
    let echo = clip("mine").encode();
    assert_eq!(None, P::device_writes(&echo, true, Some(1), Some(&sent), 41), "the Mac's echo of what the clipboard holds");
    assert_eq!(Some("mine".to_string()), P::device_writes(&echo, true, Some(1), Some(&sent), 42), "the same text after the clipboard changed");
    assert_eq!(Some("mine".to_string()), P::device_writes(&echo, true, Some(1), Some(&sent), -1), "the clipboard unreadable (-1): written");
    assert_eq!(Some("from the Mac".to_string()), P::device_writes(&from_mac, true, Some(1), Some(&sent), 41), "other text");
}

#[test]
fn chip() {
    for connected in [false, true] {
        for host in [None, Some(1)] {
            for on in [false, true] {
                for has in [false, true] {
                    for handled in [None, Some(5), Some(6)] {
                        assert_eq!(
                            connected && host.is_some() && on && has && handled != Some(6),
                            P::offers_chip(connected, host, on, has, 6, handled),
                            "connected {connected}, host {host:?}, switch {on}, text {has}, handled {handled:?}"
                        );
                    }
                }
            }
        }
    }
    for on_screen in [false, true] {
        for active in [false, true] {
            for reader in [false, true] {
                assert_eq!(on_screen && active && !reader, P::chip_counts(on_screen, active, reader));
            }
        }
    }
    assert!(!P::looks_at_session_start(None, 100.0));
    assert!(P::looks_at_session_start(Some(100.0), 100.0) && P::looks_at_session_start(Some(100.0), 115.0));
    assert!(!P::looks_at_session_start(Some(100.0), 115.01));
    assert!(!P::looks_at_session_start(Some(100.0), 99.9), "a clock that went back");
    assert!(P::ACTIVATION_WINDOW == 15.0 && P::CHIP_LIFETIME == 20.0 && P::NOTE_LIFETIME == 3.0 && P::DEVICE_EXPIRY == 600.0 && P::LOOK_INTERVAL == 0.25);
    assert!(!P::own_text_expired(1_000_000, 1_000_000 + 599_999));
    assert!(P::own_text_expired(1_000_000, 1_000_000 + 600_000));
}

#[test]
fn paste() {
    let strings = |v: &[&str]| -> Vec<String> { v.iter().map(|s| s.to_string()).collect() };
    assert_eq!(DeviceSend::Send(P::payload("abc").unwrap()), P::device_sends(&strings(&["abc"]), true, Some(1), true));
    assert_eq!(DeviceSend::Send(P::payload("b").unwrap()), P::device_sends(&strings(&["", "b", "c"]), true, Some(1), true), "the first that is not empty");
    assert_eq!(DeviceSend::Nothing, P::device_sends(&[], true, Some(1), true));
    assert_eq!(DeviceSend::Nothing, P::device_sends(&strings(&[""]), true, Some(1), true));
    assert_eq!(DeviceSend::TooLong, P::device_sends(&[at_cap() + "a"], true, Some(1), true));
    assert_eq!(DeviceSend::TooLong, P::device_sends(&["\u{1}".repeat(200_000)], true, Some(1), true), "JSON over 1 MiB");
    assert_eq!(DeviceSend::Nothing, P::device_sends(&strings(&["abc"]), false, Some(1), true));
    assert_eq!(DeviceSend::Nothing, P::device_sends(&strings(&["abc"]), true, None, true));
    assert_eq!(DeviceSend::Nothing, P::device_sends(&strings(&["abc"]), true, Some(1), false));
    assert!(P::RECEIPT_LENGTH == 16 && P::RECEIPT_LENGTH != 8);
    assert!(P::receipt_wait(None) == 4.0 && P::receipt_wait(Some(0)) == 4.0 && P::receipt_wait(Some(1000)) == 4.0 && P::receipt_wait(Some(-1)) == 4.0);
    assert!(P::receipt_wait(Some(2500)) == 10.0 && P::receipt_wait(Some(1001)) > 4.0);
    assert_eq!("Didn’t reach Mac mini. Try Paste again.", copy::not_sent("Mac mini"));
    for right_away in [false, true] {
        for late in [false, true] {
            for streaming in [false, true] {
                for host in [None, Some(0), Some(1), Some(2)] {
                    for tour in [false, true] {
                        assert_eq!(
                            right_away && !late && streaming && host.unwrap_or(0) >= 1 && !tour,
                            P::pastes_on_receipt(right_away, late, streaming, host, tour)
                        );
                    }
                }
            }
        }
    }
    assert_eq!(0x19, P::PASTE_KEY);
    assert!(P::PASTE_DELAY > 0.0 && P::PASTE_DELAY <= 0.1);
    assert_eq!(("Pasted on Mac mini.".to_string(), "Pasted on Mac mini.".to_string()), P::receipt_note(true, "Mac mini"));
    assert_eq!((copy::sent("Mac mini"), copy::sent_spoken("Mac mini")), P::receipt_note(false, "Mac mini"));
}

// MARK: words

#[test]
fn words() {
    assert_eq!("256 KB", copy::cap());
    assert_eq!("Paste on Mac mini", copy::chip_title("Mac mini"));
    assert_eq!("Then press ⌘V there.", copy::CHIP_SUBTITLE);
    assert_eq!("Sends the text you copied on this phone to Mac mini’s clipboard.", copy::paste_hint("Mac mini", "phone", false));
    assert_eq!("Sent to Mac mini’s clipboard. Press ⌘V there.", copy::sent("Mac mini"));
    assert_eq!("Sent to Mac mini’s clipboard. Press Command-V there.", copy::sent_spoken("Mac mini"));
    assert_eq!("Too long to send: Mac mini takes up to 256 KB of text.", copy::too_long("Mac mini"));
    assert!(copy::SWITCH_TITLE == "Share Clipboard" && copy::row_title("Mac mini") == "Paste on Mac mini");
    assert_eq!(
        "Text you copy on Mac mini is ready to paste on this tablet. To paste on Mac mini, copy text here, tap Paste, then press ⌘V there.",
        copy::footnote(true, false, "Mac mini", "tablet")
    );
    assert_eq!("Nothing you copy goes between this phone and Mac mini while this is off.", copy::footnote(false, false, "Mac mini", "phone"));
    assert_eq!("Where the cursor is.", copy::chip_subtitle(true));
    assert_eq!(copy::CHIP_SUBTITLE, copy::chip_subtitle(false));
    assert_eq!("Pastes the text you copied on this phone where Mac mini’s cursor is.", copy::paste_hint("Mac mini", "phone", true));
    assert_eq!("Pasted on Mac mini.", copy::pasted("Mac mini"));
    assert_eq!("Paste on Mac mini Right Away", copy::right_away_title("Mac mini"));
    assert_eq!(
        "Text you copy on Mac mini is ready to paste on this tablet. To paste on Mac mini, copy text here and tap Paste: it goes where Mac mini’s cursor is. With Paste on Mac mini Right Away off, Paste only puts it on Mac mini’s clipboard.",
        copy::footnote(true, true, "Mac mini", "tablet")
    );
    assert_eq!(copy::footnote(false, false, "Mac mini", "tablet"), copy::footnote(false, true, "Mac mini", "tablet"));
}

// MARK: models

/// A small deterministic generator (xorshift64*), so the models run the same everywhere.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn bool(&mut self) -> bool {
        self.next() >> 63 == 1
    }
    fn below(&mut self, n: u64) -> u64 {
        (self.next() >> 11) % n
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Model 1: the Mac's setting and the device's switch flipped at random; the device follows
/// `device_subscription`, the host takes each message by `from_device` and forgets every
/// subscription when its setting goes off. After every step the two agree, the device is
/// subscribed exactly while both are on, and nothing went to a host that did not take it.
#[test]
fn subscription_model() {
    let mut rng = Rng(32);
    let mut failures = 0;
    'runs: for _ in 0..5_000 {
        let (mut host_on, mut switch_on) = (rng.bool(), rng.bool());
        let (mut device_subscribed, mut host_subscribed) = (false, false);
        for _ in 0..1 + rng.below(30) {
            if rng.bool() {
                host_on = !host_on;
                if !host_on {
                    host_subscribed = false;
                }
            } else {
                switch_on = !switch_on;
            }
            let (send, after) = P::device_subscription(host_on.then_some(P::GENERATION), switch_on, device_subscribed);
            if send != Subscription::None {
                if !host_on {
                    failures += 1;
                    continue 'runs;
                }
                match P::from_device(&if send == Subscription::Subscribe { P::subscription() } else { P::stopping() }) {
                    FromDevice::Subscribe | FromDevice::Set(_) => host_subscribed = true,
                    FromDevice::Stop => host_subscribed = false,
                    FromDevice::Refuse(_) => failures += 1,
                }
            }
            device_subscribed = after;
            if device_subscribed != host_subscribed || device_subscribed != (host_on && switch_on) {
                failures += 1;
                continue 'runs;
            }
        }
    }
    assert_eq!(0, failures, "the device's subscription and the host's agree");
}

/// Model 1b: the same, quickly, with a Paste now and then, each message taken by the host's rate.
/// The host never has the device subscribed while its switch is off: a stop is never lost to the
/// rate.
#[test]
fn quick_subscription_model() {
    let mut rng = Rng(64);
    let (mut failures, mut refused) = (0, 0);
    'runs: for _ in 0..5_000 {
        let (mut host_on, mut switch_on) = (true, rng.bool());
        let (mut device_subscribed, mut host_subscribed) = (false, false);
        let mut arrivals: Vec<f64> = Vec::new();
        let mut now = 0.0;
        for _ in 0..1 + rng.below(40) {
            now += rng.unit() * 0.4;
            let mut payload: Option<Vec<u8>> = None;
            match rng.below(8) {
                0 => {
                    host_on = !host_on;
                    if !host_on {
                        host_subscribed = false;
                        arrivals.clear();
                    }
                }
                1 => {
                    if let DeviceSend::Send(p) = P::device_sends(&["pasted".to_string()], true, host_on.then_some(P::GENERATION), switch_on) {
                        payload = Some(p);
                    }
                }
                _ => switch_on = !switch_on,
            }
            let (send, after) = P::device_subscription(host_on.then_some(P::GENERATION), switch_on, device_subscribed);
            device_subscribed = after;
            if send != Subscription::None {
                payload = Some(if send == Subscription::Subscribe { P::subscription() } else { P::stopping() });
            }
            if let Some(p) = payload {
                if !host_on {
                    failures += 1;
                    continue 'runs;
                }
                let (ask, kept) = P::host_receives(&p, &arrivals, now);
                arrivals = kept;
                match ask {
                    FromDevice::Subscribe | FromDevice::Set(_) => host_subscribed = true,
                    FromDevice::Stop => host_subscribed = false,
                    FromDevice::Refuse(Refusal::Rate) => {
                        if send == Subscription::Subscribe {
                            refused += 1;
                        }
                    }
                    FromDevice::Refuse(_) => failures += 1,
                }
            }
            if host_subscribed && !(host_on && switch_on) {
                failures += 1;
                continue 'runs;
            }
        }
    }
    assert_eq!(0, failures, "never subscribed at the host while the switch is off");
    assert!(refused > 0, "the model reached the rate");
}

/// The nonce of a receipt never passes for an ordinary ping's payload (8 bytes, the time it went).
#[test]
fn receipt_nonce() {
    assert_eq!(8, payloads::ping(123.456).len());
    assert_ne!(8, P::RECEIPT_LENGTH);
}
