//! PS/2 set-1 scancode to evdev keycode.
//!
//! The wire protocol carries PS/2 set-1 scancodes because the Windows backend speaks
//! them natively (`KEYEVENTF_SCANCODE`). The XDG RemoteDesktop portal wants **evdev**
//! keycodes. Translating between them is pure arithmetic, so it lives here and is
//! tested on every platform.
//!
//! # Why most of this is the identity function
//! Linux evdev keycodes for the main keyboard block were historically derived from AT
//! set-1 scancodes, so for `0x01..=0x58` the two are numerically identical: set-1 `0x1E`
//! is `KEY_A` = 30, set-1 `0x1C` is `KEY_ENTER` = 28. That is a real property worth
//! relying on, not a coincidence to paper over — but it stops at `0x58`.
//!
//! Beyond that, set-1 encodes extended keys with an `0xE0` prefix and the two numbering
//! schemes diverge completely: extended `0x1D` (right control) is `KEY_RIGHTCTRL` = 97,
//! not 29. Those need the explicit table below, and anything absent maps to `None`
//! rather than to a plausible-looking wrong key.

/// Highest set-1 scancode that is numerically equal to its evdev keycode.
const IDENTITY_MAX: u16 = 0x58;

/// Marker bit callers use to signal "this was an `0xE0`-prefixed extended scancode".
pub const EXTENDED: u16 = 0xE000;

/// Extended (`0xE0`-prefixed) set-1 scancodes that Ultidesk forwards, and the evdev
/// keycodes they correspond to. Deliberately short: only keys that have been reasoned
/// about belong here.
const EXTENDED_TABLE: &[(u16, i32)] = &[
    (0x1D, 97),  // right ctrl   -> KEY_RIGHTCTRL
    (0x38, 100), // right alt    -> KEY_RIGHTALT
    (0x1C, 96),  // keypad enter -> KEY_KPENTER
    (0x35, 98),  // keypad slash -> KEY_KPSLASH
    (0x47, 102), // home         -> KEY_HOME
    (0x48, 103), // up           -> KEY_UP
    (0x49, 104), // page up      -> KEY_PAGEUP
    (0x4B, 105), // left         -> KEY_LEFT
    (0x4D, 106), // right        -> KEY_RIGHT
    (0x4F, 107), // end          -> KEY_END
    (0x50, 108), // down         -> KEY_DOWN
    (0x51, 109), // page down    -> KEY_PAGEDOWN
    (0x52, 110), // insert       -> KEY_INSERT
    (0x53, 111), // delete       -> KEY_DELETE
    (0x5B, 125), // left meta    -> KEY_LEFTMETA
    (0x5C, 126), // right meta   -> KEY_RIGHTMETA
];

/// Translate a PS/2 set-1 scancode to an evdev keycode.
///
/// Set the [`EXTENDED`] bit for scancodes that arrived with an `0xE0` prefix. Returns
/// `None` for anything not known to map — the caller must drop the key rather than
/// inject a guess, because a wrong keycode is worse than a missing one: it types
/// something the user did not press.
pub fn scancode_to_evdev(scancode: u16) -> Option<i32> {
    if scancode & EXTENDED != 0 {
        let base = scancode & 0x00FF;
        return EXTENDED_TABLE
            .iter()
            .find(|(sc, _)| *sc == base)
            .map(|(_, code)| *code);
    }
    if (0x01..=IDENTITY_MAX).contains(&scancode) {
        return Some(scancode as i32);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_block_is_the_identity_map() {
        // These are the keys the Milestone-0 demo actually sends.
        assert_eq!(scancode_to_evdev(0x1E), Some(30)); // A       -> KEY_A
        assert_eq!(scancode_to_evdev(0x02), Some(2)); // 1       -> KEY_1
        assert_eq!(scancode_to_evdev(0x1C), Some(28)); // enter   -> KEY_ENTER
        assert_eq!(scancode_to_evdev(0x39), Some(57)); // space   -> KEY_SPACE
        assert_eq!(scancode_to_evdev(0x0E), Some(14)); // bksp    -> KEY_BACKSPACE
        assert_eq!(scancode_to_evdev(0x0F), Some(15)); // tab     -> KEY_TAB
        assert_eq!(scancode_to_evdev(0x01), Some(1)); // esc     -> KEY_ESC
        assert_eq!(scancode_to_evdev(0x3B), Some(59)); // F1      -> KEY_F1
    }

    #[test]
    fn identity_range_is_bounded_at_0x58() {
        assert_eq!(scancode_to_evdev(IDENTITY_MAX), Some(0x58));
        // Past the identity range the schemes diverge, so a plain scancode is not a
        // safe guess and must be refused.
        assert_eq!(scancode_to_evdev(IDENTITY_MAX + 1), None);
        assert_eq!(scancode_to_evdev(0x00), None);
    }

    #[test]
    fn extended_keys_are_not_the_identity_map() {
        // The bug this guards: treating extended 0x1D as 29 (left ctrl) instead of 97
        // (right ctrl) — a wrong key, silently, forever.
        assert_eq!(scancode_to_evdev(EXTENDED | 0x1D), Some(97));
        assert_ne!(scancode_to_evdev(EXTENDED | 0x1D), Some(29));
        assert_eq!(scancode_to_evdev(EXTENDED | 0x48), Some(103)); // up
        assert_eq!(scancode_to_evdev(EXTENDED | 0x4B), Some(105)); // left
        assert_eq!(scancode_to_evdev(EXTENDED | 0x5B), Some(125)); // left meta
    }

    #[test]
    fn the_same_base_code_means_different_keys_with_and_without_the_prefix() {
        // 0x1C is enter unprefixed, keypad enter when extended.
        assert_eq!(scancode_to_evdev(0x1C), Some(28));
        assert_eq!(scancode_to_evdev(EXTENDED | 0x1C), Some(96));
    }

    #[test]
    fn unknown_extended_codes_are_dropped_not_guessed() {
        // Injecting a guessed keycode types something the user never pressed, which is
        // worse than dropping the key.
        assert_eq!(scancode_to_evdev(EXTENDED | 0x7F), None);
    }

    #[test]
    fn every_extended_entry_is_outside_the_identity_range_value_space() {
        // Sanity on the table itself: an extended key that mapped to its own base code
        // would indicate a copy-paste error.
        for (base, code) in EXTENDED_TABLE {
            assert_ne!(
                *code, *base as i32,
                "extended 0x{base:02X} maps to its own base value, likely a mistake"
            );
        }
    }
}
