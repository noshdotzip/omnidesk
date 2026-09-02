# ADR-0004: Protocol schema — Protocol Buffers canonical, generated bindings before P2P

- Status: Accepted
- Date: 2026-07-31

## Context

The brief requires a versioned cross-language schema and forbids maintaining two divergent
copies of the protocol. Setting up full protobuf codegen (prost + a TS generator) has real
setup cost, and the system has no system `protoc`. The only live cross-language contract in
Milestone 0 is the **local IPC** (agent ↔ desktop app), which is small.

## Decision

- `protocol/ultidesk.proto` is the **canonical reference** schema (proto3), version-tagged.
- For the Milestone-0 local IPC only, the wire format is JSON whose shapes match the serde
  enums in `crates/agent/src/ipc.rs` and the discriminated unions in
  `apps/desktop/src/shared/protocol.ts`. These mirrors are kept in lockstep and **guarded
  by tests** (projection state and coordinate mapping have parity unit tests on both sides;
  IPC auth/version behavior is tested in Rust).
- **Before any peer-to-peer protocol ships (Milestone 1)**, generated bindings are
  introduced — prost (with vendored `protoc`, since none is installed) for Rust and
  protobufjs/ts-proto for TypeScript — and the hand-mirrored surface is not grown further.

## Consequences

- Zero external toolchain dependency for the current slice; fast iteration.
- A bounded, explicit debt: the hand-mirror is acceptable only while the surface is tiny
  and test-guarded. Growing it without codegen is prohibited (tracked in status.md).
- `PROTOCOL_VERSION` is defined once in Rust and mirrored in TS and the `.proto`.
