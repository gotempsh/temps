-- SPDX-FileCopyrightText: 2024-2026 Temps Contributors
-- SPDX-License-Identifier: MIT OR Apache-2.0

-- What Shield would have done with each request, and why (ADR 066, I8).
-- Empty for requests no reputation list holds, and for every existing row.
ALTER TABLE proxy_logs
    ADD COLUMN IF NOT EXISTS shield_verdict LowCardinality(String) DEFAULT '',
    ADD COLUMN IF NOT EXISTS shield_reasons Array(LowCardinality(String))
