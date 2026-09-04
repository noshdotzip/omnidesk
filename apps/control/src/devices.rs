//! Reading this machine's real audio devices into the routing model.
//!
//! # Only the local machine is real
//! The control app runs on one machine and can enumerate that machine's endpoints
//! directly. The *other* machine's devices have to arrive over the peer link, and the
//! topology/settings IPC surface for that does not exist yet (ADR-0004 warns against
//! inventing a second protocol here). So the remote side is a labelled placeholder
//! rather than a silent fake — an operator must be able to tell which half of the panel
//! reflects reality.

use ultidesk_core::DeviceId;
use ultidesk_topology::{AudioDevice, DeviceKind};

/// One machine's audio endpoints, plus whether they were actually read from hardware.
pub struct MachineAudio {
    pub device_id: DeviceId,
    pub label: String,
    pub devices: Vec<AudioDevice>,
    /// `None` when enumeration succeeded; a message when it did not, or when this is
    /// the placeholder for a machine we cannot yet query.
    pub note: Option<String>,
}

/// Enumerate the machine this app is running on.
pub fn local(device_id: DeviceId, label: &str) -> MachineAudio {
    match enumerate_local(device_id) {
        Ok(devices) if devices.is_empty() => MachineAudio {
            device_id,
            label: label.to_string(),
            devices,
            // Not an error: a machine really can have no active endpoints. Saying so
            // beats an empty list that looks like a failure.
            note: Some("no active audio endpoints on this machine".into()),
        },
        Ok(devices) => MachineAudio {
            device_id,
            label: label.to_string(),
            devices,
            note: None,
        },
        Err(e) => MachineAudio {
            device_id,
            label: label.to_string(),
            devices: Vec::new(),
            note: Some(e),
        },
    }
}

/// A stand-in for a machine whose devices we cannot read yet.
pub fn remote_placeholder(device_id: DeviceId, label: &str) -> MachineAudio {
    MachineAudio {
        device_id,
        label: label.to_string(),
        devices: Vec::new(),
        note: Some(
            "not connected — a peer's devices arrive over the settings IPC, which is not \
             implemented yet"
                .into(),
        ),
    }
}

#[cfg(target_os = "linux")]
fn enumerate_local(device_id: DeviceId) -> Result<Vec<AudioDevice>, String> {
    use ultidesk_platform_linux::audio_devices::{enumerate, PwKind};
    let found = enumerate().map_err(|e| e.to_string())?;
    Ok(found
        .into_iter()
        .map(|d| AudioDevice {
            device_id,
            node: d.node,
            name: d.description,
            kind: match d.kind {
                PwKind::Sink => DeviceKind::Output,
                PwKind::Source => DeviceKind::Input,
            },
            is_default: d.is_default,
        })
        .collect())
}

#[cfg(windows)]
fn enumerate_local(device_id: DeviceId) -> Result<Vec<AudioDevice>, String> {
    use ultidesk_platform_windows::audio_devices::{enumerate, EndpointKind};
    let found = enumerate().map_err(|e| e.to_string())?;
    Ok(found
        .into_iter()
        .map(|d| AudioDevice {
            device_id,
            node: d.id,
            name: d.description,
            kind: match d.kind {
                EndpointKind::Render => DeviceKind::Output,
                EndpointKind::Capture => DeviceKind::Input,
            },
            is_default: d.is_default,
        })
        .collect())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn enumerate_local(_device_id: DeviceId) -> Result<Vec<AudioDevice>, String> {
    Err("audio device enumeration is not implemented for this platform".into())
}
