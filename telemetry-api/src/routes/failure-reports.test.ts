import { describe, it, expect, mock } from "bun:test";
import { createFailureReportsRoutes } from "./failure-reports.js";
import type { Pool } from "pg";

function makePool(queryFn: () => unknown = () => ({ rows: [] })) {
  return {
    query: mock(queryFn),
  } as unknown as Pool;
}

function makeReq(body: unknown, method = "POST"): Request {
  return new Request("http://localhost/v1/deploy-failure-reports", {
    method,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

describe("POST /v1/deploy-failure-reports", () => {
  it("accepts a valid report", async () => {
    const pool = makePool();
    const { postReport } = createFailureReportsRoutes(pool);

    const res = await postReport(
      makeReq({
        report_text: "=== build_image === Docker stream error: ...",
        temps_version: "0.1.0",
        project_id: 42,
        deployment_id: 7,
        failed_job_id: "build_image",
        failed_job_type: "BuildImageJob",
      })
    );

    expect(res.status).toBe(201);
    const json = await res.json();
    expect(json.ok).toBe(true);
    expect((pool.query as ReturnType<typeof mock>).mock.calls.length).toBe(1);
  });

  it("accepts a report with no project/deployment ids", async () => {
    const pool = makePool();
    const { postReport } = createFailureReportsRoutes(pool);

    const res = await postReport(
      makeReq({
        report_text: "trace text",
        failed_job_id: "build_image",
        failed_job_type: "BuildImageJob",
      })
    );

    expect(res.status).toBe(201);
  });

  it("rejects invalid JSON", async () => {
    const pool = makePool();
    const { postReport } = createFailureReportsRoutes(pool);

    const req = new Request("http://localhost/v1/deploy-failure-reports", {
      method: "POST",
      body: "not json",
    });
    const res = await postReport(req);
    expect(res.status).toBe(400);
  });

  it("rejects an empty report_text", async () => {
    const pool = makePool();
    const { postReport } = createFailureReportsRoutes(pool);

    const res = await postReport(
      makeReq({ report_text: "   ", failed_job_id: "x", failed_job_type: "y" })
    );
    expect(res.status).toBe(422);
  });

  it("rejects report_text over the length cap", async () => {
    const pool = makePool();
    const { postReport } = createFailureReportsRoutes(pool);

    const res = await postReport(
      makeReq({
        report_text: "a".repeat(200_001),
        failed_job_id: "build_image",
        failed_job_type: "BuildImageJob",
      })
    );
    expect(res.status).toBe(422);
  });

  it("rejects missing failed_job_id", async () => {
    const pool = makePool();
    const { postReport } = createFailureReportsRoutes(pool);

    const res = await postReport(
      makeReq({ report_text: "trace", failed_job_type: "BuildImageJob" })
    );
    expect(res.status).toBe(422);
  });

  it("rejects a non-integer project_id", async () => {
    const pool = makePool();
    const { postReport } = createFailureReportsRoutes(pool);

    const res = await postReport(
      makeReq({
        report_text: "trace",
        project_id: "not-a-number",
        failed_job_id: "build_image",
        failed_job_type: "BuildImageJob",
      })
    );
    expect(res.status).toBe(422);
  });
});

describe("POST /v1/deploy-failure-reports rate limiting", () => {
  function makeReqFromIp(ip: string): Request {
    return new Request("http://localhost/v1/deploy-failure-reports", {
      method: "POST",
      headers: { "Content-Type": "application/json", "x-forwarded-for": ip },
      body: JSON.stringify({
        report_text: "trace",
        failed_job_id: "build_image",
        failed_job_type: "BuildImageJob",
      }),
    });
  }

  it("allows requests up to the per-IP limit, then 429s", async () => {
    const pool = makePool();
    const { postReport } = createFailureReportsRoutes(pool);
    // Default limit is 30/min (FAILURE_REPORT_RATE_LIMIT_PER_MINUTE unset in
    // test env). A unique IP isolates this test's window from any other.
    const ip = "203.0.113.7";

    for (let i = 0; i < 30; i++) {
      const res = await postReport(makeReqFromIp(ip));
      expect(res.status).toBe(201);
    }

    const limited = await postReport(makeReqFromIp(ip));
    expect(limited.status).toBe(429);
  });

  it("tracks separate IPs independently", async () => {
    const pool = makePool();
    const { postReport } = createFailureReportsRoutes(pool);
    const ip = "203.0.113.8";

    for (let i = 0; i < 30; i++) {
      await postReport(makeReqFromIp(ip));
    }
    const limited = await postReport(makeReqFromIp(ip));
    expect(limited.status).toBe(429);

    // A different IP has its own fresh window.
    const other = await postReport(makeReqFromIp("203.0.113.9"));
    expect(other.status).toBe(201);
  });

  it("fails open when no client IP is resolvable", async () => {
    const pool = makePool();
    const { postReport } = createFailureReportsRoutes(pool);
    // No x-forwarded-for / x-real-ip header at all.
    for (let i = 0; i < 40; i++) {
      const res = await postReport(makeReq({ report_text: "trace", failed_job_id: "x", failed_job_type: "y" }));
      expect(res.status).toBe(201);
    }
  });
});
