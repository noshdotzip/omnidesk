//! Translating captured Linux input into wire messages for a Windows peer.
//!
//! This is the join between the two halves of the KVM. libei hands us evdev keycodes,
//! evdev button codes and continuous pointer/scroll deltas; the wire protocol carries
//! PS/2 set-1 scancodes, three named buttons, absolute screen positions and whole wheel
//! notches. Every one of those is a different representation, and each conversion has
//! its own way of being silently wrong — so they live in tested modules
//! (`keymap`, `RemotePointer`, `ScrollAccumulator`) and this type only sequences them.
//!
//! # Nothing is guessed
//! A key or button with no mapping is **dropped and counted**, never approximated.
//! Injecting a plausible-looking substitute makes the peer type or click something the
//! operator did not do, which is worse than a key that does not arrive: one is a
//! noticeable gap, the other is silent corruption.

// Only the Linux `kvm-source` path constructs a Forwarder, so on a Windows build
// everything here is technically unreachable. It is still compiled there on purpose:
// this is pure arithmetic joining two representations, it is exactly the kind of code
// that breaks silently, and its tests are the thing that catches a break. Gating the
// module on Linux would mean a Windows developer never runs them. The dead-code
// warnings are therefore structural rather than real, the same situation as `audio`.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use ultidesk_core::keymap;
use ultidesk_core::scroll::ScrollAccumulator;
use ultidesk_topology::{PointerUpdate, Rect, RemotePointer, Side};

use crate::ipc::{IpcRequest, MouseButtonDto, VirtualScreenDto};

/// Why an event produced nothing to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DropReason {
    /// An evdev keycode outside the set `keymap` can translate.
    UnmappedKey(u32),
    /// A button beyond left/right/middle — side buttons, extra buttons.
    UnmappedButton(u32),
    /// Absolute pointer motion, which is expressed in the *source's* coordinate space
    /// and so cannot be forwarded as-is.
    ///
    /// Expected to be zero in practice: a captured pointer is a relative device. It is
    /// counted rather than ignored so that if a compositor does send these, the live
    /// test shows a stream of drops instead of a pointer that mysteriously will not
    /// move.
    AbsoluteMotion,
}

/// What to do with one captured event.
#[derive(Debug, Clone, PartialEq)]
pub enum Forwarded {
    /// Send this to the peer.
    Send(IpcRequest),
    /// Consumed, with nothing to send yet — a scroll delta still under one notch.
    Pending,
    /// Could not be represented on the wire.
    Dropped(DropReason),
    /// The pointer left the peer through the edge it arrived by. Stop forwarding and
    /// release the grab.
    ReturnHome,
}

/// One captured input event, in the shape `platform-linux`'s libei client produces.
///
/// Redeclared here rather than imported so this module — and its tests — compile on
/// Windows too. The Linux crate is not a dependency of the agent on Windows, and the
/// conversion logic is exactly what benefits from being tested on both platforms.
/// [`From`] impls on the Linux side keep the two in step.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum CapturedInput {
    PointerMotion { dx: f32, dy: f32 },
    PointerMotionAbsolute { x: f32, y: f32 },
    Button { button: u32, pressed: bool },
    Scroll { dx: f32, dy: f32 },
    Key { keycode: u32, pressed: bool },
}

/// Converts captured events into wire messages for one peer screen.
pub struct Forwarder {
    pointer: RemotePointer,
    scroll: ScrollAccumulator,
    virtual_screen: VirtualScreenDto,
    dropped: u64,
}

impl Forwarder {
    /// Start forwarding onto a peer screen, entering at `entry` a `fraction` of the way
    /// along that edge.
    ///
    /// `bounds` is the peer's screen in the peer's own coordinates, and
    /// `virtual_screen` is the peer's full virtual desktop — the two differ as soon as
    /// the peer has more than one monitor, and the injector needs the latter to
    /// normalise an absolute position.
    pub fn new(bounds: Rect, virtual_screen: VirtualScreenDto, entry: Side, fraction: f64) -> Self {
        Forwarder {
            pointer: RemotePointer::enter(bounds, entry, fraction),
            scroll: ScrollAccumulator::new(),
            virtual_screen,
            dropped: 0,
        }
    }

    /// How many events could not be represented. Surfaced so a live run reports
    /// "3 keys dropped" rather than leaving the operator to notice missing keystrokes.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// The message the peer needs to place its pointer where this one currently is.
    ///
    /// Sent once on entry, before any motion, so the peer's cursor appears at the
    /// crossing point instead of wherever it was left last time.
    pub fn initial_move(&self) -> IpcRequest {
        let (x, y) = self.pointer.position();
        IpcRequest::InjectMouseMove {
            screen_x: x,
            screen_y: y,
            virtual_screen: self.virtual_screen,
        }
    }

