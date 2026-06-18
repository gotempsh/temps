---
title: "ADR-019: Forwarding Temps telemetry to an external OpenObserve instance"
status: Proposed
date: 2026-06-18
author: David Viejo
---

# ADR-019: Forwarding Temps telemetry to an external OpenObserve instance

**Status:** Proposed
**Date:** 2026-06-18
**Author:** David Viejo

## Context

A customer asked Temps to forward "as much telemetry as possible" to their own
external [OpenObserve](https://openobserve.ai) (OO) instance, which is an
OTLP-native observability backend. The instinct to hand them SDK configuration
pointing at two endpoints is the wrong model: it requires every application to be
reconfigured, breaks when apps use language SDKs that do not support multi-destination
export, and gives Temps no control over what is forwarded.

The correct model is the **OpenTelemetry Collector "receive-once, fan-out-many"
pattern**: apps keep sending OTLP to Temps' single existing ingest receiver; Temps
stores a copy locally for its native `/observe` page and routes a second copy to
the customer's OO instance. No application reconfiguration. No dual emission.

Beyond app-emitted OTLP (traces, logs, metrics), "as much as possible" also covers
Temps' platform-native signals that never leave the server: container stdout/stderr
logs, scraped service and container metrics, error events, alarms, revenue events,
analytics events, and the audit log. These require separate tap points — they are
not visible to the app's OTLP SDK.

This is an **EE-gated, licensed feature**. Its implementation lives in a new EE
crate; a minimal, no-op-by-default hook in OSS core lets OSS producer services
carry an optional sink without any EE dependency, mirroring the established
OSS-hook / EE-impl split already used by `PluginRoutes::with_override`.

### Why not decorate OtelStorage?

The initial instinct — wrap `Arc<dyn OtelStorage>` with a forwarding decorator —
is architecturally wrong for the OTLP ingest hot path. The OTLP HTTP handlers
`await` storage operations synchronously before returning the response:

- `crates/temps-otel/src/handlers/ingest_handler.rs:508`
  `state.otel_service.ingest_metrics(points).await?` — awaited before responding.
- `crates/temps-otel/src/handlers/ingest_handler.rs:577`
  `state.otel_service.ingest_spans(spans).await?` — awaited before responding.
- `crates/temps-otel/src/handlers/ingest_handler.rs:624`
  `state.otel_service.ingest_logs(records).await?` — awaited before responding.
- `crates/temps-otel/src/services/otel_service.rs:228`
  `self.storage.store_logs(db_records).await` — awaited inside the service.

A decorator on any of these trait methods would add the OpenObserve POST latency
(and failure risk) to every application's OTLP export — a latency regression on
the ingest path proportional to OO's round-trip time (tens to hundreds of
milliseconds). The correct tap is **fire-and-forget**: clone the already-decoded
`Vec<SpanRecord>` / `Vec<LogRecord>` / `Vec<MetricPoint>` and `try_send` it onto
a forwarder channel, mirroring the existing non-blocking pattern the metrics path
already uses at `ingest_handler.rs:517-524` (a non-blocking channel send to the
unified `MetricsStore`). The decorator alternative is rejected explicitly.

## Decision

**Implement `temps-ee-oo-forwarder`, a new EE crate, that taps Temps signal
producers at the handler level (for OTLP) or at post-commit / post-insert hooks
(for platform-native signals), fans records onto per-signal bounded `mpsc` channels,
and batch-POSTs them to the customer's external OpenObserve instance without
blocking any producer.**

OSS core gains a minimal `TelemetrySink` trait (no-op by default) so producer
crates can carry the optional hook with zero EE dependency. Everything is off by
default when the EE crate is absent and when `TEMPS_OPENOBSERVE_ENABLED` is false.

### 1. OSS-core `TelemetrySink` trait (greenfield addition)

Add to `crates/temps-core` a new module:

```rust
// crates/temps-core/src/telemetry_sink.rs

use async_trait::async_trait;

/// An optional, fire-and-forget sink for platform telemetry signals.
/// The no-op default implementation allows OSS producer crates to carry
/// this hook without any EE dependency.
///
/// Implementors MUST NOT block the caller.  All submissions go through a
/// bounded channel; if the channel is full, the record is silently dropped
/// and a counter is incremented.
pub trait TelemetrySink: Send + Sync + 'static {
    fn try_send_spans(&self, spans: Vec<crate::signals::SpanBatch>);
    fn try_send_logs(&self, records: Vec<crate::signals::LogBatch>);
    fn try_send_metrics(&self, points: Vec<crate::signals::MetricBatch>);
    fn try_send_container_logs(&self, lines: Vec<crate::signals::ContainerLogBatch>);
    // One method per signal family; all default to no-ops
}

/// The no-op sink installed when EE is absent.
pub struct NoopTelemetrySink;
impl TelemetrySink for NoopTelemetrySink { /* all methods are empty */ }
```

