---
title: "ADR-016: ClickHouse as the OTel telemetry backend"
status: Proposed
date: 2026-06-02
author: David Viejo
---

# ADR-016: ClickHouse as the OTel telemetry backend

**Status:** Proposed
**Date:** 2026-06-02
**Author:** David Viejo

## Context

Temps stores OpenTelemetry telemetry in PostgreSQL/TimescaleDB: the `otel_spans`,
`otel_metrics`, and `otel_log_events` hypertables (`m20260225_000001_create_otel_tables.rs`),
plus three small mutable control tables — `otel_insights`, `otel_health_summaries`,
and a derived storage-quota calculation. Everything is reached through a single
`OtelStorage` trait (`crates/temps-otel/src/storage/mod.rs`), with one
implementation today: `TimescaleDbStorage`. The plugin registers one
`Arc<dyn OtelStorage>` (`plugin.rs:277`) and every handler/service calls through it.

This works for the indie-hacker default. It does **not** scale to millions of
traces, for structural reasons (the same ones analytics events hit — see
[ADR-012](012-clickhouse-analytics-backend.md)):

1. **The trace list is a sort-after-aggregate.** A "trace" is not a row — it's a
   `GROUP BY trace_id` over many span rows. To list/sort traces, Postgres reads
   every span in the window, groups them, computes `MAX(duration_ms)` /
   `COUNT(*)` / error counts per trace, then sorts. `LIMIT` can't shortcut — the
   work happens before it. Sorting by **duration** (PR #114) makes it worse:
   `MAX(duration_ms)` is a computed aggregate, so no index can drive the `ORDER BY`.
   At millions of traces this is multi-second.
2. **`OFFSET` pagination degrades** on deep pages (compute-and-discard).
3. **Storage**: row-store + btree indexes on high-volume telemetry cost 5–10× the
   on-disk footprint of a columnar layout.

ClickHouse solves all three: columnar compression, vectorized aggregation, and a
cheap `GROUP BY trace_id` over compressed columns. We already operate ClickHouse
for web analytics (ADR-012), enabled by the same `TEMPS_CLICKHOUSE_*` env vars and
`ServerConfig::is_clickhouse_enabled()`. But **traces/metrics/logs are
TimescaleDB-only today** — ClickHouse stores only `events`/`sessions`. There is no
OTel path in ClickHouse.

We rejected TimescaleDB-native fixes (continuous aggregate keyed by
`(time_bucket, trace_id)` — high-cardinality + cross-bucket straddling + real-time
cagg `ORDER BY/LIMIT` still full-scans; Timescale's own Promscale didn't use caggs
for per-trace data) and a hand-rolled Postgres rollup table (solves the sort, not
the storage cost, re-derives columnar aggregation by hand). Since we already run
ClickHouse, a ClickHouse OTel backend is the better long-term home.

## Decision

**When ClickHouse is enabled, it is the system of record for OTel _telemetry_ —
spans, metrics, and logs all live in and are served from ClickHouse. The three
small mutable _control_ rows (insights, health summaries, storage quota) stay in
Postgres. When ClickHouse is not configured, everything stays on TimescaleDB
exactly as today.**

This is a **clean all-or-nothing switch for the telemetry data** — not a
per-domain router and not a derived replica. The only split is the principled one:
mutable control state belongs in a transactional store.

### 1. One concrete backend selected at startup — no hybrid routing for data

The plugin picks one telemetry backend:

```rust
let storage: Arc<dyn OtelStorage> = if server_config.is_clickhouse_enabled() {
    Arc::new(ClickHouseOtelStorage::new(ch_config, db.clone()))
} else {
    Arc::new(TimescaleDbStorage::with_config(db.clone(), ...))  // unchanged default
};
context.register_service(storage);
```

`ClickHouseOtelStorage` implements the full `OtelStorage` trait. Telemetry methods
(spans, metrics, logs — store + query) run against ClickHouse. The control-row
methods are delegated to a small Postgres-backed helper it holds internally:

```rust
pub struct ClickHouseOtelStorage {
    ch: ::clickhouse::Client,
    control: OtelControlStore,   // Postgres: insights, health, quota
}
```

So there is **no `HybridOtelStorage`, no per-domain selection by callers, no async
fan-out, no replica lag** — ClickHouse owns the telemetry outright when enabled.

### 2. Why insights / health / quota stay in Postgres

These are not telemetry — they're mutable control-plane rows, and ClickHouse
(append-only, eventual-merge) is a poor fit:

- **Insights** (`upsert_insight`, `list_insights`, `resolve_insight`): anomaly rows
  with a status. `resolve_insight` is a literal `UPDATE … SET status='resolved'`.
  CH has no real `UPDATE` — faking it via `ReplacingMergeTree` makes "resolved"
  eventual and unreliable.
- **Health summaries** (`store_health_summary`, `get_health_summaries`): per-service
  rollups overwritten in place — same update problem.
- **Quota** (`get_storage_quota`, `check_quota`): called on the **ingest hot path**;
  the byte estimate uses TimescaleDB-specific `pg_total_relation_size` +
  `approximate_row_count`. When telemetry lives in CH, the quota source must change
  to ClickHouse's `system.parts` for the spans/metrics/logs byte counts — but the
  quota _row/logic_ stays a small Postgres concern.

Keeping these in Postgres costs nothing operationally (they're tiny) and avoids
contorting append-only CH into a transactional store. This mirrors ADR-012's
principle: relational/mutable bits stay in Postgres.

### 3. ClickHouse telemetry schema

`spans`, `metrics`, `log_events` tables, columnar and tuned for the analytical
reads. Spans (the priority) shown; metrics/logs follow the same shape.

```sql
CREATE TABLE IF NOT EXISTS spans (
    project_id           Int32,
    deployment_id        Nullable(Int32),
    service_name         LowCardinality(String),
    service_version      LowCardinality(String),
    deployment_environment LowCardinality(String),
    trace_id             String,
    span_id              String,
    parent_span_id       String,            -- '' for root
    name                 String,
    kind                 LowCardinality(String),
    start_time           DateTime64(3),
    end_time             DateTime64(3),
    duration_ms          Float64,
    status_code          LowCardinality(String),
    status_message       String,
    attributes           String,            -- JSON as String (or Map(String,String))
    events               String,
    _version             UInt64             -- dedup key for idempotent ingest
)
ENGINE = ReplacingMergeTree(_version)
PARTITION BY toYYYYMM(start_time)
ORDER BY (project_id, trace_id, span_id)
TTL toDateTime(start_time) + INTERVAL 90 DAY;
```

For the trace list, an AggregatingMergeTree materialized view pre-aggregates one
row per trace so duration sort + pagination are columnar and fast at any scale:

```sql
CREATE MATERIALIZED VIEW IF NOT EXISTS trace_summaries_mv
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(trace_start)
ORDER BY (project_id, trace_id)
AS SELECT
    project_id,
    trace_id,
    minState(start_time)                    AS trace_start,
    maxState(duration_ms)                   AS max_duration_ms,
    countState()                            AS span_count,
    countStateIf(status_code = 'ERROR')     AS error_count,
    argMaxState(service_name, duration_ms)  AS service_name,
    argMaxState(name, duration_ms)          AS root_name
FROM spans
GROUP BY project_id, trace_id;
```

The list query reads `trace_summaries_mv` with the merge combinators and
`ORDER BY maxMerge(max_duration_ms) DESC LIMIT n` — the AggregatingMergeTree
combinators handle the cross-bucket merge that a TimescaleDB cagg cannot.

### 4. Direct ingest — no outbox/fan-out

Because ClickHouse is the system of record for telemetry (not a replica), there is
**no dual-write and no outbox worker**. `store_spans` / `store_metrics` /
`store_logs` batch-insert straight into ClickHouse (`ch.insert::<ChSpanRow>("spans")`),
with `ReplacingMergeTree(_version)` providing idempotency on OTLP retries. This is
simpler than ADR-012 (which keeps Postgres authoritative and fans out), because we
are not trying to keep two telemetry stores in sync — there is only one.

(Trade-off: telemetry is then only in ClickHouse when enabled. See Consequences —
durability/backup of CH becomes the operator's responsibility, same as their
analytics CH.)

### 5. Migrations + row types follow the analytics pattern

CH migrations under a dedicated OTel migration set, applied idempotently via the
existing `_temps_ch_migrations` runner pattern (e.g. `0001_spans.sql`,
`0002_trace_summaries_mv.sql`, `0003_metrics.sql`, `0004_log_events.sql`). Row
structs use `#[derive(::clickhouse::Row, serde::Serialize)]` mirroring `ChEventRow`.

### 6. Keyset pagination

With a real per-trace duration column in CH, the trace list moves from `OFFSET` to
keyset/cursor pagination on the sort key (duration or start_time), eliminating
deep-page cost. `limit/offset` stays for backward compatibility.

## Consequences

### Positive
- Trace list + duration sort become O(columnar scan + LIMIT) at millions of traces.
- Columnar storage cuts telemetry footprint 5–10×.
- **Simpler than a replica/hybrid design**: one telemetry backend, direct ingest,
  no fan-out worker, no read-your-writes lag, no per-domain routing.
- Reuses ADR-012 plumbing (config toggle, CH client, migration runner, Row derive,
  `ReplacingMergeTree` dedup).
- Off by default — the OSS single-box default is unchanged.

### Negative / risks
- **CH becomes system of record for telemetry when enabled.** Unlike ADR-012
  (where Postgres stays authoritative), there is no Postgres copy of spans/metrics/
  logs to fall back to. Operators who enable it own CH durability/backups for
  telemetry. (Acceptable: telemetry is high-volume, lower-criticality, 90-day TTL.)
- **`ClickHouseOtelStorage` is a large surface** — it must implement all telemetry
  read/write methods (spans, metrics, logs) in ClickHouse, including metric
  aggregation, baselines, and `get_p95_latency`. Spans are the priority; metrics/
  logs can land in later phases (until then, enabling CH would require those too —
  see phasing).
- **Quota source rewrite**: `get_storage_quota` must compute telemetry bytes from
  ClickHouse `system.parts` when CH is the backend, while the quota row stays in PG.
- **Anomaly/baseline queries that JOIN `deployments`/`environments`** must be
  reworked (CH can't JOIN Postgres). Either denormalize at ingest (like analytics
  denormalizes geo) or fetch the relational side separately.

### Neutral
- No change for operators who don't set `TEMPS_CLICKHOUSE_*`.
- Insights/health/quota rows behave identically (still Postgres) regardless of toggle.

## Phased plan

ClickHouse-for-OTel is all-or-nothing **per telemetry domain at the toggle**, so we
phase by domain. The toggle only fully activates once the domains it covers are done.

1. **Phase 0 — schema + client + ingest (spans).** CH `spans` + `trace_summaries_mv`
   migrations; `ClickHouseOtelStorage` skeleton with `store_spans` (direct insert)
   and the control-store delegation to Postgres. Behind the disabled-by-default toggle.
2. **Phase 1 — span reads.** `query_trace_summaries` / `count_traces` / `query_spans`
   / `get_trace` + GenAI trace reads against CH. Verify list + duration sort + keyset.
3. **Phase 2 — metrics in CH.** `store_metrics`, `query_metrics`, baselines,
   `get_p95_latency`, `get_recent_minute_aggregates` (denormalize deploy/env refs).
4. **Phase 3 — logs in CH.** `store_logs`, `archive_logs`, `query_logs`.
5. **Phase 4 — quota source** from CH `system.parts`; wire the toggle to select
   `ClickHouseOtelStorage` once Phases 1–4 cover the telemetry.
6. **Phase 5 — backfill** existing `otel_*` hypertables into CH (one-shot, batched,
   like the analytics `ch_backfill`) for operators migrating an existing install.

Until Phases 2–3 land, the toggle can be gated to "spans-in-CH, metrics/logs still
TimescaleDB" **only if** we accept a temporary internal split during rollout — OR
we hold the toggle until all telemetry domains are CH-ready. Prefer the latter for
a clean "all telemetry in one place" guarantee; decide at Phase 1 review.

## Open questions (resolve during Phase 0/1)

1. **MV vs projection vs query-time GROUP BY in CH** for trace summaries — benchmark.
2. **`duration_ms` semantics**: keep current `MAX(span.duration_ms)` (longest span)
   for parity with PR #114, or switch to trace wall-clock
   (`MAX(end_time)-MIN(start_time)`)? Affects the MV.
3. **Toggle granularity**: hold the toggle until all telemetry domains are CH-ready
   (clean), vs allow a documented spans-first rollout (faster, temporary split).
4. **Relational JOINs** (deploy/env names in metrics baselines): denormalize at
   ingest vs fetch separately.
5. **Quota accuracy** from `system.parts` (per-project byte attribution needs a
   `project_id`-segmented part scan or a per-project size estimate).

## References
- [ADR-012: ClickHouse as an external analytics backend](012-clickhouse-analytics-backend.md) — config toggle, CH client, migration runner, Row derive reused here.
- PR #114 — added (TimescaleDB) trace duration sort; this ADR is its scale follow-up.
- `crates/temps-otel/src/storage/mod.rs` — `OtelStorage` trait (the contract `ClickHouseOtelStorage` implements).
- `crates/temps-analytics-events/src/services/ch_fanout.rs` — Row struct + insert pattern (we reuse the insert shape, not the fan-out, since OTel ingest is direct).
