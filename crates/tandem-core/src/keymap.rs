//! Keys between a Mac and Windows. The wire format is Windows' (virtual key + scan
//! code), so a Mac translates its own key codes into it when it is the one typing, and
//! back when it is being typed on. Keys are matched by their place on the keyboard, so
//! any keyboard layout types what its keys say.
//!
//! The modifier keys are swapped the way people expect: the Mac's Command key is the
//! PC's Ctrl (so Cmd+C on a Mac copies on the PC, and Ctrl+C on the PC copies on the
//! Mac), the Mac's Control is the PC's Windows key, and Option is Alt.
//!
//! Pure and platform independent, so it is tested on every machine.

/// (Mac key code, Windows virtual key, Windows scan code, extended key)
const KEYS: &[(u16, u16, u16, bool)] = &[
    // Letters
    (0x00, 0x41, 0x1E, false), // A
    (0x0B, 0x42, 0x30, false), // B
    (0x08, 0x43, 0x2E, false), // C
    (0x02, 0x44, 0x20, false), // D
    (0x0E, 0x45, 0x12, false), // E
    (0x03, 0x46, 0x21, false), // F
    (0x05, 0x47, 0x22, false), // G
    (0x04, 0x48, 0x23, false), // H
    (0x22, 0x49, 0x17, false), // I
    (0x26, 0x4A, 0x24, false), // J
    (0x28, 0x4B, 0x25, false), // K
    (0x25, 0x4C, 0x26, false), // L
    (0x2E, 0x4D, 0x32, false), // M
    (0x2D, 0x4E, 0x31, false), // N
    (0x1F, 0x4F, 0x18, false), // O
    (0x23, 0x50, 0x19, false), // P
    (0x0C, 0x51, 0x10, false), // Q
    (0x0F, 0x52, 0x13, false), // R
    (0x01, 0x53, 0x1F, false), // S
    (0x11, 0x54, 0x14, false), // T
    (0x20, 0x55, 0x16, false), // U
    (0x09, 0x56, 0x2F, false), // V
    (0x0D, 0x57, 0x11, false), // W
    (0x07, 0x58, 0x2D, false), // X
    (0x10, 0x59, 0x15, false), // Y
    (0x06, 0x5A, 0x2C, false), // Z
    // Digits row
    (0x1D, 0x30, 0x0B, false), // 0
    (0x12, 0x31, 0x02, false), // 1
    (0x13, 0x32, 0x03, false), // 2
    (0x14, 0x33, 0x04, false), // 3
    (0x15, 0x34, 0x05, false), // 4
    (0x17, 0x35, 0x06, false), // 5
    (0x16, 0x36, 0x07, false), // 6
    (0x1A, 0x37, 0x08, false), // 7
    (0x1C, 0x38, 0x09, false), // 8
    (0x19, 0x39, 0x0A, false), // 9
    // Punctuation
    (0x1B, 0xBD, 0x0C, false), // -
    (0x18, 0xBB, 0x0D, false), // =
    (0x21, 0xDB, 0x1A, false), // [
    (0x1E, 0xDD, 0x1B, false), // ]
    (0x2A, 0xDC, 0x2B, false), // backslash
    (0x29, 0xBA, 0x27, false), // ;
    (0x27, 0xDE, 0x28, false), // '
    (0x32, 0xC0, 0x29, false), // `
    (0x2B, 0xBC, 0x33, false), // ,
    (0x2F, 0xBE, 0x34, false), // .
    (0x2C, 0xBF, 0x35, false), // /
    (0x0A, 0xE2, 0x56, false), // the extra key on European keyboards
    // Editing
    (0x24, 0x0D, 0x1C, false), // Return / Enter
    (0x30, 0x09, 0x0F, false), // Tab
    (0x31, 0x20, 0x39, false), // Space
    (0x33, 0x08, 0x0E, false), // Delete (Backspace)
    (0x35, 0x1B, 0x01, false), // Escape
    (0x75, 0x2E, 0x53, true),  // Forward delete / Delete
    (0x72, 0x2D, 0x52, true),  // Help / Insert
    (0x73, 0x24, 0x47, true),  // Home
    (0x77, 0x23, 0x4F, true),  // End
    (0x74, 0x21, 0x49, true),  // Page Up
    (0x79, 0x22, 0x51, true),  // Page Down
    (0x7B, 0x25, 0x4B, true),  // Left
    (0x7C, 0x27, 0x4D, true),  // Right
    (0x7E, 0x26, 0x48, true),  // Up
    (0x7D, 0x28, 0x50, true),  // Down
    // Modifiers (swapped, see the top of the file)
    (0x37, 0xA2, 0x1D, false), // Command      <-> left Ctrl
    (0x36, 0xA3, 0x1D, true),  // right Command <-> right Ctrl
    (0x3B, 0x5B, 0x5B, true),  // Control      <-> left Windows key
    (0x3E, 0x5C, 0x5C, true),  // right Control <-> right Windows key
    (0x3A, 0xA4, 0x38, false), // Option       <-> left Alt
    (0x3D, 0xA5, 0x38, true),  // right Option  <-> right Alt (AltGr)
    (0x38, 0xA0, 0x2A, false), // Shift
    (0x3C, 0xA1, 0x36, false), // right Shift
    (0x39, 0x14, 0x3A, false), // Caps Lock
    // Function keys
    (0x7A, 0x70, 0x3B, false), // F1
    (0x78, 0x71, 0x3C, false), // F2
    (0x63, 0x72, 0x3D, false), // F3
    (0x76, 0x73, 0x3E, false), // F4
    (0x60, 0x74, 0x3F, false), // F5
    (0x61, 0x75, 0x40, false), // F6
    (0x62, 0x76, 0x41, false), // F7
    (0x64, 0x77, 0x42, false), // F8
    (0x65, 0x78, 0x43, false), // F9
    (0x6D, 0x79, 0x44, false), // F10
    (0x67, 0x7A, 0x57, false), // F11
    (0x6F, 0x7B, 0x58, false), // F12
    // Numeric keypad
    (0x52, 0x60, 0x52, false), // 0
    (0x53, 0x61, 0x4F, false), // 1
    (0x54, 0x62, 0x50, false), // 2
    (0x55, 0x63, 0x51, false), // 3
    (0x56, 0x64, 0x4B, false), // 4
    (0x57, 0x65, 0x4C, false), // 5
    (0x58, 0x66, 0x4D, false), // 6
    (0x59, 0x67, 0x47, false), // 7
    (0x5B, 0x68, 0x48, false), // 8
    (0x5C, 0x69, 0x49, false), // 9
    (0x41, 0x6E, 0x53, false), // .
    (0x43, 0x6A, 0x37, false), // *
    (0x45, 0x6B, 0x4E, false), // +
    (0x4E, 0x6D, 0x4A, false), // -
    (0x4B, 0x6F, 0x35, true),  // /
    (0x4C, 0x0D, 0x1C, true),  // Enter
    (0x47, 0x90, 0x45, false), // Clear <-> Num Lock
];

