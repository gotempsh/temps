// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, spyOn, test } from "bun:test";
import { Command } from "commander";
import { config, credentials } from "../../config/store.js";
import {
  imageDeployment,
  imageReference,
  pinnedByDigest,
  registerPluginImageCommands,
  serviceNameFor,
} from "./image.js";
import type { ServicePlugin } from "./services.js";

const IMAGE = "registry.example.com/acme/reports:1.2.0";
const DIGEST = `registry.example.com/acme/reports@sha256:${"a".repeat(64)}`;

test("an image is named by a reference without spaces", () => {
  expect(imageReference(` ${IMAGE} `)).toBe(IMAGE);
  for (const value of [undefined, "", "  ", "two words"])
    expect(() => imageReference(value)).toThrow(/--image/);
});

test("only a digest names exact content", () => {
  expect(pinnedByDigest(DIGEST)).toBe(true);
  expect(pinnedByDigest(IMAGE)).toBe(false);
  expect(pinnedByDigest("reports@sha256:abc")).toBe(false);
});

test("a service is named after the image's repository, without tag or digest", () => {
  expect(serviceNameFor(IMAGE)).toBe("reports");
  expect(serviceNameFor(DIGEST)).toBe("reports");
  expect(serviceNameFor("localhost:5000/Audit_Export")).toBe("audit-export");
  expect(serviceNameFor("reports")).toBe("reports");
  expect(() => serviceNameFor("registry.example.com/___:1")).toThrow(/--name/);
});

test("adding from an image is the first plugin command and says what it does", () => {
  const command = new Command("plugin");
  registerPluginImageCommands(command);
  expect(command.commands.map((c) => c.name())).toEqual(["add"]);
  const help = command.commands[0]?.helpInformation() ?? "";
  expect(help).toContain("--image <image>");
  expect(help).toContain("--project <project>");
  expect(command.commands[0]?.description()).toContain(
    "an update that waits for approval",
  );
});

const plugin = (overrides: Partial<ServicePlugin>): ServicePlugin => ({
  name: "reports",
  state: "running",
  stops_when_idle: false,
  tier: "community",
  own_storage: [],
  project_id: 7,
  environment_id: 9,
  pinned_deployment_id: 41,
  ...overrides,
});

const waiting = plugin({
  state: "update_waiting",
  pending_update: {
    deployment_id: 42,
    review: "review-42",
    difference: {
      version_from: "1.1.0",
      version_to: "1.2.0",
      added_permissions: [],
      removed_permissions: [],
      added_capabilities: [],
      added_public_paths: [],
      added_events: [],
      asks_for_more: false,
    },
  },
});

interface Fake {
  /** The service found by slug or id, if any. */
  existing?: boolean;
  /** What the plugin list answers, call after call; the last one repeats. */
  lists?: ServicePlugin[][];
  live?: boolean;
  refuseDeploy?: boolean;
}

/** A Temps that knows one service at most, and records every request. */
function server(fake: Fake) {
  const requests: Array<{ method: string; path: string; body: unknown }> = [];
  const apiKey = spyOn(credentials, "getApiKey").mockResolvedValue("test-key");
  const apiUrl = spyOn(config, "get").mockImplementation(
    () => "https://temps.invalid" as never,
  );
  const watched: number[] = [];
  const watch = spyOn(imageDeployment, "watch").mockImplementation(
    async ({ deploymentId }) => {
      watched.push(deploymentId);
      return fake.live === false
        ? { success: false, error: "the container exited" }
        : { success: true };
    },
  );
  let listed = 0;
  const project = { id: 7, name: "reports", slug: "reports" };
  const problem = (status: number, detail: string) =>
    new Response(JSON.stringify({ title: "Problem", detail }), {
      status,
      headers: { "content-type": "application/problem+json" },
    });
  const fetch = spyOn(globalThis, "fetch").mockImplementation((async (
    input,
    init,
  ) => {
    const request = new Request(input, init);
    const path = new URL(request.url).pathname.replace(/^\/api/, "");
    const body = request.method === "POST" ? await request.json() : null;
    requests.push({ method: request.method, path, body });
    if (request.method === "GET" && path === "/x/plugins/services") {
      const lists = fake.lists ?? [[]];
      const plugins = lists[Math.min(listed++, lists.length - 1)] ?? [];
      return Response.json({ configured: true, plugins, retired: [] });
    }
    if (
      request.method === "GET" &&
      /^\/projects\/(by-slug\/)?[^/]+$/.test(path)
    )
      return fake.existing
        ? Response.json(project)
        : problem(404, "No such project");
    if (request.method === "GET" && path === "/projects/7/environments")
      return Response.json([
        { id: 9, name: "production", is_preview: false },
        { id: 10, name: "staging", is_preview: false },
      ]);
    if (request.method === "POST" && path === "/projects")
      return Response.json(project, { status: 201 });
    if (request.method === "POST" && /\/deploy\/image$/.test(path))
      return fake.refuseDeploy
        ? problem(400, "The image reference is not valid")
        : Response.json(
            { id: 42, slug: "deploy-42", project_id: 7, environment_id: 9 },
            { status: 202 },
          );
    if (request.method === "POST" && path === "/x/plugins/services")
      return Response.json(
        { name: "reports", version: "1.2.0", project_id: 7, environment_id: 9 },
        { status: 201 },
      );
    return problem(404, `Unexpected ${request.method} ${path}`);
  }) as typeof globalThis.fetch);
  const log = spyOn(console, "log").mockImplementation(() => {});
  return {
    requests,
    watched,
    posts: () =>
      requests
        .filter(({ method }) => method === "POST")
        .map(({ path }) => path),
    printed: () => log.mock.calls.map((call) => String(call[0])).join("\n"),
    restore() {
      log.mockRestore();
      fetch.mockRestore();
      watch.mockRestore();
      apiUrl.mockRestore();
      apiKey.mockRestore();
    },
  };
}

