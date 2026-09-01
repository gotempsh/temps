// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { chmodSync, existsSync, mkdirSync, unlinkSync } from "node:fs";
import type { IncomingMessage, ServerResponse } from "node:http";
import { dirname } from "node:path";
import {
  attachVerifiedCaller,
  type AuthenticatedCaller,
  verifyProxyHeaders,
} from "./auth.js";
import { TempsClient, type WsLike } from "./client.js";
import { PluginContext } from "./context.js";
import {
  ArgsError,
  InitializationError,
  RequestBodyTooLargeError,
  ResponseBodyTooLargeError,
} from "./errors.js";
import {
  emitHello,
  emitReady,
  PLUGIN_CHANNEL_PATH,
  PLUGIN_EVENTS_PATH,
  readLaunchConfig,
} from "./protocol.js";
import {
  MAX_PLUGIN_REQUEST_BODY_BYTES,
  MAX_PLUGIN_RESPONSE_BODY_BYTES,
  type PluginArgs,
  type PluginEvent,
  type RequestHandler,
  type TempsPlugin,
} from "./types.js";
import { createEmbeddedUiHandler, createUiHandler } from "./ui.js";

const CHANNEL_TIMEOUT_MS = 30_000;

/** Run a TypeScript Temps plugin under Bun or as a Bun-compiled executable. */
export async function runPlugin(plugin: TempsPlugin): Promise<void> {
  const args = parsePluginArgs(process.argv.slice(2));
  const manifest = plugin.manifest();
  validateManifest(manifest.name, manifest.health_path);

  // Stage 1: identify the protocol and requested privileges before receiving secrets.
  emitHello(manifest);
  const launch = await readLaunchConfig(manifest);

  mkdirSync(args.dataDir, { recursive: true });
  mkdirSync(dirname(args.socketPath), { recursive: true });
  removeSocket(args.socketPath);

  let resolveChannel: ((client: TempsClient) => void) | undefined;
  let rejectChannel: ((error: Error) => void) | undefined;
  const channelReady = new Promise<TempsClient>((resolve, reject) => {
    resolveChannel = resolve;
    rejectChannel = reject;
  });

  const messageListeners = new Map<unknown, Set<(data: unknown) => void>>();
  const closeListeners = new Map<unknown, Set<() => void>>();
  const errorListeners = new Map<unknown, Set<(error: unknown) => void>>();
  let channelClaimed = false;
  let currentContext: PluginContext | undefined;
  let pluginFetch:
    | ((request: Request, caller?: AuthenticatedCaller) => Promise<Response>)
    | undefined;
  let shuttingDown = false;

  const embeddedAssets = plugin.embeddedUiAssets?.();
  const uiDistPath = plugin.uiDistPath?.();
  const embeddedUiHandler = embeddedAssets
    ? createEmbeddedUiHandler(embeddedAssets)
    : undefined;
  const filesystemUiHandler = uiDistPath ? createUiHandler(uiDistPath) : undefined;
  const hasUi = embeddedUiHandler !== undefined || filesystemUiHandler !== undefined;

  const server = Bun.serve({
    unix: args.socketPath,

    async fetch(request, bunServer) {
      const url = new URL(request.url);

      if (url.pathname === PLUGIN_CHANNEL_PATH) {
        const verification = verifyProxyHeaders(request.headers, launch.auth_secret);
        if (!verification.verified) {
          log("warn", "Rejected unauthenticated platform channel attempt", {
            plugin: manifest.name,
          });
          return new Response("Unauthorized", { status: 401 });
        }
        if (channelClaimed) return new Response("Channel already connected", { status: 409 });

        channelClaimed = true;
        if (bunServer.upgrade(request)) return undefined;
        channelClaimed = false;
        return new Response("WebSocket upgrade failed", { status: 500 });
      }

      // Health remains available before channel initialization, matching the Rust SDK.
      if (url.pathname === manifest.health_path) {
        return Response.json({ status: "ok", plugin: manifest.name });
      }

      if (!pluginFetch || !currentContext) {
        return new Response("Plugin initializing", { status: 503 });
      }

      if (
        manifest.events.length > 0 &&
        url.pathname === PLUGIN_EVENTS_PATH &&
        request.method === "POST"
      ) {
        const verification = verifyProxyHeaders(request.headers, launch.auth_secret);
        if (!verification.verified) return authenticationRequired();
        return handleEventDelivery(request, plugin, currentContext);
      }

      const verification = verifyProxyHeaders(request.headers, launch.auth_secret);
      if (!verification.verified) return authenticationRequired();
      return pluginFetch(request, verification.caller);
    },

    websocket: {
      open(socket) {
        messageListeners.set(socket, new Set());
        closeListeners.set(socket, new Set());
        errorListeners.set(socket, new Set());
        resolveChannel?.(
          new TempsClient(
            createWsAdapter(socket, messageListeners, closeListeners, errorListeners)
          )
        );
        resolveChannel = undefined;
        rejectChannel = undefined;
      },
      message(socket, message) {
        for (const listener of messageListeners.get(socket) ?? []) listener(message);
      },
      close(socket) {
        for (const listener of closeListeners.get(socket) ?? []) listener();
        messageListeners.delete(socket);
        closeListeners.delete(socket);
        errorListeners.delete(socket);
      },
    },
  });

  try {
    chmodSync(args.socketPath, 0o600);

    // Stage 2: only report ready after the socket is bound.
    emitReady({ hasUi, openapi: plugin.openapiSchema?.() });

    const client = await withTimeout(
      channelReady,
      CHANNEL_TIMEOUT_MS,
      new InitializationError(
        manifest.name,
        `Platform channel connection timed out after ${CHANNEL_TIMEOUT_MS}ms`
      )
    );

    const context = new PluginContext({
      pluginName: manifest.name,
      dataDir: args.dataDir,
      databaseUrl: launch.database_url ?? undefined,
      hostDataDir: launch.host_data_dir ?? undefined,
      hostApiUrl: args.hostApiUrl,
      authSecret: launch.auth_secret,
      client,
    });
    currentContext = context;

    client.onEvent(async (event) => {
      try {
        await plugin.onEvent?.(context, event);
      } catch (error) {
        log("error", "Plugin event handler failed", {
          plugin: manifest.name,
          event_type: event.event_type,
          reason: errorMessage(error),
        });
      }
    });

    try {
      await plugin.onStart?.(context);
    } catch (error) {
      throw new InitializationError(manifest.name, errorMessage(error));
    }

    const handler = await plugin.handler(context);
    pluginFetch = (request, caller) =>
      nodeRequestBridge(request, handler, caller, manifest.name, [
        embeddedUiHandler,
        filesystemUiHandler,
      ]);

    const shutdown = async (signal: string) => {
      if (shuttingDown) return;
      shuttingDown = true;
      log("info", "Shutting down plugin", { plugin: manifest.name, signal });
      try {
        await plugin.onShutdown?.();
      } catch (error) {
        log("error", "Plugin shutdown hook failed", {
          plugin: manifest.name,
          reason: errorMessage(error),
        });
      }
      client.close();
      server.stop(true);
      removeSocket(args.socketPath);
    };

    process.once("SIGTERM", () => void shutdown("SIGTERM"));
    process.once("SIGINT", () => void shutdown("SIGINT"));

    log("info", "Plugin fully initialized", {
      plugin: manifest.name,
      version: manifest.version,
      socket_path: args.socketPath,
    });
  } catch (error) {
    rejectChannel?.(error instanceof Error ? error : new Error(String(error)));
    server.stop(true);
    removeSocket(args.socketPath);
    throw error;
  }
}

