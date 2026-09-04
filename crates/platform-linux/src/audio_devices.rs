//! Enumerate the machine's PipeWire audio endpoints.
//!
//! Walks the PipeWire registry directly rather than shelling out to `pw-dump` and
//! parsing its JSON. The subprocess would work, but it serialises every object in the
//! graph — nodes, ports, links, devices, factories — to find the handful of audio nodes
//! we want, and it makes the settings panel depend on the CLI tools being installed.
//! The `pipewire` crate is already a dependency for capture, so the native walk costs
//! nothing extra.
//!
//! # Why this needs two round-trips
//! A `sync` round-trip tells us every global that existed at connect time has been
//! delivered. But the default sink/source do not live on the nodes — they live on a
//! metadata object, which we can only bind *after* the registry hands it to us, i.e.
//! part-way through that first round. Its property events are therefore queued behind
//! the first `done`. Quitting there enumerates every device with `is_default: false`.
//! So the first `done` issues a second `sync`, and only the second one ends the loop.
//!
//! # Why this is bounded by a timer
//! The loop runs until those round-trips come back. If the PipeWire daemon is wedged,
//! they never do. A settings panel that hangs forever is worse than one that reports
//! "could not read the audio devices", so the loop is force-quit at a deadline.

use serde::{Deserialize, Serialize};

/// Whether an endpoint plays audio or records it.
///
/// Mirrors PipeWire's `media.class`: `Audio/Sink` is something you play *to*, and
/// capturing it means capturing its monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PwKind {
    Sink,
    Source,
}

/// One PipeWire audio endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PwDevice {
    /// `node.name` — the string `pw-record --target=` and `pw-play --target=` expect.
    pub node: String,
    /// `node.description`, falling back to the node name when a device sets no
    /// description. An empty label in a device picker is unusable.
    pub description: String,
    pub kind: PwKind,
    pub is_default: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AudioEnumError {
    #[error("this build has no PipeWire support (not a Linux target)")]
    Unsupported,
    #[error("could not reach the PipeWire daemon: {0}")]
    Connect(String),
    #[error("timed out reading the PipeWire registry")]
    TimedOut,
}

/// Pull the `name` out of a `default.audio.sink` metadata value.
///
/// PipeWire stores these as a JSON object (`{"name":"alsa_output..."}`) rather than a
/// bare string, so the raw value is not itself a node name. Kept separate from the
/// registry walk so it is testable on every platform — this is the part that silently
/// mismatches if the encoding is assumed rather than parsed.
pub fn default_node_from_metadata(value: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(value).ok()?;
    match v {
        // The documented shape.
        serde_json::Value::Object(map) => map.get("name")?.as_str().map(str::to_owned),
        // Tolerated: a bare string. Cheap to accept, and treating it as "no default"
        // would silently un-mark the default device.
        serde_json::Value::String(s) => Some(s),
        _ => None,
    }
}

/// Classify a PipeWire `media.class` into something the routing model understands.
///
/// Returns `None` for video and for stream nodes: an application's own playback stream
/// is not an endpoint you can route to, and offering one in the picker would produce a
/// route that vanishes when the application closes.
pub fn kind_from_media_class(media_class: &str) -> Option<PwKind> {
    match media_class {
        "Audio/Sink" => Some(PwKind::Sink),
        "Audio/Source" | "Audio/Source/Virtual" => Some(PwKind::Source),
        _ => None,
    }
}

/// How long to wait for the registry before giving up.
#[cfg(target_os = "linux")]
const ENUMERATE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

/// Read every audio sink and source this machine exposes.
#[cfg(target_os = "linux")]
pub fn enumerate() -> Result<Vec<PwDevice>, AudioEnumError> {
    imp::enumerate()
}

