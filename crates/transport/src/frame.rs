//! Message framing on a QUIC stream.
//!
//! # Why not the newline framing the dev transport uses
//! `tcp.rs` delimits messages with `\n` and reads with `read_line`. That works for JSON
//! that never contains a raw newline and stops working the moment anything else goes
//! down the same channel. A length prefix does not care what the payload is, and it
//! lets the size limit be enforced *before* allocating, which is the point of
//! [`MAX_MESSAGE_BYTES`]: a hostile or broken peer must not be able to make this machine
//! reserve a gigabyte by claiming it is about to send one.
//!
//! # Why a stream and not a datagram
//! Control messages have to arrive, in order — a `ReleaseAllInput` that overtakes the
//! key-down it is meant to undo leaves a modifier stuck on the other machine. QUIC
//! streams give that. Pointer motion is the opposite case (the newest position makes
//! every older one irrelevant) and belongs on datagrams; that is
//! [`crate::PeerConnection::send_datagram`], deliberately a different method so the
//! choice is made per message type rather than by default.

use ultidesk_core::protocol::MAX_MESSAGE_BYTES;

use crate::TransportError;

/// One bidirectional QUIC stream carrying length-prefixed messages.
pub struct MessageStream {
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    /// Kept so a stream failure can say *why* the connection went away.
    ///
    /// Without it every remote refusal reads as "connection lost", which is true and
    /// useless: the interesting case — the peer has not paired this machine, so it
    /// rejected the certificate after the handshake looked fine from this side — is
    /// indistinguishable from a pulled cable. The connection knows the reason; the
    /// stream is just where the operator finds out.
    conn: quinn::Connection,
}

impl MessageStream {
    pub(crate) fn new(
        send: quinn::SendStream,
        recv: quinn::RecvStream,
        conn: quinn::Connection,
    ) -> Self {
        MessageStream { send, recv, conn }
    }

    /// A stream error, with the connection's own explanation when it has one.
    fn stream_error(&self, detail: impl std::fmt::Display) -> TransportError {
        match self.conn.close_reason() {
            Some(reason) => TransportError::Stream(format!("{detail} ({reason})")),
            None => TransportError::Stream(detail.to_string()),
        }
    }

    /// Send one message.
    pub async fn send(&mut self, payload: &[u8]) -> Result<(), TransportError> {
        if payload.len() > MAX_MESSAGE_BYTES {
            return Err(TransportError::MessageTooLarge {
                len: payload.len(),
                limit: MAX_MESSAGE_BYTES,
            });
        }
        let len = payload.len() as u32;
        // Written as one call rather than two so a length can never reach the peer
        // without its payload behind it — a peer that then blocks waiting for bytes that
        // are still in this machine's buffer looks exactly like a network stall.
        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&len.to_be_bytes());
        framed.extend_from_slice(payload);
        self.send
            .write_all(&framed)
            .await
            .map_err(|e| self.stream_error(e))?;
        Ok(())
    }

    /// Read one message, or `None` when the peer closed the stream cleanly.
    pub async fn recv(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        let mut header = [0u8; 4];
        match self.recv.read_exact(&mut header).await {
            Ok(()) => {}
            // A stream that ends exactly on a message boundary is a normal goodbye.
            Err(quinn::ReadExactError::FinishedEarly(0)) => return Ok(None),
            Err(e) => return Err(self.stream_error(e)),
        }

        let len = u32::from_be_bytes(header) as usize;
        // Checked before the allocation, not after it.
        if len > MAX_MESSAGE_BYTES {
            return Err(TransportError::MessageTooLarge {
                len,
                limit: MAX_MESSAGE_BYTES,
            });
        }

        let mut payload = vec![0u8; len];
        if let Err(e) = self.recv.read_exact(&mut payload).await {
            // A truncated payload is an error and not an end of stream: the peer said
            // how many bytes were coming and then did not send them.
            return Err(self.stream_error(format!("truncated message: {e}")));
        }
        Ok(Some(payload))
    }

    /// Finish sending and wait until the peer has actually received it.
    ///
    /// The wait is not optional politeness. `finish` only marks the stream complete
    /// locally; if the connection is then dropped — which happens the moment the last
    /// handle to it goes out of scope — QUIC sends CONNECTION_CLOSE and any bytes still
    /// in flight are discarded. The symptom is a peer that reads "connection lost"
    /// instead of the reply that was, from the sender's point of view, definitely sent.
    ///
    /// So this resolves once the peer has acknowledged every byte, and a caller that
    /// awaits it may drop the connection safely.
    pub async fn finish(&mut self) -> Result<(), TransportError> {
        self.send.finish().map_err(|e| self.stream_error(e))?;
        self.send
            .stopped()
            .await
            .map(|_| ())
            .map_err(|e| TransportError::Stream(e.to_string()))
    }
}
