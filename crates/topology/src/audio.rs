//! Audio device inventory and the routing graph between machines.
//!
//! A route captures audio on one machine and plays it on another, which is the
//! "route sound from different machines" half of the product. The rules here are what
//! stop a routing table from destroying someone's hearing.
//!
//! # Why cycle detection is the whole point of this module
//! Capturing an output device means capturing its *monitor* — everything currently
//! playing on it. If that stream is then played back onto the same output, the playback
//! is itself captured and sent again, and again, gaining each pass. That is a runaway
//! feedback loop, and at headphone volume it is genuinely dangerous rather than merely
//! annoying.
//!
//! Two details decide whether a given table can run away, and both are easy to get
//! wrong:
//!
//! * **It is per-device, not per-machine.** Capturing sink X and playing to sink Y on
//!   the same machine is fine — Y's audio never reaches X's monitor. Rejecting
//!   same-machine routes outright would forbid a legitimate setup (moving game audio
//!   from one output to another) for no reason.
//! * **Only output-sourced routes can close a loop.** A microphone is not a monitor;
//!   playing a mic onto the machine's own speakers cannot re-enter the capture
//!   digitally. (It can howl acoustically, but that is a room problem, not this
//!   module's, and blocking it would forbid legitimate intercom setups.)
//!
//! So the graph's nodes are individual devices and its edges are output-sourced routes.
//! Any cycle in that graph is a runaway; anything else is allowed.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use ultidesk_core::DeviceId;

/// What a device is for, which decides whether capturing it can feed back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeviceKind {
    /// A speaker/headphone endpoint. Capturing one means capturing its monitor —
    /// everything playing on it — which is what makes loops possible.
    Output,
    /// A microphone or line-in. Capturing one picks up the room, not the mix.
    Input,
}

/// One audio endpoint on one machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    /// Which machine the device is attached to.
    pub device_id: DeviceId,
    /// The platform's own identifier, passed through to capture/playback verbatim.
    ///
    /// On Linux this is the PipeWire node name; on Windows it is the WASAPI endpoint id,
    /// or empty for "whatever the default is". It is opaque here on purpose — this
    /// module must not grow platform parsing.
    pub node: String,
    pub name: String,
    pub kind: DeviceKind,
    /// Whether the platform considers this the default endpoint for its kind.
    pub is_default: bool,
}

impl AudioDevice {
    pub fn key(&self) -> DeviceKey {
        DeviceKey {
            device_id: self.device_id,
            node: self.node.clone(),
        }
    }
}

/// Identity of one endpoint: a machine plus a node on it.
///
/// The machine alone is not enough — a machine has several outputs and they do not
/// feed into each other.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DeviceKey {
    pub device_id: DeviceId,
    pub node: String,
}

/// "Capture `source`, play it on `sink`."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub source: DeviceKey,
    pub sink: DeviceKey,
}

/// Why a routing table was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    /// The route would create a runaway feedback loop. Carries the devices in the cycle,
    /// in order, so the UI can say which ones rather than just refusing.
    FeedbackLoop(Vec<DeviceKey>),
    /// The same source/sink pair is already routed. Adding it twice would send two
    /// copies of the same audio and double its volume.
    Duplicate,
    /// A route names a device that is not in the inventory — typically unplugged since
    /// the table was saved.
    UnknownDevice(DeviceKey),
    /// The sink is a microphone. Audio cannot be played into an input.
    SinkIsNotAnOutput(DeviceKey),
}

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouteError::FeedbackLoop(cycle) => {
                write!(f, "would create an audio feedback loop: ")?;
                for (i, k) in cycle.iter().enumerate() {
                    if i > 0 {
                        write!(f, " -> ")?;
                    }
                    write!(f, "{}", k.node)?;
                }
                Ok(())
            }
            RouteError::Duplicate => write!(f, "that route already exists"),
            RouteError::UnknownDevice(k) => {
                write!(f, "no such audio device: {} on {}", k.node, k.device_id)
            }
            RouteError::SinkIsNotAnOutput(k) => {
                write!(f, "{} is an input; audio cannot be played into it", k.node)
            }
        }
    }
}

