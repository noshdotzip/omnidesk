//! Core error type. Deliberately small; subsystems wrap this with their own context.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("illegal projection transition: {0}")]
    Transition(#[from] crate::projection::TransitionError),

    #[error("input rejected by loop guard: {0}")]
    LoopGuard(String),

    #[error("protocol version mismatch: local={local}, peer={peer}")]
    ProtocolVersion { local: u32, peer: u32 },

    #[error("invalid identifier: {0}")]
    InvalidId(String),
}
