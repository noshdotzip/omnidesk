# What's next

Written 2026-09-09, after the uinput/evdev sprint. Companion to
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

### 1. The secure peer transport (ADR-0002)

Everything cross-machine currently runs over **plaintext TCP gated by a shared token**,
including keystrokes. That is fine for a bench on a trusted LAN and is not shippable in
any other sense. It also blocks the rest of this list: pairing and the settings IPC both
want a real channel to sit on, and building them against the dev transport means
building them twice.

This is first not because it is the most interesting but because everything else
inherits from it.

**Identity for it now exists** (`ultidesk-identity`, 2026-09-09): the keys to pin, the
store to pin them in, and the pairing code are built and tested. What is missing is the
channel that presents them.

**A toolchain note found while starting it.** `quinn`/`rustls` reach `ring`, and ring's
build script *hard-requires clang* on `aarch64-pc-windows-msvc` — it overrides whatever
compiler cc-rs found and asks for `clang` by name, because MSVC cannot assemble its
AArch64 sources. So the QUIC stack does not build on the Windows ARM64 machine until
LLVM is installed there. The Arch machine already has clang.

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

1. **No secure transport.** Plaintext TCP with a shared token carries every keystroke
   today. Blocks shipping anything, and blocks building pairing and settings IPC once
   rather than twice.

2. ~~**No device identity or pairing.**~~ **Resolved 2026-09-09** for the offline half:
   `DeviceId` is now derived from an Ed25519 public key, and pinning plus the six-digit
   pairing code are built and tested (`ultidesk-identity`,
   [ADR-0012](docs/adrs/0012-device-identity.md)). What remains needs blocker 1: no key
   has ever been exchanged, so no peer has been pinned outside a unit test. The private
   key is also still a plain file rather than DPAPI / Secret Service.

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

- **Plaintext TCP peer transport.** See blocker 1.
- **`pw-record` / `pw-play` subprocesses** in the audio path, instead of a native
  PipeWire client.
- **Uncompressed PCM** on the wire, with no jitter buffer.
- **`global-hotkey` segfaults when X11 is unreachable.** A non-optional dependency of
  `dioxus-desktop` on Linux with no feature flag; it calls `XDefaultRootWindow` without
  checking that `XOpenDisplay` succeeded. A normal desktop launch is fine; a pure
  Wayland session with no XWayland is not. Recorded in
  [ADR-0010](docs/adrs/0010-dioxus-control-ui.md).
- **The peer's placement is persisted by the literal string "Peer (not connected)".**
  Fine while there is one placeholder peer; it needs to become a real device id the
  moment pairing exists.
- **Mixed coordinate spaces across machines are unresolved.** Monitor geometry is stored
  in whatever the platform reports — physical pixels on Windows, logical on Wayland. On
  one machine that is coherent. Across two machines with different scale factors it is
  not yet defined which space the shared topology is expressed in, and getting it wrong
  puts crossings in the wrong place. Nothing depends on it until the settings IPC
  carries a peer's monitors, which is exactly when it must be decided.
