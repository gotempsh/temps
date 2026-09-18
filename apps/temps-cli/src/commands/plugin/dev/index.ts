// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { Command } from "commander";
import { resolve } from "node:path";
import {
  createEvent,
  DevError,
  eventTypes,
  grants,
  integer,
  readJsonFile,
  validateEvent,
} from "./model.js";
import { control } from "./session.js";
import { startRunner } from "./runner.js";
export function registerPluginDevCommands(plugin: Command) {
  const dev = plugin
    .command("dev")
    .enablePositionalOptions()
    .description(
      "Run a plugin with a local simulated host, UI preview, and events (no Temps server)",
    )
    .argument("[binary]", "Compiled plugin executable")
    .argument("[args...]", "Arguments after -- for --exec")
    .option("--session <name>", "Local session name", "default")
    .option("--exec <command>", "Run a command with arguments after --")
    .option("--port <port>", "Loopback port (0 selects a free port)", "0")
    .option("--data-dir <path>", "Persistent plugin data directory")
    .option(
      "--grant <permission...>",
      "Explicit host grants; defaults to none or saved session grants",
    )
    .option(
      "--fixtures <file>",
      "Version-1 JSON host fixtures and mock AI settings",
    )
    .option("--role <role>", "Synthetic preview role: admin or reader", "admin")
    .option("--startup-timeout <ms>", "Handshake deadline", "10000")
    .action(async (binary: string | undefined, args: string[], opts) => {
      let command: string[];
      if (opts.exec) {
        const marker = process.argv.indexOf("--");
        if (
          dev.args.length &&
          (marker < 0 ||
            JSON.stringify(dev.args) !==
              JSON.stringify(process.argv.slice(marker + 1)))
        )
          throw new DevError(
            "Use --exec <command> -- <arguments>; do not also supply a binary.",
          );
        command = [opts.exec, ...dev.args];
      } else {
        if (!binary || args.length)
          throw new DevError(
            "Supply one compiled executable, or --exec <command> -- <arguments>.",
          );
        command = [resolve(binary)];
      }
      const controller = new AbortController();
      let done: () => void = () => {};
      const signal = new Promise<void>((r) => {
        done = r;
      });
      const onSignal = () => {
        controller.abort();
        done();
      };
      process.once("SIGINT", onSignal);
      process.once("SIGTERM", onSignal);
      let runner: Awaited<ReturnType<typeof startRunner>> | undefined;
      try {
        runner = await startRunner({
          session: opts.session,
          command,
          port: integer(opts.port, "port", 0, 65535),
          dataDir: opts.dataDir,
          grants: opts.grant ? grants(opts.grant) : undefined,
          role: opts.role,
          fixtures: opts.fixtures
            ? await readJsonFile(opts.fixtures)
            : undefined,
          signal: controller.signal,
          startupTimeout: integer(
            opts.startupTimeout,
            "startup timeout",
            100,
            60000,
          ),
        });
        console.log(
          `Plugin ${runner.plugin.name}\nPreview: ${runner.url}/\nSession: ${opts.session}\nLocal simulation. Plugins run with your OS permissions.\nCtrl-C to stop. Data: ${runner.dataDir}`,
        );
        const result = await Promise.race([runner.exited, signal]);
        if (typeof result === "number" && result !== 0)
          process.exitCode = result;
      } finally {
        process.removeListener("SIGINT", onSignal);
        process.removeListener("SIGTERM", onSignal);
        await runner?.stop();
      }
    });
  const session = (cmd: Command) =>
    cmd.option("--session <name>", "Running session");
  dev
    .command("events")
    .description("List host event fixtures and example payloads")
    .action(() =>
      console.log(
        JSON.stringify(
          eventTypes.map((type) => ({ type, example: createEvent(type, {}) })),
          null,
          2,
        ),
      ),
    );
  for (const action of ["status", "logs"])
    session(
      dev
        .command(action)
        .description(
          action === "status"
            ? "Show local plugin state"
            : "Show the last 200 redacted simulator records",
        ),
    )
      .option("--json", "Machine-readable output")
      .action(async (opts) => {
        console.log(
          JSON.stringify(
            await control(opts.session ?? dev.opts().session, action),
            null,
            2,
          ),
        );
      });
  session(
    dev
      .command("emit")
      .description("Deliver an event; sent does not mean handler completed")
      .argument("[type]", "Built-in event type"),
  )
    .option("--file <path>", "Full JSON event envelope with a stable ID")
    .option("--project-id <id>", "Project ID", "1")
    .option("--environment-id <id>", "Environment ID", "1")
    .option("--deployment-id <id>", "Deployment ID", "42")
    .option("--environment <name>", "Environment", "production")
    .option("--url <url>", "Deployment URL")
    .option("--repeat <n>", "Repeat the same event ID, up to 1000")
    .option("--count <n>", "Deliver distinct IDs, up to 1000")
    .option("--transport <transport>", "auto or http", "auto")
    .option("--json", "Machine-readable receipts and envelope")
    .action(async (type, opts) => {
      opts = { ...dev.opts(), ...opts };
      if (!!type === !!opts.file)
        throw new DevError("Supply an event type or --file, but not both.");
      if (opts.repeat && opts.count)
        throw new DevError("--repeat and --count are mutually exclusive.");
      const count = integer(
        opts.repeat ?? opts.count ?? 1,
        "delivery count",
        1,
        1000,
      );
      const event = opts.file
        ? validateEvent(await readJsonFile(opts.file))
        : createEvent(type, opts);
      const results = [];
      for (let i = 0; i < count; i++)
        results.push(
          await control(opts.session, "emit", {
            event: opts.count ? { ...event, id: crypto.randomUUID() } : event,
            transport: opts.transport,
          }),
        );
      console.log(
        JSON.stringify(
          { event, deliveries: results, simulated: true },
          null,
          2,
        ),
      );
    });
  const group = dev
    .command("grants")
    .description("Change simulated host permissions immediately");
  session(
    group
      .command("set")
      .description("Replace all grants or revoke all with --clear"),
  )
    .option("--grant <permission...>", "Complete replacement grant set")
    .option("--clear", "Revoke every grant")
    .action(async (opts) => {
      opts = { ...dev.opts(), ...opts };
      if (!!opts.grant === !!opts.clear)
        throw new DevError("Use --grant <permissions...> or --clear.");
      console.log(
        JSON.stringify(
          await control(opts.session, "grants", {
            grants: grants(opts.clear ? [] : opts.grant),
          }),
          null,
          2,
        ),
      );
    });
}
