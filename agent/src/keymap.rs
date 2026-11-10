// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! USB HID keyboard usages (the wire format) → Windows keys.
//!
//! Keys are played by scan code (set 1), so they mean the same physical key
//! whatever keyboard layout or input method is active on this PC. That keeps
//! IMEs such as Chinese Pinyin working, because they see real key strokes.

/// How to play one HID usage with `SendInput`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinKey {
    /// A scan code; `extended` adds the E0 prefix.
    Scan { code: u16, extended: bool },
    /// Keys whose scan codes Windows treats specially (Pause sends an E1
    /// sequence, NumLock shares its code with Pause): played by virtual key.
    Virtual(u16),
}

const fn scan(code: u16) -> Option<WinKey> {
    Some(WinKey::Scan {
        code,
        extended: false,
    })
}

const fn extended(code: u16) -> Option<WinKey> {
    Some(WinKey::Scan {
        code,
        extended: true,
    })
}

const VK_PAUSE: u16 = 0x13;
const VK_NUMLOCK: u16 = 0x90;

pub fn windows_key(usage: u32) -> Option<WinKey> {
    match usage {
        // Letters a–z.
        0x04 => scan(0x1E),
        0x05 => scan(0x30),
        0x06 => scan(0x2E),
        0x07 => scan(0x20),
        0x08 => scan(0x12),
        0x09 => scan(0x21),
        0x0A => scan(0x22),
        0x0B => scan(0x23),
        0x0C => scan(0x17),
        0x0D => scan(0x24),
        0x0E => scan(0x25),
        0x0F => scan(0x26),
        0x10 => scan(0x32),
        0x11 => scan(0x31),
        0x12 => scan(0x18),
        0x13 => scan(0x19),
        0x14 => scan(0x10),
        0x15 => scan(0x13),
        0x16 => scan(0x1F),
        0x17 => scan(0x14),
        0x18 => scan(0x16),
        0x19 => scan(0x2F),
        0x1A => scan(0x11),
        0x1B => scan(0x2D),
        0x1C => scan(0x15),
        0x1D => scan(0x2C),
        // Digits 1–9, 0.
        0x1E..=0x26 => scan(0x02 + (usage - 0x1E) as u16),
        0x27 => scan(0x0B),
        0x28 => scan(0x1C), // Enter
        0x29 => scan(0x01), // Escape
        0x2A => scan(0x0E), // Backspace
        0x2B => scan(0x0F), // Tab
        0x2C => scan(0x39), // Space
        0x2D => scan(0x0C), // - _
        0x2E => scan(0x0D), // = +
        0x2F => scan(0x1A), // [ {
        0x30 => scan(0x1B), // ] }
        0x31 => scan(0x2B), // \ |
        0x32 => scan(0x2B), // ISO # ~ (same key position as \ on ISO boards)
        0x33 => scan(0x27), // ; :
        0x34 => scan(0x28), // ' "
        0x35 => scan(0x29), // ` ~
        0x36 => scan(0x33), // , <
        0x37 => scan(0x34), // . >
        0x38 => scan(0x35), // / ?
        0x39 => scan(0x3A), // Caps Lock
        // F1–F10, F11, F12.
        0x3A..=0x43 => scan(0x3B + (usage - 0x3A) as u16),
        0x44 => scan(0x57),
        0x45 => scan(0x58),
        0x46 => extended(0x37), // Print Screen
        0x47 => scan(0x46),     // Scroll Lock
        0x48 => Some(WinKey::Virtual(VK_PAUSE)),
        0x49 => extended(0x52), // Insert
        0x4A => extended(0x47), // Home
        0x4B => extended(0x49), // Page Up
        0x4C => extended(0x53), // Delete
        0x4D => extended(0x4F), // End
        0x4E => extended(0x51), // Page Down
        0x4F => extended(0x4D), // Right
        0x50 => extended(0x4B), // Left
        0x51 => extended(0x50), // Down
        0x52 => extended(0x48), // Up
        0x53 => Some(WinKey::Virtual(VK_NUMLOCK)),
        0x54 => extended(0x35), // Keypad /
        0x55 => scan(0x37),     // Keypad *
        0x56 => scan(0x4A),     // Keypad -
        0x57 => scan(0x4E),     // Keypad +
        0x58 => extended(0x1C), // Keypad Enter
        0x59 => scan(0x4F),     // Keypad 1
        0x5A => scan(0x50),     // Keypad 2
        0x5B => scan(0x51),     // Keypad 3
        0x5C => scan(0x4B),     // Keypad 4
        0x5D => scan(0x4C),     // Keypad 5
        0x5E => scan(0x4D),     // Keypad 6
        0x5F => scan(0x47),     // Keypad 7
        0x60 => scan(0x48),     // Keypad 8
        0x61 => scan(0x49),     // Keypad 9
        0x62 => scan(0x52),     // Keypad 0
        0x63 => scan(0x53),     // Keypad .
        0x64 => scan(0x56),     // ISO \ | (left of Z)
        0x65 => extended(0x5D), // Application / Menu
        0x67 => scan(0x59),     // Keypad =
        // F13–F23, F24.
        0x68..=0x72 => scan(0x64 + (usage - 0x68) as u16),
        0x73 => scan(0x76),
        0x7F => extended(0x20), // Mute
        0x80 => extended(0x30), // Volume Up
        0x81 => extended(0x2E), // Volume Down
        0x85 => scan(0x7E),     // Keypad , (Brazilian)
        0x87 => scan(0x73),     // International 1: JIS \ _
        0x88 => scan(0x70),     // International 2: Katakana/Hiragana
        0x89 => scan(0x7D),     // International 3: JIS ¥
        0x8A => scan(0x79),     // International 4: Henkan
        0x8B => scan(0x7B),     // International 5: Muhenkan
        0x90 => scan(0x72),     // Lang 1: Hangul / Mac かな
        0x91 => scan(0x71),     // Lang 2: Hanja / Mac 英数
        0xE0 => scan(0x1D),     // Left Control
        0xE1 => scan(0x2A),     // Left Shift
        0xE2 => scan(0x38),     // Left Alt
        0xE3 => extended(0x5B), // Left Windows
        0xE4 => extended(0x1D), // Right Control
        0xE5 => scan(0x36),     // Right Shift
        0xE6 => extended(0x38), // Right Alt (AltGr)
        0xE7 => extended(0x5C), // Right Windows
        _ => None,
    }
}

