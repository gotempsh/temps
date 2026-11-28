// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, spyOn, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Command } from "commander";
import { config, credentials } from "../../config/store.js";
import {
  imageDeployment,
  imageReference,
  imageSource,
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
  expect(help).toContain("--local-image <image>");
  expect(help).toContain("--build");
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
  /** Images the Docker on this machine has. */
  localImages?: string[];
  /** The upload says why it failed and starts nothing. */
  refuseUpload?: boolean;
  failBuild?: boolean;
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
  const uploads: Array<Parameters<typeof imageDeployment.upload>[0]> = [];
  const upload = spyOn(imageDeployment, "upload").mockImplementation(
    async (options) => {
      uploads.push(options);
      return fake.refuseUpload ? undefined : { id: 43, slug: "deploy-43" };
    },
  );
  const builds: Array<Parameters<typeof imageDeployment.build>[0]> = [];
  const build = spyOn(imageDeployment, "build").mockImplementation(
    async (options) => {
      builds.push(options);
      if (fake.failBuild) throw new Error("docker build failed with code 1");
    },
  );
  const exists = spyOn(imageDeployment, "exists").mockImplementation(
    async (image) => (fake.localImages ?? []).includes(image),
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
    uploads,
    builds,
    posts: () =>
      requests
        .filter(({ method }) => method === "POST")
        .map(({ path }) => path),
    printed: () => log.mock.calls.map((call) => String(call[0])).join("\n"),
    restore() {
      log.mockRestore();
      fetch.mockRestore();
      exists.mockRestore();
      build.mockRestore();
      upload.mockRestore();
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

test("an image comes from exactly one place", () => {
  expect(imageSource({ image: IMAGE })).toEqual({
    kind: "registry",
    image: IMAGE,
  });
  expect(imageSource({ localImage: "reports:dev" })).toEqual({
    kind: "local",
    image: "reports:dev",
  });
  const built = imageSource({ build: true, context: "/work/reports" });
  expect(built).toEqual({
    kind: "build",
    context: "/work/reports",
    dockerfile: "/work/reports/Dockerfile",
  });
  expect(
    imageSource({
      build: true,
      context: "/work/reports",
      dockerfile: "/work/Dockerfile.plugin",
    }),
  ).toMatchObject({ dockerfile: "/work/Dockerfile.plugin" });
  for (const options of [
    {},
    { image: IMAGE, localImage: "reports:dev" },
    { image: IMAGE, build: true },
    { localImage: "reports:dev", build: true },
  ])
    expect(() => imageSource(options)).toThrow(/exactly one of/);
  expect(() => imageSource({ image: IMAGE, context: "." })).toThrow(/--build/);
  expect(() => imageSource({ localImage: "two words" })).toThrow(
    /--local-image/,
  );
});

test("an image in your Docker is uploaded, deployed and added, with no registry", async () => {
  const api = server({ localImages: ["reports:dev"] });
  try {
    await run(["--local-image", "reports:dev"]);
    // The upload starts the deployment, so nothing is posted to deploy/image.
    expect(api.posts()).toEqual(["/projects", "/x/plugins/services"]);
    expect(api.uploads).toEqual([
      {
        imageName: "reports:dev",
        projectId: 7,
        environmentId: 9,
        healthCheckPath: "/health",
        timeoutSecs: 300,
      },
    ]);
    expect(api.builds).toEqual([]);
    expect(api.watched).toEqual([43]);
    const printed = api.printed();
    expect(printed).toContain("pinned to deployment 43");
    // What was uploaded is the content itself: there is no tag to move.
    expect(printed).not.toContain("named by a tag");
  } finally {
    api.restore();
  }
});

test("an image your Docker does not have stops before anything is created", async () => {
  const api = server({ localImages: [] });
  try {
    await expect(run(["--local-image", "reports:dev"])).rejects.toThrow(
      /Your Docker has no image reports:dev.*--build/,
    );
    expect(api.requests).toEqual([]);
    expect(api.uploads).toEqual([]);
  } finally {
    api.restore();
  }
});

test("a plugin is built here, uploaded and added in one command", async () => {
  const directory = mkdtempSync(join(tmpdir(), "plugin-build-"));
  const context = join(directory, "Audit_Export");
  const api = server({});
  try {
    await expect(run(["--build", "--context", context])).rejects.toThrow(
      /No Dockerfile at .*Audit_Export\/Dockerfile/,
    );
    expect(api.requests).toEqual([]);

    const dockerfile = join(directory, "Dockerfile.plugin");
    writeFileSync(dockerfile, "FROM scratch\n");
    await run(["--build", "--context", context, "--dockerfile", dockerfile]);
    expect(api.builds).toHaveLength(1);
    const built = api.builds[0];
    expect(built).toMatchObject({ dockerfile, context, buildArgs: [] });
    // Named after the directory, and never the same tag twice.
    expect(built?.tag).toMatch(/^audit-export:plugin-\d+$/);
    const created = api.requests.find(
      ({ method, path }) => method === "POST" && path === "/projects",
    );
    expect(created?.body).toMatchObject({ name: "audit-export" });
    expect(api.uploads[0]).toMatchObject({
      imageName: built?.tag,
      projectId: 7,
      environmentId: 9,
    });
    expect(api.posts()).toEqual(["/projects", "/x/plugins/services"]);
  } finally {
    api.restore();
    rmSync(directory, { recursive: true, force: true });
  }
});

test("a build or an upload that fails adds nothing and says what was left", async () => {
  const directory = mkdtempSync(join(tmpdir(), "plugin-build-"));
  writeFileSync(join(directory, "Dockerfile"), "FROM scratch\n");
  const failing = server({ failBuild: true });
  try {
    await expect(
      run(["--build", "--context", directory, "--name", "reports"]),
    ).rejects.toThrow(
      /could not be built, so nothing was created or deployed: docker build failed/,
    );
    expect(failing.posts()).toEqual([]);
    expect(failing.uploads).toEqual([]);
  } finally {
    failing.restore();
    rmSync(directory, { recursive: true, force: true });
  }
  const refused = server({ localImages: ["reports:dev"], refuseUpload: true });
  try {
    await expect(run(["--local-image", "reports:dev"])).rejects.toThrow(
      /was not uploaded to reports.*--project reports/,
    );
    expect(refused.watched).toEqual([]);
    expect(refused.posts()).toEqual(["/projects"]);
  } finally {
    refused.restore();
  }
});

test("an uploaded image deployed to a service that is a plugin is an update that waits", async () => {
  const uploadedUpdate: ServicePlugin = {
    ...waiting,
    pending_update: waiting.pending_update && {
      ...waiting.pending_update,
      deployment_id: 43,
    },
  };
  const api = server({
    existing: true,
    localImages: ["reports:dev"],
    lists: [[plugin({})], [uploadedUpdate]],
  });
  try {
    await run(["--local-image", "reports:dev", "--project", "reports"]);
    expect(api.posts()).toEqual([]);
    expect(api.uploads).toHaveLength(1);
    expect(api.printed()).toContain("waits for approval");
  } finally {
    api.restore();
  }
});
