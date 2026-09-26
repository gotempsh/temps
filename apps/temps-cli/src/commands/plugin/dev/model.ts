// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type {
  PluginEvent,
  PluginManifest,
  PluginHostPermission,
} from "../../../../../../sdks/node/packages/plugin-sdk/src/types.js";
export type { PluginEvent, PluginManifest, PluginHostPermission };
export const MAX_BODY = 1_048_576;
export const permissions = [
  "ai_generate",
  "projects_read",
  "environments_read",
  "deployments_read",
  "events_read",
  "api_read",
  "api_write",
] as const;
export class DevError extends Error {
  constructor(
    message: string,
    readonly code = "invalid_params",
  ) {
    super(message);
    this.name = "PluginDevError";
  }
}
export function record(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}
export function integer(
  value: unknown,
  label: string,
  min = 1,
  max = 2_147_483_647,
): number {
  const n =
    typeof value === "string" && /^\d+$/.test(value) ? Number(value) : value;
  if (typeof n !== "number" || !Number.isSafeInteger(n) || n < min || n > max)
    throw new DevError(
      `${label} must be an integer between ${min} and ${max}.`,
    );
  return n;
}
export function grants(value: unknown): PluginHostPermission[] {
  if (!Array.isArray(value) || value.some((p) => !permissions.includes(p)))
    throw new DevError(
      `Grants must be an array drawn from: ${permissions.join(", ")}.`,
    );
  return [...new Set(value)] as PluginHostPermission[];
}
export function manifest(value: unknown): PluginManifest {
  if (
    !record(value) ||
    typeof value.name !== "string" ||
    !/^[a-z0-9][a-z0-9-]{0,63}$/.test(value.name) ||
    typeof value.version !== "string" ||
    !Array.isArray(value.events) ||
    value.events.some((e) => typeof e !== "string" || e.length > 128) ||
    typeof value.health_path !== "string" ||
    !/^\/(?!\/|_temps|_events)[A-Za-z0-9/_-]*$/.test(value.health_path)
  )
    throw new DevError(
      "Plugin hello has an invalid manifest (name, version, events, or health_path).",
    );
  if (value.requires_host_data_access)
    throw new DevError(
      `Plugin ${value.name} requires privileged host data access, which is not simulated.`,
    );
  if (value.requires_db)
    throw new DevError(
      `Plugin ${value.name} requires a database; the development runner does not provision one.`,
    );
  grants(value.host_permissions ?? []);
  return value as unknown as PluginManifest;
}
export async function readJsonFile(path: string): Promise<unknown> {
  const file = Bun.file(path);
  if (file.size > MAX_BODY)
    throw new DevError(`Fixture ${path} exceeds 1 MiB.`);
  try {
    return await file.json();
  } catch {
    throw new DevError(
      `Cannot read JSON fixture ${path}. Check that it exists and contains valid JSON.`,
    );
  }
}
export const eventTypes = [
  "deployment.created",
  "deployment.succeeded",
  "deployment.failed",
  "deployment.cancelled",
  "deployment.ready",
  "project.created",
  "project.deleted",
  "domain.created",
  "domain.provisioned",
] as const;
export function validateEvent(value: unknown): PluginEvent {
  if (
    !record(value) ||
    typeof value.id !== "string" ||
    !value.id ||
    value.id.length > 128 ||
    typeof value.event_type !== "string" ||
    !/^[a-z][a-z0-9_.-]{0,127}$/.test(value.event_type) ||
    typeof value.timestamp !== "string" ||
    !/^\d{4}-\d\d-\d\dT.*Z$/.test(value.timestamp) ||
    !Number.isFinite(Date.parse(value.timestamp)) ||
    !record(value.data)
  )
    throw new DevError(
      "Event requires id, event_type, UTC timestamp, and an object data payload.",
    );
  if (value.project_id != null) integer(value.project_id, "event.project_id");
  if ((eventTypes as readonly string[]).includes(value.event_type)) {
    integer(value.project_id, "event.project_id");
    if (value.data.project_id !== value.project_id)
      throw new DevError("Event project_id must match data.project_id.");
    const data = value.data;
    const nullable = (key: string) => {
      if (
        !(key in data) ||
        (data[key] !== null && typeof data[key] !== "string")
      )
        throw new DevError(`Event data.${key} must be a string or null.`);
    };
    const text = (key: string) => {
      if (typeof data[key] !== "string")
        throw new DevError(`Event data.${key} must be a string.`);
    };
    if (value.event_type.startsWith("deployment.")) {
      integer(data.deployment_id, "deployment_id");
      integer(data.environment_id, "environment_id");
      text("environment_name");
      if (
        ["deployment.created", "deployment.succeeded"].includes(
          value.event_type,
        )
      )
        nullable("commit_sha");
      if (value.event_type === "deployment.created") nullable("branch");
      if (
        ["deployment.succeeded", "deployment.ready"].includes(value.event_type)
      )
        nullable("url");
      if (value.event_type === "deployment.failed") nullable("error_message");
    } else if (value.event_type.startsWith("project.")) text("project_name");
    else {
      integer(data.domain_id, "domain_id");
      text("domain_name");
    }
  }
  return value as unknown as PluginEvent;
}
export function createEvent(
  type: string,
  options: Record<string, unknown>,
): PluginEvent {
  if (!(eventTypes as readonly string[]).includes(type))
    throw new DevError(
      `Unknown fixture ${type}. Run plugin dev events, or supply a custom envelope with --file.`,
    );
  const project_id = integer(options.projectId ?? 1, "project ID");
  const data: Record<string, unknown> = { project_id };
  if (type.startsWith("deployment.")) {
    Object.assign(data, {
      deployment_id: integer(options.deploymentId ?? 42, "deployment ID"),
      environment_id: integer(options.environmentId ?? 1, "environment ID"),
      environment_name: options.environment ?? "production",
    });
    if (["deployment.created", "deployment.succeeded"].includes(type))
      data.commit_sha = null;
    if (type === "deployment.created") data.branch = "main";
    if (["deployment.succeeded", "deployment.ready"].includes(type))
      data.url = options.url ?? null;
    if (type === "deployment.failed")
      data.error_message = "Simulated deployment failure";
  } else if (type.startsWith("project.")) data.project_name = "demo";
  else Object.assign(data, { domain_id: 1, domain_name: "example.com" });
  return validateEvent({
    id: crypto.randomUUID(),
    event_type: type,
    timestamp: new Date().toISOString(),
    project_id,
    data,
  });
}
export function subscribes(events: string[], type: string): boolean {
  return events.some(
    (e) =>
      e === "*" ||
      e === type ||
      (e.endsWith(".*") && e.slice(0, -2) === type.split(".")[0]),
  );
}