Producer services hold `Option<Arc<dyn TelemetrySink>>` (None = no-op). The EE
forwarder registers the concrete sink via the plugin registration context. This
is the same wiring pattern as `BroadcastQueueService` and `MetricsStore`.

### 2. New EE crate `temps-ee-oo-forwarder`

Structure:

```
temps-ee/crates/temps-ee-oo-forwarder/
    src/
        lib.rs           // OpenObserveForwarderPlugin (impl TempsPlugin)
        forwarder.rs     // OoForwarder: channels + workers + reqwest::Client
        encode/
            traces.rs    // SpanRecord → OTLP proto ResourceSpans
            logs.rs      // LogRecord  → OTLP proto ResourceLogs
            metrics.rs   // MetricPoint → OTLP proto ResourceMetrics
            json.rs      // platform-native signals → OO _json payload
        config.rs        // reads ServerConfig fields, validates
```

Dependencies: `temps-core` (the `TelemetrySink` trait), `opentelemetry-proto`
(OTLP proto types), `prost` (encoding), `reqwest` (HTTP), `tokio` (channel +
runtime). No dependency on any OSS domain crate.

`OpenObserveForwarderPlugin` registers in the serve plugin chain at
`crates/temps-cli/src/commands/serve/console.rs`, after the OTel plugin (so the
OTel plugin's `OtelAppState` is already registered when the forwarder wires its
taps).

### 3. Off-hot-path forwarding: channels + batch workers

One bounded `tokio::sync::mpsc` channel per signal family (traces, logs, metrics,
container_logs, errors, alarms, revenue, analytics, audit). One batch-worker task
per channel. Channels are keyed by signal, not shared, so a metrics backlog cannot
stall trace forwarding.

```rust
// forwarder.rs — illustrative skeleton

pub struct OoForwarder {
    trace_tx:  mpsc::Sender<Vec<SpanRecord>>,
    log_tx:    mpsc::Sender<Vec<LogRecord>>,
    metric_tx: mpsc::Sender<Vec<MetricPoint>>,
    // ... one Sender per signal family
    dropped_total: Arc<AtomicU64>,
}

const CHANNEL_CAPACITY: usize = 256;   // batches in flight
const MAX_BATCH_BYTES:  usize = 4 * 1024 * 1024;  // 4 MB per POST
const MAX_BATCH_RECORDS: usize = 512;
const LINGER_MS: u64 = 1_000;          // max wait before flushing partial batch
const INFLIGHT_PERMITS: usize = 8;     // semaphore-bounded concurrent POSTs

impl OoForwarder {
    pub fn start(config: OoConfig, client: reqwest::Client) -> Arc<Self> {
        let (trace_tx, trace_rx) = mpsc::channel(CHANNEL_CAPACITY);
        // ... repeat for each signal

        let sem = Arc::new(Semaphore::new(INFLIGHT_PERMITS));
        let dropped = Arc::new(AtomicU64::new(0));

        tokio::spawn(trace_batch_worker(trace_rx, client.clone(),
                                        config.clone(), sem.clone(),
                                        dropped.clone()));
        // ... one spawn per signal

        Arc::new(Self { trace_tx, log_tx, metric_tx, dropped_total: dropped })
    }
}
```

**Batch worker loop (same pattern for every signal family):**

```rust
async fn trace_batch_worker(
    mut rx: mpsc::Receiver<Vec<SpanRecord>>,
    client: reqwest::Client,
    config: OoConfig,
    sem: Arc<Semaphore>,
    dropped: Arc<AtomicU64>,
) {
    let mut buf: Vec<SpanRecord> = Vec::new();
    let mut buf_bytes: usize = 0;
    let mut interval = tokio::time::interval(Duration::from_millis(LINGER_MS));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Some(batch) => {
                        let batch_bytes = estimate_bytes(&batch);
                        buf.extend(batch);
                        buf_bytes += batch_bytes;
                        if buf.len() >= MAX_BATCH_RECORDS || buf_bytes >= MAX_BATCH_BYTES {
                            flush_traces(&mut buf, &mut buf_bytes,
                                         &client, &config, &sem, &dropped).await;
                        }
                    }
                    None => break,  // channel closed; forwarder is shutting down
                }
            }
            _ = interval.tick() => {
                if !buf.is_empty() {
                    flush_traces(&mut buf, &mut buf_bytes,
                                 &client, &config, &sem, &dropped).await;
                }
            }
        }
    }
}
```

