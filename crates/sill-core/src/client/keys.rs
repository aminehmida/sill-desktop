//! Keys as the Mac takes them (Input.swift's `.key`): USB HID keyboard usages with
//! `UIKeyModifierFlags` bits (iOSClient/KeyChords.swift), and a hardware keyboard's keys on their
//! way to the Mac (`ForwardedKeys`, `KeyTyping`).

use crate::protocol::key_mods;

/// USB HID keyboard-page usages and the rules about them.
pub mod usage {
    pub const ENTER: u16 = 0x28;
    pub const ESCAPE: u16 = 0x29;
    pub const BACKSPACE: u16 = 0x2A;
    pub const TAB: u16 = 0x2B;
    pub const SPACE: u16 = 0x2C;
    pub const CAPS_LOCK: u16 = 0x39;
    pub const F1: u16 = 0x3A;
    pub const F12: u16 = 0x45;
    pub const HOME: u16 = 0x4A;
    pub const PAGE_UP: u16 = 0x4B;
    pub const FORWARD_DELETE: u16 = 0x4C;
    pub const END: u16 = 0x4D;
    pub const PAGE_DOWN: u16 = 0x4E;
    pub const RIGHT: u16 = 0x4F;
    pub const LEFT: u16 = 0x50;
    pub const DOWN: u16 = 0x51;
    pub const UP: u16 = 0x52;
    pub const LEFT_CONTROL: u16 = 0xE0;
    pub const LEFT_SHIFT: u16 = 0xE1;
    pub const LEFT_OPTION: u16 = 0xE2;
    pub const LEFT_COMMAND: u16 = 0xE3;
    pub const RIGHT_CONTROL: u16 = 0xE4;
    pub const RIGHT_SHIFT: u16 = 0xE5;
    pub const RIGHT_OPTION: u16 = 0xE6;
    pub const RIGHT_COMMAND: u16 = 0xE7;
    /// V: the key ⌘V presses.
    pub const V: u16 = 0x19;
}

pub struct Keys;

impl Keys {
    /// The modifier keys a set of flags holds, in the order a keyboard presses them: ⌃ ⇧ ⌥ ⌘.
    pub fn modifier_keys(mods: u64) -> Vec<(u16, u64)> {
        let mut out = Vec::with_capacity(4);
        if mods & key_mods::CONTROL != 0 {
            out.push((usage::LEFT_CONTROL, key_mods::CONTROL));
        }
        if mods & key_mods::SHIFT != 0 {
            out.push((usage::LEFT_SHIFT, key_mods::SHIFT));
        }
        if mods & key_mods::OPTION != 0 {
            out.push((usage::LEFT_OPTION, key_mods::OPTION));
        }
        if mods & key_mods::COMMAND != 0 {
            out.push((usage::LEFT_COMMAND, key_mods::COMMAND));
        }
        out
    }

    /// Modifier flags from what a desktop toolkit reports. The PC keyboard's Windows (Super/Meta)
    /// key is ⌘, Alt is ⌥.
    pub fn modifiers(shift: bool, control: bool, alt: bool, meta: bool) -> u64 {
        let mut m = 0;
        if shift {
            m |= key_mods::SHIFT;
        }
        if control {
            m |= key_mods::CONTROL;
        }
        if alt {
            m |= key_mods::OPTION;
        }
        if meta {
            m |= key_mods::COMMAND;
        }
        m
    }

    /// The modifier bit a modifier key's own usage stands for.
    pub fn modifier_bit(usage: u16) -> u64 {
        match usage {
            0xE0 | 0xE4 => key_mods::CONTROL,
            0xE1 | 0xE5 => key_mods::SHIFT,
            0xE2 | 0xE6 => key_mods::OPTION,
            0xE3 | 0xE7 => key_mods::COMMAND,
            _ => 0,
        }
    }

    /// The keys the text path never delivers, which go to the Mac as keys whatever is held
    /// (KeyChords' `ForwardedKeys.keyUsages`): the arrows, escape, forward delete, home, end, page
    /// up and down, F1 to F12, and ⌘, ⌃ and ⌥ themselves. Shift is not one.
    pub fn always_key(usage: u16) -> bool {
        matches!(usage, 0x4F..=0x52 | 0x29 | 0x4C | 0x4A | 0x4D | 0x4B | 0x4E | 0x3A..=0x45 | 0xE0 | 0xE2 | 0xE3 | 0xE4 | 0xE6 | 0xE7)
    }

