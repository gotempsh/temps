<!--
SCOPE: Observability — a single, queryable, cross-source log of every alarm/alert firing.
-->

# ADR-025: Unified alarm log (one queryable firing history across all sources)

**Status:** Proposed
**Date:** 2026-06-29
**Author:** David Viejo

## Context

Temps fires alerts from many subsystems, but there is **no single place to see
when anything fired**. Two compounding problems:

**1. The shared `alarms` table is effectively write-only.** A generic `alarms`
table (`temps-entities/src/alarms.rs`) already exists — `project/environment/
deployment/container/service_id` scope FKs, `alarm_type`, `severity`, `status`,
`metadata` JSON, `fired_at`/`acknowledged_at`/`resolved_at`. `AlarmService`
(`temps-monitoring/src/alarm_service.rs`) is the single writer and **already
implements `list_alarms` (paginated, filterable), `get_alarm_summary`,
`acknowledge_alarm`, `resolve_alarm`** — but **none are wired to an HTTP route.**
`temps-monitoring` has no plugin/router; the web SDK has zero alarm endpoints;
the UI has two **dead `/monitoring/alarms` links** (`ServiceMonitoring.tsx:1162`,
`storage/MonitoringCard.tsx:815`) with no page behind them. So firings are
recorded and drive notifications, but users can't see them.

**2. Firings are scattered across disjoint stores.** What writes where:

| Source | Persists to | Lifecycle | Rule/source link |
|---|---|---|---|
| Metric-threshold rules (`AlertEvaluator`) | **`alarms`** | fire+resolve | `metadata.rule_id` (JSON, no FK) |
| Container health (restart/OOM/crash, high_cpu/mem) | **`alarms`** | fire-only | `container_id` FK; no rule |
| Outage detector | **`alarms`** *and* `status_incidents` | fire+resolve | `metadata.monitor_id` |
| Disk space | `alarms` *or* notification-only (⚠ sources disagreed — verify) | fire | none |
| Error-tracking alerts | **`error_alert_fires`** (own) | fire-only | **real `rule_id` + `error_group_id` FKs** |
| Backup alerts | **`backup_alerts`** (own) | open/resolve | `schedule_id` FK; **no `project_id`** |
| Anomaly insights | **`otel_insights`** (own) | active/resolved | deploy/anomaly ids |
| Worker-node health (down/recovery/resource) | **nothing** (notification-only; only on the unmerged `harden/multi-node` worktree) | fire-and-forget | none |
| Uptime `status_checks` | raw time-series (no episodes) | — | `monitor_id` FK |

So even the data that *is* persisted lives in **four+ tables** with different
column names (`fired_at` vs `opened_at` vs `started_at`), severity vocabularies
(`info/warning/critical` vs `minor/major/critical` vs `low/medium/high/critical`),
lifecycle models, and scope keys.

### Rejected alternatives
- **Leave each source separate, build N per-feature history views.** That's
  what exists and it's exactly the problem — no cross-source "what fired".
- **Re-implement a new central alerting bus.** `alarms` + `AlarmService` already
  is the backbone for the operational sources; the gap is surfacing + onboarding
  the stragglers, not a rewrite.

## Decision

**Make `alarms` the canonical firing log: surface it over HTTP + UI, give it a
first-class source link, and route every firing source through it (mirroring the
rich own-tables that need their own payload).**

### 1. Phase 1 — surface the alarms we already record (the 80/20 win)
- Add a project-scoped **read/ack/resolve API** over the existing `AlarmService`
  methods: `GET /projects/{id}/alarms` (paginated, filter by
  type/severity/status/time/env/deployment/service), `GET …/alarms/summary`,
  `POST …/alarms/{id}/acknowledge`, `POST …/alarms/{id}/resolve`. The service
  methods already exist — this is a handler + route registration + SDK regen.
- Build the **Alarms history UI** the dead links already point at: a global,
  filterable timeline/list (fired → ack → resolved, duration, severity, source,
  value). This instantly surfaces metric, container, outage (and disk) firings.

