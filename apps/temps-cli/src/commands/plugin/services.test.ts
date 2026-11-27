// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, spyOn, test } from "bun:test";
import { Command } from "commander";
import { config, credentials } from "../../config/store.js";
import { registerPluginServiceCommands, serviceId } from "./services.js";

test("a service or environment is named by a positive numeric id", () => {
  expect(serviceId("7", "service")).toBe(7);
  for (const value of [undefined, "", "0", "-1", "reports", "1.5", "7 "])
    expect(() => serviceId(value, "service")).toThrow(/numeric id/);
});

test("listing and adding service plugins are discoverable CLI commands", () => {
  const command = new Command("plugin");
  registerPluginServiceCommands(command);
  expect(command.commands.map((c) => c.name())).toEqual([
    "services",
    "add-service",
  ]);
  const help = command.commands[1]?.helpInformation() ?? "";
  expect(help).toContain("--project <id>");
  expect(help).toContain("--environment <id>");
  expect(help).toContain("no permissions");
});

test("service plugin commands use the shared authenticated client", async () => {
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
      body: request.method === "POST" ? await request.json() : null,
    });
    return request.method === "POST"
      ? Response.json(
          {
            name: "reports",
            version: "1.0.0",
            project_id: 7,
            environment_id: 9,
          },
          { status: 201 },
        )
      : Response.json({ configured: true, plugins: [] });
  }) as typeof globalThis.fetch);
  const log = spyOn(console, "log").mockImplementation(() => {});
  try {
    const command = new Command("plugin");
    registerPluginServiceCommands(command);
    await command.parseAsync(["services"], { from: "user" });
    await command.parseAsync(
      ["add-service", "--project", "7", "--environment", "9"],
      { from: "user" },
    );

    expect(requests.map(({ url, method }) => [method, url])).toEqual([
      ["GET", "https://temps.invalid/api/x/plugins/services"],
      ["POST", "https://temps.invalid/api/x/plugins/services"],
    ]);
    expect(requests[1]?.headers.get("Authorization")).toBe("Bearer test-key");
    expect(requests[1]?.body).toEqual({ project_id: 7, environment_id: 9 });
    expect(log.mock.calls.at(-1)?.[0]).toContain('"name": "reports"');
  } finally {
    log.mockRestore();
    fetch.mockRestore();
    apiUrl.mockRestore();
    apiKey.mockRestore();
  }
});

test("a refusal reports the server's reason and an invalid id never calls the server", async () => {
  const apiKey = spyOn(credentials, "getApiKey").mockResolvedValue("test-key");
  const apiUrl = spyOn(config, "get").mockImplementation(
    () => "https://temps.invalid" as never,
  );
  let calls = 0;
  const fetch = spyOn(globalThis, "fetch").mockImplementation((async () => {
    calls += 1;
    return new Response(
      JSON.stringify({
        title: "Service Is Not A Plugin",
        detail: "Service 'web' is not a plugin: it does not answer",
      }),
      { status: 400, headers: { "content-type": "application/problem+json" } },
    );
  }) as unknown as typeof globalThis.fetch);
  try {
    const command = new Command("plugin");
    command.exitOverride();
    registerPluginServiceCommands(command);
    await expect(
      command.parseAsync(
        ["add-service", "--project", "web", "--environment", "9"],
        { from: "user" },
      ),
    ).rejects.toThrow(/numeric id/);
    expect(calls).toBe(0);
    await expect(
      command.parseAsync(
        ["add-service", "--project", "7", "--environment", "9"],
        { from: "user" },
      ),
    ).rejects.toThrow(/is not a plugin/);
    expect(calls).toBe(1);
  } finally {
    fetch.mockRestore();
    apiUrl.mockRestore();
    apiKey.mockRestore();
  }
});
