// SPDX-License-Identifier: MIT OR Apache-2.0
import type { Command } from "commander";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { promptConfirm } from "../../ui/prompts.js";
import { TARGETS, validName, PluginPublishError } from "./model.js";
import { buildPlugin, loadConfig, publishPlugin } from "./workflow.js";

export function registerPluginCommands(program: Command) {
  const plugin = program
    .command("plugin")
    .description("Create, cross-compile and publish TypeScript plugins");
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
      await writeFile(
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
            dependencies: { "@temps-sdk/plugin": "latest" },
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
        { flag: "wx" },
      );
      await writeFile(
        join(path, ".gitignore"),
        "node_modules/\n.temps-plugin/\n.env\n.env.*\n",
        { flag: "wx" },
      );
      await writeFile(
        join(path, "src/index.ts"),
        `import {runPlugin,createManifest} from '@temps-sdk/plugin';\nimport pkg from '../package.json';\nawait runPlugin({manifest:()=>createManifest(pkg.temps.name,pkg.version).displayName(pkg.temps.title).build(),handler:()=>async(_req,res)=>{res.writeHead(200,{'Content-Type':'application/json'});res.end(JSON.stringify({message:'Hello from '+pkg.temps.title}));}});\n`,
        { flag: "wx" },
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
