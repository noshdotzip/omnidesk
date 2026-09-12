//! Ultidesk control UI — display arrangement and audio routing.
//!
//! Two settings the operator has to be able to change by hand: where each machine's
//! screens sit relative to each other (which decides where the pointer crosses), and
//! which machine's audio plays on which machine's speakers. See ADR-0010.
//!
//! # All the rules live in `ultidesk-topology`
//! Snapping, overlap detection, adjacency and audio-loop detection are not implemented
//! here. If the editor computed them itself it could disagree with the agent, and the
//! result would be a desk where the pointer vanishes at an edge that looks correct on
//! screen, or a routing table the UI accepts and the agent refuses. This app is a view
//! over `Layout` and `AudioRouting` and nothing more.
//!
//! # Status
//! The arrangement editor works against an in-memory layout, and audio devices are read
//! from the machine this app runs on. Neither the peer's devices nor persistence exist
//! yet: both need the settings IPC surface, and inventing a parallel one here would be
//! the second protocol ADR-0004 warns against. The UI says which parts are real rather
//! than showing plausible placeholders.

mod agent;
mod devices;
mod monitors;
mod settings;

use devices::MachineAudio;
use dioxus::prelude::*;
use ultidesk_core::DeviceId;
use ultidesk_topology::{
    AudioRouting, DeviceKey, Layout, Monitor, MonitorId, Rotation, Route, SavedLayout, DEFAULT_SNAP,
};

fn main() {
    use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            Config::new().with_window(
                WindowBuilder::new()
                    .with_title("Ultidesk — Control")
                    // Logical, so this is 1080x680 CSS pixels whatever the display
                    // scale. Kept under the smallest logical desktop this runs on: a
                    // 1664x1109 panel at 150%% is only ~1109x739 logical, and a taller
                    // window puts its own bottom edge off-screen.
                    .with_inner_size(LogicalSize::new(1080.0, 680.0)),
            ),
        )
        .launch(App);
}

/// The machines this session knows about.
///
/// The local id is **derived from this machine's Ed25519 identity**, so it is the same
/// across launches, the same in both tabs, and the same id the agent uses. It used to
/// be a random uuid minted per tab, which meant the Displays tab and the Audio tab
/// silently disagreed about which machine was "this machine"; then it was a random uuid
/// stored in the settings file, which fixed the disagreement but still named a machine
/// nobody could verify.
///
/// The remote id is still minted per launch: pairing exists as a mechanism
/// (`ultidesk_identity::PeerStore`) but nothing connects yet, so there is no peer
/// identity to be stable about. It exists so the panels can show the shape of a
/// two-machine setup, marked as not connected.
#[derive(Clone)]
struct Machines {
    local: DeviceId,
    remote: DeviceId,
    /// The paired peer to ask the agent about, when exactly one is paired.
    ///
    /// `None` when none is paired *or* when several are: picking one to show would be
    /// guessing which machine the operator meant, and the editor would then silently
    /// arrange the wrong one.
    peer: Option<(ultidesk_identity::PeerKey, String)>,
}

/// Everything loaded from disk once, and shared by both tabs through context.
#[derive(Clone)]
struct Session {
    machines: Machines,
    dir: Option<std::path::PathBuf>,
    /// This machine's identity fingerprint, for the operator to compare when pairing.
    fingerprint: Option<String>,
    /// Anything the operator should know about the load, e.g. a settings file that
    /// could not be read.
    note: Option<String>,
}

