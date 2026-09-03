/**
 * Ultidesk agent benchmark.
 *
 * Drives the real release binary over its real named-pipe IPC protocol:
 *   spawn `ultidesk-agent serve` -> read handshake -> Hello -> N x Ping / EnumerateWindows.
 *
 * Deliberately does NOT exercise InjectMouseMove / InjectKey: those call SendInput and
 * would move the operator's cursor and type into whatever window has focus.
 *
 * Usage: node scripts/bench-agent.mjs <path-to-agent.exe> <label> [pings] [enumerates]
 *
 * e.g. cargo build --release -p ultidesk-agent
 *      node scripts/bench-agent.mjs target/release/ultidesk-agent.exe "ARM64 native"
 */

import { spawn, execFileSync } from "node:child_process";
import { readFileSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import net from "node:net";

const [, , exePath, label, pingsArg, enumsArg] = process.argv;
const PINGS = Number(pingsArg ?? 3000);
const ENUMS = Number(enumsArg ?? 300);
const WARMUP = 300;

function percentiles(samplesMs) {
  const s = [...samplesMs].sort((a, b) => a - b);
  const at = (p) => s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))];
  const mean = s.reduce((a, b) => a + b, 0) / s.length;
  return { min: s[0], p50: at(50), p90: at(90), p99: at(99), max: s[s.length - 1], mean };
}

function fmt(name, p) {
  return (
    `  ${name.padEnd(22)} ` +
    `p50=${p.p50.toFixed(3).padStart(8)} ms  ` +
    `p90=${p.p90.toFixed(3).padStart(8)} ms  ` +
    `p99=${p.p99.toFixed(3).padStart(8)} ms  ` +
    `min=${p.min.toFixed(3).padStart(7)}  max=${p.max.toFixed(3).padStart(8)}  ` +
    `mean=${p.mean.toFixed(3)}`
  );
}

/** Line-delimited JSON client over the named pipe, strictly request/response. */
class PipeClient {
  constructor(socket) {
    this.socket = socket;
    this.pending = [];
    this.buf = "";
    socket.setNoDelay?.(true);
    socket.on("data", (chunk) => {
      this.buf += chunk.toString("utf8");
      let idx;
      while ((idx = this.buf.indexOf("\n")) >= 0) {
        const line = this.buf.slice(0, idx);
        this.buf = this.buf.slice(idx + 1);
        const resolve = this.pending.shift();
        if (resolve) resolve(line);
      }
    });
  }
  request(obj) {
    return new Promise((resolve) => {
      this.pending.push(resolve);
      this.socket.write(JSON.stringify(obj) + "\n");
    });
  }
  close() {
    this.socket.destroy();
  }
}

function connect(pipeName) {
  return new Promise((resolve, reject) => {
    const attempt = (left) => {
      const s = net.connect(pipeName);
      s.once("connect", () => resolve(s));
      s.once("error", (e) => {
        s.destroy();
        if (left <= 0) return reject(e);
        setTimeout(() => attempt(left - 1), 25);
      });
    };
    attempt(80);
  });
}

function workingSetKb(pid) {
  try {
    const out = execFileSync("tasklist", ["/FI", `PID eq ${pid}`, "/FO", "CSV", "/NH"], {
      encoding: "utf8",
    });
    const m = /"([\d.,\s]+) K"/.exec(out);
    return m ? Number(m[1].replace(/[^\d]/g, "")) : null;
  } catch {
    return null;
  }
}

async function main() {
  const devDir = mkdtempSync(join(tmpdir(), "ultidesk-bench-"));
  const child = spawn(exePath, ["serve"], {
    env: { ...process.env, ULTIDESK_DEV_DIR: devDir, ULTIDESK_LOG: "warn" },
    stdio: ["ignore", "pipe", "pipe"],
  });

  const handshakePath = await new Promise((resolve, reject) => {
    let out = "";
    const t = setTimeout(() => reject(new Error("timed out waiting for handshake path")), 15000);
    child.stdout.on("data", (d) => {
      out += d.toString("utf8");
      const line = out.split(/\r?\n/)[0];
      if (line && line.trim().length > 0) {
        clearTimeout(t);
        resolve(line.trim());
      }
    });
    child.once("exit", (c) => { clearTimeout(t); reject(new Error("agent exited early, code " + c)); });
  });

  const ep = JSON.parse(readFileSync(handshakePath, "utf8"));
  const socket = await connect(ep.pipe_name);
  const client = new PipeClient(socket);

  const hello = await client.request({
    type: "Hello",
    token: ep.token,
    protocol_version: ep.protocol_version,
  });
  if (!hello.includes("HelloOk")) throw new Error("handshake failed: " + hello);

  // Warm up: first calls pay page-in, allocator growth, and (on an emulated binary)
  // the translation layer's first-execution cost. Steady state is what we report.
  for (let i = 0; i < WARMUP; i++) await client.request({ type: "Ping" });

  const pingMs = [];
  for (let i = 0; i < PINGS; i++) {
    const t0 = process.hrtime.bigint();
    const r = await client.request({ type: "Ping" });
    const t1 = process.hrtime.bigint();
    if (!r.includes("Pong")) throw new Error("bad ping response: " + r);
    pingMs.push(Number(t1 - t0) / 1e6);
  }

  for (let i = 0; i < 20; i++) await client.request({ type: "EnumerateWindows" });
  const enumMs = [];
  let windowCount = 0;
  for (let i = 0; i < ENUMS; i++) {
    const t0 = process.hrtime.bigint();
    const r = await client.request({ type: "EnumerateWindows" });
    const t1 = process.hrtime.bigint();
    enumMs.push(Number(t0 !== t1 ? t1 - t0 : 0n) / 1e6);
    if (i === 0) { const parsed = JSON.parse(r); if (parsed.type !== "Windows") throw new Error("enumerate did not return Windows, got: " + parsed.type + " " + (parsed.code ?? "")); windowCount = (parsed.windows ?? []).length; }
  }

  const rssKb = workingSetKb(child.pid);

  console.log(`\n=== ${label} ===`);
  console.log(`  binary: ${exePath}`);
  console.log(`  agent pid ${child.pid}, working set ${rssKb ?? "?"} KB, enumerated ${windowCount} windows`);
  console.log(fmt(`Ping RTT (n=${PINGS})`, percentiles(pingMs)));
  console.log(fmt(`EnumerateWindows (n=${ENUMS})`, percentiles(enumMs)));

  client.close();
  child.kill();
}

main().catch((e) => {
  console.error("BENCH FAILED:", e.message);
  process.exit(1);
});