### 2. Phase 2 — make it "all of them"
- **Source linkage column.** Add typed `source_kind` (enum string) + `source_id`
  (+ optional `rule_id`) columns to `alarms`, backfilled from `metadata`. So the
  log can pivot "all firings of rule/monitor/schedule X" with a column join, not
  brittle JSON.
- **Onboard the notification-only sources** by routing them through
  `AlarmService::fire_alarm/resolve_alarm` with new `alarm_type`s:
  `node_offline`/`node_resource` (worker-node), and `disk_space` if not already.
  They get history + ack/resolve + dedup for free.
- **Fold the own-table sources** (`error_alert_fires`, `backup_alerts`,
  `otel_insights`, `status_incidents`) — preferred: **mirror a normalized
  `alarms` row when they fire/resolve** (keep the rich table as source-of-record
  for its payload). Alternative for read-only: a UNION view with a normalization
  layer. Mirroring keeps one table to query and reuses ack/resolve.
- **Normalize** severity → `info|warning|critical` and status →
  `firing|acknowledged|resolved` at the boundary.

### 3. Lifecycle cleanup (do alongside Phase 2)
- Add a **partial-unique index** "one open alarm per (project, alarm_type, scope,
  source_id)" so an ongoing incident is one row, not a new row every 5-min
  cooldown. Point-in-time events (restart/crash) stay fire-only (`resolved_at =
  fired_at`). This makes `Job::AlarmFired/AlarmResolved` (currently emitted but
  **unconsumed**) a clean fan-out hook for a unified-log writer if needed.

## Consequences

### Positive
- One queryable, filterable firing history across metrics, databases, uptime,
  containers, nodes, backups, errors, anomalies — table-stakes for the product
  and the foundation for noise-tuning, incident timelines, and reliability
  reporting.
- Phase 1 is cheap: the service layer + UI links already exist.
- Reuses the existing notification/ack/resolve plumbing.

### Negative / risks
- Backup alerts have **no `project_id`** (derive from schedule) and are currently
  **notification-silent** — mirroring into `alarms` would also light up
  notifications unless suppressed (behavior change to gate).
- Mirroring rich sources risks drift between the own-table and the `alarms`
  mirror; needs a single write path per source.
- Worker-node alerting lives only on the **unmerged** `harden/multi-node`
  worktree — Phase 2 for nodes depends on that landing.
- Severity/status normalization can lose nuance (e.g. insight `low`).

### Neutral
- `status_checks` stays raw time-series; only an episode-detector would surface
  uptime as alarms (already covered by the outage detector for monitored envs).

## Phased plan
1. **Phase 1 (fast):** alarms read/ack/resolve API + SDK + global Alarms history
   UI. Verify the disk-space source actually writes `alarms`.
2. **Phase 2:** `source_kind`/`source_id` columns + backfill; route node/disk
   through AlarmService; mirror `error_alert_fires`/`backup_alerts` (and
   optionally `otel_insights`/`status_incidents`) into `alarms`; normalize.
3. **Phase 3 (polish):** one-open-row partial-unique index; correlation id across
   check→incident→alarm→notification; saved filters / per-rule history drill-in.

## Open questions
- Does `DiskSpaceMonitor` write `alarms` or only notify? (scout findings conflict
  — confirm before Phase 1 scoping.)
- Mirror-into-alarms vs UNION view for the rich own-tables — mirror recommended,
  but confirm we want `alarms` as the single source of truth for history.
- Where the alarms handler lives: a new `temps-monitoring` plugin vs hosting the
  routes in an existing plugin that already holds an `AlarmService` handle.

## References
- `crates/temps-entities/src/alarms.rs`, `crates/temps-monitoring/src/{alarm_service,evaluator,container_health,outage,disk_space}.rs`
- `crates/temps-entities/src/{error_alert_fires,backup_alerts,status_incidents,status_checks,monitoring_alert_rules}.rs`
- `crates/temps-otel/src/anomaly/insights.rs`; `crates/temps-cli/src/commands/serve/console.rs` (the only AlarmService wiring)
- Dead UI links: `web/src/pages/ServiceMonitoring.tsx:1162`, `web/src/components/storage/MonitoringCard.tsx:815`