impl Session {
    fn load() -> (Self, settings::Settings) {
        let dir = settings::config_dir();
        let mut loaded = match &dir {
            Some(d) => settings::load_from(d),
            // No config directory at all (no HOME, no APPDATA). The app still works;
            // it just cannot remember anything, and says so rather than pretending
            // to save.
            None => settings::Loaded {
                settings: settings::Settings::fresh(),
                note: Some(
                    "no configuration directory available; changes will not be saved".into(),
                ),
            },
        };

        // The identity, not the settings file, decides which machine this is.
        let identity = dir.as_ref().map(|d| ultidesk_identity::load_or_create(d));
        let mut fingerprint = None;
        match identity {
            Some(Ok(loaded_identity)) => {
                fingerprint = Some(loaded_identity.identity.fingerprint());
                if let Some(n) = loaded_identity.note {
                    loaded.note.get_or_insert(n);
                }
                // Carries the saved routes onto the derived id the first time this
                // build runs against a settings file written by an older one.
                if loaded
                    .settings
                    .adopt_device_id(loaded_identity.identity.device_id())
                {
                    if let Some(d) = &dir {
                        if let Err(e) = settings::save_to(d, &loaded.settings) {
                            loaded
                                .note
                                .get_or_insert(format!("could not save re-keyed settings: {e}"));
                        }
                    }
                }
            }
            // A damaged identity file is refused rather than replaced, so the app runs
            // with a throwaway id and says why. Silently re-keying would un-pair every
            // peer without telling anyone — see `ultidesk_identity::store`.
            Some(Err(e)) => {
                loaded.note.get_or_insert(format!(
                    "this machine's identity could not be read, so it is unnamed this session: {e}"
                ));
            }
            None => {}
        }

        // Which peer to ask about. Read from the same store the agent enforces against,
        // so the app cannot show a device the agent would refuse to talk to.
        let peer = dir.as_ref().and_then(|d| {
            let local_key = ultidesk_identity::store::load(d).ok().flatten()?.public();
            let store = ultidesk_identity::peers::load(d, local_key).store;
            store.only_peer().map(|p| (p.key, p.name.clone()))
        });

        let session = Session {
            machines: Machines {
                local: loaded.settings.local_device_id,
                // The peer's *derived* id when one is paired, so the monitors and audio
                // endpoints it reports line up with the machine shown in the editor. A
                // random id would put its real devices under a machine nobody has.
                remote: peer
                    .as_ref()
                    .map(|(key, _)| key.device_id())
                    .unwrap_or_else(DeviceId::new),
                peer,
            },
            dir,
            fingerprint,
            note: loaded.note,
        };
        (session, loaded.settings)
    }
}

/// Write the current settings back, reporting failure rather than swallowing it.
///
/// Saving is best-effort by design: a full disk should not stop the operator editing
/// their arrangement, but it must not look like it worked either.
fn persist(session: &Session, settings: &settings::Settings) -> Option<String> {
    let dir = session.dir.as_ref()?;
    match settings::save_to(dir, settings) {
        Ok(()) => None,
        Err(e) => Some(format!("could not save settings: {e}")),
    }
}

/// The peer's placeholder screen, until the settings IPC can carry a real one.
///
/// Deliberately a different size from any local monitor: an editor that only ever sees
/// identical screens hides most of the bugs worth catching.
fn peer_placeholder(device_id: DeviceId) -> Monitor {
    Monitor {
        device_id,
        monitor_id: MonitorId(1),
        friendly_name: "Peer (not connected)".into(),
        logical_x: 0.0,
        logical_y: 0.0,
        logical_width: 1920.0,
        logical_height: 1080.0,
        native_pixel_width: 1920,
        native_pixel_height: 1080,
        scale_factor: 1.0,
        rotation: Rotation::Landscape,
        refresh_rate: None,
        primary: false,
    }
}

/// The arrangement to show before the agent has answered — or if it never does.
///
/// Read through the window toolkit, which is the only source available without an agent.
/// It is deliberately the *fallback* now rather than the source: the agent's numbers are
/// what a peer is told, so they are the ones the editor should arrange, and having one
/// source is what stops the two disagreeing again.
fn initial_layout(
    window: &dioxus::desktop::tao::window::Window,
    machines: &Machines,
    stored: &settings::Settings,
) -> Layout {
    let found = monitors::local_monitors(window, machines.local);
    if found.is_empty() {
        // A headless or unreadable session. Falling back keeps the editor usable and,
        // because the names differ, makes it obvious these are not real.
        return demo_layout(machines);
    }
    arrange(
        machines,
        found,
        vec![peer_placeholder(machines.remote)],
        stored,
    )
}

/// The arrangement built from what the agent reported.
///
/// `None` when the agent could not say what this machine's screens are — in which case
/// whatever is already on screen is better than an empty editor.
fn layout_from_agent(
    machines: &Machines,
    view: &agent::AgentView,
    stored: &settings::Settings,
) -> Option<Layout> {
    let local = view.local_monitors.value.clone()?;
    if local.is_empty() {
        return None;
    }
    // A peer that could not be reached keeps its placeholder rather than vanishing: the
    // operator arranged it, and removing it would silently drop that arrangement.
    let peer = view
        .peer
        .as_ref()
        .and_then(|p| p.monitors.value.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| vec![peer_placeholder(machines.remote)]);
    Some(arrange(machines, local, peer, stored))
}

