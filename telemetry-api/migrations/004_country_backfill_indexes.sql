-- SPDX-FileCopyrightText: 2024-2026 Temps Contributors
-- SPDX-License-Identifier: MIT OR Apache-2.0

-- Support the country backfill (backfillCountries in src/db/events.ts, flushed
-- in the background by src/backfill.ts): instances that report a resolved
-- country get their rows that were stored with a NULL country filled in.
-- Partial indexes over only the NULL rows keep each flush's UPDATE cheap, and
-- an index-only no-op for instances that have already been backfilled.
-- (Comment-only edit after this migration was applied; the migrator tracks
-- applied files by name, and the SQL below is unchanged.)

CREATE INDEX IF NOT EXISTS idx_telemetry_events_anon_country_null
    ON telemetry_events (anonymous_id)
    WHERE country IS NULL;

CREATE INDEX IF NOT EXISTS idx_telemetry_instance_days_anon_country_null
    ON telemetry_instance_days (anonymous_id)
    WHERE country IS NULL;