    /// Return and tab: with shift held they go as keys too ("\n" and "\t" as text carry no shift).
    pub fn shifted_key(usage: u16) -> bool {
        usage == usage::ENTER || usage == usage::TAB
    }

    /// The keys a keyboard types characters with (KeyChords' `characterUsages`): the letters, the
    /// digits, Return, Delete, Tab, Space, the punctuation keys every layout has and Shift's two
    /// keys.
    pub fn character_key(usage: u16) -> bool {
        matches!(usage, 0x04..=0x28 | 0x2A | 0x2B | 0x2C | 0x2D..=0x31 | 0x33..=0x38 | 0xE1 | 0xE5)
    }

    /// The letters A to Z, by their keys' places.
    pub fn letter_key(usage: u16) -> bool {
        (0x04..=0x1D).contains(&usage)
    }

    /// A key's place on the keyboard as Linux names it (an evdev scan code) to its usage, for the
    /// keys whose characters a layout decides: the letters, digits and punctuation. The Mac takes
    /// a key by its place, as from a keyboard plugged into it, and its own layout makes the
    /// character.
    pub fn usage_for_scan_code(scan_code: u32) -> Option<u16> {
        const ROW_Q: [u16; 10] = [0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13];
        const ROW_A: [u16; 9] = [0x04, 0x16, 0x07, 0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F];
        const ROW_Z: [u16; 7] = [0x1D, 0x1B, 0x06, 0x19, 0x05, 0x11, 0x10];
        Some(match scan_code {
            2..=10 => 0x1E + (scan_code - 2) as u16,
            11 => 0x27,
            12 => 0x2D,
            13 => 0x2E,
            16..=25 => ROW_Q[(scan_code - 16) as usize],
            26 => 0x2F,
            27 => 0x30,
            30..=38 => ROW_A[(scan_code - 30) as usize],
            39 => 0x33,
            40 => 0x34,
            41 => 0x35,
            43 => 0x31,
            44..=50 => ROW_Z[(scan_code - 44) as usize],
            51 => 0x36,
            52 => 0x37,
            53 => 0x38,
            _ => return None,
        })
    }

    /// A typed character's key on a US keyboard, for a latched ⌃, ⌥ or ⌘.
    pub fn usage_for_char(ch: char) -> Option<u16> {
        let c = ch.to_ascii_lowercase();
        Some(match c {
            'a'..='z' => 0x04 + (c as u16 - 'a' as u16),
            '1'..='9' => 0x1E + (c as u16 - '1' as u16),
            '0' => 0x27,
            '\n' => usage::ENTER,
            '\t' => usage::TAB,
            ' ' => usage::SPACE,
            '-' => 0x2D,
            '=' => 0x2E,
            '[' => 0x2F,
            ']' => 0x30,
            '\\' => 0x31,
            ';' => 0x33,
            '\'' => 0x34,
            '`' => 0x35,
            ',' => 0x36,
            '.' => 0x37,
            '/' => 0x38,
            _ => return None,
        })
    }
}

/// Who types a hardware keyboard's characters: the Mac, which is sent them as keys (so it repeats
/// a held one and its own layout and dead keys apply), or this device's text path, which sends
/// what the local layout gives as text. Asked as each key goes down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyTyping {
    /// An input method is composing.
    Composing,
    /// The text path types the characters. The keys the text path never delivers still go as keys.
    Text,
    /// The Mac types them.
    Keys,
}

impl KeyTyping {
    /// Whether the session's Mac repeats a key held down: its window list's `keyRepeat` ≥ 1.
    pub fn mac_repeats(key_repeat: Option<i64>) -> bool {
        key_repeat.unwrap_or(0) >= 1
    }

    /// The typing for a key going down now.
    pub fn of(composing: bool, latched: bool, mac_repeats: bool, soft_keyboard: bool, latin_layout: bool) -> KeyTyping {
        if composing {
            return KeyTyping::Composing;
        }
        if !mac_repeats || latched || soft_keyboard {
            return KeyTyping::Text;
        }
        if latin_layout { KeyTyping::Keys } else { KeyTyping::Text }
    }

