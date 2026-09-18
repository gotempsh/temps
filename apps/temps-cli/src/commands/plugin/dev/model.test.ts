import { describe, test, expect } from "bun:test";
import {
  createEvent,
  eventTypes,
  grants,
  integer,
  manifest,
  subscribes,
  validateEvent,
} from "./model.js";
import { MockHost, parseFixtures } from "./host.js";
import { sessionPaths } from "./session.js";
describe("event contract and inputs", () => {
  for (const type of eventTypes)
    test(type, () => {
      const event = createEvent(type, {});
      expect(validateEvent(JSON.parse(JSON.stringify(event)))).toEqual(event);
      expect(event.project_id).toBe(1);
      expect(event.data.project_id).toBe(1);
    });
  test("deployment success matches Rust event mapping", () => {
    expect(
      createEvent("deployment.succeeded", {
        deploymentId: "99",
        url: "https://example.com",
      }).data,
    ).toEqual({
      project_id: 1,
      deployment_id: 99,
      environment_id: 1,
      environment_name: "production",
      commit_sha: null,
      url: "https://example.com",
    });
  });
  test("invalid envelopes and mismatched project IDs fail", () => {
    expect(() => validateEvent({})).toThrow();
    const event = createEvent("deployment.succeeded", {});
    expect(() => validateEvent({ ...event, project_id: 9 })).toThrow("match");
    expect(() => validateEvent({ ...event, timestamp: "bad" })).toThrow();
    expect(() => createEvent("deployment.typo", {})).toThrow("Unknown");
    expect(() =>
      validateEvent({ ...event, data: { ...event.data, url: 12 } }),
    ).toThrow("url");
  });
  test("subscriptions match exactly like host", () => {
    expect(subscribes(["deployment.*"], "deployment.succeeded")).toBe(true);
    expect(subscribes(["deployment.*"], "project.created")).toBe(false);
    expect(subscribes([], "deployment.succeeded")).toBe(false);
    expect(subscribes(["*"], "project.created")).toBe(true);
  });
  test("limits and grants fail closed", () => {
    for (const value of ["1x", "-1", "1.5", NaN, Infinity, true])
      expect(() => integer(value, "test")).toThrow();
    expect(() => grants(["unknown"])).toThrow();
    expect(grants(["events_read", "events_read"])).toEqual(["events_read"]);
    expect(() => sessionPaths("../escape")).toThrow();
    expect(() =>
      manifest({
        name: "x",
        version: "1",
        health_path: "/_events",
        events: [],
      }),
    ).toThrow();
    expect(() =>
      manifest({
        name: "x",
        version: "1",
        health_path: "/health",
        events: [],
        requires_db: true,
      }),
    ).toThrow("database");
  });
});
describe("mock host", () => {
  test("discovery works without grants and reads fail closed", async () => {
    const host = new MockHost("example", "actor", []);
    expect(await host.call("get_host_capabilities", {})).toMatchObject({
      permissions: [],
      ai: { configured: false },
    });
    await expect(
      host.call("get_project", { project_id: 1 }),
    ).rejects.toMatchObject({ code: "permission_denied" });
    await expect(host.call("api_call", {})).rejects.toMatchObject({
      code: "method_not_found",
    });
  });
  test("read fixtures, validation, missing records and live revocation", async () => {
    const host = new MockHost("example", "actor", [
      "projects_read",
      "environments_read",
      "deployments_read",
    ]);
    expect(await host.call("get_project", { project_id: 1 })).toMatchObject({
      name: "demo",
    });
    expect(
      await host.call("list_environments", { project_id: 1 }),
    ).toHaveLength(1);
    expect(
      await host.call("get_last_deployment", { project_id: 1 }),
    ).toMatchObject({ id: 42 });
    await await expect(
      host.call("get_project", { project_id: 5 }),
    ).rejects.toMatchObject({ code: "not_found" });
    await await expect(
      host.call("get_project", { project_id: -1 }),
    ).rejects.toMatchObject({ code: "invalid_params" });
    host.permissions = [];
    await await expect(
      host.call("get_project", { project_id: 1 }),
    ).rejects.toMatchObject({ code: "permission_denied" });
  });
  test("fixture validation checks shape and references", () => {
    expect(() => parseFixtures({ version: 2 })).toThrow();
    expect(() => parseFixtures({ projects: [] })).toThrow("project_id");
    expect(() =>
      parseFixtures({
        deployments: [
          {
            id: 1,
            project_id: 1,
            environment_id: 8,
            state: "deployed",
            created_at: "x",
          },
        ],
      }),
    ).toThrow("environment_id");
    expect(() => parseFixtures({ ai: { concurrency: 0 } })).toThrow();
    expect(() => parseFixtures({ ai: { text: "x".repeat(65537) } })).toThrow();
  });
  test("concurrency and quota admission are atomic", async () => {
    const host = new MockHost(
      "example",
      "actor",
      ["ai_generate"],
      parseFixtures({
        ai: { mode: "success", delayMs: 20, dailyCallLimit: 2, concurrency: 1 },
      }),
    );
    const input = { purpose: "test", prompt: "test" };
    const results = await Promise.allSettled(
      Array.from({ length: 5 }, () => host.call("generate_ai", input)),
    );
    expect(results.filter((r) => r.status === "fulfilled")).toHaveLength(1);
    await host.call("generate_ai", input);
    await expect(host.call("generate_ai", input)).rejects.toMatchObject({
      code: "permission_denied",
    });
  });
  for (const mode of ["error", "timeout"])
    test(`AI ${mode} releases slots`, async () => {
      const host = new MockHost(
        "example",
        "actor",
        ["ai_generate"],
        parseFixtures({ ai: { mode, concurrency: 1 } }),
      );
      for (let i = 0; i < 2; i++)
        await expect(
          host.call("generate_ai", { purpose: "test", prompt: "test" }),
        ).rejects.toMatchObject({ code: "internal" });
    });
  test("AI inputs bounded before provider simulation", async () => {
    const host = new MockHost("example", "actor", ["ai_generate"]);
    await expect(
      host.call("generate_ai", { purpose: "test", prompt: "x".repeat(65537) }),
    ).rejects.toMatchObject({ code: "invalid_params" });
    await expect(
      host.call("generate_ai", {
        purpose: "test",
        prompt: "test",
        temperature: 3,
      }),
    ).rejects.toMatchObject({ code: "invalid_params" });
    await expect(
      host.call("generate_ai", { purpose: "test", prompt: "test" }),
    ).rejects.toMatchObject({ code: "internal" });
  });
});
