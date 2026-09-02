//! Input loop prevention.
//!
//! Shared input is a graph of devices forwarding pointer/keyboard events to each
//! other. Without defenses this trivially loops: A→B, B injects the event, its own
//! hook re-captures it, B forwards it back to A, forever; or A→B→C→A around a ring.
//!
//! The brief mandates *layered* defenses rather than relying on any single one. This
//! guard implements four, all of which are pure and testable here:
//!
//! 1. **Injection marker** — every event this process synthesizes is tagged with a
//!    per-run [`InjectionMarker`]. If our own low-level hook re-captures a tagged
//!    event, we drop it. (On Windows this maps onto `SendInput`'s `dwExtraInfo`, but
//!    the marker is enforced here too so it is not the *only* mechanism.)
//! 2. **Origin identity** — an event whose `origin_device_id` is *us* has come back
//!    around; drop it.
//! 3. **Hop limit** — a bounded [`protocol::MAX_HOPS`] TTL kills ring loops even if
//!    identity/marker checks are somehow defeated.
//! 4. **Event de-duplication** — a bounded LRU of recently-seen [`EventId`]s drops
//!    replays and accidental re-delivery.
//!
//! [`protocol::MAX_HOPS`]: crate::protocol::MAX_HOPS

use crate::ids::{DeviceId, EventId};
use crate::protocol::MAX_HOPS;
use std::collections::{HashSet, VecDeque};

/// A per-process-run nonce identifying input this agent synthesized. Not persisted;
/// regenerated each run so a stale marker can never match a fresh process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InjectionMarker(pub u64);

/// The routing-relevant header of any input event, independent of payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputHeader {
    pub event_id: EventId,
    pub origin_device_id: DeviceId,
    pub hop_count: u8,
    /// Set if this event was synthesized by some agent's injector.
    pub injected_by: Option<InjectionMarker>,
}

/// Why the guard rejected an event. Surfaced (without payload) to diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// Re-captured one of our own injected events.
    OwnInjection,
    /// The event originated on this device and looped back.
    OwnOrigin,
    /// Exceeded the hop TTL.
    HopLimit,
    /// Already processed this event id.
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardDecision {
    Accept,
    Reject(RejectReason),
}

/// Bounded loop guard. `capacity` caps the de-dup memory so a flood of unique ids
/// cannot grow it without limit.
#[derive(Debug)]
pub struct LoopGuard {
    self_device: DeviceId,
    self_marker: InjectionMarker,
    seen: HashSet<EventId>,
    order: VecDeque<EventId>,
    capacity: usize,
}

