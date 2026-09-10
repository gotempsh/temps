// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Dev server: spawns the compiled plugin binary, connects the
 * WebSocket channel (keeping it alive), and proxies HTTP on
 * localhost:9876 so you can open the UI in a browser.
 *
 * Usage: bun run scripts/dev-server.ts
 */

import { spawn } from "bun";
import { existsSync, mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { createHash } from "node:crypto";

const BINARY = join(import.meta.dirname, "..", "dist", "hello-node");
const SOCKET_PATH = "/tmp/tp-manual-test/hello-node.sock";
const DATA_DIR = "/tmp/tp-manual-test/data";
const AUTH_SECRET = "dev-secret";
const PROXY_PORT = 9876;

// Cleanup stale state
if (existsSync(SOCKET_PATH)) rmSync(SOCKET_PATH);
mkdirSync("/tmp/tp-manual-test", { recursive: true });
mkdirSync(DATA_DIR, { recursive: true });

if (!existsSync(BINARY)) {
  console.error(`Binary not found at ${BINARY}. Run 'bun run build' first.`);
  process.exit(1);
}

console.log("Starting plugin binary...");

const child = spawn({
  cmd: [BINARY, "--socket-path", SOCKET_PATH, "--auth-secret", AUTH_SECRET, "--data-dir", DATA_DIR],
  stdout: "pipe",
  stderr: "inherit",
});

// Read handshake lines from stdout
const decoder = new TextDecoder();
const reader = child.stdout.getReader();
let buf = "";

async function readLine(): Promise<string> {
  while (true) {
    const idx = buf.indexOf("\n");
    if (idx !== -1) {
      const line = buf.substring(0, idx);
      buf = buf.substring(idx + 1);
      return line;
    }
    const { done, value } = await reader.read();
    if (done) throw new Error("stdout closed");
    buf += decoder.decode(value);
  }
}

const manifestLine = await readLine();
console.log("Manifest:", JSON.parse(manifestLine).name);

const readyLine = await readLine();
console.log("Ready:", JSON.parse(readyLine).ready);

// Wait for socket to be ready
await Bun.sleep(300);

// Connect WebSocket channel via raw TCP
console.log("Connecting WebSocket channel...");

const wsKey = Buffer.from(createHash("sha256").update("dev-key").digest()).subarray(0, 16).toString("base64");

await new Promise<void>((resolve, reject) => {
  const timeout = setTimeout(() => reject(new Error("WS connect timeout")), 5000);
  Bun.connect({
    unix: SOCKET_PATH,
    socket: {
      open(sock) {
        const req = [
          "GET /_temps/channel HTTP/1.1",
          "Host: localhost",
          "Connection: Upgrade",
          "Upgrade: websocket",
          "Sec-WebSocket-Version: 13",
          `Sec-WebSocket-Key: ${wsKey}`,
          "",
          "",
        ].join("\r\n");
        sock.write(req);
      },
      data(_sock, data) {
        const text = data.toString();
        if (text.includes("101")) {
          clearTimeout(timeout);
          console.log("WebSocket channel connected");
          resolve();
        }
      },
      close() {},
      error(_sock, err) {
        clearTimeout(timeout);
        reject(err);
      },
    },
  }).catch(reject);
});

// Wait for plugin to fully initialize
await Bun.sleep(500);

// Start HTTP proxy
const proxy = Bun.serve({
  port: PROXY_PORT,
  async fetch(req) {
    const url = new URL(req.url);
    const target = `http://localhost${url.pathname}${url.search}`;
    try {
      const res = await fetch(target, {
        // @ts-expect-error -- Bun-specific
        unix: SOCKET_PATH,
        method: req.method,
        headers: req.headers,
        body: req.body,
        redirect: "manual",
      });
      return new Response(res.body, {
        status: res.status,
        statusText: res.statusText,
        headers: res.headers,
      });
    } catch (err) {
      return new Response(`Proxy error: ${err}`, { status: 502 });
    }
  },
});

console.log(`\nPlugin UI available at: http://127.0.0.1:${PROXY_PORT}/ui/`);
console.log(`Health endpoint: http://127.0.0.1:${PROXY_PORT}/health`);
console.log(`API endpoint: http://127.0.0.1:${PROXY_PORT}/hello`);
console.log("\nPress Ctrl+C to stop.\n");

process.on("SIGINT", () => {
  console.log("\nShutting down...");
  child.kill("SIGTERM");
  proxy.stop();
  process.exit(0);
});
