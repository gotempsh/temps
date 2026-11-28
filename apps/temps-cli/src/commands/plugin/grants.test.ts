// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, spyOn, test } from "bun:test";
import { Command } from "commander";
import { config, credentials } from "../../config/store.js";
import {
  grantReplacement,
  pluginName,
  registerPluginGrantCommands,
  requestedGrants,
} from "./grants.js";

test("host permissions require explicit flags and preserve zero daily calls", () => {
  expect(requestedGrants({})).toBeUndefined();
  expect(
    requestedGrants({
      grant: ["ai_generate", "ai_generate"],
      aiDailyCalls: "0",
      aiMaxTokens: "512",
    }),
  ).toEqual({
    permissions: ["ai_generate"],
    ai_daily_call_limit: 0,
    ai_max_output_tokens: 512,
    write_areas: [],
    write_project_ids: null,
  });
});
test.each([
  { grant: ["system_admin"] },
  { aiDailyCalls: "100" },
  { grant: ["ai_generate"], aiDailyCalls: "-1" },
  { grant: ["ai_generate"], aiDailyCalls: "1.5" },
  { grant: ["ai_generate"], aiDailyCalls: "10001" },
  { grant: ["ai_generate"], aiMaxTokens: "0" },
  { grant: ["ai_generate"], aiMaxTokens: "4097" },
])("rejects invalid or implicit AI grants %j", (options) => {
  expect(() => requestedGrants(options)).toThrow();
});

test("a permission to change reaches nothing until areas are named", () => {
  expect(requestedGrants({ grant: ["api_write"] })).toMatchObject({
    permissions: ["api_write"],
    write_areas: [],
    write_project_ids: null,
  });
  expect(
    requestedGrants({
      grant: ["api_write"],
      write: ["deployments", "deployments", "environments"],
      writeProject: ["7", "9", "7"],
    }),
  ).toMatchObject({
    write_areas: ["deployments", "environments"],
    write_project_ids: [7, 9],
  });
});
test.each([
  // Where a plugin may change things means nothing without the permission.
  { write: ["deployments"] },
  { grant: ["api_read"], write: ["deployments"] },
  { grant: ["api_read"], writeProject: ["7"] },
  { grant: ["api_write"], write: ["databases"] },
  { grant: ["api_write"], writeProject: [] },
  { grant: ["api_write"], writeProject: ["0"] },
  { grant: ["api_write"], writeProject: ["-3"] },
  { grant: ["api_write"], writeProject: ["1.5"] },
  { grant: ["api_write"], writeProject: ["web"] },
  {
    grant: ["api_write"],
    writeProject: Array.from({ length: 101 }, (_, index) => String(index + 1)),
  },
])("rejects a scope that is not valid %j", (options) => {
  expect(() => requestedGrants(options)).toThrow(/--write/);
});
test("a plugin name that could change the request path is refused", () => {
  expect(pluginName("reports")).toBe("reports");
  for (const name of ["", "Reports", "../reload", "a/b", "-reports", "a".repeat(65)])
    expect(() => pluginName(name)).toThrow(/plugin name/);
});


test("explicit clear revokes all permissions and disables AI quota", () => {
  expect(grantReplacement({ clear: true })).toEqual({
    permissions: [],
    ai_daily_call_limit: 0,
    ai_max_output_tokens: 1024,
    write_areas: [],
    write_project_ids: null,
  });
});
test("replacement requires explicit authority and rejects ambiguous clearing", () => {
  for (const options of [
    {},
    { clear: true, grant: ["ai_generate"] },
    { clear: true, aiDailyCalls: "0" },
    { clear: true, write: ["deployments"] },
    { clear: true, writeProject: ["7"] },
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
    write_areas: [],
    write_project_ids: null,
  });
});
test("grant inspection and replacement are discoverable CLI commands", () => {
  const command = new Command("plugin");
  registerPluginGrantCommands(command);
  expect(command.commands[0]?.commands.map((c) => c.name())).toEqual([
    "get",
    "set",
  ]);
  const help = command.commands[0]?.commands[1]?.helpInformation();
  expect(help).toContain("--clear");
  expect(help).toContain("--write <areas...>");
  expect(help).toContain("--write-project <ids...>");
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
      write_areas: [],
      public_paths: [],
      events: [],
      outbound_network: {
        profile: "open",
        detail: "",
        settings_path: "/settings/network-policy",
      },
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
    await command.parseAsync(
      [
        "grants",
        "set",
        "sample",
        "--grant",
        "api_write",
        "--write",
        "deployments",
        "--write-project",
        "7",
      ],
      { from: "user" },
    );

    expect(requests).toHaveLength(3);
    expect(requests.map(({ url, method }) => [method, url])).toEqual([
      ["GET", "https://temps.invalid/api/x/plugins/sample/grants"],
      ["PUT", "https://temps.invalid/api/x/plugins/sample/grants"],
      ["PUT", "https://temps.invalid/api/x/plugins/sample/grants"],
    ]);
    expect(requests[2]?.body).toEqual({
      permissions: ["api_write"],
      ai_daily_call_limit: 100,
      ai_max_output_tokens: 1024,
      write_areas: ["deployments"],
      write_project_ids: [7],
    });
    expect(requests[0]?.headers.get("Authorization")).toBe("Bearer test-key");
    expect(requests[1]?.headers.get("Authorization")).toBe("Bearer test-key");
    expect(requests[1]?.headers.get("Content-Type")).toContain(
      "application/json",
    );
    expect(requests[1]?.body).toEqual({
      permissions: [],
      ai_daily_call_limit: 0,
      ai_max_output_tokens: 1024,
      write_areas: [],
      write_project_ids: null,
    });
  } finally {
    log.mockRestore();
    fetch.mockRestore();
    apiUrl.mockRestore();
    apiKey.mockRestore();
  }
});
