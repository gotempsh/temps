// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { timingSafeEqual } from "node:crypto";
import { HandshakeError } from "./errors.js";
import type { PluginLaunchConfig } from "./types.js";

/** Host configuration arrives through the child's private stdin, never argv. */
export function parseLaunchConfig(
  text: string,
  pluginName: string,
): PluginLaunchConfig {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    throw new HandshakeError(
      pluginName,
      "Invalid host launch configuration JSON",
    );
  }
  if (
    typeof value !== "object" ||
    value === null ||
    !("protocol_version" in value) ||
    value.protocol_version !== 2 ||
    !("auth_secret" in value) ||
    typeof value.auth_secret !== "string" ||
    !/^[A-Za-z0-9_-]{32,256}$/.test(value.auth_secret) ||
    !("database_url" in value) ||
    (value.database_url !== null && typeof value.database_url !== "string") ||
    !("host_data_dir" in value) ||
    (value.host_data_dir !== null && typeof value.host_data_dir !== "string")
  ) {
    throw new HandshakeError(
      pluginName,
      "Expected authenticated protocol 2 host launch configuration",
    );
  }
  return {
    protocol_version: 2,
    auth_secret: value.auth_secret,
    database_url: value.database_url,
    host_data_dir: value.host_data_dir,
  };
}

export async function readLaunchConfig(
  stream: ReadableStream<Uint8Array>,
  pluginName: string,
): Promise<PluginLaunchConfig> {
  const reader = stream.getReader();
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      (async () => {
        let text = "";
        let bytes = 0;
        const decoder = new TextDecoder("utf-8", { fatal: true });
        while (true) {
          const next = await reader.read();
          if (next.done)
            throw new HandshakeError(
              pluginName,
              "Host closed stdin before launch configuration",
            );
          bytes += next.value.byteLength;
          if (bytes > 65_536)
            throw new HandshakeError(
              pluginName,
              "Host launch configuration exceeds 64 KiB",
            );
          text += decoder.decode(next.value, { stream: true });
          const end = text.indexOf("\n");
          if (end !== -1)
            return parseLaunchConfig(text.slice(0, end), pluginName);
        }
      })(),
      new Promise<never>((_, reject) => {
        timer = setTimeout(
          () =>
            reject(
              new HandshakeError(
                pluginName,
                "Host launch configuration timed out",
              ),
            ),
          30_000,
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
    await reader.cancel().catch(() => undefined);
    reader.releaseLock();
  }
}

/** Verify before accepting a channel slot, forwarding identity, or delivering an event. */
export function authenticatedHostRequest(
  headers: Headers,
  secret: string,
): boolean {
  const provided = headers.get("x-temps-auth-signature");
  if (!provided || provided.length !== secret.length) return false;
  const encoder = new TextEncoder();
  const actual = encoder.encode(provided);
  const expected = encoder.encode(secret);
  return (
    actual.byteLength === expected.byteLength &&
    timingSafeEqual(actual, expected)
  );
}

export function validateHealthPath(path: string, pluginName: string): void {
  if (
    !path.startsWith("/") ||
    path.startsWith("/_temps") ||
    path.startsWith("/_events") ||
    path.includes("%") ||
    path.includes("?") ||
    path.includes("#") ||
    path.includes("\\") ||
    path.split("/").some((segment) => segment === "." || segment === "..")
  ) {
    throw new HandshakeError(
      pluginName,
      "Health path must be an absolute non-reserved route",
    );
  }
}

export function requiresHostAuthentication(
  path: string,
  healthPath: string,
): boolean {
  return (
    path.startsWith("/_temps") ||
    path.startsWith("/_events") ||
    (path !== healthPath && path !== "/health")
  );
}
