//! libei client — turns an InputCapture session's EIS socket into input events.
//!
//! This is the piece that makes a Linux machine a KVM *source*. The InputCapture portal
//! only arbitrates: it decides when the pointer has hit a barrier and hands over a
//! socket. It does not carry a single input event. Without a libei client the barriers
//! fire, the pointer is captured, and nothing arrives — which looks exactly like a
//! barrier that was never set.
//!
//! Implemented with `reis`, a pure-Rust libei, rather than binding `libei` through FFI:
//! the C library needs `libclang` to generate bindings and pulls a second event loop
//! into the process.
//!
//! # Binding seat capabilities is not optional
//! A seat arrives with capabilities advertised but *unbound*. Until the client calls
//! `bind_capabilities`, the compositor creates no devices, so no pointer or key events
//! are ever sent. Nothing errors — the stream simply stays quiet after `SeatAdded`.
//! This is the single easiest way to get a client that connects, handshakes, reports
//! success and then does nothing.
//!
//! # Receiver, not Sender
//! Two context types share this protocol. `Sender` *injects* events into the
//! compositor (that is the RemoteDesktop-style direction, already covered by
//! `remote_desktop`). `Receiver` consumes events the compositor captured on our behalf,
//! which is what a KVM source needs. Choosing the wrong one hands back a connection
//! that will never deliver anything.

use serde::{Deserialize, Serialize};

/// One input event captured from the local desktop, ready to send to a peer.
///
/// Deliberately not a re-export of `reis`' event type: this crosses the machine
/// boundary, so it has to be serialisable and stable, and it must not carry the device
/// handles and timestamps that only mean something in this process.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum CapturedInput {
    /// Relative pointer motion, in logical pixels.
    PointerMotion { dx: f32, dy: f32 },
    /// Absolute pointer position within the captured device's region.
    PointerMotionAbsolute { x: f32, y: f32 },
    /// A mouse button, by Linux `input-event-codes.h` code (BTN_LEFT is 0x110).
    Button { button: u32, pressed: bool },
    /// Scroll movement, in logical pixels.
    Scroll { dx: f32, dy: f32 },
    /// A key, by **evdev keycode** — not a keysym and not a PS/2 scancode.
    ///
    /// The distinction matters on the receiving end: `keymap` converts these for
    /// Windows, and feeding it a keysym silently types the wrong characters.
    Key { keycode: u32, pressed: bool },
}

#[derive(Debug, thiserror::Error)]
pub enum EiError {
    #[error("this build has no libei support (not a Linux target)")]
    Unsupported,
    #[error("could not open the EIS socket: {0}")]
    Socket(String),
    #[error("libei handshake failed: {0}")]
    Handshake(String),
    #[error("libei stream error: {0}")]
    Stream(String),
}

/// The name this client announces to the compositor. Appears in the desktop's own
/// input-capture indicators, so it should say who is capturing.
pub const EI_CLIENT_NAME: &str = "ultidesk";

