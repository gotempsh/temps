// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { Command } from "commander";
import { requireAuth } from "../../config/store.js";
import { client, setupClient, getErrorMessage } from "../../lib/api-client.js";
import { PluginCommandError, pluginName } from "./grants.js";
import type { ServicePlugin } from "./services.js";
import { servicePlugin } from "./updates.js";

// Plugin APIs intentionally use local interfaces, outside the canonical core SDK.
interface PluginSwitch {
  name: string;
  disabled: boolean;
  message: string;
}

/**
 * The memory and CPU a plugin's container is limited to, as a person reads
 * it. They are the limits of the plugin's service.
 */
export function limitsLine(plugin: ServicePlugin): string {
  const limits = plugin.resource_limits;
  if (!limits)
    return `Plugin ${plugin.name} has no deployment Temps knows, so no limits are recorded.`;
  const memory =
    limits.memory_limit_mb == null
      ? "memory not capped (it can use all the memory of the server)"
      : `memory ${limits.memory_limit_mb} MB`;
  const cpu =
    limits.cpu_limit_microcores == null
      ? "CPU not capped"
      : `CPU ${Number((limits.cpu_limit_microcores / 1_000_000).toFixed(2))} cores`;
  return `Plugin ${plugin.name} runs with ${memory}, ${cpu} (deployment ${limits.deployment_id}). Change them in the settings of its service.`;
}

async function switchPlugin(name: string, to: "disable" | "enable") {
  pluginName(name);
  await requireAuth();
  await setupClient();
  try {
    const result = await client.post<{ 200: PluginSwitch }, unknown, true>({
      url: `/x/plugins/${name}/${to}`,
      throwOnError: true,
    });
    console.log(result.data.message);
  } catch (error) {
    throw new PluginCommandError(getErrorMessage(error));
  }
}

export function registerPluginControlCommands(plugin: Command) {
  plugin
    .command("disable <name>")
    .description(
      "Cut a plugin off from Temps at once: no requests, no events, no calls from it; its service, permissions and approved build are kept",
    )
    .action((name: string) => switchPlugin(name, "disable"));
  plugin
    .command("enable <name>")
    .description(
      "Let a disabled plugin connect again, with the permissions it had",
    )
    .action((name: string) => switchPlugin(name, "enable"));
  plugin
    .command("limits <name>")
    .description(
      "Show the memory and CPU a plugin's container is limited to: the limits of its service",
    )
    .action(async (name: string) => {
      pluginName(name);
      await requireAuth();
      await setupClient();
      try {
        console.log(limitsLine(await servicePlugin(name)));
      } catch (error) {
        if (error instanceof PluginCommandError) throw error;
        throw new PluginCommandError(getErrorMessage(error));
      }
    });
}
