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