export function parsePluginArgs(argv: string[]): PluginArgs {
  const values = new Map<string, string>();
  const supported = new Set(["--socket-path", "--data-dir", "--host-api-url"]);

  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag || !supported.has(flag)) {
      throw new ArgsError(`unexpected argument: ${flag ?? "<missing>"}`);
    }
    if (!value || value.startsWith("--")) {
      throw new ArgsError(`${flag} requires a value`);
    }
    values.set(flag, value);
  }

  const socketPath = values.get("--socket-path");
  const dataDir = values.get("--data-dir");
  if (!socketPath) throw new ArgsError("--socket-path is required");
  if (!dataDir) throw new ArgsError("--data-dir is required");

  return {
    socketPath,
    dataDir,
    ...(values.has("--host-api-url")
      ? { hostApiUrl: values.get("--host-api-url") }
      : {}),
  };
}

function createWsAdapter(
  socket: { send(message: string | Buffer): number; close(): void },
  messageListeners: Map<unknown, Set<(data: unknown) => void>>,
  closeListeners: Map<unknown, Set<() => void>>,
  errorListeners: Map<unknown, Set<(error: unknown) => void>>
): WsLike {
  return {
    on(event: string, listener: (...args: unknown[]) => void) {
      if (event === "message") {
        messageListeners.get(socket)?.add(listener as (data: unknown) => void);
      }
      if (event === "close") {
        closeListeners.get(socket)?.add(listener as () => void);
      }
      if (event === "error") {
        errorListeners.get(socket)?.add(listener as (error: unknown) => void);
      }
    },
    send(data, callback) {
      try {
        socket.send(data);
        callback?.();
      } catch (error) {
        callback?.(error instanceof Error ? error : new Error(String(error)));
      }
    },
    close() {
      socket.close();
    },
  } as WsLike;
}

