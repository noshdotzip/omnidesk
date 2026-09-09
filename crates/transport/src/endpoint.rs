//! The QUIC endpoint, and one authenticated connection to a peer.
//!
//! # One endpoint, both roles
//! A peer both dials and accepts. Two endpoints on two ports would mean two firewall
//! rules, two addresses to discover, and a machine that can be driven but cannot drive.
//! `quinn` allows a single endpoint to carry a server config and a client config over
//! one UDP socket, and that is what a mesh peer wants.
//!
//! # Why the transport parameters are set rather than defaulted
//! The link this runs over is measured (docs/status.md): a Wi-Fi path whose round trip
//! varies by an order of magnitude depending on whether the radio is awake. Two defaults
//! matter on such a link:
//!
//! - **Keep-alive.** Without it an idle connection is torn down and the next pointer
//!   crossing pays a full handshake — visible as the cursor sticking at the edge for the
//!   first crossing after a pause.
//! - **Idle timeout.** Left at the default it is generous enough that a peer which has
//!   actually gone (a closed lid, a pulled cable) is still believed to be present, and
//!   input keeps being sent into it instead of being released locally.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rustls_pki_types::CertificateDer;
use ultidesk_core::protocol::PROTOCOL_VERSION;
use ultidesk_identity::{Identity, PeerKey};

use crate::cert::{certificate_for, peer_key, SERVER_NAME};
use crate::frame::MessageStream;
use crate::verify::{PinnedPeers, TrustPolicy};
use crate::TransportError;

/// ALPN, carrying the application protocol version.
///
/// Built from [`PROTOCOL_VERSION`] rather than written out, so the two cannot drift: a
/// hardcoded string would keep saying `ultidesk/1` after the protocol moved on, and two
/// peers would complete a handshake and then disagree about the messages inside it.
///
/// This is also why the QUIC path has no `Hello`. Version negotiation happens here, in
/// the handshake, where a mismatch is a refused connection rather than an error message
/// exchanged over a channel that should not have opened.
fn alpn() -> Vec<u8> {
    format!("ultidesk/{PROTOCOL_VERSION}").into_bytes()
}

/// Long enough not to be chatty, short enough that a NAT or a Wi-Fi power-save window
/// does not drop the connection between crossings.
const KEEP_ALIVE: Duration = Duration::from_secs(5);

/// Three missed keep-alives. A peer that has genuinely gone is noticed within a few
/// seconds — which matters because held keys are released on disconnect, and a modifier
/// stuck on the other machine is what the operator would otherwise be left with.
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);

/// This device's QUIC endpoint.
pub struct PeerEndpoint {
    endpoint: quinn::Endpoint,
    client_config: quinn::ClientConfig,
    local: PeerKey,
}

impl PeerEndpoint {
    /// Bind a socket that can both accept peers and dial them.
    ///
    /// `bind` of `0.0.0.0:0` picks a port, which is what a dialling-only process wants;
    /// [`PeerEndpoint::local_addr`] then reports what it got.
    pub fn bind(
        identity: &Identity,
        bind: SocketAddr,
        policy: TrustPolicy,
    ) -> Result<Self, TransportError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let (cert, key) = certificate_for(identity)?;

        let verifier = Arc::new(PinnedPeers::new(policy.clone(), provider.clone()));

