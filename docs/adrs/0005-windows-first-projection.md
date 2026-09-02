# ADR-0005: Windows-to-Windows projection first

- Status: Accepted
- Date: 2026-07-31

## Context

The primary deployment includes two Windows 11 laptops and an Arch Linux laptop. Wayland
capture/input is permission-mediated and varies by compositor and portal version; Windows
capture/input via documented Win32 APIs is more uniform. Trying to bring all platforms up
simultaneously would spread verification too thin and risk shipping unverified claims.

## Decision

Implement and verify **Windows-to-Windows** manual Window Projection first (Milestone 3),
then KVM (Milestone 2 is Windows-first too), and only afterward tackle Linux backends
(Milestone 9) with a real per-compositor compatibility matrix. The development machine is
`x86_64-pc-windows-msvc`, so Windows behavior can actually be exercised.

## Consequences

- Fastest path to a genuinely verified vertical slice on real hardware.
- Platform specifics stay behind traits/interfaces (`platform-windows`, future
  `platform-linux`) so Linux is additive, not a rewrite.
- ARM64 Windows and Linux remain "Untested" until executed — never marked supported by
  virtue of an API existing (see compatibility.md).