#[cfg(not(target_os = "linux"))]
pub fn enumerate() -> Result<Vec<PwDevice>, AudioEnumError> {
    Err(AudioEnumError::Unsupported)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use pipewire::metadata::{Metadata, MetadataListener};
    use pipewire::types::ObjectType;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct Collected {
        devices: Vec<PwDevice>,
        default_sink: Option<String>,
        default_source: Option<String>,
    }

    pub fn enumerate() -> Result<Vec<PwDevice>, AudioEnumError> {
        pipewire::init();

        // The `Rc` variants, matching `pipewire_capture`. The weak handles below are the
        // reason they are needed: a listener that owns the object it is registered on is
        // a reference cycle, and the core would never be freed.
        let mainloop = pipewire::main_loop::MainLoopRc::new(None)
            .map_err(|e| AudioEnumError::Connect(e.to_string()))?;
        let context = pipewire::context::ContextRc::new(&mainloop, None)
            .map_err(|e| AudioEnumError::Connect(e.to_string()))?;
        let core = context
            .connect_rc(None)
            .map_err(|e| AudioEnumError::Connect(e.to_string()))?;
        let registry = core
            .get_registry_rc()
            .map_err(|e| AudioEnumError::Connect(e.to_string()))?;

        let collected = Rc::new(RefCell::new(Collected::default()));

        // Both the proxy and its listener must outlive the loop: dropping either
        // unsubscribes it, and the default-device properties would never arrive.
        let metadata_keep: Rc<RefCell<Vec<Metadata>>> = Rc::new(RefCell::new(Vec::new()));
        let metadata_listeners: Rc<RefCell<Vec<MetadataListener>>> =
            Rc::new(RefCell::new(Vec::new()));

        let _registry_listener = {
            let collected = collected.clone();
            let registry_weak = registry.downgrade();
            let metadata_keep = metadata_keep.clone();
            let metadata_listeners = metadata_listeners.clone();
            registry
                .add_listener_local()
                .global(move |global| {
                    let Some(props) = global.props else {
                        return;
                    };

                    match global.type_ {
                        ObjectType::Node => {
                            let Some(kind) =
                                props.get("media.class").and_then(kind_from_media_class)
                            else {
                                return;
                            };
                            let Some(node) = props.get("node.name") else {
                                return;
                            };
                            let description = props
                                .get("node.description")
                                .filter(|d| !d.is_empty())
                                .unwrap_or(node)
                                .to_owned();
                            collected.borrow_mut().devices.push(PwDevice {
                                node: node.to_owned(),
                                description,
                                kind,
                                // Filled in at the end, once the metadata object has
                                // reported the defaults.
                                is_default: false,
                            });
                        }
                        ObjectType::Metadata => {
                            // Only the object literally named "default" carries the
                            // default sink/source; the others hold unrelated settings
                            // (volumes, restore policy) and binding them is wasted work.
                            if props.get("metadata.name") != Some("default") {
                                return;
                            }
                            let Some(registry) = registry_weak.upgrade() else {
                                return;
                            };
                            let Ok(metadata) = registry.bind::<Metadata, _>(global) else {
                                return;
                            };
                            let collected = collected.clone();
                            let listener = metadata
                                .add_listener_local()
                                .property(move |_subject, key, _type_, value| {
                                    if let (Some(key), Some(value)) = (key, value) {
                                        let parsed = default_node_from_metadata(value);
                                        let mut c = collected.borrow_mut();
                                        match key {
                                            "default.audio.sink" => c.default_sink = parsed,
                                            "default.audio.source" => c.default_source = parsed,
                                            _ => {}
                                        }
                                    }
                                    0
                                })
                                .register();
                            metadata_keep.borrow_mut().push(metadata);
                            metadata_listeners.borrow_mut().push(listener);
                        }
                        _ => {}
                    }
                })
                .register()
        };

        let first = core
            .sync(0)
            .map_err(|e| AudioEnumError::Connect(e.to_string()))?;
        let pending = Rc::new(RefCell::new(first));
        let second_round = Rc::new(RefCell::new(false));
        let done = Rc::new(RefCell::new(false));

        let _core_listener = {
            let mainloop = mainloop.clone();
            let core_weak = core.downgrade();
            let pending = pending.clone();
            let second_round = second_round.clone();
            let done = done.clone();
            core.add_listener_local()
                .done(move |id, seq| {
                    if id != pipewire::core::PW_ID_CORE {
                        return;
                    }
                    // Copy out before taking any mutable borrow: holding both at once
                    // panics at runtime.
                    let expected = *pending.borrow();
                    if seq != expected {
                        return;
                    }

                    let first_round = !*second_round.borrow();
                    if first_round {
                        *second_round.borrow_mut() = true;
                        // The metadata proxy was bound during this round, so its
                        // property events are queued behind us. A second round-trip
                        // lands after them.
                        if let Some(core) = core_weak.upgrade() {
                            if let Ok(next) = core.sync(0) {
                                *pending.borrow_mut() = next;
                                return;
                            }
                        }
                        // If the second sync cannot be issued, finishing with no
                        // defaults beats hanging until the deadline.
                    }

                    *done.borrow_mut() = true;
                    mainloop.quit();
                })
                .register()
        };

        // Not belt-and-braces: without this a wedged daemon hangs the caller forever.
        let timer = {
            let mainloop_for_quit = mainloop.clone();
            mainloop.loop_().add_timer(move |_| {
                mainloop_for_quit.quit();
            })
        };
        timer
            .update_timer(Some(ENUMERATE_DEADLINE), None)
            .into_result()
            .map_err(|e| AudioEnumError::Connect(format!("{e:?}")))?;

        mainloop.run();

        if !*done.borrow() {
            return Err(AudioEnumError::TimedOut);
        }

        let mut collected = collected.borrow_mut();
        let default_sink = collected.default_sink.clone();
        let default_source = collected.default_source.clone();
        for d in &mut collected.devices {
            d.is_default = match d.kind {
                PwKind::Sink => Some(&d.node) == default_sink.as_ref(),
                PwKind::Source => Some(&d.node) == default_source.as_ref(),
            };
        }
        // Stable order so the picker does not reshuffle between openings: registry
        // delivery order is not guaranteed.
        collected
            .devices
            .sort_by(|a, b| (a.kind, &a.description).cmp(&(b.kind, &b.description)));
        Ok(collected.devices.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_device_is_named_inside_a_json_object_not_stored_bare() {
        // The value really is `{"name":"..."}`, as read off the running daemon.
        // Treating it as a bare node name would mark nothing as default, silently.
        assert_eq!(
            default_node_from_metadata(
                r#"{"name":"alsa_output.pci-0000_00_1f.3.HiFi__Speaker__sink"}"#
            ),
            Some("alsa_output.pci-0000_00_1f.3.HiFi__Speaker__sink".to_string())
        );
    }

    #[test]
    fn a_bare_string_value_is_tolerated() {
        assert_eq!(
            default_node_from_metadata(r#""alsa_output.speaker""#),
            Some("alsa_output.speaker".to_string())
        );
    }

    #[test]
    fn a_cleared_or_malformed_default_yields_no_name_rather_than_a_panic() {
        assert_eq!(default_node_from_metadata("null"), None);
        assert_eq!(default_node_from_metadata(""), None);
        assert_eq!(default_node_from_metadata("not json"), None);
        assert_eq!(default_node_from_metadata("{}"), None);
        assert_eq!(default_node_from_metadata(r#"{"name":42}"#), None);
    }

    #[test]
    fn sinks_and_sources_are_classified() {
        assert_eq!(kind_from_media_class("Audio/Sink"), Some(PwKind::Sink));
        assert_eq!(kind_from_media_class("Audio/Source"), Some(PwKind::Source));
        assert_eq!(
            kind_from_media_class("Audio/Source/Virtual"),
            Some(PwKind::Source)
        );
    }

    #[test]
    fn application_streams_are_not_routable_endpoints() {
        // A stream node is one application's playback, not a device. Routing to one
        // would break as soon as that application quits.
        assert_eq!(kind_from_media_class("Stream/Output/Audio"), None);
        assert_eq!(kind_from_media_class("Stream/Input/Audio"), None);
        assert_eq!(kind_from_media_class("Video/Source"), None);
        assert_eq!(kind_from_media_class(""), None);
    }
}
