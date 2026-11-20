-- SPDX-FileCopyrightText: 2024-2026 Temps Contributors
-- SPDX-License-Identifier: MIT OR Apache-2.0

-- The client's JA4 TLS fingerprint and where it came from (ADR 066, D4 and
-- I8). Empty for requests without one, and for every existing row.
ALTER TABLE proxy_logs
    ADD COLUMN IF NOT EXISTS ja4 LowCardinality(String) DEFAULT '',
    ADD COLUMN IF NOT EXISTS ja4_source LowCardinality(String) DEFAULT ''
