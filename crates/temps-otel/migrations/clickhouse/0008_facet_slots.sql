-- SPDX-FileCopyrightText: 2024-2026 Temps Contributors
-- SPDX-License-Identifier: MIT OR Apache-2.0

-- Facet slot columns for span attribute fast-path filtering.
--
-- WHY
--
-- OTel spans store all attributes in a JSON blob (`attributes String`).
-- Filtering by attribute value is `JSONExtractString(attributes, key) = value`,
-- which requires reading and parsing the blob for every span row — a full-table
-- scan at 500M-1B row scale.
--
-- This migration adds 20 generic Nullable(String) columns (`facet_attr_1`
-- through `facet_attr_20`). An operator marks any arbitrary attribute key as
-- "faceted" via the admin API, which:
--   1. Assigns the key to the lowest free slot in the Postgres `otel_span_facets`
--      table (the key→slot mapping).
--   2. Runs an asynchronous ClickHouse mutation to populate the slot column for
--      all existing spans that have that attribute.
--   3. Going forward, new spans ingested while the key is faceted write the
--      attribute value directly into the slot column.
--
-- Query path: when filtering by a faceted key, the query layer emits
-- `facet_attr_N = ?` instead of `JSONExtractString(attributes, ?) = ?`, letting
-- ClickHouse use the bloom-filter skip index added below.
--
-- COST
--
-- 20 Nullable(String) columns, all NULL for unfaceted spans. ClickHouse encodes
-- NULLs in a separate 1-bit null mask per column, and ZSTD(1) compresses long
-- runs of NULLs to near-zero. Measured impact is small compared to the `attributes`
-- blob that already dominates per-row size.
--
-- The bloom-filter indexes add ~0.5–2% additional disk per indexed column.
-- Index granularity 4 matches the `spans` table setting (0001_spans.sql).
ALTER TABLE spans
    ADD COLUMN IF NOT EXISTS facet_attr_1  Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_2  Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_3  Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_4  Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_5  Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_6  Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_7  Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_8  Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_9  Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_10 Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_11 Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_12 Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_13 Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_14 Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_15 Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_16 Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_17 Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_18 Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_19 Nullable(String) CODEC(ZSTD(1)),
    ADD COLUMN IF NOT EXISTS facet_attr_20 Nullable(String) CODEC(ZSTD(1));
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_1  facet_attr_1  TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_2  facet_attr_2  TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_3  facet_attr_3  TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_4  facet_attr_4  TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_5  facet_attr_5  TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_6  facet_attr_6  TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_7  facet_attr_7  TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_8  facet_attr_8  TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_9  facet_attr_9  TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_10 facet_attr_10 TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_11 facet_attr_11 TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_12 facet_attr_12 TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_13 facet_attr_13 TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_14 facet_attr_14 TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_15 facet_attr_15 TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_16 facet_attr_16 TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_17 facet_attr_17 TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_18 facet_attr_18 TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_19 facet_attr_19 TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans ADD INDEX IF NOT EXISTS idx_facet_attr_20 facet_attr_20 TYPE bloom_filter(0.01) GRANULARITY 4;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_1;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_2;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_3;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_4;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_5;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_6;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_7;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_8;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_9;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_10;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_11;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_12;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_13;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_14;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_15;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_16;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_17;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_18;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_19;
ALTER TABLE spans MATERIALIZE INDEX idx_facet_attr_20;