**Backpressure:** `Semaphore` bounds concurrent in-flight POSTs. When permits are
exhausted, batches queue in the channel. When the channel fills, `try_send` returns
`Err` and the producer increments `dropped_total`. This ensures the forwarder
NEVER blocks producers regardless of OO's availability.

**Retry:** exponential backoff with jitter on 429 / 502 / 503 / 504 / connection
errors, honoring `Retry-After` on 429. Never retry 400 / 401 / 403 (no point).
On HTTP 200 with a non-empty `partial_success.rejected_*`, log a WARN with counts.

### 4. Configuration

Mirror the `TEMPS_CLICKHOUSE_*` / `is_clickhouse_enabled()` precedent in
`crates/temps-config/src/service.rs:247`.

New fields on `ServerConfig`:

| Env var | Field | Meaning |
|---|---|---|
| `TEMPS_OPENOBSERVE_URL` | `openobserve_url: Option<String>` | Base URL, no trailing slash — OO 404s on `//` |
| `TEMPS_OPENOBSERVE_AUTH_HEADER` | `openobserve_auth_header: Option<String>` | Ready-to-paste `Basic <ingestion-token>` — ingestion-scoped token, not user credentials |
| `TEMPS_OPENOBSERVE_ORG` | `openobserve_org: String` | Default `"default"` |
| `TEMPS_OPENOBSERVE_ENABLED` | `openobserve_enabled: bool` | Hard on/off gate |

```rust
pub fn is_openobserve_enabled(&self) -> bool {
    self.openobserve_enabled
        && self.openobserve_url.is_some()
        && self.openobserve_auth_header.is_some()
}
```

**Resolving the env-var vs entity-column tension:**

CLAUDE.md mandates that runtime configuration be an entity column, not an env var,
so operators can change it per-record via the API without a binary restart. This
ADR follows that rule for the per-project and per-environment **toggles** (whether
forwarding is on, which signals are forwarded). However, the OO endpoint URL, auth
header, and org are a **deployment-wide secret and connection bootstrap** — analogous
to `TEMPS_CLICKHOUSE_URL` / `TEMPS_CLICKHOUSE_PASSWORD` (ADR-016), which are also
env vars. Putting a customer's authentication secret in a DB column (even encrypted)
that is readable via the management API creates an unnecessary exfiltration surface.
The connection bootstrap therefore lives in env vars; the per-project behavioral
toggles live in entity columns. This is the explicit split:

**Entity columns (runtime-editable, auditable):**

```sql
-- projects table (new nullable columns)
ALTER TABLE projects ADD COLUMN oo_forwarding_enabled   BOOLEAN;  -- NULL=inherit global
ALTER TABLE projects ADD COLUMN oo_forward_signals      JSONB;    -- per-signal opt-out set

-- environments table (tri-state, NULL=inherit project)
ALTER TABLE environments ADD COLUMN oo_forwarding_enabled  BOOLEAN;  -- Option<bool>
```

The tri-state `Option<Option<bool>>` inheritance pattern, proven by per-environment
`attack_mode` at `crates/temps-entities/src/environments.rs:45` and the
`deserialize_optional_optional_bool` helper in
`crates/temps-environments/src/handlers/types.rs`, applies directly:
- `NULL` on `environments.oo_forwarding_enabled` → inherit the project's column.
- `true`/`false` → explicit override for this environment.

Optional per-project endpoint/auth/org overrides (encrypted columns on `projects`)
allow one Temps install to fan different tenant projects to different customer OO
orgs. This is Phase 4 scope.

### 5. Multi-tenancy and stream naming

**Default:** one OO org per global config (the `TEMPS_OPENOBSERVE_ORG` value).
Per-Temps-project isolation is achieved by **stream naming**, not by OO org
separation (OO multi-org isolation is Enterprise-only and cannot be assumed).

Stream names MUST be lowercase — OO's `format_stream_name` lowercases silently and
a case-mismatch causes a hidden split into two streams. Scheme:

| Signal | Stream name |
|---|---|
| OTLP traces | `temps_p<project_id>_traces` |
| OTLP logs | `temps_p<project_id>_logs` |
| OTLP metrics | `temps_p<project_id>_metrics` |
| Error events | `temps_p<project_id>_errors` |
| Alarms | `temps_p<project_id>_alarms` |
| Revenue events | `temps_p<project_id>_revenue` |
| Analytics events | `temps_p<project_id>_analytics` |
| Web vitals | `temps_p<project_id>_webvitals` |
| Container stdout/stderr | `temps_p<project_id>_container_logs` |
| Scraped svc metrics | `temps_p<project_id>_svc_metrics` |
| Audit log | `temps_audit` (org-wide — no project_id on the audit table) |

