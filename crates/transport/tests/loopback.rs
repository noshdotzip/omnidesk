//! Two endpoints on loopback UDP, which is enough to test everything that decides
//! whether a peer is allowed to drive this machine.
//!
//! No second machine, no desktop, and nothing is injected anywhere: the transport has no
//! opinion about the bytes it carries, so the security properties can be exercised on
//! one host in milliseconds. What these tests cannot cover is the wire behaviour of a
//! real link — latency, loss, and the Wi-Fi power-save behaviour measured in
//! docs/status.md — which is why they are not a substitute for the cross-machine run.

use std::net::SocketAddr;

use ultidesk_identity::Identity;
use ultidesk_transport::{PeerEndpoint, TrustPolicy};

fn loopback() -> SocketAddr {
    // Port 0: the OS picks, so parallel tests cannot collide on a fixed one.
    "127.0.0.1:0".parse().unwrap()
}

/// A server that accepts exactly one connection, answers one message, and stops.
///
/// Takes the endpoint by `Arc` so the test can keep it alive: dropping a `PeerEndpoint`
/// closes the QUIC endpoint under every connection it opened, which in a test reads as a
/// mysterious "connection lost" on the client.
async fn serve_one(
    endpoint: std::sync::Arc<PeerEndpoint>,
    reply: &'static [u8],
) -> Result<Vec<u8>, ultidesk_transport::TransportError> {
    let conn = endpoint
        .accept()
        .await
        .expect("the endpoint was closed before a peer arrived")?;
    let mut stream = conn
        .accept_control()
        .await
        .expect("the peer closed before opening a control stream")?;
    let received = stream.recv().await?.expect("the peer sent nothing");
    stream.send(reply).await?;
    stream.finish().await?;
    Ok(received)
}

