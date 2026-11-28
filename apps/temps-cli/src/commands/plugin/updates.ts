// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { Command } from "commander";
import { requireAuth } from "../../config/store.js";
import { client, setupClient, getErrorMessage } from "../../lib/api-client.js";
import { PluginCommandError, pluginName } from "./grants.js";
import type { PluginServicesResponse, ServicePlugin } from "./services.js";

/** The deployment an administrator reviewed, given on the command line. */
export function reviewedDeployment(value: string | undefined): number {
  if (value === undefined || !/^[1-9][0-9]{0,9}$/.test(value))
    throw new PluginCommandError(
      'Enter the numeric id of the deployment you reviewed, as shown by "temps plugin update <name>".',
    );
  return Number(value);
}

// Plugin APIs intentionally use local interfaces, outside the canonical core SDK.
interface ApprovedPluginUpdate {
  name: string;
  version: string;
  deployment_id: number;
  difference: { asks_for_more: boolean; added_permissions: string[] };
}

/**
 * What waits for a plugin, as a person reads it: the build it is pinned to,
 * the build that waits and what that one asks for beyond the approved one.
 */
export function updateLines(plugin: ServicePlugin): string[] {
  const pinned =
    plugin.pinned_deployment_id == null
      ? "no build recorded yet"
      : `deployment ${plugin.pinned_deployment_id}${plugin.image ? `, image ${plugin.image}` : ""}`;
  const update = plugin.pending_update;
  if (!update)
    return [
      `Plugin ${plugin.name} runs the build that was approved (${pinned}).`,
      "No update waits. A new deployment of its service will wait for approval.",
    ];
  const { difference } = update;
  const versions = difference.version_from
    ? `${difference.version_from} -> ${difference.version_to}`
    : difference.version_to;
  const lines = [
    `Plugin ${plugin.name} is pinned to ${pinned}.`,
    `Deployment ${update.deployment_id} (version ${versions}${update.image ? `, image ${update.image}` : ""}) waits for approval. The plugin is disconnected until then.`,
  ];
  const asked: Array<[string, string[]]> = [
    ["New permissions it asks for", difference.added_permissions],
    ["New kinds of API calls", difference.added_capabilities],
    ["New routes open without signing in", difference.added_public_paths],
    ["New events it listens for", difference.added_events],
  ];
  if (!difference.asks_for_more)
    lines.push(
      "It asks for no new access. It is still new code that would run with the plugin's permissions.",
    );
  for (const [title, items] of asked)
    if (items.length > 0) lines.push(`${title}: ${items.join(", ")}`);
  if (difference.added_permissions.length > 0)
    lines.push(
      "Approving does not allow new permissions; grant them afterwards with temps plugin grants set.",
    );
  if (difference.removed_permissions.length > 0)
    lines.push(
      `No longer asks for: ${difference.removed_permissions.join(", ")}`,
    );
  lines.push(
    `Approve: temps plugin approve-update ${plugin.name} --deployment ${update.deployment_id}`,
    "Or roll the service back to the approved deployment to keep that build.",
  );
  return lines;
}

/** The plugin `name` among the plugins that run as services. */
export async function servicePlugin(name: string): Promise<ServicePlugin> {
  const result = await client.get<
    { 200: PluginServicesResponse },
    unknown,
    true
  >({
    url: "/x/plugins/services",
    throwOnError: true,
  });
  const plugin = result.data.plugins.find((entry) => entry.name === name);
  if (!plugin)
    throw new PluginCommandError(
      `No plugin named ${name} is added. List them with temps plugin services.`,
    );
  return plugin;
}

export function registerPluginUpdateCommands(plugin: Command) {
  plugin
    .command("update <name>")
    .description(
      "Show the build a plugin is pinned to and the update that waits for approval, with what it asks for",
    )
    .option("--json", "Output as JSON")
    .action(async (name: string, options: { json?: boolean }) => {
      pluginName(name);
      await requireAuth();
      await setupClient();
      try {
        const found = await servicePlugin(name);
        if (options.json)
          console.log(
            JSON.stringify(
              {
                name: found.name,
                state: found.state,
                pinned_deployment_id: found.pinned_deployment_id ?? null,
                pending_update: found.pending_update ?? null,
              },
              null,
              2,
            ),
          );
        else for (const line of updateLines(found)) console.log(line);
      } catch (error) {
        if (error instanceof PluginCommandError) throw error;
        throw new PluginCommandError(getErrorMessage(error));
      }
    });
  plugin
    .command("approve-update <name>")
    .description(
      "Approve the update of a plugin that waits; an update is never applied without this, and it allows no new permissions",
    )
    .requiredOption(
      "--deployment <id>",
      "Id of the deployment you reviewed, as shown by temps plugin update",
    )
    .action(async (name: string, options: { deployment?: string }) => {
      pluginName(name);
      const deployment = reviewedDeployment(options.deployment);
      await requireAuth();
      await setupClient();
      try {
        const found = await servicePlugin(name);
        const update = found.pending_update;
        if (!update)
          throw new PluginCommandError(
            `No update of plugin ${name} waits for approval: it runs the build that was approved.`,
          );
        // The build that waits may have changed since it was reviewed.
        if (update.deployment_id !== deployment)
          throw new PluginCommandError(
            `The update of plugin ${name} that waits is deployment ${update.deployment_id}, not ${deployment}. Review it with temps plugin update ${name} before approving.`,
          );
        const result = await client.post<
          { 200: ApprovedPluginUpdate },
          unknown,
          true
        >({
          url: `/x/plugins/${name}/update/approve`,
          headers: { "Content-Type": "application/json" },
          body: { review: update.review },
          throwOnError: true,
        });
        console.log(
          `Plugin ${result.data.name} now runs deployment ${result.data.deployment_id} (version ${result.data.version}).`,
        );
        if (result.data.difference.added_permissions.length > 0)
          console.log(
            `It asks for new permissions that are still off: ${result.data.difference.added_permissions.join(", ")}. Grant them with temps plugin grants set ${result.data.name}.`,
          );
      } catch (error) {
        if (error instanceof PluginCommandError) throw error;
        throw new PluginCommandError(getErrorMessage(error));
      }
    });
}
