# Ultidesk threat model

Ultidesk moves input, screen pixels, clipboard, and files between computers on a trusted
LAN. That is inherently sensitive. This document enumerates the adversaries and the
defenses, and is explicit about what is implemented today vs. planned.

## Trust boundaries

- **Peer ↔ peer** (network). Two paired devices over LAN. Mutual authentication required.
- **Renderer ↔ main ↔ agent** (local IPC). The renderer is the least-trusted local
  component; the agent is the most privileged.
- **User ↔ observed content**. Window titles, filenames, clipboard, and remote frames are
  untrusted *data*, never instructions.

## Adversaries and defenses

| Threat | Defense | Status |
|---|---|---|
| Passive LAN eavesdropper | TLS 1.3 control plane; DTLS-SRTP media | planned (M1) |
| Active MITM during pairing | SAS verification code from the handshake transcript; user confirms both sides match | planned (M1) |
| Spoofed device advertisement | Discovery is only a hint; connection requires the pinned identity | planned (M1) |
| Replayed control command | Session nonces, sequence numbers, monotonic timestamps | partial (seq/ts in schema) |
| Stolen paired identity | Private key in OS secret store (DPAPI/Secret Service); revocation | planned (M1) |
| Malicious but paired peer | **Source-side** permission enforcement; a receiver claiming a permission is not enough | design enforced (see permissions.md) |
| Revoked peer reconnecting | Pinned-identity check refuses revoked keys | planned (M1) |
| Unauthorized input injection | Input only accepted on an authenticated session with a valid lease; `can_forward_input` gated to `RemoteActive` | logic implemented + tested |
| Input loops / replay (A→B→A, rings) | Layered guard: injection marker, origin id, hop TTL, event de-dup | **implemented + tested** (`core::input_guard`) |
| Stuck modifiers after failure | Session tracks held keys/buttons; released on `ReleaseAllInput` **and** on any IPC disconnect; hardcoded emergency release `Ctrl+Alt+Shift+Esc` | release-on-disconnect **implemented + tested**; emergency hotkey planned (M2) |
| Clipboard bomb / oversized content | Size caps per format; bounded message size | cap constant in place; clipboard subsystem planned (M4) |
| Malicious HTML clipboard | Treated as untrusted; sanitized on render | planned (M7) |
| File path traversal / absolute paths | Manifest validation rejects `..`, absolute paths, and (initially) symlinks | planned (M6), rules specified |
| Filename collision / silent overwrite | Staging dir + atomic rename + explicit collision handling | planned (M6) |
| Disk exhaustion | Free-space check before accepting a transfer; configurable max size | planned (M6) |
| UI spoofing / invisible capture | Persistent tray + on-screen indicator whenever capture/control is active | planned (M3+) |
| Compromised destination | Encryption does not protect displayed pixels; Ultidesk is **not** a DLP boundary and says so | documented (permissions.md) |
| Protocol downgrade / malformed messages | Version negotiation, strict schema validation, bounded sizes, reject unknown enums | partial (IPC validated + size-bounded) |
| Renderer compromise | `nodeIntegration:false`, `contextIsolation:true`, `sandbox:true`, narrow preload, strict CSP, permission handler denies all by default | **implemented** in app config |
| Local IPC impersonation (other local user) | Per-launch token (constant-time compare) gates the pipe; **plus** user-restricted ACL on the pipe/handshake | token **implemented + tested**; ACL hardening = tracked follow-up |
| Sensitive logs | No key codes, clipboard, frames, filenames, titles, or tokens are logged; titles kept out of routine logs | **implemented** (agent logging) |
| Session alive after source lock | Projection suspends/terminates on lock (Work Device: terminate) | planned (M3/M9), state `Suspended` exists |

## Principles

Mutual authentication · least privilege · **source-side** permission enforcement ·
explicit consent · visible active-session indicators · session nonces · replay
resistance · schema validation · size/rate limits · idempotent safe cleanup · no content
logging · **no custom cryptography** (use TLS/DTLS/QUIC/Ed25519 from vetted libraries) ·
the local physical user can always preempt and terminate remote access.

## Known gaps in this slice (do not treat as secure yet)

- No pairing/identity/transport encryption is implemented — the peer network path is not
  built. The current projection path is a **dev loopback** inside one process only.
- Named-pipe ACL restriction to the current user is not yet applied (token only).
- Emergency-release hotkey and on-screen capture indicators are not yet implemented.

These are Milestone-1+ items. The system must not be described as secure until they land.