OTLP signals are routed to `<base>/api/<org>/v1/{traces,logs,metrics}` with the
`stream-name` HTTP header set to the per-project stream name. Native-JSON signals
are routed to `<base>/api/<org>/<stream>/_json`.

### 6. Per-signal tap points

#### 6a. OTLP traces (P0)

**Tap:** `crates/temps-otel/src/handlers/ingest_handler.rs`, inside
`do_ingest_traces`. After `decode_traces_request` at `:558` and the debug-log loop
at `:562-575`, before `ingest_spans` at `:577`. Clone the `Vec<SpanRecord>` and
`try_send` it.

```rust
// do_ingest_traces — add after :575, before :577
if let Some(sink) = &state.telemetry_sink {
    let batch = spans.clone();
    sink.try_send_spans(batch);
}
let stored = state.otel_service.ingest_spans(spans).await?;
```

**NOT** a storage-trait decorator (see Context, rejected alternative).

**Transform:** `SpanRecord` (`crates/temps-otel/src/types.rs:167`) →
`opentelemetry_proto::tonic::trace::v1::ResourceSpans`. Key conversions:
- `trace_id` / `span_id` hex strings → 16/8-byte `Vec<u8>`.
- `start_time` / `end_time` `DateTime<Utc>` → `timestamp_unix_nano` (`u64`).
- `BTreeMap<String,String>` attributes → `Vec<KeyValue>`.
- `SpanStatusCode` → `Status { code, message }`.
- `Vec<SpanEvent>` → `Vec<opentelemetry_proto::...::Span_Event>`.
- Encode with prost; POST to `<base>/api/<org>/v1/traces` with
  `Content-Type: application/x-protobuf` and `stream-name: temps_p<pid>_traces`.

#### 6b. OTLP logs (P0)

**Tap:** inside `do_ingest_logs`. After `decode_logs_request` at `:611`, before
`ingest_logs` at `:624`.

```rust
if let Some(sink) = &state.telemetry_sink {
    sink.try_send_logs(records.clone());
}
let stored = state.otel_service.ingest_logs(records).await?;
```

**All severities forwarded.** The OSS `/observe` page DB-indexes only WARN+ (see
`otel_service.rs:220-223` severity filter), but the forwarding tap is placed before
that filter. The OO customer receives all severity levels and can filter on their
side.

**Transform:** `LogRecord` (`types.rs:233`) → `ResourceLogs` / `ScopeLogs` /
`LogRecord`. `timestamp` / `observed_timestamp` → `time_unix_nano`. Forward
`trace_id` and `span_id` strings (enables OO log-to-trace correlation).

#### 6c. OTLP metrics (P0)

**Tap:** inside `do_ingest_metrics`. After `decode_metrics_request` at `:475`,
before the `OtelStorage.ingest_metrics` call at `:508` (which is awaited).

```rust
if let Some(sink) = &state.telemetry_sink {
    sink.try_send_metrics(points.clone());
}
let stored = state.otel_service.ingest_metrics(points).await?;
```

This mirrors the existing non-blocking `try_send` pattern at `:517-524` (the
MetricsStore channel send).

**Histograms included.** The OSS `otlp_to_store_point` function at `:185-188`
deliberately returns `None` for `MetricType::Histogram` — histograms are only
stored in `otel_metrics`, not in the unified `MetricsStore`. The forwarder receives
the full `MetricPoint` (which carries `histogram_*` fields at `types.rs:95-107`)
and encodes them directly into OTLP `HistogramDataPoint`. Do NOT reuse
`otlp_to_store_point` in the encode path or histograms will be silently dropped.

**Transform:** `MetricPoint` (`types.rs:83`) → `ResourceMetrics`. Gauge/Sum points
map to OTLP `Gauge`/`Sum`; Histogram points map to OTLP `Histogram` with explicit
bucket boundaries from `histogram_bounds` + `histogram_bucket_counts`. Counters
from OTLP carry delta semantics — set `AggregationTemporality::Delta` on Sum
metrics.

#### 6d. Container stdout/stderr logs (P1)

**Tap:** `crates/temps-log-aggregator/src/services/collector.rs`, inside
`stream_container_logs`, after `parse_log_line` at `:366`. The existing broadcast
at `:372` (`let _ = tail_tx.send(line.clone())`) is already fire-and-forget; add a
sibling sink send.

