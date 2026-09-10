// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Integration test that simulates the Temps host protocol against
 * the compiled plugin binary.
 *
 * Runs with Bun -- uses Bun-native APIs exclusively:
 * - `fetch({ unix })` for HTTP over Unix socket
 * - `Bun.connect({ unix })` for raw TCP WebSocket upgrade
 * - `Bun.spawn` for process management
 *
 * Tests the same contract that the Rust host expects:
 * 1. Spawn binary with CLI args
 * 2. Read handshake: manifest (JSON line on stdout)
 * 3. Read handshake: ready (JSON line on stdout)
 * 4. HTTP health check on Unix socket
 * 5. WebSocket channel connection on /_temps/channel
 * 6. HTTP API requests with auth headers
 * 7. Embedded UI asset serving (/ui/)
 * 8. Event delivery (POST /_events)
 * 9. Graceful shutdown (SIGTERM)
 */

import { spawn, type Subprocess } from "bun";
import { existsSync, mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { createHash } from "node:crypto";

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

const BINARY = join(import.meta.dirname, "..", "dist", "hello-node");
const SOCKET_PATH = "/tmp/tp-test-node-plugin/hello-node.sock";
const DATA_DIR = "/tmp/tp-test-node-plugin-data";
const AUTH_SECRET = "test-auth-secret-12345";

let child: Subprocess | undefined;
let passed = 0;
let failed = 0;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function assert(condition: boolean, message: string): void {
  if (condition) {
    console.log(`  PASS: ${message}`);
    passed++;
  } else {
    console.error(`  FAIL: ${message}`);
    failed++;
  }
}

function assertEqual<T>(actual: T, expected: T, message: string): void {
  if (actual === expected) {
    console.log(`  PASS: ${message}`);
    passed++;
  } else {
    console.error(
      `  FAIL: ${message} (expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)})`
    );
    failed++;
  }
}

/**
 * HTTP request over Unix socket using Bun's native fetch({ unix }).
 */
async function httpRequest(
  socketPath: string,
  path: string,
  options?: {
    method?: string;
    headers?: Record<string, string>;
    body?: string;
    redirect?: RequestRedirect;
  }
): Promise<{ status: number; headers: Headers; body: string }> {
  const res = await fetch(`http://localhost${path}`, {
    // @ts-expect-error -- Bun-specific: fetch over Unix socket
    unix: socketPath,
    method: options?.method ?? "GET",
    headers: options?.headers,
    body: options?.body,
    redirect: options?.redirect ?? "manual",
  });
  const body = await res.text();
  return { status: res.status, headers: res.headers, body };
}

/**
 * Raw WebSocket upgrade over Unix socket using Bun.connect.
 *
 * Returns a minimal { send, close, onMessage, onClose } interface.
 * Uses raw TCP to perform the HTTP upgrade handshake, then speaks the
 * WebSocket wire protocol at a minimal level (text frames only).
 */
async function connectWebSocket(
  socketPath: string,
  path: string
): Promise<{
  send: (data: string) => void;
  close: () => void;
  onMessage: (fn: (data: string) => void) => void;
  onClose: (fn: () => void) => void;
  connected: boolean;
}> {
  const wsKey = Buffer.from(
    createHash("sha256").update(String(Date.now())).digest()
  )
    .subarray(0, 16)
    .toString("base64");

  const messageListeners: Array<(data: string) => void> = [];
  const closeListeners: Array<() => void> = [];
  let connected = false;
  let handshakeComplete = false;
  let headerBuffer = "";

  // Buffer for incoming WebSocket frames after handshake
  let frameBuffer = Buffer.alloc(0);

  const result = {
    send: (_data: string) => {},
    close: () => {},
    onMessage: (fn: (data: string) => void) => {
      messageListeners.push(fn);
    },
    onClose: (fn: () => void) => {
      closeListeners.push(fn);
    },
    get connected() {
      return connected;
    },
  };

  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("WS connect timeout")), 5000);

    const socket = Bun.connect({
      unix: socketPath,
      socket: {
        open(sock) {
          // Send HTTP upgrade request
          const upgradeReq = [
            `GET ${path} HTTP/1.1`,
            "Host: localhost",
            "Connection: Upgrade",
            "Upgrade: websocket",
            "Sec-WebSocket-Version: 13",
            `Sec-WebSocket-Key: ${wsKey}`,
            "",
            "",
          ].join("\r\n");
          sock.write(upgradeReq);
        },
        data(sock, data) {
          if (!handshakeComplete) {
            // Accumulate the HTTP response header
            headerBuffer += data.toString();
            const headerEnd = headerBuffer.indexOf("\r\n\r\n");
            if (headerEnd === -1) return;

            const headerSection = headerBuffer.substring(0, headerEnd);
            const statusLine = headerSection.split("\r\n")[0] ?? "";

            if (!statusLine.includes("101")) {
              clearTimeout(timeout);
              reject(new Error(`WebSocket upgrade failed: ${statusLine}`));
              return;
            }

            handshakeComplete = true;
            connected = true;

            // Keep any remaining data after headers as frame data
            const remaining = headerBuffer.substring(headerEnd + 4);
            if (remaining.length > 0) {
              frameBuffer = Buffer.from(remaining, "binary");
            }

            // Wire up send/close
            result.send = (msg: string) => {
              // Build a masked text frame (client must mask per RFC 6455)
              const payload = Buffer.from(msg, "utf-8");
              const mask = Buffer.alloc(4);
              crypto.getRandomValues(mask);

              let header: Buffer;
              if (payload.length < 126) {
                header = Buffer.alloc(2);
                header[0] = 0x81; // FIN + text opcode
                header[1] = 0x80 | payload.length; // MASK bit + length
              } else if (payload.length < 65536) {
                header = Buffer.alloc(4);
                header[0] = 0x81;
                header[1] = 0x80 | 126;
                header.writeUInt16BE(payload.length, 2);
              } else {
                header = Buffer.alloc(10);
                header[0] = 0x81;
                header[1] = 0x80 | 127;
                header.writeBigUInt64BE(BigInt(payload.length), 2);
              }

              // Apply mask
              const masked = Buffer.alloc(payload.length);
              for (let i = 0; i < payload.length; i++) {
                masked[i] = payload[i]! ^ mask[i % 4]!;
              }

              sock.write(Buffer.concat([header, mask, masked]));
            };

            result.close = () => {
              // Send close frame (masked)
              const mask = Buffer.alloc(4);
              crypto.getRandomValues(mask);
              const closeFrame = Buffer.alloc(6);
              closeFrame[0] = 0x88; // FIN + close opcode
              closeFrame[1] = 0x80 | 2; // MASK + 2 byte payload
              closeFrame[2] = mask[0]!;
              closeFrame[3] = mask[1]!;
              // payload: status code 1000 (normal), masked
              closeFrame[4] = (0x03 >> 8) ^ mask[0]!;
              closeFrame[5] = (0xe8 & 0xff) ^ mask[1]!;
              sock.write(closeFrame);
              setTimeout(() => sock.end(), 100);
            };

            clearTimeout(timeout);
            resolve(result);
            return;
          }

          // Parse WebSocket frames (unmasked from server per RFC 6455)
          frameBuffer = Buffer.concat([
            frameBuffer,
            typeof data === "string" ? Buffer.from(data, "binary") : Buffer.from(data),
          ]);

          while (frameBuffer.length >= 2) {
            const opcode = frameBuffer[0]! & 0x0f;
            const masked = (frameBuffer[1]! & 0x80) !== 0;
            let payloadLen = frameBuffer[1]! & 0x7f;
            let offset = 2;

            if (payloadLen === 126) {
              if (frameBuffer.length < 4) return;
              payloadLen = frameBuffer.readUInt16BE(2);
              offset = 4;
            } else if (payloadLen === 127) {
              if (frameBuffer.length < 10) return;
              payloadLen = Number(frameBuffer.readBigUInt64BE(2));
              offset = 10;
            }

            if (masked) offset += 4;
            if (frameBuffer.length < offset + payloadLen) return;

            const payload = frameBuffer.subarray(offset, offset + payloadLen);
            frameBuffer = frameBuffer.subarray(offset + payloadLen);

            if (opcode === 0x01) {
              // Text frame
              for (const fn of messageListeners) fn(payload.toString("utf-8"));
            } else if (opcode === 0x08) {
              // Close frame
              connected = false;
              for (const fn of closeListeners) fn();
              sock.end();
              return;
            } else if (opcode === 0x09) {
              // Ping -- respond with pong (masked)
              const mask = Buffer.alloc(4);
              crypto.getRandomValues(mask);
              const pong = Buffer.alloc(2 + 4 + payload.length);
              pong[0] = 0x8a; // FIN + pong
              pong[1] = 0x80 | payload.length;
              mask.copy(pong, 2);
              for (let i = 0; i < payload.length; i++) {
                pong[6 + i] = payload[i]! ^ mask[i % 4]!;
              }
              sock.write(pong);
            }
          }
        },
        close() {
          connected = false;
          for (const fn of closeListeners) fn();
        },
        error(_sock, err) {
          if (!handshakeComplete) {
            clearTimeout(timeout);
            reject(err);
          }
        },
      },
    });

    // Ensure the promise from Bun.connect itself is handled
    socket.catch((err: Error) => {
      clearTimeout(timeout);
      reject(err);
    });
  });
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

