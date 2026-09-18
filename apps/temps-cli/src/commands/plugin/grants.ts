// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { Command } from "commander";
import { requireAuth } from "../../config/store.js";
import { setupClient, getErrorMessage } from "../../lib/api-client.js";
import { getPluginGrants, putPluginGrants } from "../../api/sdk.gen.js";
import { installGrants, PluginInstallError, type Options } from "./install.js";

type GrantOptions = Options & { clear?: boolean };

export function grantReplacement(options: GrantOptions) {
  if (options.clear) {
    if (options.grant || options.aiDailyCalls || options.aiMaxTokens)
      throw new PluginInstallError(
        "Use --clear by itself to revoke all host permissions.",
      );
    return {
      permissions: [],
      ai_daily_call_limit: 0,
      ai_max_output_tokens: 1024,
    };
  }
  const grants = installGrants(options);
  if (!grants)
    throw new PluginInstallError(
      "Specify --grant or --clear. Grants replace the complete permission list.",
    );
  return grants;
}

function validPluginName(name: string) {
  if (!/^[a-z0-9][a-z0-9-]{0,63}$/.test(name))
    throw new PluginInstallError("Enter a valid installed plugin name.");
  return name;
}

export function registerPluginGrantCommands(plugin: Command) {
  const grants = plugin
    .command("grants")
    .description(
      "Inspect or replace a plugin's host API permissions and AI limits",
    );
  grants
    .command("get <name>")
    .description("Show current grants, actor identity, and AI availability")
    .action(async (name: string) => {
      validPluginName(name);
      await requireAuth();
      await setupClient();
      try {
        const result = await getPluginGrants({
          path: { name },
          throwOnError: true,
        });
        console.log(JSON.stringify(result.data, null, 2));
      } catch (error) {
        throw new PluginInstallError(getErrorMessage(error));
      }
    });
  grants
    .command("set <name>")
    .description(
      "Replace all host grants; --clear revokes all permissions immediately",
    )
    .option("--grant <permissions...>", "Complete list of permissions to grant")
    .option("--ai-daily-calls <count>", "Daily AI call limit (0–10000)")
    .option(
      "--ai-max-tokens <count>",
      "Maximum AI output tokens per call (1–4096)",
    )
    .option("--clear", "Revoke all host permissions")
    .action(async (name: string, options: GrantOptions) => {
      validPluginName(name);
      const body = grantReplacement(options);
      await requireAuth();
      await setupClient();
      try {
        const result = await putPluginGrants({
          path: { name },
          body,
          throwOnError: true,
        });
        console.log(JSON.stringify(result.data, null, 2));
      } catch (error) {
        throw new PluginInstallError(getErrorMessage(error));
      }
    });
}
