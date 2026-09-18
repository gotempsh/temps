// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { DevError, grants, integer, record } from "./model.js";
import type { PluginHostPermission } from "./model.js";
export interface Fixtures {
  version: 1;
  projects: Record<string, unknown>[];
  environments: Record<string, unknown>[];
  deployments: Record<string, unknown>[];
  ai: {
    mode: "unconfigured" | "success" | "error" | "timeout";
    text: string;
    delayMs: number;
    dailyCallLimit: number;
    concurrency: number;
    maxOutputTokens: number;
  };
}
export function parseFixtures(value: unknown = {}): Fixtures {
  if (!record(value) || (value.version !== undefined && value.version !== 1))
    throw new DevError("Fixtures must be an object with version: 1.");
  const now = "2026-01-01T00:00:00Z";
  const projects = value.projects ?? [
    {
      id: 1,
      name: "demo",
      slug: "demo",
      repo_name: "demo",
      repo_owner: "example",
      main_branch: "main",
      preset: "static",
      source_type: "git",
      created_at: now,
      updated_at: now,
      enable_preview_environments: true,
    },
  ];
  const environments = value.environments ?? [
    {
      id: 1,
      project_id: 1,
      name: "production",
      slug: "production",
      is_preview: false,
      created_at: now,
      updated_at: now,
    },
  ];
  const deployments = value.deployments ?? [
    {
      id: 42,
      project_id: 1,
      environment_id: 1,
      state: "deployed",
      created_at: now,
    },
  ];
  for (const [name, rows] of Object.entries({
    projects,
    environments,
    deployments,
  })) {
    if (
      !Array.isArray(rows) ||
      rows.length > 1000 ||
      rows.some((row) => !record(row))
    )
      throw new DevError(`Fixture ${name} must contain at most 1,000 records.`);
    const ids = rows.map((row) => integer(row.id, `${name}.id`));
    if (new Set(ids).size !== ids.length)
      throw new DevError(`Fixture ${name} IDs must be unique.`);
    const required =
      name === "projects"
        ? [
            "name",
            "slug",
            "repo_name",
            "repo_owner",
            "main_branch",
            "preset",
            "source_type",
            "created_at",
            "updated_at",
          ]
        : name === "environments"
          ? ["name", "slug", "created_at", "updated_at"]
          : ["state", "created_at"];
    for (const row of rows) {
      for (const key of required)
        if (typeof row[key] !== "string")
          throw new DevError(`Fixture ${name}.${key} must be a string.`);
      if (
        name === "projects" &&
        typeof row.enable_preview_environments !== "boolean"
      )
        throw new DevError(
          "Fixture projects.enable_preview_environments must be boolean.",
        );
      if (name === "environments" && typeof row.is_preview !== "boolean")
        throw new DevError("Fixture environments.is_preview must be boolean.");
    }
  }
  const p = projects as Record<string, unknown>[];
  const e = environments as Record<string, unknown>[];
  const d = deployments as Record<string, unknown>[];
  for (const row of [...e, ...d])
    if (!p.some((project) => project.id === row.project_id))
      throw new DevError(
        "Fixture environment/deployment references an unknown project_id.",
      );
  for (const row of d)
    if (
      !e.some(
        (env) =>
          env.id === row.environment_id && env.project_id === row.project_id,
      )
    )
      throw new DevError(
        "Fixture deployment references an unknown environment_id or mismatched project_id.",
      );
  const ai = value.ai ?? {};
  if (
    !record(ai) ||
    !["unconfigured", "success", "error", "timeout"].includes(
      String(ai.mode ?? "unconfigured"),
    ) ||
    (ai.text !== undefined &&
      (typeof ai.text !== "string" || Buffer.byteLength(ai.text) > 65536))
  )
    throw new DevError(
      "Fixture ai needs a valid mode and bounded text (64 KiB maximum).",
    );
  return {
    version: 1,
    projects: p,
    environments: e,
    deployments: d,
    ai: {
      mode: (ai.mode ?? "unconfigured") as Fixtures["ai"]["mode"],
      text: String(
        ai.text ?? "Mock AI response from the local Temps plugin runner.",
      ),
      delayMs: integer(ai.delayMs ?? 0, "ai.delayMs", 0, 5000),
      dailyCallLimit: integer(
        ai.dailyCallLimit ?? 100,
        "ai.dailyCallLimit",
        0,
        10000,
      ),
      concurrency: integer(ai.concurrency ?? 2, "ai.concurrency", 1, 8),
      maxOutputTokens: integer(
        ai.maxOutputTokens ?? 1024,
        "ai.maxOutputTokens",
        1,
        4096,
      ),
    },
  };
}
export class MockHost {
  permissions: PluginHostPermission[];
  private inFlight = 0;
  private calls = 0;
  constructor(
    readonly name: string,
    readonly actorId: string,
    permissionList: PluginHostPermission[],
    readonly fixtures = parseFixtures(),
  ) {
    this.permissions = grants(permissionList);
  }
  capabilities() {
    return {
      actor: { id: this.actorId, name: this.name, active: true },
      permissions: this.permissions,
      ai: {
        configured: this.fixtures.ai.mode !== "unconfigured",
        reason:
          this.fixtures.ai.mode === "unconfigured"
            ? "Local mock AI is not configured. Set ai.mode in --fixtures."
            : null,
        setup_path: "/settings/ai",
        daily_call_limit: this.fixtures.ai.dailyCallLimit,
        max_output_tokens: this.fixtures.ai.maxOutputTokens,
        max_prompt_bytes: 65536,
      },
    };
  }
  async call(
    method: string,
    params: Record<string, unknown>,
  ): Promise<unknown> {
    if (method === "get_host_capabilities") return this.capabilities();
    const needed: Record<string, PluginHostPermission> = {
      get_project: "projects_read",
      list_projects: "projects_read",
      get_environment: "environments_read",
      list_environments: "environments_read",
      get_deployment: "deployments_read",
      list_deployments: "deployments_read",
      get_last_deployment: "deployments_read",
      generate_ai: "ai_generate",
    };
    const permission = needed[method];
    if (!permission)
      throw new DevError(
        `Host method ${method} is not simulated. Add a supported fixture; no request was sent to a real host.`,
        "method_not_found",
      );
    if (!this.permissions.includes(permission))
      throw new DevError(
        `Plugin ${this.name} needs the ${permission} grant.`,
        "permission_denied",
      );
    if (method === "generate_ai") return this.generate(params);
    const table = method.includes("project")
      ? this.fixtures.projects
      : method.includes("environment")
        ? this.fixtures.environments
        : this.fixtures.deployments;
    if (["get_project", "get_environment", "get_deployment"].includes(method)) {
      const key = method.slice(4) + "_id";
      const id = integer(params[key], key);
      const row = table.find((row) => row.id === id);
      if (!row)
        throw new DevError(
          `No ${method.slice(4)} fixture has ID ${id}.`,
          "not_found",
        );
      return row;
    }
    let rows = table;
    if (method !== "list_projects") {
      const id = integer(params.project_id, "project_id");
      rows = rows.filter((r) => r.project_id === id);
    }
    if (params.environment_id != null) {
      const id = integer(params.environment_id, "environment_id");
      rows = rows.filter((r) => r.environment_id === id);
    }
    if (method === "get_last_deployment") {
      const latest = [...rows].sort((a, b) =>
        String(b.created_at).localeCompare(String(a.created_at)),
      )[0];
      if (!latest)
        throw new DevError(
          "No deployment fixture matches the project/environment.",
          "not_found",
        );
      return latest;
    }
    if (method !== "list_deployments") return rows;
    return [...rows]
      .sort((a, b) => String(b.created_at).localeCompare(String(a.created_at)))
      .slice(0, Math.min(integer(params.limit ?? 20, "limit", 0), 100));
  }
  private async generate(params: Record<string, unknown>) {
    const ai = this.fixtures.ai;
    if (
      typeof params.purpose !== "string" ||
      !/^[a-zA-Z0-9_.-]{1,64}$/.test(params.purpose) ||
      typeof params.prompt !== "string" ||
      Buffer.byteLength(params.prompt) > 65536 ||
      (params.system != null &&
        (typeof params.system !== "string" ||
          Buffer.byteLength(params.system) > 16384))
    )
      throw new DevError(
        "AI request has an invalid purpose, prompt, or system message.",
      );
    integer(
      params.max_tokens ?? ai.maxOutputTokens,
      "max_tokens",
      1,
      ai.maxOutputTokens,
    );
    if (
      params.temperature != null &&
      (typeof params.temperature !== "number" ||
        !Number.isFinite(params.temperature) ||
        params.temperature < 0 ||
        params.temperature > 2)
    )
      throw new DevError("temperature must be within 0..2.");
    if (ai.mode === "unconfigured")
      throw new DevError(
        "Mock AI is not configured. Set ai.mode in the fixtures file.",
        "internal",
      );
    if (this.calls >= ai.dailyCallLimit)
      throw new DevError(
        "Mock AI quota reached (counters reset with the runner).",
        "permission_denied",
      );
    if (this.inFlight >= ai.concurrency)
      throw new DevError("Mock AI concurrency limit is busy.", "internal");
    this.calls++;
    this.inFlight++;
    try {
      if (ai.delayMs) await Bun.sleep(ai.delayMs);
      if (ai.mode === "error")
        throw new DevError("Simulated AI provider failure.", "internal");
      if (ai.mode === "timeout")
        throw new DevError("Simulated AI provider timeout.", "internal");
      return { text: ai.text, model: "temps-local-mock" };
    } finally {
      this.inFlight--;
    }
  }
}
