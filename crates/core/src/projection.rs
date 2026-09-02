//! The projection lifecycle state machine.
//!
//! Every projection — a source window streamed to one destination proxy — moves
//! through this machine. It is the single source of truth for:
//!
//! * which transitions are legal (illegal ones are rejected, never silently applied);
//! * when destination input may be forwarded to the source (only in [`RemoteActive`]);
//! * that **every** failure path (`Disconnect`, `Fault`, `FirstFrameTimeout`,
//!   `NegotiationFailed`) leaves the projection in a state whose documented cleanup
//!   returns control to the local machine and releases held input.
//!
//! Mirrors the state list in the implementation brief §14. Keep this in lockstep
//! with the TypeScript mirror in `apps/desktop/src/projection/state.ts`.
//!
//! [`RemoteActive`]: ProjectionState::RemoteActive

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Projection lifecycle states. `Error` and `Closed` are terminal-ish sinks:
/// `Error` is recoverable only by `Close`; `Closed` is fully terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectionState {
    /// Window is fully local; no projection in flight.
    Local,
    /// User is choosing a source window.
    Selecting,
    /// Checking destination permission before spending any capture resources.
    Authorizing,
    /// Capture backend is starting on the source.
    CaptureStarting,
    /// Media (WebRTC) is being negotiated with the destination.
    Negotiating,
    /// Waiting for the destination to report a decoded first frame.
    WaitingForFirstFrame,
    /// Live: destination proxy is showing frames and may forward input.
    RemoteActive,
    /// A handoff (this destination → another destination) is being pre-negotiated.
    HandoffPreparing,
    /// The handoff is committing; the old proxy stays up until the new one confirms.
    HandoffCommitting,
    /// Returning the window to its source and ending projection.
    Returning,
    /// Temporarily suspended (e.g. source locked); recoverable via `Resume`.
    Suspended,
    /// A fault occurred. Cleanup has run / must run; only `Close` leaves this state.
    Error,
    /// Fully closed and cleaned up.
    Closed,
}

/// Triggers that drive the projection machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionEvent {
    Select,
    AuthorizeGranted,
    AuthorizeDenied,
    CaptureStarted,
    CaptureFailed,
    NegotiationComplete,
    NegotiationFailed,
    FirstFrame,
    FirstFrameTimeout,
    HandoffRequested,
    HandoffCommitted,
    HandoffAborted,
    ReturnRequested,
    ReturnComplete,
    Suspend,
    Resume,
    /// Network/peer loss. The source application stays locally recoverable.
    Disconnect,
    /// Any other fault, carrying a short non-sensitive reason for diagnostics.
    Fault,
    /// User (or peer, with permission) closed the projection.
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("illegal transition: {state:?} cannot handle {event:?}")]
pub struct TransitionError {
    pub state: ProjectionState,
    pub event: ProjectionEvent,
}

/// A projection state machine instance.
#[derive(Debug, Clone)]
pub struct ProjectionStateMachine {
    state: ProjectionState,
}

impl Default for ProjectionStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl ProjectionStateMachine {
    pub fn new() -> Self {
        Self {
            state: ProjectionState::Local,
        }
    }

    pub fn state(&self) -> ProjectionState {
        self.state
    }

    /// Destination input may only be forwarded to the source while a projection is
    /// truly live. This is the single gate the input router consults so that a proxy
    /// can never drive the source during setup, handoff, suspension, or teardown.
    pub fn can_forward_input(&self) -> bool {
        matches!(self.state, ProjectionState::RemoteActive)
    }

