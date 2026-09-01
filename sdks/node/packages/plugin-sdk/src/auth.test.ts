// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { IncomingMessage } from "node:http";
import { describe, expect, it } from "vitest";
import {
  attachVerifiedCaller,
  extractAuthContext,
  secureSecretMatches,
  verifyProxyHeaders,
} from "./auth.js";

describe("proxy authentication", () => {
  it("creates a typed caller only after verifying the process assertion", () => {
    const headers = new Headers({
      "x-temps-auth-signature": "secret",
      "x-temps-user-id": "42",
      "x-temps-user-email": "developer@example.com",
      "x-temps-user-role": "custom",
      "x-temps-user-permissions": "projects:read,deployments:create,INVALID",
      "x-temps-request-id": "request-1",
      "x-temps-actor-token": "signed-actor",
    });

    const result = verifyProxyHeaders(headers, "secret");
    expect(result.verified).toBe(true);
    if (!result.verified || !result.caller) throw new Error("caller missing");
    expect(result.caller.userId).toBe(42);
    expect(result.caller.hasPermission("projects:read")).toBe(true);
    expect(result.caller.hasPermission("INVALID")).toBe(false);
    expect(JSON.stringify(result.caller)).not.toContain("signed-actor");
  });

  it("rejects forged, missing, and partial assertions", () => {
    expect(verifyProxyHeaders(new Headers(), "secret").verified).toBe(false);
    expect(
      verifyProxyHeaders(
        new Headers({
          "x-temps-auth-signature": "forged",
          "x-temps-user-role": "admin",
        }),
        "secret"
      ).verified
    ).toBe(false);
    expect(
      verifyProxyHeaders(
        new Headers({
          "x-temps-auth-signature": "secret",
          "x-temps-user-role": "admin",
        }),
        "secret"
      ).verified
    ).toBe(false);
    expect(secureSecretMatches("secret", "secret")).toBe(true);
    expect(secureSecretMatches("secret-extra", "secret")).toBe(false);
  });

  it("never parses raw request headers in extractAuthContext", () => {
    const request = {
      headers: { "x-temps-user-role": "admin" },
    } as unknown as IncomingMessage;
    expect(extractAuthContext(request)).toBeUndefined();

    const verified = verifyProxyHeaders(
      new Headers({
        "x-temps-auth-signature": "secret",
        "x-temps-user-id": "7",
        "x-temps-user-email": "reader@example.com",
        "x-temps-user-role": "reader",
      }),
      "secret"
    );
    if (!verified.verified) throw new Error("verification failed");
    attachVerifiedCaller(request, verified.caller);
    expect(extractAuthContext(request)?.role).toBe("reader");
  });
});
