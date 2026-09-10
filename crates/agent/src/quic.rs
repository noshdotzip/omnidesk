//! Serving peers over the authenticated QUIC channel, and pairing with a new one.
//!
//! This is `tcp.rs`'s job done properly. The differences are not cosmetic:
//!
//! | | `tcp.rs` (dev) | here |
//! |---|---|---|
//! | Confidentiality | none — keystrokes in the clear | TLS 1.3 |
//! | Who may connect | anyone holding a shared token | a pinned Ed25519 identity, proved |
//! | Which peer sent this | unknowable | `PeerConnection::peer_key` |
//! | Version negotiation | a `Hello` message | ALPN, before the channel opens |
//!
//! The message handling is deliberately *unchanged*: the same `IpcRequest`, the same
//! `Session::handle`, the same release-everything-on-disconnect rule. Only the envelope
//! is different, so the logic that decides what a peer is allowed to do has one
//! implementation rather than one per transport.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use ultidesk_identity::{Identity, PeerKey, PeerStore};
use ultidesk_transport::{MessageStream, PeerConnection, PeerEndpoint, TrustPolicy};

use crate::ipc::{IpcRequest, IpcResponse, LocalBackends, Session};

/// Serve pinned peers until the process is stopped.
///
/// The backends arrive already chosen rather than being selected here, so that both
/// this transport and the dev TCP one are handed the *same* injector: which one is in
/// use decides whether the agent can run unattended, and picking it twice is two chances
/// to pick differently.
pub async fn serve(
    identity: &Identity,
    bind: SocketAddr,
    peers: &PeerStore,
    backends: Arc<LocalBackends>,
) -> anyhow::Result<()> {
    let trusted = peers.keys();
    // Refused up front rather than accepted-and-always-rejected. An agent that listens
    // while trusting nobody looks like it is working and answers every peer with a
    // refusal that reads as a network fault.
    if trusted.is_empty() {
        anyhow::bail!(
            "no peers are paired, so nothing could connect; run `ultidesk-agent pair` first"
        );
    }

    let endpoint = PeerEndpoint::bind(identity, bind, TrustPolicy::Pinned(trusted))
        .with_context(|| format!("failed to bind {bind}"))?;
    tracing::info!(
        bind = %endpoint.local_addr()?,
        paired = peers.peers().len(),
        "peer transport listening (QUIC, mutually authenticated)"
    );

    while let Some(incoming) = endpoint.accept().await {
        let conn = match incoming {
            Ok(conn) => conn,
            Err(e) => {
                // Refusals land here, and they are the interesting log line on this
                // path: an unpaired machine trying to connect is exactly what an
                // operator wants to see.
                tracing::warn!(error = %e, "refused an incoming connection");
                continue;
            }
        };
        let name = peers
            .get(&conn.peer_key())
            .map(|p| p.name.clone())
            // Unreachable while the pinned set comes from this store, but reported
            // rather than unwrapped: a peer that authenticated must never be dropped
            // because its label is missing.
            .unwrap_or_else(|| conn.peer_key().fingerprint());
        // Read once, from the store, for the key the handshake proved. A peer cannot
        // assert its own permissions and cannot change them by reconnecting to a
        // different name.
        let permissions = peers.permissions(&conn.peer_key());
        let backends = backends.clone();

        tokio::spawn(async move {
            tracing::info!(
                peer = %name,
                addr = %conn.remote_address(),
                allowed = ?permissions.granted(),
                "peer connected"
            );
            if let Err(e) = serve_connection(&conn, backends, permissions).await {
                tracing::warn!(peer = %name, error = %e, "peer connection ended with error");
            }
            tracing::info!(peer = %name, "peer disconnected");
        });
    }
    Ok(())
}

