// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect, mock } from "bun:test";
import type { Pool } from "pg";
import { createStatsRoutes } from "./stats.js";

describe("GET /v1/stats/funnel", () => {
  it("returns per-instance counts without instance identifiers", async () => {
    const pool = {
      query: mock(() => ({
        rows: [
          { anonymous_id: "inst_secret_1", attempted: "3", succeeded: "2", failed: "1" },
          { anonymous_id: "inst_secret_2", attempted: "1", succeeded: "0", failed: "1" },
        ],
      })),
    } as unknown as Pool;

    const res = await createStatsRoutes(pool).getFunnel(new Request("http://localhost/v1/stats/funnel"));
    const body = await res.json();

    expect(JSON.stringify(body)).not.toContain("inst_secret");
    expect(body.cohorts).toEqual([
      { attempted: 3, succeeded: 2, failed: 1 },
      { attempted: 1, succeeded: 0, failed: 1 },
    ]);
    expect(body.total_instances_with_deploys).toBe(2);
    expect(body.instances_with_at_least_one_success).toBe(1);
    expect(body.instances_that_never_succeeded).toBe(1);
  });
});
