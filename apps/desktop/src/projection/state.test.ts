import { describe, it, expect } from "vitest";
import { ProjectionStateMachine, IllegalTransitionError, type ProjectionEvent } from "./state";

function drive(events: ProjectionEvent[]): ProjectionStateMachine {
  const m = new ProjectionStateMachine();
  for (const e of events) m.on(e);
  return m;
}

const TO_LIVE: ProjectionEvent[] = [
  "Select",
  "AuthorizeGranted",
  "AuthorizeGranted",
  "CaptureStarted",
  "NegotiationComplete",
  "FirstFrame",
];

describe("ProjectionStateMachine (parity with Rust core)", () => {
  it("reaches RemoteActive on the happy path and allows input", () => {
    const m = drive(TO_LIVE);
    expect(m.state).toBe("RemoteActive");
    expect(m.canForwardInput()).toBe(true);
    expect(m.mediaActive()).toBe(true);
  });

  it("gates input forwarding to RemoteActive only", () => {
    const m = drive(TO_LIVE);
    expect(m.canForwardInput()).toBe(true);
    m.on("Suspend");
    expect(m.canForwardInput()).toBe(false);
  });

  it("rolls back to Error on first-frame timeout", () => {
    const m = drive(["Select", "AuthorizeGranted", "AuthorizeGranted", "CaptureStarted", "NegotiationComplete"]);
    expect(m.state).toBe("WaitingForFirstFrame");
    expect(m.on("FirstFrameTimeout")).toBe("Error");
    expect(m.canForwardInput()).toBe(false);
  });

  it("faults and stops input on disconnect from live", () => {
    const m = drive(TO_LIVE);
    expect(m.on("Disconnect")).toBe("Error");
    expect(m.canForwardInput()).toBe(false);
  });

  it("keeps the old projection live when a handoff fails", () => {
    const m = drive([...TO_LIVE, "HandoffRequested"]);
    expect(m.state).toBe("HandoffPreparing");
    expect(m.on("NegotiationFailed")).toBe("RemoteActive");
    expect(m.canForwardInput()).toBe(true);
  });

  it("completes a handoff back to RemoteActive", () => {
    const m = drive([...TO_LIVE, "HandoffRequested", "NegotiationComplete", "HandoffCommitted"]);
    expect(m.state).toBe("RemoteActive");
  });

  it("returns to Local via Returning", () => {
    const m = drive([...TO_LIVE, "ReturnRequested"]);
    expect(m.on("ReturnComplete")).toBe("Local");
  });

  it("allows Close from every state", () => {
    for (const setup of [[], ["Select"], TO_LIVE] as ProjectionEvent[][]) {
      const m = drive(setup);
      expect(m.on("Close")).toBe("Closed");
      expect(m.isTerminal()).toBe(true);
    }
  });

  it("rejects illegal transitions and leaves state unchanged", () => {
    const m = new ProjectionStateMachine();
    expect(() => m.on("FirstFrame")).toThrow(IllegalTransitionError);
    expect(m.state).toBe("Local");
  });

  it("only exits Error via Close", () => {
    const m = drive(["Select"]);
    m.on("Fault");
    expect(m.state).toBe("Error");
    expect(() => m.on("Resume")).toThrow();
    expect(m.on("Close")).toBe("Closed");
  });
});
