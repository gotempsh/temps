-- SPDX-FileCopyrightText: 2024-2026 Temps Contributors
-- SPDX-License-Identifier: MIT OR Apache-2.0

-- Support the ingest-time country backfill (routes/events.ts backfillCountry):
-- every event with a resolved country fills in that instance's rows that were
-- stored with a NULL country. Partial indexes over only the NULL rows make that
-- UPDATE an index-only no-op once an instance has been backfilled, instead of
-- rescanning all of a busy instance's events on every request.

CREATE INDEX IF NOT EXISTS idx_telemetry_events_anon_country_null
    ON telemetry_events (anonymous_id)
    WHERE country IS NULL;

CREATE INDEX IF NOT EXISTS idx_telemetry_instance_days_anon_country_null
    ON telemetry_instance_days (anonymous_id)
    WHERE country IS NULL;
