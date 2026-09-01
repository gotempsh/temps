// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DeploymentInfo, ProjectInfo } from "@temps-sdk/plugin";

export type HealthState =
  | "healthy"
  | "failed"
  | "active"
  | "paused"
  | "never"
  | "unknown";

export interface ProjectPulse {
  id: number;
  name: string;
  slug: string;
  preset: string;
  sourceType: string;
  health: HealthState;
  latestDeployment: DeploymentInfo | null;
  recentDeployments: number;
  recentFailures: number;
  deployments24h: number;
  failures24h: number;
  successRate: number | null;
  error?: string;
}

export interface DeploymentOverview {
  generatedAt: string;
  summary: {
    total: number;
    healthy: number;
    failed: number;
    active: number;
    paused: number;
    never: number;
    unknown: number;
    deployments24h: number;
    failures24h: number;
  };
  projects: ProjectPulse[];
}

const SUCCESS_STATES = new Set(["completed", "deployed", "ready", "succeeded"]);
const FAILED_STATES = new Set(["failed", "cancelled", "canceled", "error"]);
const ACTIVE_STATES = new Set([
  "pending",
  "queued",
  "building",
  "built",
  "deploying",
  "running",
]);
const PAUSED_STATES = new Set(["paused", "stopped"]);

export function classifyDeployment(state?: string): HealthState {
  if (!state) return "never";
  const normalized = state.toLowerCase();
  if (SUCCESS_STATES.has(normalized)) return "healthy";
  if (FAILED_STATES.has(normalized)) return "failed";
  if (ACTIVE_STATES.has(normalized)) return "active";
  if (PAUSED_STATES.has(normalized)) return "paused";
  return "unknown";
}

export function summarizeProject(
  project: ProjectInfo,
  deployments: DeploymentInfo[],
  now = new Date(),
): ProjectPulse {
  const ordered = [...deployments].sort(
    (a, b) => Date.parse(b.created_at) - Date.parse(a.created_at),
  );
  const latestDeployment = ordered[0] ?? null;
  const finished = ordered.filter((deployment) => {
    const health = classifyDeployment(deployment.state);
    return health === "healthy" || health === "failed";
  });
  const successes = finished.filter(
    (deployment) => classifyDeployment(deployment.state) === "healthy",
  ).length;
  const recentFailures = finished.length - successes;
  const cutoff = now.getTime() - 24 * 60 * 60 * 1000;
  const deployments24h = ordered.filter(
    (deployment) => Date.parse(deployment.created_at) >= cutoff,
  );

  return {
    id: project.id,
    name: project.name,
    slug: project.slug,
    preset: project.preset || "custom",
    sourceType: project.source_type,
    health: classifyDeployment(latestDeployment?.state),
    latestDeployment,
    recentDeployments: ordered.length,
    recentFailures,
    deployments24h: deployments24h.length,
    failures24h: deployments24h.filter(
      (deployment) => classifyDeployment(deployment.state) === "failed",
    ).length,
    successRate:
      finished.length === 0 ? null : Math.round((successes / finished.length) * 100),
  };
}

export function buildOverview(
  projects: ProjectPulse[],
  now = new Date(),
): DeploymentOverview {
  const summary = {
    total: projects.length,
    healthy: 0,
    failed: 0,
    active: 0,
    paused: 0,
    never: 0,
    unknown: 0,
    deployments24h: 0,
    failures24h: 0,
  };

  for (const project of projects) {
    summary[project.health] += 1;
    summary.deployments24h += project.deployments24h;
    summary.failures24h += project.failures24h;
  }

  const priority: Record<HealthState, number> = {
    failed: 0,
    active: 1,
    unknown: 2,
    paused: 3,
    healthy: 4,
    never: 5,
  };

  return {
    generatedAt: now.toISOString(),
    summary,
    projects: [...projects].sort((a, b) => {
      const healthOrder = priority[a.health] - priority[b.health];
      return healthOrder || a.name.localeCompare(b.name);
    }),
  };
}
