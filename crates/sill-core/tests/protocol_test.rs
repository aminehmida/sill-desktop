//! The Rust side against the vectors `Sources/StreamProtocol` is checked with
//! (Tests/checks/protocol/main.swift, computed in Python), and against the JSON the Mac's Codable
//! types write and read. Ported from the Android client's ProtocolTest.kt; every vector kept.

use serde_json::{json, Value};
use sill_core::client::{Menu, MenuOutcome, MenuState, SettingsField, SettingsLedger};
use sill_core::crypto::*;
use sill_core::net::{DialOrder, Endpoint, SavedMac};
use sill_core::protocol::*;

fn v(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

// MARK: pairing

#[test]
fn pairing_proof_vectors() {
    let fp_mac = sha256(b"mac");
    let fp_dev = sha256(b"device");
    assert_eq!("348a629f5ceed032c3e8706ec47d9bfafb00fb4250b018dd965435ca50cb836e", to_hex(&fp_mac));
    assert_eq!("263a4dbe41488fb87214b0032339dbb9f0c8da14c16dfcf13084bf3c2552eca5", to_hex(&fp_dev));
    let k = pairing_proof::code_key("482913557208", &fp_mac);
    assert_eq!("e9baf879a1cc4ac8b21554df480b6c1621671319107c91e7cb05d154f649d31d", to_hex(&k));
    assert_eq!("dk7PZj4vHsAxj--1u9Vg7ehSn8a1tWc0RCG46YpFVO8", base64url::encode(&pairing_proof::device_proof(&k, &fp_dev, &fp_mac)));
    assert!(pairing_proof::is_valid_mac_proof(
        &base64url::decode("6BmGWC9XT8J8OY8phhq9bASgoSnoWpRxBeqZtvRrWmk").unwrap(),
        &k,
        &fp_mac,
        &fp_dev
    ));
    let qr = [0x22u8; 16];
    assert_eq!("AyZAJqNTU5nSKEaHx6RZOf0K5dOeygR_8JSrCV_gNOo", base64url::encode(&pairing_proof::device_proof(&qr, &fp_dev, &fp_mac)));
    let mac_proof = base64url::decode("XAX-uZPsq_Ax8fvI_sV7Ex0a-ROi19rI-FW-ykPh9TE").unwrap();
    assert!(pairing_proof::is_valid_mac_proof(&mac_proof, &qr, &fp_mac, &fp_dev));
    // Swapped fingerprints never check.
    assert!(!pairing_proof::is_valid_mac_proof(&mac_proof, &qr, &fp_dev, &fp_mac));
    // What a host computes is what a device checks.
    assert_eq!(mac_proof, pairing_proof::mac_proof(&qr, &fp_mac, &fp_dev));
}

#[test]
fn mac_id_vectors() {
    let mut fp = hex("50d858e0985ecc7f6041");
    fp.extend_from_slice(&[0u8; 22]);
    assert_eq!("A3C5HR4RBV67YR21", mac_id::make(&fp));
    assert_eq!("A3C5HR4RBV67YR21", mac_id::make(&sha256(b"example")));
    assert_eq!("0000000000000000", mac_id::make(&[0u8; 32]));
    assert_eq!("ZZZZZZZZZZZZZZZZ", mac_id::make(&[0xFFu8; 32]));
}

#[test]
fn recognition_tag_vector() {
    let key = [0x11u8; 32];
    assert!(recognition_tag::matches("oaKjpKWmL4V47Ctm", &key));
    assert!(!recognition_tag::matches("oaKjpKWmL4V47Ctn", &key));
    assert!(!recognition_tag::matches("oaKjpKWmL4V47Ctm", &[0x12u8; 32]));
    assert!(!recognition_tag::matches("oaKjpKWmL4V47Ct", &key));
    assert!(!recognition_tag::matches("oaKjpKWmL4V47Ctm=", &key));
}

#[test]
fn pairing_code_check() {
    use pairing_code::Check;
    assert_eq!(Check::Ok("482913557208".into()), pairing_code::check("4829 1355 7208"));
    assert_eq!(Check::Ok("482913557208".into()), pairing_code::check("4829-1355-7208"));
    assert_eq!(Check::Typo, pairing_code::check("4829 1355 7209"));
    assert_eq!(Check::Typo, pairing_code::check("4829 1355 7280"));
    assert_eq!(Check::Length, pairing_code::check("48291355720"));
    assert_eq!(Check::Length, pairing_code::check("٤٨٢٩١٣٥٥٧٢٠٨"));
    assert_eq!("4829 1355 7208", pairing_code::grouped("482913557208"));
    assert_eq!(8, pairing_code::check_digit(&[4, 8, 2, 9, 1, 3, 5, 5, 7, 2, 0]));
}

#[test]
fn base64url_is_canonical() {
    assert!(base64url::decode("AyZAJqNTU5nSKEaHx6RZOf0K5dOeygR_8JSrCV_gNOo").is_some());
    assert!(base64url::decode("AyZAJqNTU5nSKEaHx6RZOf0K5dOeygR_8JSrCV_gNOo=").is_none()); // padding
    assert!(base64url::decode("AyZAJqNTU5nSKEaHx6RZOf0K5dOeygR/8JSrCV+gNOo").is_none()); // the standard alphabet
    assert!(base64url::decode("A").is_none());
}

#[test]
fn pair_link_from_a_host() {
    // A synthetic host's link (its key's Mac ID is m).
    let text = "sill://pair?v=1&m=9MAGJQ55FET6EQ97&k=TRUJXKV7tGddJ4fdpWDw5VxaoJJ6CCrGSdJ58SYXv4M&s=XAs4dXaCYc9mS0w-czOAmQ\
        &n=Tom%E2%80%99s%20Mac%20mini%20%282%29&p=47455&a=127.0.0.1&a=100.101.102.103&a=%5Bfd7a:115c:a1e0::1234%5D&a=192.168.1.20";
    let link = PairLink::parse(text).unwrap();
    assert_eq!("9MAGJQ55FET6EQ97", link.mac_id);
    assert_eq!("Tom’s Mac mini (2)", link.name);
    assert_eq!(47455, link.port);
    assert_eq!(vec!["127.0.0.1", "100.101.102.103", "[fd7a:115c:a1e0::1234]", "192.168.1.20"], link.addresses);
    assert_eq!(16, link.secret.len());
    // m must be k's Mac ID.
    assert!(PairLink::parse(&text.replace("m=9MAGJQ55FET6EQ97", "m=9MAGJQ55FET6EQ98")).is_err());
    assert!(PairLink::parse(&text.replace("v=1", "v=2")).is_err());
    assert!(PairLink::parse("https://example.com/?v=1").is_err());
    assert_eq!(Some(Endpoint::new("fd7a:115c:a1e0::1234", 47455)), Endpoint::parse(&link.addresses[2], link.port));
}

#[test]
fn safe_text_labels() {
    assert_eq!("Mac mini", safe_text::label("Mac\u{0}\u{202e} mini", 64));
    assert_eq!("Mac mini", safe_text::label("  Mac\n\t  mini  ", 64));
    assert_eq!("iPad", safe_text::label("\u{202e}i\u{200b}Pad", 64));
    assert_eq!("abc", safe_text::label("abcdef", 3));
    assert_eq!("ab", safe_text::label("ab cd", 3));
}

// MARK: addresses and dial order

#[test]
fn endpoints() {
    assert_eq!(Some(Endpoint::new("mac.tail1234.ts.net", 7455)), Endpoint::parse("mac.tail1234.ts.net", 7455));
    assert_eq!(Some(Endpoint::new("100.101.102.103", 9000)), Endpoint::parse("100.101.102.103:9000", 7455));
    assert_eq!(Some(Endpoint::new("::1", 47455)), Endpoint::parse("[::1]:47455", 7455));
    assert_eq!(Some(Endpoint::new("fd7a:115c:a1e0::1", 7455)), Endpoint::parse("fd7a:115c:a1e0::1", 7455));
    assert_eq!(None, Endpoint::parse("sill://pair?v=1", 7455));
    assert_eq!(None, Endpoint::parse("a b", 7455));
    assert!(Endpoint::is_tailscale("100.64.0.1") && Endpoint::is_tailscale("100.127.255.255") && !Endpoint::is_tailscale("100.128.0.1"));
    assert!(Endpoint::is_tailscale("mac.tail1234.ts.net") && Endpoint::is_tailscale("fd7a:115c:a1e0::1234"));
    assert_eq!("[::1]:47455", Endpoint::new("::1", 47455).to_string());
}

#[test]
fn dial_order_puts_tailscale_first_and_last_worked_first_of_all() {
    let addr = |host: &str, kind: &str, via: &str| MacAddress { host: host.into(), port: None, kind: kind.into(), via: via.into() };
    let mac = SavedMac {
        mac_id: mac_id::make(&[0u8; 32]),
        fingerprint: vec![0u8; 32],
        name: "Mac".into(),
        recognition_key: None,
        remote_port: 7455,
        addresses: vec![
            addr("192.168.1.5", "lan", "Wi‑Fi"),
            addr("100.101.102.103", "vpn", "Tailscale"),
            addr("mac.tail1234.ts.net", "vpn", "Tailscale"),
        ],
        typed_addresses: vec!["100.101.102.103".into()],
        info_issued_at: 0.0,
        last_worked: None,
        paired_at: 0.0,
        last_connected_at: None,
        revoked: false,
    };
    let hosts: Vec<String> = DialOrder::endpoints(&mac).into_iter().map(|e| e.host).collect();
    assert_eq!(vec!["mac.tail1234.ts.net", "100.101.102.103", "192.168.1.5"], hosts);
    let worked = SavedMac { last_worked: Some("192.168.1.5|7455".into()), ..mac.clone() };
    assert_eq!("192.168.1.5", DialOrder::endpoints(&worked)[0].host);
    // A saved record round-trips, and is refused when its Mac ID is not its key's.
    assert_eq!(Some(mac.clone()), SavedMac::from_json(&mac.to_json()));
    let mut wrong = mac.to_json();
    wrong["macID"] = json!("A3C5HR4RBV67YR21");
    assert_eq!(None, SavedMac::from_json(&wrong));
}

// MARK: video

fn len(b: &[u8]) -> Vec<u8> {
    let mut o = vec![(b.len() >> 8) as u8, b.len() as u8];
    o.extend_from_slice(b);
    o
}

#[test]
fn sps_of_a_videotoolbox_stream() {
    // VideoToolbox's SPS for a 1512×948 stream (captured from a synthetic host).
    let sps = hex("420101216000000300b0000003000003007ba002f4803b9f79cf924489bf97136008");
    assert_eq!(Some((1512, 948)), hevc_sps_size(&sps));
    let mut payload = vec![4u8, 3];
    payload.extend(len(&hex("40010c01ffff216000000300b0000003000003007b3c09")));
    payload.extend(len(&sps));
    payload.extend(len(&hex("4401c0252f053240")));
    let ps = ParameterSets::parse(&payload).unwrap();
    assert_eq!(Some((1512, 948)), ps.picture_size());
    assert_eq!(4, ps.nal_length);
    assert_eq!(&[0, 0, 0, 1], &ps.annex_b()[..4]);
    assert_eq!(Some(&sps[..]), ps.sps());
    assert!(ps.vps().is_some() && ps.pps().is_some());
}

#[test]
fn annex_b() {
    let frame = [0u8, 0, 0, 2, 0x26, 0x01, 0, 0, 0, 1, 0x7F];
    assert_eq!(Some(vec![0, 0, 0, 1, 0x26, 0x01, 0, 0, 0, 1, 0x7F]), ParameterSets::to_annex_b(&frame, 4));
    assert_eq!(None, ParameterSets::to_annex_b(&[0, 0, 0, 9, 1], 4));
    assert_eq!(Some(vec![0, 0, 0, 1, 5, 6]), ParameterSets::to_annex_b(&[0, 2, 5, 6], 2));
    let mut in_place = frame;
    assert!(ParameterSets::to_annex_b_in_place(&mut in_place));
    assert_eq!([0, 0, 0, 1, 0x26, 0x01, 0, 0, 0, 1, 0x7F], in_place);
    assert!(!ParameterSets::to_annex_b_in_place(&mut [0, 0, 0, 9, 1]));
}

#[test]
fn blobs_as_the_mac_packs_them() {
    let t = thumbnail(&[0, 0, 1, 2, 0xFF, 0xD8]).unwrap();
    assert_eq!((258, vec![0xFF, 0xD8]), (t.window_id, t.jpeg));
    assert!(thumbnail(&[0, 0, 1, 2]).is_none());
    let i = icon(&[0, 3, b'a', b'.', b'b', 0x89, b'P']).unwrap();
    assert_eq!(("a.b", vec![0x89, b'P']), (i.bundle_id.as_str(), i.png));
    assert!(icon(&[0, 3, b'a', b'.', b'b']).is_none());
    let mut c = Vec::new();
    for f in [1.5f32, 2.0, 16.0, 24.0] {
        c.extend_from_slice(&f.to_bits().to_be_bytes());
    }
    c.push(7);
    let s = cursor_shape(&c).unwrap();
    assert_eq!((1.5, 2.0, 16.0, 24.0, vec![7]), (s.hot_x, s.hot_y, s.width, s.height, s.png));
}

// MARK: JSON as the Mac's Codable types have it

#[test]
fn input_events() {
    assert_eq!(v(r#"{"pointer":{"_0":"leftDown","x":0.5,"y":0.25}}"#), v(&input::pointer(PointerAction::LeftDown, 0.5, 0.25)));
    assert_eq!(v(r#"{"scroll":{"dx":0.01,"dy":-0.02,"x":0.5,"y":0.5}}"#), v(&input::scroll(0.5, 0.5, 0.01, -0.02)));
    assert_eq!(
        v(r#"{"scrollGesture":{"_0":"momentumBegan","x":0.1,"y":0.2}}"#),
        v(&input::scroll_gesture(ScrollPhase::MomentumBegan, 0.1, 0.2))
    );
    assert_eq!("hi\n\u{8}", v(&input::text("hi\n\u{8}"))["text"]["_0"].as_str().unwrap());
    assert_eq!(r#"{"key":{"down":true,"hidUsage":44,"modifiers":1048576}}"#, input::key(44, true, key_mods::COMMAND));
    // A position that is no number never reaches the Mac's decoder.
    assert_eq!(v(r#"{"pointer":{"_0":"move","x":0.0,"y":0.0}}"#), v(&input::pointer(PointerAction::Move, f64::NAN, f64::INFINITY)));
}

#[test]
fn sources_and_others() {
    assert_eq!(r#"{"desktop":{}}"#, payloads::select(StreamSource::Desktop));
    assert_eq!(r#"{"none":{}}"#, payloads::select(StreamSource::None));
    assert_eq!(r#"{"window":{"_0":42}}"#, payloads::select(StreamSource::Window(42)));
    let list = WindowList::parse(br#"{"active":{"window":{"_0":42}}}"#).unwrap();
    assert_eq!(StreamSource::Window(42), list.active);
    assert_eq!(
        v(r#"{"bitrate":15000000,"token":7}"#),
        v(&payloads::change_settings(7, &SettingsChange { bitrate: Some(15_000_000), ..Default::default() }))
    );
    assert_eq!(v(r#"{"fingers":3,"gesture":"swipeUp"}"#), v(&payloads::gesture("swipeUp", 3)));
    let hello = v(&payloads::hello("0.5.1", "1", "Galaxy A15 (SM-A155F)", None, None));
    assert_eq!(1, hello["protocol"]);
    assert_eq!("0.5.1", hello["appVersion"]);
    assert_eq!(v(r#"{"bundleID":"com.apple.Safari"}"#), v(&payloads::launch_app("com.apple.Safari")));
    assert_eq!(v(r#"{"id":7,"action":"close"}"#), v(&payloads::window_command(7, window_actions::CLOSE)));
    assert_eq!(v(r#"{"token":3}"#), v(&payloads::fetch_menu(3, None, None, None)));
    assert_eq!(v(r#"{"token":4,"id":"1.0","version":2,"title":"New"}"#), v(&payloads::press_menu_item(4, Some(2), "1.0", Some("New"))));
    assert_eq!(
        v(r#"{"v":1,"method":"code","proof":"p","name":"n","model":"m"}"#),
        v(&payloads::pair_request("code", "p", "n", "m"))
    );
    assert_eq!(1.5f64.to_bits().to_be_bytes(), payloads::ping(1.5));
}

#[test]
fn window_list_and_settings_from_the_mac() {
    let list = WindowList::parse(
        br#"{"macName":"Mac","windows":[{"id":7,"title":"T","appName":"A","bundleID":"b","width":800,"height":600}],
            "active":{"window":{"_0":7}},"launchID":"x","hostVersion":"0.5.1","protocol":1,"gestures":1}"#,
    )
    .unwrap();
    assert_eq!(StreamSource::Window(7), list.active);
    assert_eq!(Some(1), list.gestures);
    assert_eq!(800, list.windows[0].width);
    let s = HostSettingsState::parse(
        br#"{"settings":{"maxFPS":60,"bitrate":4000000,"captureScale":1,"prioritizeSpeed":false,"virtualDisplay":false},
            "persistent":true,"virtualDisplayAvailable":true,"softwareEncoder":false,"answering":3,
            "away":{"homeBitrate":15000000,"homeCaptureScale":2,"awayBitrate":4000000,"awayCaptureScale":1,"thisConnectionAway":true,"awayRunning":true}}"#,
    )
    .unwrap();
    assert_eq!(1.0, s.settings.capture_scale);
    assert_eq!(None, s.settings.direct_wireless);
    assert_eq!(Some(3), s.answering);
    assert!(s.away.unwrap().away_running);
    // Unknown keys are ignored; a settings object missing a required field is no state at all.
    assert!(HostSettingsState::parse(br#"{"settings":{"maxFPS":60},"later":1}"#).is_none());
    assert_eq!(vec![AppInfo { name: "Safari".into(), bundle_id: "com.apple.Safari".into() }], parse_app_list(br#"[{"name":"Safari","bundleID":"com.apple.Safari","new":1}]"#));
    assert!(parse_app_list(b"{}").is_empty());
    let g = Goodbye::parse(br#"{"reason":"update","message":"Update Sill.","minimumVersion":"1.2","reconnect":false}"#).unwrap();
    assert_eq!(("update", Some(false)), (g.reason.as_str(), g.reconnect));
    assert!(Goodbye::parse(b"{}").is_none());
    let p = MacPointer::parse(br#"{"x":0.25,"y":0.5,"inside":true,"seen":4}"#).unwrap();
    assert_eq!((Some((0.25, 0.5)), 4), (p.position(), p.seen));
    assert_eq!(None, MacPointer::parse(br#"{"inside":false}"#).unwrap().position());
    let r = PairResult::parse(br#"{"ok":false,"reason":"code","triesLeft":2}"#).unwrap();
    assert_eq!((false, Some("code"), Some(2)), (r.ok, r.reason.as_deref(), r.tries_left));
}

// MARK: framing

#[test]
fn framing() {
    let m = encode_at(kind::PONG, &[1, 2, 3], true, 12.5);
    assert_eq!(17, m.len());
    let mut bytes = m.clone();
    bytes.extend(encode(kind::TICK, &[]));
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    rt.block_on(async {
        let mut pieces = 0;
        let mut r = MessageReader::new(&bytes[..], || pieces += 1);
        let (a, b) = (r.next().await.unwrap(), r.next().await.unwrap());
        assert_eq!((kind::PONG, true, 12.5), (a.kind, a.is_keyframe, a.timestamp));
        assert_eq!(vec![1, 2, 3], a.payload);
        assert_eq!(kind::TICK, b.kind);
        assert!(matches!(r.next().await, Err(ReadError::Eof)));
        assert!(pieces >= 3);
        // A header announcing more than any host sends ends the session (an SSH banner reads as 1.7 GB).
        let huge = [200u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x60, 0, 0, 0];
        assert!(matches!(MessageReader::new(&huge[..], || {}).next().await, Err(ReadError::Io(_))));
        // A stream that ends inside a message is not a clean end.
        assert!(matches!(MessageReader::new(&m[..10], || {}).next().await, Err(ReadError::Io(_))));
        assert!(matches!(MessageReader::new(&m[..15], || {}).next().await, Err(ReadError::Io(_))));
    });
    let h = parse_header(m[..HEADER_LENGTH].try_into().unwrap());
    assert_eq!((kind::PONG, 3), (h.kind, h.length));
    assert_eq!(MAX_FRAME_PAYLOAD, host_payload_cap(kind::FRAME));
    assert_eq!(MAX_OTHER_HOST_PAYLOAD, host_payload_cap(kind::WINDOW_LIST));
}

// MARK: the settings ledger and the menus

fn state(bitrate: i64, answering: Option<i64>) -> HostSettingsState {
    let answering = answering.map(|a| format!(",\"answering\":{a}")).unwrap_or_default();
    HostSettingsState::parse(
        format!(
            r#"{{"settings":{{"maxFPS":60,"bitrate":{bitrate},"captureScale":2,"prioritizeSpeed":false,"virtualDisplay":false,"directWireless":false}},
        "persistent":true,"virtualDisplayAvailable":false,"softwareEncoder":false{answering}}}"#
        )
        .as_bytes(),
    )
    .unwrap()
}

fn bitrate(b: i64) -> SettingsChange {
    SettingsChange { bitrate: Some(b), ..Default::default() }
}

#[test]
fn ledger() {
    let mut l = SettingsLedger::new();
    assert!(l.pick(&bitrate(8_000_000), 0).is_none(), "nothing before the first kind 16");
    l.receive(state(4_000_000, None));
    assert!(l.pick(&bitrate(4_000_000), 0).is_none(), "a pick that changes nothing sends nothing");
    let (token, sent) = l.pick(&SettingsChange { bitrate: Some(15_000_000), max_fps: Some(60), ..Default::default() }, 0).unwrap();
    assert_eq!(bitrate(15_000_000), sent);
    assert_eq!(15_000_000, l.shown().unwrap().bitrate);
    assert!(l.receive(state(4_000_000, Some(token + 100))).is_empty() && l.is_pending(SettingsField::Bitrate), "an answer to another token keeps the pick");
    assert_eq!(vec![SettingsField::Bitrate], l.receive(state(4_000_000, Some(token)))); // refused
    assert_eq!(4_000_000, l.shown().unwrap().bitrate);
    l.pick(&bitrate(8_000_000), 1_000);
    assert!(l.expire(6_000, 4_000));
    assert_eq!(4_000_000, l.shown().unwrap().bitrate);
}

#[test]
fn menus() {
    let menu = |s: String| MacMenu::parse(s.as_bytes()).unwrap();
    let mut m = MenuState::new();
    let sub = m.subscribe();
    m.receive(&menu(format!(r#"{{"version":3,"app":"Probe","answering":{},"menus":[{{"id":"1","title":"File","submenu":true}}]}}"#, sub.token)));
    assert_eq!(Some(3), m.version());
    assert_eq!(Some("File"), m.top()[0].title.as_deref());
    let top = m.top()[0].clone();
    let req = m.open(&top, 0).unwrap();
    assert_eq!((Some("1"), Some(3), Some("File")), (req.id.as_deref(), req.version, req.title.as_deref()));
    m.receive(&menu(format!(
        r#"{{"version":3,"answering":{},"menu":"1","items":[{{"id":"1.0","title":"New","key":"⌘N"}},{{"separator":true}}]}}"#,
        req.token
    )));
    let Some(Menu::Items { items, .. }) = m.menus().get("1").cloned() else { panic!("the menu's items") };
    assert_eq!(Some("⌘N"), items[0].key.as_deref());
    assert!(items[1].separator);
    let press = m.choose(&items[0], 0).unwrap();
    assert_eq!(25, press.kind);
    assert_eq!(
        MenuOutcome::Pressed { ok: false, note: Some("gone".into()) },
        m.receive(&menu(format!(r#"{{"version":3,"answering":{},"pressed":false,"note":"gone"}}"#, press.token)))
    );
    // A new version settles a menu still loading.
    m.open(&top, 0);
    m.receive(&menu(r#"{"version":4,"menus":[{"id":"1","title":"File"}]}"#.to_string()));
    assert!(matches!(m.menus().get("1"), Some(Menu::Failed(_))));
}

// MARK: this device's identity

#[test]
fn identity_is_kept_and_its_certificate_names_its_key() {
    let store = MemoryKeyStore::default();
    assert!(!DeviceIdentity::exists(&store));
    let a = DeviceIdentity::load_or_create(&store).unwrap();
    assert!(DeviceIdentity::exists(&store));
    let b = DeviceIdentity::load_or_create(&store).unwrap();
    assert_eq!(a.fingerprint(), b.fingerprint());
    assert_eq!(a.certificate_der(), b.certificate_der());
    assert_eq!(Some(*a.fingerprint()), spki::fingerprint_of_certificate(a.certificate_der()));
    assert_eq!(*a.fingerprint(), spki::fingerprint(a.public_point()));
    assert_ne!(a.fingerprint(), DeviceIdentity::generate().unwrap().fingerprint());
    assert_eq!(None, spki::fingerprint_of_certificate(b"not a certificate"));
}

#[test]
fn file_key_store_keeps_named_secrets_private() {
    let dir = std::env::temp_dir().join(format!("sill-core-keys-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = FileKeyStore::new(&dir);
    assert_eq!(None, store.load("device-identity").unwrap());
    store.save("device-identity", b"secret").unwrap();
    assert_eq!(Some(b"secret".to_vec()), store.load("device-identity").unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("device-identity.key")).unwrap().permissions().mode();
        assert_eq!(0o600, mode & 0o777);
    }
    assert!(store.save("../escape", b"x").is_err());
    store.delete("device-identity").unwrap();
    store.delete("device-identity").unwrap();
    assert_eq!(None, store.load("device-identity").unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn saved_macs_survive_a_restart_and_addresses_are_classified() {
    use sill_core::net::SavedMacs;
    let dir = std::env::temp_dir().join(format!("sill-core-saved-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let fp = sha256(b"a mac");
    let mac = SavedMac {
        mac_id: mac_id::make(&fp),
        fingerprint: fp.to_vec(),
        name: "Studio".into(),
        recognition_key: Some("abc".into()),
        remote_port: 47455,
        addresses: vec![MacAddress { host: "100.101.102.103".into(), port: Some(7000), kind: "vpn".into(), via: "Tailscale".into() }],
        typed_addresses: vec!["mac.tail1234.ts.net".into()],
        info_issued_at: 5.0,
        last_worked: Some("100.101.102.103|7000".into()),
        paired_at: 1.0,
        last_connected_at: Some(2.0),
        revoked: false,
    };
    SavedMacs::open(&dir).upsert(mac.clone());
    let again = SavedMacs::open(&dir);
    assert_eq!(vec![mac.clone()], again.all());
    again.update(&mac.mac_id, |m| m.revoked = true);
    assert!(SavedMacs::open(&dir).by_id(&mac.mac_id).unwrap().revoked);
    again.remove(&mac.mac_id);
    assert!(SavedMacs::open(&dir).all().is_empty());
    let _ = std::fs::remove_dir_all(&dir);

    assert!(SavedMacs::keepable("100.101.102.103", false) && SavedMacs::keepable("fearless.ts.net", false));
    assert!(!SavedMacs::keepable("169.254.3.4", true) && !SavedMacs::keepable("fe80::1", true) && !SavedMacs::keepable("fd00::1%en0", true));
    assert!(!SavedMacs::keepable("127.0.0.1", false) && SavedMacs::keepable("127.0.0.1", true));
    assert!(!SavedMacs::keepable("[::1]", false) && !SavedMacs::keepable("::ffff:169.254.1.1", true) && SavedMacs::keepable("::ffff:10.0.0.1", false));
    assert!(!SavedMacs::keepable("localhost", false) && !SavedMacs::keepable("febf::1", false));
}
