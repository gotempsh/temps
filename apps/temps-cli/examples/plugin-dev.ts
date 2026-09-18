// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
// Source example: bunx @temps-sdk/cli plugin dev --exec bun -- run examples/plugin-dev.ts
// Compile: bun build --compile examples/plugin-dev.ts --outfile /tmp/temps-example-plugin
import { runPlugin } from "../../../sdks/node/packages/plugin-sdk/src/runtime.js";
import type { PluginEvent } from "../../../sdks/node/packages/plugin-sdk/src/types.js";
import { join } from "node:path";
import { rename } from "node:fs/promises";
let events: PluginEvent[] = [];
let queue = Promise.resolve();
const page = `<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1"><title>Deployment journal · Temps plugin</title><style>body{font:16px system-ui;background:#101114;color:#eee;margin:0}main{max-width:850px;margin:auto;padding:32px}h1{font-size:32px}p{color:#b9bcc5}pre{white-space:pre-wrap;overflow-wrap:anywhere;background:#1c1e24;padding:20px;border-radius:8px}button{padding:12px 20px;background:#fb744a;border:0;border-radius:6px;cursor:pointer}</style></head><body><main><p>Temps · Local example plugin</p><h1>Deployment journal</h1><p>Send a deployment event from the CLI. This plugin stores unique events across restarts.</p><button id="refresh">Refresh</button> <button id="ai">Try mock AI</button><pre id="state">Loading…</pre><pre id="result" hidden></pre></main><script>
const base=location.pathname.slice(0,location.pathname.indexOf('/ui'));
async function refresh(){const r=await fetch(base+'/api/state');document.querySelector('#state').textContent=JSON.stringify(await r.json(),null,2)}
document.querySelector('#refresh').onclick=refresh;
document.querySelector('#ai').onclick=async()=>{const r=await fetch(base+'/api/ai',{method:'POST'});const el=document.querySelector('#result');el.hidden=false;el.textContent=JSON.stringify(await r.json(),null,2)};
refresh().catch(e=>document.querySelector('#state').textContent=e.message);
</script></body></html>`;
await runPlugin({
  manifest: () => ({
    name: "deployment-journal",
    version: "0.1.0",
    nav: [],
    requires_db: false,
    health_path: "/health",
    events: ["deployment.*"],
    host_permissions: ["events_read", "ai_generate", "projects_read"],
  }),
  async onStart(ctx) {
    const file = Bun.file(join(ctx.dataDir, "events.json"));
    if (await file.exists()) events = await file.json();
  },
  onEvent(ctx, event) {
    queue = queue
      .then(async () => {
        if (events.some((e) => e.id === event.id)) return;
        const next = [...events, event].slice(-100);
        const file = join(ctx.dataDir, "events.json");
        await Bun.write(file + ".tmp", JSON.stringify(next));
        await rename(file + ".tmp", file);
        events = next;
      })
      .catch((error) => {
        console.error("Cannot persist deployment journal:", error.message);
      });
    return queue;
  },
  async onShutdown() {
    await queue;
  },
  handler(ctx) {
    return async (req, res) => {
      const path = (req.url ?? "").split("?")[0];
      if (path === "/ui/" || path === "/ui") {
        res.writeHead(200, { "content-type": "text/html" });
        res.end(page);
        return;
      }
      try {
        let data: unknown;
        if (path === "/api/state")
          data = {
            events,
            capabilities: await ctx.permissions(),
            role: req.headers["x-temps-user-role"],
          };
        else if (path === "/api/ai" && req.method === "POST")
          data = await ctx.ai.generate({
            purpose: "deployment.summary",
            prompt: "Summarize the deployment.",
          });
        else if (path === "/api/project") data = await ctx.temps.getProject(1);
        else {
          res.writeHead(404);
          res.end("Not found");
          return;
        }
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify(data));
      } catch (error) {
        res.writeHead(400, { "content-type": "application/json" });
        res.end(
          JSON.stringify({
            error:
              error instanceof Error ? error.message : "Plugin request failed",
          }),
        );
      }
    };
  },
});
