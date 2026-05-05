---
title: "ADR-012: ClickHouse as an external analytics backend"
status: Accepted
date: 2026-05-05
author: David Viejo
---

# ADR-012: ClickHouse as an external analytics backend

**Status:** Accepted
**Date:** 2026-05-05
**Author:** David Viejo

## Context

Temps stores analytics events in PostgreSQL with the TimescaleDB extension. A
single Hetzner box handles thousands of events per second comfortably, and
the schema lives next to relational data (workspaces, projects, users) so
queries can `JOIN` freely. This works well for the indie-hacker default and
small teams.

It does not work indefinitely. Above ~100 GB of `events_hot` chunks or ~50M
events/month, three things start to hurt:

1. **Aggregation queries (funnels, retention, top-K)** scan multi-million
   rows and saturate a single Postgres core. TimescaleDB's continuous
   aggregates help, but you maintain one per dashboard widget and they fall
   over when the dashboard schema changes.
2. **Backups**: pg_dumpall with the analytics hypertable becomes a 30+ minute
   operation that locks chunks at the worst moment. WAL-G helps but the
   restore RPO grows linearly with hot-data volume.
3. **Storage**: row-store with btree indexes on `(project_id, timestamp,
   event_name, ...)` costs 5–10× the on-disk footprint of a columnar layout.

ClickHouse solves all three: columnar compression, vectorized aggregations,
and engine-level features like `windowFunnel`, `retention`, `uniqHLL12`,
`WITH FILL`, and `Map(String,String)` for event properties. The same query
that takes 8 seconds in Timescale takes 80 ms in ClickHouse.

The natural reaction is "switch the analytics backend." That reaction is
wrong for our codebase.

## Decision

**Postgres remains the system of record. ClickHouse is an optional, derived
analytical replica that operators bring their own.**

Concretely:

1. **Two backends, one trait**. The `AnalyticsEvents` trait
   (`temps-analytics-events::services::traits`) defines the read query
   surface. `AnalyticsEventsService` (Postgres/Timescale) implements it
   today; a future `ClickHouseEventsService` will implement the same trait
   query-by-query, validated by a parity test harness that runs the same
   inputs through both backends and asserts equal outputs.

2. **Writes go to Postgres synchronously, then fan out to ClickHouse
   asynchronously.** A new `events_ch_outbox` table records every PG insert.
   A `ChFanoutWorker` (in `temps-analytics-events::services::ch_fanout`)
   batches outbox rows and pushes them to ClickHouse via
   `ReplacingMergeTree(_version)`, which dedupes on `event_id` so retries
   are safe.

3. **ClickHouse is not a managed service.** Unlike Postgres/Redis/Mongo,
   Temps does **not** spin up a ClickHouse container, manage its lifecycle,
   or back it up via the `ExternalService` provider abstraction. Operators
   point Temps at an existing CH cluster they already run (Altinity Cloud,
   ClickHouse Cloud, self-hosted) via configuration:

   ```toml
   [analytics.clickhouse]
   enabled = true
   url      = "https://ch.example.internal:8443"
   database = "temps_analytics"
   user     = "temps"
   password = "..."
   ```

   This matches how production ClickHouse is actually operated — its
   ops surface (sharding, replication via ZooKeeper/Keeper, partition
   management) is large enough that bundling it into Temps would either be
   shallow (and broken under load) or huge (and out of scope for a PaaS).

4. **Postgres alone remains a complete product.** Disabling
   `analytics.clickhouse.enabled` (the default) gives you the same Temps
   you have today — single static binary, no extra services. The CH path
   is purely additive.

5. **Backups**: PG backups already work via the existing `ExternalService`
   trait (`backup_to_s3`, `restore_from_s3`, `RestoreCapabilities`).
   ClickHouse backups are the operator's responsibility on their cluster.
   We document the recommended pattern (`BACKUP DATABASE temps_analytics
   TO S3(...)` or `clickhouse-backup`) but don't drive it. This is
   consistent with point 3.

## Hybrid read-routing

Reads pick a backend per query, not per request:

| Query class | Backend | Rationale |
|---|---|---|
| Single-event lookup by `event_id`, recent (≤72h) | Postgres | Already in `events_hot`; sub-millisecond btree hit |
| Recent activity feed (last N events for a user) | Postgres | Joins to `users`, `projects` are free |
| Funnels, retention, time-bucketed aggregates over arbitrary windows | ClickHouse | Where columnar wins decisively |
| GDPR/project deletes | PG cascades via FK; CH catches up via outbox tombstones | Self-hosted operator owns compliance |
| Counts joined to OLTP entities (project name, owner) | PG | Already there, no cross-system join |

The router lives in `temps-analytics-events` and is the only place the
backend choice is made.

## Why not make ClickHouse a `ServiceType`

We considered adding `ServiceType::Clickhouse` to the existing
`temps-providers::externalsvc` framework so operators could spin up a CH
container the same way they spin up Postgres. Rejected because:

- A useful CH deployment is a cluster (shards, replicas, Keeper). Single-node
  CH is fine for dev but doesn't survive the workloads that motivate
  switching to CH in the first place.
- The `ExternalService` trait surface (`provision_resource`,
  `get_runtime_env_vars`, `backup_to_s3`, `restore_pitr`) doesn't model CH
  cleanly. We'd be implementing 30 trait methods just to satisfy the
  contract, several with `unsupported` returns.
- Operators with > 50M events/month already run CH or know how to set it up.
  The bundled experience would not be better than `clickhouse-server` from
  upstream.

If demand changes — specifically, if we hear "I want ClickHouse but don't
know how to run it" repeatedly from indie hackers — we can revisit by
adding a thin `ClickHouseService` impl that wraps a single-node Docker
container for the dev tier only.

## Consequences

### Positive

- Default Temps install stays a single binary against Postgres. Zero new
  ops burden until an operator opts in.
- Operators who already run CH can plug Temps in with four config keys, no
  migration, no schema reconciliation.
- The hybrid model preserves Postgres's relational ergonomics where they
  matter (operational queries, joins) while getting CH's analytical speed
  where it matters (dashboards, funnels).
- Backups stay simple: PG via the existing path, CH via whatever the
  operator's CH cluster already does.

### Negative

- Two SQL dialects to maintain in `AnalyticsEvents` impls. Mitigated by the
  parity test harness — any divergence is a test failure, not a runtime
  bug.
- Outbox lag is observable to users: writes appear in dashboards seconds
  after they happen, not synchronously. Acceptable for analytics; we'll
  surface the lag as a metric.
- "Hybrid" is a more complex mental model than "pick a backend." We pay
  for it in docs and onboarding clarity.

### Neutral

- Doubled storage for the ≤72h hot window in `events_hot`. Negligible at
  Temps Cloud scale (~1–3 GB).
- ClickHouse retention policy is per-table TTL on `events`; no per-project
  retention initially. If demanded, we add a column to `projects` and a
  scheduled `ALTER TABLE … DELETE WHERE` job.

## References

- Phase 1 commit: `refactor(analytics-events): extract AnalyticsEvents
  read-side trait` — adds the trait, no behavior change.
- Phase 2 commit: `feat(analytics): ClickHouse client + outbox migration +
  fan-out skeleton` — adds the CH client, schema, outbox table, and
  worker scaffold.
- Phase 4 commit: `feat(config): analytics.clickhouse config keys` —
  exposes the four config keys above.
