//! Talking to the agent running on this machine.
//!
//! # Why a client at all
//! The control app can read its own hardware directly, and did. What it cannot do
//! directly is anything involving the *other* machine: it holds no device identity, no
//! pinned keys, and no peer connection. Those live in the agent, so asking the agent is
//! the only way its peer panels stop being placeholders.
//!
//! Once the connection exists, the app should also stop reading its own monitors through
//! a window toolkit. Two enumerators on one machine already disagreed once — by a factor
//! of 1.5, because one of the two processes had never declared DPI awareness (see
//! `ultidesk_platform_windows::dpi`). Having one source removes that class of bug rather
//! than leaving it to be noticed again.
//!
//! # One connection, one session
//! The agent authenticates per connection and releases any input a connection was holding
//! when it drops. So a [`Client`] owns its connection for its lifetime rather than
//! reconnecting per request: reconnecting would re-authenticate every time and, worse,
//! make "the connection dropped" invisible to the caller.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::endpoint::{read_handshake, Endpoint};
use crate::message::{IpcRequest, IpcResponse};
use ultidesk_core::protocol::{MAX_MESSAGE_BYTES, PROTOCOL_VERSION};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// No agent is listening.
    ///
    /// Covers both shapes of it: no handshake file at all, and a handshake file left
    /// behind by an agent that has since died. They are the same situation for a caller
    /// — nothing to talk to — and the raw OS error for the second one ("the system
    /// cannot find the file specified") describes a pipe nobody asked about.
    #[error("no agent is running on this machine ({detail})")]
    NoAgent { detail: String },
    #[error("could not read the agent handshake: {0}")]
    Handshake(String),
    #[error("could not reach the agent at {address}: {source}")]
    Connect {
        address: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the agent refused the connection: {code}: {message}")]
    Refused { code: String, message: String },
    #[error("the agent went away mid-request")]
    Disconnected,
    #[error("io error talking to the agent: {0}")]
    Io(#[from] std::io::Error),
    #[error("the agent sent something unreadable: {0}")]
    Malformed(String),
    #[error("the agent did not answer within {0:?}")]
    TimedOut(Duration),
}

/// An authenticated connection to this machine's agent.
pub struct Client {
    stream: Stream,
    /// How long to wait for one answer.
    timeout: Duration,
}

impl Client {
    /// Find the agent through its handshake file, connect, and authenticate.
    pub async fn connect() -> Result<Self, ClientError> {
        Self::connect_with_timeout(DEFAULT_TIMEOUT).await
    }

    /// As [`Client::connect`], with a deadline for every request on this connection.
    ///
    /// A deadline matters here more than it looks: a request may be a *relay*, and the
    /// agent will sit waiting on a peer that has gone away. A control app that blocks for
    /// ever on a panel refresh is indistinguishable from one that has crashed.
    pub async fn connect_with_timeout(timeout: Duration) -> Result<Self, ClientError> {
        let endpoint = load_endpoint()?;
        let stream = match Stream::connect(&endpoint.endpoint_path).await {
            Ok(stream) => stream,
            // A handshake file whose endpoint refuses is one an agent left behind when it
            // died. Reported as "no agent" because that is what it means, with the stale
            // path named so it can be cleared if it is in the way.
            Err(ClientError::Connect { address, source })
                if matches!(
                    source.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Err(ClientError::NoAgent {
                    detail: format!("a handshake file names {address}, but nothing is listening"),
                })
            }
            Err(e) => return Err(e),
        };
        let mut client = Client { stream, timeout };
        client.hello(&endpoint.token).await?;
        Ok(client)
    }

    /// Ask the agent something.
    ///
    /// An `Error` response comes back as [`ClientError::Refused`] rather than an `Ok`
    /// carrying a failure, so a caller cannot accidentally treat a refusal as an answer.
    pub async fn request(&mut self, request: IpcRequest) -> Result<IpcResponse, ClientError> {
        match self.send(request).await? {
            IpcResponse::Error { code, message } => Err(ClientError::Refused { code, message }),
            other => Ok(other),
        }
    }

    async fn hello(&mut self, token: &str) -> Result<(), ClientError> {
        let response = self
            .send(IpcRequest::Hello {
                token: token.to_string(),
                protocol_version: PROTOCOL_VERSION,
            })
            .await?;
        match response {
            IpcResponse::HelloOk { .. } => Ok(()),
            IpcResponse::Error { code, message } => Err(ClientError::Refused { code, message }),
            other => Err(ClientError::Malformed(format!(
                "expected HelloOk, got {other:?}"
            ))),
        }
    }

    async fn send(&mut self, request: IpcRequest) -> Result<IpcResponse, ClientError> {
        let mut line =
            serde_json::to_string(&request).map_err(|e| ClientError::Malformed(e.to_string()))?;
        line.push('\n');

        let exchange = async {
            self.stream.write_all(line.as_bytes()).await?;
            self.stream.flush().await?;
            self.stream.read_line().await
        };
        let raw = tokio::time::timeout(self.timeout, exchange)
            .await
            .map_err(|_| ClientError::TimedOut(self.timeout))??;

        let raw = raw.ok_or(ClientError::Disconnected)?;
        serde_json::from_str(raw.trim()).map_err(|e| ClientError::Malformed(e.to_string()))
    }
}

/// Long enough for a relay to a peer on a slow link, short enough that a wedged agent
/// does not look like a hung application. The measured round trip to the peer on this
/// desk is tens of milliseconds; the margin is for a peer that is *not* answering.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

fn load_endpoint() -> Result<Endpoint, ClientError> {
    let path = crate::endpoint::handshake_path();
    match read_handshake(&path) {
        Ok(endpoint) => Ok(endpoint),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ClientError::NoAgent {
            detail: format!("no handshake file at {}", path.display()),
        }),
        Err(e) => Err(ClientError::Handshake(format!("{}: {e}", path.display()))),
    }
}

