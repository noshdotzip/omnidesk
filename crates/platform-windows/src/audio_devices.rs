//! Enumerate this machine's WASAPI audio endpoints.
//!
//! The counterpart to the Linux PipeWire walk: same shape, so the routing UI can treat
//! both machines the same way.
//!
//! # Identity is the endpoint id, not the friendly name
//! Two identical headsets produce two endpoints with the same friendly name, and a
//! name changes when the user renames the device in Sound settings. The opaque endpoint
//! id (`{0.0.0.00000000}.{guid}`) is what stays stable, so that is what a saved route
//! stores. The friendly name is display only.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Whether an endpoint plays audio or records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EndpointKind {
    /// A render endpoint — speakers or headphones. Capturing one means WASAPI loopback.
    Render,
    /// A capture endpoint — a microphone or line-in.
    Capture,
}

/// One WASAPI audio endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WinAudioDevice {
    /// The endpoint id. Stable across renames and reboots; this is what a route stores.
    pub id: String,
    /// `PKEY_Device_FriendlyName`, for display only.
    pub description: String,
    pub kind: EndpointKind,
    pub is_default: bool,
}

#[derive(Debug, Error)]
pub enum AudioEnumError {
    #[error("this build has no WASAPI support (not a Windows target)")]
    Unsupported,
    #[error("WASAPI enumeration failed: {0}")]
    Wasapi(String),
}

/// Read every active audio endpoint on this machine.
#[cfg(windows)]
pub fn enumerate() -> Result<Vec<WinAudioDevice>, AudioEnumError> {
    imp::enumerate()
}

#[cfg(not(windows))]
pub fn enumerate() -> Result<Vec<WinAudioDevice>, AudioEnumError> {
    Err(AudioEnumError::Unsupported)
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, eRender, EDataFlow, IMMDeviceEnumerator, MMDeviceEnumerator,
        DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
    };

    fn wasapi(e: impl std::fmt::Display) -> AudioEnumError {
        AudioEnumError::Wasapi(e.to_string())
    }

    pub fn enumerate() -> Result<Vec<WinAudioDevice>, AudioEnumError> {
        // Enumeration is cheap and synchronous, so unlike capture it does not need its
        // own thread. COM may already be initialized on this thread with a different
        // model; that returns RPC_E_CHANGED_MODE, which is not fatal for what follows.
        // SAFETY: initializing COM before any COM call on this thread.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

        // SAFETY: standard MMDevice enumeration. Every interface is checked, and the
        // one raw pointer (the endpoint id) is copied into a String and freed below.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(wasapi)?;

            let mut out = Vec::new();
            for (flow, kind) in [
                (eRender, EndpointKind::Render),
                (eCapture, EndpointKind::Capture),
            ] {
                let default_id = default_endpoint_id(&enumerator, flow);
                collect_flow(&enumerator, flow, kind, default_id.as_deref(), &mut out)?;
            }

            // Stable order so the picker does not reshuffle between openings.
            out.sort_by(|a, b| (a.kind, &a.description).cmp(&(b.kind, &b.description)));
            Ok(out)
        }
    }

    /// The default endpoint for one direction, or `None` when the machine has none.
    ///
    /// A machine with no output at all is a normal state (a headless box, or every
    /// device disabled), not an error — so this reports absence rather than failing the
    /// whole enumeration.
    unsafe fn default_endpoint_id(
        enumerator: &IMMDeviceEnumerator,
        flow: EDataFlow,
    ) -> Option<String> {
        let device = enumerator.GetDefaultAudioEndpoint(flow, eConsole).ok()?;
        let raw = device.GetId().ok()?;
        let id = raw.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(raw.0 as *const _));
        id
    }

    unsafe fn collect_flow(
        enumerator: &IMMDeviceEnumerator,
        flow: EDataFlow,
        kind: EndpointKind,
        default_id: Option<&str>,
        out: &mut Vec<WinAudioDevice>,
    ) -> Result<(), AudioEnumError> {
        // DEVICE_STATE_ACTIVE only: unplugged and disabled endpoints still exist in the
        // enumeration, and offering one would produce a route that silently never runs.
        let collection = enumerator
            .EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)
            .map_err(wasapi)?;
        let count = collection.GetCount().map_err(wasapi)?;

        for i in 0..count {
            let Ok(device) = collection.Item(i) else {
                continue;
            };
            let Ok(raw_id) = device.GetId() else {
                continue;
            };
            let id = raw_id.to_string().ok();
            windows::Win32::System::Com::CoTaskMemFree(Some(raw_id.0 as *const _));
            let Some(id) = id else {
                continue;
            };

            // A device with no readable friendly name is still usable, so fall back to
            // its id rather than skipping it — a missing entry is worse than an ugly one.
            let description = device
                .OpenPropertyStore(STGM_READ)
                .ok()
                .and_then(|store| store.GetValue(&PKEY_Device_FriendlyName).ok())
                .map(|v| v.to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| id.clone());

            out.push(WinAudioDevice {
                is_default: default_id == Some(id.as_str()),
                id,
                description,
                kind,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(windows))]
    fn non_windows_reports_unsupported() {
        assert!(matches!(
            enumerate().unwrap_err(),
            AudioEnumError::Unsupported
        ));
    }

    #[test]
    #[cfg(windows)]
    fn every_endpoint_has_an_id_and_a_label() {
        // Runs against the real machine. The invariant that matters for the picker is
        // that nothing comes back unlabelled or unidentified — either would render as a
        // blank row the operator cannot choose meaningfully.
        let devices = enumerate().expect("enumeration should succeed on Windows");
        for d in &devices {
            assert!(!d.id.is_empty(), "endpoint with no id: {d:?}");
            assert!(!d.description.is_empty(), "endpoint with no label: {d:?}");
        }
    }

    #[test]
    #[cfg(windows)]
    fn at_most_one_default_per_direction() {
        // Two defaults would make the UI mark two devices as "default" and the routing
        // panel would pick whichever it saw first.
        let devices = enumerate().expect("enumeration should succeed on Windows");
        for kind in [EndpointKind::Render, EndpointKind::Capture] {
            let defaults = devices
                .iter()
                .filter(|d| d.kind == kind && d.is_default)
                .count();
            assert!(defaults <= 1, "{defaults} defaults for {kind:?}");
        }
    }

    #[test]
    #[cfg(windows)]
    fn endpoint_ids_are_unique() {
        // Routes are keyed by id; a duplicate would make two devices indistinguishable.
        let devices = enumerate().expect("enumeration should succeed on Windows");
        let mut ids: Vec<&str> = devices.iter().map(|d| d.id.as_str()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate endpoint ids in enumeration");
    }
}
