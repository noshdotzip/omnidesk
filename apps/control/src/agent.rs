//! What this app can learn by asking its own agent.
//!
//! # Why ask at all when the hardware is right here
//! For the *local* machine the app could read the hardware directly, and did. It cannot
//! for the other machine: it holds no device identity, no pinned keys and no peer
//! connection, all of which live in the agent. So every peer panel was a placeholder —
//! not for want of an answer but for want of anyone to ask.
//!
//! The local answers come through the same connection anyway, and that is deliberate.
//! Two enumerators on one machine already disagreed by a factor of 1.5, because the agent
//! had never declared DPI awareness and Windows had been quietly scaling everything it
//! was shown (`ultidesk_platform_windows::dpi`). The bug is fixed, but the *class* of bug
//! only goes away when there is one source. What the agent says is the version a peer is
//! told, so it is the version the editor should arrange.
//!
//! # No agent is a normal state, not an error
//! The app is useful without one — the arrangement editor still works on whatever is
//! saved. So everything here returns a note rather than failing, and the UI says which
//! parts are real. Silently showing stale or invented geometry would be worse than
//! showing nothing and saying why.

use std::time::Duration;

use ultidesk_identity::PeerKey;
use ultidesk_ipc::{Client, ClientError, IpcRequest, IpcResponse, PeerQuery};
use ultidesk_topology::{AudioDevice, Monitor};

/// What one question to the agent produced.
pub struct Answer<T> {
    pub value: Option<T>,
    /// Why there is no value, or something the operator should know despite there being
    /// one. Shown in the UI rather than logged.
    pub note: Option<String>,
}

impl<T> Answer<T> {
    fn missing(note: String) -> Self {
        Answer {
            value: None,
            note: Some(note),
        }
    }
}

/// Everything the app asks for on a refresh, in one connection.
///
/// One connection rather than one per question: the agent authenticates per connection,
/// so asking four things separately means authenticating four times, and a peer that
/// went away between two of them would produce an inconsistent picture.
pub struct AgentView {
    pub local_monitors: Answer<Vec<Monitor>>,
    pub local_audio: Answer<Vec<AudioDevice>>,
    pub peer: Option<PeerView>,
    /// Set when there is no agent to ask at all.
    pub note: Option<String>,
}

/// What the agent could tell us about the other machine.
///
/// No key field: the caller named the peer it asked about, so echoing it back would only
/// be somewhere for the two to drift apart.
pub struct PeerView {
    pub name: String,
    pub monitors: Answer<Vec<Monitor>>,
    pub audio: Answer<Vec<AudioDevice>>,
}

/// Ask the local agent for everything the panels need.
///
/// `peer` is the device to ask about, when one is paired. Passing `None` skips the peer
/// questions entirely rather than asking and discarding — a relay to a machine that is
/// asleep costs the connection's whole timeout.
pub async fn fetch(peer: Option<(PeerKey, String)>) -> AgentView {
    let mut client = match Client::connect_with_timeout(Duration::from_secs(5)).await {
        Ok(client) => client,
        Err(e) => {
            return AgentView {
                local_monitors: Answer::missing(String::new()),
                local_audio: Answer::missing(String::new()),
                peer: None,
                note: Some(describe(&e)),
            }
        }
    };

    let local_monitors = ask(&mut client, IpcRequest::ListMonitors, |r| match r {
        IpcResponse::Monitors { monitors } => Some(monitors),
        _ => None,
    })
    .await;
    let local_audio = ask(&mut client, IpcRequest::ListAudioDevices, |r| match r {
        IpcResponse::AudioDevices { devices } => Some(devices),
        _ => None,
    })
    .await;

    let peer = match peer {
        Some((key, name)) => {
            let monitors = ask(
                &mut client,
                IpcRequest::AskPeer {
                    peer: key,
                    query: PeerQuery::Monitors,
                },
                |r| match r {
                    IpcResponse::PeerMonitors { monitors, .. } => Some(monitors),
                    _ => None,
                },
            )
            .await;
            let audio = ask(
                &mut client,
                IpcRequest::AskPeer {
                    peer: key,
                    query: PeerQuery::AudioDevices,
                },
                |r| match r {
                    IpcResponse::PeerAudioDevices { devices, .. } => Some(devices),
                    _ => None,
                },
            )
            .await;
            Some(PeerView {
                name,
                monitors,
                audio,
            })
        }
        None => None,
    };

    AgentView {
        local_monitors,
        local_audio,
        peer,
        note: None,
    }
}

async fn ask<T>(
    client: &mut Client,
    request: IpcRequest,
    extract: impl FnOnce(IpcResponse) -> Option<T>,
) -> Answer<T> {
    match client.request(request).await {
        Ok(response) => match extract(response) {
            Some(value) => Answer {
                value: Some(value),
                note: None,
            },
            None => Answer::missing("the agent answered with something unexpected".into()),
        },
        Err(e) => Answer::missing(describe(&e)),
    }
}

/// A client error as something an operator can act on.
///
/// Worth spelling out rather than printing the error: "no agent is running" and "the peer
/// did not answer" lead to completely different next steps, and the raw messages bury
/// that under transport detail.
fn describe(e: &ClientError) -> String {
    match e {
        ClientError::NoAgent { .. } => {
            "no agent is running on this machine, so nothing here is live — start \
             `ultidesk-agent serve`"
                .into()
        }
        ClientError::Refused { code, message } if code == "peer_address_unknown" => message.clone(),
        ClientError::Refused { code, message } if code == "peer_unreachable" => {
            format!("the peer did not answer: {message}")
        }
        ClientError::TimedOut(_) => "the agent did not answer in time".into(),
        other => other.to_string(),
    }
}
