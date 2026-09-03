//! `org.freedesktop.portal.RemoteDesktop` — injecting input into this machine.
//!
//! This is the **destination** half of a KVM link: events arriving from a peer are
//! replayed into the local Wayland session here. The source half (grabbing input at a
//! screen edge) is [`crate::input_capture`], a different portal.
//!
//! # Lifecycle
//! `CreateSession` -> `SelectDevices` -> `Start`. Only `Start` shows the user a
//! dialog, and until they accept it no injection is possible. There is no way to skip
//! that dialog and none is attempted (ADR-0006).
//!
//! # Persistence
//! `SelectDevices` asks for `persist_mode = 2` (persistent). On success the portal
//! returns a `restore_token`; passing it back on a later run lets the compositor
//! re-grant without prompting. Callers should store it and hand it to
//! [`SessionOptions::restore_token`] next time — otherwise a KVM prompts on every
//! launch, which users reasonably read as broken.
//!
//! # Coordinates
//! [`RemoteDesktopSession::pointer_motion`] is **relative**. Absolute positioning needs
//! a `stream` id from an associated ScreenCast session, which a pure-input KVM does not
//! have, so relative motion is the primitive that works standalone.

use crate::caps::DeviceTypes;
use crate::portal::PortalError;

/// evdev button codes, from `linux/input-event-codes.h`. The portal speaks evdev, not
/// X11 button numbers — using X11's 1/2/3 here would inject the wrong buttons.
pub mod evdev {
    pub const BTN_LEFT: i32 = 0x110;
    pub const BTN_RIGHT: i32 = 0x111;
    pub const BTN_MIDDLE: i32 = 0x112;
}

/// Which mouse button to act on. Mirrors the Windows backend's enum so the projection
/// layer can stay platform-neutral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    /// Map to the evdev code the portal expects.
    pub fn to_evdev(self) -> i32 {
        match self {
            MouseButton::Left => evdev::BTN_LEFT,
            MouseButton::Right => evdev::BTN_RIGHT,
            MouseButton::Middle => evdev::BTN_MIDDLE,
        }
    }
}

/// Portal key/button state values: 0 released, 1 pressed.
pub fn state_bit(pressed: bool) -> u32 {
    u32::from(pressed)
}

/// How to open a RemoteDesktop session.
#[derive(Debug, Clone, Default)]
pub struct SessionOptions {
    /// Which device classes to request. Defaults to keyboard + pointer.
    pub devices: Option<DeviceTypes>,
    /// A `restore_token` from a previous session, to avoid re-prompting the user.
    pub restore_token: Option<String>,
}

impl SessionOptions {
    /// The device mask to send, defaulting to a keyboard-and-pointer KVM.
    pub fn device_bits(&self) -> u32 {
        self.devices
            .unwrap_or(DeviceTypes {
                keyboard: true,
                pointer: true,
                touchscreen: false,
            })
            .to_bits()
    }
}

#[cfg(target_os = "linux")]
pub use imp::RemoteDesktopSession;

#[cfg(not(target_os = "linux"))]
/// Stub so the type name resolves on non-Linux; every constructor fails.
pub struct RemoteDesktopSession;