/// Serve every stream the peer opens, concurrently.
///
/// Concurrently and not one at a time: QUIC's whole point is that a connection carries
/// several independent streams, and draining them in sequence would let a long-lived
/// control stream block a second one from ever being read — which looks, from the
/// peer's side, like a connection that accepted a stream and then went silent.
async fn serve_connection(
    conn: &PeerConnection,
    backends: Arc<LocalBackends>,
    permissions: ultidesk_identity::Permissions,
) -> anyhow::Result<()> {
    let mut streams = tokio::task::JoinSet::new();
    while let Some(stream) = conn.accept_control().await {
        let mut stream = stream?;
        let backends = backends.clone();
        streams.spawn(async move {
            // One session per stream, so the held-input bookkeeping — and therefore the
            // release below — is scoped to exactly the stream that pressed the keys.
            let mut session = Session::for_authenticated_peer(permissions);
            let result = pump(&mut stream, &mut session, backends.as_ref()).await;

            // However the stream ended — cleanly, a crash, a pulled cable — anything it
            // was holding must be released, or a modifier stays down on this machine and
            // the operator is no longer looking at it.
            let released = session.release_all(backends.injector.as_ref());
            if released > 0 {
                tracing::info!(released, "released held input after the stream ended");
            }
            if let Err(e) = result {
                tracing::warn!(error = %e, "control stream ended with error");
            }
        });
    }
    // The connection is gone; wait for the streams rather than aborting them. Aborting
    // would cancel the task mid-`release_all` and leave exactly the stuck modifier that
    // release exists to prevent. Each stream's `recv` fails immediately once the
    // connection is closed, so this is bounded.
    while streams.join_next().await.is_some() {}
    Ok(())
}

async fn pump(
    stream: &mut MessageStream,
    session: &mut Session,
    backends: &LocalBackends,
) -> anyhow::Result<()> {
    while let Some(payload) = stream.recv().await? {
        let response = match serde_json::from_slice::<IpcRequest>(&payload) {
            Ok(req) => session.handle(req, &backends.borrow()),
            Err(e) => IpcResponse::Error {
                code: "bad_request".into(),
                message: format!("invalid request json: {e}"),
            },
        };
        stream.send(&serde_json::to_vec(&response)?).await?;
    }
    Ok(())
}

// ---- pairing ---------------------------------------------------------------------

/// What a completed pairing produced, for the caller to persist.
pub struct Paired {
    pub key: PeerKey,
    pub code: String,
}

/// Wait for a machine that has never connected before, and show the code to compare.
///
/// Deliberately one connection and then done. A listener that stayed open under
/// [`TrustPolicy::Pairing`] would accept anything on the network for as long as it ran,
/// which is the opposite of what pairing is for.
pub async fn pair_listen(identity: &Identity, bind: SocketAddr) -> anyhow::Result<Paired> {
    let endpoint = PeerEndpoint::bind(identity, bind, TrustPolicy::Pairing)
        .with_context(|| format!("failed to bind {bind}"))?;
    println!("listening for a peer on {}", endpoint.local_addr()?);
    println!("run this on the other machine:");
    println!(
        "    ultidesk-agent pair <this machine's address>:{}",
        endpoint.local_addr()?.port()
    );

    let conn = endpoint
        .accept()
        .await
        .ok_or_else(|| anyhow::anyhow!("the endpoint closed before a peer arrived"))??;
    let paired = Paired {
        key: conn.peer_key(),
        code: conn.pairing_code()?,
    };

    // Held open briefly so the dialling side can export the same keying material before
    // this end tears the connection down.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    conn.close("pairing complete");
    Ok(paired)
}

/// Dial a machine that has never connected before and show the code to compare.
pub async fn pair_connect(identity: &Identity, addr: SocketAddr) -> anyhow::Result<Paired> {
    let endpoint = PeerEndpoint::bind(identity, "0.0.0.0:0".parse()?, TrustPolicy::Pairing)?;
    let conn = endpoint
        .connect(addr)
        .await
        .with_context(|| format!("could not reach a peer at {addr}"))?;
    let paired = Paired {
        key: conn.peer_key(),
        code: conn.pairing_code()?,
    };
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    conn.close("pairing complete");
    Ok(paired)
}

