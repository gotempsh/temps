// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Pool } from "pg";
import { countryForRequest } from "../geo.js";
import { errorFields, log } from "../log.js";
import { ingestLimitsFromEnv } from "../limits.js";
import {
  createRateLimiter,
  rateLimitAddress,
  rateLimitedResponse,
  type RateLimiter,
} from "../rate-limit.js";


// This endpoint is deliberately unauthenticated (any self-hosted instance in
// the world can call it), so it has no API key or session to fall back on for
// abuse control. A generous per-IP rate limit is the floor: high enough that a
// real instance sending a real report never notices it, low enough that a
// flood can't grow `deploy_failure_reports` unbounded. Tuned with
// FAILURE_REPORT_RATE_LIMIT_PER_MINUTE (see limits.ts).

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

// maxTextChars is a generous ceiling on the redacted trace text: large enough
// for a real multi-stage build log, small enough that a misbehaving/malicious
// client can't use this as an unbounded blob store (FAILURE_REPORT_MAX_CHARS).
function parseFailureReport(
  raw: unknown,
  maxTextChars: number
): FailureReportBody | { error: string } {
  if (typeof raw !== "object" || raw === null) {
    return { error: "body must be an object" };
  }
  const obj = raw as Record<string, unknown>;

  if (!isNonEmptyString(obj.report_text)) {
    return { error: "report_text must be a non-empty string" };
  }
  if (obj.report_text.length > maxTextChars) {
    return { error: `report_text exceeds ${maxTextChars} chars` };
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

export interface FailureReportsRoutesDeps {
  rateLimiter?: RateLimiter;
  maxTextChars?: number;
}

export function createFailureReportsRoutes(pool: Pool, deps: FailureReportsRoutesDeps = {}) {
  const rateLimiter =
    deps.rateLimiter ?? createRateLimiter(ingestLimitsFromEnv().failureReportsPerMinute);
  const maxTextChars = deps.maxTextChars ?? ingestLimitsFromEnv().failureReportMaxChars;

  return {
    // POST /v1/deploy-failure-reports
    // `peer` is the TCP peer address, from the server.
    async postReport(req: Request, peer: string | null = null): Promise<Response> {
      const ip = rateLimitAddress(req, peer);
      // No resolvable address (neither a forwarded header nor a peer, which
      // only happens when a handler is called outside the server) fails open
      // -- the per-payload size cap is still in force either way.
      if (ip && rateLimiter.isLimited(ip)) {
        return rateLimitedResponse();
      }

      let body: unknown;
      try {
        body = await req.json();
      } catch {
        return Response.json({ error: "invalid JSON body" }, { status: 400 });
      }

      const parsed = parseFailureReport(body, maxTextChars);
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
        log("error", "failure-reports", "db insert failed", errorFields(err));
        return Response.json({ error: "internal server error" }, { status: 500 });
      }

      return Response.json({ ok: true }, { status: 201 });
    },
  };
}
