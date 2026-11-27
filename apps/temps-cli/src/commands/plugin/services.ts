// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { Command } from "commander";
import { requireAuth } from "../../config/store.js";
import { client, setupClient, getErrorMessage } from "../../lib/api-client.js";
import { PluginInstallError } from "./install.js";

// Plugin APIs intentionally use local interfaces, outside the canonical core SDK.
interface ServicePlugin {
  name: string;
  state: "running" | "asleep" | "unavailable";
  detail?: string | null;
  project_id?: number | null;
  environment_id?: number | null;
  service_name?: string | null;
  service_slug?: string | null;
  environment_name?: string | null;
  stops_when_idle: boolean;
  deployment_id?: number | null;
}

interface PluginServicesResponse {
  configured: boolean;
  reason?: string | null;
  setup_path?: string | null;
  plugins: ServicePlugin[];
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
    throw new PluginInstallError(
      `Enter the numeric id of the ${what}, as shown by "temps projects list" and "temps environments list".`,
    );
  return Number(value);
}

export function registerPluginServiceCommands(plugin: Command) {
  plugin
    .command("services")
    .description(
      "List the plugins that run as Temps services and whether each is running",
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
        throw new PluginInstallError(getErrorMessage(error));
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
        throw new PluginInstallError(getErrorMessage(error));
      }
    });
}
