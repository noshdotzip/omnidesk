# Ultidesk

Ultidesk is a **distributed desktop compositor**: it makes several physical computers
feel like one workstation. Applications keep running on their original computer; their
windows can be **projected** onto another computer, where each appears as an
individually movable, native-looking proxy window that is interactive. Input, clipboard,
and files can flow between machines under explicit, per-peer permission.

It is **not** full-desktop screen sharing, VNC, RDP, or process migration. The real
process never leaves its source machine — see [docs/architecture.md](docs/architecture.md).

> ⚠️ **Status: early Milestone-0 vertical slice.** A verified foundation exists (secure
> Rust core, Windows window enumeration, authenticated local IPC, coordinate/state
> logic). The end-to-end WebRTC projection GUI is implemented but **not yet runtime-
> verified** — see [docs/status.md](docs/status.md) for exactly what is and isn't proven.

## Repository layout

```
apps/desktop/        Electron/TypeScript app: picker, proxy windows, WebRTC, agent bridge
crates/core/         Pure logic: projection state machine, input loop guard, ids, protocol
crates/topology/     Monitor topology + letterbox/edge-crossing coordinate math
crates/platform-windows/  Win32 window enumeration + SendInput injection
crates/agent/        User-session agent binary + authenticated named-pipe IPC
protocol/            Canonical wire schema (ultidesk.proto)
docs/                Architecture, threat model, protocols, permissions, testing, status, ADRs
```

Only the crates the current slice needs exist; future subsystems (identity, discovery,
control transport, clipboard, transfer) are **not** stubbed out — see
[docs/status.md](docs/status.md).

## Prerequisites

- Rust stable (pinned via `rust-toolchain.toml`), tested with 1.93 on
  `x86_64-pc-windows-msvc` and 1.86 on `aarch64-pc-windows-msvc`.
- Node 20+ and pnpm 10+.

**On Windows ARM64, install through Corepack**, not the standalone `pnpm.exe`:

```bash
corepack enable
corepack pnpm install
```

The `pnpm.exe` distributed for Windows is an x64 binary. Under Prism it reports itself
as x64 and silently installs the x64 esbuild and rollup binaries, which then either run
emulated or cannot be loaded by native ARM64 Node at all. A `preinstall` check fails the
install if that happens; see [ADR-0008](docs/adrs/0008-windows-arm64-native.md).

## Build, test, verify

Rust (fully runnable and verified on Windows):

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p ultidesk-agent -- enumerate    # prints capturable top-level windows as JSON
```

TypeScript (pure-logic tests + typecheck are verified; GUI run is a follow-up):

```bash
ELECTRON_SKIP_BINARY_DOWNLOAD=1 corepack pnpm install
corepack pnpm --filter @ultidesk/desktop test   # projection-state, mapping, media-stats
corepack pnpm --filter @ultidesk/desktop typecheck
```

On x64 a plain `pnpm` works identically; `corepack` is written out here because it is
the form that is correct on both architectures.

Running the Electron GUI end-to-end requires a renderer bundling step (Vite) that is the
immediate next task; see [docs/status.md](docs/status.md) and
[docs/testing.md](docs/testing.md) for the manual projection test plan.

## Security & scope

Ultidesk respects OS security boundaries. It never bypasses Windows UIPI/UAC, the Secure
Desktop, Wayland's permission portals, anti-cheat, or organizational policy (Group
Policy, MDM, DLP). Displaying work data on a personal device may violate workplace rules;
Ultidesk does not make a personal device compliant. See
[docs/permissions.md](docs/permissions.md) and [docs/threat-model.md](docs/threat-model.md).

## License

No license has been chosen yet — the repository is currently `UNLICENSED` / all rights
reserved. Do not assume open-source terms until a license is added deliberately.
