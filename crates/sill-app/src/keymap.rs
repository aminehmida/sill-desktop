//! Keys as the Mac takes them: winit's physical key codes (the W3C `code` values, a key's place on
//! the keyboard whatever layout this computer uses) to USB HID keyboard usages (page 7). The Mac
//! applies its own layout, as it does for a keyboard plugged into it.

use winit::keyboard::KeyCode;

/// The HID usage of a physical key, or nil for a key a Mac has no use for.
pub fn hid_usage(code: KeyCode) -> Option<u16> {
    use KeyCode::*;
    Some(match code {
        KeyA => 0x04, KeyB => 0x05, KeyC => 0x06, KeyD => 0x07, KeyE => 0x08, KeyF => 0x09, KeyG => 0x0A,
        KeyH => 0x0B, KeyI => 0x0C, KeyJ => 0x0D, KeyK => 0x0E, KeyL => 0x0F, KeyM => 0x10, KeyN => 0x11,
        KeyO => 0x12, KeyP => 0x13, KeyQ => 0x14, KeyR => 0x15, KeyS => 0x16, KeyT => 0x17, KeyU => 0x18,
        KeyV => 0x19, KeyW => 0x1A, KeyX => 0x1B, KeyY => 0x1C, KeyZ => 0x1D,
        Digit1 => 0x1E, Digit2 => 0x1F, Digit3 => 0x20, Digit4 => 0x21, Digit5 => 0x22,
        Digit6 => 0x23, Digit7 => 0x24, Digit8 => 0x25, Digit9 => 0x26, Digit0 => 0x27,
        Enter => 0x28, Escape => 0x29, Backspace => 0x2A, Tab => 0x2B, Space => 0x2C,
        Minus => 0x2D, Equal => 0x2E, BracketLeft => 0x2F, BracketRight => 0x30, Backslash => 0x31,
        Semicolon => 0x33, Quote => 0x34, Backquote => 0x35, Comma => 0x36, Period => 0x37, Slash => 0x38,
        CapsLock => 0x39,
        F1 => 0x3A, F2 => 0x3B, F3 => 0x3C, F4 => 0x3D, F5 => 0x3E, F6 => 0x3F,
        F7 => 0x40, F8 => 0x41, F9 => 0x42, F10 => 0x43, F11 => 0x44, F12 => 0x45,
        PrintScreen => 0x46, ScrollLock => 0x47, Pause => 0x48, Insert => 0x49, Home => 0x4A, PageUp => 0x4B,
        Delete => 0x4C, End => 0x4D, PageDown => 0x4E,
        ArrowRight => 0x4F, ArrowLeft => 0x50, ArrowDown => 0x51, ArrowUp => 0x52,
        NumLock => 0x53, NumpadDivide => 0x54, NumpadMultiply => 0x55, NumpadSubtract => 0x56, NumpadAdd => 0x57,
        NumpadEnter => 0x58, Numpad1 => 0x59, Numpad2 => 0x5A, Numpad3 => 0x5B, Numpad4 => 0x5C, Numpad5 => 0x5D,
        Numpad6 => 0x5E, Numpad7 => 0x5F, Numpad8 => 0x60, Numpad9 => 0x61, Numpad0 => 0x62, NumpadDecimal => 0x63,
        IntlBackslash => 0x64, ContextMenu => 0x65, NumpadEqual => 0x67,
        F13 => 0x68, F14 => 0x69, F15 => 0x6A, F16 => 0x6B, F17 => 0x6C, F18 => 0x6D, F19 => 0x6E, F20 => 0x6F,
        NumpadComma => 0x85, IntlRo => 0x87, KanaMode => 0x88, IntlYen => 0x89, Convert => 0x8A, NonConvert => 0x8B,
        Lang1 => 0x90, Lang2 => 0x91,
        ControlLeft => 0xE0, ShiftLeft => 0xE1, AltLeft => 0xE2, SuperLeft => 0xE3,
        ControlRight => 0xE4, ShiftRight => 0xE5, AltRight => 0xE6, SuperRight => 0xE7,
        _ => return None,
    })
}

/// How this computer's modifier keys stand for the Mac's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ModifierLayout {
    /// By position, as a PC keyboard plugged into a Mac: Ctrl is ⌃, the Windows key ⌘, Alt ⌥.
    #[default]
    ByPosition,
    /// By habit on Windows and Linux: Ctrl is ⌘ (Ctrl+C copies), the Windows key ⌃, Alt ⌥.
    ControlIsCommand,
}

impl ModifierLayout {
    /// The usage sent for a key's own usage under this layout.
    pub fn apply(self, usage: u16) -> u16 {
        match (self, usage) {
            (Self::ControlIsCommand, 0xE0) => 0xE3,
            (Self::ControlIsCommand, 0xE3) => 0xE0,
            (Self::ControlIsCommand, 0xE4) => 0xE7,
            (Self::ControlIsCommand, 0xE7) => 0xE4,
            (_, u) => u,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_not_letters() {
        assert_eq!(hid_usage(KeyCode::KeyA), Some(0x04));
        assert_eq!(hid_usage(KeyCode::KeyZ), Some(0x1D));
        assert_eq!(hid_usage(KeyCode::Digit0), Some(0x27));
        assert_eq!(hid_usage(KeyCode::ArrowUp), Some(0x52));
        assert_eq!(hid_usage(KeyCode::SuperRight), Some(0xE7));
        assert_eq!(hid_usage(KeyCode::Fn), None);
    }

    #[test]
    fn control_is_command_swaps_both_sides() {
        let l = ModifierLayout::ControlIsCommand;
        assert_eq!(l.apply(0xE0), 0xE3);
        assert_eq!(l.apply(0xE3), 0xE0);
        assert_eq!(l.apply(0xE4), 0xE7);
        assert_eq!(l.apply(0x04), 0x04);
        assert_eq!(ModifierLayout::ByPosition.apply(0xE0), 0xE0);
    }
}