    pub fn translate(&mut self, event: CapturedInput) -> Forwarded {
        match event {
            CapturedInput::PointerMotion { dx, dy } => {
                match self.pointer.apply(dx as f64, dy as f64) {
                    PointerUpdate::ReturnedHome => {
                        // Drop any partial notch so it cannot surface as a stray scroll
                        // the next time control crosses over.
                        self.scroll.reset();
                        Forwarded::ReturnHome
                    }
                    PointerUpdate::Moved => {
                        let (x, y) = self.pointer.position();
                        Forwarded::Send(IpcRequest::InjectMouseMove {
                            screen_x: x,
                            screen_y: y,
                            virtual_screen: self.virtual_screen,
                        })
                    }
                }
            }
            CapturedInput::PointerMotionAbsolute { .. } => {
                self.dropped += 1;
                Forwarded::Dropped(DropReason::AbsoluteMotion)
            }
            CapturedInput::Button { button, pressed } => match button_dto(button) {
                Some(b) => Forwarded::Send(IpcRequest::InjectMouseButton {
                    button: b,
                    down: pressed,
                }),
                None => {
                    self.dropped += 1;
                    Forwarded::Dropped(DropReason::UnmappedButton(button))
                }
            },
            CapturedInput::Key { keycode, pressed } => {
                match keymap::evdev_to_scancode(keycode as i32) {
                    Some(scancode) => Forwarded::Send(IpcRequest::InjectKey {
                        scancode,
                        down: pressed,
                    }),
                    None => {
                        self.dropped += 1;
                        Forwarded::Dropped(DropReason::UnmappedKey(keycode))
                    }
                }
            }
            CapturedInput::Scroll { dx, dy } => {
                let delta = self.scroll.push(dx as f64, dy as f64);
                if delta.is_zero() {
                    Forwarded::Pending
                } else {
                    Forwarded::Send(IpcRequest::InjectScroll {
                        delta_x: delta.x,
                        delta_y: delta.y,
                    })
                }
            }
        }
    }
}