async function nodeRequestBridge(
  request: Request,
  handler: RequestHandler,
  caller: AuthenticatedCaller | undefined,
  pluginName: string,
  uiHandlers: Array<ReturnType<typeof createUiHandler> | undefined>
): Promise<Response> {
  const url = new URL(request.url);
  let body: Buffer;
  try {
    body = await readRequestBody(request);
  } catch (error) {
    if (error instanceof RequestBodyTooLargeError) return payloadTooLarge(error);
    throw error;
  }

  return new Promise<Response>((resolve) => {
    const incoming = Object.create(null) as IncomingMessage;
    incoming.url = `${url.pathname}${url.search}`;
    incoming.method = request.method;
    const incomingHeaders: Record<string, string> = {};
    request.headers.forEach((value, name) => {
      incomingHeaders[name] = value;
    });
    incoming.headers = incomingHeaders;
    attachVerifiedCaller(incoming, caller);

    const dataListeners: Array<(chunk: Buffer) => void> = [];
    const endListeners: Array<() => void> = [];
    incoming.on = ((event: string, listener: (...args: unknown[]) => void) => {
      if (event === "data") dataListeners.push(listener as (chunk: Buffer) => void);
      if (event === "end") endListeners.push(listener as () => void);
      return incoming;
    }) as typeof incoming.on;

    let status = 200;
    let ended = false;
    const headers: Record<string, string> = {};
    const responseBody = new ResponseBodyAccumulator();
    const response = Object.create(null) as ServerResponse;

    const failOversizedResponse = (error: ResponseBodyTooLargeError) => {
      if (ended) return;
      ended = true;
      log("error", "Plugin response exceeded the runtime body limit", {
        plugin: pluginName,
        actual_bytes: error.actualBytes,
        maximum_bytes: error.maximumBytes,
      });
      resolve(responseTooLarge(error));
    };

    Object.defineProperty(response, "statusCode", {
      get: () => status,
      set: (value: number) => {
        status = value;
      },
    });
    response.setHeader = ((name: string, value: string | number | readonly string[]) => {
      headers[name] = Array.isArray(value) ? value.join(", ") : String(value);
      return response;
    }) as typeof response.setHeader;
    response.getHeader = ((name: string) => headers[name]) as typeof response.getHeader;
    response.writeHead = ((code: number, values?: Record<string, string>) => {
      status = code;
      if (values) Object.assign(headers, values);
      return response;
    }) as typeof response.writeHead;
    response.write = ((value: string | Buffer) => {
      if (ended) return false;
      try {
        responseBody.append(value);
        return true;
      } catch (error) {
        if (error instanceof ResponseBodyTooLargeError) {
          failOversizedResponse(error);
          return false;
        }
        throw error;
      }
    }) as typeof response.write;
    response.end = ((value?: string | Buffer) => {
      if (ended) return response;
      try {
        if (value) responseBody.append(value);
      } catch (error) {
        if (error instanceof ResponseBodyTooLargeError) {
          failOversizedResponse(error);
          return response;
        }
        throw error;
      }
      ended = true;
      const body = responseBody.toBuffer();
      resolve(
        new Response(body.length === 0 ? null : (body as unknown as BodyInit), {
          status,
          headers,
        })
      );
      return response;
    }) as typeof response.end;

    const uiHandled = uiHandlers.some((uiHandler) => uiHandler?.(incoming, response));
    if (!uiHandled) {
      void Promise.resolve(handler(incoming, response)).catch((error) => {
        if (ended) return;
        log("error", "Plugin request handler failed", {
          plugin: pluginName,
          reason: errorMessage(error),
        });
        status = 500;
        headers["Content-Type"] = "application/problem+json";
        response.end(
          JSON.stringify({
            type: "https://temps.sh/problems/plugin-handler-failed",
            title: "Plugin Handler Failed",
            status: 500,
            detail: "The plugin could not complete this request.",
          })
        );
      });
    }

    queueMicrotask(() => {
      if (body.length > 0) for (const listener of dataListeners) listener(body);
      for (const listener of endListeners) listener();
    });
  });
}

async function handleEventDelivery(
  request: Request,
  plugin: TempsPlugin,
  context: PluginContext
): Promise<Response> {
  let event: PluginEvent;
  try {
    const body = await readRequestBody(request);
    event = JSON.parse(body.toString("utf8")) as PluginEvent;
    if (
      typeof event.id !== "string" ||
      typeof event.event_type !== "string" ||
      typeof event.timestamp !== "string" ||
      typeof event.data !== "object" ||
      event.data === null
    ) {
      throw new Error("event is missing required fields");
    }
  } catch (error) {
    if (error instanceof RequestBodyTooLargeError) return payloadTooLarge(error);
    return Response.json(
      {
        type: "https://temps.sh/problems/plugin-event-invalid",
        title: "Invalid Plugin Event",
        status: 400,
        detail: "The event payload is not valid.",
      },
      { status: 400 }
    );
  }

  try {
    await plugin.onEvent?.(context, event);
    return new Response(null, { status: 200 });
  } catch (error) {
    log("error", "Plugin event handler failed", {
      plugin: context.pluginName,
      event_type: event.event_type,
      reason: errorMessage(error),
    });
    return Response.json(
      {
        type: "https://temps.sh/problems/plugin-event-handler-failed",
        title: "Plugin Event Handler Failed",
        status: 500,
        detail: "The plugin could not process this event.",
      },
      { status: 500 }
    );
  }
}

