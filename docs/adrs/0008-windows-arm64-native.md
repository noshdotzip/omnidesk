# ADR-0008: Windows ARM64 is a native target, never an emulated one

- Status: Accepted
- Date: 2026-09-02

## Context

Windows 11 on ARM ships **Prism**, an x64 translation layer good enough that an x64
build of Ultidesk would run without visible errors. That makes emulation the *default
failure mode* rather than an obvious one: nothing warns, and the cost shows up only as
latency and battery drain — precisely the two budgets a projection compositor cannot
afford.

The trap is not in our own code. It is in the toolchain. On an ARM64 machine:

- The standalone `pnpm.exe` distributed for Windows is an **x64** binary (it bundles its
  own x64 Node). Under Prism it reports `process.arch === "x64"`, so it resolves every
  `cpu`-gated optional dependency to the x64 variant.
- Measured on a Snapdragon dev machine, a plain `pnpm install` selected
  `@esbuild/win32-x64` and `@rollup/rollup-win32-x64-msvc`. The first is an x64
  *executable* invoked on every build; the second is a native addon that ARM64 Node
  **cannot load at all**, silently demoting rollup to its slow JS fallback.
- Unlike WOW64, Prism does not set `PROCESSOR_ARCHITEW6432`, so the usual emulation
  probe does not fire. `PROCESSOR_IDENTIFIER` is the one environment variable left
  intact ("ARMv8 (64-bit) ... Qualcomm Technologies Inc").

Measured cost of the emulated path, bundling the desktop main process with esbuild
(15 iterations, after a warm-up):

| esbuild binary   | median | min     | max      |
|---|---|---|---|
| ARM64 native     | 29.4 ms | 26.8 ms | 39.8 ms  |
| x64 under Prism  | 44.1 ms | 41.5 ms | 131.7 ms |

That is ~1.5x on a startup-dominated workload; a full renderer bundle has more to lose.

### The shipped agent binary, native vs translated (measured 2026-09-02)

The same release profile cross-compiled both ways and driven over its real named-pipe
IPC (`ultidesk-agent serve`; n=3000 Ping, n=300 EnumerateWindows, three runs, warmed
up; cold start n=25). The x64 process was confirmed to be translated rather than
assumed so: it loads `xtajit64se.dll`, the Prism JIT engine.

| Metric | ARM64 native | x64 under Prism | Ratio |
|---|---|---|---|
| Cold start (`enumerate`, whole process) p50 | 17.2 ms | 37.6 ms | **2.2x** |
| Idle working set (`serve`), mean of 3 | 9,957 KB | 20,277 KB | **2.0x** |
| `EnumerateWindows` RTT, mean | ~0.099 ms | ~0.132 ms | ~1.3x |
| `Ping` RTT p50 | ~0.021 ms | ~0.023 ms | no reliable difference |
| Binary size | 1350 KB | 1508 KB | 1.12x |

The nuance is recorded deliberately, because it cuts against the decision: **steady-
state IPC is very nearly Prism-neutral.** Ping and EnumerateWindows are dominated by
kernel transitions — named-pipe I/O and Win32 window enumeration — and Prism does not
translate kernel-side work. The Ping gap sits inside run-to-run noise; one of the three
runs tied exactly. Anyone re-running this should expect the same and should not report
a speedup there.

Where translation actually costs is process startup, resident memory, and CPU-bound
user-mode code. That is not a reason to tolerate it, because the agent is the *least*
CPU-bound component Ultidesk has. The components that will dominate the projection
budget — the Chromium encode path and the renderer bundler — are precisely the
CPU-bound workloads where the esbuild measurement above shows the penalty landing.
Cold start also matters more than it looks: the desktop app spawns the agent on launch.

Benchmark harness: `scripts/bench-agent.mjs`. It exercises Ping and EnumerateWindows
only — never `InjectMouseMove`/`InjectKey`, which call `SendInput` and would move the
operator's cursor and type into whatever window holds focus.

## Decision

Windows ARM64 is a **native** target. Ultidesk does not ship, build, or test through
Prism.

1. The JavaScript toolchain is installed by a package manager running on native ARM64
   Node — in practice `corepack pnpm`, since `packageManager` is already pinned.
2. A `preinstall` guard (`scripts/check-native-arch.mjs`) hard-fails an install whose
   package manager or Node is emulated, comparing `npm_config_user_agent` and
   `process.arch` against the true CPU read from `PROCESSOR_IDENTIFIER`. Escape hatch:
   `ULTIDESK_ALLOW_EMULATED_TOOLCHAIN=1`.
3. `pnpm.supportedArchitectures` was tried as belt-and-braces (pin both Windows
   arches so a wrong-arch install is still survivable) and then **removed**. pnpm
   applies it as a full `os x cpu` cross product, so on the Arch Linux dev box it
   pulled four esbuild and five rollup variants — including `linux-arm64`, which is
   useless there — for roughly 50 MB of pure waste on every install, on every
   platform. The guard in (2) already blocks the actual failure mode before install,
   so the redundancy was not worth its cost. If cross-arch artifact builds are ever
   needed from CI, add it back scoped to that job rather than to every developer.

### Rust: no ARM64 tuning flags

The obvious lever — `-C target-feature=+lse` for ARMv8.1 single-instruction atomics —
was tried and **measured to be a no-op**. Disassembling `ultidesk-agent.exe`:

| build | LSE atomics | LL/SC pairs |
|---|---|---|
| default | 919 | 104 |
| `+lse` | 919 | 104 |
| `-lse` | 0 | 1923 |

Rust's `aarch64-pc-windows-msvc` target already enables LSE; `--print cfg` simply does
not surface it as a `target_feature`. The flag is therefore **not** configured — a
no-op carrying a comment that claims a benefit is worse than no config at all.

Note the scope limit for any future attempt: `RUSTFLAGS` affects locally codegen'd code
only. Precompiled `std` internals would need `-Z build-std`, which is nightly and
excluded by the toolchain rule in `rust-toolchain.toml`.

## Consequences

- Contributors on ARM64 must install via `corepack pnpm install`. A bare `pnpm install`
  from the x64 launcher now fails fast with remediation text instead of silently
  producing an emulated toolchain.
- x64 Windows is unaffected: the guard only engages when the host CPU is ARM.
- Whether Chromium reaches the Snapdragon hardware H.264 encoder is a *runtime*
  question this ADR does not settle. `MediaStats.hardwareEncode` / `hardwareDecode`
  exist to answer it once the GUI runs; a software-OpenH264 fallback on ARM64 is to be
  treated as a bug, not a baseline.