#[cfg(not(target_os = "linux"))]
impl RemoteDesktopSession {
    pub fn open(_options: SessionOptions) -> Result<Self, PortalError> {
        Err(PortalError::Unsupported)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use crate::portal_call::{
        bus, call_and_await, close_session, session_handle, PORTAL_PATH, PORTAL_SERVICE,
    };
    use crate::request::sanitize_token;
    use std::collections::HashMap;
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{OwnedObjectPath, Value};

    const REMOTE_DESKTOP: &str = "org.freedesktop.portal.RemoteDesktop";

    /// A live, user-approved RemoteDesktop session.
    pub struct RemoteDesktopSession {
        conn: Connection,
        session: OwnedObjectPath,
        restore_token: Option<String>,
        // Atomic, not Cell: the session is shared across connection tasks by the
        // peer transport, so it must be Sync.
        counter: std::sync::atomic::AtomicU64,
    }

    impl RemoteDesktopSession {
        fn next_token(&self, prefix: &str) -> String {
            let n = self
                .counter
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            sanitize_token(&format!("ultidesk_{prefix}_{n}"))
        }

        fn portal(&self) -> Result<Proxy<'_>, PortalError> {
            Proxy::new(&self.conn, PORTAL_SERVICE, PORTAL_PATH, REMOTE_DESKTOP).map_err(bus)
        }

        /// Open a session: create, select devices, then start (which prompts the user).
        pub fn open(options: SessionOptions) -> Result<Self, PortalError> {
            let conn = Connection::session().map_err(|e| PortalError::Connect(e.to_string()))?;

            // 1. CreateSession
            let handle_token = sanitize_token("ultidesk_create_0");
            let session_token = sanitize_token("ultidesk_session_0");
            let mut opts: HashMap<&str, Value> = HashMap::new();
            opts.insert("handle_token", Value::from(handle_token.as_str()));
            opts.insert("session_handle_token", Value::from(session_token.as_str()));
            tracing::info!("portal step 1/3: CreateSession (no dialog expected)");
            let results = call_and_await(
                &conn,
                REMOTE_DESKTOP,
                "CreateSession",
                &handle_token,
                &(opts,),
            )?;

            let session = session_handle(&results, "CreateSession")?;

            let me = RemoteDesktopSession {
                conn,
                session,
                restore_token: None,
                counter: std::sync::atomic::AtomicU64::new(1),
            };

            // 2. SelectDevices
            let tok = me.next_token("devices");
            let mut opts: HashMap<&str, Value> = HashMap::new();
            opts.insert("handle_token", Value::from(tok.as_str()));
            opts.insert("types", Value::U32(options.device_bits()));
            // Ask for a persistent grant so a KVM does not prompt on every launch.
            opts.insert("persist_mode", Value::U32(2));
            if let Some(rt) = options.restore_token.as_deref() {
                opts.insert("restore_token", Value::from(rt));
            }
            tracing::info!(
                device_bits = options.device_bits(),
                "portal step 2/3: SelectDevices (no dialog expected)"
            );
            call_and_await(
                &me.conn,
                REMOTE_DESKTOP,
                "SelectDevices",
                &tok,
                &(me.session.clone(), opts),
            )?;

            // 3. Start — the step the user sees and must approve. Everything past this
            // point waits on a human; an apparent hang here is an unanswered dialog.
            let tok = me.next_token("start");
            let mut opts: HashMap<&str, Value> = HashMap::new();
            opts.insert("handle_token", Value::from(tok.as_str()));
            tracing::info!(
                "portal step 3/3: Start — RAISES THE PERMISSION DIALOG, waiting for the user"
            );
            let results = call_and_await(
                &me.conn,
                REMOTE_DESKTOP,
                "Start",
                &tok,
                &(me.session.clone(), "", opts),
            )?;
            tracing::info!("portal: Start returned — permission granted");

            let restore_token = results
                .get("restore_token")
                .and_then(|v| String::try_from(v.clone()).ok());

            Ok(RemoteDesktopSession {
                restore_token,
                ..me
            })
        }

        /// The token to persist and pass back next launch to avoid re-prompting.
        pub fn restore_token(&self) -> Option<&str> {
            self.restore_token.as_deref()
        }

        /// Move the pointer by a relative delta.
        pub fn pointer_motion(&self, dx: f64, dy: f64) -> Result<(), PortalError> {
            let opts: HashMap<&str, Value> = HashMap::new();
            self.portal()?
                .call_method("NotifyPointerMotion", &(self.session.clone(), opts, dx, dy))
                .map_err(bus)?;
            Ok(())
        }

        pub fn pointer_button(
            &self,
            button: MouseButton,
            pressed: bool,
        ) -> Result<(), PortalError> {
            let opts: HashMap<&str, Value> = HashMap::new();
            self.portal()?
                .call_method(
                    "NotifyPointerButton",
                    &(
                        self.session.clone(),
                        opts,
                        button.to_evdev(),
                        state_bit(pressed),
                    ),
                )
                .map_err(bus)?;
            Ok(())
        }

        /// Press or release a key by **evdev** keycode.
        pub fn key(&self, evdev_keycode: i32, pressed: bool) -> Result<(), PortalError> {
            let opts: HashMap<&str, Value> = HashMap::new();
            self.portal()?
                .call_method(
                    "NotifyKeyboardKeycode",
                    &(
                        self.session.clone(),
                        opts,
                        evdev_keycode,
                        state_bit(pressed),
                    ),
                )
                .map_err(bus)?;
            Ok(())
        }

        pub fn close(&self) -> Result<(), PortalError> {
            close_session(&self.conn, &self.session)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_map_to_evdev_not_x11_numbering() {
        // The bug this guards: sending X11's 1/2/3, which evdev reads as unrelated keys.
        assert_eq!(MouseButton::Left.to_evdev(), 0x110);
        assert_eq!(MouseButton::Right.to_evdev(), 0x111);
        assert_eq!(MouseButton::Middle.to_evdev(), 0x112);
        assert_ne!(MouseButton::Left.to_evdev(), 1);
    }

    #[test]
    fn state_bit_is_one_for_pressed_zero_for_released() {
        assert_eq!(state_bit(true), 1);
        assert_eq!(state_bit(false), 0);
    }

    #[test]
    fn default_session_requests_keyboard_and_pointer_but_not_touch() {
        let bits = SessionOptions::default().device_bits();
        let d = DeviceTypes::from_bits(bits);
        assert!(d.keyboard && d.pointer);
        assert!(!d.touchscreen, "do not request devices we do not use");
        assert!(d.supports_keyboard_and_pointer());
    }

    #[test]
    fn explicit_device_selection_is_honoured() {
        let opts = SessionOptions {
            devices: Some(DeviceTypes {
                keyboard: false,
                pointer: true,
                touchscreen: false,
            }),
            restore_token: None,
        };
        assert_eq!(opts.device_bits(), DeviceTypes::POINTER);
    }
}
