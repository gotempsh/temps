// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getPool } from "./db/pool.js";
import { createEventsRoutes } from "./routes/events.js";
import { createStatsRoutes } from "./routes/stats.js";
import { createFailureReportsRoutes } from "./routes/failure-reports.js";
import { initGeo } from "./geo.js";
import { CountryBackfiller } from "./backfill.js";
import { gracefulShutdown } from "./shutdown.js";
import { errorFields, log } from "./log.js";

const PORT = parseInt(process.env.PORT ?? "4200", 10);

async function main() {
  // Eagerly connect to confirm DB is reachable at startup
  const pool = getPool();
  const client = await pool.connect();
  await client.query("SELECT 1");
  client.release();
  log("info", "server", "database connection ok");

  // Load the GeoLite2-Country DB once. Required in production: a missing or
  // unusable DB fails startup (and so the deploy's health check) instead of
  // silently storing NULL countries. Degrades to null countries elsewhere.
  await initGeo({ required: process.env.NODE_ENV === "production" });

  // Country backfill runs in the background, off the ingest request path.
  const backfill = new CountryBackfiller(pool);
  backfill.start();

  const events = createEventsRoutes(pool, { backfill });
  const stats = createStatsRoutes(pool);
  const failureReports = createFailureReportsRoutes(pool);

  const server = Bun.serve({
    port: PORT,
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

  log("info", "server", "temps telemetry API listening", { port: server.port });

  // On a platform stop: drain in-flight requests, flush the backfill queue,
  // then exit (see shutdown.ts for why the order matters).
  for (const signal of ["SIGTERM", "SIGINT"] as const) {
    process.once(signal, () => {
      void gracefulShutdown(
        {
          stopServer: () => server.stop(),
          stopBackfill: () => backfill.stop(),
          pendingBackfill: () => backfill.pendingCount,
          exit: (code) => process.exit(code),
        },
        signal
      );
    });
  }
}

main().catch((err) => {
  log("error", "server", "startup failed", errorFields(err));
  process.exit(1);
});
