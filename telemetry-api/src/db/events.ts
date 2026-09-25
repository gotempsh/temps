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
// touched — a country that is already known is never overwritten. Setup
// attempts stay country-less by design.
//
// Called by the background CountryBackfiller (src/backfill.ts), never on the
// ingest request path. One statement per flush for every queued instance,
// each with its own country: both tables are updated atomically (a
// data-modifying CTE runs in the same statement), and the partial indexes from
// migration 004 keep it cheap once an instance has no NULL rows left.
export async function backfillCountries(
  pool: Pool,
  entries: ReadonlyArray<readonly [anonymousId: string, country: string]>
): Promise<void> {
  if (entries.length === 0) return;
  await pool.query(
    `WITH src AS (
       SELECT * FROM unnest($1::text[], $2::text[]) AS s(anonymous_id, country)
     ), filled_days AS (
       UPDATE telemetry_instance_days d SET country = src.country
       FROM src
       WHERE d.anonymous_id = src.anonymous_id AND d.country IS NULL
     )
     UPDATE telemetry_events e SET country = src.country
     FROM src
     WHERE e.anonymous_id = src.anonymous_id AND e.country IS NULL
       AND e.event_type <> 'cli_setup_step'`,
    [entries.map(([id]) => id), entries.map(([, country]) => country)]
  );
}
