// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Plugin runtime -- handles the full lifecycle of a Temps external plugin.
 *
 * Mirrors: crates/temps-plugin-sdk/src/runtime.rs
 *
 * Built for Bun -- compiled to a single binary via `bun build --compile`.
 *
 * Lifecycle:
 * 1. Parse CLI arguments (--socket-path, --auth-secret, --data-dir, etc.)
 * 2. Emit manifest to stdout (handshake phase 1)
 * 3. Start Bun.serve on Unix domain socket
 * 4. Emit ready to stdout (handshake phase 2)
 * 5. Accept WebSocket connection from host on /_temps/channel
 * 6. Create PluginContext with TempsClient
 * 7. Call plugin.onStart()
 * 8. Serve plugin routes, health, events, and embedded UI
 * 9. Handle SIGTERM for graceful shutdown
 */

import { existsSync, mkdirSync, unlinkSync } from "node:fs";
import { dirname } from "node:path";
import type { TempsPlugin, PluginArgs, PluginEvent } from "./types.js";
import { TempsClient, type WsLike } from "./client.js";
import { PluginContext } from "./context.js";
import { emitManifest, emitReady, PLUGIN_CHANNEL_PATH, extractAuthContext } from "./protocol.js";
import { createEmbeddedUiHandler, type EmbeddedAssets } from "./ui.js";
import { ArgsError, InitializationError } from "./errors.js";

/**
 * Run a Temps plugin. This is the main entry point.
 *
 * @example
 * ```ts
 * import { runPlugin, createManifest } from "@temps-sdk/plugin";
 *
 * runPlugin({
 *   manifest: () => createManifest("my-plugin", "0.1.0")
 *     .displayName("My Plugin")
 *     .addNav("Dashboard", "layout-dashboard", "/dashboard")
 *     .build(),
 *
 *   handler: (ctx) => async (req, res) => {
 *     res.writeHead(200, { "Content-Type": "application/json" });
 *     res.end(JSON.stringify({ hello: "world" }));
 *   },
 * });
 * ```
 */
