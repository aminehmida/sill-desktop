//! `KeyTyping` and `ForwardedKeys`, after iOS's Tests/checks/key-strokes and text-composition
//! (HardwareKeysTest.kt). Android's key codes and its layouts' scripts are not ported.

use sill_core::client::{usage, ForwardedKey, ForwardedKeys, KeyTyping, Keys};
use sill_core::protocol::key_mods;

const A: u16 = 0x04;
const Z: u16 = 0x1D;
const SHIFT: u16 = 0xE1;
const COMMAND: u16 = 0xE3;

fn key(usage: u16, down: bool, modifiers: u64) -> Option<ForwardedKey> {
    Some(ForwardedKey { usage, down, modifiers })
}

#[test]
fn who_types() {
    assert_eq!(KeyTyping::Composing, KeyTyping::of(true, false, true, false, true));
    assert_eq!(KeyTyping::Text, KeyTyping::of(false, false, false, false, true), "a Mac before key repeat");
    assert_eq!(KeyTyping::Text, KeyTyping::of(false, true, true, false, true), "a latch on the key row");
    assert_eq!(KeyTyping::Text, KeyTyping::of(false, false, true, true, true), "the software keyboard");
    assert_eq!(KeyTyping::Text, KeyTyping::of(false, false, true, false, false), "a Cyrillic layout");
    assert_eq!(KeyTyping::Keys, KeyTyping::of(false, false, true, false, true));
    assert!(KeyTyping::mac_repeats(Some(1)) && KeyTyping::mac_repeats(Some(2)));
    assert!(!(KeyTyping::mac_repeats(Some(0)) || KeyTyping::mac_repeats(None)));
}

#[test]
fn languages() {
    assert!(KeyTyping::types_on_mac(Some("en-US"), Some("Latn")));
    assert!(KeyTyping::types_on_mac(Some("fr"), Some("Latn")));
    assert!(!KeyTyping::types_on_mac(Some("vi-VN"), Some("Latn")), "Vietnamese composes");
    assert!(!KeyTyping::types_on_mac(Some("ja-JP"), Some("Jpan")));
    assert!(!KeyTyping::types_on_mac(Some("zh-Hans-CN"), Some("Hans")));
    assert!(!KeyTyping::types_on_mac(Some("ko"), Some("Kore")));
    assert!(!KeyTyping::types_on_mac(Some(""), Some("Latn")));
    assert!(!KeyTyping::types_on_mac(None, Some("Latn")));
}

#[test]
fn which_keys_go_as_keys() {
    for u in [0x4F, 0x29, 0x4C, 0x3A, 0x45, 0xE0, 0xE3, 0xE6] {
        assert!(ForwardedKeys::goes_as_key(u, 0, KeyTyping::Text));
    }
    assert!(!ForwardedKeys::goes_as_key(SHIFT, 0, KeyTyping::Text), "shift is the text's while it types");
    assert!(!ForwardedKeys::goes_as_key(A, key_mods::SHIFT, KeyTyping::Text), "a letter is the text's");
    assert!(ForwardedKeys::goes_as_key(A, key_mods::COMMAND, KeyTyping::Text), "⌘ makes any key a key");
    assert!(ForwardedKeys::goes_as_key(usage::ENTER, key_mods::SHIFT, KeyTyping::Text), "⇧Return is a key");
    assert!(!ForwardedKeys::goes_as_key(usage::ENTER, 0, KeyTyping::Text), "Return is the text's");
    for u in [A, Z, 0x27, usage::ENTER, usage::BACKSPACE, usage::TAB, usage::SPACE, 0x2D, 0x38, SHIFT, 0xE5] {
        assert!(ForwardedKeys::goes_as_key(u, 0, KeyTyping::Keys), "while the Mac types: {u}");
    }
    for u in [0x32, 0x39, 0x64] {
        assert!(!ForwardedKeys::goes_as_key(u, 0, KeyTyping::Keys), "ISO extras and Caps Lock stay the text's: {u}");
    }
}

#[test]
fn ups_follow_their_downs() {
    let mut f = ForwardedKeys::new();
    // ⌘S: ⌘'s own key carries what is held; S carries ⌘; the ups carry what is still down.
    assert_eq!(key(COMMAND, true, key_mods::COMMAND), f.began(COMMAND, key_mods::COMMAND, KeyTyping::Text, false));
    assert_eq!(key(0x16, true, key_mods::COMMAND), f.began(0x16, key_mods::COMMAND, KeyTyping::Text, false));
    assert_eq!(key(COMMAND, false, 0), f.ended(COMMAND), "⌘ let go first");
    assert_eq!(key(0x16, false, 0), f.ended(0x16), "then S, with nothing held");
    assert_eq!(None, f.ended(A), "a key the text path took sends no up");
    assert_eq!(None, f.began(A, 0, KeyTyping::Text, false));
    // While the Mac types: shift and a letter as keys.
    assert_eq!(key(SHIFT, true, key_mods::SHIFT), f.began(SHIFT, key_mods::SHIFT, KeyTyping::Keys, false));
    assert_eq!(key(A, true, key_mods::SHIFT), f.began(A, key_mods::SHIFT, KeyTyping::Keys, false));
    assert!(f.is_down(A));
    // Every key still down comes up, the last first.
    assert_eq!(vec![key(A, false, key_mods::SHIFT).unwrap(), key(SHIFT, false, 0).unwrap()], f.release_all());
    assert!(f.down().is_empty());
}

#[test]
fn caps_lock_carries_shift_for_letters() {
    assert_eq!(key_mods::SHIFT, ForwardedKeys::carried(A, 0, true));
    assert_eq!(0, ForwardedKeys::carried(0x1E, 0, true), "not a digit");
    assert_eq!(key_mods::COMMAND, ForwardedKeys::carried(A, key_mods::COMMAND, true), "a shortcut keeps its modifiers");
    assert_eq!(0, ForwardedKeys::carried(A, 0, false));
}

#[test]
fn keys_by_their_place_and_modifiers() {
    assert_eq!(Some(0x1C), Keys::usage_for_scan_code(21), "the key in Y's place");
    assert_eq!(Some(0x14), Keys::usage_for_scan_code(16), "the key in Q's place");
    assert_eq!(None, Keys::usage_for_scan_code(58));
    // Every place maps to a character key's usage.
    for scan in 1..=60 {
        if let Some(u) = Keys::usage_for_scan_code(scan) {
            assert!((0x04..=0x38).contains(&u), "scan {scan} → {u}");
        }
    }
    assert_eq!(Some(0x04), Keys::usage_for_char('A'));
    assert_eq!(Some(usage::ENTER), Keys::usage_for_char('\n'));
    assert_eq!(None, Keys::usage_for_char('é'));
    assert_eq!(
        vec![(usage::LEFT_CONTROL, key_mods::CONTROL), (usage::LEFT_SHIFT, key_mods::SHIFT), (usage::LEFT_OPTION, key_mods::OPTION), (usage::LEFT_COMMAND, key_mods::COMMAND)],
        Keys::modifier_keys(key_mods::COMMAND | key_mods::SHIFT | key_mods::OPTION | key_mods::CONTROL)
    );
    assert_eq!(key_mods::OPTION | key_mods::COMMAND, Keys::modifiers(false, false, true, true));
    assert_eq!(key_mods::SHIFT, Keys::modifier_bit(usage::RIGHT_SHIFT));
}
