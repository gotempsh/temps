// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * A plugin that runs as an ordinary Temps service: what it reads from its
 * container's environment, the secret it shares with Temps, and a platform
 * channel that survives the platform reconnecting.
 *
 * Mirrors: crates/temps-plugin-sdk/src/runtime.rs (service mode)
 */

import { createHash } from "node:crypto";
import { mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { WsLike } from "./client.js";
import { ArgsError, ChannelClosedError } from "./errors.js";

/** Authenticated route Temps reads a plugin service's manifest from. */
export const PLUGIN_HELLO_PATH = "/_temps/plugin/hello";

/** Domain separation for the secret derived from the deployment token. */
export const SERVICE_AUTH_CONTEXT = "temps-plugin-service-auth-v1";

/** Port a plugin service listens on when its environment names none. */
export const DEFAULT_SERVICE_PORT = 8080;

/**
 * Where a plugin service keeps its files when its environment names no
 * directory. Mount the service's volume here to keep them across deployments.
 */
export const DEFAULT_SERVICE_DATA_DIR = "/data";

/**
 * The request-assertion secret of a plugin that runs as a Temps service: the
 * lowercase hex SHA-256 of {@link SERVICE_AUTH_CONTEXT}, a NUL byte and the
 * service's deployment token.
 *
 * Temps computes the same value from the token it issued, so nothing has to
 * be exchanged, and the token itself is never sent to the plugin's port.
 */
export function serviceAuthSecret(apiToken: string): string {
  return createHash("sha256")
    .update(SERVICE_AUTH_CONTEXT, "utf8")
    .update(Buffer.from([0]))
    .update(apiToken, "utf8")
    .digest("hex");
}

/** What a plugin service reads from the environment Temps gives it. */
export interface ServiceEnvironment {
  /** `PORT`: the port the service must listen on. Defaults to 8080. */
  port: number;
  /** The secret Temps presents with every request to this service. */
  authSecret: string;
  /** `TEMPS_PLUGIN_DATA_DIR`, or `/data`: where the plugin keeps its files. */
  dataDir: string;
}

/**
 * Read the service environment. Temps sets all of it on every deployment; a
 * plugin needs no configuration of its own to be reachable.
 */
export function serviceEnvironment(
  env: Record<string, string | undefined>,
): ServiceEnvironment {
  const value = (name: string): string | undefined => {
    const raw = env[name]?.trim();
    return raw === undefined || raw === "" ? undefined : raw;
  };
  const rawPort = value("PORT");
  let port = DEFAULT_SERVICE_PORT;
  if (rawPort !== undefined) {
    port = /^[0-9]{1,5}$/.test(rawPort) ? Number(rawPort) : 0;
    if (port < 1 || port > 65_535)
      throw new ArgsError(
        `PORT is '${rawPort}', which is not a TCP port between 1 and 65535`,
      );
  }
  const apiToken = value("TEMPS_API_TOKEN");
  if (apiToken === undefined)
    throw new ArgsError(
      "TEMPS_API_TOKEN is not set. A plugin runs as a Temps service: deploy it to a Temps instance, which sets this variable on every deployment, and add the service under Settings > Plugins. Temps and the plugin both derive the request-assertion secret from it.",
    );
  return {
    port,
    authSecret: serviceAuthSecret(apiToken),
    dataDir: value("TEMPS_PLUGIN_DATA_DIR") ?? DEFAULT_SERVICE_DATA_DIR,
  };
}

/**
 * Whether the plugin was started by a Temps that runs it as a child process
 * (it is given a socket to bind) rather than deployed as a service.
 */
export function startedAsChildProcess(argv: string[]): boolean {
  return argv.includes("--socket-path");
}

/**
 * The directory a plugin service keeps its files in.
 *
 * The configured directory is used when it can be created. Otherwise the
 * plugin still starts, on a temporary directory that does not survive the
 * container, and says so: a plugin that stores nothing should not fail to
 * start because no volume is mounted.
 */
export function serviceDataDir(pluginName: string, configured: string): string {
  try {
    mkdirSync(configured, { recursive: true });
    return configured;
  } catch (error) {
    const fallback = join(tmpdir(), "temps-plugin-data");
    mkdirSync(fallback, { recursive: true });
    console.error(
      `[temps-plugin] ${pluginName}: the data directory ${configured} cannot be created (${error instanceof Error ? error.message : String(error)}); using ${fallback}, which does not survive the container. Mount a volume at ${configured} to keep the plugin's files.`,
    );
    return fallback;
  }
}

type Listener = (...args: unknown[]) => void;

/**
 * The platform channel as the client sees it: one object for the life of the
 * plugin, over whichever connection the platform currently holds.
 *
 * Temps opens the channel, and opens it again after it restarts or after a
 * network break. Attaching a new connection ends the previous one: the client
 * is told the channel closed (requests in flight fail) and then that it is
 * open again. A connection that ends late, after it was replaced, changes
 * nothing.
 */
export class PlatformChannel implements WsLike {
  private listeners = new Map<string, Set<Listener>>();
  private current: WsLike | undefined;

  /** Whether the platform is connected right now. */
  get connected(): boolean {
    return this.current !== undefined;
  }

  on(event: string, fn: Listener): void {
    let listeners = this.listeners.get(event);
    if (!listeners) {
      listeners = new Set();
      this.listeners.set(event, listeners);
    }
    listeners.add(fn);
  }

  send(data: string, cb?: (err?: Error) => void): void {
    if (!this.current) {
      cb?.(new ChannelClosedError());
      return;
    }
    this.current.send(data, cb);
  }

  /** Close the current connection; the platform may connect again. */
  close(): void {
    const current = this.current;
    if (!current) return;
    this.current = undefined;
    current.close();
    this.emit("close");
  }

  /** Use `connection` from now on, replacing the one in use. */
  attach(connection: WsLike): void {
    const previous = this.current;
    if (previous) {
      this.current = undefined;
      this.emit("close");
      previous.close();
    }
    this.current = connection;
    connection.on("message", (...args: unknown[]) => {
      if (this.current === connection) this.emit("message", ...args);
    });
    connection.on("close", () => {
      if (this.current !== connection) return;
      this.current = undefined;
      this.emit("close");
    });
    this.emit("open");
  }

  private emit(event: string, ...args: unknown[]): void {
    for (const listener of this.listeners.get(event) ?? []) listener(...args);
  }
}
