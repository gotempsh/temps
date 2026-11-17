// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// The HTTP server: routing plus the server-level limits Bun enforces before a
// handler runs. Kept apart from index.ts (which connects to the database and
// starts background work) so the whole request path can be tested against a
// real Bun server.

import type { Server } from "bun";
import type { createEventsRoutes } from "./routes/events.js";
import type { createFailureReportsRoutes } from "./routes/failure-reports.js";
import type { createStatsRoutes } from "./routes/stats.js";
import { errorFields, log } from "./log.js";

export interface ServerRoutes {
  events: ReturnType<typeof createEventsRoutes>;
  failureReports: ReturnType<typeof createFailureReportsRoutes>;
  stats: ReturnType<typeof createStatsRoutes>;
}

export interface ServerOptions {
  port: number;
  // Bun answers 413 above this before a handler reads the body. Its own
  // default (128 MiB) would let one request write that much into Postgres.
  maxRequestBodyBytes: number;
  routes: ServerRoutes;
}

export function startServer({ port, maxRequestBodyBytes, routes }: ServerOptions): Server<undefined> {
  const { events, failureReports, stats } = routes;

  return Bun.serve({
    port,
    maxRequestBodySize: maxRequestBodyBytes,
    async fetch(req) {
      const url = new URL(req.url);
      const method = req.method.toUpperCase();
      const path = url.pathname;

      // Health check — no auth required
      if (method === "GET" && path === "/health") {
        return Response.json({ ok: true });
      }

      // Ingest endpoints
      if (method === "POST" && path === "/v1/events") {
        return events.postEvent(req);
      }
      if (method === "POST" && path === "/v1/events/batch") {
        return events.postBatch(req);
      }
      if (method === "POST" && path === "/v1/deploy-failure-reports") {
        return failureReports.postReport(req);
      }

      // Stats endpoints (add auth in production via INGEST_API_KEY or network policy)
      if (method === "GET" && path === "/v1/stats/overview") {
        return stats.getOverview(req);
      }
      if (method === "GET" && path === "/v1/stats/active-instances") {
        return stats.getActiveInstances(req);
      }
      if (method === "GET" && path === "/v1/stats/funnel") {
        return stats.getFunnel(req);
      }
      if (method === "GET" && path === "/v1/stats/countries") {
        return stats.getCountries(req);
      }

      return Response.json({ error: "not found" }, { status: 404 });
    },
    error(err) {
      log("error", "server", "unhandled error", errorFields(err));
      return Response.json({ error: "internal server error" }, { status: 500 });
    },
  });
}
