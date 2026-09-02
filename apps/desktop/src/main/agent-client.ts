/**
 * Client for the Rust user-session agent's local IPC (Windows named pipe).
 *
 * The renderer never talks to this directly. Path is:
 *   renderer -> preload bridge -> main (this client) -> named pipe -> agent.
 *
 * Framing is newline-delimited JSON; the agent answers one response per request in
 * order, so a simple FIFO of pending resolvers is sufficient.
 */

import { connect, type Socket } from "node:net";
import { spawn, type ChildProcess } from "node:child_process";
import { readFileSync } from "node:fs";
import type { IpcRequest, IpcResponse } from "../shared/protocol.js";
import { PROTOCOL_VERSION } from "../shared/protocol.js";

interface Endpoint {
  pipe_name: string;
  token: string;
  protocol_version: number;
  pid: number;
}

export class AgentClient {
  private socket: Socket | null = null;
  private buffer = "";
  private readonly pending: Array<(r: IpcResponse) => void> = [];

  private constructor(
    private readonly endpoint: Endpoint,
    private readonly child: ChildProcess | null,
  ) {}

  /**
   * Spawn the agent binary in `serve` mode, read the handshake it prints (the path to
   * its endpoint JSON), and connect + authenticate.
   */
  static async spawnAndConnect(agentBinaryPath: string): Promise<AgentClient> {
    const child = spawn(agentBinaryPath, ["serve"], { stdio: ["ignore", "pipe", "inherit"] });
    const handshakePath = await firstStdoutLine(child);
    const endpoint = JSON.parse(readFileSync(handshakePath, "utf8")) as Endpoint;
    const client = new AgentClient(endpoint, child);
    await client.connect();
    await client.hello();
    return client;
  }

  private connect(): Promise<void> {
    return new Promise((resolve, reject) => {
      const sock = connect(this.endpoint.pipe_name);
      sock.setEncoding("utf8");
      sock.on("connect", () => resolve());
      sock.on("error", reject);
      sock.on("data", (chunk: string) => this.onData(chunk));
      sock.on("close", () => this.failAllPending("agent connection closed"));
      this.socket = sock;
    });
  }

  private onData(chunk: string): void {
    this.buffer += chunk;
    let idx: number;
    while ((idx = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, idx).trim();
      this.buffer = this.buffer.slice(idx + 1);
      if (!line) continue;
      const resolve = this.pending.shift();
      if (resolve) resolve(JSON.parse(line) as IpcResponse);
    }
  }

  private failAllPending(reason: string): void {
    while (this.pending.length > 0) {
      const resolve = this.pending.shift();
      if (resolve) resolve({ type: "Error", code: "disconnected", message: reason });
    }
  }

  request(req: IpcRequest): Promise<IpcResponse> {
    return new Promise((resolve, reject) => {
      if (!this.socket) {
        reject(new Error("agent not connected"));
        return;
      }
      this.pending.push(resolve);
      this.socket.write(JSON.stringify(req) + "\n");
    });
  }

  private async hello(): Promise<void> {
    const res = await this.request({
      type: "Hello",
      token: this.endpoint.token,
      protocol_version: PROTOCOL_VERSION,
    });
    if (res.type !== "HelloOk") {
      throw new Error(`agent handshake failed: ${JSON.stringify(res)}`);
    }
    if (res.protocol_version !== PROTOCOL_VERSION) {
      throw new Error(`agent protocol v${res.protocol_version} != app v${PROTOCOL_VERSION}`);
    }
  }

  /** Best-effort release of any input the agent still holds for this session. */
  async releaseAllInput(): Promise<void> {
    try {
      await this.request({ type: "ReleaseAllInput" });
    } catch {
      // ignore — we are likely tearing down
    }
  }

  dispose(): void {
    this.socket?.destroy();
    this.child?.kill();
  }
}

function firstStdoutLine(child: ChildProcess): Promise<string> {
  return new Promise((resolve, reject) => {
    let acc = "";
    const to = setTimeout(() => reject(new Error("timed out waiting for agent handshake")), 10_000);
    child.stdout?.setEncoding("utf8");
    child.stdout?.on("data", (chunk: string) => {
      acc += chunk;
      const nl = acc.indexOf("\n");
      if (nl >= 0) {
        clearTimeout(to);
        resolve(acc.slice(0, nl).trim());
      }
    });
    child.on("error", (e) => {
      clearTimeout(to);
      reject(e);
    });
    child.on("exit", (code) => {
      clearTimeout(to);
      reject(new Error(`agent exited early with code ${code}`));
    });
  });
}
