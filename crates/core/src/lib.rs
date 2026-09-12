//! `ultidesk-core` — shared, platform-independent types and pure logic for Ultidesk.
//!
//! This crate intentionally contains **no** OS calls, no networking, and no I/O.
//! Everything here is deterministic and unit-testable so that the riskiest control
//! logic (projection lifecycle, input loop prevention) can be verified without a
//! second machine, a GUI, or elevated permissions.
//!
//! The single concession is [`paths`], which reads process environment variables to
//! locate the per-user configuration directory. It lives here because every process
//! must resolve the same directory, and the resolution itself is a pure function over
//! an explicit struct.
//!
//! Platform code lives in `ultidesk-platform-*`; coordinate/topology math lives in
//! `ultidesk-topology`; process orchestration lives in `ultidesk-agent`.

pub mod error;
pub mod ids;
pub mod input_guard;
pub mod keymap;
pub mod kvm;
pub mod paths;
pub mod projection;
pub mod protocol;
pub mod scroll;

pub use error::{CoreError, Result};
pub use ids::{DeviceId, EventId, LeaseId, ProjectionId, SessionId};
pub use paths::{config_dir, resolve_config_dir, runtime_dir, ConfigEnv, RuntimeEnv};
pub use projection::{ProjectionEvent, ProjectionState, ProjectionStateMachine, TransitionError};
pub use scroll::{ScrollAccumulator, WheelDelta, PIXELS_PER_NOTCH, WHEEL_DELTA};
