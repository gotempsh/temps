// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, spyOn, test } from "bun:test";
import { Command } from "commander";
import { config, credentials } from "../../config/store.js";
import { limitsLine, registerPluginControlCommands } from "./controls.js";
import type { ServicePlugin } from "./services.js";

const plugin = (overrides: Partial<ServicePlugin>): ServicePlugin => ({
  name: "reports",
  state: "running",
  stops_when_idle: false,
  tier: "community",
  own_storage: [],
  ...overrides,
});

test("a plugin's limits are said in megabytes and cores, with the deployment they belong to", () => {
  expect(
    limitsLine(
      plugin({
        resource_limits: {
          deployment_id: 41,
          memory_limit_mb: 512,
          cpu_limit_microcores: 500_000,
        },
      }),
    ),
  ).toBe(
    "Plugin reports runs with memory 512 MB, CPU 0.5 cores (deployment 41). Change them in the settings of its service.",
  );
});

test("a plugin without a memory limit says what that means", () => {
  const line = limitsLine(plugin({ resource_limits: { deployment_id: 41 } }));
  expect(line).toContain("memory not capped (it can use all the memory");
  expect(line).toContain("CPU not capped");
  expect(limitsLine(plugin({}))).toContain("no limits are recorded");
});

test("disabling and enabling a plugin are discoverable CLI commands", () => {
  const command = new Command("plugin");
  registerPluginControlCommands(command);
  expect(command.commands.map((c) => c.name())).toEqual([
    "disable",
    "enable",
    "limits",
  ]);
  expect(command.commands[0]?.description()).toContain("at once");
});

/** Answer every request with `response` and record what was asked. */
function server(response: () => Response) {
  const requests: Array<{ url: string; method: string }> = [];
  const apiKey = spyOn(credentials, "getApiKey").mockResolvedValue("test-key");
  const apiUrl = spyOn(config, "get").mockImplementation(
    () => "https://temps.invalid" as never,
  );
  const fetch = spyOn(globalThis, "fetch").mockImplementation((async (
    input,
    init,
  ) => {
    const request = new Request(input, init);
    requests.push({ url: request.url, method: request.method });
    return response();
  }) as typeof globalThis.fetch);
  const log = spyOn(console, "log").mockImplementation(() => {});
  return {
    requests,
    printed: () => log.mock.calls.map((call) => String(call[0])).join("\n"),
    restore() {
      log.mockRestore();
      fetch.mockRestore();
      apiUrl.mockRestore();
      apiKey.mockRestore();
    },
  };
}

function commands() {
  const command = new Command("plugin");
  command.exitOverride();
  registerPluginControlCommands(command);
  return command;
}

test("disabling and enabling each make one call and print what the server did", async () => {
  const api = server(() =>
    Response.json({
      name: "reports",
      disabled: true,
      message: "Plugin 'reports' is disabled: Temps sends it nothing.",
    }),
  );
  try {
    await commands().parseAsync(["disable", "reports"], { from: "user" });
    await commands().parseAsync(["enable", "reports"], { from: "user" });
    expect(api.requests).toEqual([
      {
        method: "POST",
        url: "https://temps.invalid/api/x/plugins/reports/disable",
      },
      {
        method: "POST",
        url: "https://temps.invalid/api/x/plugins/reports/enable",
      },
    ]);
    expect(api.printed()).toContain("Temps sends it nothing");
  } finally {
    api.restore();
  }
});

test("a refused switch reports the server's reason, and an unsafe name never reaches it", async () => {
  const api = server(
    () =>
      new Response(
        JSON.stringify({
          title: "Plugin Not Installed",
          detail: "No plugin named 'absent' is added.",
        }),
        {
          status: 404,
          headers: { "content-type": "application/problem+json" },
        },
      ),
  );
  try {
    await expect(
      commands().parseAsync(["disable", "absent"], { from: "user" }),
    ).rejects.toThrow(/No plugin named 'absent'/);
    await expect(
      commands().parseAsync(["enable", "absent"], { from: "user" }),
    ).rejects.toThrow(/No plugin named 'absent'/);
    const before = api.requests.length;
    await expect(
      commands().parseAsync(["disable", "../reload"], { from: "user" }),
    ).rejects.toThrow(/plugin name/);
    expect(api.requests).toHaveLength(before);
  } finally {
    api.restore();
  }
});

test("the limits of a plugin are read from the list, and an unknown plugin says so", async () => {
  const api = server(() =>
    Response.json({
      configured: true,
      retired: [],
      plugins: [
        plugin({
          resource_limits: { deployment_id: 41, memory_limit_mb: 512 },
        }),
      ],
    }),
  );
  try {
    await commands().parseAsync(["limits", "reports"], { from: "user" });
    expect(api.printed()).toContain("memory 512 MB, CPU not capped");
    await expect(
      commands().parseAsync(["limits", "absent"], { from: "user" }),
    ).rejects.toThrow(/No plugin named absent/);
    expect(api.requests.every(({ method }) => method === "GET")).toBe(true);
  } finally {
    api.restore();
  }
});