export async function runPlugin(plugin: TempsPlugin): Promise<void> {
  // 1. Parse CLI arguments
  const args = parseArgs(process.argv.slice(2));

  // Ensure data directory exists
  mkdirSync(args.dataDir, { recursive: true });

  const manifest = plugin.manifest();

  // 2. Emit manifest (handshake phase 1)
  emitManifest(manifest);

  // Ensure socket directory exists and clean up stale socket
  const socketDir = dirname(args.socketPath);
  mkdirSync(socketDir, { recursive: true });
  if (existsSync(args.socketPath)) {
    unlinkSync(args.socketPath);
  }

  // Channel connection promise -- resolved when the host connects
  let resolveChannel: ((client: TempsClient) => void) | undefined;
  const channelReady = new Promise<TempsClient>((resolve) => {
    resolveChannel = resolve;
  });

  // Track state
  let currentContext: PluginContext | undefined;
  let pluginReady = false;

  // Embedded UI
  const embeddedAssets = plugin.embeddedUiAssets?.();
  const hasUi = embeddedAssets !== undefined || plugin.uiDistPath?.() !== undefined;

  // Build embedded UI handler if available
  const embeddedUiHandler = embeddedAssets
    ? createEmbeddedUiHandler(embeddedAssets)
    : undefined;

  // Active plugin fetch handler -- set once plugin is initialized
  let pluginFetch: ((req: Request) => Response | Promise<Response>) | undefined;

  // Bun WebSocket adapter plumbing
  const wsMessageListeners = new Map<unknown, Set<(data: string) => void>>();
  const wsCloseListeners = new Map<unknown, Set<() => void>>();

  // 3. Start Bun.serve on Unix socket
  const server = Bun.serve({
    unix: args.socketPath,

    fetch(req: Request, server) {
      const url = new URL(req.url);

      // WebSocket upgrade for platform channel
      if (url.pathname === PLUGIN_CHANNEL_PATH) {
        if (server.upgrade(req)) {
          return undefined as unknown as Response;
        }
        return new Response("WebSocket upgrade failed", { status: 500 });
      }

      // Health endpoint -- always available
      if (url.pathname === manifest.health_path || url.pathname === "/health") {
        return Response.json({ status: "ok", plugin: manifest.name });
      }

      // Event delivery endpoint
      if (url.pathname === "/_events" && req.method === "POST") {
        return handleEventDelivery(req, plugin, currentContext);
      }

      // Not initialized yet
      if (!pluginReady || !pluginFetch) {
        return new Response("Plugin initializing", { status: 503 });
      }

      // Try embedded UI
      if (embeddedUiHandler) {
        const uiResponse = handleEmbeddedUi(url, embeddedAssets!);
        if (uiResponse) return uiResponse;
      }

      // Delegate to plugin
      return pluginFetch(req);
    },

    websocket: {
      open(ws) {
        wsMessageListeners.set(ws, new Set());
        wsCloseListeners.set(ws, new Set());

        const adapter = createWsAdapter(ws, wsMessageListeners, wsCloseListeners);
        const client = new TempsClient(adapter);
        resolveChannel?.(client);
      },
      message(ws, message) {
        const msgStr = typeof message === "string" ? message : message.toString();
        const listeners = wsMessageListeners.get(ws);
        if (listeners) {
          for (const fn of listeners) fn(msgStr);
        }
      },
      close(ws) {
        const listeners = wsCloseListeners.get(ws);
        if (listeners) {
          for (const fn of listeners) fn();
        }
        wsMessageListeners.delete(ws);
        wsCloseListeners.delete(ws);
      },
    },
  });

  // 4. Emit ready (handshake phase 2)
  emitReady(hasUi);

  // 5. Wait for the host to connect via WebSocket channel (with timeout)
  const channelTimeoutMs = 30_000;
  let client: TempsClient;
  try {
    client = await Promise.race([
      channelReady,
      new Promise<never>((_, reject) =>
        setTimeout(
          () => reject(new Error("Channel connection timeout")),
          channelTimeoutMs
        )
      ),
    ]);
  } catch {
    console.error(
      "[temps-plugin] Warning: platform channel not connected, running without platform data access"
    );
    client = null as unknown as TempsClient;
  }

  // 6. Create PluginContext
  const ctx = new PluginContext({
    pluginName: manifest.name,
    dataDir: args.dataDir,
    authSecret: args.authSecret,
    client,
  });
  currentContext = ctx;

  if (client) {
    client.onEvent((event: PluginEvent) => {
      plugin.onEvent?.(ctx, event);
    });
  }

  // 7. Call plugin.onStart()
  try {
    await plugin.onStart?.(ctx);
  } catch (err) {
    throw new InitializationError(
      manifest.name,
      err instanceof Error ? err.message : String(err)
    );
  }

  // 8. Mount the plugin handler
  const nodeHandler = await plugin.handler(ctx);

  // Wrap the Node-style (req, res) handler into a Bun fetch handler
  pluginFetch = (req: Request) => nodeRequestBridge(req, nodeHandler);
  pluginReady = true;

  // 9. Graceful shutdown
  const shutdown = async () => {
    console.error(`[temps-plugin] Shutting down ${manifest.name}...`);
    try {
      await plugin.onShutdown?.();
    } catch (err) {
      console.error("[temps-plugin] Error during shutdown:", err);
    }
    client?.close();
    server.stop();

    if (existsSync(args.socketPath)) {
      try { unlinkSync(args.socketPath); } catch { /* ignore */ }
    }
    process.exit(0);
  };

  process.on("SIGTERM", shutdown);
  process.on("SIGINT", shutdown);

  console.error(
    `[temps-plugin] ${manifest.display_name ?? manifest.name} v${manifest.version} running on ${args.socketPath}`
  );
}

// ---------------------------------------------------------------------------
// Embedded UI serving (pure Bun Response)
// ---------------------------------------------------------------------------

function handleEmbeddedUi(
  url: URL,
  assets: EmbeddedAssets
): Response | undefined {
  const pathname = url.pathname;

  // Redirect /ui to /ui/
  if (pathname === "/ui") {
    return new Response(null, {
      status: 302,
      headers: { Location: "/ui/" },
    });
  }

  if (!pathname.startsWith("/ui/")) {
    return undefined;
  }

  const relativePath = pathname.slice("/ui/".length) || "index.html";

  if (relativePath.includes("..")) {
    return new Response("Forbidden", { status: 403 });
  }

  // Exact match
  const file = assets.get(relativePath);
  if (file) {
    return new Response(new Uint8Array(file.content), {
      headers: {
        "Content-Type": file.contentType,
        "Content-Length": String(file.content.byteLength),
        "Cache-Control": file.immutable
          ? "public, max-age=31536000, immutable"
          : "no-cache, no-store, must-revalidate",
      },
    });
  }

  // SPA fallback for paths without file extensions
  const hasExt = (relativePath.split("/").pop() ?? "").includes(".");
  if (!hasExt) {
    const index = assets.get("index.html");
    if (index) {
      return new Response(new Uint8Array(index.content), {
        headers: {
          "Content-Type": index.contentType,
          "Content-Length": String(index.content.byteLength),
          "Cache-Control": "no-cache, no-store, must-revalidate",
        },
      });
    }
  }

  return new Response("Not Found", { status: 404 });
}

// ---------------------------------------------------------------------------
// Bun WebSocket -> WsLike adapter
// ---------------------------------------------------------------------------

