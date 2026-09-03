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
