//! `ultidesk-topology` — monitor layout types and the coordinate math shared by the
//! KVM (edge crossing) and Window Projection (letterbox pointer mapping) subsystems.
//!
//! All math is pure `f64` and unit-tested. Getting this wrong is one of the top
//! sources of "clicks land in the wrong place" bugs, so it lives in one audited place
//! rather than being re-derived in the renderer and the agent.

pub mod audio;
pub mod layout;
pub mod mapping;
pub mod monitor;
pub mod remote_pointer;

pub use audio::{AudioDevice, AudioRouting, DeviceKey, DeviceKind, Route, RouteError};
pub use layout::{Adjacency, Layout, Rect, Side, DEFAULT_SNAP};
pub use mapping::{
    letterbox_content_rect, map_edge_crossing, map_proxy_to_source, Edge, NormPoint, Point, Size,
};
pub use monitor::{Monitor, MonitorId, Rotation};
pub use remote_pointer::{PointerUpdate, RemotePointer};
