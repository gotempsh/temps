// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getPool } from "./db/pool.js";
import { createEventsRoutes } from "./routes/events.js";
import { createStatsRoutes } from "./routes/stats.js";
import { createFailureReportsRoutes } from "./routes/failure-reports.js";
import { initGeo } from "./geo.js";
import { CountryBackfiller } from "./backfill.js";
import { gracefulShutdown } from "./shutdown.js";
import { startServer } from "./server.js";
import { errorFields, log } from "./log.js";
import { conflictingLimits, ingestLimitsFromEnv } from "./limits.js";
import { createRateLimiter } from "./rate-limit.js";

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

  // Abuse limits for the public ingest routes; see limits.ts for each
  // variable and its default.
  const limits = ingestLimitsFromEnv();
  log("info", "server", "ingest limits", { ...limits });
  for (const conflict of conflictingLimits(limits)) {
    log("warn", "config", "ingest limits contradict each other", { conflict });
  }

  const events = createEventsRoutes(pool, {
    backfill,
    rateLimiter: createRateLimiter(limits.eventsPerMinute),
    limits,
  });
  const stats = createStatsRoutes(pool);
  const failureReports = createFailureReportsRoutes(pool, {
    rateLimiter: createRateLimiter(limits.failureReportsPerMinute),
    maxTextChars: limits.failureReportMaxChars,
  });

  const server = startServer({
    port: PORT,
    maxRequestBodyBytes: limits.maxRequestBodyBytes,
    routes: { events, failureReports, stats },
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