// ---- probing ---------------------------------------------------------------------

/// Round-trip a `Ping` against a paired peer and report the latency.
///
/// Deliberately the only client this file has, and deliberately harmless: `Ping` is the
/// one request that touches no injector, so this proves the authenticated path end to
/// end — pinning, handshake, framing, dispatch, reply — without moving a pointer or
/// pressing a key on a machine nobody is watching.
///
/// The timing is worth having on its own. docs/status.md records a link whose round trip
/// varies between 4 ms and half a second depending on Wi-Fi power save, and every input
/// latency figure is meaningless until that is settled. This measures the same path the
/// KVM will use, rather than ICMP, so it includes the framing and dispatch costs.
pub async fn peer_ping(
    identity: &Identity,
    peers: &PeerStore,
    addr: SocketAddr,
    count: u32,
) -> anyhow::Result<()> {
    let trusted = peers.keys();
    if trusted.is_empty() {
        anyhow::bail!("no peers are paired; run `ultidesk-agent pair` first");
    }

    let endpoint =
        PeerEndpoint::bind(identity, "0.0.0.0:0".parse()?, TrustPolicy::Pinned(trusted))?;
    let conn = endpoint
        .connect(addr)
        .await
        .with_context(|| format!("could not reach a paired peer at {addr}"))?;
    let name = peers
        .get(&conn.peer_key())
        .map(|p| p.name.clone())
        .unwrap_or_else(|| conn.peer_key().fingerprint());
    println!("connected to {name} ({})", conn.peer_key().fingerprint());

    let mut stream = conn.open_control().await?;
    let mut samples = Vec::new();
    for i in 0..count {
        let started = std::time::Instant::now();
        stream.send(&serde_json::to_vec(&IpcRequest::Ping)?).await?;
        // The first successful round trip is also the first proof that the *peer*
        // accepted us: its check of our certificate happens after we believe we are
        // connected, so nothing before this point means we were let in.
        let reply = stream
            .recv()
            .await?
            .ok_or_else(|| anyhow::anyhow!("the peer closed the stream without replying"))?;
        let elapsed = started.elapsed();

        match serde_json::from_slice::<IpcResponse>(&reply)? {
            IpcResponse::Pong => {}
            other => anyhow::bail!("peer answered a Ping with {other:?}"),
        }
        println!(
            "  ping {} -> pong in {:.1} ms",
            i + 1,
            elapsed.as_secs_f64() * 1000.0
        );
        samples.push(elapsed);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    stream.finish().await?;
    conn.close("done");

    if let (Some(min), Some(max)) = (samples.iter().min(), samples.iter().max()) {
        let total: std::time::Duration = samples.iter().sum();
        let avg = total / samples.len() as u32;
        println!(
            "{} round trips: min {:.1} ms, avg {:.1} ms, max {:.1} ms",
            samples.len(),
            min.as_secs_f64() * 1000.0,
            avg.as_secs_f64() * 1000.0,
            max.as_secs_f64() * 1000.0
        );
    }
    Ok(())
}

/// Ask a paired peer what audio endpoints it has.
///
/// # The answer is checked against the identity that was authenticated
/// Every endpoint carries the `DeviceId` of the machine that owns it, and the peer fills
/// that in itself. Nothing stops a compromised peer from labelling its endpoints with
/// *another* machine's id — and if it did, a saved route would silently be re-pointed at
/// a device on a third machine.
///
/// It cannot get away with it here, and only because the id is derived from the public
/// key ([ADR-0012]): this end knows which key completed the handshake, so it can compute
/// the id that key is entitled to and reject anything else. That check is the concrete
/// payoff of deriving the id rather than drawing it at random — with a random uuid there
/// would be nothing to compare against.
///
/// [ADR-0012]: ../../../docs/adrs/0012-device-identity.md
pub async fn peer_audio_devices(
    identity: &Identity,
    peers: &PeerStore,
    addr: SocketAddr,
) -> anyhow::Result<Vec<ultidesk_topology::AudioDevice>> {
    let trusted = peers.keys();
    if trusted.is_empty() {
        anyhow::bail!("no peers are paired; run `ultidesk-agent pair` first");
    }

    let endpoint =
        PeerEndpoint::bind(identity, "0.0.0.0:0".parse()?, TrustPolicy::Pinned(trusted))?;
    let conn = endpoint
        .connect(addr)
        .await
        .with_context(|| format!("could not reach a paired peer at {addr}"))?;
    let peer_key = conn.peer_key();
    let name = peers
        .get(&peer_key)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| peer_key.fingerprint());

    let mut stream = conn.open_control().await?;
    stream
        .send(&serde_json::to_vec(&IpcRequest::ListAudioDevices)?)
        .await?;
    let reply = stream
        .recv()
        .await?
        .ok_or_else(|| anyhow::anyhow!("the peer closed the stream without replying"))?;
    stream.finish().await?;
    conn.close("done");

    let devices = match serde_json::from_slice::<IpcResponse>(&reply)? {
        IpcResponse::AudioDevices { devices } => devices,
        IpcResponse::Error { code, message } => {
            anyhow::bail!("{name} refused the request ({code}): {message}")
        }
        other => anyhow::bail!("{name} answered ListAudioDevices with {other:?}"),
    };

    check_owner(&devices, peer_key).map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
    Ok(devices)
}

