// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { timingSafeEqual } from "node:crypto";
import type { IncomingMessage } from "node:http";
import type { PluginRole, TempsUserContext } from "./types.js";
import {
  HEADER_ACTOR_TOKEN,
  HEADER_AUTH_SIGNATURE,
  HEADER_REQUEST_ID,
  HEADER_USER_EMAIL,
  HEADER_USER_ID,
  HEADER_USER_PERMISSIONS,
  HEADER_USER_ROLE,
} from "./protocol.js";

const ROLES = new Set<PluginRole>([
  "admin",
  "platform_admin",
  "user",
  "reader",
  "api_reader",
  "custom",
  "metrics_ingest",
]);

const verifiedCallers = new WeakMap<IncomingMessage, AuthenticatedCaller | undefined>();

export class AuthenticatedCaller implements TempsUserContext {
  readonly userId: number;
  readonly userEmail: string;
  readonly role: PluginRole;
  readonly permissions: ReadonlySet<string>;
  readonly requestId: string;
  readonly #actorToken?: string;

  constructor(options: {
    userId: number;
    userEmail: string;
    role: PluginRole;
    permissions: Iterable<string>;
    requestId: string;
    actorToken?: string;
  }) {
    this.userId = options.userId;
    this.userEmail = options.userEmail;
    this.role = options.role;
    this.permissions = new Set(options.permissions);
    this.requestId = options.requestId;
    this.#actorToken = options.actorToken;
  }

  isAdmin(): boolean {
    return this.role === "admin";
  }

  hasPermission(permission: string): boolean {
    return this.permissions.has(permission);
  }

  /** @internal Used by PluginContext to bind platform calls to this caller. */
  actorToken(): string | undefined {
    return this.#actorToken;
  }

  toJSON(): Record<string, unknown> {
    return {
      userId: this.userId,
      userEmail: this.userEmail,
      role: this.role,
      permissions: [...this.permissions],
      requestId: this.requestId,
    };
  }
}

export type ProxyVerification =
  | { verified: true; caller?: AuthenticatedCaller }
  | { verified: false; reason: string };

/** Verify the per-process assertion before trusting any x-temps-* identity. */
export function verifyProxyHeaders(
  headers: Headers,
  expectedSecret: string
): ProxyVerification {
  const providedSecret = headers.get(HEADER_AUTH_SIGNATURE);
  if (!providedSecret || !secureSecretMatches(providedSecret, expectedSecret)) {
    return { verified: false, reason: "missing or invalid platform assertion" };
  }

  const userIdRaw = headers.get(HEADER_USER_ID);
  const userEmail = headers.get(HEADER_USER_EMAIL);
  const roleRaw = headers.get(HEADER_USER_ROLE);

  if (!userIdRaw && !userEmail && !roleRaw) {
    return { verified: true };
  }

  const userId = userIdRaw === null ? Number.NaN : Number(userIdRaw);
  if (
    !Number.isSafeInteger(userId) ||
    !userEmail ||
    !roleRaw ||
    !ROLES.has(roleRaw as PluginRole)
  ) {
    return { verified: false, reason: "incomplete or invalid caller assertion" };
  }

  const permissions = (headers.get(HEADER_USER_PERMISSIONS) ?? "")
    .split(",")
    .map((permission) => permission.trim())
    .filter((permission) => /^[a-z_:]+$/.test(permission));

  return {
    verified: true,
    caller: new AuthenticatedCaller({
      userId,
      userEmail,
      role: roleRaw as PluginRole,
      permissions,
      requestId: headers.get(HEADER_REQUEST_ID) ?? "",
      actorToken: headers.get(HEADER_ACTOR_TOKEN) ?? undefined,
    }),
  };
}

export function secureSecretMatches(provided: string, expected: string): boolean {
  const providedBytes = Buffer.from(provided);
  const expectedBytes = Buffer.from(expected);
  return (
    providedBytes.length === expectedBytes.length &&
    timingSafeEqual(providedBytes, expectedBytes)
  );
}

/** @internal Associate a verified caller with the SDK-owned request object. */
export function attachVerifiedCaller(
  request: IncomingMessage,
  caller: AuthenticatedCaller | undefined
): void {
  verifiedCallers.set(request, caller);
}

/**
 * Return only the caller previously verified by the SDK runtime.
 * Raw request headers are deliberately never parsed here.
 */
export function extractAuthContext(
  request: IncomingMessage
): AuthenticatedCaller | undefined {
  return verifiedCallers.get(request);
}
