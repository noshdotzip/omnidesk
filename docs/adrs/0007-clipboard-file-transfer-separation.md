# ADR-0007: Clipboard sync and file transfer are separate subsystems

- Status: Accepted
- Date: 2026-07-31

## Context

It is tempting to treat "copy/paste files across machines" as one feature riding on the
clipboard. But source file paths (`C:\Users\Alice\report.docx`) are meaningless on another
computer, file bytes can be large and must not block input/video, and clipboard and file
drag-and-drop are genuinely different OS mechanisms (OLE vs. XDND vs. Wayland data-device
vs. portals). Conflating them produces broken pastes and UI that lies about what happened.

## Decision

Keep **clipboard synchronization** and **file transfer** as distinct, separately
permissioned subsystems that share the common identity/permission/session/logging model:

- Clipboard: format manifest, origin tracking, event versioning, loop suppression, size
  caps; text first, then HTML/images, then **file *offers*** (not raw `CF_HDROP` paths).
- File transfer: explicit send/approval, chunked/streamed/resumable, SHA-256 verified,
  staged + atomically finalized, path-traversal/symlink-safe, disk-space checked.
- A file clipboard offer triggers a **transfer-before-local-path**: the receiver fetches
  the bytes, verifies, writes locally, and only then puts a valid *destination-local* path
  into its native clipboard.
- Cross-device file drag is an Ultidesk-controlled edge drop target — it does **not**
  pretend a single native OS drag object travels across machines.

## Consequences

- Correct pastes and honest UX; large transfers never freeze input or projection.
- More surface area (two subsystems) but each is testable and permissioned independently.
- Clipboard history and rich formats are opt-in and off by default (Work Device: disabled).
