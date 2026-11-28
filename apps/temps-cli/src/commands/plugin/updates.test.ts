// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, spyOn, test } from "bun:test";
import { Command } from "commander";
import { config, credentials } from "../../config/store.js";
import type { ServicePlugin } from "./services.js";
import {
  registerPluginUpdateCommands,
  reviewedDeployment,
  updateLines,
} from "./updates.js";

const plugin = (overrides: Partial<ServicePlugin>): ServicePlugin => ({
  name: "reports",
  state: "running",
  stops_when_idle: false,
  tier: "community",
  own_storage: [],
  pinned_deployment_id: 41,
  ...overrides,
});

const waiting = (
  difference: Partial<
    NonNullable<ServicePlugin["pending_update"]>["difference"]
  > = {},
): ServicePlugin =>
  plugin({
    state: "update_waiting",
    pending_update: {
      deployment_id: 42,
      review: "review-42",
      difference: {
        version_from: "1.0.0",
        version_to: "1.1.0",
        added_permissions: [],
        removed_permissions: [],
        added_capabilities: [],
        added_public_paths: [],
        added_events: [],
        asks_for_more: false,
        ...difference,
      },
    },
  });

test("the deployment that was reviewed is named by a positive numeric id", () => {
  expect(reviewedDeployment("42")).toBe(42);
  for (const value of [undefined, "", "0", "-1", "latest", "1.5"])
    expect(() => reviewedDeployment(value)).toThrow(/deployment you reviewed/);
});

test("a plugin on its approved build says so and that the next deployment will wait", () => {
  const lines = updateLines(plugin({})).join("\n");
  expect(lines).toContain("runs the build that was approved (deployment 41)");
  expect(lines).toContain("will wait for approval");
});

test("a plugin deployed from an image names the approved image and the one that waits", () => {
  const held = waiting();
  const lines = updateLines({
    ...held,
    image: "registry.example.com/acme/reports:1.0.0",
    pending_update: {
      ...held.pending_update!,
      image: "registry.example.com/acme/reports:1.1.0",
    },
  }).join("\n");
  expect(lines).toContain(
    "pinned to deployment 41, image registry.example.com/acme/reports:1.0.0",
  );
  expect(lines).toContain(
    "Deployment 42 (version 1.0.0 -> 1.1.0, image registry.example.com/acme/reports:1.1.0) waits",
  );
});

test("a waiting update that asks for nothing more is still called new code", () => {
  const lines = updateLines(waiting()).join("\n");
  expect(lines).toContain("is pinned to deployment 41");
  expect(lines).toContain(
    "Deployment 42 (version 1.0.0 -> 1.1.0) waits for approval",
  );
  expect(lines).toContain("asks for no new access");
  expect(lines).toContain(
    "temps plugin approve-update reports --deployment 42",
  );
  expect(lines).toContain("roll the service back");
});

test("a waiting update that asks for more names each kind and that approving allows none", () => {
  const lines = updateLines(
    waiting({
      added_permissions: ["api_write"],
      added_capabilities: ["api_write"],
      added_public_paths: ["/export/*"],
      added_events: ["project.deleted"],
      removed_permissions: ["events_read"],
      asks_for_more: true,
    }),
  ).join("\n");
  expect(lines).toContain("New permissions it asks for: api_write");
  expect(lines).toContain("New kinds of API calls: api_write");
  expect(lines).toContain("New routes open without signing in: /export/*");
  expect(lines).toContain("New events it listens for: project.deleted");
  expect(lines).toContain("Approving does not allow new permissions");
  expect(lines).toContain("No longer asks for: events_read");
  expect(lines).not.toContain("asks for no new access");
});

test("showing and approving an update are discoverable CLI commands", () => {
  const command = new Command("plugin");
  registerPluginUpdateCommands(command);
  expect(command.commands.map((c) => c.name())).toEqual([
    "update",
    "approve-update",
  ]);
  expect(command.commands[1]?.description()).toContain(
    "never applied without this",
  );
  expect(command.commands[1]?.helpInformation()).toContain("--deployment <id>");
});

