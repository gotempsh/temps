// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, test, expect } from "bun:test";
import { mkdtemp, mkdir, rm, stat, readdir, symlink } from "node:fs/promises";
import { saveAtomic, readState } from "./state.js";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Command } from "commander";
import { registerPluginCommands } from "./index.js";
import {
  TARGETS,
  parseConfig,
  platformManifest,
  releaseMetadata,
  type PluginConfig,
} from "./model.js";
import {
  buildPlugin,
  publishPlugin,
  validateState,
  validateStatus,
  configDigest,
  command,
  type PublishDependencies,
} from "./workflow.js";
const config: PluginConfig = {
  name: "@example/pulse",
  version: "1.0.0",
  description: "Deployment health",
  author: "Example",
  repository: "https://github.com/example/plugin",
  temps: {
    name: "pulse",
    title: "Pulse",
    category: "Observability",
    entrypoint: "src/index.ts",
    platforms: ["linux-amd64-gnu", "darwin-arm64"],
  },
};
const draft = () => ({
  id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
  expiresAt: new Date(Date.now() + 3600000).toISOString(),
  challenges: releaseMetadata(config).packages.map((p, i) => ({
    ...p,
    id: `00000000-0000-0000-0000-00000000000${i}`,
    code: "a".repeat(43),
  })),
});
const statusPackages = () =>
  draft().challenges.map((p) => ({ id: p.id, verifiedAt: null }));
