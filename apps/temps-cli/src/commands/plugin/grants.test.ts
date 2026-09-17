// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from "bun:test";
import { Command } from "commander";
import { grantReplacement, registerPluginGrantCommands } from "./grants.js";

test("explicit clear revokes all permissions and disables AI quota", () => {
  expect(grantReplacement({ clear: true })).toEqual({
    permissions: [],
    ai_daily_call_limit: 0,
    ai_max_output_tokens: 1024,
  });
});
test("replacement requires explicit authority and rejects ambiguous clearing", () => {
  for (const options of [
    {},
    { clear: true, grant: ["ai_generate"] },
    { clear: true, aiDailyCalls: "0" },
    { grant: ["system_admin"] },
  ])
    expect(() => grantReplacement(options)).toThrow();
});
test("grant replacement preserves zero quota and explicit token bound", () => {
  expect(
    grantReplacement({
      grant: ["ai_generate"],
      aiDailyCalls: "0",
      aiMaxTokens: "32",
    }),
  ).toEqual({
    permissions: ["ai_generate"],
    ai_daily_call_limit: 0,
    ai_max_output_tokens: 32,
  });
});
test("grant inspection and replacement are discoverable CLI commands", () => {
  const command = new Command("plugin");
  registerPluginGrantCommands(command);
  expect(command.commands[0]?.commands.map((c) => c.name())).toEqual([
    "get",
    "set",
  ]);
  expect(command.commands[0]?.commands[1]?.helpInformation()).toContain(
    "--clear",
  );
});