function run(args: string[]) {
  const command = new Command("plugin");
  command.exitOverride();
  registerPluginImageCommands(command);
  return command.parseAsync(["add", ...args], { from: "user" });
}

test("an image becomes a new service, is deployed like any image and is added as a plugin", async () => {
  const api = server({});
  try {
    await run(["--image", IMAGE]);
    expect(api.posts()).toEqual([
      "/projects",
      "/projects/7/environments/9/deploy/image",
      "/x/plugins/services",
    ]);
    const [created, deployed, added] = api.requests.filter(
      ({ method }) => method === "POST",
    );
    expect(created?.body).toMatchObject({
      name: "reports",
      source_type: "docker_image",
      exposed_port: 8080,
    });
    expect(deployed?.body).toEqual({
      image_ref: IMAGE,
      health_check_path: "/health",
    });
    expect(added?.body).toEqual({ project_id: 7, environment_id: 9 });
    expect(api.watched).toEqual([42]);
    const printed = api.printed();
    expect(printed).toContain("Created the service reports.");
    expect(printed).toContain("named by a tag");
    expect(printed).toContain("pinned to deployment 42");
    expect(printed).toContain("It has no permissions yet");
  } finally {
    api.restore();
  }
});

test("an image named by digest is not warned about", async () => {
  const api = server({});
  try {
    await run(["--image", DIGEST, "--name", "reports"]);
    expect(api.printed()).not.toContain("named by a tag");
  } finally {
    api.restore();
  }
});

test("an image is never deployed over a service that only shares its name", async () => {
  const api = server({ existing: true });
  try {
    await expect(run(["--image", IMAGE])).rejects.toThrow(
      /already exists.*--project reports/,
    );
    expect(api.posts()).toEqual([]);
  } finally {
    api.restore();
  }
});

test("an image deployed to a service that is a plugin is an update that waits", async () => {
  const api = server({
    existing: true,
    lists: [[plugin({})], [plugin({})], [waiting]],
  });
  try {
    await run(["--image", IMAGE, "--project", "reports"]);
    // Deployed, and nothing else: no service is created and none is added.
    expect(api.posts()).toEqual(["/projects/7/environments/9/deploy/image"]);
    const printed = api.printed();
    expect(printed).toContain("Deployment 42");
    expect(printed).toContain("waits for approval");
    expect(printed).toContain(
      "temps plugin approve-update reports --deployment 42",
    );
  } finally {
    api.restore();
  }
});

test("a deployment that does not become live adds nothing and says how to try again", async () => {
  const api = server({ live: false });
  try {
    await expect(run(["--image", IMAGE])).rejects.toThrow(
      /did not become live: the container exited.*Nothing was added.*--project reports/s,
    );
    expect(api.posts()).not.toContain("/x/plugins/services");
  } finally {
    api.restore();
  }
});

test("a refused image, an unknown service or environment and bad options stop before anything is added", async () => {
  const refused = server({ refuseDeploy: true });
  try {
    await expect(run(["--image", IMAGE])).rejects.toThrow(
      /could not be deployed to reports: .*not valid/,
    );
    expect(refused.watched).toEqual([]);
  } finally {
    refused.restore();
  }
  const api = server({});
  try {
    await expect(
      run(["--image", IMAGE, "--project", "absent"]),
    ).rejects.toThrow(/No service absent was found/);
    await expect(
      run(["--image", IMAGE, "--environment", "preview"]),
    ).rejects.toThrow(
      /no environment named preview. It has: production, staging/,
    );
    const before = api.requests.length;
    await expect(run(["--image", "two words"])).rejects.toThrow(/--image/);
    await expect(
      run(["--image", IMAGE, "--health-check-path", "health"]),
    ).rejects.toThrow(/must start with/);
    await expect(run(["--image", IMAGE, "--timeout", "soon"])).rejects.toThrow(
      /number of seconds/,
    );
    expect(api.requests).toHaveLength(before);
    await expect(run(["--image", IMAGE, "--port", "0"])).rejects.toThrow(
      /between 1 and 65535/,
    );
    expect(api.posts()).not.toContain("/x/plugins/services");
  } finally {
    api.restore();
  }
});
