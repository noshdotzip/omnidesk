//! Platform-independent data types describing enumerated windows.

use serde::{Deserialize, Serialize};

/// A pixel rectangle in screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RectPx {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl RectPx {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

/// A top-level window that could be selected for projection.
///
/// `hwnd` is stored as `i64` (never exposed as a raw pointer type across the IPC/wire
/// boundary) and is only meaningful on its source device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Opaque native handle, valid only on the source device.
    pub hwnd: i64,
    /// Window title. Treated as untrusted display text everywhere downstream and
    /// kept out of routine logs (brief: do not log window titles).
    pub title: String,
    /// Owning process id (source device only).
    pub process_id: u32,
    /// Screen rectangle at enumeration time.
    pub rect: RectPx,
}
