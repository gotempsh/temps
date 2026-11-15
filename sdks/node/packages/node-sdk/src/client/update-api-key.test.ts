// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it, vi } from "vitest";
import { createClient, createConfig } from "./client";
import { updateApiKey } from "./sdk.gen";

describe("updateApiKey", () => {
  // The server routes `/api-keys/{id}` as PATCH only; a PUT is refused with 405.
  it("sends the update as PATCH to the API key's path", async () => {
    const fetchMock = vi.fn(
      async (_request: Request) =>
        new Response(JSON.stringify({ id: 7, name: "renamed" }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }),
    );
    const client = createClient(
      createConfig({
        baseUrl: "https://api.test.com",
        fetch: fetchMock as unknown as typeof fetch,
      }),
    );

    await updateApiKey({
      client,
      path: { id: 7 },
      body: { name: "renamed" },
    });

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const request = fetchMock.mock.calls[0][0];
    expect(request.method).toBe("PATCH");
    expect(new URL(request.url).pathname).toBe("/api-keys/7");
  });
});
