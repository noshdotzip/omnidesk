//! The KVM handoff state machine.
//!
//! Handoff is the dangerous half of a KVM. Mirroring a pointer is harmless; *grabbing*
//! local input means the operator's own keyboard and mouse stop reaching their machine,
//! and every way out of that state has to work. This module owns those transitions so
//! they can be exhaustively tested without a compositor, a peer, or a real input grab.
//!
//! Two invariants are the reason this exists, and both are pinned by tests:
//!
//! 1. **Release is unconditional.** An emergency release, a lost peer, or a peer that
//!    ended the session returns to [`KvmState::Local`] from any state, with no
//!    precondition that could fail.
//! 2. **A confined pointer never crosses.** When an application has locked the cursor
//!    to itself — a first-person game, a CAD viewport, anything using pointer
//!    confinement or raw input — moving to the screen edge is *aiming*, not a request
//!    to switch machines. Handing off there would rip the user out of the game
//!    mid-motion, so crossing is suppressed for as long as the lock is held.
//! 3. **Release actually sticks.** After an emergency release the pointer is usually
//!    still sitting on the crossing edge. Re-arming on edge contact alone would grab
//!    input again on the very next poll, which makes the release button useless
//!    precisely when it is needed. Crossing stays disarmed until the pointer leaves.

use serde::{Deserialize, Serialize};

/// Where input is currently going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KvmState {
    /// Input drives this machine. Nothing is grabbed.
    Local,
    /// Input is grabbed and forwarded to the peer.
    Remote,
}

/// Why control returned to the local machine. Kept for logging and for telling the
/// operator what happened — "the link dropped" and "you pressed the release" deserve
/// different messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReleaseReason {
    EmergencyHotkey,
    PeerLost,
    PeerEnded,
    ReturnedAcrossEdge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KvmEvent {
    /// The pointer reached the edge configured as the crossing point.
    EdgeReached,
    /// The pointer moved off the crossing edge.
    EdgeLeft,
    /// An application locked the pointer to itself, or released it.
    ///
    /// While locked, edge contact is meaningless as a crossing signal: the pointer is
    /// being used to aim, and the OS may be holding it in place regardless of how far
    /// the user moves the mouse.
    PointerConfined(bool),
    /// The operator hit the emergency release hotkey.
    EmergencyRelease,
    /// The peer link dropped unexpectedly.
    PeerLost,
    /// The peer ended the session cleanly.
    PeerEnded,
    /// The peer reports the pointer left its far edge, coming back to us.
    ReturnedAcrossEdge,
}

/// The outcome of feeding an event to the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KvmOutcome {
    /// Nothing changed.
    Unchanged,
    /// Control moved to the peer; the caller should start grabbing input.
    Captured,
    /// Control came back; the caller must stop grabbing and release held keys.
    Released(ReleaseReason),
}

#[derive(Debug, Clone, Copy)]
pub struct KvmMachine {
    state: KvmState,
    /// Whether an application currently owns the pointer.
    confined: bool,
    /// Whether edge contact may start a capture.
    ///
    /// Cleared on release and restored only when the pointer leaves the edge, so a
    /// release cannot be immediately undone by the pointer resting where it already is.
    armed: bool,
}

impl Default for KvmMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl KvmMachine {
    pub fn new() -> Self {
        KvmMachine {
            state: KvmState::Local,
            confined: false,
            // Armed at startup: the pointer has not just been released onto an edge.
            armed: true,
        }
    }

    pub fn state(&self) -> KvmState {
        self.state
    }

    /// Whether the caller should currently be grabbing local input.
    ///
    /// The single source of truth for the grab. Anything that keeps its own copy of
    /// this flag can drift out of sync with the state machine, and drifting *on* is a
    /// locked-out machine.
    pub fn grab_active(&self) -> bool {
        matches!(self.state, KvmState::Remote)
    }

    /// Whether edge contact would currently start a capture.
    pub fn armed(&self) -> bool {
        self.armed
    }

    /// Whether an application currently owns the pointer.
    pub fn pointer_confined(&self) -> bool {
        self.confined
    }

