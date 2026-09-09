# What's next

Written 2026-09-09 after the uinput/evdev sprint; revised the same day after the
identity and transport work landed. Companion to
[docs/status.md](docs/status.md), which records what is *verified*; this one records
what I would build next and what is genuinely in the way.

---

## Where the three product goals actually stand

**1. Mouse and keyboard across all screens — closest to done.**

Windows → Arch works end to end and is pixel-accurate: the Windows agent drove the Arch
cursor over TCP through uinput, landing at 399,298 for a requested 400,300, with no
permission dialog anywhere. Arch → Windows is code-complete along two independent paths
(libei via the portal, and evdev directly) but neither has run against a live desktop.

What is missing is not a backend. It is the **orchestration**: something that watches
the pointer, notices it hit an edge that `Layout` says is shared, decides which peer
owns that edge, grabs input, streams it, and hands back on the return crossing.
`KvmMachine` holds the state transitions and `Layout` holds the geometry, but nothing
joins them to a live pointer and a connection. Today that only exists as demo
subcommands driven by hand.

As of 2026-09-09 the connection those subcommands should be using now exists and is
authenticated, so the assembly no longer has to be built twice.

**2. Dragging windows between machines — least complete.**

Capture is real: zero-copy DMA-BUF, per-window, verified. Everything after it is
missing — there is no encoder, no media transport, and no way to display a remote
window locally. This is the goal with the most unbuilt surface, and the one most
dependent on the transport work below.

**3. Routing audio between machines — model done, plumbing not.**

Device enumeration, the routing graph and the loop refusal are built, tested and
verified on both machines. The stream itself is still uncompressed PCM over plaintext
TCP, and Linux playback shells out to `pw-play`.

---

## What I would build next, in order

### 1. ~~The secure peer transport (ADR-0002)~~ — **done 2026-09-09**

`ultidesk-transport`: QUIC over TLS 1.3, mutually authenticated, pinned to Ed25519
device identities. Pairing (a six-digit code compared on both screens), revocation,
`serve-peer` and `peer-ping` all work; an unpaired machine is refused at the handshake,
by fingerprint, before reaching any dispatcher. Details and the exact measurements are in
[docs/status.md](docs/status.md).

Two things this did **not** finish, both deliberate:

- **Pointer motion still rides the ordered control stream.** ADR-0002 calls for
  datagrams, and QUIC provides them; it matters when the KVM daemon streams motion, not
  before.
- **It has never run between the two machines.** Both ends of every measured run were on
  the Windows box. That makes it a verified protocol and an unverified link.

The plaintext TCP transport is still present as `serve-peer-dev`, kept for bench
comparison. It should be deleted once item 3 runs on the real channel.

### 2. Local IPC on Linux, then the settings IPC

The control app can read the *local* machine's monitors and audio devices, and says so
honestly for the peer. Making the peer real needs the agent to answer questions — and
on Linux the agent has **no local IPC transport at all**. `pipe.rs` is Windows-only
named pipes; the Linux equivalent (a Unix socket in `$XDG_RUNTIME_DIR`) does not exist.

Once it does, the same request set serves both: monitors, audio devices, current
topology, and applying a changed one. That turns the control app from an editor of its
own in-memory state into an actual control surface.

### 3. Wire the KVM together as a daemon

With a transport and an IPC surface, goal 1 becomes real: `serve` watches the pointer,
consults `Layout` for shared borders, drives `KvmMachine`, and forwards through the
`Forwarder` that already exists. The pieces are built and tested in isolation; this is
the assembly.

The one piece genuinely missing is **hotkey-free emergency release on Linux**. On
Windows `hotkey.rs` registers one. With evdev grabbing every keyboard, the release has
to be detected inside the captured stream itself, because nothing else will see it.

### 4. Opus for audio, and a native PipeWire playback client

Uncompressed stereo s16 at 48 kHz is ~1.5 Mbit/s with no jitter buffer. On the measured
link that is the wrong shape twice over: the bandwidth is affordable but the latency
variance is not, and a dropout has nothing to hide behind. Opus over RTP fixes both, and
replacing the `pw-record`/`pw-play` shims removes two subprocesses from the media path.

### 5. Then window projection

Encoder, media transport, remote window surface. Deliberately last: it is the largest
piece and the one that benefits most from the transport already being solid.

---

## The permission story, and how much of it is now bypassed

This was raised repeatedly and deserves a plain summary, because the three cases have
genuinely different answers.

### Input injection — solved, no permission needed at all

`uinput` replaces the RemoteDesktop portal entirely. `/dev/uinput` is root-owned but
logind grants the active session's user an ACL on it, so **no sudo, no udev rule, no
group change, and no dialog**. Verified on the machine. `serve-peer-dev` now prefers it
and reports which injector it chose.

It is also better than the portal on the merits: absolute positioning instead of
dead-reckoned relative motion, and immune to libinput's pointer acceleration — which
was measured turning an injected +700,+450 into roughly +1350,+875.

### Input capture — solved, one command away

`evdev_capture` replaces the InputCapture portal. It needs read access to
`/dev/input/event*`, which logind does *not* grant:

```bash
sudo usermod -aG input $USER
```

Then log out and back in. `ultidesk-agent input-devices` will list the capturable
devices; today it reports the permission error instead. The code is written and its
logic is tested; only the live run is waiting.

### Screen capture — approve-once available, never-ask is a real project

This is the honest one, and it is not as good as the other two.