#[tokio::test(flavor = "multi_thread")]
async fn two_paired_devices_authenticate_each_other_and_exchange_a_message() {
    let alice = Identity::generate();
    let bob = Identity::generate();

    // Each pins only the other. This is what a completed pairing leaves behind.
    let server =
        PeerEndpoint::bind(&alice, loopback(), TrustPolicy::Pinned(vec![bob.public()])).unwrap();
    let addr = server.local_addr().unwrap();

    let client =
        PeerEndpoint::bind(&bob, loopback(), TrustPolicy::Pinned(vec![alice.public()])).unwrap();

    let server = std::sync::Arc::new(server);
    let serving = tokio::spawn(serve_one(server.clone(), b"pong"));

    let conn = client.connect(addr).await.unwrap();
    // Proved by the handshake, not asserted by the peer.
    assert_eq!(conn.peer_key(), alice.public());

    let mut stream = conn.open_control().await.unwrap();
    stream.send(b"ping").await.unwrap();
    let answer = stream.recv().await.unwrap().unwrap();
    assert_eq!(answer, b"pong");

    assert_eq!(serving.await.unwrap().unwrap(), b"ping");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_is_not_pinned_cannot_connect() {
    let alice = Identity::generate();
    let stranger = Identity::generate();
    let expected = Identity::generate();

    // Alice pins someone else entirely.
    let server = PeerEndpoint::bind(
        &alice,
        loopback(),
        TrustPolicy::Pinned(vec![expected.public()]),
    )
    .unwrap();
    let addr = server.local_addr().unwrap();
    let accepting = tokio::spawn(async move { server.accept().await.map(|r| r.is_ok()) });

    let client = PeerEndpoint::bind(
        &stranger,
        loopback(),
        TrustPolicy::Pinned(vec![alice.public()]),
    )
    .unwrap();

    // The stranger trusts Alice, so it gets past *its* check. Alice's check is the one
    // that matters, and it happens on her side after the client believes it is
    // connected — see the note on `connect`. So the refusal is observed on the first
    // round trip, which is exactly how a caller must treat it.
    let outcome = round_trip(&client, addr).await;
    assert!(
        outcome.is_err(),
        "an unpinned device completed a round trip: {outcome:?}"
    );
    // And it is told why, naming its own fingerprint. Without this the refusal reads as
    // "connection lost", and the operator has no way to tell a missing pairing from a
    // network fault — nor which key to add on the other machine.
    let message = outcome.err().unwrap().to_string();
    assert!(
        message.contains(&stranger.public().fingerprint()),
        "unhelpful refusal: {message}"
    );

    // Nothing reached the application on Alice's side either.
    let accepted = accepting.await.unwrap();
    assert!(
        accepted != Some(true),
        "a refused peer must not surface as an accepted connection"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_has_pinned_nobody_refuses_everyone() {
    // The state a machine is in after its peer store is lost or has never been written.
    // Failing closed is the whole reason `peers::load` returns an empty store.
    let alice = Identity::generate();
    let bob = Identity::generate();

    let server = PeerEndpoint::bind(&alice, loopback(), TrustPolicy::Pinned(Vec::new())).unwrap();
    let addr = server.local_addr().unwrap();
    let accepting = tokio::spawn(async move { server.accept().await.map(|r| r.is_ok()) });

    let client =
        PeerEndpoint::bind(&bob, loopback(), TrustPolicy::Pinned(vec![alice.public()])).unwrap();

    assert!(round_trip(&client, addr).await.is_err());
    assert!(accepting.await.unwrap() != Some(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_dialling_device_also_checks_who_answered() {
    // The asymmetric case: the server would happily take anyone, but the client has
    // pinned a specific machine and something else answered. Without this check, an
    // attacker who could take the address would be driving the operator's input.
    let impostor = Identity::generate();
    let expected = Identity::generate();
    let bob = Identity::generate();

    let server = PeerEndpoint::bind(&impostor, loopback(), TrustPolicy::Pairing).unwrap();
    let addr = server.local_addr().unwrap();
    let _accepting = tokio::spawn(async move { server.accept().await.map(|r| r.is_ok()) });

    let client = PeerEndpoint::bind(
        &bob,
        loopback(),
        TrustPolicy::Pinned(vec![expected.public()]),
    )
    .unwrap();

    let result = client.connect(addr).await;
    assert!(
        result.is_err(),
        "connected to a machine that is not the pinned one"
    );
    // The failure names the fingerprint the operator was shown, so "wrong machine" can
    // be told from "not paired yet".
    let message = result.err().unwrap().to_string();
    assert!(
        message.contains(&impostor.public().fingerprint()),
        "unhelpful refusal: {message}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn both_ends_of_a_pairing_connection_show_the_same_code() {
    // Two machines that have never met. The channel is established so that a code can be
    // compared; it is not trust, and nothing may act on it until the operator confirms.
    let alice = Identity::generate();
    let bob = Identity::generate();

    let server = PeerEndpoint::bind(&alice, loopback(), TrustPolicy::Pairing).unwrap();
    let addr = server.local_addr().unwrap();
    let accepting = tokio::spawn(async move {
        let conn = server.accept().await.unwrap().unwrap();
        let code = conn.pairing_code().unwrap();
        let peer = conn.peer_key();
        // Held open until the client has read its own code; closing here would tear the
        // connection down before the client can export keying material from it.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        (code, peer)
    });

    let client = PeerEndpoint::bind(&bob, loopback(), TrustPolicy::Pairing).unwrap();
    let conn = client.connect(addr).await.unwrap();
    let client_code = conn.pairing_code().unwrap();

    let (server_code, server_saw) = accepting.await.unwrap();
    assert_eq!(
        client_code, server_code,
        "the operator would be comparing two different numbers"
    );
    assert_eq!(server_saw, bob.public());
    assert_eq!(conn.peer_key(), alice.public());
    assert_eq!(client_code.replace(' ', "").len(), 6);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_different_pairings_do_not_share_a_code() {
    // The property the whole comparison rests on: a machine in the middle holds two
    // separate sessions, and they cannot both show the operator's number.
    let alice = Identity::generate();

    let mut codes = Vec::new();
    for _ in 0..2 {
        let bob = Identity::generate();
        let server = PeerEndpoint::bind(&alice, loopback(), TrustPolicy::Pairing).unwrap();
        let addr = server.local_addr().unwrap();
        let accepting = tokio::spawn(async move {
            let conn = server.accept().await.unwrap().unwrap();
            let code = conn.pairing_code().unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            code
        });
        let client = PeerEndpoint::bind(&bob, loopback(), TrustPolicy::Pairing).unwrap();
        let _conn = client.connect(addr).await.unwrap();
        codes.push(accepting.await.unwrap());
    }

    assert_ne!(codes[0], codes[1]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_oversized_message_is_refused_before_it_reaches_the_wire() {
    let alice = Identity::generate();
    let bob = Identity::generate();

    let server =
        PeerEndpoint::bind(&alice, loopback(), TrustPolicy::Pinned(vec![bob.public()])).unwrap();
    let addr = server.local_addr().unwrap();
    let _accepting = tokio::spawn(serve_one(std::sync::Arc::new(server), b"ok"));

    let client =
        PeerEndpoint::bind(&bob, loopback(), TrustPolicy::Pinned(vec![alice.public()])).unwrap();
    let conn = client.connect(addr).await.unwrap();
    let mut stream = conn.open_control().await.unwrap();

    let too_big = vec![0u8; ultidesk_core::protocol::MAX_MESSAGE_BYTES + 1];
    let err = stream.send(&too_big).await.unwrap_err();
    assert!(
        matches!(
            err,
            ultidesk_transport::TransportError::MessageTooLarge { .. }
        ),
        "{err}"
    );
}

/// Connect and complete one exchange, so that a refusal which arrives after the client
/// believes it is connected is still observed.
async fn round_trip(
    client: &PeerEndpoint,
    addr: SocketAddr,
) -> Result<Vec<u8>, ultidesk_transport::TransportError> {
    let conn = client.connect(addr).await?;
    let mut stream = conn.open_control().await?;
    stream.send(b"ping").await?;
    stream
        .recv()
        .await?
        .ok_or_else(|| ultidesk_transport::TransportError::Stream("peer hung up".to_string()))
}