```rust
let line = parse_log_line(msg, ts, stream_type, &ctx);
let _ = tail_tx.send(line.clone());   // existing live-tail broadcast
if let Some(sink) = &self.telemetry_sink {
    sink.try_send_container_logs(vec![line.clone()]);
}
```

**Transform:** `LogLine` (`crates/temps-log-aggregator/src/types.rs:10`) → OO
native JSON. `LogLine.ts` is a `DateTime<Utc>` (ISO-8601, NOT microseconds — the
microseconds conversion happens at the DB write layer). The forwarder must
explicitly convert to OO's required `_timestamp` field (microseconds since epoch):

```rust
"_timestamp": line.ts.timestamp_micros(),
"level": line.level.to_string(),
"stream": line.stream.to_string(),
"message": line.msg,
"container_id": &line.container_id,
"service": &line.service,
"env": &line.env,
"project_id": line.project_id,
"deploy_id": line.deploy_id,
// "fields" flattened inline or as a sub-object
```

POST to `<base>/api/<org>/temps_p<pid>_container_logs/_json` as a JSON array.

#### 6e. Error events (P1)

**Tap:** `crates/temps-error-tracking/src/services/error_ingestion_service.rs`,
after `new_event.insert(...)`. Exact current line should be verified before editing;
the verified finding places this in `create_error_event` around `:600`.

**Transform:** → OO native JSON. Include at minimum:
`_timestamp`, `level: "ERROR"`, `error_type`, `message`, `group_id`, `project_id`,
`environment_id`, `deployment_id`, `trace_id` (enables OO error-to-trace
correlation), `user`/`device`/`request` context fields.

**Critical:** OO enforces `ZO_COLS_PER_RECORD_LIMIT=200` columns per record and
silently discards records exceeding that limit. The raw Sentry-compatible `payload`
field is typically wide. Nest it under a single JSON-string column (`raw_payload`)
rather than flattening. Verify the customer's OO instance has been configured with
`ZO_COLS_PER_RECORD_LIMIT` raised if the event schema is wider than 200 top-level
keys.

#### 6f. Scraped service/DB metrics (P2)

**Tap:** `crates/temps-metrics/src/scraper.rs`, inside `MetricsScraper::run_cycle`,
before `self.store.write_batch(all_points)`. The `all_points` vector is moved into
`write_batch`, so it must be cloned before the move. There is no sink field on
`MetricsScraper` today — add `metrics_sink: Option<Arc<dyn TelemetrySink>>` to
the struct and constructor.

```rust
// run_cycle — add before write_batch call
if let Some(sink) = &self.metrics_sink {
    sink.try_send_metrics(all_points.clone());
}
self.store.write_batch(all_points).await?;
```

**Counter delta semantics:** `crates/temps-metrics/src/store/mod.rs:57-72` shows
that the scraper emits delta values for counters (current reading minus previous
reading). The forwarder must set `AggregationTemporality::Delta` on Sum metrics.
Dashboard operators need to understand that the forwarded counter values are deltas,
not cumulative totals, to build correct rate-of-change visualizations in OO.

#### 6g. Container resource metrics (P2)

**Tap:** `crates/temps-monitoring/src/container_health.rs`, inside
`write_container_metrics`, before `store.write_batch`. The store write is already
non-fatal; the sink send must likewise be non-fatal.

**Transform:** gauges (cpu_utilization_percent, memory_used_bytes, network I/O
deltas) with project / environment / deployment / container labels as OTLP metric
attributes. POST to OTLP metrics endpoint with
`stream-name: temps_p<pid>_svc_metrics`.

#### 6h. Alarms (P2)

**Tap:** Subscribe to the existing `JobQueue` broadcast via `JobQueue::subscribe()`
(`crates/temps-core/src/jobs.rs:465`). Filter for `Job::AlarmFired` and
`Job::AlarmResolved` emitted at `crates/temps-monitoring/src/alarm_service.rs`
(verified around `:274-289`). No new producer hook is needed — consume off the
broadcast queue.

**Known limitation:** The `AlarmFired` job struct carries only `alarm_id`,
`project_id`, `environment_id`, `deployment_id`, `alarm_type`, `severity`, and
`title`. Fields like `fired_at`, `message`, `metadata`, `container_id`, and
`service_id` are absent. The forwarder subscriber must either enrich from the DB
(look up the alarm row by `alarm_id`) or accept that the forwarded alarm record
will be thin. The DB-enrichment path is safer for observability completeness but
adds a DB read per alarm event.

#### 6i. Revenue events (P2)

