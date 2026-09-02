# Ultidesk permissions

## Principle: enforce on the device providing the capability

A peer that *claims* it has permission is never sufficient. The device that owns the
resource (the source of frames/input, the holder of the clipboard, the file sender/
receiver) checks the granted permission for that specific peer before acting. A malicious
receiver cannot grant itself capture or control.

## Per-peer permissions

Each peer relationship independently configures:

- Control this computer / Control only while approved
- Send cursor/keyboard input
- Project windows from this computer / Receive projected windows
- Read text clipboard / Write text clipboard / Share rich clipboard formats
- Send files / Receive files
- Receive audio
- Launch configured applications
- Automatically reconnect
- Remember projection approval / Require approval every session

## Profiles

**Trusted personal peer (default):** text clipboard on, files opt-in, audio off,
projection remembers approval.

**Work Device profile (predefined, restrictive):**

- Clipboard disabled · file transfer disabled · audio disabled
- Window projection requires approval **every session**
- Remote desktop control requires approval **every session**
- No automatic reconnection after lock
- Terminate projection when the source **locks**; no access while locked
- No persistent frames, thumbnails, or clipboard history
- No file Inbox unless explicitly enabled
- Visible tray + on-screen indicator while capture/control is active
- One-click terminate-all-sessions
- Audit **connection metadata only** — never typed keys, clipboard, pixels, filenames,
  or file contents; active window names are not exposed in ordinary logs

## OS security boundaries Ultidesk respects (never bypasses)

- **Windows UIPI / integrity levels**: a normal-integrity agent cannot inject into an
  elevated window. We surface this as `input_blocked` (the OS `ERROR_ACCESS_DENIED` from
  `SendInput`) instead of silently dropping input. No silent elevation, no `uiAccess`
  tricks, no UAC/Secure Desktop interaction.
- **Wayland**: capture/input go through the compositor's permission portals
  (ScreenCast/RemoteDesktop/InputCapture). No silent window enumeration or interception.
- **Anti-cheat, DRM, protected surfaces**: not bypassed; failures are reported clearly.
- **Organizational controls** (Group Policy, MDM, endpoint protection, firewall, DLP,
  application allowlists): not circumvented.

## Honest limitations the UI must state

- Ultidesk does **not** make a personal destination device compliant with workplace policy.
- Encryption does not protect content once it is displayed on a compromised/unauthorized
  destination. Ultidesk cannot prevent the destination from taking screenshots.
- Ultidesk is **not** a DLP boundary.
- Showing RSAT/MMC, student data, organizational records, or credentials on a personal
  device may violate workplace rules. Users must have authorization.

## What is implemented today

- The source-side enforcement *model* and the `input_blocked` surfacing are implemented.
- The per-peer permission store, pairing, and Work Device runtime enforcement are
  Milestone-1+ and not yet built. See [status.md](status.md).
