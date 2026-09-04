# ADR-0011: Audio routing is a device graph, and cycles in it are refused

Status: **Accepted** (2026-09-04)

## Context

One of the three product goals is routing sound between machines: capture what one
machine is playing and play it on another. The naive model — "pick a source machine and
a destination machine" — has a failure mode that is not merely a bug.

Capturing an output device means capturing its *monitor*: everything currently playing
on it. If that stream is played back onto the same output, the playback is itself
captured and sent again, gaining each pass. This is a runaway feedback loop. At
headphone volume it is a hearing-safety problem, not an annoyance, and it can be created
by two clicks in a settings panel.

So the routing table needs a validity rule, and the rule has to be exactly right in both
directions: too permissive and it lets someone build the runaway; too strict and it
forbids setups people legitimately want.

## Decision

Model routing as a directed graph whose **nodes are individual devices** and whose
**edges are output-sourced routes**. Refuse any table containing a cycle.

Two details decide correctness, and both are easy to get wrong:

**It is per-device, not per-machine.** Capturing sink X and playing to sink Y on the same
machine is fine — Y's audio never reaches X's monitor. A per-machine rule would reject
every same-machine route, forbidding a real use case (moving game audio from one output
to another) to prevent a loop that does not exist.

**Only output-sourced routes can close a loop.** A microphone is not a monitor: playing a
mic onto the machine's own speakers cannot re-enter the capture digitally. It can howl
acoustically, but that is a room problem and blocking it would forbid intercom setups.
So mic-sourced edges are not walked during cycle detection.

Cycle detection therefore catches the direct self-route, the two-machine round trip
(A's mix to B, B's mix back to A), and longer rings, while leaving fan-out, fan-in
(mixing several machines into one speaker), same-machine cross-routing, and every
mic-sourced route allowed.

The rule lives in `ultidesk-topology::audio`, not in the UI, for the same reason the
layout math does: a UI that computed it independently could accept a table the agent
refuses.

## Alternatives rejected

**Per-machine cycle detection.** Simpler, and wrong in both directions: it forbids
legitimate same-machine routes and — because it cannot see which device a route lands on
— would still need per-device information to be sure about the cases it does allow.

**Refusing all same-machine routes.** The cheap version of the above. Rejected for the
same reason: it solves the safety problem by removing a feature.

**Detecting the loop at runtime (level or correlation detection on the stream).** Would
catch acoustic feedback too, which the static rule cannot. Rejected as the *primary*
mechanism: it necessarily acts after the loop has started, which is exactly when the
volume is already climbing. A static rule that makes the dangerous table unbuildable is
the right first line. Runtime detection remains open as a later addition for the
acoustic case.

## Consequences

Route errors must be rendered through `AudioRouting::explain`, not `Display`. The error
type does not carry the device inventory, and on Windows a node id is a GUID
(`{0.0.0.00000000}.{b6a8d9b4-…}`); a message built from ids names the loop without
telling the operator which speaker to change. `explain` resolves ids to friendly names
and falls back to the id only when the device is gone.

Saved routes are keyed by `(machine, node id)`, not by friendly name. Two identical
headsets produce the same name, and a name changes when the user renames the device.

Devices that disappear do not silently drop their routes. `stale_routes` surfaces them,
so audio that stops because something was unplugged has a visible cause.

## Enumeration

Both platforms enumerate natively rather than shelling out — the PipeWire registry on
Linux, WASAPI endpoint enumeration on Windows. `pw-dump` would have worked but
serialises the entire graph to find a handful of nodes and makes the settings panel
depend on the CLI tools.

The Linux walk needs **two** `sync` round-trips. The default sink and source are not
node properties; they live on a metadata object that can only be bound part-way through
the first round, so its property events are queued behind that round's `done`. This was
measured, not assumed: quitting on the first `done` returns every device with
`is_default: false` (0 of 6 marked), while two round-trips return the correct 2.

The walk is bounded by a 3-second timer. A settings panel that hangs forever on a wedged
daemon is worse than one that reports that it could not read the devices.

Windows loopback capture takes an optional endpoint id rather than always using the
default, so the panel cannot offer a device that the capture path would ignore.

## Status of the surrounding pieces

The routing model and enumeration are done and verified on both machines. What is **not**
built: the transport still carries uncompressed PCM over plaintext TCP (Opus/RTP over the
ADR-0002 transport is the target), playback on Linux still shells out to `pw-play`, and
the control app can only read the *local* machine's devices — a peer's inventory needs
the settings IPC surface, which does not exist. The panel labels the peer as not
connected rather than showing a plausible placeholder.
