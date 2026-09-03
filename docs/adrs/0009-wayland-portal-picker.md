# ADR-0009: On Wayland the compositor owns the picker, not Ultidesk

- Status: Accepted
- Date: 2026-09-02

## Context

The Windows backend works by enumeration: `EnumWindows` lists every top-level window,
Ultidesk filters that list, draws its own picker, and the user chooses. ADR-0005 assumed
the Linux backend would be "additive, not a rewrite" behind the same interface.

That assumption does not survive contact with Wayland. **Wayland has no API for one
client to see another client's windows.** It is not missing, it is refused: the
compositor is the only component with that knowledge, and it does not share it. There is
no `EnumWindows` to port.

Probing a real KDE Plasma 6.7 Wayland session (see compatibility.md) confirmed the two
KDE-specific escape hatches and why neither is acceptable:

- `org.kde.KWin.queryWindowInfo()` — works, but is *interactive*: it forces the user to
  click a window. It cannot populate a list.
- `org.kde.KWin` `/Scripting` `loadScript` — can reach `workspace.windowList()`, but
  only by injecting a script into the compositor process. It is KDE-only, breaks on
  GNOME and wlroots, and routes around the portal permission model entirely.

Meanwhile the sanctioned path is present and capable: `org.freedesktop.portal.ScreenCast`
v4 advertises `AvailableSourceTypes = 7`, which includes `WINDOW`.

## Decision

The Linux backend does not enumerate windows. `enumerate_top_level_windows()` in
`ultidesk-platform-linux` returns empty, permanently and by design, and says so in its
documentation rather than carrying a TODO.

Window selection on Linux inverts the Windows flow:

| Step | Windows | Wayland |
|---|---|---|
| Discover windows | `EnumWindows` | not possible |
| Draw the picker | Ultidesk | the compositor |
| Identify the choice | `HWND` | PipeWire node id |
| Permission | none (same session) | portal grant, revocable |

Ultidesk asks the ScreenCast portal to start a session; the compositor shows its own
picker; the user's choice comes back as a PipeWire node. The projection layer therefore
cannot assume it has a window *list* — only that it can obtain a window *stream*.

We do not use the KWin scripting or `queryWindowInfo` routes. This follows ADR-0006's
refusal to work around OS security boundaries, and keeps one code path that also works
on GNOME and wlroots.

## Can the per-window picker be skipped?

Asked directly, because selecting one window at a time is unusable for a workspace
where any window should be draggable to another machine. The honest answer is that
there is **no sanctioned bypass**, and three legitimate options with real trade-offs.

There is no compositor-blessed "capture everything" grant on Wayland. Anything that
produced one would be defeating the permission model rather than using it, which
ADR-0006 rules out. What follows is what can actually be built.

### 1. `multiple: true` — one dialog, many windows

`SelectSources` accepts `multiple`, and the picker then lets the user shift-select
several windows in a single prompt. Combined with `persist_mode = 2` and the returned
`restore_token`, that becomes *one* prompt covering a set of windows, replayed
silently on every later launch.

Best fidelity: each window arrives as its own PipeWire stream, composited by the
compositor, correct even when a window is covered or partly offscreen. Limitation:
the set is fixed at grant time, so a newly opened window needs a new prompt.

### 2. Capture the monitor once, crop per window

`AvailableSourceTypes` includes `MONITOR`, so one grant covers the whole screen and
any number of windows can be cropped out of it client-side, including windows opened
later. One prompt, forever, for everything.

The costs are severe and worth stating plainly:

- **Occlusion breaks it.** A monitor capture is the composited result. A window behind
  another is simply not in the frame, so its proxy shows whatever is on top of it.
  Per-window capture does not have this problem because the compositor renders each
  window separately.
- **Quality and bandwidth.** Encoding a whole 4K monitor to project one small window
  wastes most of the bitrate on pixels nobody asked for, then crops away the detail
  that mattered.
- **It is screen sharing.** The README's central claim is that Ultidesk is not that.
  A monitor grant means the app holds the whole screen even while projecting one
  window, which is exactly the privacy posture the project rejects.

### 3. Per-window grants, remembered forever

Prompt once per window and store its `restore_token` keyed by the application, so a
given app is approved once and never again. Worst first-run friction, best steady
state, and it keeps per-window fidelity.

### Decision

Pursue **(1) plus (3)**: `multiple: true` so a session can be authorised in a single
prompt, and persisted `restore_token`s so it is not re-asked. Option (2) is rejected
as a default because occlusion makes it *incorrect*, not merely wasteful — a proxy
window showing the wrong content is worse than one that needed a prompt. It may still
be worth offering explicitly for a "project my whole screen" mode, which is a
different feature honestly labelled.

## Consequences

- The picker UI must be conditional per platform: a list on Windows, a "choose a
  window…" affordance that delegates to the compositor on Wayland. This is a real UI
  difference, not something to abstract away.
- Ultidesk never learns the titles or geometry of windows the user did not choose. That
  is a privacy improvement, and it means the Windows-side habit of keeping a live window
  list has no Linux equivalent to keep in sync.
- Capture permission on Linux is grantable and revocable at any time by the user or the
  compositor. The projection state machine needs a "permission revoked mid-session"
  transition that the Windows path never required.
- `WindowInfo.hwnd` is meaningless on Linux. The shared shapes in
  `ultidesk-platform-windows` need a neutral home and a handle type that can represent
  either an `HWND` or a PipeWire node before the two backends really share an interface.
- Input is unaffected by this inversion: `RemoteDesktop` (inject) and `InputCapture`
  (grab at an edge) are both present at v2 and both map onto the existing model.
