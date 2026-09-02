//! Monitor / topology records. These mirror the fields listed in the brief §10 and
//! are what the topology editor persists and what edge-crossing consults.

use serde::{Deserialize, Serialize};
use ultidesk_core::DeviceId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MonitorId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rotation {
    Landscape,
    Portrait,
    LandscapeFlipped,
    PortraitFlipped,
}

/// One monitor on one device, expressed in the shared *logical* desktop coordinate
/// space used by the topology editor. `scale_factor` and native pixels are retained
/// so mixed-DPI pointer mapping can convert precisely.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    pub device_id: DeviceId,
    pub monitor_id: MonitorId,
    pub friendly_name: String,
    pub logical_x: f64,
    pub logical_y: f64,
    pub logical_width: f64,
    pub logical_height: f64,
    pub native_pixel_width: u32,
    pub native_pixel_height: u32,
    pub scale_factor: f64,
    pub rotation: Rotation,
    pub refresh_rate: f32,
    pub primary: bool,
}

impl Monitor {
    /// Right edge x in logical coords.
    pub fn right(&self) -> f64 {
        self.logical_x + self.logical_width
    }
    /// Bottom edge y in logical coords.
    pub fn bottom(&self) -> f64 {
        self.logical_y + self.logical_height
    }
}