/// Place two machines' screens and restore anything the operator saved.
fn arrange(
    machines: &Machines,
    local: Vec<Monitor>,
    peer: Vec<Monitor>,
    stored: &settings::Settings,
) -> Layout {
    // Machines in a strip, left to right, in the order they connected. `left_to_right`
    // translates each machine's desktop as one block, so every screen keeps its exact
    // position relative to its own machine's others — which is what stops a shared
    // internal boundary becoming a gap the pointer cannot cross. See
    // `ultidesk_topology::arrange`.
    let arranged = ultidesk_topology::left_to_right(vec![
        ultidesk_topology::MachineMonitors {
            device_id: machines.local,
            monitors: local,
        },
        ultidesk_topology::MachineMonitors {
            device_id: machines.remote,
            monitors: peer,
        },
    ]);
    // Saved positions are applied last, so anything the operator arranged wins over both
    // the platform's idea of where the screens are and the default strip.
    stored.layout.apply(arranged.monitors)
}

/// What to tell the operator about where these numbers came from.
fn describe_view(view: &agent::AgentView) -> Option<String> {
    if let Some(note) = &view.note {
        return Some(note.clone());
    }
    if let Some(note) = &view.local_monitors.note {
        return Some(format!("this machine's screens: {note}"));
    }
    match &view.peer {
        Some(peer) => peer
            .monitors
            .note
            .as_ref()
            .map(|note| format!("{}: {note}", peer.name)),
        None => Some("no peer is paired, so the second screen is a placeholder".into()),
    }
}

/// Placeholder monitors, used only when the toolkit reports no displays at all.
fn demo_layout(m: &Machines) -> Layout {
    Layout::new(vec![
        Monitor {
            device_id: m.local,
            monitor_id: MonitorId(1),
            friendly_name: "This machine".into(),
            logical_x: 0.0,
            logical_y: 0.0,
            logical_width: 1664.0,
            logical_height: 1109.0,
            native_pixel_width: 1664,
            native_pixel_height: 1109,
            scale_factor: 1.0,
            rotation: Rotation::Landscape,
            refresh_rate: Some(60.0),
            primary: true,
        },
        Monitor {
            device_id: m.remote,
            monitor_id: MonitorId(1),
            friendly_name: "Peer".into(),
            logical_x: 1664.0,
            logical_y: 0.0,
            logical_width: 1920.0,
            logical_height: 1080.0,
            native_pixel_width: 1920,
            native_pixel_height: 1080,
            scale_factor: 1.0,
            rotation: Rotation::Landscape,
            refresh_rate: Some(144.0),
            primary: false,
        },
    ])
}

/// Fallback canvas width in CSS pixels, used only until the real one is measured.
///
/// The canvas is fluid (`width:100%`) and its true width is read from the DOM on mount
/// and on resize. Hardcoding it does not work: the WebView renders at the display scale
/// factor, so on a 150%% panel a nominal 640px canvas overflows the viewport and the
/// monitors past the edge cannot be reached. The measured width also has to feed the
/// drag math — the pointer-to-layout mapping divides by it, so a CSS width that did not
/// match would place the dragged screen under a different part of the cursor.
const CANVAS_W_FALLBACK: f64 = 560.0;
const CANVAS_H: f64 = 320.0;
/// Breathing room so a monitor dragged to the edge is still grabbable.
const CANVAS_PAD: f64 = 28.0;

/// Scale factor that fits the whole arrangement into the canvas.
///
/// Recomputed from the live bounds rather than fixed, so dragging a monitor far away
/// zooms out instead of pushing it off-screen where it cannot be dragged back.
fn view_scale(layout: &Layout, canvas_w: f64) -> (f64, f64, f64) {
    let Some(b) = layout.bounds() else {
        return (1.0, 0.0, 0.0);
    };
    let sx = (canvas_w - CANVAS_PAD * 2.0) / b.width.max(1.0);
    let sy = (CANVAS_H - CANVAS_PAD * 2.0) / b.height.max(1.0);
    let s = sx.min(sy).min(1.0);
    (s, b.x, b.y)
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Displays,
    Audio,
}