**Tap:** `crates/temps-revenue/src/service/ingestion.rs`, post-commit, after
`event_row.insert(txn)` (verified around `:205`), best-effort (mirroring the
`mark_active` call at `:151` which is already non-fatal).

**Known prerequisite:** The correlation columns `deployment_id`, `environment_id`,
and `trace_id` exist on `revenue_events` (migration `m20260502_000001`) but are
never populated — `crates/temps-revenue/src/handlers/public.rs` does not extract
`X-Temps-*` headers. Forwarded revenue records will have these as `NULL` until a
separate prerequisite fix lands. Flag this to the customer.

**Transform:** `revenue_events::Model` → OO native JSON. Exclude `payload`
(`#[serde(skip_serializing)]` in the entity), which is the raw webhook body.

#### 6j. Analytics events and web vitals (P2)

**Tap:** `crates/temps-analytics-events/src/services/events_service.rs`, after
`event.insert` (verified around `:1346`). Model after the existing
`events_ch_outbox` non-blocking fan-out pattern (`:1542-1558`): the outbox insert
is separate, non-transactional, and failure is logged at debug — the same semantics
apply to the OO sink.

**Session replay is excluded.** rrweb blob payloads are not observability signals;
forwarding them to OO would cause storage explosion in the customer's OO instance.
Set this expectation with the customer explicitly.

**Transform:** events → OO native JSON `temps_p<pid>_analytics`. Web vitals arrive
in the `events` table as optional columns and also in a separate
`performance_metrics` table; the `events` table is the unified tap for both.

#### 6k. Audit log (P2)

**Tap:** `crates/temps-audit/src/services/audit_service.rs`, after insert in
`create_audit_log_typed` (verified around `:50`). Failures non-fatal.

**Important limitation:** the `audit_logs` table has no `project_id` column
(verified). The forwarded audit stream is therefore org-wide
(`temps_audit`) — there is no per-project isolation possible without a schema
change. If the customer expects per-project audit log isolation in OO, this
requires adding `project_id NULLABLE` to `audit_logs` first (a separate,
opt-in migration). Forward only what the schema carries: `_timestamp` (from
`audit_date`), `operation_type`, `user_id`, `user_agent`, `ip`, `data`.

### 7. EE gating

This feature should gate on a licensed `Feature::OpenObserveForwarding`. However,
the EE licensing crate (`temps-ee-license` with the `Feature` enum) is not present
in the current `temps-ee` checkout — `temps-ee/crates/` contains only
`temps-ee-network-policy`. The active branch at time of writing is
`feat/on-demand-tls`, not the branch where the licensing crate was developed.
Until the licensing crate is located and the branch is merged or cherry-picked,
**the EE binary shipping the plugin is itself the gate**: operators who do not run
the EE binary cannot enable OO forwarding regardless of env var configuration.
The `Feature` enum import will be added once the crate is available.

See Open Questions for the tracking item.

## Consequences

### Positive

- Apps emit OTLP once to Temps; Temps fans out to OO without any SDK reconfiguration.
  The customer's OO instance gets traces, logs, and metrics in OTLP-native format
  with zero changes to application code.
- Platform signals (container logs, errors, alarms, scraped metrics, revenue,
  analytics, audit) are forwarded to OO as well — enriching the customer's OO
  dashboards with infrastructure context not visible to the app SDK.
- Bounded `mpsc` channels and a `Semaphore` on in-flight POSTs ensure OO
  availability problems never stall the OTLP ingest hot path or any platform signal
  producer. The forwarder degrades to load-shedding under sustained OO slowness.
- The `TelemetrySink` trait in OSS core is a clean seam: OSS producer crates carry
  the hook at zero cost (no-op default), and swapping in the EE forwarder requires
  no changes to OSS code.
- Off by default. The OSS single-binary `temps serve` is unchanged.

### Negative / risks

- **EE licensing crate missing from current checkout (top blocker):** `Feature`
  gating cannot be wired until `temps-ee-license` is located and merged. Until
  then, the EE binary is the only gate. Any operator who obtains the EE binary and
  sets `TEMPS_OPENOBSERVE_ENABLED=true` can use the feature regardless of license.
- **OO 200-field record limit:** `ZO_COLS_PER_RECORD_LIMIT=200` is enforced at
  ingestion and silently discards records exceeding the limit. Error events with
  a wide Sentry payload and high-cardinality metric labels are the most likely to
  hit this. The forwarder must nest extras under a single JSON-string column; the
  customer must raise the limit in their OO config if needed.