/** @internal Read a plugin request without allowing unbounded process memory growth. */
export async function readRequestBody(
  request: Request,
  maximumBytes = MAX_PLUGIN_REQUEST_BODY_BYTES
): Promise<Buffer> {
  const declaredLength = request.headers.get("content-length");
  if (declaredLength !== null) {
    const parsedLength = Number(declaredLength);
    if (Number.isFinite(parsedLength) && parsedLength > maximumBytes) {
      throw new RequestBodyTooLargeError(parsedLength, maximumBytes);
    }
  }

  if (!request.body) return Buffer.alloc(0);

  const reader = request.body.getReader();
  const chunks: Buffer[] = [];
  let totalBytes = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    totalBytes += value.byteLength;
    if (totalBytes > maximumBytes) {
      await reader.cancel("plugin request body limit exceeded");
      throw new RequestBodyTooLargeError(totalBytes, maximumBytes);
    }
    chunks.push(Buffer.from(value));
  }
  return Buffer.concat(chunks, totalBytes);
}

/** @internal Bounded response collector used by the Node-compatible bridge. */
export class ResponseBodyAccumulator {
  readonly #maximumBytes: number;
  readonly #chunks: Buffer[] = [];
  #totalBytes = 0;

  constructor(maximumBytes = MAX_PLUGIN_RESPONSE_BODY_BYTES) {
    this.#maximumBytes = maximumBytes;
  }

  append(value: string | Buffer): void {
    const chunk = typeof value === "string" ? Buffer.from(value) : value;
    const nextTotal = this.#totalBytes + chunk.byteLength;
    if (nextTotal > this.#maximumBytes) {
      throw new ResponseBodyTooLargeError(nextTotal, this.#maximumBytes);
    }
    this.#chunks.push(chunk);
    this.#totalBytes = nextTotal;
  }

  toBuffer(): Buffer {
    return Buffer.concat(this.#chunks, this.#totalBytes);
  }
}

function authenticationRequired(): Response {
  return Response.json(
    {
      type: "https://temps.sh/problems/plugin-authentication-required",
      title: "Authentication Required",
      status: 401,
      detail: "This plugin route requires a request asserted by Temps.",
    },
    { status: 401, headers: { "Content-Type": "application/problem+json" } }
  );
}

function payloadTooLarge(error: RequestBodyTooLargeError): Response {
  return Response.json(
    {
      type: "https://temps.sh/problems/plugin-request-body-too-large",
      title: "Plugin Request Body Too Large",
      status: 413,
      detail: error.message,
      maximum_bytes: error.maximumBytes,
    },
    { status: 413, headers: { "Content-Type": "application/problem+json" } }
  );
}

function responseTooLarge(error: ResponseBodyTooLargeError): Response {
  return Response.json(
    {
      type: "https://temps.sh/problems/plugin-response-body-too-large",
      title: "Plugin Response Body Too Large",
      status: 502,
      detail: "The plugin produced a response larger than the runtime limit.",
      maximum_bytes: error.maximumBytes,
    },
    { status: 502, headers: { "Content-Type": "application/problem+json" } }
  );
}

function validateManifest(name: string, healthPath: string): void {
  if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(name)) {
    throw new InitializationError(name, "Plugin name must be lowercase kebab-case");
  }
  if (!healthPath.startsWith("/") || healthPath.includes("..")) {
    throw new InitializationError(name, `Invalid health path: ${healthPath}`);
  }
}

function withTimeout<T>(promise: Promise<T>, milliseconds: number, error: Error): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(error), milliseconds);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (reason) => {
        clearTimeout(timer);
        reject(reason);
      }
    );
  });
}

function removeSocket(path: string): void {
  if (!existsSync(path)) return;
  try {
    unlinkSync(path);
  } catch (error) {
    throw new InitializationError(path, `Failed to remove Unix socket: ${errorMessage(error)}`);
  }
}

function log(
  level: "info" | "warn" | "error",
  message: string,
  fields: Record<string, unknown>
): void {
  process.stderr.write(
    `${JSON.stringify({ timestamp: new Date().toISOString(), level, message, ...fields })}\n`
  );
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