#[component]
fn App() -> Element {
    let mut tab = use_signal(|| Tab::Displays);
    // Loaded once and shared, so both tabs agree on this machine's identity and both
    // write into the same file.
    let (session, stored) = use_hook(|| {
        let (session, stored) = Session::load();
        (session, std::rc::Rc::new(std::cell::RefCell::new(stored)))
    });
    use_context_provider(|| session.clone());
    use_context_provider(|| stored.clone());

    rsx! {
        style { {STYLE} }
        div { class: "app",
            if let Some(note) = &session.note {
                div { class: "warn", "{note}" }
            }
            // Shown rather than hidden behind a settings page: pairing works by an
            // operator comparing this string with the one on the other machine, so it
            // has to be somewhere they can find without being told where to look.
            if let Some(fingerprint) = &session.fingerprint {
                div { class: "note", "This machine's identity: {fingerprint}" }
            }
            // The paired peer, by the fingerprint the operator compared when pairing —
            // so the two machines on screen can be told apart by the same string that
            // established they were the right two.
            match &session.machines.peer {
                Some((key, name)) => rsx! {
                    div { class: "note", "Paired with {name}: {key.fingerprint()}" }
                },
                None => rsx! {
                    div { class: "note", "No peer paired — run `ultidesk-agent pair`" }
                },
            }
            div { class: "tabs",
                button {
                    class: if *tab.read() == Tab::Displays { "tab on" } else { "tab" },
                    onclick: move |_| tab.set(Tab::Displays),
                    "Displays"
                }
                button {
                    class: if *tab.read() == Tab::Audio { "tab on" } else { "tab" },
                    onclick: move |_| tab.set(Tab::Audio),
                    "Audio routing"
                }
            }
            match *tab.read() {
                Tab::Displays => rsx! { Displays {} },
                Tab::Audio => rsx! { AudioPanel {} },
            }
        }
    }
}

