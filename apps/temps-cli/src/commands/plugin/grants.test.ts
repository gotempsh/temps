// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, spyOn, test } from "bun:test";
import { Command } from "commander";
import { config, credentials } from "../../config/store.js";
import { grantReplacement, registerPluginGrantCommands } from "./grants.js";

test("explicit clear revokes all permissions and disables AI quota", () => {
  expect(grantReplacement({ clear: true })).toEqual({
    permissions: [],
    ai_daily_call_limit: 0,
    ai_max_output_tokens: 1024,
  });
});
test("replacement requires explicit authority and rejects ambiguous clearing", () => {
  for (const options of [
    {},
    { clear: true, grant: ["ai_generate"] },
    { clear: true, aiDailyCalls: "0" },
    { grant: ["system_admin"] },
  ])
    expect(() => grantReplacement(options)).toThrow();
});
test("grant replacement preserves zero quota and explicit token bound", () => {
  expect(
    grantReplacement({
      grant: ["ai_generate"],
      aiDailyCalls: "0",
      aiMaxTokens: "32",
    }),
  ).toEqual({
    permissions: ["ai_generate"],
    ai_daily_call_limit: 0,
    ai_max_output_tokens: 32,
  });
});
test("grant inspection and replacement are discoverable CLI commands", () => {
  const command = new Command("plugin");
  registerPluginGrantCommands(command);
  expect(command.commands[0]?.commands.map((c) => c.name())).toEqual([
    "get",
    "set",
  ]);
  expect(command.commands[0]?.commands[1]?.helpInformation()).toContain(
    "--clear",
  );
});

test("grant commands use the shared authenticated client for GET and PUT", async () => {
  const apiKey = spyOn(credentials, "getApiKey").mockResolvedValue("test-key");
  const apiUrl = spyOn(config, "get").mockImplementation(
    () => "https://temps.invalid" as never,
  );
  const requests: Array<{
    url: string;
    method: string;
    headers: Headers;
    body: unknown;
  }> = [];
  const fetch = spyOn(globalThis, "fetch").mockImplementation((async (
    input,
    init,
  ) => {
    const request = new Request(input, init);
    requests.push({
      url: request.url,
      method: request.method,
      headers: request.headers,
      body: request.method === "PUT" ? await request.json() : null,
    });
    return Response.json({
      actor: { id: "plugin:sample", name: "sample", active: true },
      ai: {
        configured: true,
        daily_call_limit: 0,
        max_output_tokens: 1024,
        max_prompt_bytes: 0,
        setup_path: "",
      },
      permissions: [],
      requested_permissions: [],
    });
  }) as typeof globalThis.fetch);
  const log = spyOn(console, "log").mockImplementation(() => {});
  try {
    const command = new Command("plugin");
    registerPluginGrantCommands(command);
    await command.parseAsync(["grants", "get", "sample"], { from: "user" });
    await command.parseAsync(["grants", "set", "sample", "--clear"], {
      from: "user",
    });

    expect(requests).toHaveLength(2);
    expect(requests.map(({ url, method }) => [method, url])).toEqual([
      ["GET", "https://temps.invalid/api/x/plugins/sample/grants"],
      ["PUT", "https://temps.invalid/api/x/plugins/sample/grants"],
    ]);
    expect(requests[0]?.headers.get("Authorization")).toBe("Bearer test-key");
    expect(requests[1]?.headers.get("Authorization")).toBe("Bearer test-key");
    expect(requests[1]?.headers.get("Content-Type")).toContain(
      "application/json",
    );
    expect(requests[1]?.body).toEqual({
      permissions: [],
      ai_daily_call_limit: 0,
      ai_max_output_tokens: 1024,
    });
  } finally {
    log.mockRestore();
    fetch.mockRestore();
    apiUrl.mockRestore();
    apiKey.mockRestore();
  }
});
