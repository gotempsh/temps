// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, it } from "vitest";
import { spawn, spawnSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { request } from "node:http";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { WebSocket } from "ws";
import { serviceAuthSecret } from "./service.js";

const bunMissing = spawnSync("bun", ["--version"]).status !== 0;
const sdk = JSON.stringify(new URL("./index.ts", import.meta.url).href);
const TOKEN = "dt_fixture";

/** A plugin that reports what the platform tells it, started the way a container starts it. */
function pluginSource(manifest: string): string {
  return `import {runPlugin,createManifest} from ${sdk};
    process.argv=["bun","fixture"];
    await runPlugin({
      manifest:()=>${manifest},
      handler:ctx=>async(req,res)=>{
        try {
          const projects=await ctx.temps.listProjects();
          res.writeHead(200,{"Content-Type":"application/json"});
          res.end(JSON.stringify({projects,dataDir:ctx.dataDir}));
        } catch (error) {res.writeHead(503);res.end(String(error));}
      }
    });`;
}

function startPlugin(source: string, env: Record<string, string>) {
  const inherited = { ...process.env };
  for (const name of ["PORT", "TEMPS_API_TOKEN", "TEMPS_PLUGIN_DATA_DIR"])
    delete inherited[name];
  const child = spawn("bun", ["-e", source], {
    stdio: ["ignore", "ignore", "pipe"],
    env: { ...inherited, ...env },
  });
  let stderr = "";
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  const exited = new Promise<number | null>((resolve) => {
    child.once("exit", (code) => resolve(code));
  });
  return {
    child,
    exited,
    stderr: () => stderr,
    async stop() {
      if (child.exitCode === null) child.kill("SIGTERM");
      return exited;
    },
  };
}

async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const probe = createServer();
    probe.once("error", reject);
    probe.listen(0, "127.0.0.1", () => {
      const address = probe.address();
      probe.close(() =>
        typeof address === "object" && address
          ? resolve(address.port)
          : reject(new Error("no port")),
      );
    });
  });
}

function http(
  port: number,
  path: string,
  secret?: string,
): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const req = request(
      {
        host: "127.0.0.1",
        port,
        path,
        headers: secret ? { "x-temps-auth-signature": secret } : {},
      },
      (res) => {
        let body = "";
        res.on("data", (chunk) => {
          body += chunk;
        });
        res.on("end", () => resolve({ status: res.statusCode ?? 0, body }));
      },
    );
    req.on("error", reject);
    req.end();
  });
}