impl std::error::Error for RouteError {}

/// The full audio picture: which devices exist, and what is routed where.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AudioRouting {
    pub devices: Vec<AudioDevice>,
    routes: Vec<Route>,
}

impl AudioRouting {
    pub fn new(devices: Vec<AudioDevice>) -> Self {
        AudioRouting {
            devices,
            routes: Vec::new(),
        }
    }

    pub fn routes(&self) -> &[Route] {
        &self.routes
    }

    pub fn device(&self, key: &DeviceKey) -> Option<&AudioDevice> {
        self.devices.iter().find(|d| &d.key() == key)
    }

    /// Devices on one machine, for grouping in the UI.
    pub fn devices_on(&self, device_id: DeviceId) -> impl Iterator<Item = &AudioDevice> {
        self.devices
            .iter()
            .filter(move |d| d.device_id == device_id)
    }

    /// Check a route without adding it, so the UI can grey out impossible choices
    /// instead of letting the operator pick one and then refusing.
    pub fn check(&self, route: &Route) -> Result<(), RouteError> {
        if self.device(&route.source).is_none() {
            return Err(RouteError::UnknownDevice(route.source.clone()));
        }
        let Some(dst) = self.device(&route.sink) else {
            return Err(RouteError::UnknownDevice(route.sink.clone()));
        };
        if dst.kind != DeviceKind::Output {
            return Err(RouteError::SinkIsNotAnOutput(route.sink.clone()));
        }
        if self.routes.contains(route) {
            return Err(RouteError::Duplicate);
        }
        // Test the graph as it *would* be, not as it is: a route is only a loop in
        // combination with the ones already present.
        let mut candidate = self.routes.clone();
        candidate.push(route.clone());
        if let Some(cycle) = self.find_cycle(&candidate) {
            return Err(RouteError::FeedbackLoop(cycle));
        }
        Ok(())
    }

    /// Render an error with device names instead of raw node ids.
    ///
    /// [`RouteError`] cannot do this itself: it does not carry the inventory, and on
    /// Windows a node id is an opaque GUID. `Display` alone therefore produces a
    /// message no operator can act on — it names the loop without saying which speaker.
    pub fn explain(&self, err: &RouteError) -> String {
        match err {
            RouteError::FeedbackLoop(cycle) => {
                let names: Vec<String> = cycle.iter().map(|k| self.name_of(k)).collect();
                format!(
                    "would create an audio feedback loop: {}",
                    names.join(" -> ")
                )
            }
            RouteError::UnknownDevice(k) => format!("no such audio device: {}", self.name_of(k)),
            RouteError::SinkIsNotAnOutput(k) => format!(
                "{} is an input; audio cannot be played into it",
                self.name_of(k)
            ),
            RouteError::Duplicate => err.to_string(),
        }
    }