- **OO memory circuit-breaker and nginx body limit:** OO has no classic rate limit
  but enforces a memory circuit-breaker and (when behind nginx) a `proxy_body_size`
  cap typically around 1 MB. The forwarder's 4 MB `MAX_BATCH_BYTES` may need to be
  tuned down for community-edition OO deployments. Size batches conservatively.
- **Counter delta semantics:** scraped service metrics carry delta values
  (`store/mod.rs:57-72`). Setting `AggregationTemporality::Delta` on Sum metrics is
  correct, but OO dashboards expecting cumulative counters will show spiky rates
  rather than monotonically increasing totals. Document this per-signal.
- **Silent partial loss under sustained OO slowness:** bounded channels load-shed
  when full. `dropped_total` makes the loss observable, but the customer will not
  see dropped records in OO. The forwarder's self-observability (Phase 4) must
  surface this counter in the Temps management UI.
- **Forwarding doubles egress for high-volume tenants.** Traces and container logs
  can be the largest signals. Monitor egress cost before enabling on high-volume
  projects.
- **Revenue correlation fields currently always NULL.** Until the revenue ingestion
  handler is fixed to extract `X-Temps-*` headers, `deployment_id`,
  `environment_id`, and `trace_id` on revenue events forward as NULL.
- **Audit log has no project_id.** Org-wide stream only. Per-project isolation
  requires a schema migration not in scope here.
- **Session replay excluded.** rrweb blobs are not observability signals. The
  customer should not expect session replay recordings in OO.

### Neutral

- No change for OSS operators who do not set `TEMPS_OPENOBSERVE_ENABLED=true`.
  The `TelemetrySink` slot is None; the no-op path has no overhead.
- OO-specific stream naming (`temps_p<pid>_*`) keeps signals isolated without
  requiring OO Enterprise multi-org. The customer can create OO dashboards and
  alert rules scoped to a stream name.

## Phased plan

### Phase 0 — Foundation

- Add `TEMPS_OPENOBSERVE_*` fields to `ServerConfig`
  (`crates/temps-config/src/service.rs`) and `is_openobserve_enabled()`.
- Add `TelemetrySink` trait and `NoopTelemetrySink` to `crates/temps-core`.
- Create the `temps-ee-oo-forwarder` crate: shared `reqwest::Client` (gzip,
  `.timeout(Duration::from_secs(30))`, `.connect_timeout(Duration::from_secs(10))`);
  per-signal bounded `mpsc` channels + batch workers with size/linger flush;
  `Semaphore`-bounded in-flight POSTs; exponential-backoff retry; `dropped_total`
  counter.
- Implement OTLP encode helpers (`opentelemetry-proto` / `prost`) for traces, logs,
  and metrics, and native `_json` POST helper.

### Phase 1 — P0: OTLP signals (the core ask)

- Wire `OtelAppState` to carry `Option<Arc<dyn TelemetrySink>>`.
- Add handler-level fire-and-forget taps in `do_ingest_traces` (after `:558`),
  `do_ingest_logs` (after `:611`), and `do_ingest_metrics` (after `:475`).
- Per-project stream naming (`temps_p<pid>_*`) and global org from config.
- Satisfies the customer's primary request: apps unchanged, OO receives all OTLP.

### Phase 2 — P1: platform signals

- Container logs tap in `collector.rs` + `_json` POST helper.
- Error events tap in `error_ingestion_service.rs`.
- Per-project `oo_forwarding_enabled` entity columns + tri-state inheritance.

### Phase 3 — P2: remaining platform-native signals

- Scraped service/DB metrics (OTLP) + container resource metrics (OTLP).
- Alarms (JobQueue subscriber + DB enrich).
- Revenue events (`_json`).
- Analytics events + web vitals (`_json`).

### Phase 4 — Hardening and multi-tenant

- Audit log (`_json`, org-wide stream, explicit limitation documented in UI).
- Per-project entity columns for endpoint/auth/org overrides (encrypted columns on
  `projects`), enabling one Temps install to fan to different customer OO instances.
- Forwarder self-observability: queue depth gauge, `dropped_total` counter, OO
  4xx/5xx rates, `partial_success.rejected_*` counts — all surfaced in Temps
  management UI.
- `Feature::OpenObserveForwarding` gate once the EE licensing crate is located.

## Open questions

1. **Where is the `temps-ee-license` crate (the `Feature` enum)?** The current
   `temps-ee` checkout (`feat/on-demand-tls`) contains only `temps-ee-network-policy`.
   The licensing crate must be located or cherry-picked before `Feature::OpenObserveForwarding`
   can be wired. Confirm branch before starting Phase 1.

