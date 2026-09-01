// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from "bun:test";
import type { DeploymentInfo, ProjectInfo } from "@temps-sdk/plugin";
import { buildOverview, classifyDeployment, summarizeProject } from "./overview.js";

const project = {
  id: 7,
  name: "Registry",
  slug: "registry",
  preset: "nextjs",
  source_type: "git",
} as ProjectInfo;

function deployment(id: number, state: string, createdAt: string): DeploymentInfo {
  return {
    id,
    project_id: project.id,
    environment_id: 1,
    state,
    created_at: createdAt,
  };
}

describe("classifyDeployment", () => {
  it("normalizes the states emitted by current and older Temps hosts", () => {
    expect(classifyDeployment("deployed")).toBe("healthy");
    expect(classifyDeployment("COMPLETED")).toBe("healthy");
    expect(classifyDeployment("running")).toBe("active");
    expect(classifyDeployment("cancelled")).toBe("failed");
    expect(classifyDeployment("stopped")).toBe("paused");
    expect(classifyDeployment("future-state")).toBe("unknown");
    expect(classifyDeployment()).toBe("never");
  });
});

describe("summarizeProject", () => {
  it("uses the newest deployment and calculates the completed success rate", () => {
    const pulse = summarizeProject(
      project,
      [
        deployment(1, "failed", "2026-08-31T08:00:00.000Z"),
        deployment(3, "running", "2026-09-01T10:00:00.000Z"),
        deployment(2, "deployed", "2026-09-01T09:00:00.000Z"),
      ],
      new Date("2026-09-01T12:00:00.000Z"),
    );

    expect(pulse.health).toBe("active");
    expect(pulse.latestDeployment?.id).toBe(3);
    expect(pulse.recentFailures).toBe(1);
    expect(pulse.successRate).toBe(50);
    expect(pulse.deployments24h).toBe(2);
    expect(pulse.failures24h).toBe(0);
  });

  it("reports a project with no deployments without inventing a success rate", () => {
    const pulse = summarizeProject(project, []);
    expect(pulse.health).toBe("never");
    expect(pulse.latestDeployment).toBeNull();
    expect(pulse.successRate).toBeNull();
  });
});

describe("buildOverview", () => {
  it("counts current health and puts projects needing attention first", () => {
    const now = new Date("2026-09-01T12:00:00.000Z");
    const failed = summarizeProject(
      { ...project, id: 8, name: "API" },
      [deployment(4, "failed", "2026-09-01T11:00:00.000Z")],
      now,
    );
    const healthy = summarizeProject(
      project,
      [deployment(5, "deployed", "2026-09-01T10:00:00.000Z")],
      now,
    );
    const overview = buildOverview(
      [healthy, failed],
      now,
    );

    expect(overview.summary).toMatchObject({
      total: 2,
      healthy: 1,
      failed: 1,
      deployments24h: 2,
      failures24h: 1,
    });
    expect(overview.projects.map((entry) => entry.name)).toEqual(["API", "Registry"]);
  });
});