/// The reverse of [`windows_key`], for keys captured on this PC: the HID
/// usage of a key from a low-level hook's scan code, E0 flag and virtual key.
pub fn hid_usage(scan_code: u32, extended: bool, virtual_key: u32) -> Option<u32> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    struct Reverse {
        scan: HashMap<(u16, bool), u32>,
        virtual_key: HashMap<u16, u32>,
    }
    static REVERSE: OnceLock<Reverse> = OnceLock::new();
    let reverse = REVERSE.get_or_init(|| {
        let mut scan = HashMap::new();
        let mut virtual_key = HashMap::new();
        // Ascending, so a shared key position keeps its first usage (0x31
        // over the ISO 0x32).
        for usage in 0..=0xFF {
            match windows_key(usage) {
                Some(WinKey::Scan { code, extended }) => {
                    scan.entry((code, extended)).or_insert(usage);
                }
                Some(WinKey::Virtual(key)) => {
                    virtual_key.entry(key).or_insert(usage);
                }
                None => {}
            }
        }
        Reverse { scan, virtual_key }
    });
    if let Ok(key) = u16::try_from(virtual_key)
        && let Some(&usage) = reverse.virtual_key.get(&key)
    {
        return Some(usage);
    }
    // Right Shift has no E0 prefix on the wire, but keyboard layouts mark it
    // extended, and a low-level hook reports it so (`LLKHF_EXTENDED`).
    if virtual_key == VK_RSHIFT {
        return Some(0xE5);
    }
    let code = u16::try_from(scan_code).ok()?;
    reverse.scan.get(&(code, extended)).copied()
}

const VK_RSHIFT: u32 = 0xA1;

#[cfg(test)]
mod tests {
    use super::{WinKey, hid_usage, windows_key};

