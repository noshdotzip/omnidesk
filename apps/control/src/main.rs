//! Ultidesk control UI — the display arrangement editor.
//!
//! Lets the operator drag each machine's monitors into their real physical arrangement,
//! which is what decides where the pointer crosses between machines. See ADR-0010.
//!
//! # All the geometry lives in `ultidesk-topology`
//! Snapping, overlap detection and adjacency are not implemented here. If the editor
//! computed them itself it could disagree with the agent, and the result would be a desk
//! where the pointer vanishes at an edge that looks perfectly correct on screen. This
//! module is a view over `Layout` and nothing more.
//!
//! # Status
//! The arrangement editor works against an in-memory layout. It does not yet load the
//! real monitor list from the agent or persist changes — that needs the IPC surface for
//! topology, which does not exist yet, and inventing a parallel one here would be the
//! second protocol ADR-0004 warns against.

use dioxus::prelude::*;
use ultidesk_core::DeviceId;
use ultidesk_topology::{Layout, Monitor, MonitorId, Rotation, DEFAULT_SNAP};

fn main() {
    use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            Config::new().with_window(
                WindowBuilder::new()
                    .with_title("Ultidesk — Control")
                    .with_inner_size(LogicalSize::new(980.0, 780.0)),
            ),
        )
        .launch(App);
}

/// Placeholder monitors so the editor is usable before topology IPC exists.
///
/// Deliberately two devices with differently sized screens: an editor that only ever
/// sees identical monitors hides most of the bugs worth catching.
fn demo_layout() -> Layout {
    let windows = DeviceId::new();
    let arch = DeviceId::new();
    Layout::new(vec![
        Monitor {
            device_id: windows,
            monitor_id: MonitorId(1),
            friendly_name: "Windows ARM64".into(),
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
            device_id: arch,
            monitor_id: MonitorId(1),
            friendly_name: "Arch KDE".into(),
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

/// Canvas size in CSS pixels.
const CANVAS_W: f64 = 900.0;
const CANVAS_H: f64 = 460.0;
/// Breathing room so a monitor dragged to the edge is still grabbable.
const CANVAS_PAD: f64 = 40.0;

/// Scale factor that fits the whole arrangement into the canvas.
///
/// Recomputed from the live bounds rather than fixed, so dragging a monitor far away
/// zooms out instead of pushing it off-screen where it cannot be dragged back.
fn view_scale(layout: &Layout) -> (f64, f64, f64) {
    let Some(b) = layout.bounds() else {
        return (1.0, 0.0, 0.0);
    };
    let sx = (CANVAS_W - CANVAS_PAD * 2.0) / b.width.max(1.0);
    let sy = (CANVAS_H - CANVAS_PAD * 2.0) / b.height.max(1.0);
    let s = sx.min(sy).min(1.0);
    (s, b.x, b.y)
}

#[component]
fn App() -> Element {
    let mut layout = use_signal(demo_layout);
    let mut dragging = use_signal(|| None::<(usize, f64, f64)>);

    let snapshot = layout.read().clone();
    let (scale, origin_x, origin_y) = view_scale(&snapshot);
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
        style { {STYLE} }
        div { class: "app",
            h1 { "Display arrangement" }
            p { class: "hint",
                "Drag each screen into its real physical position. Edges snap together — "
                "the pointer can only cross where two screens actually touch."
            }

            div {
                class: "canvas",
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
}

const STYLE: &str = r#"
:root { color-scheme: dark; }
body { margin:0; background:#15171c; color:#e7e9ee;
       font:14px/1.5 system-ui,-apple-system,Segoe UI,sans-serif; }
.app { padding:24px 28px; }
h1 { font-size:20px; margin:0 0 4px; font-weight:600; }
h2 { font-size:14px; margin:0 0 8px; font-weight:600; color:#aeb4c0; }
.hint { color:#8b93a3; margin:0 0 18px; }
.canvas { position:relative; width:900px; height:460px; background:#1b1e25;
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
.badge { margin-top:4px; font-size:11px; padding:1px 6px; border-radius:999px;
         background:#3a4a66; color:#c9d6ea; }
.warn { margin-top:16px; padding:10px 12px; border-radius:8px;
        background:#3a2222; border:1px solid #a84a4a; color:#f0d6d6; }
.panel { margin-top:22px; }
ul { margin:0; padding-left:18px; }
li { margin:2px 0; color:#c3cad6; }
"#;
