import { describe, test, expect } from "bun:test";
import { mkdtemp, mkdir, writeFile, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
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
describe("TypeScript plugin publishing", () => {
  test("validates metadata and maps all six native platforms", () => {
    expect(parseConfig(config)).toEqual(config);
    expect(Object.keys(TARGETS)).toHaveLength(6);
    for (const platform of Object.keys(TARGETS) as (keyof typeof TARGETS)[]) {
      const pkg = platformManifest(config, platform, "proof");
      expect(pkg.files).toEqual(["plugin"]);
      expect(pkg.temps?.verification).toBe("proof");
      expect(pkg).not.toHaveProperty("scripts");
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
    await writeFile(join(dir, "src/index.ts"), 'console.log("fixture")');
    const events: string[] = [];
    const remote = new Set<string>();
    let fail = true;
    const runner: PublishDependencies["command"] = async (argv) => {
      if (argv[0] === "bun") {
        await writeFile(argv[argv.length - 1]!, "native fixture");
        return;
      }
    };
    const deps: PublishDependencies = {
      buildPlugin: (cwd, c, state) => buildPlugin(cwd, c, state, runner),
      npmExists: async (name) => remote.has(name),
      command: async (_argv, cwd) => {
        const pkg = JSON.parse(
          await readFile(join(cwd, "package.json"), "utf8"),
        );
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
        if (b.operation === "status") return { status: "draft", packages: [] };
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
        publisher: async () => ({ status: "pending", packages: [] }),
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
