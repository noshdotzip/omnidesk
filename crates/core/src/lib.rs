//! `ultidesk-core` — shared, platform-independent types and pure logic for Ultidesk.
//!
//! This crate intentionally contains **no** OS calls, no networking, and no I/O.
//! Everything here is deterministic and unit-testable so that the riskiest control
//! logic (projection lifecycle, input loop prevention) can be verified without a
//! second machine, a GUI, or elevated permissions.
//!
//! Platform code lives in `ultidesk-platform-*`; coordinate/topology math lives in
//! `ultidesk-topology`; process orchestration lives in `ultidesk-agent`.

pub mod error;
pub mod ids;
pub mod input_guard;
pub mod kvm;
pub mod projection;
pub mod protocol;

pub use error::{CoreError, Result};
pub use ids::{DeviceId, EventId, LeaseId, ProjectionId, SessionId};
pub use projection::{ProjectionEvent, ProjectionState, ProjectionStateMachine, TransitionError};
