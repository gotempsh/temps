// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it, vi } from "vitest";
import { PlatformApi } from "./api.js";
import { AuthenticatedCaller } from "./auth.js";
import type { TempsClient } from "./client.js";

describe("caller-scoped platform API", () => {
  it("encodes JSON calls with the verified actor token", async () => {
    const apiCall = vi.fn().mockResolvedValue({
      status: 201,
      body: JSON.stringify({ id: 9 }),
    });
    const client = { apiCall } as unknown as TempsClient;
    const caller = new AuthenticatedCaller({
      userId: 1,
      userEmail: "developer@example.com",
      role: "user",
      permissions: ["projects:create"],
      requestId: "request-1",
      actorToken: "signed-actor",
    });

    const result = await new PlatformApi(client, caller).post<{ id: number }>(
      "/projects",
      PlatformApi.json({ name: "demo" })
    );

    expect(apiCall).toHaveBeenCalledWith({
      method: "POST",
      path: "/projects",
      actor: "signed-actor",
      body: { kind: "json", body: JSON.stringify({ name: "demo" }) },
    });
    expect(result).toEqual({ status: 201, body: { id: 9 }, ok: true });
  });

  it("fails closed when the caller has no actor token", () => {
    const caller = new AuthenticatedCaller({
      userId: 1,
      userEmail: "developer@example.com",
      role: "user",
      permissions: [],
      requestId: "request-1",
    });
    expect(() => new PlatformApi({} as TempsClient, caller)).toThrow(/no platform actor token/);
  });
});