    /// Whether an input method's language (a BCP 47 tag, and its likely script) types the Latin
    /// alphabet directly: written in the Latin script, and not one whose Latin keyboard composes
    /// (Vietnamese, Hawaiian).
    pub fn types_on_mac(language_tag: Option<&str>, script: Option<&str>) -> bool {
        let Some(tag) = language_tag.filter(|t| !t.is_empty()) else { return false };
        if script != Some("Latn") {
            return false;
        }
        let language = tag.split(['-', '_']).next().unwrap_or("").to_lowercase();
        !["vi", "haw"].contains(&language.as_str())
    }
}

/// One key event for the Mac.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForwardedKey {
    pub usage: u16,
    pub down: bool,
    pub modifiers: u64,
}

/// A hardware keyboard's keys sent to the Mac as keys. A key goes to the Mac as a key, or to the
/// text path, when it goes down, and its up follows the same way whatever the modifiers say by
/// then, so ⌘'s own release and a key let go after ⌘ reach the Mac, and a key the text path took
/// never sends a stray up.
///
/// A key's down carries the modifiers the key event gives it (and Shift for a letter typed with
/// Caps Lock on); a modifier's own key and every up carry only the modifier keys down on the Mac.
#[derive(Debug, Clone, Default)]
pub struct ForwardedKeys {
    down_keys: Vec<u16>,
}

const SHORTCUT_MAKERS: u64 = key_mods::CONTROL | key_mods::OPTION | key_mods::COMMAND;

impl ForwardedKeys {
    pub fn new() -> ForwardedKeys {
        ForwardedKeys::default()
    }

    /// The keys whose down went to the Mac and whose up has not, in the order they went down.
    pub fn down(&self) -> &[u16] {
        &self.down_keys
    }

    pub fn is_down(&self, usage: u16) -> bool {
        self.down_keys.contains(&usage)
    }

    /// A key pressed: its down, when it goes to the Mac as a key; `None` when the text path takes it.
    pub fn began(&mut self, usage: u16, modifiers: u64, typing: KeyTyping, caps_lock: bool) -> Option<ForwardedKey> {
        if !Self::goes_as_key(usage, modifiers, typing) {
            return None;
        }
        if !self.down_keys.contains(&usage) {
            self.down_keys.push(usage);
        }
        let is_modifier = Keys::modifier_bit(usage) != 0;
        Some(ForwardedKey { usage, down: true, modifiers: if is_modifier { self.held() } else { Self::carried(usage, modifiers, caps_lock) } })
    }

    /// A key released: its up, when its down went to the Mac, with the modifier keys still down there.
    pub fn ended(&mut self, usage: u16) -> Option<ForwardedKey> {
        let i = self.down_keys.iter().position(|u| *u == usage)?;
        self.down_keys.remove(i);
        Some(ForwardedKey { usage, down: false, modifiers: self.held() })
    }

    /// Every key still down on the Mac going up, the last pressed first (focus lost, say).
    pub fn release_all(&mut self) -> Vec<ForwardedKey> {
        let mut ups = Vec::new();
        while let Some(last) = self.down_keys.last().copied() {
            if let Some(k) = self.ended(last) {
                ups.push(k);
            }
        }
        ups
    }

    /// The modifier bits of the modifier keys down on the Mac.
    pub fn held(&self) -> u64 {
        self.down_keys.iter().fold(0, |m, u| m | Keys::modifier_bit(*u))
    }

    /// Whether a key pressed with these modifiers goes to the Mac as a key: one of the always-keys,
    /// return or tab with shift held, any key while ⌘, ⌃ or ⌥ is held (shortcuts), and, while the
    /// Mac types the characters, every key that types one.
    pub fn goes_as_key(usage: u16, modifiers: u64, typing: KeyTyping) -> bool {
        Keys::always_key(usage)
            || modifiers & SHORTCUT_MAKERS != 0
            || (Keys::shifted_key(usage) && modifiers & key_mods::SHIFT != 0)
            || (typing == KeyTyping::Keys && Keys::character_key(usage))
    }

    /// The modifiers a key's down carries: the key event's, and Shift with them for a letter typed
    /// with Caps Lock on and neither ⌘, ⌃ nor ⌥ held, so the Mac types it in capitals.
    pub fn carried(usage: u16, modifiers: u64, caps_lock: bool) -> u64 {
        if !Keys::letter_key(usage) || !caps_lock || modifiers & SHORTCUT_MAKERS != 0 {
            return modifiers;
        }
        modifiers | key_mods::SHIFT
    }
}
