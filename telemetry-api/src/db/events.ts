// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Data access for ingested telemetry events. Routes validate and decide policy;
// this module owns the SQL.

import type { Pool } from "pg";

export interface IngestBody {
  anonymous_id: string;
  event_type: string;
  properties?: Record<string, unknown>;
  temps_version?: string;
  occurred_at?: string;
}

// `country` is the 2-letter ISO code derived from the request IP at ingest time
// (see geo.ts). The IP itself is never passed here or stored — only the country.
export async function insertEvent(
  pool: Pool,
  event: IngestBody,
  country: string | null
): Promise<void> {
  if (event.event_type === "cli_setup_step") country = null;
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

  // An installer attempt is not an active Temps server.
  if (event.event_type === "cli_setup_step") return;

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

// Distinct instances in a request that should inherit the request's country.
// Setup attempts stay country-less by design, so they never qualify.
export function backfillTargets(events: IngestBody[]): string[] {
  return [
    ...new Set(
      events
        .filter((e) => e.event_type !== "cli_setup_step")
        .map((e) => e.anonymous_id)
    ),
  ];
}

// Fill in the country on the given instances' earlier rows that were stored
// without one (private IP, missing geo DB at the time, ...). Only NULLs are
// touched — a country that is already known is never overwritten.
//
// One statement for every instance in the request: both tables are updated
// atomically (a data-modifying CTE runs in the same statement snapshot), and a
// 100-event batch costs one round trip, not one per instance. The partial
// indexes from migration 004 keep it a near-free no-op once an instance has no
// NULL rows left.
export async function backfillCountry(
  pool: Pool,
  anonymousIds: string[],
  country: string | null
): Promise<void> {
  if (!country || anonymousIds.length === 0) return;
  await pool.query(
    `WITH filled_days AS (
       UPDATE telemetry_instance_days SET country = $2
       WHERE anonymous_id = ANY($1::text[]) AND country IS NULL
     )
     UPDATE telemetry_events SET country = $2
     WHERE anonymous_id = ANY($1::text[]) AND country IS NULL
       AND event_type <> 'cli_setup_step'`,
    [anonymousIds, country]
  );
}
