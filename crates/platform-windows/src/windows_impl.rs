//! Win32 top-level window enumeration.
//!
//! Applies the "alt-tab list" heuristic (visible, unowned, not a tool window, not DWM
//! cloaked, has a title) to approximate the set of user-facing top-level windows a
//! user would sensibly project. This is intentionally conservative; the brief requires
//! excluding hidden/secure/shell surfaces and Ultidesk's own UI (the latter is done by
//! the picker via source-process id, since the agent enumerates from a separate
//! process).

use crate::types::{RectPx, WindowInfo};
use std::ffi::c_void;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, GWL_EXSTYLE, GW_OWNER, WS_EX_TOOLWINDOW,
};

pub fn enumerate() -> Vec<WindowInfo> {
    let mut out: Vec<WindowInfo> = Vec::new();
    // SAFETY: `out` outlives the EnumWindows call; the callback only touches it.
    let ptr = &mut out as *mut Vec<WindowInfo> as isize;
    unsafe {
        // EnumWindows returns Err if the callback ever returned FALSE; we always
        // return TRUE, so a real error here means the OS call itself failed.
        let _ = EnumWindows(Some(enum_cb), LPARAM(ptr));
    }
    out
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let out = &mut *(lparam.0 as *mut Vec<WindowInfo>);
    if let Some(info) = describe_window(hwnd) {
        out.push(info);
    }
    TRUE // keep enumerating regardless of individual-window filtering
}

unsafe fn describe_window(hwnd: HWND) -> Option<WindowInfo> {
    if !IsWindowVisible(hwnd).as_bool() {
        return None;
    }
    // Owned windows (dialogs/tool windows owned by another) are tracked as part of a
    // window family later, not offered as independent picker entries.
    if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
        if !owner.0.is_null() {
            return None;
        }
    }
    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
        return None;
    }
    // Skip DWM-cloaked windows (e.g. suspended UWP apps, virtual-desktop-hidden).
    let mut cloaked: u32 = 0;
    let _ = DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        &mut cloaked as *mut u32 as *mut c_void,
        std::mem::size_of::<u32>() as u32,
    );
    if cloaked != 0 {
        return None;
    }
    let title = window_title(hwnd);
    if title.is_empty() {
        return None;
    }
    let mut rect = RECT::default();
    if GetWindowRect(hwnd, &mut rect).is_err() {
        return None;
    }
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));

    Some(WindowInfo {
        hwnd: hwnd.0 as isize as i64,
        title,
        process_id: pid,
        rect: RectPx {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        },
    })
}

unsafe fn window_title(hwnd: HWND) -> String {
    let len = GetWindowTextLengthW(hwnd);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; (len + 1) as usize];
    let copied = GetWindowTextW(hwnd, &mut buf);
    if copied <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..copied as usize])
}
