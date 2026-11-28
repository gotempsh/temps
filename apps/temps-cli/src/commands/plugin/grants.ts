// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { Command } from "commander";
import { requireAuth } from "../../config/store.js";
import { client, setupClient, getErrorMessage } from "../../lib/api-client.js";

/** A plugin command that cannot run as asked, with the reason to show. */
export class PluginCommandError extends Error {
  override name = "PluginCommandError";
}

/** A plugin name as Temps accepts it. */
export function pluginName(name: string): string {
  if (!/^[a-z0-9][a-z0-9-]{0,63}$/.test(name))
    throw new PluginCommandError(
      "Enter a plugin name: lowercase letters, digits and hyphens, at most 64 characters.",
    );
  return name;
}

const HOST_PERMISSIONS = [
  "ai_generate",
  "projects_read",
  "environments_read",
  "deployments_read",
  "events_read",
  "api_read",
  "api_write",
] as const;

/** The areas of Temps a permission to change can be narrowed to. */
const WRITE_AREAS = ["projects", "environments", "deployments"] as const;

/** How many services a permission to change can be limited to. */
const MAX_WRITE_PROJECTS = 100;

type GrantConfig = {
  permissions: (typeof HOST_PERMISSIONS)[number][];
  ai_daily_call_limit: number;
  ai_max_output_tokens: number;
  /** What `api_write` may change. Empty: nothing. */
  write_areas: (typeof WRITE_AREAS)[number][];
  /** The services its changes are limited to; `null` is every service. */
  write_project_ids: number[] | null;
};

export type GrantOptions = {
  grant?: readonly string[];
  aiDailyCalls?: string;
  aiMaxTokens?: string;
  write?: readonly string[];
  writeProject?: readonly string[];
  clear?: boolean;
};

/** Where a permission to change reaches, from `--write` and `--write-project`. */
function writeScope(
  options: GrantOptions,
  permissions: readonly string[],
): Pick<GrantConfig, "write_areas" | "write_project_ids"> {
  const areas = [...new Set(options.write ?? [])];
  if (
    areas.some(
      (area) => !WRITE_AREAS.includes(area as (typeof WRITE_AREAS)[number]),
    )
  )
    throw new PluginCommandError(
      `Supported --write values: ${WRITE_AREAS.join(", ")}.`,
    );
  if (
    (options.write !== undefined || options.writeProject !== undefined) &&
    !permissions.includes("api_write")
  )
    throw new PluginCommandError(
      "--write and --write-project require an explicit --grant api_write.",
    );
  if (options.writeProject === undefined)
    return {
      write_areas: areas as GrantConfig["write_areas"],
      write_project_ids: null,
    };
  const projects = [...new Set(options.writeProject)];
  if (
    projects.length === 0 ||
    projects.length > MAX_WRITE_PROJECTS ||
    projects.some((id) => !/^[1-9]\d*$/.test(id))
  )
    throw new PluginCommandError(
      `--write-project takes 1 to ${MAX_WRITE_PROJECTS} service ids; omit it to allow every service.`,
    );
  return {
    write_areas: areas as GrantConfig["write_areas"],
    write_project_ids: projects.map(Number),
  };
}

/**
 * The grants the flags ask for, or nothing when no flag was given. A plugin
 * is only ever granted what the operator names; AI limits need the AI grant.
 */