/// The per-platform door, behind one name.
///
/// Both are byte streams carrying the same newline-delimited JSON, so everything above
/// this is platform-independent — the split is four functions wide, not a second client.
enum Stream {
    #[cfg(windows)]
    Pipe(BufReader<tokio::net::windows::named_pipe::NamedPipeClient>),
    #[cfg(unix)]
    Socket(BufReader<tokio::net::UnixStream>),
}

impl Stream {
    async fn connect(address: &str) -> Result<Self, ClientError> {
        #[cfg(windows)]
        {
            let pipe = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(address)
                .map_err(|source| ClientError::Connect {
                    address: address.to_string(),
                    source,
                })?;
            Ok(Stream::Pipe(BufReader::new(pipe)))
        }
        #[cfg(unix)]
        {
            let socket = tokio::net::UnixStream::connect(std::path::Path::new(address))
                .await
                .map_err(|source| ClientError::Connect {
                    address: address.to_string(),
                    source,
                })?;
            Ok(Stream::Socket(BufReader::new(socket)))
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = address;
            Err(ClientError::Handshake(
                "no local IPC transport exists for this platform".into(),
            ))
        }
    }

    async fn write_all(&mut self, bytes: &[u8]) -> Result<(), std::io::Error> {
        match self {
            #[cfg(windows)]
            Stream::Pipe(s) => s.get_mut().write_all(bytes).await,
            #[cfg(unix)]
            Stream::Socket(s) => s.get_mut().write_all(bytes).await,
        }
    }

    async fn flush(&mut self) -> Result<(), std::io::Error> {
        match self {
            #[cfg(windows)]
            Stream::Pipe(s) => s.get_mut().flush().await,
            #[cfg(unix)]
            Stream::Socket(s) => s.get_mut().flush().await,
        }
    }

    /// One line, or `None` at a clean end of stream.
    ///
    /// Bounded so a confused or hostile agent cannot make this process allocate without
    /// limit — the same cap the agent applies to what it reads.
    async fn read_line(&mut self) -> Result<Option<String>, ClientError> {
        let mut line = String::new();
        let read = match self {
            #[cfg(windows)]
            Stream::Pipe(s) => {
                s.take(MAX_MESSAGE_BYTES as u64)
                    .read_line(&mut line)
                    .await?
            }
            #[cfg(unix)]
            Stream::Socket(s) => {
                s.take(MAX_MESSAGE_BYTES as u64)
                    .read_line(&mut line)
                    .await?
            }
        };
        if read == 0 {
            return Ok(None);
        }
        Ok(Some(line))
    }
}