/** Serve `plugins` to the list request and record every request made. */
function server(plugins: ServicePlugin[], approval?: Response) {
  const requests: Array<{ url: string; method: string; body: unknown }> = [];
  const apiKey = spyOn(credentials, "getApiKey").mockResolvedValue("test-key");
  const apiUrl = spyOn(config, "get").mockImplementation(
    () => "https://temps.invalid" as never,
  );
  const fetch = spyOn(globalThis, "fetch").mockImplementation((async (
    input,
    init,
  ) => {
    const request = new Request(input, init);
    requests.push({
      url: request.url,
      method: request.method,
      body: request.method === "POST" ? await request.json() : null,
    });
    if (request.method === "POST")
      return (
        approval ??
        Response.json({
          name: "reports",
          version: "1.1.0",
          deployment_id: 42,
          difference: { asks_for_more: true, added_permissions: ["api_write"] },
        })
      );
    return Response.json({ configured: true, plugins, retired: [] });
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
  registerPluginUpdateCommands(command);
  return command;
}

test("approving sends the review of the deployment that was named, and says what is still off", async () => {
  const api = server([waiting({ added_permissions: ["api_write"] })]);
  try {
    await commands().parseAsync(
      ["approve-update", "reports", "--deployment", "42"],
      { from: "user" },
    );
    expect(api.requests.map(({ method, url }) => [method, url])).toEqual([
      ["GET", "https://temps.invalid/api/x/plugins/services"],
      ["POST", "https://temps.invalid/api/x/plugins/reports/update/approve"],
    ]);
    expect(api.requests[1]?.body).toEqual({ review: "review-42" });
    expect(api.printed()).toContain("now runs deployment 42");
    expect(api.printed()).toContain("still off: api_write");
  } finally {
    api.restore();
  }
});

test("nothing is approved when another deployment waits, none waits, or the plugin is unknown", async () => {
  const api = server([waiting(), plugin({ name: "audit-export" })]);
  try {
    await expect(
      commands().parseAsync(
        ["approve-update", "reports", "--deployment", "40"],
        { from: "user" },
      ),
    ).rejects.toThrow(/that waits is deployment 42, not 40/);
    await expect(
      commands().parseAsync(
        ["approve-update", "audit-export", "--deployment", "42"],
        { from: "user" },
      ),
    ).rejects.toThrow(/No update of plugin audit-export waits/);
    await expect(
      commands().parseAsync(
        ["approve-update", "absent", "--deployment", "42"],
        { from: "user" },
      ),
    ).rejects.toThrow(/No plugin named absent/);
    await expect(
      commands().parseAsync(
        ["approve-update", "reports", "--deployment", "latest"],
        { from: "user" },
      ),
    ).rejects.toThrow(/deployment you reviewed/);
    expect(api.requests.every(({ method }) => method === "GET")).toBe(true);
    // An unsafe name never reaches the server.
    const before = api.requests.length;
    await expect(
      commands().parseAsync(
        ["approve-update", "../reload", "--deployment", "42"],
        { from: "user" },
      ),
    ).rejects.toThrow(/plugin name/);
    expect(api.requests).toHaveLength(before);
  } finally {
    api.restore();
  }
});

test("a refused approval reports the server's reason", async () => {
  const api = server(
    [waiting()],
    new Response(
      JSON.stringify({
        title: "Plugin Update Changed",
        detail:
          "The update of plugin 'reports' that waits is now deployment 43, which is not the one that was reviewed.",
      }),
      { status: 409, headers: { "content-type": "application/problem+json" } },
    ),
  );
  try {
    await expect(
      commands().parseAsync(
        ["approve-update", "reports", "--deployment", "42"],
        { from: "user" },
      ),
    ).rejects.toThrow(/now deployment 43/);
  } finally {
    api.restore();
  }
});

test("showing an update prints what waits, or the record as JSON", async () => {
  const api = server([waiting()]);
  try {
    await commands().parseAsync(["update", "reports"], { from: "user" });
    expect(api.printed()).toContain("Deployment 42");
    await commands().parseAsync(["update", "reports", "--json"], {
      from: "user",
    });
    expect(api.printed()).toContain('"review": "review-42"');
    expect(api.requests.every(({ method }) => method === "GET")).toBe(true);
  } finally {
    api.restore();
  }
});