/// Refuse a device list that claims to belong to a machine other than the one that
/// proved its identity.
fn check_owner(
    devices: &[ultidesk_topology::AudioDevice],
    peer_key: PeerKey,
) -> Result<(), String> {
    let expected = peer_key.device_id();
    if let Some(bad) = devices.iter().find(|d| d.device_id != expected) {
        return Err(format!(
            "endpoint {:?} is labelled as belonging to device {}, but the peer that sent \
             it authenticated as {} ({})",
            bad.node,
            bad.device_id,
            expected,
            peer_key.fingerprint()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ultidesk_topology::{AudioDevice, DeviceKind};

    fn device(device_id: ultidesk_core::DeviceId) -> AudioDevice {
        AudioDevice {
            device_id,
            node: "speakers".into(),
            name: "Speakers".into(),
            kind: DeviceKind::Output,
            is_default: true,
        }
    }

    #[test]
    fn a_peers_own_devices_are_accepted() {
        let key = Identity::from_secret_bytes([4u8; 32]).public();
        assert_eq!(check_owner(&[device(key.device_id())], key), Ok(()));
    }

    #[test]
    fn an_empty_list_is_fine() {
        // A machine really can have no endpoints, and that is not a spoofing attempt.
        let key = Identity::from_secret_bytes([4u8; 32]).public();
        assert_eq!(check_owner(&[], key), Ok(()));
    }

    #[test]
    fn a_peer_cannot_label_its_devices_as_another_machines() {
        // The whole point: with a random uuid there would be nothing to compare against,
        // and a compromised peer could re-point a saved route at a third machine.
        let peer = Identity::from_secret_bytes([4u8; 32]).public();
        let someone_else = Identity::from_secret_bytes([5u8; 32]).public();

        let err = check_owner(&[device(someone_else.device_id())], peer).unwrap_err();
        assert!(err.contains(&peer.fingerprint()), "{err}");
        assert!(err.contains("speakers"), "{err}");
    }

    #[test]
    fn one_bad_entry_rejects_the_whole_list() {
        // Not filtered: a list that is partly forged is not a list to act on, and
        // silently dropping entries would leave the operator wondering where a device
        // went.
        let peer = Identity::from_secret_bytes([4u8; 32]).public();
        let other = Identity::from_secret_bytes([6u8; 32]).public();
        let list = [device(peer.device_id()), device(other.device_id())];
        assert!(check_owner(&list, peer).is_err());
    }
}
