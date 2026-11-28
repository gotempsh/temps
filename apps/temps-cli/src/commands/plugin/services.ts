// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { Command } from "commander";
import { requireAuth } from "../../config/store.js";
import { client, setupClient, getErrorMessage } from "../../lib/api-client.js";
import { PluginCommandError, pluginName } from "./grants.js";

// Plugin APIs intentionally use local interfaces, outside the canonical core SDK.
/** What a build asks for compared with the build that was approved. */
export interface AccessDifference {
  version_from?: string | null;
  version_to: string;
  added_permissions: string[];
  removed_permissions: string[];
  added_capabilities: string[];
  added_public_paths: string[];
  added_events: string[];
  asks_for_more: boolean;
}

/** A build of a plugin's service that waits for an administrator. */
export interface PendingPluginUpdate {
  deployment_id: number;
  /** What an approval must quote: this build answering this manifest. */
  review: string;
  since?: string | null;
  /** The image that waits, when the deployment was made from an image. */
  image?: string | null;
  difference: AccessDifference;
}

/** Memory and CPU limits; an absent value means that one is not capped. */
export interface PluginResourceLimits {
  deployment_id: number;
  memory_limit_mb?: number | null;
  /** 1,000,000 microcores are one core. */
  cpu_limit_microcores?: number | null;
}

export interface ServicePlugin {
  name: string;
  state: "running" | "asleep" | "update_waiting" | "disabled" | "unavailable";
  detail?: string | null;
  project_id?: number | null;
  environment_id?: number | null;
  service_name?: string | null;
  service_slug?: string | null;
  environment_name?: string | null;
  stops_when_idle: boolean;
  deployment_id?: number | null;
  /** Who stands behind the plugin; a community plugin has the least access. */
  tier: "community" | "verified";
  /** The managed databases linked to the plugin's service: its own data. */
  own_storage: string[];
  /** The deployment whose build was approved; no other is attached to. */
  pinned_deployment_id?: number | null;
  /** A newer build that waits for approval and is never applied on its own. */
  pending_update?: PendingPluginUpdate | null;
  /** When an administrator switched the plugin off, and their user id. */
  disabled_at?: string | null;
  disabled_by?: number | null;
  /** The limits the plugin's container runs with: those of its service. */
  resource_limits?: PluginResourceLimits | null;
  /** The image the plugin runs, when its service was deployed from one. */
  image?: string | null;
}

/** A plugin an earlier version installed as an executable; it never runs. */
interface RetiredPlugin {
  name: string;
  version?: string | null;
  reason: string;
}

export interface PluginServicesResponse {
  configured: boolean;
  reason?: string | null;
  setup_path?: string | null;
  plugins: ServicePlugin[];
  retired: RetiredPlugin[];
}

interface RemovedPlugin {
  message: string;
}

interface AddedServicePlugin {
  name: string;
  version: string;
  display_name?: string | null;
  project_id: number;
  environment_id: number;
}

/** A positive integer id given on the command line. */
export function serviceId(value: string | undefined, what: string): number {
  if (value === undefined || !/^[1-9][0-9]{0,9}$/.test(value))
    throw new PluginCommandError(
      `Enter the numeric id of the ${what}, as shown by "temps projects list" and "temps environments list".`,
    );
  return Number(value);
}

export function registerPluginServiceCommands(plugin: Command) {
  plugin
    .command("services")
    .description(
      "List the plugins, the service each runs as and whether it is running; also lists plugins that no longer run, with the reason",
    )
    .action(async () => {
      await requireAuth();
      await setupClient();
      try {
        const result = await client.get<
          { 200: PluginServicesResponse },
          unknown,
          true
        >({
          url: "/x/plugins/services",
          throwOnError: true,
        });
        console.log(JSON.stringify(result.data, null, 2));
      } catch (error) {
        throw new PluginCommandError(getErrorMessage(error));
      }
    });
  plugin
    .command("add-service")
    .description(
      "Make a deployed service a plugin; it starts with no permissions",
    )
    .requiredOption("--project <id>", "Id of the service the plugin runs as")
    .requiredOption(
      "--environment <id>",
      "Id of the environment whose deployment is the plugin",
    )
    .action(async (options: { project?: string; environment?: string }) => {
      const body = {
        project_id: serviceId(options.project, "service"),
        environment_id: serviceId(options.environment, "environment"),
      };
      await requireAuth();
      await setupClient();
      try {
        const result = await client.post<
          { 201: AddedServicePlugin },
          unknown,
          true
        >({
          url: "/x/plugins/services",
          headers: { "Content-Type": "application/json" },
          body,
          throwOnError: true,
        });
        console.log(JSON.stringify(result.data, null, 2));
      } catch (error) {
        throw new PluginCommandError(getErrorMessage(error));
      }
    });
  plugin
    .command("remove <name>")
    .description(
      "Remove a plugin and revoke its permissions; its service and the files it stored are kept",
    )
    .action(async (name: string) => {
      pluginName(name);
      await requireAuth();
      await setupClient();
      try {
        const result = await client.post<{ 200: RemovedPlugin }, unknown, true>(
          {
            url: `/x/plugins/${name}/uninstall`,
            throwOnError: true,
          },
        );
        console.log(result.data.message);
      } catch (error) {
        throw new PluginCommandError(getErrorMessage(error));
      }
    });
}