/// evdev button code to the wire's three-button enum.
///
/// The codes themselves live in `keymap` so there is one definition of what BTN_LEFT
/// is; this only names the wire variants.
fn button_dto(button: u32) -> Option<MouseButtonDto> {
    match keymap::evdev_button_index(button)? {
        0 => Some(MouseButtonDto::Left),
        1 => Some(MouseButtonDto::Right),
        2 => Some(MouseButtonDto::Middle),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vs() -> VirtualScreenDto {
        VirtualScreenDto {
            left: 0,
            top: 0,
            width: 1920,
            height: 1080,
        }
    }

    fn peer() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        }
    }

    fn forwarder() -> Forwarder {
        Forwarder::new(peer(), vs(), Side::Left, 0.5)
    }

    #[test]
    fn the_pointer_starts_at_the_crossing_point_not_where_the_peer_left_it() {
        let f = forwarder();
        assert_eq!(
            f.initial_move(),
            IpcRequest::InjectMouseMove {
                screen_x: 0,
                screen_y: 540,
                virtual_screen: vs(),
            }
        );
    }

    #[test]
    fn relative_motion_becomes_an_absolute_position() {
        let mut f = forwarder();
        assert_eq!(
            f.translate(CapturedInput::PointerMotion { dx: 100.0, dy: 0.0 }),
            Forwarded::Send(IpcRequest::InjectMouseMove {
                screen_x: 100,
                screen_y: 540,
                virtual_screen: vs(),
            })
        );
    }

    #[test]
    fn moving_back_out_the_entry_edge_asks_to_return() {
        let mut f = forwarder();
        f.translate(CapturedInput::PointerMotion { dx: 40.0, dy: 0.0 });
        assert_eq!(
            f.translate(CapturedInput::PointerMotion { dx: -50.0, dy: 0.0 }),
            Forwarded::ReturnHome
        );
    }

    #[test]
    fn letters_translate_through_the_identity_range() {
        let mut f = forwarder();
        // evdev KEY_A is 30; set-1 A is 0x1E, also 30.
        assert_eq!(
            f.translate(CapturedInput::Key {
                keycode: 30,
                pressed: true
            }),
            Forwarded::Send(IpcRequest::InjectKey {
                scancode: 0x1E,
                down: true
            })
        );
    }

    #[test]
    fn extended_keys_keep_their_prefix_bit() {
        // Right control must not arrive as left control.
        let mut f = forwarder();
        assert_eq!(
            f.translate(CapturedInput::Key {
                keycode: 97,
                pressed: true
            }),
            Forwarded::Send(IpcRequest::InjectKey {
                scancode: 0xE01D,
                down: true
            })
        );
    }

    #[test]
    fn an_unmapped_key_is_dropped_and_counted_not_approximated() {
        // Injecting a nearby scancode would type a character the operator never
        // pressed. A counted drop is visible; silent corruption is not.
        let mut f = forwarder();
        assert_eq!(
            f.translate(CapturedInput::Key {
                keycode: 190,
                pressed: true
            }),
            Forwarded::Dropped(DropReason::UnmappedKey(190))
        );
        assert_eq!(f.dropped(), 1);
    }

    #[test]
    fn mouse_buttons_use_evdev_numbering() {
        let mut f = forwarder();
        assert_eq!(
            f.translate(CapturedInput::Button {
                button: 0x110,
                pressed: true
            }),
            Forwarded::Send(IpcRequest::InjectMouseButton {
                button: MouseButtonDto::Left,
                down: true
            })
        );
        // X11 numbering must not be silently accepted as "button 1 = left".
        assert!(matches!(
            f.translate(CapturedInput::Button {
                button: 1,
                pressed: true
            }),
            Forwarded::Dropped(DropReason::UnmappedButton(1))
        ));
    }

    #[test]
    fn side_buttons_are_dropped_rather_than_folded_onto_a_real_button() {
        let mut f = forwarder();
        assert_eq!(
            f.translate(CapturedInput::Button {
                button: 0x113,
                pressed: true
            }),
            Forwarded::Dropped(DropReason::UnmappedButton(0x113))
        );
    }

    #[test]
    fn a_partial_scroll_notch_sends_nothing_yet() {
        let mut f = forwarder();
        assert_eq!(
            f.translate(CapturedInput::Scroll { dx: 0.0, dy: 3.0 }),
            Forwarded::Pending
        );
    }

    #[test]
    fn accumulated_scrolling_eventually_sends_a_whole_notch() {
        let mut f = forwarder();
        let mut sent = None;
        for _ in 0..10 {
            if let Forwarded::Send(req) = f.translate(CapturedInput::Scroll { dx: 0.0, dy: 3.0 }) {
                sent = Some(req);
                break;
            }
        }
        match sent {
            Some(IpcRequest::InjectScroll { delta_x, delta_y }) => {
                assert_eq!(delta_x, 0);
                // Negative: scrolling toward the user is a negative Win32 wheel delta.
                assert!(
                    delta_y < 0,
                    "expected a downward wheel delta, got {delta_y}"
                );
            }
            other => panic!("expected a scroll message, got {other:?}"),
        }
    }

    #[test]
    fn returning_home_clears_a_partial_scroll_notch() {
        // Otherwise the leftover fires as a stray scroll the next time control crosses.
        let mut f = forwarder();
        f.translate(CapturedInput::Scroll { dx: 0.0, dy: 14.0 });
        assert_eq!(
            f.translate(CapturedInput::PointerMotion { dx: -1.0, dy: 0.0 }),
            Forwarded::ReturnHome
        );
        assert_eq!(
            f.translate(CapturedInput::Scroll { dx: 0.0, dy: 2.0 }),
            Forwarded::Pending,
            "the pending notch should have been discarded"
        );
    }

    #[test]
    fn absolute_motion_is_dropped_and_counted_so_it_is_visible_if_it_ever_happens() {
        let mut f = forwarder();
        assert_eq!(
            f.translate(CapturedInput::PointerMotionAbsolute { x: 10.0, y: 10.0 }),
            Forwarded::Dropped(DropReason::AbsoluteMotion)
        );
        assert_eq!(f.dropped(), 1);
    }

    #[test]
    fn the_peer_position_is_clamped_to_its_screen() {
        // A fast flick must not ask the peer to place its cursor off-screen.
        let mut f = forwarder();
        let out = f.translate(CapturedInput::PointerMotion {
            dx: 9999.0,
            dy: 9999.0,
        });
        assert_eq!(
            out,
            Forwarded::Send(IpcRequest::InjectMouseMove {
                screen_x: 1920,
                screen_y: 1080,
                virtual_screen: vs(),
            })
        );
    }
}