impl LoopGuard {
    pub fn new(self_device: DeviceId, self_marker: InjectionMarker, capacity: usize) -> Self {
        Self {
            self_device,
            self_marker,
            seen: HashSet::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Decide whether an incoming event may be injected locally. On `Accept` the event
    /// id is recorded so an immediate re-delivery is rejected as a duplicate.
    pub fn evaluate(&mut self, header: &InputHeader) -> GuardDecision {
        if header.injected_by == Some(self.self_marker) {
            return GuardDecision::Reject(RejectReason::OwnInjection);
        }
        if header.origin_device_id == self.self_device {
            return GuardDecision::Reject(RejectReason::OwnOrigin);
        }
        if header.hop_count >= MAX_HOPS {
            return GuardDecision::Reject(RejectReason::HopLimit);
        }
        if self.seen.contains(&header.event_id) {
            return GuardDecision::Reject(RejectReason::Duplicate);
        }
        self.remember(header.event_id);
        GuardDecision::Accept
    }

    /// Prepare an event to be forwarded onward: bump the hop count. Returns `None` if
    /// forwarding it would exceed the TTL (caller must drop it).
    pub fn stamp_for_forward(&self, header: &InputHeader) -> Option<InputHeader> {
        let next = header.hop_count.checked_add(1)?;
        if next >= MAX_HOPS {
            return None;
        }
        Some(InputHeader {
            hop_count: next,
            ..*header
        })
    }

    /// Tag a header as injected by this process (call right before local injection).
    pub fn mark_injected(&self, header: &InputHeader) -> InputHeader {
        InputHeader {
            injected_by: Some(self.self_marker),
            ..*header
        }
    }

    fn remember(&mut self, id: EventId) {
        self.seen.insert(id);
        self.order.push_back(id);
        while self.order.len() > self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev() -> DeviceId {
        DeviceId::new()
    }

    fn header(origin: DeviceId) -> InputHeader {
        InputHeader {
            event_id: EventId::new(),
            origin_device_id: origin,
            hop_count: 0,
            injected_by: None,
        }
    }

    #[test]
    fn accepts_fresh_remote_event() {
        let me = dev();
        let mut g = LoopGuard::new(me, InjectionMarker(1), 128);
        let peer = dev();
        assert_eq!(g.evaluate(&header(peer)), GuardDecision::Accept);
    }

    #[test]
    fn rejects_our_own_origin() {
        let me = dev();
        let mut g = LoopGuard::new(me, InjectionMarker(1), 128);
        assert_eq!(
            g.evaluate(&header(me)),
            GuardDecision::Reject(RejectReason::OwnOrigin)
        );
    }

    #[test]
    fn rejects_recaptured_own_injection() {
        let me = dev();
        let marker = InjectionMarker(42);
        let mut g = LoopGuard::new(me, marker, 128);
        let peer = dev();
        let mut h = header(peer);
        h.injected_by = Some(marker);
        assert_eq!(
            g.evaluate(&h),
            GuardDecision::Reject(RejectReason::OwnInjection)
        );
    }

    #[test]
    fn injection_from_a_different_agent_is_not_ours() {
        let me = dev();
        let mut g = LoopGuard::new(me, InjectionMarker(42), 128);
        let peer = dev();
        let mut h = header(peer);
        h.injected_by = Some(InjectionMarker(99)); // some other machine's marker
        assert_eq!(g.evaluate(&h), GuardDecision::Accept);
    }

    #[test]
    fn rejects_duplicate_event_id() {
        let me = dev();
        let mut g = LoopGuard::new(me, InjectionMarker(1), 128);
        let peer = dev();
        let h = header(peer);
        assert_eq!(g.evaluate(&h), GuardDecision::Accept);
        assert_eq!(
            g.evaluate(&h),
            GuardDecision::Reject(RejectReason::Duplicate)
        );
    }

    #[test]
    fn rejects_hop_limit() {
        let me = dev();
        let mut g = LoopGuard::new(me, InjectionMarker(1), 128);
        let peer = dev();
        let mut h = header(peer);
        h.hop_count = MAX_HOPS;
        assert_eq!(
            g.evaluate(&h),
            GuardDecision::Reject(RejectReason::HopLimit)
        );
    }

    #[test]
    fn forward_stamp_increments_then_drops_at_ttl() {
        let g = LoopGuard::new(dev(), InjectionMarker(1), 128);
        let mut h = header(dev());
        h.hop_count = 0;
        let f1 = g.stamp_for_forward(&h).unwrap();
        assert_eq!(f1.hop_count, 1);
        // Near the limit, forwarding must refuse.
        h.hop_count = MAX_HOPS - 1;
        assert!(g.stamp_for_forward(&h).is_none());
    }

    #[test]
    fn dedup_memory_is_bounded_lru() {
        let me = dev();
        let peer = dev();
        let mut g = LoopGuard::new(me, InjectionMarker(1), 2);
        let a = header(peer);
        let b = header(peer);
        let c = header(peer);
        assert_eq!(g.evaluate(&a), GuardDecision::Accept);
        assert_eq!(g.evaluate(&b), GuardDecision::Accept);
        assert_eq!(g.evaluate(&c), GuardDecision::Accept); // evicts `a`
                                                           // `a` fell out of the LRU window, so it is accepted again (bounded memory,
                                                           // by design — the hop limit and origin/marker checks remain the hard stops).
        assert_eq!(g.evaluate(&a), GuardDecision::Accept);
        // `c` is still remembered.
        assert_eq!(
            g.evaluate(&c),
            GuardDecision::Reject(RejectReason::Duplicate)
        );
    }

    #[test]
    fn mark_injected_sets_our_marker() {
        let marker = InjectionMarker(7);
        let g = LoopGuard::new(dev(), marker, 8);
        let h = header(dev());
        assert_eq!(g.mark_injected(&h).injected_by, Some(marker));
    }
}
