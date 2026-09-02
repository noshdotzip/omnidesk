# ADR-0006: No kernel driver and no virtual-display driver in the MVP

- Status: Accepted
- Date: 2026-07-31

## Context

Some "make it feel seamless" features tempt low-level solutions: a virtual display driver
to host projected windows off-screen, a kernel driver or filter for input, or `uiAccess`/
elevation tricks to defeat UIPI. These carry heavy costs: driver signing, install-time
privilege, broad compatibility testing, a large attack surface, and — for the UIPI tricks —
crossing security boundaries we have committed not to cross.

## Decision

The MVP uses **no kernel driver** and **no virtual-display driver**. Projection leaves the
real source window usable (not minimized/cloaked/moved to a virtual display) because many
apps stop rendering when hidden, and shows a visible "Projected to <device>" indicator.
Input uses supported user-mode APIs (`SendInput`) and **fails closed** against elevated
targets rather than elevating. Optional privileged features (virtual HID, `/dev/uinput`,
a virtual display, a startup service) are separate, explicit, signed, narrowly scoped,
independently reviewed post-MVP projects.

## Consequences

- Lower risk, no driver-signing burden, honest about what can/can't be captured/controlled.
- Some scenarios (elevated MMC/RSAT control, exclusive-fullscreen games, source-side
  visual hiding) are limited or unavailable in the MVP — documented, not faked.
- Better source-window behavior for capture reliability at the cost of the source screen
  visibly showing the projected window.
