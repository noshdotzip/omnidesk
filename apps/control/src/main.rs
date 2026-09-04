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

mod devices;

use devices::MachineAudio;
use dioxus::prelude::*;
use ultidesk_core::DeviceId;
use ultidesk_topology::{
    AudioRouting, DeviceKey, Layout, Monitor, MonitorId, Rotation, Route, DEFAULT_SNAP,
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
/// Both ids are minted here because pairing does not exist yet. The local one is what
/// the enumerated devices are attached to; the remote one exists so the routing panel
/// can show the shape of a two-machine setup honestly, marked as not connected.
#[derive(Clone)]
struct Machines {
    local: DeviceId,
    remote: DeviceId,
}

/// Placeholder monitors so the editor is usable before topology IPC exists.
///
/// Deliberately two devices with differently sized screens: an editor that only ever
/// sees identical monitors hides most of the bugs worth catching.
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
            refresh_rate: 60.0,
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
            refresh_rate: 144.0,
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

    rsx! {
        style { {STYLE} }
        div { class: "app",
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
    let machines = use_hook(|| Machines {
        local: DeviceId::new(),
        remote: DeviceId::new(),
    });
    let mut layout = use_signal(|| demo_layout(&machines));
    let mut dragging = use_signal(|| None::<(usize, f64, f64)>);

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
            onmouseup: move |_| dragging.set(None),
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
                        "{m.native_pixel_width}×{m.native_pixel_height} · {m.refresh_rate:.0} Hz"
                    }
                    if m.primary {
                        div { class: "badge", "primary" }
                    }
                }
            }
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
    fn load() -> Self {
        let local_id = DeviceId::new();
        let remote_id = DeviceId::new();
        let machines = vec![
            devices::local(local_id, "This machine"),
            devices::remote_placeholder(remote_id, "Peer"),
        ];
        let all = machines.iter().flat_map(|m| m.devices.clone()).collect();
        AudioState {
            machines,
            routing: AudioRouting::new(all),
        }
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
    let mut state = use_signal(AudioState::load);
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
                onclick: move |_| {
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
                                message.set("route added".into());
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
                                    move |_| {
                                        let route = state.read().routing.routes().get(idx).cloned();
                                        if let Some(route) = route {
                                            state.write().routing.remove(&route);
                                            message.set("route removed".into());
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