        // TLS 1.3 only, explicitly. QUIC mandates it, but stating it here means the
        // refusal is this code's decision rather than a property of a dependency that
        // could change.
        let mut server_crypto = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| TransportError::Tls(e.to_string()))?
            .with_client_cert_verifier(verifier.clone())
            .with_single_cert(vec![cert.clone()], key.clone_key())
            .map_err(|e| TransportError::Tls(e.to_string()))?;
        server_crypto.alpn_protocols = vec![alpn()];

        let mut client_crypto = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| TransportError::Tls(e.to_string()))?
            .dangerous()
            // "Dangerous" because it replaces web-PKI verification. What replaces it is
            // stricter for this purpose: one specific key rather than any certificate a
            // public CA will sign. See `verify.rs`.
            .with_custom_certificate_verifier(verifier)
            .with_client_auth_cert(vec![cert], key)
            .map_err(|e| TransportError::Tls(e.to_string()))?;
        client_crypto.alpn_protocols = vec![alpn()];

        let transport = Arc::new(transport_config());

        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
                .map_err(|e| TransportError::Tls(e.to_string()))?,
        ));
        server_config.transport_config(transport.clone());

        let mut client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)
                .map_err(|e| TransportError::Tls(e.to_string()))?,
        ));
        client_config.transport_config(transport);

        let endpoint =
            quinn::Endpoint::server(server_config, bind).map_err(|e| TransportError::Bind {
                addr: bind,
                source: e,
            })?;

        Ok(PeerEndpoint {
            endpoint,
            client_config,
            local: identity.public(),
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        self.endpoint
            .local_addr()
            .map_err(|e| TransportError::Stream(e.to_string()))
    }

    pub fn local_key(&self) -> PeerKey {
        self.local
    }

    /// Wait for a peer to connect. `None` once the endpoint is closed.
    ///
    /// A connection that fails the pinning check never comes back from here: it is
    /// refused inside the handshake, so a rejected peer cannot occupy a slot or reach
    /// any application code.
    pub async fn accept(&self) -> Option<Result<PeerConnection, TransportError>> {
        let incoming = self.endpoint.accept().await?;
        Some(match incoming.await {
            Ok(conn) => PeerConnection::adopt(conn, self.local),
            Err(e) => Err(TransportError::Handshake(e.to_string())),
        })
    }

    /// Dial a peer.
    ///
    /// # A returned connection is not yet an accepted one
    /// This resolves once *this* end is satisfied — it has verified the peer's key and
    /// sent its own certificate. The peer validates that certificate afterwards, so a
    /// machine this one is not paired with hands back a live `PeerConnection` and then
    /// closes it a round trip later. That is TLS 1.3's shape, not a bug here, and it
    /// cannot be papered over inside the transport without inventing an
    /// application-level acknowledgement that belongs to the protocol above.
    ///
    /// So a caller must treat "connected" as "the peer is who I expected", never as
    /// "the peer accepted me", and must not report success to an operator until a
    /// message has actually round-tripped.
    pub async fn connect(&self, addr: SocketAddr) -> Result<PeerConnection, TransportError> {
        // The name is a placeholder the verifier ignores; the key is the identity.
        let connecting = self
            .endpoint
            .connect_with(self.client_config.clone(), addr, SERVER_NAME)
            .map_err(|e| TransportError::Handshake(e.to_string()))?;
        let conn = connecting
            .await
            .map_err(|e| TransportError::Handshake(e.to_string()))?;
        PeerConnection::adopt(conn, self.local)
    }

    /// Stop accepting and wait for open connections to close.
    pub async fn close(&self) {
        self.endpoint.close(0u32.into(), b"shutting down");
        self.endpoint.wait_idle().await;
    }
}

fn transport_config() -> quinn::TransportConfig {
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    transport.max_idle_timeout(Some(
        IDLE_TIMEOUT
            .try_into()
            .expect("15s is within QUIC's idle-timeout range"),
    ));
    transport
}

/// One authenticated connection to a peer whose key passed the pinning check.
pub struct PeerConnection {
    conn: quinn::Connection,
    peer: PeerKey,
    local: PeerKey,
}

impl PeerConnection {
    fn adopt(conn: quinn::Connection, local: PeerKey) -> Result<Self, TransportError> {
        // Read back from the completed handshake rather than remembered by the verifier.
        // The verifier is shared by every connection on this endpoint, so a field it
        // wrote would be whichever peer handshaked most recently — a race that would
        // attribute one peer's input to another.
        let certs = conn
            .peer_identity()
            .and_then(|any| any.downcast::<Vec<CertificateDer<'static>>>().ok())
            .ok_or_else(|| {
                TransportError::Handshake(
                    "the peer completed the handshake without presenting a certificate".to_string(),
                )
            })?;
        let end_entity = certs.first().ok_or_else(|| {
            TransportError::Handshake("the peer presented an empty certificate chain".to_string())
        })?;
        let peer = peer_key(end_entity)?;

        Ok(PeerConnection { conn, peer, local })
    }

    /// The device on the other end. Proved, not claimed.
    pub fn peer_key(&self) -> PeerKey {
        self.peer
    }

    pub fn remote_address(&self) -> SocketAddr {
        self.conn.remote_address()
    }

    /// The six digits to compare on both screens before pinning a new device.
    ///
    /// Bound to this connection through TLS exported keying material, so the code an
    /// attacker in the middle could produce is not the code either real machine shows.
    pub fn pairing_code(&self) -> Result<String, TransportError> {
        let mut binding = [0u8; 32];
        self.conn
            .export_keying_material(&mut binding, b"ultidesk pairing", b"")
            .map_err(|e| {
                TransportError::Handshake(format!("no channel binding available: {e:?}"))
            })?;
        Ok(ultidesk_identity::pairing_code(
            &self.local,
            &self.peer,
            &binding,
        ))
    }

    /// Open a control stream to the peer.
    pub async fn open_control(&self) -> Result<MessageStream, TransportError> {
        let (send, recv) = self
            .conn
            .open_bi()
            .await
            .map_err(|e| TransportError::Stream(e.to_string()))?;
        Ok(MessageStream::new(send, recv, self.conn.clone()))
    }

    /// Accept a control stream the peer opened. `None` once the connection ends.
    pub async fn accept_control(&self) -> Option<Result<MessageStream, TransportError>> {
        match self.conn.accept_bi().await {
            Ok((send, recv)) => Some(Ok(MessageStream::new(send, recv, self.conn.clone()))),
            Err(quinn::ConnectionError::ApplicationClosed(_)) => None,
            Err(quinn::ConnectionError::LocallyClosed) => None,
            Err(e) => Some(Err(TransportError::Stream(e.to_string()))),
        }
    }

    pub fn close(&self, reason: &str) {
        self.conn.close(0u32.into(), reason.as_bytes());
    }
}
