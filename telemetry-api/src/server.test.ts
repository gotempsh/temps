// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, it, mock } from "bun:test";
import type { Server } from "bun";
import type { Pool } from "pg";
import { startServer } from "./server.js";
import { createEventsRoutes } from "./routes/events.js";
import { createFailureReportsRoutes } from "./routes/failure-reports.js";
import { createStatsRoutes } from "./routes/stats.js";
import { createRateLimiter } from "./rate-limit.js";

const BODY_CAP = 2048;

let server: Server<undefined> | null = null;

afterEach(() => {
  server?.stop(true);
  server = null;
});

// A real Bun server on an ephemeral port, wired to routes over a mock pool.
function start() {
  const query = mock(() => ({ rows: [] }));
  const pool = { query } as unknown as Pool;
  server = startServer({
    port: 0,
    maxRequestBodyBytes: BODY_CAP,
    routes: {
      events: createEventsRoutes(pool, {
        backfill: { enqueue: () => {} },
        rateLimiter: createRateLimiter(1000),
        limits: { eventPropertiesMaxBytes: 1_000_000, eventsBatchMaxEvents: 100, tempsVersionMaxChars: 64 },
      }),
      failureReports: createFailureReportsRoutes(pool, {
        rateLimiter: createRateLimiter(1000),
        maxTextChars: 1_000_000,
      }),
      stats: createStatsRoutes(pool),
    },
  });
  return { url: `http://localhost:${server.port}`, query };
}

const event = (properties: Record<string, unknown> = {}) => ({
  anonymous_id: "instance-abcdef12",
  event_type: "deploy_attempted",
  properties,
});

function post(url: string, body: unknown): Promise<Response> {
  return fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

describe("startServer request body cap", () => {
  it("accepts a body under the cap", async () => {
    const { url, query } = start();
    const res = await post(`${url}/v1/events`, event({ note: "small" }));
    expect(res.status).toBe(201);
    expect(query.mock.calls.length).toBeGreaterThan(0);
  });

  it("answers 413 above the cap, on every ingest route, without a database write", async () => {
    const { url, query } = start();
    // Over the cap, though every route-level limit here would accept it.
    const blob = "x".repeat(BODY_CAP * 2);

    const single = await post(`${url}/v1/events`, event({ blob }));
    const batch = await post(`${url}/v1/events/batch`, { events: [event({ blob })] });
    const report = await post(`${url}/v1/deploy-failure-reports`, {
      report_text: blob,
      failed_job_id: "build_image",
      failed_job_type: "BuildImageJob",
    });

    expect(single.status).toBe(413);
    expect(batch.status).toBe(413);
    expect(report.status).toBe(413);
    expect(query.mock.calls.length).toBe(0);
  });

  it("routes health checks and unknown paths", async () => {
    const { url } = start();
    expect((await fetch(`${url}/health`)).status).toBe(200);
    expect((await fetch(`${url}/nope`)).status).toBe(404);
  });
});
