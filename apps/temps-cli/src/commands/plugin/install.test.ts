// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from "bun:test";
import { Command } from "commander";
import {
  installBody,
  validateInstallResult,
  registerPluginInstallCommands,
  installGrants,
} from "./install.js";

const repository = "https://github.com/gotempsh/temps-plugin-template";
test("repo-only install omits name and ref for server auto-detection", () => {
  expect(installBody(repository, {})).toEqual({ repository_url: repository });
});
test("host permissions require explicit flags and preserve zero daily calls", () => {
  expect(installGrants({})).toBeUndefined();
  expect(
    installBody(repository, {
      grant: ["ai_generate", "ai_generate"],
      aiDailyCalls: "0",
      aiMaxTokens: "512",
    }).grants,
  ).toEqual({
    permissions: ["ai_generate"],
    ai_daily_call_limit: 0,
    ai_max_output_tokens: 512,
  });
});
test.each([
  { grant: ["system_admin"] },
  { aiDailyCalls: "100" },
  { grant: ["ai_generate"], aiDailyCalls: "-1" },
  { grant: ["ai_generate"], aiDailyCalls: "1.5" },
  { grant: ["ai_generate"], aiDailyCalls: "10001" },
  { grant: ["ai_generate"], aiMaxTokens: "0" },
  { grant: ["ai_generate"], aiMaxTokens: "4097" },
])("rejects invalid or implicit AI grants %j", (options) => {
  expect(() => installGrants(options)).toThrow();
});
test("advanced overrides are preserved", () => {
  expect(installBody(repository, { name: "my-plugin", ref: "v1.0.0" })).toEqual(
    { repository_url: repository, name: "my-plugin", ref_name: "v1.0.0" },
  );
});
test.each([
  "https://token@github.com/org/repo",
  "https://example.com/org/repo",
  "https://github.com/org/repo?token=secret",
])("rejects unsafe repo without echoing credentials: %s", (repo) => {
  expect(() => installBody(repo, {})).toThrow("without embedded credentials");
});
test.each(["--upload-pack=bad", "../main", "", "main;env"])(
  "rejects unsafe ref %s",
  (ref) => {
    expect(() => installBody(repository, { ref })).toThrow("valid branch");
  },
);
test("rejects unsafe plugin name", () =>
  expect(() => installBody(repository, { name: "../escape" })).toThrow());
test("validates success before reporting installation", () => {
  const result = {
    name: "my-plugin",
    version: "0.1.0",
    source_commit: "a".repeat(40),
    message: "Installed",
  };
  expect(validateInstallResult(result)).toEqual(result);
  for (const value of [
    null,
    {},
    { ...result, source_commit: "" },
    { ...result, version: null },
  ])
    expect(() => validateInstallResult(value)).toThrow(
      "invalid plugin installation response",
    );
});
test("install and update are discoverable with advanced ref options", () => {
  const command = new Command("plugin");
  registerPluginInstallCommands(command);
  expect(command.commands.map((c) => c.name())).toEqual(["install", "update"]);
  expect(command.commands[0]?.helpInformation()).toContain("--ref");
  expect(command.commands[0]?.helpInformation()).toContain("--grant");
  expect(command.commands[1]?.helpInformation()).not.toContain("--grant");
});

for (const path of ["plugins/demo", "site-crawl-plugin", "nested/plugin.v2"]) {
  test(`install supports subdirectory ${path} and custom ref`, () => {
    expect(installBody(repository, { path, ref: "release/v2" })).toEqual({
      repository_url: repository,
      path,
      ref_name: "release/v2",
    });
  });
}
for (const path of [
  "../plugin",
  "/plugin",
  "a//b",
  "a/./b",
  "a/../b",
  "a/",
  "a\\b",
  ".git/plugin",
  "a/.GIT/plugin",
  "%2e%2e/plugin",
  "a".repeat(513),
]) {
  test(`rejects unsafe subdirectory ${path}`, () =>
    expect(() => installBody(repository, { path })).toThrow("--path"));
}
test("empty path preserves root install request", () =>
  expect(installBody(repository, { path: "" })).toEqual({
    repository_url: repository,
  }));