    /// A device's friendly name, falling back to its node id when it is gone.
    fn name_of(&self, key: &DeviceKey) -> String {
        self.device(key)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| format!("{} (missing)", key.node))
    }

    pub fn add(&mut self, route: Route) -> Result<(), RouteError> {
        self.check(&route)?;
        self.routes.push(route);
        Ok(())
    }

    pub fn remove(&mut self, route: &Route) -> bool {
        let before = self.routes.len();
        self.routes.retain(|r| r != route);
        self.routes.len() != before
    }

    /// Routes naming a device that is no longer present.
    ///
    /// Returned rather than silently dropped: audio that stops because a device was
    /// unplugged should be visible in the UI, not a mystery.
    pub fn stale_routes(&self) -> Vec<&Route> {
        self.routes
            .iter()
            .filter(|r| self.device(&r.source).is_none() || self.device(&r.sink).is_none())
            .collect()
    }

    /// Depth-first search for a cycle among output-sourced routes.
    ///
    /// Only output-sourced edges are walked: see the module docs for why a microphone
    /// cannot close a loop.
    fn find_cycle(&self, routes: &[Route]) -> Option<Vec<DeviceKey>> {
        let mut edges: HashMap<DeviceKey, Vec<DeviceKey>> = HashMap::new();
        for r in routes {
            let sourced_from_output = self
                .device(&r.source)
                .map(|d| d.kind == DeviceKind::Output)
                .unwrap_or(false);
            if sourced_from_output {
                edges
                    .entry(r.source.clone())
                    .or_default()
                    .push(r.sink.clone());
            }
        }

        let mut visited: HashSet<DeviceKey> = HashSet::new();
        let mut stack: Vec<DeviceKey> = Vec::new();
        let mut on_stack: HashSet<DeviceKey> = HashSet::new();

        // Sorted so a cycle is reported identically run to run; HashMap order is not
        // stable and a test that depended on it would flake.
        let mut starts: Vec<DeviceKey> = edges.keys().cloned().collect();
        starts.sort();

        for start in &starts {
            if visited.contains(start) {
                continue;
            }
            if let Some(cycle) = dfs(start, &edges, &mut visited, &mut stack, &mut on_stack) {
                return Some(cycle);
            }
        }
        None
    }
}

