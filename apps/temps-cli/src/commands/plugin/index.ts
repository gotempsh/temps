// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { Command } from "commander";
import { mkdir } from "node:fs/promises";
import { resolve, join } from "node:path";
import { promptConfirm } from "../../ui/prompts.js";
import { TARGETS, validName, PluginPublishError } from "./model.js";
import { buildPlugin, loadConfig, publishPlugin } from "./workflow.js";
import { registerPluginInstallCommands } from "./install.js";
import { registerPluginGrantCommands } from "./grants.js";

export function registerPluginCommands(program: Command) {
  const plugin = program
    .command("plugin")
    .description("Create, install, update and build TypeScript plugins");
  registerPluginInstallCommands(plugin);
  registerPluginGrantCommands(plugin);
  plugin.hook("preAction", () => {
    if (typeof Bun === "undefined")
      throw new PluginPublishError(
        "Plugin commands require Bun. Run bunx --bun @temps-sdk/cli plugin <command>.",
      );
  });
  plugin
    .command("init")
    .argument("<directory>", "New plugin directory")
    .requiredOption(
      "--name <name>",
      "Scoped npm name, e.g. @your-scope/my-plugin",
    )
    .action(async (dir: string, options: { name: string }) => {
      if (!validName(options.name))
        throw new PluginPublishError("Use a scoped npm package name.");
      const path = resolve(dir);
      await mkdir(path); // Never overwrite an existing project.
      const name = options.name
        .slice(options.name.indexOf("/") + 1)
        .replace(/[._]/g, "-");
      await mkdir(join(path, "src"));
      await Bun.write(
        join(path, "package.json"),
        JSON.stringify(
          {
            name: options.name,
            version: "0.1.0",
            private: true,
            type: "module",
            description: "Describe what your plugin does",
            author: "Your team",
            repository: "https://github.com/your-org/your-plugin",
            dependencies: { "@temps-sdk/plugin": "0.1.0-beta.1" },
            temps: {
              name,
              title: name,
              category: "Development",
              entrypoint: "src/index.ts",
              platforms: Object.keys(TARGETS),
            },
          },
          null,
          2,
        ) + "\n",
      );
      await Bun.write(
        join(path, ".gitignore"),
        "node_modules/\n.temps-plugin/\n.env\n.env.*\n",
      );
      await Bun.write(
        join(path, "src/index.ts"),
        `// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { runPlugin, createManifest, createEmbeddedUiHandler } from "@temps-sdk/plugin";
import type { EmbeddedAssets, RequestHandler, TempsPlugin } from "@temps-sdk/plugin";
import pkg from "../package.json";
import { page } from "./page";

export function manifest() {
  return createManifest(pkg.temps.name, pkg.version)
    .displayName(pkg.temps.title)
    .description(pkg.description)
    .addNav(pkg.temps.title, "puzzle", "/ui/", { section: "platform", order: 50 })
    .ui({ entry_js: "/ui/app.js", css: [], routes: [{ path: "/ui/", title: pkg.temps.title }] })
    .build();
}

const assets: EmbeddedAssets = new Map([
  ["index.html", { content: Buffer.from(page(pkg.temps.title)), contentType: "text/html; charset=utf-8", immutable: false }],
  ["app.js", { content: Buffer.from("fetch(new URL('../', location.href).pathname.replace(/\\/$/, '')).then(r => { if (!r.ok) throw new Error('Unavailable'); return r.json(); }).then(data => { document.querySelector('[data-message]').textContent = data.message; }).catch(() => { document.querySelector('[data-message]').textContent = 'The JSON endpoint is unavailable.'; });"), contentType: "application/javascript; charset=utf-8", immutable: false }],
]);

export function handler(): RequestHandler {
  const serveUi = createEmbeddedUiHandler(assets);
  return (req, res) => {
    if (req.method === "GET" && serveUi(req, res)) return;
    if (req.method === "GET" && (req.url === "/" || req.url?.startsWith("/?"))) {
      res.writeHead(200, { "Content-Type": "application/json; charset=utf-8" });
      res.end(JSON.stringify({ message: "Hello from " + pkg.temps.title }));
      return;
    }
    res.writeHead(404, { "Content-Type": "application/json; charset=utf-8" });
    res.end(JSON.stringify({ error: "Not found" }));
  };
}

export const plugin: TempsPlugin = { manifest, handler, embeddedUiAssets: () => assets };

if (import.meta.main) await runPlugin(plugin);
`,
      );
      await Bun.write(
        join(path, "src/page.ts"),
        `// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[character]!);
}

export function page(title: string): string {
  const safeTitle = escapeHtml(title);
  return \`<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>\${safeTitle} · Temps</title>
  <style>
    :root { color-scheme: light dark; --ink:#18181b; --muted:#71717a; --paper:#fafafa; --surface:#fff; --line:#e4e4e7; --accent:#e85d3f; }
    @media (prefers-color-scheme: dark) { :root { --ink:#f4f4f5; --muted:#a1a1aa; --paper:#09090b; --surface:#18181b; --line:#3f3f46; --accent:#ff8466; } }
    * { box-sizing: border-box; }
    body { margin:0; background:var(--paper); color:var(--ink); font:14px ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; }
    main { max-width:800px; margin:auto; padding:40px 24px; }
    h1 { margin:8px 0; font-size:30px; letter-spacing:-.03em; }
    p { color:var(--muted); line-height:1.6; }
    .card { margin-top:28px; padding:24px; border:1px solid var(--line); border-radius:12px; background:var(--surface); }
  </style>
</head>
<body>
  <main>
    <p>Temps / Plugin workspace</p>
    <h1>\${safeTitle}</h1>
    <p>Your plugin is installed and ready. This page is served inside Temps.</p>
    <section class="card"><h2>Make this space yours</h2><p>Edit src/page.ts to build your interface. The plugin root still returns JSON.</p><p data-message>Checking API…</p></section>
  </main>
  <script src="./app.js" defer></script>
</body>
</html>\`;
}
`,
      );
      console.log(
        `Created ${path}. Edit package.json metadata, run bun install there, then temps plugin build --all.`,
      );
    });
  plugin
    .command("build")
    .description(
      "Build every platform selected in package.json; --all selects all six supported targets",
    )
    .option("--all", "Build all supported targets")
    .action(async (options: { all?: boolean }) => {
      const c = await loadConfig(process.cwd());
      if (options.all)
        c.temps.platforms = Object.keys(TARGETS) as (keyof typeof TARGETS)[];
      console.log(await buildPlugin(process.cwd(), c));
    });
  plugin
    .command("publish")
    .description(
      "Build, publish native npm packages, verify ownership and submit for review; resumes interrupted releases",
    )
    .option("-y, --yes", "Confirm public npm publication non-interactively")
    .action(async (options: { yes?: boolean }) => {
      const c = await loadConfig(process.cwd());
      if (
        !options.yes &&
        !(await promptConfirm({
          message: `Publish ${c.name}@${c.version} for ${c.temps.platforms.length} platforms to public npm and submit for review?`,
          default: false,
        }))
      )
        return;
      await publishPlugin(process.cwd(), c);
    });
}