async function eventually<T>(
  attempt: () => Promise<T | undefined>,
  what: string,
): Promise<T> {
  for (let tries = 0; tries < 250; tries++) {
    const value = await attempt().catch(() => undefined);
    if (value !== undefined) return value;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  throw new Error(`Timed out waiting for ${what}`);
}

/** The platform's side of the channel: answers project queries with `projects`. */
async function connectPlatform(
  port: number,
  secret: string,
  projects: unknown[],
): Promise<{ socket: WebSocket; calls: string[] }> {
  const calls: string[] = [];
  const socket = new WebSocket(`ws://127.0.0.1:${port}/_temps/channel`, {
    headers: { "x-temps-auth-signature": secret },
  });
  socket.on("message", (raw) => {
    const message = JSON.parse(raw.toString());
    calls.push(message.call.method);
    socket.send(
      JSON.stringify({
        type: "response",
        id: message.id,
        outcome: { ok: { method: message.call.method, result: projects } },
      }),
    );
  });
  await new Promise<void>((resolve, reject) => {
    socket.once("open", resolve);
    socket.once("error", reject);
  });
  return { socket, calls };
}

// A real Bun subprocess listening on TCP, as in a deployment's container.
it.skipIf(bunMissing)(
  "runs as a service: answers Temps only, waits for the channel and carries on after a reconnect",
  async () => {
    const dataDir = await mkdtemp(join(tmpdir(), "temps-plugin-service-"));
    const port = await freePort();
    const secret = serviceAuthSecret(TOKEN);
    const plugin = startPlugin(
      pluginSource(
        `createManifest("reports","1.2.0").displayName("Reports").requestPermissions("projects_read").build()`,
      ),
      {
        PORT: String(port),
        TEMPS_API_TOKEN: TOKEN,
        TEMPS_PLUGIN_DATA_DIR: dataDir,
      },
    );
    const sockets: WebSocket[] = [];
    try {
      // The deployment's health check has no secret and needs none.
      const health = await eventually(async () => {
        const response = await http(port, "/health");
        return response.status === 200 ? response : undefined;
      }, `the plugin to listen: ${plugin.stderr()}`);
      expect(JSON.parse(health.body)).toEqual({
        status: "ok",
        plugin: "reports",
      });

      // Nothing else answers without the secret, or with another instance's.
      for (const path of [
        "/_temps/plugin/hello",
        "/_temps/channel",
        "/_events",
        "/",
      ]) {
        expect((await http(port, path)).status, path).toBe(401);
        expect(
          (await http(port, path, serviceAuthSecret("dt_another_instance")))
            .status,
          path,
        ).toBe(401);
      }
      // The deployment token is not the secret: it never travels to this port.
      expect((await http(port, "/_temps/plugin/hello", TOKEN)).status).toBe(
        401,
      );

      const hello = await http(port, "/_temps/plugin/hello", secret);
      expect(hello.status).toBe(200);
      expect(JSON.parse(hello.body)).toMatchObject({
        protocol_version: 2,
        has_ui: false,
        manifest: {
          name: "reports",
          version: "1.2.0",
          display_name: "Reports",
          requires_db: false,
          host_permissions: ["projects_read"],
        },
      });

      // Until Temps connects the channel the plugin's own routes say why.
      const waiting = await http(port, "/", secret);
      expect(waiting.status).toBe(503);
      expect(waiting.body).toContain("waiting for Temps");

      const first = await connectPlatform(port, secret, [
        { id: 1, name: "web" },
      ]);
      sockets.push(first.socket);
      const served = await eventually(async () => {
        const response = await http(port, "/", secret);
        return response.status === 200 ? response : undefined;
      }, "the plugin to serve its routes");
      expect(JSON.parse(served.body)).toEqual({
        projects: [{ id: 1, name: "web" }],
        dataDir,
      });
      expect(first.calls).toEqual(["list_projects"]);

      // Temps restarts: the channel breaks. The plugin keeps running, says
      // the platform is unreachable, and still answers the hello.
      first.socket.terminate();
      const broken = await eventually(async () => {
        const response = await http(port, "/", secret);
        return response.status === 503 ? response : undefined;
      }, "the plugin to notice the break");
      expect(broken.body).toContain("Platform channel is closed");
      expect((await http(port, "/_temps/plugin/hello", secret)).status).toBe(
        200,
      );

      // Temps attaches again and the same plugin process carries on.
      const second = await connectPlatform(port, secret, [
        { id: 2, name: "api" },
      ]);
      sockets.push(second.socket);
      const resumed = await eventually(async () => {
        const response = await http(port, "/", secret);
        return response.status === 200 ? response : undefined;
      }, "the plugin to use the new channel");
      expect(JSON.parse(resumed.body).projects).toEqual([
        { id: 2, name: "api" },
      ]);
      expect(second.calls).toEqual(["list_projects"]);
      expect(plugin.child.exitCode).toBeNull();

      // Stopping the container stops the plugin cleanly.
      expect(await plugin.stop()).toBe(0);
    } finally {
      for (const socket of sockets) socket.terminate();
      await plugin.stop();
      await rm(dataDir, { recursive: true, force: true });
    }
  },
  20_000,
);

it.skipIf(bunMissing)(
  "does not start outside a Temps deployment, and says how a plugin is run",
  async () => {
    const plugin = startPlugin(
      pluginSource(`createManifest("reports","1.2.0").build()`),
      { PORT: String(await freePort()) },
    );
    try {
      expect(await plugin.exited).not.toBe(0);
      expect(plugin.stderr()).toContain("TEMPS_API_TOKEN is not set");
      expect(plugin.stderr()).toContain("A plugin runs as a Temps service");
    } finally {
      await plugin.stop();
    }
  },
  20_000,
);

it.skipIf(bunMissing)(
  "does not start when its manifest asks for the control-plane database",
  async () => {
    const port = await freePort();
    const plugin = startPlugin(
      pluginSource(
        `createManifest("reports","1.2.0").requiresDb(true).build()`,
      ),
      { PORT: String(port), TEMPS_API_TOKEN: TOKEN },
    );
    try {
      expect(await plugin.exited).not.toBe(0);
      expect(plugin.stderr()).toContain(
        "a plugin running as a Temps service never receives",
      );
      expect(plugin.stderr()).toContain("Remove requires_db");
      // It refused before listening.
      await expect(http(port, "/health")).rejects.toThrow();
    } finally {
      await plugin.stop();
    }
  },
  20_000,
);