    pub fn on(&mut self, event: KvmEvent) -> KvmOutcome {
        match event {
            // Releases are handled first and identically from any state, so no future
            // edit can accidentally make an escape route conditional.
            KvmEvent::EmergencyRelease => self.release(ReleaseReason::EmergencyHotkey),
            KvmEvent::PeerLost => self.release(ReleaseReason::PeerLost),
            KvmEvent::PeerEnded => self.release(ReleaseReason::PeerEnded),
            KvmEvent::ReturnedAcrossEdge => self.release(ReleaseReason::ReturnedAcrossEdge),

            KvmEvent::EdgeLeft => {
                // Leaving the edge is what re-arms crossing after a release.
                self.armed = true;
                KvmOutcome::Unchanged
            }

            KvmEvent::PointerConfined(confined) => {
                let was = self.confined;
                self.confined = confined;
                // Becoming confined disarms: a game that grabs the pointer while it
                // happens to rest on the edge must not hand off the moment it lets go.
                // Releasing the lock also leaves it disarmed until the pointer moves
                // off the edge, for the same reason as after an emergency release.
                if confined != was {
                    self.armed = false;
                }
                KvmOutcome::Unchanged
            }

            KvmEvent::EdgeReached => {
                if self.state == KvmState::Local && self.armed && !self.confined {
                    self.state = KvmState::Remote;
                    KvmOutcome::Captured
                } else {
                    KvmOutcome::Unchanged
                }
            }
        }
    }

