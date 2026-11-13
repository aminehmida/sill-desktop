//! What Sill for Mac 0.6 added to the wire, and the rules ported with it (Sill06Test.kt).

use serde_json::Value;
use sill_core::client::{DeviceIdlePolicy, KeyframeDecision, KeyframeRecovery, SettingsLedger};
use sill_core::protocol::*;

fn v(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

fn list(json: &str) -> WindowList {
    WindowList::parse(json.as_bytes()).unwrap()
}

#[test]
fn hello_says_what_this_device_plays_and_shows() {
    let o = v(&payloads::hello("0.6.1", "7", "Pixel (P)", Some(&["aac-eld".to_string()]), Some(1)));
    assert_eq!("aac-eld", o["audio"][0]);
    assert_eq!(1, o["offConsoleView"]);
    // Without sound, no `audio` key at all: a host then sends no kind 43.
    assert!(v(&payloads::hello("0.6.1", "7", "Pixel (P)", None, None)).get("audio").is_none());
}

#[test]
fn window_list_fields_of_06() {
    let l = list(r#"{"macName":"Mac","windows":[],"active":{"none":{}},"clipboard":1,"keyRepeat":1,"locked":true,"offConsole":true,"offConsoleView":true}"#);
    assert_eq!((Some(1), Some(1)), (l.clipboard, l.key_repeat));
    assert_eq!(Some(LockWhy::OtherUser), LockedMac::why(&l));
    assert!(LockedMac::view(&l));
    let old = list(r#"{"macName":"Mac","windows":[],"active":{"none":{}}}"#);
    assert_eq!((None, None, None, false), (old.clipboard, old.key_repeat, LockedMac::why(&old), LockedMac::view(&old)));
    assert_eq!(Some(LockWhy::ScreenLocked), LockedMac::why(&list(r#"{"macName":"Mac","windows":[],"active":{"none":{}},"locked":true}"#)));
}

#[test]
fn locked_mac_cover() {
    // Another user: covered; the off-console view: not; a locked screen streams its lock screen: no cover.
    assert_eq!(Some(LockShade::Locked(LockWhy::OtherUser)), LockedMac::shade(None, Some(LockWhy::OtherUser), false));
    assert_eq!(None, LockedMac::shade(None, Some(LockWhy::OtherUser), true));
    assert_eq!(None, LockedMac::shade(None, Some(LockWhy::ScreenLocked), false));
    // After a cover, "Unlocking…" until a keyframe.
    let unlocking = LockedMac::shade(Some(LockShade::Locked(LockWhy::OtherUser)), None, false);
    assert_eq!(Some(LockShade::Unlocking), unlocking);
    assert_eq!(Some(LockShade::Unlocking), LockedMac::shade_after_frame(unlocking, false));
    assert_eq!(None, LockedMac::shade_after_frame(unlocking, true));
    assert!(LockedMac::detail(LockShade::Locked(LockWhy::ScreenLocked), Some(true)).unwrap().starts_with("To unlock it from here"));
    assert_eq!("The Mac is locked", LockedMac::title("", LockShade::Locked(LockWhy::ScreenLocked)));
    assert!(LockedMac::covers(Some(LockWhy::OtherUser), false) && !LockedMac::covers(Some(LockWhy::OtherUser), true));
}

#[test]
fn settings_of_06() {
    let st = HostSettingsState::parse(
        r#"{"settings":{"maxFPS":60,"bitrate":15000000,"captureScale":2,"prioritizeSpeed":false,"virtualDisplay":true,"sendAudio":true},
           "persistent":true,"virtualDisplayAvailable":true,"softwareEncoder":false,"audioNote":"Couldn’t capture","sessionPaired":false}"#
            .as_bytes(),
    )
    .unwrap();
    assert_eq!(Some(true), st.settings.send_audio);
    assert_eq!(Some("Couldn’t capture"), st.audio_note.as_deref());
    assert_eq!(Some(false), st.session_paired);
    let off = SettingsChange { send_audio: Some(false), ..Default::default() };
    assert_eq!(v(r#"{"sendAudio":false,"token":3}"#), v(&payloads::change_settings(3, &off)));
    // The ledger sends Send Audio only to a Mac that reported it.
    let mut ledger = SettingsLedger::new();
    ledger.receive(st);
    let (_, out) = ledger.pick(&off, 0).unwrap();
    assert_eq!(Some(false), out.send_audio);
    let mut old = SettingsLedger::new();
    old.receive(
        HostSettingsState::parse(
            br#"{"settings":{"maxFPS":60,"bitrate":15000000,"captureScale":2,"prioritizeSpeed":false,"virtualDisplay":true},"persistent":true,"virtualDisplayAvailable":true,"softwareEncoder":false}"#,
        )
        .unwrap(),
    );
    assert!(old.pick(&SettingsChange { send_audio: Some(true), ..Default::default() }, 0).is_none());
}

#[test]
fn stats_carry_sound_only_once_it_played() {
    assert!(v(&payloads::client_stats(60, 20, 5, "d", 30, 8, None, None)).get("audioBehindMs").is_none());
    let o = v(&payloads::client_stats(60, 20, 5, "d", 30, 8, Some(-1), Some(2)));
    assert_eq!((-1, 2), (o["audioBehindMs"].as_i64().unwrap(), o["audioLate"].as_i64().unwrap()));
    assert_eq!((60, 20, 5, "d", 30, 8), (o["fps"].as_i64().unwrap(), o["frameAgeMs"].as_i64().unwrap(), o["rttMs"].as_i64().unwrap(), o["device"].as_str().unwrap(), o["frameAgeMaxMs"].as_i64().unwrap(), o["rttMaxMs"].as_i64().unwrap()));
}

#[test]
fn viewport_held_to_what_a_host_takes() {
    assert!(payloads::viewport(0.0, 400.0, None, 60, true).is_none());
    assert!(ViewportBounds::held(f64::NAN, 1.0, None).is_none());
    let o = v(&payloads::viewport(1e9, 0.5, Some(10.0), 60, true).unwrap());
    assert_eq!((16384.0, 1.0, 4.0), (o["width"].as_f64().unwrap(), o["height"].as_f64().unwrap(), o["scale"].as_f64().unwrap()));
    assert_eq!((60, true), (o["fps"].as_i64().unwrap(), o["localCursor"].as_bool().unwrap()));
    assert!(v(&payloads::viewport(800.0, 600.0, Some(-1.0), 60, true).unwrap()).get("scale").is_none());
}

#[test]
fn text_scale() {
    assert_eq!(vec![0.25, 0.5, 0.75, 1.0, 1.25, 1.5], TextScale::detents(true));
    assert_eq!(1.0, TextScale::position(None, false));
    assert_eq!(TextScale::OFF, TextScale::position(None, true));
    assert_eq!(None, TextScale::scale_at(TextScale::OFF));
    assert_eq!(("1.25×", "1×", "Off"), (TextScale::title(1.25).as_str(), TextScale::title(1.0).as_str(), TextScale::title(TextScale::OFF).as_str()));
    assert_eq!(0.5, TextScale::snap(0.3, false));
    // A window on the virtual display: no Off; in place with the setting on: Off is back.
    let w = StreamSource::Window(3);
    assert!(!TextScale::offers_off(Some(true), w, Some(true)));
    assert!(TextScale::offers_off(Some(true), w, Some(false)));
    assert!(!TextScale::offers_off(Some(true), StreamSource::Desktop, None));
    assert!(TextScale::selectable(w) && !TextScale::selectable(StreamSource::Desktop));
}

#[test]
fn scroll_direction() {
    assert_eq!(-1.0, ScrollDirection::factor(ScrollSource::Finger, true));
    assert_eq!(-1.0, ScrollDirection::factor(ScrollSource::Pad, true));
    assert_eq!(1.0, ScrollDirection::factor(ScrollSource::Wheel, true));
    assert_eq!(1.0, ScrollDirection::factor(ScrollSource::Trackpad, true));
    assert_eq!(1.0, ScrollDirection::factor(ScrollSource::Finger, false));
}

#[test]
fn keyframe_recovery() {
    let mut r = KeyframeRecovery::new();
    assert_eq!(KeyframeDecision::Nothing, r.consider(0.0, true, true, 4.0));
    r.interrupted(10.0);
    assert_eq!(KeyframeDecision::AskNow, r.consider(10.0, true, true, 4.0));
    // Out and due: no second ask until the wait passes, then a repeat at twice the spacing.
    assert_eq!(KeyframeDecision::KeyframeDue(14.0), r.consider(11.0, true, true, 4.0));
    assert_eq!(KeyframeDecision::AskNow, r.consider(14.5, true, true, 4.0));
    assert_eq!(1, r.repeats());
    r.picture_back(15.0, true);
    // A loss within 10 s of the picture coming back is a repeat: 8 s spacing now.
    r.interrupted(16.0);
    assert_eq!(2, r.repeats());
    assert_eq!(KeyframeDecision::AskAt(14.5 + 8.0), r.consider(16.0, true, true, 4.0));
    // Not on screen: nothing; the trip away starts the spacing again.
    assert_eq!(KeyframeDecision::Nothing, r.consider(17.0, false, true, 4.0));
    r.left_foreground();
    assert_eq!(0, r.repeats());
    assert_eq!(KeyframeDecision::AskNow, r.consider(17.0, true, true, 4.0));
    assert_eq!(6.0, KeyframeRecovery::answer_wait(Some(1500)));
    assert_eq!(4.0, KeyframeRecovery::answer_wait(None));
}

#[test]
fn device_idle() {
    let limit = DeviceIdlePolicy::IDLE_LIMIT_S;
    assert_eq!(Some(700.0), DeviceIdlePolicy::hold_ends(true, true, Some(100.0), 200.0, limit));
    assert_eq!(None, DeviceIdlePolicy::hold_ends(true, true, Some(100.0), 701.0, limit));
    assert_eq!(None, DeviceIdlePolicy::hold_ends(false, true, Some(100.0), 200.0, limit));
    assert_eq!(None, DeviceIdlePolicy::hold_ends(true, false, Some(100.0), 200.0, limit));
    // The automatic reconnect never counts as use.
    assert_eq!(Some(5.0), DeviceIdlePolicy::session_start(Some(5.0), true, 50.0));
    assert_eq!(Some(50.0), DeviceIdlePolicy::session_start(Some(5.0), false, 50.0));
}

#[test]
fn away_copy_and_presets() {
    let link = |suggested: Option<i64>| LinkReport {
        state: "behind".into(),
        withheld_per_second: 9,
        bitrate: 15_000_000,
        carried_kbps: Some(3000),
        suggested_bitrate: suggested,
        suggested_capture_scale: suggested.map(|_| 1.0),
    };
    let c = AwayCopy::callout(Some(&link(Some(4_000_000))), None, "Studio").unwrap();
    assert_eq!("The link can’t carry Balanced. Low · Standard is recommended.", c.text);
    assert_eq!(Some("Use Low · Standard"), c.button.as_deref());
    assert_eq!(Some(4_000_000), c.change.unwrap().bitrate);
    assert_eq!("The link to Studio is too slow for a steady picture, even at Low.", AwayCopy::callout(Some(&link(None)), None, "Studio").unwrap().text);
    assert_eq!(Some("The link to Studio can’t keep up.".to_string()), AwayCopy::stream_line(Some(&link(None)), None, "Studio"));
    let steady = LinkReport { state: "steady".into(), ..link(None) };
    assert!(AwayCopy::callout(Some(&steady), None, "Studio").is_none());
    assert_eq!(("2.5 Mbps", "Pro"), (QualityPreset::name(2_500_000).as_str(), QualityPreset::name(40_000_000).as_str()));
}
