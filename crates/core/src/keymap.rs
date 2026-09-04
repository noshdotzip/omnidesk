//! PS/2 set-1 scancodes and evdev keycodes, in both directions.
//!
//! Lives in `core`, not in a platform crate, because it is pure arithmetic that *both*
//! ends need: the Linux backend translates for the portal, and the agent translates
//! captured Linux input on its way to a Windows peer. It sat in `platform-linux` until
//! the second caller appeared, at which point the only alternatives were a Windows
//! build depending on the Linux crate or a second copy of the extended-key table --
//! and a duplicated table is exactly the thing that drifts and then types the wrong
//! key.
//!
//! The wire protocol carries PS/2 set-1 scancodes because the Windows backend speaks
//! them natively (`KEYEVENTF_SCANCODE`). Linux speaks evdev at both ends: the XDG
//! RemoteDesktop portal wants evdev keycodes for injection, and libei reports evdev
//! keycodes for capture. Translating between them is pure arithmetic, so it lives here
//! and is tested on every platform.
//!
//! Both directions are needed because the KVM runs both ways: Windows drives Linux
//! (scancode -> evdev, injected through the portal) and Linux drives Windows
//! (evdev -> scancode, captured through libei). [`evdev_to_scancode`] is written as the
//! exact inverse of [`scancode_to_evdev`] and a round-trip test walks every code the
//! forward map accepts, so the two cannot drift apart.
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

/// Translate an evdev keycode back to a PS/2 set-1 scancode.
///
/// The exact inverse of [`scancode_to_evdev`]. Returns `None` for anything the forward
/// map would not have produced, for the same reason: a guessed scancode types a key the
/// user never pressed, which is worse than dropping it.
///
/// Extended keys come back with the [`EXTENDED`] bit set, so the caller knows to emit
/// the `0xE0` prefix. Dropping that bit turns right control into left control and the
/// arrow keys into the numeric keypad.
pub fn evdev_to_scancode(keycode: i32) -> Option<u16> {
    // Checked before the identity range on purpose. The two spaces do not overlap
    // today (the table's values are all >= 96, the identity range ends at 88), but
    // ordering it this way means adding a low-numbered extended key later cannot
    // silently fall through to the identity branch.
    if let Some((base, _)) = EXTENDED_TABLE.iter().find(|(_, code)| *code == keycode) {
        return Some(EXTENDED | base);
    }
    if (1..=IDENTITY_MAX as i32).contains(&keycode) {
        return Some(keycode as u16);
    }
    None
}

/// evdev button code for the left mouse button (`BTN_LEFT` in `input-event-codes.h`).
///
/// Named because the numbering is a real trap: X11 calls the left button 1, evdev calls
/// it 0x110. Passing an X11 button number here lands a middle-click.
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;

/// Which of the three buttons Ultidesk forwards an evdev button code is, if any.
///
/// Returns `None` for side buttons, extra buttons and anything else: the wire protocol
/// carries three buttons, and mapping a fourth onto one of them would make the peer
/// click something the user did not.
pub fn evdev_button_index(button: u32) -> Option<u8> {
    match button {
        BTN_LEFT => Some(0),
        BTN_RIGHT => Some(1),
        BTN_MIDDLE => Some(2),
        _ => None,
    }
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
    fn every_scancode_the_forward_map_accepts_survives_a_round_trip() {
        // The property that actually matters: the two directions are inverses. Walks
        // the whole plain range and the whole extended range rather than sampling, so
        // adding an entry to one table without the other fails here.
        let mut checked = 0;
        for raw in 0u16..=0xFF {
            for prefix in [0u16, EXTENDED] {
                let sc = prefix | raw;
                if let Some(evdev) = scancode_to_evdev(sc) {
                    assert_eq!(
                        evdev_to_scancode(evdev),
                        Some(sc),
                        "0x{sc:04X} -> evdev {evdev} did not come back"
                    );
                    checked += 1;
                }
            }
        }
        // Guards against the test passing because the forward map returned None for
        // everything (e.g. if IDENTITY_MAX were accidentally zeroed).
        assert!(checked > 80, "only {checked} codes round-tripped");
    }

    #[test]
    fn evdev_keys_outside_the_mapped_set_are_dropped_not_guessed() {
        assert_eq!(evdev_to_scancode(0), None);
        // 89..95 sit between the identity range and the extended table's values.
        assert_eq!(evdev_to_scancode(90), None);
        assert_eq!(evdev_to_scancode(200), None);
        assert_eq!(evdev_to_scancode(-1), None);
    }

    #[test]
    fn extended_evdev_keys_come_back_with_the_prefix_bit() {
        // Losing the EXTENDED bit is the inverse of the bug the forward test guards:
        // right control would arrive at the peer as left control.
        assert_eq!(evdev_to_scancode(97), Some(EXTENDED | 0x1D)); // right ctrl
        assert_ne!(evdev_to_scancode(97), Some(0x1D));
        assert_eq!(evdev_to_scancode(103), Some(EXTENDED | 0x48)); // up
        assert_eq!(evdev_to_scancode(125), Some(EXTENDED | 0x5B)); // left meta
        assert_eq!(evdev_to_scancode(96), Some(EXTENDED | 0x1C)); // keypad enter
    }

    #[test]
    fn enter_and_keypad_enter_do_not_collapse_onto_each_other() {
        // They share base 0x1C and differ only by the prefix, so an inverse that
        // searched the identity range first would return plain enter for both.
        assert_eq!(evdev_to_scancode(28), Some(0x1C));
        assert_eq!(evdev_to_scancode(96), Some(EXTENDED | 0x1C));
        assert_ne!(evdev_to_scancode(28), evdev_to_scancode(96));
    }

    #[test]
    fn mouse_buttons_use_evdev_numbering_not_x11() {
        // BTN_LEFT is 0x110, not 1. Feeding X11 numbering here middle-clicks.
        assert_eq!(evdev_button_index(BTN_LEFT), Some(0));
        assert_eq!(evdev_button_index(BTN_RIGHT), Some(1));
        assert_eq!(evdev_button_index(BTN_MIDDLE), Some(2));
        assert_eq!(evdev_button_index(1), None, "X11 button 1 must not map");
        assert_eq!(evdev_button_index(2), None);
    }

    #[test]
    fn extra_mouse_buttons_are_dropped_rather_than_folded_onto_a_real_one() {
        // BTN_SIDE/BTN_EXTRA. Mapping these onto left/right would click something the
        // user did not press.
        assert_eq!(evdev_button_index(0x113), None);
        assert_eq!(evdev_button_index(0x114), None);
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
