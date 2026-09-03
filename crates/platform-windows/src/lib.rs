//! `ultidesk-platform-windows` — Win32 window enumeration and input injection.
//!
//! This crate is the *only* place Win32 is called for the projection slice. Higher
//! layers depend on the plain data types here ([`WindowInfo`], [`InputError`]), never
//! on `HWND`/`windows` directly, so a Linux/macOS backend can later satisfy the same
//! shapes.
//!
//! # Security boundary (brief §11)
//! Input injection uses `SendInput`, which is subject to UIPI: a normal-integrity
//! Ultidesk process **cannot** inject into a higher-integrity (elevated) window. We do
//! not attempt to bypass this. [`inject::inject_events`] surfaces the OS failure as
//! [`InputError::Blocked`] so the projection layer can report "target is elevated"
//! rather than silently dropping input. We never touch the Secure Desktop.

mod types;
pub use types::{RectPx, WindowInfo};

pub mod cursor;
pub mod hook;
pub mod hotkey;
pub mod inject;

#[cfg(windows)]
mod windows_impl;

/// Enumerate capturable top-level windows (visible, titled, not cloaked, excluding
/// obvious tool/shell windows and Ultidesk's own windows). Ordering is z-order as
/// reported by the OS.
///
/// On non-Windows targets this returns an empty list — the crate exists so the
/// workspace builds everywhere, but the capability is Windows-only for now.
pub fn enumerate_top_level_windows() -> Vec<WindowInfo> {
    #[cfg(windows)]
    {
        windows_impl::enumerate()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}
