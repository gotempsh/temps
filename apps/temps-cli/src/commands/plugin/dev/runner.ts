// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { mkdir, chmod, rm } from "node:fs/promises";
import { resolve, join } from "node:path";
import { WebSocket } from "ws";
import {
  DevError,
  MAX_BODY,
  grants,
  integer,
  manifest,
  record,
  subscribes,
  validateEvent,
} from "./model.js";
import type {
  PluginEvent,
  PluginHostPermission,
  PluginManifest,
} from "./model.js";
import { MockHost, parseFixtures } from "./host.js";
import {
  loadState,
  privateDirectory,
  saveState,
  sessionPaths,
} from "./session.js";
export interface RunnerOptions {
  session: string;
  command: string[];
  port?: number;
  dataDir?: string;
  grants?: PluginHostPermission[];
  role?: string;
  fixtures?: unknown;
  startupTimeout?: number;
  signal?: AbortSignal;
}
async function startupStep<T>(
  work: Promise<T>,
  timeoutMs: number,
  signal?: AbortSignal,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let abort = () => {};
  try {
    return await Promise.race([
      work,
      new Promise<never>((_, reject) => {
        abort = () =>
          reject(
            new DevError(
              "Plugin startup interrupted; temporary processes and sockets were cleaned up.",
            ),
          );
        if (signal?.aborted) abort();
        else signal?.addEventListener("abort", abort, { once: true });
        timer = setTimeout(
          () =>
            reject(
              new DevError(
                `Plugin startup timed out after ${timeoutMs} ms. Check the protocol-2 handshake.`,
              ),
            ),
          timeoutMs,
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
    signal?.removeEventListener("abort", abort);
  }
}
export async function startRunner(options: RunnerOptions) {
  if (!["darwin", "linux"].includes(process.platform))
    throw new DevError(
      "Plugin dev currently supports macOS and Linux Unix sockets.",
    );
  const role = options.role ?? "admin";
  if (!["admin", "reader"].includes(role))
    throw new DevError("Preview role must be admin or reader.");
  const fixtures = parseFixtures(options.fixtures);
  const startupTimeout = integer(
    options.startupTimeout ?? 10000,
    "startup timeout",
    100,
    60000,
  );
  const port = integer(options.port ?? 0, "port", 0, 65535);
  if (!options.command.length)
    throw new DevError("Supply a plugin executable or --exec command.");
  const paths = sessionPaths(options.session);
  await privateDirectory(paths.root);
  await privateDirectory(paths.state);
  await privateDirectory(paths.runtimeRoot);
  try {
    await mkdir(paths.runtime, { mode: 0o700 });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "EEXIST")
      throw new DevError(
        `Session ${options.session} is occupied. Check plugin dev status --session ${options.session}. If a previous runner crashed, use a new --session and --data-dir ${join(paths.state, "data")} to retain plugin data. Lock: ${paths.runtime}`,
      );
    throw error;
  }
  const secret =
    crypto.randomUUID().replaceAll("-", "") +
    crypto.randomUUID().replaceAll("-", "");
  const cookie = crypto.randomUUID();
  const statePath = join(paths.state, "state.json");
  let child: ChildProcessWithoutNullStreams | undefined;
  let ws: WebSocket | undefined;
  let preview: ReturnType<typeof Bun.serve> | undefined;
  let controlServer: ReturnType<typeof Bun.serve> | undefined;
  let stopping = false;
  let exitResolve: (code: number) => void = () => {};
  const exited = new Promise<number>((r) => {
    exitResolve = r;
  });
  const logs: Record<string, unknown>[] = [];
  function log(operation: string, fields: Record<string, unknown> = {}) {
    const entry = {
      timestamp: new Date().toISOString(),
      session: options.session,
      operation,
      simulated: true,
      ...fields,
    };
    logs.push(entry);
    if (logs.length > 200) logs.shift();
    process.stderr.write(JSON.stringify(entry) + "\n");
  }
  let stopPromise: Promise<void> | undefined;
  function stop() {
    return (stopPromise ??= (async () => {
      stopping = true;
      controlServer?.stop(true);
      preview?.stop(true);
      ws?.terminate();
      if (child?.pid) {
        const signalGroup = (signal: NodeJS.Signals) => {
          try {
            process.kill(-child!.pid!, signal);
          } catch (error) {
            if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
          }
        };
        signalGroup("SIGTERM");
        const deadline = Date.now() + 2000;
        while (Date.now() < deadline) {
          try {
            process.kill(-child.pid, 0);
          } catch {
            break;
          }
          await Bun.sleep(20);
        }
        signalGroup("SIGKILL");
        await exited;
      }
      await rm(paths.runtime, { recursive: true, force: true });
    })());
  }
  try {
    const previous = await loadState(statePath);
    if (
      previous !== undefined &&
      (!record(previous) ||
        typeof previous.actorId !== "string" ||
        !Array.isArray(previous.grants) ||
        typeof previous.pluginName !== "string")
    )
      throw new DevError(
        `Invalid saved state for session ${options.session}; use a new session.`,
      );
    let effectiveGrants = grants(
      options.grants ?? (record(previous) ? previous.grants : []),
    );
    const actorId = record(previous)
      ? String(previous.actorId)
      : crypto.randomUUID();
    const dataDir = resolve(options.dataDir ?? join(paths.state, "data"));
    await privateDirectory(dataDir);
    child = spawn(
      options.command[0]!,
      [
        ...options.command.slice(1),
        "--socket-path",
        paths.socket,
        "--data-dir",
        dataDir,
      ],
      { stdio: ["pipe", "pipe", "pipe"], detached: true },
    );
    child.on("error", () => {
      exitResolve(1);
    });
    child.on("exit", (code) => {
      exitResolve(code ?? 1);
      if (!stopping) log("plugin_exit", { code });
    });
    // Redact complete lines so a launch secret split across pipe chunks cannot leak.
    let stderrLine = "";
    let droppingLine = false;
    child.stderr.on("data", (chunk: Buffer) => {
      for (const piece of chunk.toString("utf8").split(/(?<=\n)/)) {
        if (!droppingLine) stderrLine += piece;
        if (stderrLine.length > 8192) {
          stderrLine = "";
          droppingLine = true;
        }
        if (piece.endsWith("\n")) {
          if (!droppingLine)
            log("plugin_stderr", {
              message: stderrLine
                .replaceAll(secret, "[redacted]")
                .replaceAll(cookie, "[redacted]")
                .slice(0, 2000),
            });
          stderrLine = "";
          droppingLine = false;
        }
      }
    });
    child.stdin.on("error", () => {});
    let buffer = "";
    let pluginManifest: PluginManifest | undefined;
    let readyResolve: (m: PluginManifest) => void = () => {};
    let readyReject: (e: Error) => void = () => {};
    let readyDone = false;
    const ready = new Promise<PluginManifest>((r, j) => {
      readyResolve = r;
      readyReject = j;
    });
    child.stdout.on("data", (chunk: Buffer) => {
      if (readyDone) return;
      buffer += chunk.toString("utf8");
      if (Buffer.byteLength(buffer) > 65536) {
        readyReject(new DevError("Plugin handshake exceeds 64 KiB."));
        readyDone = true;
        return;
      }
      let end: number;
      while ((end = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, end);
        buffer = buffer.slice(end + 1);
        try {
          const msg = JSON.parse(line);
          if (!record(msg) || msg.protocol_version !== 2)
            throw new DevError(
              "Plugin must use protocol 2; rebuild with a compatible Temps plugin SDK.",
            );
          if (msg.type === "hello" && !pluginManifest) {
            pluginManifest = manifest(msg.manifest);
            if (record(previous) && previous.pluginName !== pluginManifest.name)
              throw new DevError(
                "Session belongs to a different plugin. Use a new --session.",
              );
            child!.stdin.write(
              JSON.stringify({
                protocol_version: 2,
                auth_secret: secret,
                database_url: null,
                host_data_dir: null,
              }) + "\n",
            );
          } else if (
            msg.type === "ready" &&
            msg.ready === true &&
            pluginManifest
          ) {
            readyDone = true;
            readyResolve(pluginManifest);
            return;
          } else
            throw new DevError(
              "Unexpected plugin handshake order; expected hello, then ready.",
            );
        } catch (error) {
          readyDone = true;
          readyReject(
            error instanceof DevError
              ? error
              : new DevError(
                  "Plugin emitted invalid handshake JSON. Keep logs on stderr.",
                ),
          );
          return;
        }
      }
    });
    const earlyExit = exited.then(() => {
      throw new DevError(
        `Plugin command ${options.command[0]} exited before initialization. Check the executable and SDK version.`,
      );
    });
    const m = await startupStep(
      Promise.race([ready, earlyExit]),
      startupTimeout,
      options.signal,
    );
    await chmod(paths.socket, 0o600);
    const host = new MockHost(
      m.name,
      actorId,
      effectiveGrants.filter((p) => (m.host_permissions ?? []).includes(p)),
      fixtures,
    );
    await saveState(statePath, {
      actorId,
      pluginName: m.name,
      grants: effectiveGrants,
    });
    const authHeaders = () => ({
      "x-temps-auth-signature": secret,
      "x-temps-plugin": m.name,
      "x-temps-request-id": crypto.randomUUID(),
      "x-temps-user-role": role,
      "x-temps-user-id": "1",
      "x-temps-user-email": "developer@example.invalid",
    });
    ws = new WebSocket(`ws+unix://${paths.socket}:/_temps/channel`, {
      headers: authHeaders(),
      maxPayload: MAX_BODY,
      handshakeTimeout: startupTimeout,
    });
    let pending = 0;
    ws.on("message", (raw) => {
      let msg: unknown;
      try {
        msg = JSON.parse(raw.toString());
      } catch {
        ws?.close(1007);
        return;
      }
      if (
        !record(msg) ||
        msg.type !== "request" ||
        !Number.isSafeInteger(msg.id) ||
        (msg.id as number) < 0 ||
        !record(msg.call) ||
        typeof msg.call.method !== "string" ||
        msg.call.method.length > 128 ||
        !record(msg.call.params)
      ) {
        ws?.close(1008);
        return;
      }
      const id = msg.id;
      const method = msg.call.method;
      const params = msg.call.params;
      const reply = (outcome: unknown) => {
        if (ws?.readyState === WebSocket.OPEN)
          ws.send(JSON.stringify({ type: "response", id, outcome }));
      };
      if (pending >= 32) {
        reply({
          err: {
            code: "internal",
            message: "Local host is saturated (32 concurrent requests).",
          },
        });
        return;
      }
      pending++;
      void host
        .call(method, params)
        .then(
          (result) => {
            reply({ ok: { method, result } });
            log("host_call", { actor_id: actorId, method, outcome: "ok" });
          },
          (error) => {
            const code = error instanceof DevError ? error.code : "internal";
            reply({
              err: {
                code,
                message:
                  error instanceof DevError
                    ? error.message
                    : "Local host simulation failed.",
              },
            });
            log("host_call", { actor_id: actorId, method, outcome: code });
          },
        )
        .finally(() => {
          pending--;
        });
    });
    ws.on("error", () => {
      log("channel_error");
    });
    await startupStep(
      Promise.race([
        new Promise<void>((r, j) => {
          ws!.once("open", r);
          ws!.once("error", () =>
            j(
              new DevError(
                "Cannot connect to the authenticated plugin channel.",
              ),
            ),
          );
        }),
        earlyExit,
      ]),
      startupTimeout,
      options.signal,
    );
    const proxyPrefix = `/x/${m.name}`;
    let origin = "";
    preview = Bun.serve({
      hostname: "127.0.0.1",
      port,
      maxRequestBodySize: MAX_BODY,
      idleTimeout: 30,
      async fetch(req) {
        const url = new URL(req.url);
        if (
          req.headers.get("host") !== new URL(origin).host ||
          (req.headers.has("origin") && req.headers.get("origin") !== origin) ||
          req.headers.get("sec-fetch-site") === "cross-site"
        )
          return new Response("Local preview only", { status: 403 });
        if (url.pathname === "/")
          return new Response(null, {
            status: 302,
            headers: {
              location: `${proxyPrefix}/ui/`,
              "set-cookie": `temps_dev_session=${cookie}; HttpOnly; SameSite=Strict; Path=/`,
            },
          });
        const routePrefix = url.pathname.startsWith(`/api${proxyPrefix}/`)
          ? `/api${proxyPrefix}`
          : proxyPrefix;
        if (!url.pathname.startsWith(`${routePrefix}/`))
          return new Response("Not found", { status: 404 });
        const path = url.pathname.slice(routePrefix.length);
        let decoded: string;
        try {
          decoded = decodeURIComponent(path);
        } catch {
          return new Response("Invalid path", { status: 400 });
        }
        if (
          decoded.includes("..") ||
          decoded.includes("\\") ||
          decoded.startsWith("/_temps") ||
          decoded.startsWith("/_events")
        )
          return new Response("Internal plugin endpoint", { status: 403 });
        if (
          !["GET", "HEAD"].includes(req.method) &&
          (req.headers.get("origin") !== origin ||
            !(req.headers.get("cookie") ?? "")
              .split(";")
              .some((c) => c.trim() === `temps_dev_session=${cookie}`))
        )
          return new Response(
            "Open the local preview first; writes require its session cookie and same-origin Origin header.",
            { status: 403 },
          );
        const headers = new Headers(req.headers);
        const requestKeys: string[] = [];
        headers.forEach((_, key) => requestKeys.push(key));
        for (const key of requestKeys)
          if (
            key.startsWith("x-temps-") ||
            [
              "host",
              "connection",
              "upgrade",
              "authorization",
              "proxy-authorization",
              "cookie",
            ].includes(key)
          )
            headers.delete(key);
        for (const [key, value] of Object.entries(authHeaders()))
          headers.set(key, value);
        headers.set("x-forwarded-prefix", proxyPrefix);
        try {
          const response = await fetch(`http://localhost${path}${url.search}`, {
            unix: paths.socket,
            method: req.method,
            headers,
            body: ["GET", "HEAD"].includes(req.method) ? undefined : req.body,
            redirect: "manual",
            signal: AbortSignal.timeout(30000),
          });
          const out = new Headers(response.headers);
          const responseKeys: string[] = [];
          out.forEach((_, key) => responseKeys.push(key));
          for (const key of responseKeys)
            if (
              key.startsWith("x-temps-") ||
              ["set-cookie", "set-cookie2"].includes(key)
            )
              out.delete(key);
          const location = out.get("location");
          if (
            location?.startsWith("/") &&
            !location.startsWith("//") &&
            !location.startsWith(proxyPrefix + "/")
          )
            out.set("location", proxyPrefix + location);
          return new Response(response.body, {
            status: response.status,
            headers: out,
          });
        } catch {
          return new Response(
            "Plugin unavailable. Check runner status and restart if it exited.",
            { status: 502 },
          );
        }
      },
    });
    origin = `http://127.0.0.1:${preview.port}`;
    let deliveries = 0;
    async function emit(event: PluginEvent, transport: string) {
      if (!host.permissions.includes("events_read"))
        throw new DevError(
          `Event denied: grant events_read with plugin dev grants set --session ${options.session} --grant events_read.`,
          "permission_denied",
        );
      if (!subscribes(m.events, event.event_type))
        throw new DevError(
          `Plugin ${m.name} does not subscribe to ${event.event_type}.`,
          "permission_denied",
        );
      if (deliveries >= 8)
        throw new DevError(
          "Event delivery saturated (8 concurrent deliveries). Retry after current delivery finishes.",
        );
      deliveries++;
      try {
        let status: string;
        if (transport !== "http" && ws?.readyState === WebSocket.OPEN) {
          if (ws.bufferedAmount > MAX_BODY)
            throw new DevError(
              "Plugin event channel is saturated. Retry after it drains.",
            );
          await new Promise<void>((r, j) =>
            ws!.send(JSON.stringify({ type: "event", event }), (error) =>
              error
                ? j(
                    new DevError(
                      "Event channel write failed; delivery is uncertain.",
                    ),
                  )
                : r(),
            ),
          );
          status = "sent";
        } else {
          const response = await fetch("http://localhost/_events", {
            unix: paths.socket,
            method: "POST",
            body: JSON.stringify(event),
            headers: { ...authHeaders(), "content-type": "application/json" },
            signal: AbortSignal.timeout(5000),
          });
          await response.body?.cancel();
          if (!response.ok)
            throw new DevError(
              `Plugin event endpoint returned HTTP ${response.status}.`,
            );
          status = "http_accepted";
        }
        log("event_delivery", {
          actor_id: actorId,
          event_id: event.id,
          event_type: event.event_type,
          status,
        });
        return { id: event.id, status, handler_completed: false };
      } finally {
        deliveries--;
      }
    }
    controlServer = Bun.serve({
      unix: paths.control,
      maxRequestBodySize: MAX_BODY,
      async fetch(req) {
        try {
          if (req.method !== "POST")
            return new Response("POST required", { status: 405 });
          const payload: unknown = await req.json();
          if (!record(payload))
            throw new DevError("Control payload must be an object.");
          switch (new URL(req.url).pathname) {
            case "/status":
              return Response.json({
                session: options.session,
                plugin: m.name,
                preview: origin + "/",
                data_dir: dataDir,
                permissions: host.permissions,
                subscriptions: m.events,
                channel_connected: ws?.readyState === WebSocket.OPEN,
                simulated: true,
                ai_counters: "runner lifetime",
                role,
              });
            case "/logs":
              return Response.json(logs);
            case "/grants": {
              effectiveGrants = grants(payload.grants);
              await saveState(statePath, {
                actorId,
                pluginName: m.name,
                grants: effectiveGrants,
              });
              host.permissions = effectiveGrants.filter((p) =>
                (m.host_permissions ?? []).includes(p),
              );
              log("grants_changed", {
                actor_id: actorId,
                permissions: effectiveGrants,
              });
              return Response.json({ permissions: effectiveGrants });
            }
            case "/emit": {
              const event = validateEvent(payload.event);
              const transport = payload.transport ?? "auto";
              if (!["auto", "http"].includes(String(transport)))
                throw new DevError("transport must be auto or http.");
              return Response.json(await emit(event, String(transport)));
            }
            default:
              return new Response("Unknown control action", { status: 404 });
          }
        } catch (error) {
          return Response.json(
            {
              error:
                error instanceof DevError
                  ? error.message
                  : "Local runner request failed.",
              code: error instanceof DevError ? error.code : "internal",
            },
            { status: 400 },
          );
        }
      },
    });
    await chmod(paths.control, 0o600);
    if (options.signal?.aborted)
      throw new DevError("Plugin startup interrupted.");
    log("runner_ready", {
      plugin: m.name,
      preview: origin + "/",
      permissions: host.permissions,
      note: "Local host simulation; plugin executes with your OS permissions.",
    });
    return { url: origin, plugin: m, host, paths, stop, exited, dataDir };
  } catch (error) {
    await stop();
    throw error;
  }
}