fn dfs(
    node: &DeviceKey,
    edges: &HashMap<DeviceKey, Vec<DeviceKey>>,
    visited: &mut HashSet<DeviceKey>,
    stack: &mut Vec<DeviceKey>,
    on_stack: &mut HashSet<DeviceKey>,
) -> Option<Vec<DeviceKey>> {
    visited.insert(node.clone());
    stack.push(node.clone());
    on_stack.insert(node.clone());

    if let Some(next) = edges.get(node) {
        for n in next {
            if on_stack.contains(n) {
                // Return the cycle itself rather than a bare "true": the UI needs to
                // name the devices involved for the message to be actionable.
                let at = stack.iter().position(|k| k == n).unwrap_or(0);
                let mut cycle = stack[at..].to_vec();
                cycle.push(n.clone());
                return Some(cycle);
            }
            if !visited.contains(n) {
                if let Some(cycle) = dfs(n, edges, visited, stack, on_stack) {
                    return Some(cycle);
                }
            }
        }
    }

    stack.pop();
    on_stack.remove(node);
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(machine: DeviceId, node: &str, kind: DeviceKind) -> AudioDevice {
        AudioDevice {
            device_id: machine,
            node: node.into(),
            name: node.into(),
            kind,
            is_default: false,
        }
    }

    fn key(machine: DeviceId, node: &str) -> DeviceKey {
        DeviceKey {
            device_id: machine,
            node: node.into(),
        }
    }

    fn route(from: DeviceKey, to: DeviceKey) -> Route {
        Route {
            source: from,
            sink: to,
        }
    }

    /// Two machines, each with a speaker, a second output and a microphone.
    fn fixture() -> (DeviceId, DeviceId, AudioRouting) {
        let a = DeviceId::new();
        let b = DeviceId::new();
        let routing = AudioRouting::new(vec![
            dev(a, "a-speakers", DeviceKind::Output),
            dev(a, "a-headset", DeviceKind::Output),
            dev(a, "a-mic", DeviceKind::Input),
            dev(b, "b-speakers", DeviceKind::Output),
            dev(b, "b-mic", DeviceKind::Input),
        ]);
        (a, b, routing)
    }

    #[test]
    fn playing_an_output_back_onto_itself_is_the_classic_runaway_and_is_rejected() {
        let (a, _b, mut r) = fixture();
        let err = r
            .add(route(key(a, "a-speakers"), key(a, "a-speakers")))
            .unwrap_err();
        assert!(matches!(err, RouteError::FeedbackLoop(_)));
    }

    #[test]
    fn two_outputs_on_one_machine_do_not_feed_each_other() {
        // The reason the check is per-device rather than per-machine: audio played on
        // the headset never reaches the speakers' monitor, so this is a legitimate
        // "move game audio to the other output" setup.
        let (a, _b, mut r) = fixture();
        r.add(route(key(a, "a-speakers"), key(a, "a-headset")))
            .expect("distinct outputs on one machine are not a loop");
    }

    #[test]
    fn a_mic_routed_to_its_own_machines_speakers_is_allowed() {
        // A microphone is not a monitor, so the played audio cannot re-enter the
        // capture digitally. Blocking this would forbid an intercom.
        let (a, _b, mut r) = fixture();
        r.add(route(key(a, "a-mic"), key(a, "a-speakers")))
            .expect("an input source cannot close a digital loop");
    }

    #[test]
    fn a_two_machine_round_trip_is_a_loop() {
        // A's mix goes to B, and B's mix comes back to A. A's playback of B lands in
        // A's monitor, which is captured again — runaway across the network.
        let (a, b, mut r) = fixture();
        r.add(route(key(a, "a-speakers"), key(b, "b-speakers")))
            .expect("first leg is fine on its own");
        let err = r
            .add(route(key(b, "b-speakers"), key(a, "a-speakers")))
            .unwrap_err();
        assert!(matches!(err, RouteError::FeedbackLoop(_)));
    }

    #[test]
    fn a_three_machine_round_trip_is_also_a_loop() {
        let a = DeviceId::new();
        let b = DeviceId::new();
        let c = DeviceId::new();
        let mut r = AudioRouting::new(vec![
            dev(a, "a-out", DeviceKind::Output),
            dev(b, "b-out", DeviceKind::Output),
            dev(c, "c-out", DeviceKind::Output),
        ]);
        r.add(route(key(a, "a-out"), key(b, "b-out"))).unwrap();
        r.add(route(key(b, "b-out"), key(c, "c-out"))).unwrap();
        let err = r.add(route(key(c, "c-out"), key(a, "a-out"))).unwrap_err();
        match err {
            RouteError::FeedbackLoop(cycle) => {
                // The reported cycle must name every machine involved, or the message
                // sends the operator to the wrong device to fix it.
                assert!(cycle.len() >= 3, "cycle was {cycle:?}");
            }
            other => panic!("expected a feedback loop, got {other:?}"),
        }
    }

    #[test]
    fn a_mic_leg_does_not_complete_an_otherwise_cyclic_path() {
        // A's mix -> B's speakers, and B's *microphone* -> A's speakers. The second leg
        // captures the room, not B's mix, so nothing returns digitally.
        let (a, b, mut r) = fixture();
        r.add(route(key(a, "a-speakers"), key(b, "b-speakers")))
            .unwrap();
        r.add(route(key(b, "b-mic"), key(a, "a-speakers")))
            .expect("an input leg cannot complete a digital cycle");
    }

    #[test]
    fn fanning_one_source_out_to_several_sinks_is_allowed() {
        let a = DeviceId::new();
        let b = DeviceId::new();
        let c = DeviceId::new();
        let mut r = AudioRouting::new(vec![
            dev(a, "a-out", DeviceKind::Output),
            dev(b, "b-out", DeviceKind::Output),
            dev(c, "c-out", DeviceKind::Output),
        ]);
        r.add(route(key(a, "a-out"), key(b, "b-out"))).unwrap();
        r.add(route(key(a, "a-out"), key(c, "c-out")))
            .expect("one source may drive many sinks");
    }

    #[test]
    fn several_sources_may_mix_into_one_sink() {
        let a = DeviceId::new();
        let b = DeviceId::new();
        let c = DeviceId::new();
        let mut r = AudioRouting::new(vec![
            dev(a, "a-out", DeviceKind::Output),
            dev(b, "b-out", DeviceKind::Output),
            dev(c, "c-out", DeviceKind::Output),
        ]);
        r.add(route(key(a, "a-out"), key(c, "c-out"))).unwrap();
        r.add(route(key(b, "b-out"), key(c, "c-out")))
            .expect("mixing two machines into one speaker is legitimate");
    }

    #[test]
    fn the_same_route_twice_is_rejected_rather_than_doubling_the_volume() {
        let (a, b, mut r) = fixture();
        r.add(route(key(a, "a-speakers"), key(b, "b-speakers")))
            .unwrap();
        assert_eq!(
            r.add(route(key(a, "a-speakers"), key(b, "b-speakers")))
                .unwrap_err(),
            RouteError::Duplicate
        );
    }

    #[test]
    fn audio_cannot_be_played_into_a_microphone() {
        let (a, b, mut r) = fixture();
        let err = r
            .add(route(key(a, "a-speakers"), key(b, "b-mic")))
            .unwrap_err();
        assert!(matches!(err, RouteError::SinkIsNotAnOutput(_)));
    }

    #[test]
    fn routing_to_a_device_that_does_not_exist_is_rejected() {
        let (a, b, mut r) = fixture();
        let err = r
            .add(route(key(a, "a-speakers"), key(b, "b-nonexistent")))
            .unwrap_err();
        assert!(matches!(err, RouteError::UnknownDevice(_)));
    }

    #[test]
    fn removing_a_leg_makes_the_cycle_addable_again() {
        // Confirms the check runs against live state rather than a stale snapshot.
        let (a, b, mut r) = fixture();
        let first = route(key(a, "a-speakers"), key(b, "b-speakers"));
        let second = route(key(b, "b-speakers"), key(a, "a-speakers"));
        r.add(first.clone()).unwrap();
        assert!(r.add(second.clone()).is_err());
        assert!(r.remove(&first));
        r.add(second)
            .expect("the cycle is gone once the first leg is removed");
    }

    #[test]
    fn unplugging_a_device_surfaces_its_route_instead_of_dropping_it() {
        let (a, b, mut r) = fixture();
        r.add(route(key(a, "a-speakers"), key(b, "b-speakers")))
            .unwrap();
        assert!(r.stale_routes().is_empty());
        r.devices.retain(|d| d.node != "b-speakers");
        assert_eq!(
            r.stale_routes().len(),
            1,
            "the orphaned route must stay visible"
        );
    }

    #[test]
    fn removing_a_route_that_was_never_there_reports_no_change() {
        let (a, b, mut r) = fixture();
        assert!(!r.remove(&route(key(a, "a-speakers"), key(b, "b-speakers"))));
    }

    #[test]
    fn explain_uses_friendly_names_not_opaque_node_ids() {
        // On Windows a node id is a GUID. A message built from ids alone tells the
        // operator a loop exists but not which speaker to change.
        let a = DeviceId::new();
        let mut r = AudioRouting::new(vec![AudioDevice {
            device_id: a,
            node: "{0.0.0.00000000}.{b6a8d9b4-45a7-483f-982b-ca8f265d027c}".into(),
            name: "Speakers (Aqstic)".into(),
            kind: DeviceKind::Output,
            is_default: true,
        }]);
        let k = key(a, "{0.0.0.00000000}.{b6a8d9b4-45a7-483f-982b-ca8f265d027c}");
        let err = r.add(route(k.clone(), k)).unwrap_err();
        let msg = r.explain(&err);
        assert!(msg.contains("Speakers (Aqstic)"), "message was {msg}");
        assert!(!msg.contains("b6a8d9b4"), "raw guid leaked into {msg}");
    }

    #[test]
    fn explain_falls_back_to_the_node_id_when_the_device_is_gone() {
        let (a, b, mut r) = fixture();
        r.add(route(key(a, "a-speakers"), key(b, "b-speakers")))
            .unwrap();
        r.devices.retain(|d| d.node != "b-speakers");
        let msg = r.explain(&RouteError::UnknownDevice(key(b, "b-speakers")));
        assert!(msg.contains("b-speakers"), "message was {msg}");
    }

    #[test]
    fn a_loop_error_names_the_devices_involved() {
        let (a, _b, r) = fixture();
        let err = r
            .check(&route(key(a, "a-speakers"), key(a, "a-speakers")))
            .unwrap_err();
        assert!(
            err.to_string().contains("a-speakers"),
            "message was {err}, which does not tell the operator what to fix"
        );
    }
}
