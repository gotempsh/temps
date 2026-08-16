import type { Pool } from "pg";
import { countryForRequest, clientIpFromHeaders } from "../geo.js";

// A generous ceiling on the redacted trace text — large enough for a real
// multi-stage build log, small enough that a misbehaving/malicious client
// can't use this as an unbounded blob store.
const MAX_REPORT_TEXT_CHARS = 200_000;

// This endpoint is deliberately unauthenticated (any self-hosted instance in
// the world can call it), so it's the one ingest route here with no API key
// or session to fall back on for abuse control. A generous per-IP rate limit
// is the floor: high enough that a real instance sending a real report never
// notices it, low enough that a flood can't grow `deploy_failure_reports`
// unbounded. Configurable via env because "generous" depends on deployment
// scale (e.g. behind a shared proxy that NATs many real instances to one IP).
const RATE_LIMIT_PER_MINUTE = Math.max(
  1,
  parseInt(process.env.FAILURE_REPORT_RATE_LIMIT_PER_MINUTE ?? "30", 10) || 30
);
const RATE_LIMIT_WINDOW_MS = 60_000;

// In-memory sliding-window counter, keyed by client IP. Fine for a
// single-process service (this one is): no shared state needed, and a
// process restart merely resets everyone's window rather than under- or
// over-counting. Swept lazily on each check so the map can't grow unbounded
// from one-off IPs.
const requestLog = new Map<string, number[]>();

function isRateLimited(ip: string): boolean {
  const now = Date.now();
  const cutoff = now - RATE_LIMIT_WINDOW_MS;
  const timestamps = (requestLog.get(ip) ?? []).filter((t) => t > cutoff);

  if (timestamps.length >= RATE_LIMIT_PER_MINUTE) {
    requestLog.set(ip, timestamps);
    return true;
  }

  timestamps.push(now);
  requestLog.set(ip, timestamps);

  // Occasional sweep of fully-expired IPs so long-running processes don't
  // accumulate an ever-growing map of one-off callers.
  if (requestLog.size > 10_000) {
    for (const [key, ts] of requestLog) {
      if (ts.every((t) => t <= cutoff)) requestLog.delete(key);
    }
  }

  return false;
}

interface FailureReportBody {
  report_text: string;
  temps_version?: string;
  project_id?: number;
  deployment_id?: number;
  failed_job_id: string;
  failed_job_type: string;
}

function isNonEmptyString(v: unknown): v is string {
  return typeof v === "string" && v.trim().length > 0;
}

function isOptionalInt(v: unknown): v is number | undefined {
  return v === undefined || (typeof v === "number" && Number.isInteger(v));
}

function parseFailureReport(raw: unknown): FailureReportBody | { error: string } {
  if (typeof raw !== "object" || raw === null) {
    return { error: "body must be an object" };
  }
  const obj = raw as Record<string, unknown>;

  if (!isNonEmptyString(obj.report_text)) {
    return { error: "report_text must be a non-empty string" };
  }
  if (obj.report_text.length > MAX_REPORT_TEXT_CHARS) {
    return { error: `report_text exceeds ${MAX_REPORT_TEXT_CHARS} chars` };
  }
  if (!isNonEmptyString(obj.failed_job_id)) {
    return { error: "failed_job_id must be a non-empty string" };
  }
  if (!isNonEmptyString(obj.failed_job_type)) {
    return { error: "failed_job_type must be a non-empty string" };
  }
  if (obj.temps_version !== undefined && typeof obj.temps_version !== "string") {
    return { error: "temps_version must be a string" };
  }
  if (!isOptionalInt(obj.project_id)) {
    return { error: "project_id must be an integer" };
  }
  if (!isOptionalInt(obj.deployment_id)) {
    return { error: "deployment_id must be an integer" };
  }

  return {
    report_text: obj.report_text,
    temps_version: obj.temps_version as string | undefined,
    project_id: obj.project_id as number | undefined,
    deployment_id: obj.deployment_id as number | undefined,
    failed_job_id: obj.failed_job_id,
    failed_job_type: obj.failed_job_type,
  };
}

export function createFailureReportsRoutes(pool: Pool) {
  return {
    // POST /v1/deploy-failure-reports
    async postReport(req: Request): Promise<Response> {
      const ip = clientIpFromHeaders(req);
      // No resolvable IP (e.g. direct connection with no proxy headers) fails
      // open rather than blocking every request behind a misconfigured proxy
      // -- the per-payload size cap is still in force either way.
      if (ip && isRateLimited(ip)) {
        return Response.json(
          { error: "rate limit exceeded, try again shortly" },
          { status: 429 }
        );
      }

      let body: unknown;
      try {
        body = await req.json();
      } catch {
        return Response.json({ error: "invalid JSON body" }, { status: 400 });
      }

      const parsed = parseFailureReport(body);
      if ("error" in parsed) {
        return Response.json({ error: parsed.error }, { status: 422 });
      }

      const country = countryForRequest(req);

      try {
        await pool.query(
          `INSERT INTO deploy_failure_reports
             (report_text, temps_version, project_id, deployment_id, failed_job_id, failed_job_type, country)
           VALUES ($1, $2, $3, $4, $5, $6, $7)`,
          [
            parsed.report_text,
            parsed.temps_version ?? null,
            parsed.project_id ?? null,
            parsed.deployment_id ?? null,
            parsed.failed_job_id,
            parsed.failed_job_type,
            country,
          ]
        );
      } catch (err) {
        console.error("[failure-reports] db insert failed:", err);
        return Response.json({ error: "internal server error" }, { status: 500 });
      }

      return Response.json({ ok: true }, { status: 201 });
    },
  };
}