function createWsAdapter(
  ws: { send: (msg: string | Buffer) => void; close: () => void },
  messageListeners: Map<unknown, Set<(data: string) => void>>,
  closeListeners: Map<unknown, Set<() => void>>
): WsLike {
  return {
    on(event: string, fn: (...args: unknown[]) => void) {
      if (event === "message") {
        messageListeners.get(ws)?.add(fn as (data: string) => void);
      } else if (event === "close") {
        closeListeners.get(ws)?.add(fn as () => void);
      }
    },
    send(data: string, cb?: (err?: Error) => void) {
      try {
        ws.send(data);
        cb?.();
      } catch (err) {
        cb?.(err as Error);
      }
    },
    close() {
      ws.close();
    },
  };
}

// ---------------------------------------------------------------------------
// Node (req, res) handler -> Bun fetch bridge
// ---------------------------------------------------------------------------

import type { IncomingMessage, ServerResponse } from "node:http";

async function nodeRequestBridge(
  req: Request,
  handler: (req: IncomingMessage, res: ServerResponse) => void | Promise<void>
): Promise<Response> {
  const url = new URL(req.url);

  // Read body upfront
  let bodyBuffer = Buffer.alloc(0);
  if (req.body) {
    bodyBuffer = Buffer.from(await req.arrayBuffer());
  }

  return new Promise<Response>((resolve) => {
    // Build a minimal IncomingMessage shim
    const fakeReq = Object.create(null) as IncomingMessage;
    fakeReq.url = url.pathname + url.search;
    fakeReq.method = req.method;

    const hdrs: Record<string, string> = {};
    req.headers.forEach((v, k) => { hdrs[k] = v; });
    fakeReq.headers = hdrs;

    // Body event emitter shim
    const dataFns: Array<(chunk: Buffer) => void> = [];
    const endFns: Array<() => void> = [];
    fakeReq.on = ((event: string, fn: (...a: unknown[]) => void) => {
      if (event === "data") dataFns.push(fn as (c: Buffer) => void);
      if (event === "end") endFns.push(fn as () => void);
      return fakeReq;
    }) as typeof fakeReq.on;

    // Response collector shim
    let statusCode = 200;
    const resHeaders: Record<string, string> = {};
    const chunks: Buffer[] = [];

    const fakeRes = Object.create(null) as ServerResponse;
    fakeRes.writeHead = ((code: number, h?: Record<string, string>) => {
      statusCode = code;
      if (h) Object.assign(resHeaders, h);
      return fakeRes;
    }) as typeof fakeRes.writeHead;

    fakeRes.end = ((data?: string | Buffer) => {
      if (data) chunks.push(typeof data === "string" ? Buffer.from(data) : data);
      const body = Buffer.concat(chunks);
      resolve(new Response(body.length > 0 ? body : null, {
        status: statusCode,
        headers: resHeaders,
      }));
    }) as typeof fakeRes.end;

    fakeRes.write = ((data: string | Buffer) => {
      chunks.push(typeof data === "string" ? Buffer.from(data) : data);
      return true;
    }) as typeof fakeRes.write;

    // Call the handler
    handler(fakeReq, fakeRes);

    // Deliver body
    queueMicrotask(() => {
      if (bodyBuffer.length > 0) {
        for (const fn of dataFns) fn(bodyBuffer);
      }
      for (const fn of endFns) fn();
    });
  });
}

// ---------------------------------------------------------------------------
// Event delivery handler (POST /_events)
// ---------------------------------------------------------------------------

async function handleEventDelivery(
  req: Request,
  plugin: TempsPlugin,
  ctx?: PluginContext
): Promise<Response> {
  try {
    const event = (await req.json()) as PluginEvent;
    if (ctx) {
      plugin.onEvent?.(ctx, event);
    }
    return Response.json({ ok: true });
  } catch (err) {
    return Response.json(
      {
        error: "Invalid event payload",
        detail: err instanceof Error ? err.message : String(err),
      },
      { status: 400 }
    );
  }
}

// ---------------------------------------------------------------------------
// CLI argument parsing
// ---------------------------------------------------------------------------

function parseArgs(argv: string[]): PluginArgs {
  const args: Partial<PluginArgs> = {};

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]!;
    const next = argv[i + 1];

    switch (arg) {
      case "--socket-path":
        args.socketPath = next;
        i++;
        break;
      case "--database-url":
        args.databaseUrl = next;
        i++;
        break;
      case "--auth-secret":
        args.authSecret = next;
        i++;
        break;
      case "--data-dir":
        args.dataDir = next;
        i++;
        break;
    }
  }

  if (!args.socketPath) {
    throw new ArgsError("--socket-path is required");
  }
  if (!args.authSecret) {
    throw new ArgsError("--auth-secret is required");
  }
  if (!args.dataDir) {
    throw new ArgsError("--data-dir is required");
  }

  return args as PluginArgs;
}