/// The Windows key for a Mac key code: (virtual key, scan code, extended).
pub fn mac_to_win(keycode: u16) -> Option<(u16, u16, bool)> {
    KEYS.iter()
        .find(|k| k.0 == keycode)
        .map(|k| (k.1, k.2, k.3))
}

/// The Mac key code for a Windows key. The scan code (the key's place) decides; the
/// virtual key is used when the scan code is missing, and for the generic Ctrl / Shift
/// / Alt codes some programs send.
pub fn win_to_mac(vk: u16, scan: u16, extended: bool) -> Option<u16> {
    if scan != 0 {
        if let Some(k) = KEYS.iter().find(|k| k.2 == scan && k.3 == extended) {
            return Some(k.0);
        }
    }
    let vk = match vk {
        0x10 => 0xA0, // Shift
        0x11 => {
            if extended {
                0xA3
            } else {
                0xA2
            }
        } // Ctrl
        0x12 => {
            if extended {
                0xA5
            } else {
                0xA4
            }
        } // Alt
        v => v,
    };
    KEYS.iter().find(|k| k.1 == vk).map(|k| k.0)
}

/// Mac modifier keys and the event flag each one sets.
pub fn mac_modifier_flag(keycode: u16) -> Option<u64> {
    Some(match keycode {
        0x37 | 0x36 => 0x0010_0000, // Command
        0x38 | 0x3C => 0x0002_0000, // Shift
        0x3B | 0x3E => 0x0004_0000, // Control
        0x3A | 0x3D => 0x0008_0000, // Option
        0x39 => 0x0001_0000,        // Caps Lock
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_goes_there_and_back() {
        for &(mac, vk, scan, ext) in KEYS {
            assert_eq!(mac_to_win(mac), Some((vk, scan, ext)), "mac {mac:#x}");
            assert_eq!(
                win_to_mac(vk, scan, ext),
                Some(mac),
                "win {vk:#x}/{scan:#x}"
            );
        }
    }

    #[test]
    fn no_key_is_listed_twice() {
        for (i, a) in KEYS.iter().enumerate() {
            for b in &KEYS[i + 1..] {
                assert_ne!(a.0, b.0, "mac code {:#x} twice", a.0);
                assert!(!(a.2 == b.2 && a.3 == b.3), "scan {:#x} twice", a.2);
            }
        }
    }

    #[test]
    fn copy_paste_shortcuts_follow_each_system() {
        // PC Ctrl -> Mac Command, so Ctrl+C copies on a Mac.
        assert_eq!(win_to_mac(0xA2, 0x1D, false), Some(0x37));
        assert_eq!(win_to_mac(0x11, 0, false), Some(0x37));
        // Mac Command -> PC Ctrl.
        assert_eq!(mac_to_win(0x37), Some((0xA2, 0x1D, false)));
        // C itself by its place on the keyboard.
        assert_eq!(win_to_mac(0x43, 0x2E, false), Some(0x08));
    }

    #[test]
    fn keypad_and_arrows_are_told_apart() {
        assert_eq!(win_to_mac(0x25, 0x4B, true), Some(0x7B)); // arrow left
        assert_eq!(win_to_mac(0x64, 0x4B, false), Some(0x56)); // keypad 4
        assert_eq!(win_to_mac(0x0D, 0x1C, false), Some(0x24)); // Return
        assert_eq!(win_to_mac(0x0D, 0x1C, true), Some(0x4C)); // keypad Enter
    }

    #[test]
    fn unknown_keys_are_dropped() {
        assert_eq!(mac_to_win(0x3F), None); // fn
        assert_eq!(win_to_mac(0x2C, 0, false), None); // Print Screen
    }
}