// ---------------------------------------------------------------------------
// Cleanup
// ---------------------------------------------------------------------------

function cleanup(): void {
  if (child) {
    child.kill("SIGKILL");
  }
  try {
    if (existsSync(SOCKET_PATH)) rmSync(SOCKET_PATH);
    if (existsSync(DATA_DIR)) rmSync(DATA_DIR, { recursive: true });
  } catch {
    // ignore
  }
}

// ---------------------------------------------------------------------------
// Main test sequence
// ---------------------------------------------------------------------------

async function main(): Promise<void> {
  cleanup();
  mkdirSync("/tmp/tp-test-node-plugin", { recursive: true });

  if (!existsSync(BINARY)) {
    console.error(`Binary not found at ${BINARY}. Run 'bun run build' first.`);
    process.exit(1);
  }

  console.log("\n=== Integration Test: Node.js Plugin Binary ===\n");

  // -----------------------------------------------------------------------
  // 1. Spawn the binary
  // -----------------------------------------------------------------------
  console.log("1. Spawning plugin binary...");

  child = spawn({
    cmd: [
      BINARY,
      "--socket-path", SOCKET_PATH,
      "--auth-secret", AUTH_SECRET,
      "--data-dir", DATA_DIR,
    ],
    stdout: "pipe",
    stderr: "pipe",
  });

  // Capture stderr for debugging
  const stderrLines: string[] = [];
  const stderrReader = child.stderr.getReader();
  const decoder = new TextDecoder();
  // Read stderr in the background
  (async () => {
    try {
      while (true) {
        const { done, value } = await stderrReader.read();
        if (done) break;
        const text = decoder.decode(value).trim();
        if (text) {
          for (const line of text.split("\n")) {
            stderrLines.push(line.trim());
          }
        }
      }
    } catch {
      // Process exited
    }
  })();

  // -----------------------------------------------------------------------
  // 2. Read handshake phase 1: manifest
  // -----------------------------------------------------------------------
  console.log("\n2. Reading handshake: manifest...");

  let stdoutBuffer = "";

  // Read stdout line-by-line using the async iterator
  const readLine = async (): Promise<string> => {
    // Check buffer first
    const nlIdx = stdoutBuffer.indexOf("\n");
    if (nlIdx !== -1) {
      const line = stdoutBuffer.substring(0, nlIdx);
      stdoutBuffer = stdoutBuffer.substring(nlIdx + 1);
      return line;
    }

    // Read chunks until we find a newline
    return new Promise<string>((resolve, reject) => {
      const timeout = setTimeout(
        () => reject(new Error("Handshake timeout (no stdout line within 10s)")),
        10_000
      );

      const reader = child!.stdout.getReader();
      const pump = async () => {
        try {
          while (true) {
            const { done, value } = await reader.read();
            if (done) {
              clearTimeout(timeout);
              reader.releaseLock();
              reject(new Error("stdout closed before line received"));
              return;
            }
            stdoutBuffer += decoder.decode(value);
            const idx = stdoutBuffer.indexOf("\n");
            if (idx !== -1) {
              const line = stdoutBuffer.substring(0, idx);
              stdoutBuffer = stdoutBuffer.substring(idx + 1);
              clearTimeout(timeout);
              reader.releaseLock();
              resolve(line);
              return;
            }
          }
        } catch (err) {
          clearTimeout(timeout);
          reader.releaseLock();
          reject(err);
        }
      };
      pump();
    });
  };

  const manifestLine = await readLine();
  const manifestMsg = JSON.parse(manifestLine);

  assertEqual(manifestMsg.type, "manifest", "Manifest message type");
  assertEqual(manifestMsg.name, "hello-node", "Plugin name");
  assertEqual(manifestMsg.version, "0.1.0", "Plugin version");
  assertEqual(manifestMsg.display_name, "Hello Node", "Display name");
  assert(Array.isArray(manifestMsg.nav), "Nav entries is array");
  assert(manifestMsg.nav.length >= 2, "Has at least 2 nav entries");
  assertEqual(manifestMsg.requires_db, false, "Does not require DB");
  assertEqual(manifestMsg.health_path, "/health", "Health path");
  assert(
    manifestMsg.events.includes("deployment.succeeded"),
    "Subscribes to deployment.succeeded"
  );
  assert(
    manifestMsg.events.includes("deployment.failed"),
    "Subscribes to deployment.failed"
  );

  // -----------------------------------------------------------------------
  // 3. Read handshake phase 2: ready
  // -----------------------------------------------------------------------
  console.log("\n3. Reading handshake: ready...");

  const readyLine = await readLine();
  const readyMsg = JSON.parse(readyLine);

  assertEqual(readyMsg.type, "ready", "Ready message type");
  assertEqual(readyMsg.ready, true, "Plugin is ready");
  assertEqual(readyMsg.has_ui, true, "Plugin has UI");

  // Wait a moment for the Unix socket to be ready
  await sleep(500);

  // -----------------------------------------------------------------------
  // 4. Health check on Unix socket
  // -----------------------------------------------------------------------
  console.log("\n4. Health check...");

  const healthRes = await httpRequest(SOCKET_PATH, "/health");
  assertEqual(healthRes.status, 200, "Health returns 200");
  const healthBody = JSON.parse(healthRes.body);
  assertEqual(healthBody.status, "ok", "Health status is ok");
  assertEqual(healthBody.plugin, "hello-node", "Health returns plugin name");

  // -----------------------------------------------------------------------
  // 5. WebSocket channel connection
  // -----------------------------------------------------------------------
  console.log("\n5. WebSocket channel connection...");

  const ws = await connectWebSocket(SOCKET_PATH, "/_temps/channel");
  assert(ws.connected, "WebSocket connected");

  // Wait for the plugin to finish initializing after channel connects
  await sleep(1000);

  // -----------------------------------------------------------------------
  // 6. HTTP API requests with auth headers
  // -----------------------------------------------------------------------
  console.log("\n6. API requests with auth headers...");

  const helloRes = await httpRequest(SOCKET_PATH, "/hello", {
    headers: {
      "x-temps-user-id": "42",
      "x-temps-user-email": "test@example.com",
      "x-temps-user-role": "admin",
      "x-temps-request-id": "req-001",
    },
  });
  assertEqual(helloRes.status, 200, "Hello returns 200");
  const helloBody = JSON.parse(helloRes.body);
  assertEqual(helloBody.message, "Hello test@example.com!", "Hello includes user email");
  assertEqual(helloBody.plugin, "hello-node", "Hello includes plugin name");
  assert(helloBody.dataDir.includes("tp-test-node-plugin-data"), "Hello includes data dir");

  // Test 404 for unknown route
  const notFoundRes = await httpRequest(SOCKET_PATH, "/unknown-route");
  assertEqual(notFoundRes.status, 404, "Unknown route returns 404");

  // -----------------------------------------------------------------------
  // 7. Embedded UI asset serving
  // -----------------------------------------------------------------------
  console.log("\n7. Embedded UI serving...");

  // /ui should redirect to /ui/
  const uiRedirectRes = await httpRequest(SOCKET_PATH, "/ui");
  assertEqual(uiRedirectRes.status, 302, "GET /ui redirects");
  assertEqual(uiRedirectRes.headers.get("location"), "/ui/", "Redirects to /ui/");

  // /ui/ should serve index.html
  const uiIndexRes = await httpRequest(SOCKET_PATH, "/ui/");
  assertEqual(uiIndexRes.status, 200, "GET /ui/ returns 200");
  assert(
    uiIndexRes.headers.get("content-type")?.includes("text/html") ?? false,
    "Index has text/html content type"
  );
  assert(uiIndexRes.body.includes("<!DOCTYPE html>"), "Index contains HTML");
  assert(uiIndexRes.body.includes("root"), "Index contains root element");
  assertEqual(
    uiIndexRes.headers.get("cache-control"),
    "no-cache, no-store, must-revalidate",
    "Index has no-cache headers"
  );

  // JS asset should be served with immutable cache
  // Find the actual JS filename from the HTML
  const jsMatch = uiIndexRes.body.match(/assets\/(index-[a-zA-Z0-9]+\.js)/);
  if (jsMatch) {
    const jsRes = await httpRequest(SOCKET_PATH, `/ui/assets/${jsMatch[1]}`);
    assertEqual(jsRes.status, 200, `JS asset ${jsMatch[1]} returns 200`);
    assert(
      jsRes.headers.get("content-type")?.includes("javascript") ?? false,
      "JS asset has correct content type"
    );
    assertEqual(
      jsRes.headers.get("cache-control"),
      "public, max-age=31536000, immutable",
      "JS asset has immutable cache"
    );
    assert(jsRes.body.length > 1000, "JS asset has substantial content");
  } else {
    console.error("  FAIL: Could not find JS asset reference in index.html");
    failed++;
  }

  // SPA fallback: non-existent path without extension should serve index.html
  const spaRes = await httpRequest(SOCKET_PATH, "/ui/some/nested/route");
  assertEqual(spaRes.status, 200, "SPA fallback returns 200");
  assert(spaRes.body.includes("<!DOCTYPE html>"), "SPA fallback serves index.html");

  // Non-existent asset file should 404
  const missing404 = await httpRequest(SOCKET_PATH, "/ui/assets/nonexistent.js");
  assertEqual(missing404.status, 404, "Missing asset returns 404");

  // -----------------------------------------------------------------------
  // 8. Event delivery
  // -----------------------------------------------------------------------
  console.log("\n8. Event delivery...");

  const eventRes = await httpRequest(SOCKET_PATH, "/_events", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      id: "evt-001",
      event_type: "deployment.succeeded",
      timestamp: new Date().toISOString(),
      project_id: 7,
      data: { deployment_id: 42, url: "https://app.example.com" },
    }),
  });
  assertEqual(eventRes.status, 200, "Event delivery returns 200");
  const eventBody = JSON.parse(eventRes.body);
  assertEqual(eventBody.ok, true, "Event delivery returns ok");

  // Bad event payload
  const badEventRes = await httpRequest(SOCKET_PATH, "/_events", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: "not json",
  });
  assertEqual(badEventRes.status, 400, "Bad event returns 400");

  // -----------------------------------------------------------------------
  // 9. Graceful shutdown
  // -----------------------------------------------------------------------
  console.log("\n9. Graceful shutdown...");

  // Close the WebSocket before sending SIGTERM
  ws.close();
  await sleep(200);

  child!.kill("SIGTERM");

  // Wait for exit
  const exitCode = await Promise.race([
    child!.exited,
    sleep(5000).then(() => -1),
  ]);

  assert(exitCode !== -1, "Plugin exited within 5 seconds");
  // Check stderr for shutdown message
  await sleep(200); // Let stderr reader finish
  const hasShutdownLog = stderrLines.some((l) => l.includes("Shutting down"));
  assert(hasShutdownLog, "Shutdown message logged to stderr");

  // Socket should be cleaned up
  await sleep(200);
  assert(!existsSync(SOCKET_PATH), "Socket file cleaned up");

  // -----------------------------------------------------------------------
  // Results
  // -----------------------------------------------------------------------
  console.log("\n=== Results ===");
  console.log(`  Passed: ${passed}`);
  console.log(`  Failed: ${failed}`);
  console.log(`  Total:  ${passed + failed}`);

  cleanup();

  if (failed > 0) {
    console.error("\nSome tests failed!");
    process.exit(1);
  } else {
    console.log("\nAll tests passed!");
    process.exit(0);
  }
}

main().catch((err) => {
  console.error("\nFatal error:", err);
  cleanup();
  process.exit(1);
});