#[component]
fn Displays() -> Element {
    let session = use_context::<Session>();
    let stored = use_context::<std::rc::Rc<std::cell::RefCell<settings::Settings>>>();
    let machines = session.machines.clone();
    let window = dioxus::desktop::use_window();

    // What the agent says, fetched once on mount. `use_resource` rather than a blocking
    // read because one of these questions is a *relay* to the other machine, and a panel
    // that freezes while a sleeping peer times out looks exactly like a crash.
    let asked = machines.peer.clone();
    let view = use_resource(move || {
        let asked = asked.clone();
        async move { std::rc::Rc::new(agent::fetch(asked).await) }
    });

    // The layout is a signal so dragging can move it; it is (re)built from the agent's
    // answer as soon as one arrives, and from the toolkit until then.
    let mut layout = use_signal(|| initial_layout(&window, &machines, &stored.borrow()));
    let mut agent_note = use_signal(|| None::<String>);
    let mut built = use_signal(|| false);

    // Rebuild once, when the first answer lands. Rebuilding on every render would throw
    // away whatever the operator had just dragged.
    if let Some(answer) = view.read().as_ref() {
        if !built() {
            built.set(true);
            agent_note.set(describe_view(answer));
            if let Some(fresh) = layout_from_agent(&machines, answer, &stored.borrow()) {
                layout.set(fresh);
            }
        }
    }
    let mut dragging = use_signal(|| None::<(usize, f64, f64)>);
    let mut save_error = use_signal(|| None::<String>);

    // Measured from the DOM rather than assumed; see CANVAS_W_FALLBACK.
    let mut canvas_w = use_signal(|| CANVAS_W_FALLBACK);
    let snapshot = layout.read().clone();
    let (scale, origin_x, origin_y) = view_scale(&snapshot, canvas_w());
    let overlaps = snapshot.overlapping_pairs();

    // Every touching pair, so the operator can see where crossing is actually possible
    // rather than inferring it from how the boxes look.
    let mut borders: Vec<String> = Vec::new();
    for i in 0..snapshot.monitors.len() {
        for j in (i + 1)..snapshot.monitors.len() {
            if let Some(adj) = snapshot.adjacency(i, j) {
                borders.push(format!(
                    "{} → {:?} → {}  ({:.0}px of shared border)",
                    snapshot.monitors[i].friendly_name,
                    adj.side,
                    snapshot.monitors[j].friendly_name,
                    adj.span()
                ));
            }
        }
    }

    rsx! {
        h1 { "Display arrangement" }
        p { class: "hint",
            "Drag each screen into its real physical position. Edges snap together — "
            "the pointer can only cross where two screens actually touch."
        }

        div {
            class: "canvas",
            onmounted: move |e| async move {
                if let Ok(rect) = e.data().get_client_rect().await {
                    canvas_w.set(rect.size.width);
                }
            },
            onresize: move |e| {
                if let Ok(size) = e.get_content_box_size() {
                    canvas_w.set(size.width);
                }
            },
            onmousemove: move |e| {
                if let Some((idx, grab_x, grab_y)) = *dragging.read() {
                    let c = e.data().client_coordinates();
                    let lx = origin_x + (c.x - grab_x) / scale;
                    let ly = origin_y + (c.y - grab_y) / scale;
                    layout.write().move_monitor(idx, lx, ly, DEFAULT_SNAP);
                }
            },
            // Saved on release rather than on every motion event: a drag emits
            // hundreds of those, and rewriting the file on each one would mean a
            // constant stream of disk writes for one gesture.
            onmouseup: {
                let session = session.clone();
                let stored = stored.clone();
                move |_| {
                if dragging.read().is_some() {
                    dragging.set(None);
                    stored.borrow_mut().layout = SavedLayout::from_layout(&layout.read());
                    if let Some(err) = persist(&session, &stored.borrow()) {
                        save_error.set(Some(err));
                    }
                }
                }
            },
            onmouseleave: move |_| dragging.set(None),

            for (i, m) in snapshot.monitors.iter().enumerate() {
                div {
                    key: "{i}",
                    class: if overlaps.iter().any(|(a, b)| *a == i || *b == i) {
                        "screen overlapping"
                    } else {
                        "screen"
                    },
                    style: "left:{CANVAS_PAD + (m.logical_x - origin_x) * scale}px;
                            top:{CANVAS_PAD + (m.logical_y - origin_y) * scale}px;
                            width:{m.logical_width * scale}px;
                            height:{m.logical_height * scale}px;",
                    onmousedown: move |e| {
                        // Remember where inside the box the grab happened, so the
                        // screen does not jump to centre itself under the cursor.
                        let c = e.data().client_coordinates();
                        let cur = layout.read();
                        if let Some(mon) = cur.monitors.get(i) {
                            let off_x = c.x - (mon.logical_x - origin_x) * scale;
                            let off_y = c.y - (mon.logical_y - origin_y) * scale;
                            dragging.set(Some((i, off_x, off_y)));
                        }
                    },
                    div { class: "name", "{m.friendly_name}" }
                    div { class: "meta",
                        match m.refresh_rate {
                            Some(hz) => format!(
                                "{}×{} · {:.0} Hz",
                                m.native_pixel_width, m.native_pixel_height, hz
                            ),
                            // Linux reports no video modes, so the size stands alone
                            // rather than being padded with an invented rate.
                            None => format!("{}×{}", m.native_pixel_width, m.native_pixel_height),
                        }
                    }
                    if m.primary {
                        div { class: "badge", "primary" }
                    }
                }
            }
        }

        if let Some(err) = save_error.read().as_ref() {
            div { class: "warn", "{err}" }
        }

        if !overlaps.is_empty() {
            div { class: "warn",
                strong { "Screens overlap. " }
                "A point inside the overlap belongs to two displays at once, so the "
                "pointer's position there is ambiguous. Move them apart before saving."
            }
        }

        div { class: "panel",
            h2 { "Shared borders" }
            if borders.is_empty() {
                p { class: "hint", "No screens touch, so the pointer cannot cross between them." }
            } else {
                ul { for b in borders.iter() { li { key: "{b}", "{b}" } } }
            }
        }
    }
}

/// State the audio panel owns: the machine inventory and the routing table over it.
struct AudioState {
    machines: Vec<MachineAudio>,
    routing: AudioRouting,
}

impl AudioState {
    /// The panel as it looks before the agent has answered, or with no agent at all.
    fn load(machines_ids: &Machines, saved_routes: &[Route]) -> Self {
        let machines = vec![
            devices::local(machines_ids.local, "This machine"),
            devices::remote_placeholder(machines_ids.remote, "Peer"),
        ];
        Self::from_machines(machines, saved_routes)
    }