    #[test]
    fn captured_keys_map_back_to_the_usage_they_play() {
        for usage in 0..=0xFF {
            let back = match windows_key(usage) {
                Some(WinKey::Scan { code, extended }) => hid_usage(code.into(), extended, 0),
                Some(WinKey::Virtual(key)) => hid_usage(0, false, key.into()),
                None => continue,
            };
            let expected = if usage == 0x32 { 0x31 } else { usage };
            assert_eq!(back, Some(expected), "usage {usage:#x}");
        }
        // Left and right Control share a scan code; E0 tells them apart.
        assert_eq!(hid_usage(0x1D, false, 0xA2), Some(0xE0));
        assert_eq!(hid_usage(0x1D, true, 0xA3), Some(0xE4));
        // Right Shift, with or without the extended flag a hook reports.
        assert_eq!(hid_usage(0x36, true, 0xA1), Some(0xE5));
        assert_eq!(hid_usage(0x36, false, 0xA1), Some(0xE5));
        assert_eq!(hid_usage(0x2A, false, 0xA0), Some(0xE1));
    }

    fn code(usage: u32) -> (u16, bool) {
        match windows_key(usage) {
            Some(WinKey::Scan { code, extended }) => (code, extended),
            other => panic!("usage {usage:#x} maps to {other:?}"),
        }
    }

    #[test]
    fn letters_digits_and_function_keys_follow_set_one() {
        assert_eq!(code(0x04), (0x1E, false)); // a
        assert_eq!(code(0x1D), (0x2C, false)); // z
        assert_eq!(code(0x1E), (0x02, false)); // 1
        assert_eq!(code(0x26), (0x0A, false)); // 9
        assert_eq!(code(0x27), (0x0B, false)); // 0
        assert_eq!(code(0x3A), (0x3B, false)); // F1
        assert_eq!(code(0x43), (0x44, false)); // F10
        assert_eq!(code(0x45), (0x58, false)); // F12
        assert_eq!(code(0x68), (0x64, false)); // F13
        assert_eq!(code(0x72), (0x6E, false)); // F23
        assert_eq!(code(0x73), (0x76, false)); // F24
    }

    #[test]
    fn navigation_and_right_modifiers_are_extended() {
        for usage in [
            0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0x50, 0x51, 0x52, 0xE3, 0xE4, 0xE6, 0xE7,
        ] {
            assert!(code(usage).1, "usage {usage:#x} should be extended");
        }
        for usage in [0xE0, 0xE1, 0xE2, 0xE5, 0x39] {
            assert!(!code(usage).1, "usage {usage:#x} should not be extended");
        }
    }

    #[test]
    fn pause_and_num_lock_use_virtual_keys() {
        assert_eq!(windows_key(0x48), Some(WinKey::Virtual(0x13)));
        assert_eq!(windows_key(0x53), Some(WinKey::Virtual(0x90)));
    }

    #[test]
    fn every_key_the_mac_sends_is_mapped() {
        // Usages produced by rinflow-macos/Rinflow/Input/HIDKeyMap.swift.
        let mac = [
            0x04, 0x16, 0x07, 0x09, 0x0B, 0x0A, 0x1D, 0x1B, 0x06, 0x19, 0x05, 0x14, 0x1A, 0x08,
            0x15, 0x1C, 0x17, 0x1E, 0x1F, 0x20, 0x21, 0x23, 0x22, 0x2E, 0x26, 0x24, 0x2D, 0x25,
            0x27, 0x30, 0x12, 0x18, 0x2F, 0x0C, 0x13, 0x28, 0x0F, 0x0D, 0x34, 0x0E, 0x33, 0x31,
            0x36, 0x38, 0x11, 0x10, 0x37, 0x2B, 0x2C, 0x35, 0x2A, 0x29, 0xE7, 0xE3, 0xE1, 0x39,
            0xE2, 0xE0, 0xE5, 0xE6, 0xE4, 0x6C, 0x63, 0x55, 0x57, 0x53, 0x54, 0x58, 0x56, 0x67,
            0x62, 0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F, 0x60, 0x61, 0x3E, 0x3F, 0x40, 0x3C,
            0x41, 0x42, 0x44, 0x46, 0x45, 0x68, 0x43, 0x69, 0x6A, 0x49, 0x4A, 0x4B, 0x4C, 0x3D,
            0x4D, 0x3B, 0x4E, 0x3A, 0x50, 0x4F, 0x51, 0x52,
        ];
        for usage in mac {
            assert!(
                windows_key(usage).is_some(),
                "usage {usage:#x} is not mapped"
            );
        }
    }
}
