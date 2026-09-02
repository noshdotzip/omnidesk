/**
 * TypeScript mirror of the Rust `ultidesk-core::projection` state machine.
 *
 * The destination proxy (renderer/main) and the source agent (Rust) must agree on the
 * projection lifecycle exactly. This file is kept in lockstep with
 * `crates/core/src/projection.rs`; the parity is enforced by the tests in
 * `state.test.ts` (same transition table, same input-gating rule). If you change one,
 * change both.
 */

export type ProjectionState =
  | "Local"
  | "Selecting"
  | "Authorizing"
  | "CaptureStarting"
  | "Negotiating"
  | "WaitingForFirstFrame"
  | "RemoteActive"
  | "HandoffPreparing"
  | "HandoffCommitting"
  | "Returning"
  | "Suspended"
  | "Error"
  | "Closed";

export type ProjectionEvent =
  | "Select"
  | "AuthorizeGranted"
  | "AuthorizeDenied"
  | "CaptureStarted"
  | "CaptureFailed"
  | "NegotiationComplete"
  | "NegotiationFailed"
  | "FirstFrame"
  | "FirstFrameTimeout"
  | "HandoffRequested"
  | "HandoffCommitted"
  | "HandoffAborted"
  | "ReturnRequested"
  | "ReturnComplete"
  | "Suspend"
  | "Resume"
  | "Disconnect"
  | "Fault"
  | "Close";

export class IllegalTransitionError extends Error {
  constructor(
    public readonly state: ProjectionState,
    public readonly event: ProjectionEvent,
  ) {
    super(`illegal transition: ${state} cannot handle ${event}`);
    this.name = "IllegalTransitionError";
  }
}

export class ProjectionStateMachine {
  private current: ProjectionState = "Local";

  get state(): ProjectionState {
    return this.current;
  }

  /** Input may be forwarded to the source only while truly live. */
  canForwardInput(): boolean {
    return this.current === "RemoteActive";
  }

  mediaActive(): boolean {
    return (
      this.current === "RemoteActive" ||
      this.current === "HandoffPreparing" ||
      this.current === "HandoffCommitting"
    );
  }

  isTerminal(): boolean {
    return this.current === "Closed";
  }

  /** Apply an event; throws IllegalTransitionError and leaves state unchanged on illegal input. */
  on(event: ProjectionEvent): ProjectionState {
    // Close always wins, from any state.
    if (event === "Close") {
      this.current = "Closed";
      return this.current;
    }
    if (this.current === "Closed") {
      throw new IllegalTransitionError(this.current, event);
    }

    const next = transition(this.current, event);
    if (next === null) {
      throw new IllegalTransitionError(this.current, event);
    }
    this.current = next;
    return this.current;
  }
}

/** Pure transition table. Returns null for illegal (state, event) pairs. `Close` is handled by the machine. */
export function transition(
  state: ProjectionState,
  event: ProjectionEvent,
): ProjectionState | null {
  // Any active state faults on Disconnect/Fault (source stays locally recoverable).
  const faulted = (): ProjectionState | null =>
    event === "Disconnect" || event === "Fault" ? "Error" : null;

  switch (state) {
    case "Local":
      return event === "Select" ? "Selecting" : null;
    case "Selecting":
      if (event === "AuthorizeGranted") return "Authorizing";
      return faulted();
    case "Authorizing":
      if (event === "AuthorizeGranted") return "CaptureStarting";
      if (event === "AuthorizeDenied") return "Error";
      return faulted();
    case "CaptureStarting":
      if (event === "CaptureStarted") return "Negotiating";
      if (event === "CaptureFailed") return "Error";
      return faulted();
    case "Negotiating":
      if (event === "NegotiationComplete") return "WaitingForFirstFrame";
      if (event === "NegotiationFailed") return "Error";
      return faulted();
    case "WaitingForFirstFrame":
      if (event === "FirstFrame") return "RemoteActive";
      if (event === "FirstFrameTimeout") return "Error";
      return faulted();
    case "RemoteActive":
      if (event === "HandoffRequested") return "HandoffPreparing";
      if (event === "ReturnRequested") return "Returning";
      if (event === "Suspend") return "Suspended";
      return faulted();
    case "HandoffPreparing":
      if (event === "NegotiationComplete") return "HandoffCommitting";
      if (event === "HandoffAborted" || event === "NegotiationFailed") return "RemoteActive";
      return faulted();
    case "HandoffCommitting":
      if (event === "HandoffCommitted" || event === "HandoffAborted") return "RemoteActive";
      return faulted();
    case "Suspended":
      if (event === "Resume") return "RemoteActive";
      if (event === "ReturnRequested") return "Returning";
      return faulted();
    case "Returning":
      if (event === "ReturnComplete") return "Local";
      return faulted();
    case "Error":
      return null; // only Close (handled by the machine) leaves Error
    case "Closed":
      return null;
    default:
      return null;
  }
}
