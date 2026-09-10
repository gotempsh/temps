// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Protocol constants and helpers for host <-> plugin communication.
 *
 * Mirrors: crates/temps-plugin-sdk/src/protocol.rs
 */

import type {
  HandshakeMessage,
  PluginManifest,
  TempsUserContext,
} from "./types.js";
import type { IncomingMessage } from "node:http";

// ---------------------------------------------------------------------------
// Header names injected by the Temps proxy on forwarded requests
// ---------------------------------------------------------------------------

export const HEADER_PLUGIN_NAME = "x-temps-plugin";
export const HEADER_USER_ID = "x-temps-user-id";
export const HEADER_USER_EMAIL = "x-temps-user-email";
export const HEADER_USER_ROLE = "x-temps-user-role";
export const HEADER_REQUEST_ID = "x-temps-request-id";
export const HEADER_AUTH_SIGNATURE = "x-temps-auth-signature";

/** WebSocket path for the bidirectional data channel. */
export const PLUGIN_CHANNEL_PATH = "/_temps/channel";

// ---------------------------------------------------------------------------
// Handshake helpers
// ---------------------------------------------------------------------------

/**
 * Write a handshake message to stdout as a JSON line.
 * The host reads these during plugin startup.
 */
export function writeHandshakeMessage(msg: HandshakeMessage): void {
  const line = JSON.stringify(msg) + "\n";
  process.stdout.write(line);
}

/**
 * Emit the manifest handshake message (phase 1).
 */
export function emitManifest(manifest: PluginManifest): void {
  writeHandshakeMessage({
    type: "manifest",
    ...manifest,
  });
}

/**
 * Emit the ready handshake message (phase 2).
 */
export function emitReady(hasUi: boolean): void {
  writeHandshakeMessage({
    type: "ready",
    ready: true,
    has_ui: hasUi,
  });
}

// ---------------------------------------------------------------------------
// Auth context extraction
// ---------------------------------------------------------------------------

/**
 * Extract authenticated user context from proxy-injected headers.
 *
 * @param req - Incoming HTTP request with Temps headers.
 * @returns Parsed user context, or undefined if no auth headers present.
 */
export function extractAuthContext(
  req: IncomingMessage
): TempsUserContext | undefined {
  const role = getHeader(req, HEADER_USER_ROLE);
  const requestId = getHeader(req, HEADER_REQUEST_ID);

  if (!role || !requestId) {
    return undefined;
  }

  const userIdRaw = getHeader(req, HEADER_USER_ID);

  return {
    userId: userIdRaw ? parseInt(userIdRaw, 10) : undefined,
    userEmail: getHeader(req, HEADER_USER_EMAIL) ?? undefined,
    role,
    requestId,
  };
}

function getHeader(
  req: IncomingMessage,
  name: string
): string | undefined {
  const value = req.headers[name];
  if (Array.isArray(value)) return value[0];
  return value ?? undefined;
}