**Available now:** the ScreenCast portal's own `restore_token` with `persist_mode: 2`,
which is already implemented. Approve the picker **once**, store the token, and every
later session restores silently with no dialog. That directly answers "I don't want to
approve every time I connect". It needs one run with someone at the machine to
establish the token — that is the outstanding blocker below.

**Its limit:** the token restores *the windows that were picked*. A window opened later
is not in it. For "every window, always, no picker", the portal has no answer by
design.

**What a true bypass would take.** I looked at the routes and none is cheap:

- *A KWin plugin.* KWin loads C++ plugins in-process with access to every window's
  texture. This is the only route that genuinely gives all windows with no picker. It
  is a separate C++ codebase, tied to KWin's unstable internal ABI, and needs rebuilding
  against each Plasma release. Real, but a project rather than a task.
- *`wlr-screencopy`.* The obvious Wayland answer, and KWin does not implement it — it is
  a wlroots protocol.
- *KWin's D-Bus surface.* Exposes `/Scripting` and `/Effects`; scripting can enumerate
  windows but cannot hand out buffers. Screenshot APIs exist but are single-shot, not a
  stream.
- *DRM/KMS scanout capture.* Requires DRM master, which the compositor holds. Would mean
  displacing KWin.
- *Running as root.* Does not help. The picker is a compositor policy decision, not a
  filesystem permission, so root does not skip it.

My recommendation: ship approve-once via `restore_token`, capture whole outputs when the
operator wants "everything" rather than enumerating windows, and treat the KWin plugin
as a deliberate later decision rather than something to attempt in passing.

---

## Big blockers

**Ordered by how much they hold back.**

1. ~~**No secure transport.**~~ **Resolved 2026-09-09.** What replaces it as the top
   blocker is that it has never carried a byte between the two *machines*: the protocol
   is verified, the link is not. One cross-machine `pair` plus `peer-ping` would settle
   it, and needs someone at the Arch machine.

2. ~~**No device identity or pairing.**~~ **Resolved 2026-09-09.** `DeviceId` is derived
   from an Ed25519 public key, and two agents have paired for real over the live channel
   (`ultidesk-identity`, [ADR-0012](docs/adrs/0012-device-identity.md)). What is left of
   it: the private key is still a plain file rather than DPAPI / Secret Service, and it
   is unrestricted on Windows, which has no mode bits. Any process running as this user
   can read it.

3. **No local IPC on Linux.** `pipe.rs` is Windows-only. The control app cannot ask the
   agent anything on Linux, so every peer-side panel stays a placeholder.

4. **The ScreenCast picker**, as above. Approve-once is available and unverified; never-ask
   is a KWin plugin.

5. **No video encoder on the Arch machine.** `/dev/dri/renderD128` exists but no VAAPI
   encoder plugins are installed, and GStreamer has neither `pipewiresrc` nor any H.264
   encoder. Window projection cannot progress past capture until that is resolved.

6. **The Wi-Fi power-save fix does not survive a reboot.** It is a live `iw` setting.
   Persist it with a NetworkManager drop-in at
   `/etc/NetworkManager/conf.d/wifi-powersave.conf` containing `wifi.powersave = 2`, or
   plug in the idle `eno1` and remove the variable entirely. Without it, round trips go
   back to ~150 ms average with half-second spikes, which is enough to make the KVM feel
   broken and to make any latency measurement meaningless.

### Waiting on someone at the Arch machine

- `sudo usermod -aG input $USER`, then re-login — unblocks evdev capture.
- One ScreenCast picker approval — establishes the `restore_token`, and also confirms the
  scroll sign conventions, which are currently derived from documentation rather than
  measured.

---

## Known debt that is not shippable as-is

Carried here so it is not rediscovered later:

- **The plaintext TCP peer transport still exists** as `serve-peer-dev`, kept for bench
  comparison against the QUIC path. Delete it once the KVM daemon runs on the real
  channel; a dev transport that outlives its purpose is one somebody eventually ships.
- **`pw-record` / `pw-play` subprocesses** in the audio path, instead of a native
  PipeWire client.
- **Uncompressed PCM** on the wire, with no jitter buffer.
- **`global-hotkey` segfaults when X11 is unreachable.** A non-optional dependency of
  `dioxus-desktop` on Linux with no feature flag; it calls `XDefaultRootWindow` without
  checking that `XOpenDisplay` succeeded. A normal desktop launch is fine; a pure
  Wayland session with no XWayland is not. Recorded in
  [ADR-0010](docs/adrs/0010-dioxus-control-ui.md).
- **A paired peer may send every request the dispatcher accepts.** Pairing is currently
  all-or-nothing: there is no per-peer permission store, so "this machine may control me"
  and "this machine may read my clipboard" are the same decision. Source-side enforcement
  is designed in docs/permissions.md and not built.
- **The peer's placement is persisted by the literal string "Peer (not connected)".**
  Fine while there is one placeholder peer; it needs to become a real device id the
  moment pairing exists.
- **Mixed coordinate spaces across machines are unresolved.** Monitor geometry is stored
  in whatever the platform reports — physical pixels on Windows, logical on Wayland. On
  one machine that is coherent. Across two machines with different scale factors it is
  not yet defined which space the shared topology is expressed in, and getting it wrong
  puts crossings in the wrong place. Nothing depends on it until the settings IPC
  carries a peer's monitors, which is exactly when it must be decided.
