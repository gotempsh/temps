import type { Pool } from "pg";
import { countryForRequest } from "../geo.js";

// A generous ceiling on the redacted trace text — large enough for a real
// multi-stage build log, small enough that a misbehaving/malicious client
// can't use this as an unbounded blob store.
const MAX_REPORT_TEXT_CHARS = 200_000;

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