    fn release(&mut self, reason: ReleaseReason) -> KvmOutcome {
        // Disarm regardless of current state: after any release the pointer may be
        // resting on the edge, and re-capturing without it moving away first would
        // defeat the release.
        self.armed = false;
        if self.state == KvmState::Local {
            return KvmOutcome::Unchanged;
        }
        self.state = KvmState::Local;
        KvmOutcome::Released(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured() -> KvmMachine {
        let mut m = KvmMachine::new();
        assert_eq!(m.on(KvmEvent::EdgeReached), KvmOutcome::Captured);
        assert!(m.grab_active());
        m
    }

    #[test]
    fn starts_local_and_ungrabbed() {
        let m = KvmMachine::new();
        assert_eq!(m.state(), KvmState::Local);
        assert!(!m.grab_active());
        assert!(m.armed());
    }

    #[test]
    fn edge_contact_captures() {
        let m = captured();
        assert_eq!(m.state(), KvmState::Remote);
    }

    #[test]
    fn every_release_event_returns_control_from_remote() {
        // Invariant 1: no escape route may be conditional.
        for (event, reason) in [
            (KvmEvent::EmergencyRelease, ReleaseReason::EmergencyHotkey),
            (KvmEvent::PeerLost, ReleaseReason::PeerLost),
            (KvmEvent::PeerEnded, ReleaseReason::PeerEnded),
            (
                KvmEvent::ReturnedAcrossEdge,
                ReleaseReason::ReturnedAcrossEdge,
            ),
        ] {
            let mut m = captured();
            assert_eq!(m.on(event), KvmOutcome::Released(reason), "{event:?}");
            assert_eq!(m.state(), KvmState::Local, "{event:?}");
            assert!(!m.grab_active(), "{event:?} left input grabbed");
        }
    }

    #[test]
    fn release_while_sitting_on_the_edge_is_not_instantly_undone() {
        // Invariant 2, and the whole reason `armed` exists. The pointer is at the edge
        // when the operator hits the release; the next poll reports EdgeReached again.
        // Re-capturing there would make the emergency release useless.
        let mut m = captured();
        m.on(KvmEvent::EmergencyRelease);
        assert!(!m.armed());

        assert_eq!(m.on(KvmEvent::EdgeReached), KvmOutcome::Unchanged);
        assert!(
            !m.grab_active(),
            "release was undone by the pointer resting on the edge"
        );
    }

    #[test]
    fn leaving_the_edge_re_arms_crossing() {
        let mut m = captured();
        m.on(KvmEvent::EmergencyRelease);
        m.on(KvmEvent::EdgeReached); // still disarmed, ignored
        assert!(!m.grab_active());

        m.on(KvmEvent::EdgeLeft);
        assert!(m.armed());
        assert_eq!(m.on(KvmEvent::EdgeReached), KvmOutcome::Captured);
    }

    #[test]
    fn a_lost_peer_never_leaves_input_grabbed() {
        // Fail-safe: if the link dies while input is grabbed, the operator would
        // otherwise be locked out with nothing left to send a release.
        let mut m = captured();
        m.on(KvmEvent::PeerLost);
        assert!(!m.grab_active());
    }

    #[test]
    fn releasing_when_already_local_is_a_no_op_but_still_disarms() {
        let mut m = KvmMachine::new();
        assert_eq!(m.on(KvmEvent::EmergencyRelease), KvmOutcome::Unchanged);
        assert_eq!(m.state(), KvmState::Local);
        // Still disarmed: a release pressed while local should not leave the machine
        // primed to capture the instant the pointer brushes the edge.
        assert!(!m.armed());
    }

    #[test]
    fn edge_contact_while_already_remote_changes_nothing() {
        let mut m = captured();
        assert_eq!(m.on(KvmEvent::EdgeReached), KvmOutcome::Unchanged);
        assert_eq!(m.state(), KvmState::Remote);
    }

    #[test]
    fn repeated_releases_are_idempotent() {
        let mut m = captured();
        assert!(matches!(
            m.on(KvmEvent::EmergencyRelease),
            KvmOutcome::Released(_)
        ));
        assert_eq!(m.on(KvmEvent::EmergencyRelease), KvmOutcome::Unchanged);
        assert_eq!(m.on(KvmEvent::PeerLost), KvmOutcome::Unchanged);
        assert!(!m.grab_active());
    }

    #[test]
    fn a_confined_pointer_never_crosses() {
        // The bug this guards: aiming right in a first-person game reaches the screen
        // edge, and the user gets ripped onto the other machine mid-fight.
        let mut m = KvmMachine::new();
        m.on(KvmEvent::PointerConfined(true));
        assert!(m.pointer_confined());
        assert_eq!(m.on(KvmEvent::EdgeReached), KvmOutcome::Unchanged);
        assert!(!m.grab_active());
    }

    #[test]
    fn releasing_a_lock_on_the_edge_does_not_instantly_hand_off() {
        // Games commonly release the pointer wherever it happens to be. If that spot
        // is the crossing edge, re-arming immediately would hand off the instant the
        // user alt-tabs out.
        let mut m = KvmMachine::new();
        m.on(KvmEvent::PointerConfined(true));
        m.on(KvmEvent::PointerConfined(false));
        assert!(!m.pointer_confined());
        assert!(!m.armed(), "unlocking must not re-arm on its own");
        assert_eq!(m.on(KvmEvent::EdgeReached), KvmOutcome::Unchanged);

        // Moving off the edge is the deliberate act that re-arms it.
        m.on(KvmEvent::EdgeLeft);
        assert_eq!(m.on(KvmEvent::EdgeReached), KvmOutcome::Captured);
    }

    #[test]
    fn a_repeated_confinement_report_does_not_disarm_a_live_setup() {
        // Confinement is polled, so the same state arrives over and over. Only a
        // change should disturb the arming, or crossing could never fire while a
        // background app holds a lock we already know about.
        let mut m = KvmMachine::new();
        m.on(KvmEvent::PointerConfined(false));
        m.on(KvmEvent::PointerConfined(false));
        assert!(m.armed());
        assert_eq!(m.on(KvmEvent::EdgeReached), KvmOutcome::Captured);
    }

    #[test]
    fn confinement_reported_while_remote_does_not_release_control() {
        // Input is grabbed, so a local application cannot meaningfully own the
        // pointer. Treating this as a release would drop the session spuriously.
        let mut m = captured();
        assert_eq!(m.on(KvmEvent::PointerConfined(true)), KvmOutcome::Unchanged);
        assert_eq!(m.state(), KvmState::Remote);
        assert!(m.grab_active());
    }

    #[test]
    fn emergency_release_still_works_while_the_pointer_is_confined() {
        // The escape hatch must not depend on the pointer being free.
        let mut m = captured();
        m.on(KvmEvent::PointerConfined(true));
        assert!(matches!(
            m.on(KvmEvent::EmergencyRelease),
            KvmOutcome::Released(ReleaseReason::EmergencyHotkey)
        ));
        assert!(!m.grab_active());
    }
    #[test]
    fn no_event_sequence_leaves_input_grabbed_after_an_emergency_release() {
        // Exhaustive-ish: for every reachable event ordering that ends in an emergency
        // release, the grab must be off. A future transition that violates this is the
        // one that locks somebody out.
        let events = [
            KvmEvent::EdgeReached,
            KvmEvent::EdgeLeft,
            KvmEvent::PeerLost,
            KvmEvent::PeerEnded,
            KvmEvent::ReturnedAcrossEdge,
        ];
        for a in events {
            for b in events {
                for c in events {
                    let mut m = KvmMachine::new();
                    m.on(a);
                    m.on(b);
                    m.on(c);
                    m.on(KvmEvent::EmergencyRelease);
                    assert!(
                        !m.grab_active(),
                        "grab survived emergency release after {a:?}, {b:?}, {c:?}"
                    );
                }
            }
        }
    }
}
