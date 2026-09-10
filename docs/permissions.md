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

**Pairing and a per-peer permission store are built and enforced** (2026-09-10). Three
permissions, each with a request actually behind it — none is declared ahead of one:

| Permission | Gates | Default on pairing |
|---|---|---|
| `control-input` | pointer motion, buttons, keys, wheel | granted |
| `read-devices` | this machine's audio endpoints (monitors and topology as they land) | granted |
| `list-windows` | the window list, **titles included** | **not** granted |

Window listing is separate from device reading because a title says what the operator is
doing — the document open, the site being read, a customer's name — while an endpoint
list says a machine has speakers. Granting one must not grant the other.

Enforcement is source-side, as this document requires: the permissions come from the
*receiving* machine's own store, keyed by the public key the handshake proved, read once
per connection. A peer cannot assert its own and cannot change them by reconnecting under
a different name.

Two requests are ungated on purpose. `Ping`, because refusing liveness makes "the peer is
gone" and "the peer denies me" indistinguishable and discloses nothing the completed
handshake did not. `ReleaseAllInput`, because it only undoes what the session already
did — a peer whose input permission is revoked mid-connection must still be able to drop
a held modifier, or revoking a permission becomes the thing that leaves a key stuck down.

The local IPC is allowed everything. The token holder is a process running as this user
on this machine and could drive the same APIs directly; a check there would be theatre,
and would make these peer checks look like the same gesture.

Managed with `ultidesk-agent peers`, `peers allow <key> <perm>` and
`peers deny <key> <perm>`.

### Still not built

- Everything else in the per-peer list above — clipboard, files, projection approval,
  audio streams, reconnection — has no request behind it yet, and no permission is
  declared for it in advance.
- The **Work Device profile** as a whole, including approval-every-session, terminate-on-
  lock, and the audit trail.
- On-screen indicators and the tray, which the profile depends on.