2. **OO Community vs Enterprise or Cloud?** Community single-org (isolate by stream
   naming, as designed here) vs OO Enterprise/Cloud (true org-per-tenant with RBAC)?
   Determines whether the per-project endpoint/auth/org override columns in Phase 4
   are needed or whether stream naming is sufficient.

3. **One OO destination for the whole install, or per-project destinations?** If
   the customer wants different Temps projects to forward to different OO instances
   (different customer orgs or different environments), the Phase 4 per-project
   encrypted column design covers it. Confirm scope before implementing Phase 4.

4. **Scraped counter metrics: delta or cumulative?** Forward deltas as-is with
   `AggregationTemporality::Delta` (correct for what the scraper emits, but spiky
   in OO dashboards) or reconstruct cumulative client-side (requires state and
   is error-prone across restarts)? Delta is the honest representation; confirm
   the customer's OO dashboard style before deciding.

5. **Alarm enrichment:** forward the thin `AlarmFired` job struct (only alarm_id +
   basic fields) or enrich from the DB? Enrichment adds a DB read per alarm but
   gives the customer a complete alarm record in OO. Decide before Phase 3.

6. **OO column limit:** has the customer raised `ZO_COLS_PER_RECORD_LIMIT` above
   200 in their OO instance? If not, error events and revenue events must
   aggressively nest or flatten wide payloads to stay under the limit.

7. **Forwarding audit logs without project_id scoping:** is org-wide forwarding
   acceptable to the customer, or does the audit log require per-project isolation?
   If the latter, `audit_logs` needs a `project_id NULLABLE` column (a separate
   schema change).

8. **Session replay:** the customer asked for "as much as possible" — confirm
   explicitly that rrweb session replay blobs are excluded and the customer
   understands why.

9. **Exact line numbers:** the otel handler taps (`do_ingest_metrics/traces/logs`)
   were directly verified against the codebase at time of writing. The platform-
   native taps (error_ingestion_service, events_service, scraper, alarm_service)
   were cited from a prior code-reading pass and should be re-verified immediately
   before editing, as these files drift between sessions.

## References

- [ADR-016: ClickHouse as the OTel telemetry backend](016-clickhouse-traces-backend.md) —
  the `OtelStorage` backend this forwarder taps in front of; the `TEMPS_CLICKHOUSE_*` /
  `is_clickhouse_enabled()` config precedent is directly reused.
- [ADR-012: ClickHouse as an external analytics backend](012-clickhouse-analytics-backend.md) —
  the `events_ch_outbox` fan-out pattern reused for the analytics signal tap.
- `crates/temps-otel/src/handlers/ingest_handler.rs` — the verified OTLP tap points
  (`:475` metrics, `:558` traces, `:611` logs; awaited storage calls at `:508`, `:577`,
  `:624`; existing non-blocking channel send pattern at `:517-524`).
- `crates/temps-otel/src/services/otel_service.rs:228` — `store_logs` awaited
  synchronously; confirms the storage-decorator alternative is rejected.
- `crates/temps-otel/src/types.rs` — `MetricPoint` (`:83`), `SpanRecord` (`:167`),
  `LogRecord` (`:233`) — the decoded structs the forwarder clones.
- `crates/temps-otel/src/handlers/ingest_handler.rs:185-188` — `otlp_to_store_point`
  returns `None` for `MetricType::Histogram`; the forwarder must NOT reuse this function.
- `crates/temps-log-aggregator/src/types.rs:10` — `LogLine` struct; `ts` is
  `DateTime<Utc>` (ISO-8601), not microseconds.
- `crates/temps-log-aggregator/src/services/collector.rs:366-372` — `parse_log_line`
  call and existing `tail_tx.send(line.clone())` fire-and-forget; forwarder tap goes here.
- `crates/temps-core/src/jobs.rs:460-465` — `JobQueue::subscribe()` (alarm tap).
- `crates/temps-config/src/service.rs:247` — `is_clickhouse_enabled()` precedent
  for `is_openobserve_enabled()`.
- `crates/temps-entities/src/environments.rs:45` — `attack_mode: Option<bool>`
  tri-state per-environment precedent.
- `crates/temps-environments/src/handlers/types.rs` — `deserialize_optional_optional_bool`
  helper for `Option<Option<bool>>` PATCH semantics.
- `crates/temps-metrics/src/store/mod.rs:57-72` — delta counter semantics on scraped
  service metrics.
- OpenObserve documentation — OTLP ingestion endpoints; native `_json` bulk API;
  `ZO_COLS_PER_RECORD_LIMIT`; stream-name casing requirement.