export function requestedGrants(options: GrantOptions): GrantConfig | undefined {
  if (
    !options.grant &&
    options.aiDailyCalls === undefined &&
    options.aiMaxTokens === undefined &&
    options.write === undefined &&
    options.writeProject === undefined
  )
    return undefined;
  const permissions = [...new Set(options.grant ?? [])];
  if (
    permissions.some(
      (permission) =>
        !HOST_PERMISSIONS.includes(
          permission as GrantConfig["permissions"][number],
        ),
    )
  )
    throw new PluginCommandError(
      `Supported --grant values: ${HOST_PERMISSIONS.join(", ")}.`,
    );
  function limit(
    value: string | undefined,
    fallback: number,
    min: number,
    max: number,
    flag: string,
  ): number {
    if (value === undefined) return fallback;
    if (!/^\d+$/.test(value) || Number(value) < min || Number(value) > max)
      throw new PluginCommandError(
        `${flag} must be an integer between ${min} and ${max}.`,
      );
    return Number(value);
  }
  if (
    (options.aiDailyCalls !== undefined || options.aiMaxTokens !== undefined) &&
    !permissions.includes("ai_generate")
  )
    throw new PluginCommandError(
      "AI limits require an explicit --grant ai_generate.",
    );
  return {
    permissions: permissions as GrantConfig["permissions"],
    ai_daily_call_limit: limit(
      options.aiDailyCalls,
      100,
      0,
      10000,
      "--ai-daily-calls",
    ),
    ai_max_output_tokens: limit(
      options.aiMaxTokens,
      1024,
      1,
      4096,
      "--ai-max-tokens",
    ),
    ...writeScope(options, permissions),
  };
}

// Plugin APIs intentionally use local interfaces, outside the canonical core SDK.
interface PluginGrantsResponse {
  actor: { id: string; name: string; active: boolean };
  ai: {
    configured: boolean;
    daily_call_limit: number;
    max_output_tokens: number;
    max_prompt_bytes: number;
    reason?: string | null;
    setup_path: string;
  };
  permissions: string[];
  requested_permissions: string[];
  write_areas: string[];
  write_project_ids?: number[] | null;
  public_paths: string[];
  events: string[];
  outbound_network: { profile: string; detail: string; settings_path: string };
}


export function grantReplacement(options: GrantOptions) {
  if (options.clear) {
    if (
      options.grant ||
      options.aiDailyCalls ||
      options.aiMaxTokens ||
      options.write ||
      options.writeProject
    )
      throw new PluginCommandError(
        "Use --clear by itself to revoke all host permissions.",
      );
    return {
      permissions: [],
      ai_daily_call_limit: 0,
      ai_max_output_tokens: 1024,
      write_areas: [],
      write_project_ids: null,
    };
  }
  const grants = requestedGrants(options);
  if (!grants)
    throw new PluginCommandError(
      "Specify --grant or --clear. Grants replace the complete permission list.",
    );
  return grants;
}

export function registerPluginGrantCommands(plugin: Command) {
  const grants = plugin
    .command("grants")
    .description(
      "Inspect or replace a plugin's host API permissions and AI limits",
    );
  grants
    .command("get <name>")
    .description(
      "Show what a plugin asks for, what it is allowed, and what it can reach",
    )
    .action(async (name: string) => {
      pluginName(name);
      await requireAuth();
      await setupClient();
      try {
        const result = await client.get<
          { 200: PluginGrantsResponse },
          unknown,
          true
        >({
          url: `/x/plugins/${name}/grants`,
          throwOnError: true,
        });
        console.log(JSON.stringify(result.data, null, 2));
      } catch (error) {
        throw new PluginCommandError(getErrorMessage(error));
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
    .option(
      "--write <areas...>",
      "With api_write: what may change (projects, environments, deployments). Omitted: nothing",
    )
    .option(
      "--write-project <ids...>",
      "With api_write: the only services that may change. Omitted: every service",
    )
    .option("--clear", "Revoke all host permissions")
    .action(async (name: string, options: GrantOptions) => {
      pluginName(name);
      const body = grantReplacement(options);
      await requireAuth();
      await setupClient();
      try {
        const result = await client.put<
          { 200: PluginGrantsResponse },
          unknown,
          true
        >({
          url: `/x/plugins/${name}/grants`,
          headers: { "Content-Type": "application/json" },
          body,
          throwOnError: true,
        });
        console.log(JSON.stringify(result.data, null, 2));
      } catch (error) {
        throw new PluginCommandError(getErrorMessage(error));
      }
    });
}