    /// Whether a media stream is (or should be) flowing in this state. Handoff keeps
    /// the *old* stream up, so those states count too.
    pub fn media_active(&self) -> bool {
        matches!(
            self.state,
            ProjectionState::RemoteActive
                | ProjectionState::HandoffPreparing
                | ProjectionState::HandoffCommitting
        )
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self.state, ProjectionState::Closed)
    }

    /// Apply an event. Returns the new state, or a [`TransitionError`] if the event is
    /// illegal from the current state (the state is left unchanged on error).
    pub fn on(&mut self, event: ProjectionEvent) -> Result<ProjectionState, TransitionError> {
        use ProjectionEvent as E;
        use ProjectionState as S;

        // `Close` is always legal and always wins — an operator/emergency path must be
        // able to tear a projection down from any state.
        if event == E::Close {
            self.state = S::Closed;
            return Ok(self.state);
        }

        // From any non-terminal, non-error state, an unexpected loss faults safely.
        // Modeled explicitly per-state below for the states where it matters; here we
        // only special-case the terminal sinks so nothing "revives" a closed machine.
        if self.state == S::Closed {
            return Err(TransitionError {
                state: self.state,
                event,
            });
        }

        let next = match (self.state, &event) {
            // Happy path setup.
            (S::Local, E::Select) => S::Selecting,
            (S::Selecting, E::AuthorizeGranted) => S::Authorizing,
            (S::Selecting, E::Disconnect) | (S::Selecting, E::Fault) => S::Error,
            (S::Authorizing, E::AuthorizeGranted) => S::CaptureStarting,
            (S::Authorizing, E::AuthorizeDenied) => S::Error,
            (S::Authorizing, E::Disconnect) | (S::Authorizing, E::Fault) => S::Error,
            (S::CaptureStarting, E::CaptureStarted) => S::Negotiating,
            (S::CaptureStarting, E::CaptureFailed) => S::Error,
            (S::CaptureStarting, E::Disconnect) | (S::CaptureStarting, E::Fault) => S::Error,
            (S::Negotiating, E::NegotiationComplete) => S::WaitingForFirstFrame,
            (S::Negotiating, E::NegotiationFailed) => S::Error,
            (S::Negotiating, E::Disconnect) | (S::Negotiating, E::Fault) => S::Error,
            (S::WaitingForFirstFrame, E::FirstFrame) => S::RemoteActive,
            (S::WaitingForFirstFrame, E::FirstFrameTimeout) => S::Error,
            (S::WaitingForFirstFrame, E::Disconnect) | (S::WaitingForFirstFrame, E::Fault) => {
                S::Error
            }

            // Live.
            (S::RemoteActive, E::HandoffRequested) => S::HandoffPreparing,
            (S::RemoteActive, E::ReturnRequested) => S::Returning,
            (S::RemoteActive, E::Suspend) => S::Suspended,
            (S::RemoteActive, E::Disconnect) | (S::RemoteActive, E::Fault) => S::Error,

            // Handoff (B→C while process stays on A). Old stream stays up until commit.
            (S::HandoffPreparing, E::NegotiationComplete) => S::HandoffCommitting,
            (S::HandoffPreparing, E::HandoffAborted) => S::RemoteActive,
            (S::HandoffPreparing, E::NegotiationFailed) => S::RemoteActive,
            (S::HandoffPreparing, E::Disconnect) | (S::HandoffPreparing, E::Fault) => S::Error,
            (S::HandoffCommitting, E::HandoffCommitted) => S::RemoteActive,
            (S::HandoffCommitting, E::HandoffAborted) => S::RemoteActive,
            (S::HandoffCommitting, E::Disconnect) | (S::HandoffCommitting, E::Fault) => S::Error,

            // Suspend / resume.
            (S::Suspended, E::Resume) => S::RemoteActive,
            (S::Suspended, E::ReturnRequested) => S::Returning,
            (S::Suspended, E::Disconnect) | (S::Suspended, E::Fault) => S::Error,

            // Returning the window to its source.
            (S::Returning, E::ReturnComplete) => S::Local,
            (S::Returning, E::Disconnect) | (S::Returning, E::Fault) => S::Error,

            // Error is a holding pen; only Close (handled above) leaves it.
            (S::Error, _) => {
                return Err(TransitionError {
                    state: self.state,
                    event,
                })
            }

            _ => {
                return Err(TransitionError {
                    state: self.state,
                    event,
                })
            }
        };

        self.state = next;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::ProjectionEvent as E;
    use super::ProjectionState as S;
    use super::*;

    fn drive(events: &[E]) -> ProjectionStateMachine {
        let mut m = ProjectionStateMachine::new();
        for e in events {
            m.on(e.clone()).expect("expected legal transition");
        }
        m
    }

    #[test]
    fn happy_path_reaches_remote_active() {
        let m = drive(&[
            E::Select,
            E::AuthorizeGranted, // Selecting -> Authorizing
            E::AuthorizeGranted, // Authorizing -> CaptureStarting
            E::CaptureStarted,
            E::NegotiationComplete,
            E::FirstFrame,
        ]);
        assert_eq!(m.state(), S::RemoteActive);
        assert!(m.can_forward_input());
        assert!(m.media_active());
    }

    #[test]
    fn input_forwarding_gated_to_remote_active_only() {
        let mut m = ProjectionStateMachine::new();
        assert!(!m.can_forward_input()); // Local
        m.on(E::Select).unwrap();
        assert!(!m.can_forward_input()); // Selecting
                                         // Reach RemoteActive then suspend — forwarding must stop.
        let mut m = drive(&[
            E::Select,
            E::AuthorizeGranted,
            E::AuthorizeGranted,
            E::CaptureStarted,
            E::NegotiationComplete,
            E::FirstFrame,
        ]);
        assert!(m.can_forward_input());
        m.on(E::Suspend).unwrap();
        assert!(
            !m.can_forward_input(),
            "suspended proxy must not forward input"
        );
    }

    #[test]
    fn first_frame_timeout_rolls_back_to_error() {
        let mut m = drive(&[
            E::Select,
            E::AuthorizeGranted,
            E::AuthorizeGranted,
            E::CaptureStarted,
            E::NegotiationComplete,
        ]);
        assert_eq!(m.state(), S::WaitingForFirstFrame);
        assert_eq!(m.on(E::FirstFrameTimeout).unwrap(), S::Error);
        assert!(!m.can_forward_input());
    }

    #[test]
    fn disconnect_from_live_faults_and_stops_input() {
        let mut m = drive(&[
            E::Select,
            E::AuthorizeGranted,
            E::AuthorizeGranted,
            E::CaptureStarted,
            E::NegotiationComplete,
            E::FirstFrame,
        ]);
        assert_eq!(m.on(E::Disconnect).unwrap(), S::Error);
        assert!(
            !m.can_forward_input(),
            "no input forwarding after disconnect"
        );
    }

    #[test]
    fn failed_handoff_keeps_old_projection_live() {
        let mut m = drive(&[
            E::Select,
            E::AuthorizeGranted,
            E::AuthorizeGranted,
            E::CaptureStarted,
            E::NegotiationComplete,
            E::FirstFrame,
            E::HandoffRequested,
        ]);
        assert_eq!(m.state(), S::HandoffPreparing);
        // Negotiation with C fails -> B stays active on A's stream.
        assert_eq!(m.on(E::NegotiationFailed).unwrap(), S::RemoteActive);
        assert!(m.can_forward_input());
    }

    #[test]
    fn successful_handoff_returns_to_remote_active() {
        let m = drive(&[
            E::Select,
            E::AuthorizeGranted,
            E::AuthorizeGranted,
            E::CaptureStarted,
            E::NegotiationComplete,
            E::FirstFrame,
            E::HandoffRequested,
            E::NegotiationComplete, // HandoffPreparing -> HandoffCommitting
            E::HandoffCommitted,
        ]);
        assert_eq!(m.state(), S::RemoteActive);
    }

    #[test]
    fn return_to_source_ends_at_local() {
        let mut m = drive(&[
            E::Select,
            E::AuthorizeGranted,
            E::AuthorizeGranted,
            E::CaptureStarted,
            E::NegotiationComplete,
            E::FirstFrame,
            E::ReturnRequested,
        ]);
        assert_eq!(m.on(E::ReturnComplete).unwrap(), S::Local);
    }

    #[test]
    fn close_is_legal_from_every_state() {
        for setup in [
            vec![],
            vec![E::Select],
            vec![E::Select, E::AuthorizeGranted],
            vec![
                E::Select,
                E::AuthorizeGranted,
                E::AuthorizeGranted,
                E::CaptureStarted,
                E::NegotiationComplete,
                E::FirstFrame,
            ],
        ] {
            let mut m = drive(&setup);
            assert_eq!(m.on(E::Close).unwrap(), S::Closed);
            assert!(m.is_terminal());
        }
    }

    #[test]
    fn illegal_transitions_are_rejected_and_leave_state_unchanged() {
        let mut m = ProjectionStateMachine::new();
        // Cannot receive a first frame while still Local.
        let err = m.on(E::FirstFrame).unwrap_err();
        assert_eq!(err.state, S::Local);
        assert_eq!(
            m.state(),
            S::Local,
            "state must not change on illegal event"
        );
    }

    #[test]
    fn closed_machine_rejects_further_events() {
        let mut m = ProjectionStateMachine::new();
        m.on(E::Close).unwrap();
        assert!(m.on(E::Select).is_err());
        assert!(m.on(E::Resume).is_err());
    }

    #[test]
    fn error_state_only_exits_via_close() {
        let mut m = drive(&[E::Select]);
        m.on(E::Fault).unwrap();
        assert_eq!(m.state(), S::Error);
        assert!(m.on(E::Resume).is_err());
        assert!(m.on(E::Select).is_err());
        assert_eq!(m.on(E::Close).unwrap(), S::Closed);
    }
}
