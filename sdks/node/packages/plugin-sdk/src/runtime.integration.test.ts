// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, it } from "vitest";
import { spawn, spawnSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { request } from "node:http";
import { createConnection } from "node:net";
import { createInterface } from "node:readline";
import { WebSocket } from "ws";

// A real Bun subprocess, Unix HTTP socket and WebSocket—not a mocked SDK client.
it.skipIf(spawnSync("bun", ["--version"]).status !== 0)(
  "starts with protocol 2 and adapts to live AI revocation",
  async () => {
    const directory = await mkdtemp("/tmp/temps-sdk-ai-");
    const socketPath = `${directory}/plugin.sock`;
    const secret = "d3a13e4c-0d28-4ccf-988f-82ab9f454c8a";
    const runtimeUrl = new URL("./index.ts", import.meta.url).href;
    const source = `import {runPlugin,createManifest} from ${JSON.stringify(runtimeUrl)};
    process.argv=["bun","fixture","--socket-path",${JSON.stringify(socketPath)},"--data-dir",${JSON.stringify(directory)}];
    let startup;
    await runPlugin({
      manifest:()=>createManifest("ai-fixture","1.0.0").requestPermissions("ai_generate").build(),
      onStart:async ctx=>{startup=await ctx.permissions();},
      handler:ctx=>async(req,res)=>{
        try {
          const current=await ctx.permissions();
          const result=current.permissions.includes("ai_generate") ? await ctx.ai.generate({purpose:"test",prompt:"Hello"}) : null;
          res.writeHead(200,{"Content-Type":"application/json"}); res.end(JSON.stringify({startup,current,result}));
        } catch {res.writeHead(503);res.end("Host call failed");}
      }
    });`;
    const child = spawn("bun", ["-e", source], {
      stdio: ["pipe", "pipe", "pipe"],
    });
    const lines = createInterface({ input: child.stdout });
    const iterator = lines[Symbol.asyncIterator]();
    let ws: WebSocket | undefined;
    let approved = true;
    const calls: string[] = [];
    async function http(
      path: string,
      authenticated = true,
    ): Promise<{ status: number; body: string }> {
      return new Promise((resolve, reject) => {
        const req = request(
          {
            socketPath,
            path,
            headers: authenticated ? { "x-temps-auth-signature": secret } : {},
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
    try {
      const hello = JSON.parse((await iterator.next()).value ?? "null");
      expect(hello.type).toBe("hello");
      expect(hello.protocol_version).toBe(2);
      expect(hello.manifest.host_permissions).toEqual(["ai_generate"]);
      child.stdin.end(
        JSON.stringify({
          protocol_version: 2,
          auth_secret: secret,
          database_url: null,
          host_data_dir: null,
        }) + "\n",
      );
      expect(JSON.parse((await iterator.next()).value ?? "null")).toMatchObject(
        { type: "ready", protocol_version: 2 },
      );
      expect((await http("/_temps/channel", false)).status).toBe(401);
      expect((await http("/_events", false)).status).toBe(401);
      expect((await http("/", false)).status).toBe(401);
      ws = new WebSocket("ws://localhost/_temps/channel", {
        headers: { "x-temps-auth-signature": secret },
        createConnection: () => createConnection(socketPath),
      });
      ws.on("message", (raw) => {
        const message = JSON.parse(raw.toString());
        calls.push(message.call.method);
        const result =
          message.call.method === "get_host_capabilities"
            ? {
                actor: { id: "test-actor", name: "ai-fixture", active: true },
                permissions: approved ? ["ai_generate"] : [],
                ai: { configured: true },
              }
            : { text: "Hello from the host provider", model: "mock-model" };
        ws?.send(
          JSON.stringify({
            type: "response",
            id: message.id,
            outcome: { ok: { method: message.call.method, result } },
          }),
        );
      });
      await new Promise<void>((resolve, reject) => {
        ws?.once("open", resolve);
        ws?.once("error", reject);
      });
      let response = await http("/");
      for (
        let attempt = 0;
        response.status === 503 && attempt < 50;
        attempt++
      ) {
        await new Promise((resolve) => setTimeout(resolve, 20));
        response = await http("/");
      }
      expect(response.status).toBe(200);
      expect(JSON.parse(response.body)).toMatchObject({
        startup: { permissions: ["ai_generate"] },
        result: { text: "Hello from the host provider" },
      });
      expect(calls.filter((method) => method === "generate_ai")).toHaveLength(
        1,
      );
      approved = false;
      const revoked = await http("/");
      expect(JSON.parse(revoked.body)).toMatchObject({
        current: { permissions: [] },
        result: null,
      });
      expect(calls.filter((method) => method === "generate_ai")).toHaveLength(
        1,
      );
    } finally {
      ws?.terminate();
      child.kill("SIGTERM");
      await new Promise<void>((resolve) => {
        if (child.exitCode !== null) resolve();
        else child.once("exit", () => resolve());
      });
      lines.close();
      await rm(directory, { recursive: true, force: true });
    }
  },
  15_000,
);
