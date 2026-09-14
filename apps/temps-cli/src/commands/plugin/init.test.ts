// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Command } from "commander";
import { registerPluginCommands } from "./index.js";

test("plugin init generates a sidebar-ready embedded UI and retains JSON root", async () => {
  const directory = await mkdtemp(join(tmpdir(), "temps-plugin-sidebar-"));
  const target = join(directory, "plugin");
  try {
    const command = new Command();
    registerPluginCommands(command);
    await command.parseAsync(["plugin", "init", target, "--name", "@example/hello"], { from: "user" });

    const pkg = await Bun.file(join(target, "package.json")).json();
    expect(pkg.dependencies["@temps-sdk/plugin"]).toBe("0.1.0-beta.1");

    const source = await Bun.file(join(target, "src/index.ts")).text();
    expect(source).toContain('.addNav(pkg.temps.title, "puzzle", "/ui/", { section: "platform", order: 50 })');
    expect(source).toContain('.ui({ entry_js: "/ui/app.js"');
    expect(source).toContain("createEmbeddedUiHandler(assets)");
    expect(source).toContain("embeddedUiAssets: () => assets");
    expect(source).toContain("fetch(new URL('../', location.href).pathname.replace(/\\/$/, ''))");
    expect(source).not.toContain("fetch('../')");
    expect(source).toContain('req.url === "/"');
    expect(source).toContain('res.end(JSON.stringify({ message: "Hello from " + pkg.temps.title }))');
    expect(source).toContain("if (import.meta.main) await runPlugin(plugin)");
    expect(source).toStartWith("// SPDX-FileCopyrightText:");

    const pageSource = await Bun.file(join(target, "src/page.ts")).text();
    expect(pageSource).toStartWith("// SPDX-FileCopyrightText:");
    const { page } = await import(join(target, "src/page.ts"));
    const html = page('<script>alert("x")</script>');
    expect(html).toContain("&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;");
    expect(html).not.toContain('<script>alert("x")</script>');
    expect(html).toContain('<script src="./app.js" defer></script>');

    const build = await Bun.build({
      entrypoints: [join(target, "src/index.ts")],
      target: "bun",
      external: ["@temps-sdk/plugin"],
    });
    expect(build.success).toBe(true);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