    /// The panel built from what the agent reported, including the peer's real devices.
    ///
    /// Each half falls back independently: a peer that could not be reached leaves its
    /// side a labelled placeholder while this machine's endpoints stay real, which is a
    /// more useful screen than an empty one and an honest one either way.
    fn from_agent(
        machines_ids: &Machines,
        view: &agent::AgentView,
        saved_routes: &[Route],
    ) -> Self {
        let local = match view.local_audio.value.clone() {
            Some(devices) => devices::from_agent(machines_ids.local, "This machine", devices),
            None => devices::local(machines_ids.local, "This machine"),
        };
        let peer = match (
            &view.peer,
            view.peer.as_ref().and_then(|p| p.audio.value.clone()),
        ) {
            (Some(p), Some(devices)) => devices::from_agent(machines_ids.remote, &p.name, devices),
            (Some(p), None) => {
                let note = p
                    .audio
                    .note
                    .clone()
                    .unwrap_or_else(|| "the peer did not answer".into());
                devices::unreachable(machines_ids.remote, &p.name, note)
            }
            (None, _) => devices::remote_placeholder(machines_ids.remote, "Peer"),
        };
        Self::from_machines(vec![local, peer], saved_routes)
    }

    fn from_machines(machines: Vec<MachineAudio>, saved_routes: &[Route]) -> Self {
        let all = machines.iter().flat_map(|m| m.devices.clone()).collect();
        let mut routing = AudioRouting::new(all);
        // Re-checked on the way in rather than trusted. A device may be gone, or the
        // inventory may have changed such that a once-valid pair is now a loop; adding
        // through `add` is what refuses those, and skipping the check here would let a
        // saved file reintroduce exactly the feedback the model exists to prevent.
        for route in saved_routes {
            let _ = routing.add(route.clone());
        }
        AudioState { machines, routing }
    }

    fn label_for(&self, key: &DeviceKey) -> String {
        self.routing
            .device(key)
            .map(|d| d.name.clone())
            // A route can outlive its device; showing the raw node beats showing nothing.
            .unwrap_or_else(|| format!("{} (missing)", key.node))
    }

    fn machine_label(&self, key: &DeviceKey) -> String {
        self.machines
            .iter()
            .find(|m| m.device_id == key.device_id)
            .map(|m| m.label.clone())
            .unwrap_or_else(|| "unknown machine".into())
    }
}

