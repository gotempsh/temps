import type { Pool } from "pg";
import { countryForRequest } from "../geo.js";

// Known event types — kept in lockstep with the Rust binary's
// `TelemetryEventKind::as_str()` (temps-core/src/telemetry.rs). The validator
// rejects anything not in this set so a typo or rogue client can't pollute the
// table. When adding an event, add it in BOTH places.
export const KNOWN_EVENT_TYPES = new Set([
  // Instance lifecycle
  "instance_started",
  "instance_setup_completed",
  "upgrade_completed",
  "worker_node_joined",

  // Deployment funnel
  "deploy_attempted",
  "deploy_succeeded",
  "deploy_failed",
  "rollback_triggered",
  "first_deploy_succeeded",

  // Project & environment
  "project_created",
  "environment_created",
  "scale_to_zero_configured",
  "auto_deploy_enabled",
  "attack_mode_enabled",

  // Git & source
  "git_provider_connected",

  // Domains & networking
  "custom_domain_added",
  "ssl_certificate_issued",

  // Managed services
  "service_created",
  "service_cluster_created",
  "pg_major_upgrade_completed",
  "pitr_restore_triggered",
  "backup_configured",

  // Observability suite activation
  "analytics_first_event_received",
  "session_replay_first_session",
  "error_tracking_first_error",
  "ai_gateway_first_request",

  // AI features
  "ai_sre_conversation_started",
  "autofixer_fix_accepted",
  "autofixer_fix_rejected",

  // Auth & security
  "oidc_provider_configured",
  "api_key_created",
  "vulnerability_scan_triggered",

  // Email
  "email_provider_configured",

  // Status page
  "status_page_published",
]);

interface IngestBody {
  anonymous_id: string;
  event_type: string;
  properties?: Record<string, unknown>;
  temps_version?: string;
  occurred_at?: string;
}

interface BatchIngestBody {
  events: IngestBody[];
}

function isValidAnonymousId(id: unknown): id is string {
  return (
    typeof id === "string" &&
    id.length >= 4 &&
    id.length <= 128 &&
    // UUID-ish or slug — no whitespace
    !/\s/.test(id)
  );
}

function isValidEventType(type: unknown): type is string {
  return typeof type === "string" && KNOWN_EVENT_TYPES.has(type);
}

function isValidProperties(p: unknown): p is Record<string, unknown> {
  return typeof p === "object" && p !== null && !Array.isArray(p);
}

function sanitizeProperties(raw: Record<string, unknown>): Record<string, unknown> {
  // Drop keys that look like PII to be safe
  const PII_KEYS = new Set(["email", "name", "ip", "user_agent", "password", "token"]);
  return Object.fromEntries(
    Object.entries(raw).filter(([k]) => !PII_KEYS.has(k.toLowerCase()))
  );
}

function parseEvent(raw: unknown): IngestBody | { error: string } {
  if (typeof raw !== "object" || raw === null) {
    return { error: "event must be an object" };
  }
  const obj = raw as Record<string, unknown>;

  if (!isValidAnonymousId(obj.anonymous_id)) {
    return { error: "anonymous_id must be a non-empty string (8-128 chars, no whitespace)" };
  }
  if (!isValidEventType(obj.event_type)) {
    return {
      error: `unknown event_type '${obj.event_type}'. Accepted: ${[...KNOWN_EVENT_TYPES].join(", ")}`,
    };
  }

  const properties =
    obj.properties !== undefined && isValidProperties(obj.properties)
      ? sanitizeProperties(obj.properties)
      : {};

  let occurred_at: string | undefined;
  if (obj.occurred_at !== undefined) {
    const d = new Date(obj.occurred_at as string);
    if (isNaN(d.getTime())) {
      return { error: "occurred_at must be a valid ISO 8601 timestamp" };
    }
    occurred_at = d.toISOString();
  }

  return {
    anonymous_id: obj.anonymous_id,
    event_type: obj.event_type,
    properties,
    temps_version: typeof obj.temps_version === "string" ? obj.temps_version : undefined,
    occurred_at,
  };
}

