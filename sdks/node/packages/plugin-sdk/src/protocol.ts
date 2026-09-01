// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  HandshakeMessage,
  PluginLaunchConfig,
  PluginManifest,
} from "./types.js";
import { EXTERNAL_PLUGIN_PROTOCOL_VERSION } from "./types.js";
import { HandshakeError } from "./errors.js";

export const HEADER_PREFIX = "x-temps-";
export const HEADER_PLUGIN_NAME = "x-temps-plugin";
export const HEADER_USER_ID = "x-temps-user-id";
export const HEADER_USER_EMAIL = "x-temps-user-email";
export const HEADER_USER_ROLE = "x-temps-user-role";
export const HEADER_USER_PERMISSIONS = "x-temps-user-permissions";
export const HEADER_REQUEST_ID = "x-temps-request-id";
export const HEADER_AUTH_SIGNATURE = "x-temps-auth-signature";
export const HEADER_ACTOR_TOKEN = "x-temps-actor-token";

export const PLUGIN_CHANNEL_PATH = "/_temps/channel";
export const PLUGIN_EVENTS_PATH = "/_events";

export function encodeHandshakeMessage(message: HandshakeMessage): string {
  return `${JSON.stringify(message)}\n`;
}

export function writeHandshakeMessage(message: HandshakeMessage): void {
  process.stdout.write(encodeHandshakeMessage(message));
}

export function emitHello(manifest: PluginManifest): void {
  writeHandshakeMessage({
    type: "hello",
    protocol_version: EXTERNAL_PLUGIN_PROTOCOL_VERSION,
    manifest,
  });
}

export function emitReady(options: {
  hasUi: boolean;
  openapi?: Record<string, unknown>;
}): void {
  writeHandshakeMessage({
    type: "ready",
    ready: true,
    has_ui: options.hasUi,
    protocol_version: EXTERNAL_PLUGIN_PROTOCOL_VERSION,
    ...(options.openapi === undefined ? {} : { openapi: options.openapi }),
  });
}

export function parseLaunchConfigLine(
  line: string,
  manifest: PluginManifest
): PluginLaunchConfig {
  let value: unknown;
  try {
    value = JSON.parse(line);
  } catch (error) {
    throw new HandshakeError(
      manifest.name,
      `Temps sent invalid launch configuration JSON: ${errorMessage(error)}`
    );
  }

  if (!isRecord(value)) {
    throw new HandshakeError(manifest.name, "Temps sent a non-object launch configuration");
  }

  const protocolVersion = value.protocol_version;
  const authSecret = value.auth_secret;
  const databaseUrl = optionalString(value.database_url);
  const hostDataDir = optionalString(value.host_data_dir);

  if (protocolVersion !== EXTERNAL_PLUGIN_PROTOCOL_VERSION) {
    throw new HandshakeError(
      manifest.name,
      `Temps sent external-plugin protocol ${String(protocolVersion)}, but this SDK requires ${EXTERNAL_PLUGIN_PROTOCOL_VERSION}`
    );
  }
  if (typeof authSecret !== "string" || authSecret.trim() === "") {
    throw new HandshakeError(manifest.name, "Temps supplied an empty request-assertion secret");
  }
  if (databaseUrl === undefined || hostDataDir === undefined) {
    throw new HandshakeError(manifest.name, "Temps sent invalid host path fields in launch configuration");
  }
  if (
    manifest.requires_db !== (databaseUrl !== null) ||
    manifest.requires_host_data_access !== (hostDataDir !== null)
  ) {
    throw new HandshakeError(
      manifest.name,
      "Temps launch configuration does not match the manifest's declared host access requirements"
    );
  }

  return {
    protocol_version: protocolVersion,
    auth_secret: authSecret,
    database_url: databaseUrl,
    host_data_dir: hostDataDir,
  };
}

export async function readLaunchConfig(
  manifest: PluginManifest,
  input: NodeJS.ReadStream = process.stdin
): Promise<PluginLaunchConfig> {
  const line = await readOneLine(input, manifest.name);
  return parseLaunchConfigLine(line, manifest);
}

function readOneLine(input: NodeJS.ReadStream, pluginName: string): Promise<string> {
  return new Promise((resolve, reject) => {
    let buffer = "";

    const cleanup = () => {
      input.off("data", onData);
      input.off("end", onEnd);
      input.off("error", onError);
      input.pause();
    };
    const finish = (line: string) => {
      cleanup();
      resolve(line);
    };
    const onData = (chunk: string | Buffer) => {
      buffer += chunk.toString();
      const newline = buffer.indexOf("\n");
      if (newline >= 0) finish(buffer.slice(0, newline));
    };
    const onEnd = () => {
      cleanup();
      reject(
        new HandshakeError(
          pluginName,
          "Temps closed stdin before sending typed launch configuration; the plugin and Temps likely use incompatible SDK versions"
        )
      );
    };
    const onError = (error: Error) => {
      cleanup();
      reject(
        new HandshakeError(
          pluginName,
          `Failed to read typed launch configuration: ${error.message}`
        )
      );
    };

    input.setEncoding("utf8");
    input.on("data", onData);
    input.once("end", onEnd);
    input.once("error", onError);
    input.resume();
  });
}

function optionalString(value: unknown): string | null | undefined {
  return value === null || typeof value === "string" ? value : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
