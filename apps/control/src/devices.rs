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
use ultidesk_topology::AudioDevice;

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

/// A machine's endpoints as the agent reported them.
///
/// The agent is the source now, for both machines. For the peer it is the only possible
/// source; for this machine it is the *right* one, because what the agent says is what a
/// peer is told, and one source is what stops two from disagreeing.
pub fn from_agent(device_id: DeviceId, label: &str, devices: Vec<AudioDevice>) -> MachineAudio {
    let note = devices
        .is_empty()
        // Not an error: a machine really can have no active endpoints, and saying so
        // beats an empty list that looks like a failure.
        .then(|| "no active audio endpoints on this machine".to_string());
    MachineAudio {
        device_id,
        label: label.to_string(),
        devices,
        note,
    }
}

/// A paired machine that could not be reached just now.
///
/// Distinct from [`remote_placeholder`] on purpose: "not paired" and "paired but asleep"
/// are different situations with different fixes, and an operator should not have to
/// guess which one they are looking at.
pub fn unreachable(device_id: DeviceId, label: &str, why: String) -> MachineAudio {
    MachineAudio {
        device_id,
        label: label.to_string(),
        devices: Vec::new(),
        note: Some(why),
    }
}

/// A stand-in for a machine whose devices we cannot read yet.
pub fn remote_placeholder(device_id: DeviceId, label: &str) -> MachineAudio {
    MachineAudio {
        device_id,
        label: label.to_string(),
        devices: Vec::new(),
        note: Some("no peer is paired with this machine yet".into()),
    }
}

// The mapping from each platform's own endpoint DTO to `AudioDevice` lives in the
// platform crate that owns the DTO. This is only the per-platform selection, and the
// agent makes the same call for the settings IPC — so what a saved route is keyed on
// cannot drift between the editor and the agent that honours it.
#[cfg(target_os = "linux")]
fn enumerate_local(device_id: DeviceId) -> Result<Vec<AudioDevice>, String> {
    ultidesk_platform_linux::audio_devices::enumerate_shared(device_id).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn enumerate_local(device_id: DeviceId) -> Result<Vec<AudioDevice>, String> {
    ultidesk_platform_windows::audio_devices::enumerate_shared(device_id).map_err(|e| e.to_string())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn enumerate_local(_device_id: DeviceId) -> Result<Vec<AudioDevice>, String> {
    Err("audio device enumeration is not implemented for this platform".into())
}