// `country` is the 2-letter ISO code derived from the request IP at ingest time
// (see geo.ts). The IP itself is never passed here or stored — only the country.
async function insertEvent(
  pool: Pool,
  event: IngestBody,
  country: string | null
): Promise<void> {
  await pool.query(
    `INSERT INTO telemetry_events
       (anonymous_id, event_type, properties, temps_version, occurred_at, country)
     VALUES ($1, $2, $3, $4, $5, $6)`,
    [
      event.anonymous_id,
      event.event_type,
      JSON.stringify(event.properties ?? {}),
      event.temps_version ?? null,
      event.occurred_at ?? new Date().toISOString(),
      country,
    ]
  );

  // Upsert the instance-day record for cheap DAI (daily active instances)
  // queries. Backfill country if it was previously null (an instance's country
  // shouldn't change, but the first event of the day may pre-date the lookup).
  await pool.query(
    `INSERT INTO telemetry_instance_days (anonymous_id, day, temps_version, country)
     VALUES ($1, $2::date, $3, $4)
     ON CONFLICT (anonymous_id, day) DO UPDATE
       SET country = COALESCE(telemetry_instance_days.country, EXCLUDED.country)`,
    [
      event.anonymous_id,
      (event.occurred_at ?? new Date().toISOString()).slice(0, 10),
      event.temps_version ?? null,
      country,
    ]
  );
}

export function createEventsRoutes(pool: Pool) {
  return {
    // POST /v1/events — single event
    async postEvent(req: Request): Promise<Response> {
      let body: unknown;
      try {
        body = await req.json();
      } catch {
        return Response.json({ error: "invalid JSON body" }, { status: 400 });
      }

      const parsed = parseEvent(body);
      if ("error" in parsed) {
        return Response.json({ error: parsed.error }, { status: 422 });
      }

      // Derive country from the request IP (never stored) for this request.
      const country = countryForRequest(req);

      try {
        await insertEvent(pool, parsed, country);
      } catch (err) {
        console.error("[events] db insert failed:", err);
        return Response.json({ error: "internal server error" }, { status: 500 });
      }

      return Response.json({ ok: true }, { status: 201 });
    },

    // POST /v1/events/batch — up to 100 events in one request
    async postBatch(req: Request): Promise<Response> {
      let body: unknown;
      try {
        body = await req.json();
      } catch {
        return Response.json({ error: "invalid JSON body" }, { status: 400 });
      }

      const b = body as BatchIngestBody;
      if (!Array.isArray(b?.events)) {
        return Response.json({ error: "body must have an 'events' array" }, { status: 422 });
      }
      if (b.events.length === 0) {
        return Response.json({ error: "events array must not be empty" }, { status: 422 });
      }
      if (b.events.length > 100) {
        return Response.json({ error: "max 100 events per batch" }, { status: 422 });
      }

      const parsed: IngestBody[] = [];
      const errors: { index: number; error: string }[] = [];
      for (let i = 0; i < b.events.length; i++) {
        const result = parseEvent(b.events[i]);
        if ("error" in result) {
          errors.push({ index: i, error: result.error });
        } else {
          parsed.push(result);
        }
      }

      if (errors.length > 0) {
        return Response.json({ error: "validation failed", details: errors }, { status: 422 });
      }

      // One country for the whole batch — all events in a request share the
      // same client IP. Derived transiently; the IP is never stored.
      const country = countryForRequest(req);

      try {
        // Insert all events concurrently (pool handles connection reuse)
        await Promise.all(parsed.map((e) => insertEvent(pool, e, country)));
      } catch (err) {
        console.error("[events] batch insert failed:", err);
        return Response.json({ error: "internal server error" }, { status: 500 });
      }

      return Response.json({ ok: true, accepted: parsed.length }, { status: 201 });
    },
  };
}
