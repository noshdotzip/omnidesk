//! Asking a peer a question on a local caller's behalf.
//!
//! # Why the agent is the one that asks
//! The control app speaks only to its own agent. It has no identity of its own, holds no
//! pinned keys, and opening a second connection to the peer from a second process would
//! mean a second thing to authenticate, a second thing to trust, and two answers that
//! could disagree. The agent already holds the peer relationship, so it does the asking.
//!
//! # The ownership check cannot be skipped, and cannot be moved
//! A peer labels every device and monitor it reports with the id of the machine it
//! claims to own them. Only the party that completed the handshake can tell whether that
//! label is honest, because only it knows which key was proved — and that party is this
//! agent, not the control app. So the check runs here, in `quic::ask`, before the answer
//! is relayed at all.
//!
//! What the relayed answer carries is the peer's key, so the reply says whose it is
//! rather than leaving the caller to assume. That is a statement of fact about a check
//! already performed, not a second check the caller is expected to repeat: a control app
//! that could not trust its own agent has already lost — the agent injects its input and
//! holds its private key.
//!
//! # Relaying is not a permission
//! [`ipc::Gate::LocalOnly`] refuses this request from a peer session outright, and no
//! grant can express otherwise. A peer able to relay could reach a third machine it was
//! never paired with, wearing this machine's trust to do it. Belt and braces: the
//! peer-facing transports are built with no [`RelayContext`] at all, so even a mistake in
//! the gate leaves nothing to relay with.

use std::sync::Arc;

use ultidesk_identity::{Identity, PeerKey, PeerStore};

use crate::ipc::{IpcResponse, PeerQuery};

/// What the agent needs to reach a peer: who it is, and who it trusts.
pub struct RelayContext {
    identity: Arc<Identity>,
    peers: PeerStore,
}

impl RelayContext {
    pub fn new(identity: Arc<Identity>, peers: PeerStore) -> Self {
        RelayContext { identity, peers }
    }

    /// Put one question to a paired peer and turn the answer into a reply.
    ///
    /// Every failure comes back as an `Error` response rather than tearing down the
    /// caller's session: a peer being unreachable is ordinary, and the control app needs
    /// to show it rather than lose its connection to its own agent over it.
    pub async fn ask(&self, peer: PeerKey, query: PeerQuery) -> IpcResponse {
        let Some(entry) = self.peers.get(&peer) else {
            return error(
                "unknown_peer",
                format!("{} is not paired with this device", peer.fingerprint()),
            );
        };
        let Some(address) = entry.address.clone() else {
            return error(
                "peer_address_unknown",
                format!(
                    "{} has never been reached from this device, so there is no address to \
                     try; discovery does not exist yet",
                    entry.name
                ),
            );
        };
        let Ok(addr) = address.parse() else {
            return error(
                "peer_address_invalid",
                format!(
                    "{} has a stored address that is not host:port: {address}",
                    entry.name
                ),
            );
        };

        match query {
            PeerQuery::AudioDevices => {
                match crate::quic::peer_audio_devices(&self.identity, &self.peers, addr).await {
                    Ok(devices) => IpcResponse::PeerAudioDevices { peer, devices },
                    Err(e) => error("peer_unreachable", e.to_string()),
                }
            }
            PeerQuery::Monitors => {
                match crate::quic::peer_monitors(&self.identity, &self.peers, addr).await {
                    Ok(monitors) => IpcResponse::PeerMonitors { peer, monitors },
                    Err(e) => error("peer_unreachable", e.to_string()),
                }
            }
        }
    }
}

fn error(code: &str, message: String) -> IpcResponse {
    IpcResponse::Error {
        code: code.to_string(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ultidesk_identity::peers::now_unix;

    fn context(setup: impl FnOnce(&mut PeerStore)) -> RelayContext {
        let identity = Arc::new(Identity::generate());
        let mut peers = PeerStore::new(identity.public());
        setup(&mut peers);
        RelayContext::new(identity, peers)
    }

    fn is_error(response: &IpcResponse, expected: &str) -> bool {
        matches!(response, IpcResponse::Error { code, .. } if code == expected)
    }

    #[tokio::test]
    async fn an_unpaired_peer_is_refused_before_any_connection_is_attempted() {
        let stranger = Identity::from_secret_bytes([3u8; 32]).public();
        let ctx = context(|_| {});
        let r = ctx.ask(stranger, PeerQuery::Monitors).await;
        assert!(is_error(&r, "unknown_peer"), "{r:?}");
    }

    #[tokio::test]
    async fn a_paired_peer_with_no_known_address_says_so() {
        // The state right after pairing over a listener: trusted, never dialled. It is a
        // different answer from "unreachable", and an operator needs to tell them apart.
        let peer = Identity::from_secret_bytes([4u8; 32]).public();
        let ctx = context(|store| {
            store.pin(peer, "arch", now_unix()).unwrap();
        });
        let r = ctx.ask(peer, PeerQuery::AudioDevices).await;
        assert!(is_error(&r, "peer_address_unknown"), "{r:?}");
    }

    #[tokio::test]
    async fn a_stored_address_that_is_not_an_address_is_reported_not_panicked_on() {
        let peer = Identity::from_secret_bytes([5u8; 32]).public();
        let ctx = context(|store| {
            store.pin(peer, "arch", now_unix()).unwrap();
            store.remember_address(&peer, "not-an-address");
        });
        let r = ctx.ask(peer, PeerQuery::Monitors).await;
        assert!(is_error(&r, "peer_address_invalid"), "{r:?}");
    }

    #[tokio::test]
    async fn an_unreachable_peer_is_an_error_response_not_a_dropped_session() {
        // Port 1 on loopback: nothing listens there, so the connection fails fast.
        // The caller must get an answer it can show, not lose its own connection.
        let peer = Identity::from_secret_bytes([6u8; 32]).public();
        let ctx = context(|store| {
            store.pin(peer, "arch", now_unix()).unwrap();
            store.remember_address(&peer, "127.0.0.1:1");
        });
        let r = ctx.ask(peer, PeerQuery::Monitors).await;
        assert!(is_error(&r, "peer_unreachable"), "{r:?}");
    }
}