#[cfg(target_os = "linux")]
pub use imp::{capture_events, EiSession};

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use futures_util::StreamExt;
    use reis::{ei, event::DeviceCapability, event::EiEvent};
    use std::os::unix::io::OwnedFd;
    use std::os::unix::net::UnixStream;

    /// A live libei receiver attached to an InputCapture session.
    pub struct EiSession {
        context: ei::Context,
    }

    impl EiSession {
        /// Adopt the file descriptor returned by `InputCapture.ConnectToEIS`.
        pub fn from_fd(fd: OwnedFd) -> Result<Self, EiError> {
            let stream = UnixStream::from(fd);
            let context = ei::Context::new(stream).map_err(|e| EiError::Socket(e.to_string()))?;
            Ok(EiSession { context })
        }
    }

    /// Stream captured input to `on_event` until the compositor disconnects.
    ///
    /// Returns when the peer closes the connection or the stream errors. `on_event`
    /// returning `false` stops the loop — that is how a caller releases capture without
    /// having to tear the whole session down.
    pub async fn capture_events<F>(session: EiSession, mut on_event: F) -> Result<(), EiError>
    where
        F: FnMut(CapturedInput) -> bool,
    {
        let (_connection, mut events) = session
            .context
            .handshake_tokio(EI_CLIENT_NAME, ei::handshake::ContextType::Receiver)
            .await
            .map_err(|e| EiError::Handshake(e.to_string()))?;

        while let Some(event) = events.next().await {
            let event = event.map_err(|e| EiError::Stream(e.to_string()))?;
            match event {
                // Without this the compositor creates no devices and the stream goes
                // silent. See the module docs.
                EiEvent::SeatAdded(evt) => {
                    evt.seat.bind_capabilities(
                        DeviceCapability::Pointer
                            | DeviceCapability::PointerAbsolute
                            | DeviceCapability::Keyboard
                            | DeviceCapability::Scroll
                            | DeviceCapability::Button,
                    );
                    // The bind is a request; nothing happens until it is flushed.
                    session
                        .context
                        .flush()
                        .map_err(|e| EiError::Stream(e.to_string()))?;
                    tracing::debug!("bound seat capabilities");
                }
                EiEvent::DeviceAdded(evt) => {
                    tracing::info!(
                        device_type = ?evt.device.device_type(),
                        "libei device added"
                    );
                }
                EiEvent::Disconnected(_) => {
                    tracing::info!("compositor closed the libei connection");
                    return Ok(());
                }
                other => {
                    if let Some(input) = translate(&other) {
                        if !on_event(input) {
                            return Ok(());
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Map a libei event onto the wire type, or `None` for events that carry no input
    /// (frame boundaries, device lifecycle, modifier state).
    fn translate(event: &EiEvent) -> Option<CapturedInput> {
        Some(match event {
            EiEvent::PointerMotion(e) => CapturedInput::PointerMotion { dx: e.dx, dy: e.dy },
            EiEvent::PointerMotionAbsolute(e) => CapturedInput::PointerMotionAbsolute {
                x: e.dx_absolute,
                y: e.dy_absolute,
            },
            EiEvent::Button(e) => CapturedInput::Button {
                button: e.button,
                pressed: e.state == ei::button::ButtonState::Press,
            },
            EiEvent::ScrollDelta(e) => CapturedInput::Scroll { dx: e.dx, dy: e.dy },
            EiEvent::KeyboardKey(e) => CapturedInput::Key {
                keycode: e.key,
                pressed: e.state == ei::keyboard::KeyState::Press,
            },
            _ => return None,
        })
    }
}

// Deliberately no `EiSession` stub on non-Linux targets. Every other module in this
// crate offers one that returns `Unsupported`, but this constructor would have to name
// `OwnedFd`, which does not exist off Unix — a stub would have to invent a signature
// that no caller could satisfy. `CapturedInput` and `EiError` are still compiled
// everywhere so the wire format stays under test on both platforms.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_input_round_trips_over_the_wire() {
        // These cross machines, so a shape change must be caught here rather than by a
        // peer that silently stops moving.
        let events = [
            CapturedInput::PointerMotion { dx: -3.5, dy: 12.0 },
            CapturedInput::PointerMotionAbsolute { x: 0.0, y: 1079.0 },
            CapturedInput::Button {
                button: 0x110,
                pressed: true,
            },
            CapturedInput::Scroll { dx: 0.0, dy: -15.0 },
            CapturedInput::Key {
                keycode: 30,
                pressed: false,
            },
        ];
        for e in events {
            let json = serde_json::to_string(&e).expect("serialise");
            let back: CapturedInput = serde_json::from_str(&json).expect("deserialise");
            assert_eq!(e, back, "round trip changed the event");
        }
    }

    #[test]
    fn button_codes_are_evdev_not_x11() {
        // BTN_LEFT is 0x110 in input-event-codes.h, not 1 as in the X11 numbering.
        // Getting this wrong sends button 1 as "left" and lands a middle-click.
        let e = CapturedInput::Button {
            button: 0x110,
            pressed: true,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("272"), "expected decimal 272 in {json}");
    }
}