describe("TypeScript plugin publishing", () => {
  test("Bun subprocesses preserve literal arguments and report failed or missing executables", async () => {
    const dir = await mkdtemp(join(tmpdir(), "temps-plugin-command-"));
    try {
      const literal = "literal; $(not-a-shell-command)";
      await command(
        [
          process.execPath,
          "-e",
          "await Bun.write('argument.txt', process.argv.at(-1))",
          literal,
        ],
        dir,
      );
      expect(await Bun.file(join(dir, "argument.txt")).text()).toBe(literal);
      await expect(
        command([process.execPath, "-e", "process.exit(7)"], dir),
      ).rejects.toThrow("failed (7)");
      await expect(
        command([join(dir, "missing-executable")], dir),
      ).rejects.toThrow("Could not start");
      await expect(command([], dir)).rejects.toThrow(
        "Missing command executable",
      );
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });
  test("Bun scaffolding writes a complete project without overwriting an existing directory", async () => {
    const dir = await mkdtemp(join(tmpdir(), "temps-plugin-init-"));
    const target = join(dir, "plugin");
    const program = new Command();
    registerPluginCommands(program);
    try {
      await program.parseAsync(
        ["plugin", "init", target, "--name", "@example/hello"],
        { from: "user" },
      );
      expect((await Bun.file(join(target, "package.json")).json()).name).toBe(
        "@example/hello",
      );
      expect(await Bun.file(join(target, ".gitignore")).text()).toContain(
        ".temps-plugin/",
      );
      expect(await Bun.file(join(target, "src/index.ts")).text()).toContain(
        "runPlugin",
      );
      await expect(
        program.parseAsync(
          ["plugin", "init", target, "--name", "@example/replacement"],
          { from: "user" },
        ),
      ).rejects.toThrow();
      expect((await Bun.file(join(target, "package.json")).json()).name).toBe(
        "@example/hello",
      );
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });
  test("recovers the same durable create request after the server commits but its response is lost", async () => {
    const dir = await mkdtemp(join(tmpdir(), "temps-plugin-recovery-"));
    const tokens: string[] = [];
    let builds = 0;
    const response = draft();
    const deps: PublishDependencies = {
      npmExists: async () => false,
      buildPlugin: async () => {
        builds++;
        return dir;
      },
      command: async () => {
        throw new Error("Unexpected npm invocation");
      },
      publisher: async (body) => {
        const b = body as { operation: string; recoveryToken: string };
        if (b.operation === "create") {
          const journal = await Bun.file(
            join(dir, ".temps-plugin", config.version, "request.json"),
          ).json();
          expect(journal.recoveryToken).toBe(b.recoveryToken);
          tokens.push(b.recoveryToken);
          if (tokens.length === 1)
            throw new Error("Response lost after commit");
          return response;
        }
        return { status: "pending", packages: statusPackages() };
      },
    };
    try {
      await expect(publishPlugin(dir, config, deps)).rejects.toThrow(
        "Response lost",
      );
      await publishPlugin(dir, config, deps);
      expect(tokens).toHaveLength(2);
      expect(tokens[0]).toBe(tokens[1]);
      expect(builds).toBe(1);
      expect(
        (
          await Bun.file(
            join(dir, ".temps-plugin", config.version, "release.json"),
          ).json()
        ).id,
      ).toBe(response.id);
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });
  test("rejects malformed create/status responses before use", () => {
    const state = { ...draft(), digest: configDigest(config) };
    for (const value of [
      null,
      [],
      { ...state, expiresAt: undefined },
      { ...state, expiresAt: "not a date" },
      { ...state, challenges: [null, null] },
      { ...state, challenges: [state.challenges[0], state.challenges[0]] },
    ])
      expect(() => validateState(value, config)).toThrow();
    for (const value of [
      null,
      {},
      { status: "draft" },
      { status: "unknown", packages: statusPackages() },
      { status: "draft", packages: [] },
      {
        status: "draft",
        packages: [
          { id: state.challenges[0]!.id, verifiedAt: "invalid" },
          statusPackages()[1],
        ],
      },
    ])
      expect(() => validateStatus(value, state)).toThrow(
        "Invalid publisher status",
      );
    expect(() =>
      validateStatus({ status: "draft", packages: statusPackages() }, state),
    ).not.toThrow();
  });
  test("atomic state writes preserve the old copy on serialization failure and reject corrupt state", async () => {
    const dir = await mkdtemp(join(tmpdir(), "temps-plugin-atomic-"));
    const path = join(dir, "release.json");
    try {
      await saveAtomic(path, { id: "original" });
      await expect(saveAtomic(path, { bad: 1n })).rejects.toThrow(
        "durably save",
      );
      expect(await readState(path)).toEqual({ id: "original" });
      expect((await stat(path)).mode & 0o777).toBe(0o600);
      expect(await readdir(dir)).toEqual(["release.json"]);
      await saveAtomic(path, { id: "replacement" });
      expect(await readState(path)).toEqual({ id: "replacement" });
      await symlink(path, join(dir, "link.json"));
      await expect(saveAtomic(join(dir, "link.json"), {})).rejects.toThrow();
      await Bun.write(path, '{"id":');
      await expect(readState(path)).rejects.toThrow("Preserve it for recovery");
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });
  test("validates metadata and maps all six native platforms", () => {
    expect(parseConfig(config)).toEqual(config);
    expect(Object.keys(TARGETS)).toHaveLength(6);
    for (const platform of Object.keys(TARGETS) as (keyof typeof TARGETS)[]) {
      const pkg = platformManifest(config, platform, "proof");
      expect(pkg.files).toEqual(["plugin"]);
      expect(pkg.temps?.verification).toBe("proof");
      expect(pkg).not.toHaveProperty("scripts");
      const target = TARGETS[platform];
      expect(pkg.os).toEqual([target.os]);
      expect(pkg.cpu).toEqual([target.cpu]);
      if ("libc" in target) expect(pkg.libc).toEqual([target.libc]);
      else expect(pkg).not.toHaveProperty("libc");
    }
    expect(releaseMetadata(config).packages[0]?.name).toBe(
      "@example/pulse-linux-x64-gnu",
    );
  });
  test.each(["../escape.ts", "src/../../escape.ts", "/tmp/entry.ts"])(
    "rejects unsafe entrypoint %s",
    (entrypoint) =>
      expect(() =>
        parseConfig({ ...config, temps: { ...config.temps, entrypoint } }),
      ).toThrow(),
  );
  test("rejects ranges, arbitrary categories and duplicate targets", () => {
    expect(() => parseConfig({ ...config, version: "latest" })).toThrow();
    expect(() =>
      parseConfig({
        ...config,
        temps: { ...config.temps, category: "anything" },
      }),
    ).toThrow();
    expect(() =>
      parseConfig({
        ...config,
        temps: { ...config.temps, platforms: ["darwin-arm64", "darwin-arm64"] },
      }),
    ).toThrow();
  });
  test("builds isolated allowlisted packages and resumes npm interruption without a second draft", async () => {
    const dir = await mkdtemp(join(tmpdir(), "temps-plugin-workflow-test-"));
    await mkdir(join(dir, "src"));
    await Bun.write(join(dir, "src/index.ts"), 'console.log("fixture")');
    const events: string[] = [];
    const remote = new Set<string>();
    let fail = true;
    const runner: PublishDependencies["command"] = async (argv) => {
      if (argv[0] === "bun") {
        await Bun.write(argv[argv.length - 1]!, "native fixture");
        return;
      }
    };
    const deps: PublishDependencies = {
      buildPlugin: (cwd, c, state) => buildPlugin(cwd, c, state, runner),
      npmExists: async (name) => remote.has(name),
      command: async (_argv, cwd) => {
        const pkg = await Bun.file(join(cwd, "package.json")).json();
        events.push("npm:" + pkg.name);
        if (fail) {
          fail = false;
          throw new Error("2FA interrupted");
        }
        remote.add(pkg.name);
      },
      publisher: async (body) => {
        const b = body as { operation: string };
        events.push(b.operation);
        if (b.operation === "create")
          return {
            id: "a".repeat(8) + "-aaaa-aaaa-aaaa-" + "a".repeat(12),
            expiresAt: new Date(Date.now() + 3600000).toISOString(),
            challenges: releaseMetadata(config).packages.map((p, i) => ({
              ...p,
              id: `00000000-0000-0000-0000-00000000000${i}`,
              code: "a".repeat(43),
            })),
          };
        if (b.operation === "status")
          return { status: "draft", packages: statusPackages() };
        return {};
      },
    };
    try {
      await expect(publishPlugin(dir, config, deps)).rejects.toThrow(
        "2FA interrupted",
      );
      await publishPlugin(dir, config, deps);
      expect(events.filter((e) => e === "create")).toHaveLength(1);
      expect(events.filter((e) => e === "verify")).toHaveLength(2);
      expect(events.at(-1)).toBe("submit");
      expect(remote.size).toBe(2);
      const before = events.length;
      await publishPlugin(dir, config, {
        ...deps,
        publisher: async () => ({
          status: "pending",
          packages: statusPackages(),
        }),
      });
      expect(events.length).toBe(before);
      await expect(
        publishPlugin(dir, { ...config, description: "changed" }, deps),
      ).rejects.toThrow("does not match");
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });
  test("existing npm versions stop before building or creating a draft", async () => {
    const dir = await mkdtemp(join(tmpdir(), "temps-plugin-existing-test-"));
    const forbidden = async () => {
      throw new Error("Unexpected side effect");
    };
    try {
      await expect(
        publishPlugin(dir, config, {
          npmExists: async () => true,
          publisher: forbidden,
          command: forbidden,
          buildPlugin: forbidden,
        }),
      ).rejects.toThrow("already exists");
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });
});
