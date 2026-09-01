// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createManifest,
  runPlugin,
  type PluginContext,
  type PluginEvent,
  type ProjectInfo,
  type RequestHandler,
  type TempsPlugin,
} from "@temps-sdk/plugin";
import { embeddedAssets } from "./_embedded_ui.js";
import { buildOverview, summarizeProject, type ProjectPulse } from "./overview.js";

const MAX_CONCURRENT_PROJECT_QUERIES = 6;

async function loadProjectPulses(
  ctx: PluginContext,
  projects: ProjectInfo[],
): Promise<ProjectPulse[]> {
  const results: ProjectPulse[] = [];

  for (let offset = 0; offset < projects.length; offset += MAX_CONCURRENT_PROJECT_QUERIES) {
    const batch = projects.slice(offset, offset + MAX_CONCURRENT_PROJECT_QUERIES);
    const pulses = await Promise.all(
      batch.map(async (project): Promise<ProjectPulse> => {
        try {
          const deployments = await ctx.temps.listDeployments(project.id, { limit: 10 });
          return summarizeProject(project, deployments);
        } catch (error) {
          console.error(
            JSON.stringify({
              level: "warn",
              message: "Could not load deployment history",
              project_id: project.id,
              reason: error instanceof Error ? error.message : String(error),
            }),
          );
          return {
            ...summarizeProject(project, []),
            health: "unknown",
            error: "Deployment history is temporarily unavailable",
          };
        }
      }),
    );
    results.push(...pulses);
  }

  return results;
}

function json(res: Parameters<RequestHandler>[1], status: number, body: unknown): void {
  res.writeHead(status, {
    "Content-Type": "application/json; charset=utf-8",
    "Cache-Control": "no-store",
  });
  res.end(JSON.stringify(body));
}

const plugin: TempsPlugin = {
  manifest() {
    return createManifest("deployment-pulse", "0.1.0")
      .displayName("Deployment Pulse")
      .description("See deployment health across every project at a glance")
      .requiresDb(false)
      .addNav("Deployment Pulse", "activity", "/")
      .event("deployment.succeeded")
      .event("deployment.failed")
      .build();
  },

  embeddedUiAssets() {
    return embeddedAssets;
  },

  handler(ctx: PluginContext): RequestHandler {
    return async (req, res) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (req.method !== "GET" || url.pathname !== "/overview") {
        json(res, 404, { error: "Not found" });
        return;
      }

      try {
        const projects = await ctx.temps.listProjects();
        const pulses = await loadProjectPulses(ctx, projects);
        json(res, 200, buildOverview(pulses));
      } catch (error) {
        console.error(
          JSON.stringify({
            level: "error",
            message: "Could not build deployment overview",
            reason: error instanceof Error ? error.message : String(error),
          }),
        );
        json(res, 502, {
          error: "Deployment data is temporarily unavailable",
          retryable: true,
        });
      }
    };
  },

  onStart() {
    console.error(JSON.stringify({ level: "info", message: "Deployment Pulse started" }));
  },

  onEvent(_ctx: PluginContext, event: PluginEvent) {
    console.error(
      JSON.stringify({
        level: "info",
        message: "Deployment state changed",
        event_type: event.event_type,
        project_id: event.project_id,
      }),
    );
  },

  onShutdown() {
    console.error(JSON.stringify({ level: "info", message: "Deployment Pulse stopped" }));
  },
};

await runPlugin(plugin);