#[component]
fn AudioPanel() -> Element {
    let session = use_context::<Session>();
    let stored = use_context::<std::rc::Rc<std::cell::RefCell<settings::Settings>>>();
    let mut state = use_signal(|| {
        let saved = stored.borrow().audio_routes.clone();
        AudioState::load(&session.machines, &saved)
    });

    // The same fetch the Displays tab does, over its own connection: the tabs mount
    // independently and sharing one would mean the second tab showed whatever the first
    // happened to have asked for.
    let asked = session.machines.peer.clone();
    let view = use_resource(move || {
        let asked = asked.clone();
        async move { std::rc::Rc::new(agent::fetch(asked).await) }
    });
    let mut built = use_signal(|| false);
    if let Some(answer) = view.read().as_ref() {
        if !built() {
            built.set(true);
            let saved = stored.borrow().audio_routes.clone();
            state.set(AudioState::from_agent(&session.machines, answer, &saved));
        }
    }
    let mut source = use_signal(|| None::<DeviceKey>);
    let mut sink = use_signal(|| None::<DeviceKey>);
    let mut message = use_signal(String::new);

    let s = state.read();
    let selected_source = source.read().clone();
    let selected_sink = sink.read().clone();

    // Ask the model whether the pending pair is legal, so the button explains itself
    // before it is pressed rather than failing after.
    let pending = match (&selected_source, &selected_sink) {
        (Some(a), Some(b)) => Some(Route {
            source: a.clone(),
            sink: b.clone(),
        }),
        _ => None,
    };
    // Rendered through the model so the message names devices rather than raw node
    // ids — a WASAPI endpoint id is a GUID and says nothing to the operator.
    let pending_error = pending
        .as_ref()
        .and_then(|r| s.routing.check(r).err())
        .map(|e| s.routing.explain(&e));
    let can_add = pending.is_some() && pending_error.is_none();

    let existing: Vec<(usize, String, String, String, String)> = s
        .routing
        .routes()
        .iter()
        .enumerate()
        .map(|(i, r)| {
            (
                i,
                s.machine_label(&r.source),
                s.label_for(&r.source),
                s.machine_label(&r.sink),
                s.label_for(&r.sink),
            )
        })
        .collect();
    let stale = s.routing.stale_routes().len();

    rsx! {
        h1 { "Audio routing" }
        p { class: "hint",
            "Capture what one machine is playing and play it on another. Routes that "
            "would feed back into themselves are refused — see the note below."
        }

        div { class: "cols",
            div { class: "col",
                h2 { "Capture from" }
                for m in s.machines.iter() {
                    div { key: "{m.device_id}", class: "machine",
                        div { class: "machine-name", "{m.label}" }
                        if let Some(note) = &m.note {
                            div { class: "note", "{note}" }
                        }
                        for d in m.devices.iter() {
                            button {
                                key: "{d.node}",
                                class: if selected_source.as_ref() == Some(&d.key()) {
                                    "dev on"
                                } else {
                                    "dev"
                                },
                                onclick: {
                                    let k = d.key();
                                    move |_| {
                                        source.set(Some(k.clone()));
                                        message.set(String::new());
                                    }
                                },
                                span { class: "dev-name", "{d.name}" }
                                span { class: "dev-kind", "{kind_label(d.kind)}" }
                                if d.is_default {
                                    span { class: "badge", "default" }
                                }
                            }
                        }
                    }
                }
            }

            div { class: "col",
                h2 { "Play on" }
                for m in s.machines.iter() {
                    div { key: "{m.device_id}", class: "machine",
                        div { class: "machine-name", "{m.label}" }
                        if let Some(note) = &m.note {
                            div { class: "note", "{note}" }
                        }
                        for d in m.devices.iter().filter(|d| d.kind == ultidesk_topology::DeviceKind::Output) {
                            button {
                                key: "{d.node}",
                                class: if selected_sink.as_ref() == Some(&d.key()) {
                                    "dev on"
                                } else {
                                    "dev"
                                },
                                onclick: {
                                    let k = d.key();
                                    move |_| {
                                        sink.set(Some(k.clone()));
                                        message.set(String::new());
                                    }
                                },
                                span { class: "dev-name", "{d.name}" }
                                if d.is_default {
                                    span { class: "badge", "default" }
                                }
                            }
                        }
                    }
                }
            }
        }

        div { class: "actions",
            button {
                class: if can_add { "primary" } else { "primary disabled" },
                disabled: !can_add,
                onclick: {
                    // Cloned per closure: both handlers need them, and `Rc`/`Session`
                    // are not `Copy`, so the first `move` would take them.
                    let session = session.clone();
                    let stored = stored.clone();
                    move |_| {
                    let route = {
                        let (a, b) = (source.read().clone(), sink.read().clone());
                        match (a, b) {
                            (Some(a), Some(b)) => Some(Route { source: a, sink: b }),
                            _ => None,
                        }
                    };
                    if let Some(route) = route {
                        // The write borrow is scoped so the error can be rendered
                        // through the same signal afterwards.
                        let outcome = state.write().routing.add(route);
                        match outcome {
                            Ok(()) => {
                                stored.borrow_mut().audio_routes =
                                    state.read().routing.routes().to_vec();
                                match persist(&session, &stored.borrow()) {
                                    Some(err) => message.set(err),
                                    None => message.set("route added".into()),
                                }
                                source.set(None);
                                sink.set(None);
                            }
                            // The model is the authority even though the button is
                            // pre-checked: state can change between render and click.
                            Err(e) => {
                                let text = state.read().routing.explain(&e);
                                message.set(text);
                            }
                        }
                    }
                    }
                },
                "Add route"
            }
            if let Some(err) = &pending_error {
                span { class: "err", "{err}" }
            } else if !message.read().is_empty() {
                span { class: "ok", "{message}" }
            }
        }

        div { class: "panel",
            h2 { "Active routes" }
            if existing.is_empty() {
                p { class: "hint", "No audio is being routed between machines." }
            } else {
                ul { class: "routes",
                    for (i, src_machine, src_dev, dst_machine, dst_dev) in existing.iter() {
                        li { key: "{i}",
                            span { class: "route-text",
                                "{src_machine} · {src_dev}  →  {dst_machine} · {dst_dev}"
                            }
                            button {
                                class: "link",
                                onclick: {
                                    let idx = *i;
                                    let session = session.clone();
                                    let stored = stored.clone();
                                    move |_| {
                                        let route = state.read().routing.routes().get(idx).cloned();
                                        if let Some(route) = route {
                                            state.write().routing.remove(&route);
                                            stored.borrow_mut().audio_routes =
                                                state.read().routing.routes().to_vec();
                                            match persist(&session, &stored.borrow()) {
                                                Some(err) => message.set(err),
                                                None => message.set("route removed".into()),
                                            }
                                        }
                                    }
                                },
                                "remove"
                            }
                        }
                    }
                }
            }
            if stale > 0 {
                div { class: "warn",
                    strong { "{stale} route(s) name a device that is gone. " }
                    "They are kept rather than dropped so the silence has a visible cause."
                }
            }
        }

        div { class: "panel",
            h2 { "Why some routes are refused" }
            p { class: "hint",
                "Capturing an output means capturing everything playing on it. Play that "
                "back onto the same output — directly, or around a loop through another "
                "machine — and the playback is captured and sent again, louder each pass. "
                "Routes that close such a loop are refused. Routing between two different "
                "outputs, or from a microphone, cannot feed back and is allowed."
            }
        }
    }
}

fn kind_label(kind: ultidesk_topology::DeviceKind) -> &'static str {
    match kind {
        ultidesk_topology::DeviceKind::Output => "output",
        ultidesk_topology::DeviceKind::Input => "input",
    }
}

const STYLE: &str = r#"
:root { color-scheme: dark; }
body { margin:0; background:#15171c; color:#e7e9ee;
       font:14px/1.5 system-ui,-apple-system,Segoe UI,sans-serif; }
.app { padding:20px 28px 40px; }
h1 { font-size:20px; margin:0 0 4px; font-weight:600; }
h2 { font-size:13px; margin:0 0 8px; font-weight:600; color:#aeb4c0;
     text-transform:uppercase; letter-spacing:.04em; }
.hint { color:#8b93a3; margin:0 0 18px; }
.tabs { display:flex; gap:4px; margin-bottom:22px;
        border-bottom:1px solid #2a2f3a; }
.tab { background:none; border:none; color:#8b93a3; font:inherit; cursor:pointer;
       padding:8px 14px; border-bottom:2px solid transparent; }
.tab:hover { color:#e7e9ee; }
.tab.on { color:#e7e9ee; border-bottom-color:#5b86d6; }
.canvas { position:relative; width:100%; max-width:900px; height:320px; background:#1b1e25;
          border:1px solid #2a2f3a; border-radius:10px; overflow:hidden;
          user-select:none; }
.screen { position:absolute; background:#2b394f;
          border:1px solid #4a6da8; border-radius:6px; cursor:grab;
          display:flex; flex-direction:column; justify-content:center;
          align-items:center; overflow:hidden; }
.screen:active { cursor:grabbing; }
.screen.overlapping { background:#4f2b2b; border-color:#a84a4a; }
.name { font-weight:600; }
.meta { color:#9aa3b4; font-size:12px; }
.badge { font-size:10px; padding:1px 6px; border-radius:999px;
         background:#3a4a66; color:#c9d6ea; margin-left:6px; }
.warn { margin-top:16px; padding:10px 12px; border-radius:8px;
        background:#3a2222; border:1px solid #a84a4a; color:#f0d6d6; }
.panel { margin-top:22px; }
ul { margin:0; padding-left:18px; }
li { margin:2px 0; color:#c3cad6; }
.cols { display:flex; gap:20px; }
.col { flex:1; min-width:0; }
.machine { margin-bottom:14px; }
.machine-name { font-weight:600; margin-bottom:4px; }
.note { color:#8b93a3; font-size:12px; font-style:italic; margin-bottom:6px; }
.dev { display:flex; align-items:center; gap:8px; width:100%; text-align:left;
       background:#1b1e25; border:1px solid #2a2f3a; border-radius:6px;
       color:#c3cad6; font:inherit; padding:7px 10px; margin-bottom:4px;
       cursor:pointer; }
.dev:hover { border-color:#43506b; }
.dev.on { background:#243352; border-color:#5b86d6; color:#e7e9ee; }
.dev-name { flex:1; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
.dev-kind { color:#8b93a3; font-size:11px; }
.actions { display:flex; align-items:center; gap:12px; margin-top:8px; }
.primary { background:#3a5a9b; border:1px solid #5b86d6; color:#fff; font:inherit;
           padding:7px 16px; border-radius:6px; cursor:pointer; }
.primary.disabled { background:#242832; border-color:#2a2f3a; color:#6b7383;
                    cursor:not-allowed; }
.err { color:#e8a0a0; }
.ok { color:#8fce9b; }
.routes { list-style:none; padding:0; }
.routes li { display:flex; align-items:center; gap:12px; padding:6px 0;
             border-bottom:1px solid #23272f; }
.route-text { flex:1; }
.link { background:none; border:none; color:#7fa3e0; font:inherit; cursor:pointer;
        padding:0; }
.link:hover { text-decoration:underline; }
"#;
