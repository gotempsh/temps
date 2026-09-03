# ADR-043: Cloud-Primary Writes for All ClickHouse-Backed Entities — Phase C/D

**Status:** Proposed
**Date:** 2026-09-03
**Author:** David Viejo
**Builds on:** ADR-040 (Cloud Telemetry Read Source), ADR-041 (Cloud-Primary Telemetry Writes), ADR-042 (One-Click Cloud Telemetry Activation)

> **Numbering note.** ADR-042 (`042-one-click-cloud-telemetry-activation.md`) is
> the most recent committed ADR; 043 is the next free number in the committed
> sequence.

> **Revision note — 2026-09-03, after implementation review.** This ADR was
> revised in place while still `Proposed`. The original text specified three
> layers of bespoke machinery on top of the outbox: a per-entity JSON wire
> protocol (`MetricBatchMessage`, `AnalyticsEventBatchMessage`,
> `ProxyLogBatchMessage`) with a content-type per entity that Cloud must parse
> and reinsert; per-entity typed outbox accessors (`SpanOutbox`, `MetricOutbox`,
> `AnalyticsEventOutbox`, …) each with their own claim/deliver/retry/dead-letter
> method set; and a declare/upload/complete handshake for backfill borrowed from
> the backup-mirror path. **All three duplicate something that already exists:
> ClickHouse's own row wire format, which every entity in scope already has a
> `#[derive(clickhouse::Row)]` struct for on its local insert path, and which
> ADR-040 §2 already established as the correct reuse pattern for the read
> side.** The durable local outbox itself is *not* reverted — it is required
> regardless of wire format, because CLAUDE.md's hot-path rules forbid
> synchronous network I/O on span ingest, analytics ingest and the proxy path —
> and neither is the shipped outbox schema. What the revision removes is the
> hand-rolled protocol and the over-typing above that schema.
>
> The most visible consequence is that **Alternatives Considered → Option D (a
> generic outbox) is now the chosen design rather than the rejected one**; that
> entry records why the original rejection reasoning was wrong, so a reader who
> jumps straight there is not left guessing. Unchanged by this revision: the
> Context's product goal, the two-switch write-mode granularity decision (§1),
> the phase sequencing rationale (C1 metrics → C2 analytics → C3 proxy logs),
> and every Open Question.

> **Revision note 2 — 2026-09-03, same day, second review pass.** §6's backfill
> design still had per-entity granularity left over from before the outbox
> simplification: a required `--entity spans|metrics|analytics-events|proxy-logs`
> flag, justified by per-entity volume/consent differences. The human owner
> rejected this too: the write-mode switches are already grouped by design (§1)
> so an operator reaches Cloud-primary with two decisions total; a backfill
> command that then demands a third, finer-grained decision per entity fights
> that grouping. §6 now derives backfill's scope from whichever switch is
> `cloud` rather than taking it as a flag — one command, `temps backfill
> cloud-telemetry --project <id>`, no `--entity`. Per-entity visibility survives
> in the `--dry-run` output (a row/byte/ETA breakdown per table), just not as an
> input. Unchanged by this pass: everything the first revision note lists,
> plus §5's insert-transport design and the four-step backfill loop mechanics
> (only the command surface and the migration-sequence example changed).

---

## Context

### The goal

ADR-041 §0 explicitly scoped out all non-span ClickHouse-backed signals:
`otel_metrics`, `analytics_events`, `analytics_sessions`, `proxy_logs`,
`service_metrics`, and `cross_project_trace_refs`. The stated motivation for
that scope was delivery risk, not a disagreement about the goal. The product
owner's stated end state — confirmed again as the prompt for this ADR — is:

> **A self-hosted instance connected to Temps Cloud should not need to run
> ClickHouse locally at all.**

Today spans have a Cloud-primary path. Everything else still requires local
ClickHouse or degrades silently to the TimescaleDB fallback. This ADR designs
the remaining phases.

### What "no local ClickHouse needed" actually means — and what it does not

Finding 1 of ADR-041 is load-bearing here: `ServerConfig::is_clickhouse_enabled()`
is already the *optional* branch. The default install ships no ClickHouse at all
(`docker-compose.yml` has no ClickHouse service). Every domain that has a
ClickHouse backend also has a TimescaleDB/Postgres fallback that is already used
by the majority of self-hosted operators. So the product goal is not "ClickHouse
stops being a dependency" — it already is not. The goal is:

> **Operators who choose to run no ClickHouse, and link to Temps Cloud, should
> be able to access full-fidelity analytics, metrics, and proxy-log data from
> the Cloud-served UI rather than silently losing those features.**

The delta from today: the six entity domains listed above, when Temps Cloud is
linked, should each be serveable from Cloud instead of from local storage, so
the gap between "ClickHouse disabled" and "full feature set" closes.

### The six entities, their properties, and how they differ from spans

Understanding the differences between spans and the remaining entities drives
every design decision in this ADR.

| Entity | Volume at scale | Existing local store | Existing CH backend? | Existing trait abstraction? | PII / consent surface |
|---|---|---|---|---|---|
| `otel_metrics` | Medium (label-cardinality-bounded) | TimescaleDB hypertable `otel_metrics` | `ClickHouseOtelStorage` (part of `OtelStorage`) | `OtelStorage` | Low — metric names + label values, no free text |
| `analytics_events` | **Highest** (per page-view) | Postgres `events` table (system of record) + ClickHouse via `events_ch_outbox` | `ClickHouseEventsBackend` / `AnalyticsEvents` | `AnalyticsEvents` | **Highest** — URLs, referrers, user agents, visitor IDs, custom properties — PII by construction |
| `analytics_sessions` | High (per session) | Postgres `sessions` (system of record) + CH derived | `ClickHouseEventsBackend` | `AnalyticsEvents` (same surface) | High — same PII surface as events |
| `proxy_logs` | **Highest** (per request on the reverse proxy path) | TimescaleDB hypertable `proxy_logs` | ClickHouse (`crates/temps-proxy/src/storage/clickhouse.rs`, `ChProxyLogRow`) | Weak — mostly concrete types over `DatabaseConnection` | Medium — request paths and headers, no body |
| `service_metrics` | Medium | TimescaleDB hypertable (separate from `otel_metrics`) | `ClickHouseOtelStorage` | `OtelStorage` | Low — same label-bounded shape as OTel metrics |
| `cross_project_trace_refs` | Low | TimescaleDB / Postgres `cross_project_trace_refs` | `ClickHouseOtelStorage` | `OtelStorage` | Low — trace IDs only (already gated on `Queryable` fidelity) |

Three entities — `otel_metrics`, `service_metrics`, and `cross_project_trace_refs`
— are already part of the `OtelStorage` trait. Their Cloud-primary path is
mechanically similar to spans: the same decorator (`CloudRoutedOtelStorage`) and
the same outbox/worker pattern, operating on a different set of methods. They
inherit the span path's infrastructure rather than needing their own.

Two entities — `analytics_events` and `analytics_sessions` — are the highest-volume
signals and carry the largest PII surface. They also already have the most
complete prior art: `events_ch_outbox` in Postgres (already production-proven for
the local CH fanout), and the `AnalyticsEvents` trait already abstracts reads.
Their Cloud-primary path is structurally a new sink in the existing outbox, not a
new outbox.

`proxy_logs` stands alone in volume characteristics (every request through the
reverse proxy), storage location (a TimescaleDB hypertable with no Postgres
system-of-record row to reference), and trait maturity (the weakest abstraction
of the six). It is the hardest entity to extend and the last that should go.

### The write-mode switch granularity question

ADR-041 §0 left this open: one switch per project covering all entities, one
switch per entity per project, or an instance-wide default with per-entity
overrides.

This ADR decides:

> **One additional per-project switch for non-span telemetry signals, covering
> analytics events, analytics sessions, otel metrics, service metrics, and
> proxy logs as a group, with cross_project_trace_refs following the span
> switch automatically.**

The reasoning for grouping rather than per-entity is operator ergonomics and
the stated product goal. An operator cutting over to Cloud wants zero local
ClickHouse. If each entity has its own switch, reaching that state requires
five separate API calls (or UI interactions) per project, each with its own
prerequisite chain — a usability failure that defeats the stated purpose of
reducing operational burden.

The reasoning for two groups (spans / everything-else) rather than one is
volume asymmetry. The span switch exists and has its own guard logic in ADR-041
precisely because spans were ready first and the other entities were not. Merging
them into one switch now would mean a project cannot go Cloud-primary for
analytics without also going Cloud-primary for spans and vice versa, which
removes the ability to migrate incrementally. The two switches remain orthogonal:
a project may have spans in Cloud and analytics local, or vice versa, as an
intermediate state during a cutover. Both switches default to `local`; neither
gate guards the other.

The reasoning for excluding `cross_project_trace_refs` from the non-span group is
architectural: trace refs are a Postgres-backed reverse index used to discover
cross-project traces (ADR-027). They are populated by the same span ingest path
that produces Cloud-primary spans, and the existing `CloudRoutedOtelStorage`
already delegates `record_trace_refs` to local unconditionally — the comment in
that code explains why routing them to Cloud would break cross-project linking
for exactly the projects that need it most. Trace refs are in scope for inclusion
as a Cloud-readable signal (the read path), but their write mode follows the span
write mode by construction: if the span write is Cloud-primary, a cross-project
query already reaches Cloud to fetch the trace. No separate toggle is needed.

The two-switch outcome at steady state:

```
projects.cloud_telemetry_write_mode       ← spans (ADR-041)
projects.cloud_analytics_write_mode       ← analytics events, sessions, otel metrics,
                                            service metrics, proxy logs (this ADR)
```

Both default to `local`; both require `queryable` fidelity; both require an
active Cloud link. The non-span switch has its own, weaker consent surface
because the PII implications differ by sub-entity within the group (see §2
below).

### The outbox/routing infrastructure generalization question

The span path has:

- `cloud_span_outbox` — Postgres outbox table with payload stored in-row
- `SpanOutbox` (`temps-cloud-client/src/outbox.rs`) — the claim/deliver/dead-letter
  type, tightly coupled to `SpanRecord` and `temps_cloud_protocol`
- `CloudTelemetrySpanSource` (`temps-otel/src/storage/cloud_spans.rs`) — the
  Cloud-side read implementation for spans
- `CloudRoutedOtelStorage` (`temps-otel/src/storage/cloud_routed.rs`) — the read
  routing decorator for the `OtelStorage` trait
- `TelemetryWriteModeService` — the interval ledger

Six more entities means six more table-per-table, type-per-type copies of this
infrastructure if not generalized. The question is whether generalizing
`cloud_span_outbox` into a shared `cloud_telemetry_outbox` keyed by entity type
is the right move.

**First, what is genuinely required, and why the outbox is not the thing to cut.**
None of these entities can be written to Cloud synchronously from the code path
that produces them. Span ingest, analytics event ingest and the proxy request
path are all hot paths under CLAUDE.md's classification, and the hot-path rules
are explicit: *no synchronous I/O and no per-operation network/DB/disk calls*. A
Cloud write is a network round trip to a third party that may be slow,
rate-limited or down. So a durable local buffer with an asynchronous drain is
not an artefact of any particular wire format — it is the only shape that is
allowed here, and it stays. `cloud_telemetry_outbox` is not the overengineered
part.

**Decision: generalize the outbox table *and* the worker type — and push
everything entity-specific upstream of both.**

Specifically:

1. Add an `entity_type` discriminant column to `cloud_span_outbox` and rename it
   `cloud_telemetry_outbox`. The existing span rows remain valid (their entity
   type is `span`). No data migration is needed — the column defaults to `span`
   for any existing row. *(Shipped; see §2.)*
2. The payload in each row is **the entity's own ClickHouse row, serialized by
   the same `#[derive(clickhouse::Row)]` struct the local insert path already
   uses.** Every entity in scope already has one, because every one of them
   already inserts into a local ClickHouse table: `ChSpanRow` and `ChMetricRow`
   (`temps-otel`), a second `ChMetricRow` for `service_metrics`
   (`temps-metrics`), `ChTraceRefRow` (`temps-otel`), `ChEventRow`
   (`temps-analytics-events`), `ChProxyLogRow` (`temps-proxy`). Nothing new is
   defined to describe a row that both sides already know how to describe.
3. Because the payload arrives already in its final destination shape, the outbox
   needs **no per-entity Rust type**. One generic accessor keyed by
   `(entity_type, target_table, row_bytes)` serves all of them: claim, deliver,
   ack, retry, dead-letter, byte-cap and gap-window are byte-identical
   operations regardless of which domain produced the row.
4. Everything that *is* entity-specific — the field allowlist projection, the
   visitor/session pseudonymisation, the label filtering — happens **before** the
   row reaches the outbox, in the domain service that builds its `Ch*Row`. That
   is a place that already exists per domain, already owns that domain's error
   enum, and already sits on the correct side of the crate boundary. Cloud-primary
   hooks in there as an additional sink for a row the domain was building anyway.
5. The outbox worker, retry logic, dead-letter handling, and gap-window semantics
   are shared: one background task drains the shared table in batches, groups a
   claimed batch by `(entity_type, target_table)`, and issues one insert per
   group. New entity types slot in without adding new background loops **and
   without adding new Rust types**.
6. The byte-cap enforcement from ADR-041 §3d is applied per-entity-type, not
   globally: analytics events at "full volume" must not crowd out span writes.
   The byte cap is therefore `max_pending_bytes_per_entity_type`, stored as an
   operator-configurable setting on the `settings` row, not an env var.

**Where the entity-specific/uniform boundary actually falls.** The original text
of this ADR drew it one layer too high: it reasoned that because *projection* is
entity-specific, *delivery* must be too, and therefore each entity needs its own
outbox type. It does not follow. Once a row has been projected and serialized,
the only facts the delivery layer needs are which table it goes to and how many
bytes it is — and both of those are plain data on the outbox row, not type
information. See Alternatives Considered → Option D, whose verdict this revision
reverses.

**What this preserves.** The spans path is load-tested to 500 spans/sec (≈30k
spans/min, ≈500 row-inserts/min in the outbox at the default batch size) and
handles 100 concurrent projects without contention (ADR-041's load test in
`crates/temps-otel/tests/cloud_primary_outbox_load_test.rs`). Sharing the table
does not change any of that: the spans worker still claims with
`WHERE entity_type = 'span' AND state = 'pending'`, which the partial index
covers, and no new lock contention is introduced. Each entity type's claim is
independently indexed.

**What this avoids.** Six additional Postgres tables, six sets of `CREATE INDEX`
statements, six additional DDL migrations that all implement near-identical
schemas — and, after this revision, six near-identical Rust accessors, six
payload message types, six Cloud-side parsers and six Cloud releases. The
maintenance surface is the generalization, not the proliferation.

**The check on that claim.** `SpanOutbox` and `MetricOutbox` as they exist in the
branch today differ only in a string literal in their `WHERE` clause. Two types,
one behaviour; the `AnalyticsEventOutbox`, session, proxy-log and trace-ref
variants the original design implied would each have added the same redundancy
again. That is the concrete evidence that the boundary in point 4 above is in
the right place.

### The interval ledger

`project_telemetry_write_intervals` records when each project was in each
write mode, and is the source of truth for read routing (when was this project
Cloud-primary vs. local?). The same table, with a new
`signal_group` discriminant column (values: `spans`, `analytics`), serves both
switches. The routing decorator for each domain queries its own signal group and
gets an independent interval. This is the same approach the outbox takes for
`entity_type`, and for the same reason: one well-tested table with a
discriminant beats two near-identical tables forever diverging in their index
strategies and vacuum behaviour.

---

## Decision

### 1. Granularity of the write-mode switch

As stated in the Context:

- One new column `projects.cloud_analytics_write_mode TEXT NOT NULL DEFAULT 'local'`
  with a `CHECK (cloud_analytics_write_mode IN ('local', 'cloud'))` constraint.
- Its gate: requires `cloud_telemetry_fidelity = 'queryable'`, the instance is
  linked, and the Cloud telemetry feature switch is on. Identical prerequisite
  chain to the span switch.
- It controls: `otel_metrics`, `service_metrics`, `analytics_events`,
  `analytics_sessions`, `proxy_logs` collectively.
- `cross_project_trace_refs` follows the span switch implicitly (no column).

**On the PII question within the non-span group.** `otel_metrics` and
`service_metrics` carry label names and values (bounded cardinality, not
free-text), which are low PII risk. `analytics_events` and `analytics_sessions`
carry URLs, visitor IDs, referrers, and custom properties (high PII risk).
`proxy_logs` carries request paths and potentially query parameters (medium PII).

A single switch for the whole group does not distinguish these risk levels —
which is acceptable precisely because `queryable` fidelity already surfaces the
consent decision at the project level, and every egress uses the existing
attribute/field allowlist model from ADR-040 §1. The consent copy shown when
raising `cloud_analytics_write_mode` to `cloud` must explicitly enumerate what
leaves: metric label names and values; analytics event types, paths and custom
properties (the exact field set already sent to local ClickHouse); session data;
proxy request paths. Operators who object to sending analytics event data but are
willing to send metrics can split the two groups — but this ADR does not add a
third switch for that split. If that split proves necessary from operator
feedback, it is a future additive column, not a design decision that needs to be
made before the first entity ships.

### 2. Shared outbox infrastructure

#### 2a. Schema (shipped — not up for revision)

Rename `cloud_span_outbox` to `cloud_telemetry_outbox` and add:

```sql
ALTER TABLE cloud_span_outbox RENAME TO cloud_telemetry_outbox;
ALTER TABLE cloud_telemetry_outbox
    ADD COLUMN entity_type TEXT NOT NULL DEFAULT 'span'
    CHECK (entity_type IN ('span', 'metric', 'analytics_event', 'proxy_log'));
```

The partial index on `(enqueued_at, id) WHERE state = 'pending'` is replaced by
a partial index on `(entity_type, enqueued_at, id) WHERE state = 'pending'`,
which is what the per-entity-type claim scan needs. The per-project index on
`(project_id, state, enqueued_at)` gains `entity_type` as the leading column
for the same reason. The settled-row retention index is unchanged — the sweep
deletes by age regardless of entity type, so adding `entity_type` there would
widen the index for no benefit.

> **Rename requires coordination.** The migration renames the table; every
> Sea-ORM entity that references `cloud_span_outbox` by its `table_name` must be
> updated in the same migration. `temps_entities::cloud_span_outbox` becomes
> `temps_entities::cloud_telemetry_outbox`. The rename is a single `ALTER TABLE`
> statement — not a copy + drop — so it is instant and does not rewrite rows.

This is implemented (`m20260903_000001_generalize_cloud_telemetry_outbox`), along
with `signal_group` on `project_telemetry_write_intervals`
(`m20260903_000002_…`) and `cloud_analytics_write_mode` on `projects`
(`m20260903_000003_…`). **The revision below does not roll any of it back.** The
four-value `entity_type` CHECK in particular turns out to be exactly right under
the revised design — see 2c.

#### 2b. One generic outbox accessor, not one per entity

The Rust layer on top of that schema is a **single** accessor over the tuple

```
(entity_type, target_table, row_bytes)
```

with no type parameter and no per-domain subclass. Its whole method set — claim
a batch, mark delivered, count attempts, dead-letter, enforce the byte cap,
open/close a gap window — reads and writes those three fields plus the
bookkeeping columns that already exist (`project_id`, `payload_bytes`,
`enqueued_at`, `attempts`, `state`, `settled_at`, `last_error`). None of those
operations can tell a metric row from a proxy-log row, and none of them needs to.

- **`row_bytes`** is the entity's own ClickHouse row, serialized by the same
  `#[derive(clickhouse::Row)]` struct that the local insert path uses. The
  producer calls the *same* projection function it would call to write locally;
  Cloud-primary is an additional sink for a value the domain was already
  constructing, not a second construction path with a second field list that can
  drift from the first.
- **`target_table`** is the destination ClickHouse table name
  (`otel_metrics`, `service_metrics`, `events`, `proxy_logs`, …). It is what
  makes the accessor generic rather than a `match` over entity types, and it is
  what lets two different tables share one `entity_type` (2c).
- **`entity_type`** stops being "which payload struct do I deserialize" and
  becomes "which byte cap, which worker class, which operator-visible queue" —
  i.e. exactly the coarse scheduling/quota discriminant the shipped CHECK
  constraint encodes.

**What each domain contributes is one function, not one type.** A domain that
wants Cloud-primary writes provides: the `Ch*Row` it already has, the projection
that builds it (allowlist, pseudonymisation — unchanged and unmoved), and the
target table name. It does not provide an outbox type, a message type, a
content-type, a serializer, a retry policy, or a dead-letter sweep.

**Column names travel with the row type, not with the row.** The `clickhouse`
crate's `Row` derive already exposes a struct's column-name list, and
`Client::insert::<T>(table)` builds `INSERT INTO {table}({columns}) FORMAT {fmt}`
from it. So the drain loop's only per-entity knowledge is a small static map
`entity_type × target_table → column list`, derived from the same `Ch*Row` types
— a data table, not a code path. There is no hand-maintained column list to keep
in sync with anything.

**The drain loop.** Claim a batch under the existing partial index; group the
claimed rows by `(entity_type, target_table)`; for each group, concatenate the
stored row payloads into one request body and issue one insert against the
Cloud-pointed client (§5); ack or retry the whole group. Concatenation is valid
framing rather than a trick: ClickHouse's row wire format has no per-row header,
so N serialized rows appended together *are* the body of an N-row insert. This
is the same property that makes the local `Inserter` buffer work.

#### 2c. Two consequences for the shipped schema, both additive

1. **`entity_type` stays a four-value enum, and the earlier `analytics_session`
   value this ADR's original §3 implied is no longer needed.** With
   `target_table` on the row, `metric` covers both `otel_metrics` and
   `service_metrics`, and `analytics_event` covers events and any future
   session-shaped rows, each distinguished by destination rather than by
   discriminant. The shipped CHECK constraint is therefore correct as written and
   needs no change. (Under the *original* design it would have needed a fifth
   value, which the shipped migration does not permit — the revision removes a
   latent follow-up migration rather than creating one.)
2. **Two additive columns are still required**, and this is the one place the
   revision touches DDL:
   - `target_table TEXT` — new, `NULL`able, so existing `span` rows (whose
     destination is implied) keep working and are backfilled/derived on read.
   - a **binary** payload column. The shipped `payload` column is `TEXT`,
     because the span payload is JSON (`SpanRecord`). ClickHouse's row format is
     binary; base64/hex into a `TEXT` column would inflate the highest-volume
     path by 33–100% and would be charged against the byte cap, which is the one
     number operators are asked to reason about. A nullable `BYTEA` sibling
     column (`payload_row`) is the correct shape: `payload` stays for the
     unchanged span path, `payload_row` carries every new entity, and
     `payload_bytes` keeps its meaning for both.

   Both are additive, defaulted and reversible. Nothing shipped is dropped,
   renamed or rewritten.

#### 2d. Byte cap per entity type

```rust
// settings row, not an env var
pub struct CloudTelemetryOutboxSettings {
    pub max_pending_bytes_span: i64,        // default: current span cap (see ADR-041 §3d)
    pub max_pending_bytes_metric: i64,      // default: 50 MiB
    pub max_pending_bytes_analytics: i64,   // default: 200 MiB
    pub max_pending_bytes_proxy_log: i64,   // default: 100 MiB
}
```

The defaults are illustrative; the correct values depend on load-testing each
entity type at the same 500 req/s level applied to spans in ADR-041, which has
not been done yet. The defaults must be revised before any entity type ships to
production. This is noted as an open question below. Note that the row format is
materially more compact than the equivalent JSON, so the *same* cap buys
noticeably more queue depth than the original design would have — but that is a
reason to re-measure the defaults, not to assume them.

### 3. Phase-by-phase rollout

#### Phase C1 — OTel metrics (first, recommended)

**Why first.** Metrics are the simplest non-span entity: bounded label
cardinality (no free text), read from the same `OtelStorage` trait that the span
path already instruments, and the lowest PII risk of all six entities. The write
path is a new method group on the shared outbox worker, and the read path is
a new set of routed methods on `CloudRoutedOtelStorage`. Because `OtelStorage` is
the same trait that the spans decorator already wraps, the routing infrastructure
is already installed — adding metric routing is a method-by-method extension of
the existing decorator, not a new decorator. No new `Arc<dyn SomethingElse>` is
needed in any plugin or handler.

There is a sequencing constraint: `service_metrics` live in `OtelStorage` too,
so Phase C1 naturally covers both OTel metrics and service metrics. Treating them
as one unit is both simpler and more correct.

**What ships in Phase C1:**

- `cloud_telemetry_outbox` table (the renamed outbox with `entity_type` column)
  — *already shipped*, plus the two additive columns from §2c (`target_table`,
  binary `payload_row`).
- `projects.cloud_analytics_write_mode` column and CHECK constraint (even though
  analytics events are Phase C2/C3 — the column gates all non-span writes and
  must exist before any of them ship) — *already shipped*.
- `project_telemetry_write_intervals.signal_group` discriminant column —
  *already shipped*.
- **The single generic outbox accessor** over `(entity_type, target_table,
  row_bytes)` (§2b), replacing `SpanOutbox`/`MetricOutbox`. Spans move onto it
  unchanged in behaviour; this is a deletion of two types, not an addition of a
  third.
- **The Cloud insert transport** on `CloudLink` — the write-side sibling of the
  existing `clickhouse_query_client()`, built from the same enrollment-derived
  credential and pointed at Cloud's per-tenant insert surface (§5). One
  transport, shared by every entity and by backfill.
- **Metric projection reusing the existing local row structs.** `ChMetricRow`
  (`temps-otel`, destination `otel_metrics`) and `ChMetricRow`
  (`temps-metrics`, destination `service_metrics`) become the Cloud-bound
  payload, built by the same allowlist-applying projection that feeds the local
  insert. The outbox stores their serialized bytes. **No `MetricBatchMessage`,
  no `MetricRow` protocol type, no metric content-type** — those are superseded
  by this revision and removed.
- In `CloudRoutedOtelStorage`: `store_metrics`, `query_metrics`,
  `list_metric_names`, `list_metric_label_keys`, `list_metric_label_values`,
  `get_metric_baseline`, `get_recent_minute_aggregates` routed to Cloud when
  `cloud_analytics_write_mode = cloud` and the resolved window is Cloud-served.
  The `service_metrics`-related methods follow identically.
- New Cloud read source for metrics: the same pattern as `CloudTelemetrySpanSource`
  but querying the `telemetry_metrics` table on Cloud (see §5).
- Outbox drain for `entity_type = 'metric'` in the shared worker — a target-table
  entry, not a new worker.
- `TelemetryWriteModeService` extended to gate `cloud_analytics_write_mode`
  changes with the same prerequisite chain as the span gate.
- `temps backfill cloud-telemetry` extended so its scope-detection covers
  `cloud_analytics_write_mode` (metrics/service metrics) in addition to the
  existing span switch — no new flag, no new binary entrypoint — implemented as
  the batched read-project-insert loop in §6 over the same transport.
- `apps/temps-cli` parity for the new `cloud_analytics_write_mode` PATCH
  endpoint.
- `TelemetrySourceBadge` wired to the Metrics page in the console.

**Migration:** two additive columns (`entity_type` on the outbox table;
`signal_group` on the intervals table), one column on `projects`
(`cloud_analytics_write_mode`), plus the table rename. All defaulted, all
backward-compatible.

**Security review required before merge.** The consent surface (what leaves the
instance) and the attribute/field allowlist for metrics must be reviewed by
`security-auditor`. Metric label values can contain application-level PII
depending on how the operator's instrumentation is written (e.g., a label called
`user_id` is not impossible). The allowlist model from ADR-040 §1 applies here
too: metrics are sent with only the label keys that appear on the operator's
allowlist, not all of them. The default allowlist for metrics is empty (same
default-deny as spans), and the consent copy must say so.

#### Phase C2 — Analytics events and sessions

**Why second.** Analytics events and sessions are the highest-volume entities and
carry the highest PII risk. However, they have the most complete outbox
infrastructure: `events_ch_outbox` already exists in production, carries the same
claim/deliver/attempts/dead-letter semantics as `cloud_span_outbox`, and its
worker (`ch_fanout.rs`) is the model for the Cloud-primary fan-out. Phase C2 adds
a second sink in that fan-out: instead of (or in addition to) writing to local
ClickHouse, the worker can write to Cloud's ingest endpoint.

The existing `events_ch_outbox` is a reference-style outbox: it carries an
`event_id` FK into the Postgres `events` table (the system of record) rather than
the payload itself. This is correct for the local CH fan-out (Postgres is always
present) but does not directly translate to the Cloud-primary path, where Cloud
must receive the full payload. Two options:

**Option A** (recommended): Add the payload column to `events_ch_outbox`. The
event row is already the system of record in Postgres, so adding a serialized
copy of the fields to be sent to Cloud is duplication — but a small, bounded one,
and it matches the model already proven for spans. The worker reads the full event
from Postgres (one row join, since `event_id` is the PK) and sends the payload.
No new table; no renamed column; the existing delivery cursor becomes a
dual-sink delivery cursor.

**Option B**: A separate `cloud_analytics_event_outbox` table that stores the
payload as sent to Cloud (i.e., only the allowlisted fields, already projected).
This avoids the dual-read join and keeps the payload projection stable regardless
of what Postgres holds, but adds a table and another migration. The shared
`cloud_telemetry_outbox` design in §2 above effectively offers this as `entity_type
= 'analytics_event'` rows — which is the preferred implementation: analytics
events in Phase C2 use the shared outbox rather than the existing `events_ch_outbox`,
so the worker and byte-cap logic are consistent across all entities.

The chosen design is **Option B using the shared outbox**, not Option A. Rationale:
the shared outbox already carries the payload in its final destination shape —
a serialized `ChEventRow` carrying the Cloud projection, not the raw event —
which is exactly what Cloud's insert surface consumes. The existing
`events_ch_outbox` remains unchanged and continues to serve the local ClickHouse
fan-out independently. Cloud-primary analytics events are a separate write path
that converges at Cloud, not a modification of the existing local-CH path.

Note the pleasing consequence of the revision here: `ch_fanout.rs` already
builds a `ChEventRow` and inserts it into the local `events` table. The
Cloud-primary path builds *the same struct with the same projection* and hands
its bytes to the outbox. The two sinks cannot drift in field selection, because
there is only one field selection.

**Consent copy for analytics events.** Because analytics events are PII by
construction (URLs, visitor IDs, referrers), the consent flow for raising
`cloud_analytics_write_mode` to `cloud` must explicitly list what fields are
sent. The field-level allowlist model from ADR-040 §1 applies, with an
analytics-specific default allowlist that includes: event type, path, hostname,
session ID (pseudonymised using the same HMAC technique as trace IDs at Queryable
fidelity — the raw visitor ID never leaves), browser family and OS family (not
full user agent), and referrer hostname (not full referrer). Custom event
properties follow the same exact-match allowlist model as span attributes — empty
by default, operator-editable. This is a named decision for the human owner to
confirm; see Open Questions §1.

**What ships in Phase C2:**

- `cloud_analytics_write_mode` is already on the projects table from Phase C1.
- Analytics event projection to the shared outbox at the existing analytics
  ingest path, gated on `cloud_analytics_write_mode = cloud`, **reusing
  `ChEventRow`** (`temps-analytics-events`) as the Cloud-bound payload. **No
  `AnalyticsEventBatchMessage`, no analytics content-type** — superseded by this
  revision.
- Outbox drain for `entity_type = 'analytics_event'` with `target_table =
  'events'` — one map entry, no new worker and no new outbox type.
- New Cloud read source for analytics: a `CloudAnalyticsEventsSource` implementing
  `AnalyticsEvents`, analogous to `CloudTelemetrySpanSource`, using
  `CloudLink::clickhouse_query_client()` against the Cloud analytics tables.
- `CloudRoutedAnalyticsEvents` routing decorator — the decorator already exists
  as a stub from ADR-040 Phase B (the note in `temps-analytics-events/src/services/routed.rs`
  says "wired but routing every method to local — Phase D"). Unwiring the local-
  only routing and connecting it to the Cloud source is the Phase C2 implementation
  work. No new trait; no new crate.
- `AnalyticsBackend::CloudBackend` health check contributing to the badge.
- `TelemetrySourceBadge` wired to the Analytics pages.
- Backfill: `cloud_analytics_write_mode`'s scope in `temps backfill
  cloud-telemetry` extends to cover analytics events/sessions once C2 ships —
  no new flag, no new command.

> **`analytics_sessions` needs a decision before it can be in this phase, and the
> revision is what surfaced it.** The reuse principle above requires an existing
> local ClickHouse row struct to reuse — and sessions do not have one. The
> ClickHouse `sessions` table was *dropped* in
> `crates/temps-analytics-backend/migrations/clickhouse/0005_drop_sessions.sql`
> because nothing wrote to it and nothing read it: session and visitor analytics
> are served from PostgreSQL, and the ClickHouse read paths derive session-level
> figures from the `events` table (`uniq(session_id)` and friends). So there are
> two honest options, and this ADR takes the first:
>
> 1. **Sessions follow events (chosen).** Cloud stores events; every
>    session-level aggregate the `AnalyticsEvents` trait exposes is computed over
>    `telemetry_analytics_events` by the same query the `ClickHouseEventsBackend`
>    already builds locally. Nothing session-shaped is written, sent or stored
>    separately. This is consistent with what the local ClickHouse path already
>    does today.
> 2. **Reintroduce a local ClickHouse sessions table first**, with a row struct,
>    a write path and a read path — then mirror it. That is a standalone piece of
>    analytics work with its own justification; it is not a sub-task of Cloud
>    enablement, and doing it here would mean inventing a schema on both sides of
>    the Cloud boundary for a table this instance does not itself use.
>
> Consequence: the `telemetry_analytics_sessions` table in §5 is **dropped from
> the contract**. Anything that needs it is a Phase D decision, not a C2
> deliverable.

**Security review required before merge.** The visitor ID pseudonymisation,
the field allowlist, and the consent copy must be reviewed by `security-auditor`.

#### Phase C3 — Proxy logs

**Why last.** Proxy logs are the most operationally complex entity:

- Volume is highest — every HTTP request through the reverse proxy generates a
  log row. At 1k req/s (reachable on a single hosted project under load), that
  is 86 million rows/day. The Cloud ingest endpoint must be able to handle this
  rate, and the outbox must not become the bottleneck on the proxy path.
- The local store is a TimescaleDB hypertable with no Postgres system-of-record
  row — the log row IS the data, same shape as the span outbox (payload-in-row),
  which means the full row must be stored in the outbox before Cloud confirms
  receipt.
- There is no existing storage trait abstraction for proxy logs comparable to
  `OtelStorage` or `AnalyticsEvents`. Adding Cloud-primary writes without a
  trait means the routing logic lives in the concrete handler, which is the
  failure mode ADR-041 §8 already identified for spans (the AI chat stopping
  quietly).

Phase C3 therefore has a named prerequisite not shared by C1 or C2:

> **Introduce a `ProxyLogStorage` trait** over
> `crates/temps-proxy/src/storage/` (where the concrete proxy-log ClickHouse and
> TimescaleDB writers actually live) before adding a Cloud-primary path. The same reason the span decorator is installed at the
> plugin's `register_service` call site rather than in individual handlers
> applies here. Without the trait, every consumer of proxy logs must be found
> and individually updated to check write mode — which is exactly the
> error-prone approach that silently broke traces for HealthComputeService,
> CrossProjectTraceService, TraceReader, and the Observe page in the span case.

Introducing the trait is a refactor of the current concrete implementation and
is estimated to be a standalone PR before the Cloud-primary path is added. It
does not need to ship in the same migration as Phase C3's outbox work.

Volume handling: the proxy log outbox worker must use a higher batch size than
the span worker (suggested starting point: 5,000 rows per claim vs. 500 for
spans) and a shorter max-age flush (suggested: 5 seconds vs. 15 seconds for
spans). These are not final values — they require load testing at the same proxy
path throughput the proxy is already handling. The byte cap per entity type
(§2) is the actual safety valve; batch size and flush cadence tune latency and
throughput within that cap.

**What ships in Phase C3:**

- `ProxyLogStorage` trait (prerequisite, separate PR).
- `ProxyLogStorage` Cloud routing decorator, analogous to `CloudRoutedOtelStorage`.
- Proxy-log projection **reusing `ChProxyLogRow`** (`temps-proxy`) as the
  Cloud-bound payload, with the field allowlist applied in the same projection
  that feeds the local insert. **No `ProxyLogBatchMessage`, no proxy-log
  content-type** — superseded by this revision.
- Outbox drain for `entity_type = 'proxy_log'` with `target_table =
  'proxy_logs'`, using a higher batch size and shorter cadence — tuning
  parameters on the shared worker, not a separate worker.
- New Cloud read source for proxy logs against Cloud's proxy-log table.
- `TelemetrySourceBadge` wired to the Proxy Logs page.
- Backfill: `cloud_analytics_write_mode`'s scope extends to cover proxy logs
  once C3 ships — no new flag, no new command. The `--dry-run` breakdown makes
  this table's cost visible before an operator commits, per §6.

**Security review required before merge.** Proxy logs can contain full request
paths, query parameters, and response codes. The field allowlist must exclude
anything that cannot leave the instance by default. What that means concretely
for the default allowlist is a named decision for the human owner (see Open
Questions §2).

### 4. The "no local ClickHouse needed" end state

Once all three phases ship, an operator who:

1. Links to Temps Cloud
2. Sets `cloud_telemetry_write_mode = cloud` for their projects
3. Sets `cloud_analytics_write_mode = cloud` for their projects

...can decommission local ClickHouse entirely. `ServerConfig::is_clickhouse_enabled()`
returns `false` and the process starts with `TimescaleDbStorage` as the local
fallback for any future project that is not Cloud-primary. Everything on the
"Cloud-primary" projects is served from Cloud.

**What `is_clickhouse_enabled()` becomes.** It does not need to change its
signature or meaning. When it returns `false`, the local ClickHouse storage
constructors (`ClickHouseOtelStorage`, `ClickHouseEventsBackend`) are not built.
The routing decorator wraps the TimescaleDB fallback as `local` rather than a
ClickHouse backend — for Cloud-primary projects no local write happens, and for
local-mode projects writes go to TimescaleDB. The function currently controls
whether any ClickHouse connection is established at startup; that remains true.
No code path forces a ClickHouse client construction when a project is in
Cloud-primary mode, because the routing decorator intercepts the write before
it reaches the storage backend. This is a consequence of where the decorator
is installed (plugin's `register_service` call site, inherited by all consumers)
rather than something that needs a special code path.

**Error tracking, session replay, and other non-CH entities.** These are
deliberately out of scope for "no local ClickHouse needed":

- Error tracking events live in Postgres and TimescaleDB, not ClickHouse. They
  are unaffected when ClickHouse is disabled. They are out of scope for this ADR
  for the same structural reason ADR-040 §5 identified: an error group is a
  mutable, stateful entity with Postgres foreign keys; the immutable/mutable split
  needed to make it Cloud-readable is a separate ADR with a separate prerequisite
  (`ErrorStore` trait).
- Session replay lives in S3/blob storage, not ClickHouse, and is out of scope.
- The monitoring/alerting subsystem uses TimescaleDB for its own series data,
  not ClickHouse. Out of scope.

The "no local ClickHouse needed" goal is fully satisfied by the three phases of
this ADR, for the entities that ClickHouse currently owns.

**What an operator actually removes.** After all three phases:

- The `TEMPS_CLICKHOUSE_*` environment variables can be unset.
- `ServerConfig::is_clickhouse_enabled()` returns `false`.
- No ClickHouse container is needed in docker-compose.
- The four domain ClickHouse clients (`ClickHouseOtelStorage`, `ClickHouseEventsBackend`,
  `ClickHouseBackend`, and the proxy-log CH writer) are simply not constructed.
- All data for Cloud-primary projects flows through the shared outbox to Cloud.
- All reads for Cloud-primary projects flow through the routing decorators to
  Cloud's ClickHouse query proxy (ADR-040 §4).

For projects that remain in `local` write mode, the TimescaleDB fallback serves
reads and accepts writes. This is today's behaviour for all projects on the
default (ClickHouse-disabled) install, and it remains the correct degraded mode.

### 5. Cloud-side API contract (temps-cloud-app)

This section describes what the private Cloud backend must implement for each
phase. It does not specify Cloud-internal architecture; only the interface the
OSS side depends on. The private repo carries its own ADR for implementation.

#### 5a. The ingest surface: a per-tenant-scoped ClickHouse insert interface

ADR-040 §4 already settled this shape for the **read** side. Cloud does not
expose a hand-designed query API; it exposes `POST /v1/telemetry/query`, which
accepts a request in exactly the shape the `clickhouse` Rust crate's HTTP client
already sends, forwards it scoped to the caller's tenant, and returns
ClickHouse's own response unmodified — *"no new query DSL, no per-question route,
and no Cloud release required when the OSS side needs a query shape it didn't
need before."*

The **write** side follows the identical principle, and this revision is what
makes it do so:

> Cloud exposes a per-tenant-scoped ClickHouse **insert** interface. It accepts
> an `INSERT INTO <table> (<columns>) FORMAT <row-format>` statement in exactly
> the shape `clickhouse::Client::insert::<T>()` already produces, with a body
> that is the concatenation of serialized rows the outbox is already storing. It
> scopes the insert to the caller's tenant and forwards it.

What that buys, concretely:

- **No per-entity parsing, validation or reinsertion code on the Cloud side.**
  Cloud is not deserializing a `MetricBatchMessage` into its own metric type and
  re-inserting it row by row. It authenticates, resolves the tenant, checks the
  statement and target table, and forwards a body it never has to interpret.
- **No Cloud release per entity.** Phase C2 and C3 add rows to new tables; they
  do not add message types, content-types, parsers or handlers. The only
  Cloud-side change per phase is the table DDL plus one entry in the
  writable-table allowlist.
- **One transport for live writes and for backfill** (§6), so a backfilled row
  and a live row are indistinguishable on the wire and cannot diverge in
  projection.
- **A new column on an OSS row struct is a DDL change on both sides and nothing
  else** — no protocol version, no content-type negotiation, no dual-parse
  window.

**Auth is unchanged.** The instance's enrollment-derived token, the same
credential `POST /v1/telemetry` and `POST /v1/telemetry/query` already use,
carried the same way. No new credential, no browser-facing token; the OSS
backend holds it and the browser never sees it.

**Where Cloud serves it is Cloud's choice, with one hard constraint.** The OSS
side needs one URL accepting the statement + body pair above. Whether that is
`POST /v1/telemetry/insert`, the existing `POST /v1/telemetry` under a different
content-type, or a sibling of the query proxy is a private-repo decision — except
that it **must not be the same handler as the read proxy**, whose entire security
property is that it injects `readonly=1` into everything it forwards.

**The existing span path does not change in this ADR.** Spans keep shipping via
`POST /v1/telemetry` with the existing `TelemetryBatch`/`SpanRecord` payload.
Re-plumbing a working, load-tested path to prove a point is not a requirement of
this design; new entities use the insert surface from day one, and migrating
spans onto it later is a strictly-subtractive follow-up.

#### 5b. What Cloud must enforce on the insert surface

This is the write-side counterpart of ADR-040 §4's read-only enforcement, and it
is the entire security boundary this design places on the Cloud side — there is
no per-endpoint payload shape constraining what a request can contain, exactly as
there is none on the read side. **`security-auditor` review of this surface is a
hard gate before Phase C1 ships**, on the OSS side for what it sends and on the
private side for what it accepts.

1. **Statement shape.** After comment-stripping, the statement must be
   `INSERT INTO <table> (<columns>) FORMAT <fmt>` with nothing following the
   format token. No `INSERT … SELECT`, no sub-statement, no settings smuggled
   into the statement text, no multi-statement bodies.
2. **Target-table allowlist.** `<table>` must be one of the telemetry tables in
   5c. A tenant's token cannot name a table Cloud did not publish for this
   purpose, and cannot name a system table.
3. **Format allowlist.** `<fmt>` must be one of the row formats the OSS client
   is known to emit (see the schema-coupling note in 5d — the names-and-types
   variant is required, not optional). This is what keeps the body a bounded,
   self-describing parse rather than an arbitrary one.
4. **Tenant scoping is server-side, never caller-supplied.** Same rule as reads:
   the insert is executed under credentials scoped to whichever tenant the
   instance's token resolves to. `project_ref` inside the rows is the *project*
   discriminant **within** a tenant — an HMAC pseudonym computed OSS-side,
   unchanged from ADR-041 — and must never be mistaken for the tenant boundary.
5. **Cost and size bounding.** Maximum body size, maximum rows per request,
   per-tenant insert concurrency, and the metering hooks `POST /v1/telemetry`
   already has. ADR-040 §4's known gap applies verbatim: a per-process
   concurrency limit is not a limit once the data-plane role has replicas.
6. **Idempotency.** The outbox is at-least-once by construction — a batch that is
   delivered but whose ack is lost will be redelivered. The ack semantics the
   span path already relies on must hold here, or at-least-once delivery becomes
   at-least-once *billing* and at-least-once *rows*. Whether that is a
   deduplicating engine on the Cloud tables, a batch idempotency key, or both is
   Cloud's choice; **that it exists is a named requirement of this contract, not
   an assumption.**

#### 5c. Table schemas (contract, not implementation)

These are unchanged from this ADR's original text and remain correct — the
revision changed how rows *get into* them, not what they hold. Each carries
`project_ref` as its leading scoping column, and each must be column-compatible
with the corresponding OSS-side `Ch*Row` struct (5d).

**Phase C1 — OTel metrics and service metrics.** Source row structs: `ChMetricRow`
in `temps-otel` (local table `otel_metrics`) and `ChMetricRow` in `temps-metrics`
(local table `service_metrics`). Fields sent are the allowlisted subset: metric
name, timestamp, value, and label key/value pairs that appear on the operator's
allowlist. No unit or description — those are schema metadata, not per-row data.

```
telemetry_metrics (
    project_ref     String,
    name            LowCardinality(String),
    ts              DateTime64(3),
    value           Float64,
    label_keys      Array(String),
    label_values    Array(String)
)
```

Read query shape: the Cloud-pointing `clickhouse::Client` already queries
`telemetry_spans`; it now also queries `telemetry_metrics` with equivalent
`project_ref` scoping. The read-only proxy accepts any `SELECT` from either
table for the authenticated tenant.

**Phase C2 — Analytics events.** Source row struct: `ChEventRow` in
`temps-analytics-events` — the same struct `ch_fanout.rs` inserts into the local
`events` table.

```
telemetry_analytics_events (
    project_ref      String,
    event_type       LowCardinality(String),
    path             String,
    hostname         String,
    ts               DateTime64(3),
    session_id       String,
    visitor_id       String,
    browser_family   LowCardinality(String),
    os_family        LowCardinality(String),
    referrer_hostname String,
    custom_properties Map(String, String)
)
```

**Critical: `visitor_id` and `session_id` are pseudonymised on the OSS side**
using the same HMAC mechanism as trace IDs at `Queryable` fidelity. The raw
visitor ID and session ID — which may be stable across sessions and linkable to
a specific person — never leave the instance. Cloud stores the pseudonym only.
This must be stated explicitly in the consent copy and confirmed by
`security-auditor`. Under the revised design the pseudonymisation happens where
the `ChEventRow` is built, upstream of the outbox, which is what guarantees the
raw value is never serialized into a durable local queue either.

There is deliberately **no** `telemetry_analytics_sessions` table in this
contract; see the session note in Phase C2 above. Session-level figures are
computed over `telemetry_analytics_events` by the same queries the local
`ClickHouseEventsBackend` already builds.

**Phase C3 — Proxy logs.** Source row struct: `ChProxyLogRow` in `temps-proxy`
(local table `proxy_logs`). Fields sent are the allowlisted subset. By default:
timestamp, method, `path` *without query string* (query strings are too likely to
contain PII or tokens), status code, duration, response bytes, environment. No
headers, no client IP, no user agent on the proxy-log path.

```
telemetry_proxy_logs (
    project_ref   String,
    ts            DateTime64(3),
    method        LowCardinality(String),
    path          String,
    status_code   UInt16,
    duration_ms   Float64,
    response_bytes UInt64,
    environment   LowCardinality(String)
)
```

The `request_id` correlation field (relevant to proxy-log-to-trace linking) is
deliberately excluded from the Cloud projection in Phase C3 unless there is an
explicit trace-linking use case that requires it. Sending a request ID that can
be correlated with a trace ID in Cloud's own tables is a cross-entity
linkability question that needs a separate consent decision.

#### 5d. The one real cost: the row format couples the two schemas

This is the honest downside of reusing ClickHouse's own wire format, and it must
be stated rather than discovered later.

`Client::insert::<T>(table)` emits `INSERT INTO {table}({columns}) FORMAT {fmt}`,
naming the columns explicitly from `T`'s derive, and the body is positionally
serialized against that named list. So the Cloud table's *declaration order* does
not have to match the Rust struct — but the **column names and types do**. A
Cloud table whose `duration_ms` is `Float32` where the OSS struct writes `Float64`
does not fail loudly by default; it writes garbage.

Three requirements follow, and none is optional:

1. **The names-and-types row format is mandatory on this path.** The `clickhouse`
   crate emits `RowBinaryWithNamesAndTypes` — which makes ClickHouse itself
   validate the header against the target table and reject a mismatch — when
   client-side validation is enabled, and the bare positional format otherwise.
   Cloud-bound inserts must always use the validating variant, and Cloud's format
   allowlist (5b.3) must refuse the bare one. A silent-corruption mode that is
   one config flag away is not acceptable on a primary write path.
2. **Cloud's telemetry DDL is a published contract, versioned with the OSS row
   structs.** Adding a column with a default is safe in either order. Renaming,
   retyping or removing one is a breaking change requiring the usual
   Cloud-deploys-first sequencing from ADR-040/041.
3. **The mismatch must be diagnosable by an operator with no support channel.**
   A rejected insert must dead-letter with the ClickHouse error text preserved in
   `last_error` and surfaced next to the queue depth — not retried silently
   forever. "Cloud rejected this row: column X expected Float32, got Float64" is
   a fixable message; a growing queue with no explanation is not.

Note the symmetry: the read side already has this coupling and has accepted it
since ADR-040 §2, because `ClickHouseOtelStorage` builds `SELECT`s naming columns
that must exist on Cloud. The revision does not introduce a new class of
coupling — it extends an existing, deliberate one to the write direction.

### 6. Migration, backfill, and decommissioning UX

**Second simplification (post-review): no `--entity` flag.** The first revision
of this section required `--entity spans|metrics|analytics-events|proxy-logs` on
every invocation, on the theory that per-entity volume and consent differences
justified per-entity operator control. The human owner rejected this during
implementation: the write-mode switches are already grouped (§1) — one flag for
spans, one for everything else — specifically so an operator reaches "cloud
primary" with two decisions, not five. A backfill command that then demands a
*third*, finer-grained decision per entity contradicts that grouping and adds
back the exact per-entity operational burden §1 exists to avoid. Corrected
design:

```
temps backfill cloud-telemetry \
  --project <id> \
  [--from <ts> --to <ts>] \
  [--dry-run]
```

**Scope is derived from the switches, not chosen on the command line.** The
command backfills whatever is currently cloud-primary for the project: spans if
`cloud_telemetry_write_mode = cloud`, and metrics/service metrics/analytics
events/proxy logs together if `cloud_analytics_write_mode = cloud` (whichever
of C1–C3 have shipped). If neither switch is `cloud` yet, the command reports
that and exits — there is nothing to backfill until a switch is flipped. This
means flipping a switch and running backfill are the only two operator actions;
there is no third axis to get wrong.

Per-entity visibility is not lost, only moved from an input flag to output: the
`--dry-run` report and the live progress output break the estimate down by
table (row count, byte estimate, and ETA per table, summed for a total), so an
operator sees exactly what a heavy table like `telemetry_proxy_logs` will cost
before committing — they just can't ask to run it separately from the rest of
its switch's group. An operator who genuinely needs to bound one run's size
(e.g. to schedule the heaviest table's history for an off-peak window) uses
`--from`/`--to` to shrink the time window, the same lever backfill already
offers for every table, rather than a per-entity selector.

**Backfill is the drain loop in bulk-catchup mode. It is not a protocol.** The
original text of this ADR left the wire mechanics unspecified and the
surrounding design implied the declare/upload/complete handshake used by
`crates/temps-cloud/src/backup_mirror.rs`. That pattern is right for backups —
opaque blob files that need S3 credential vending, multi-object snapshots and a
completion sentinel — and wrong for structured telemetry rows, which already have
a native bulk-copy mechanism: read them, serialize them, insert them. This
revision specifies that mechanism instead.

For each table in scope (derived from the switches, as above — the loop runs
once per table but the operator invokes it once per project), the loop is four
steps, each of which reuses something that already exists:

1. **Read a batch of historical rows through the trait the domain already has** —
   `OtelStorage` for metrics, service metrics and trace refs; `AnalyticsEvents`
   for analytics events; `ProxyLogStorage` for proxy logs once C3's prerequisite
   lands. These traits already support bounded, ordered, time-windowed reads,
   because the console's own paginated views need exactly that. No new read path,
   no direct ClickHouse access from backfill code, and — importantly — the read
   works identically whether the local store is ClickHouse or the TimescaleDB
   fallback.
2. **Project each row through the same function the live write path uses**, into
   the same `Ch*Row` struct. Identical allowlist, identical pseudonymisation,
   identical field set. A backfill therefore *cannot* egress a field the live
   path would have stripped, because it is not a second implementation of the
   projection — it is the same one.
3. **Insert the batch over the same Cloud transport as live writes** (§5), with
   the same retry/backoff curve and the same error handling. A backfill batch and
   a drained outbox batch are byte-indistinguishable on the wire; Cloud needs no
   knowledge that a backfill is happening and no separate endpoint, message type
   or state machine for it.
4. **Persist the cursor after each acknowledged batch**, so a retry resumes
   rather than re-ships. This reuses `cloud_telemetry_backfills` and
   `CloudBackfillProgressService` (ADR-042 §6) — not a parallel progress surface.

**Backfill deliberately does not go through the outbox table.** The outbox exists
to keep the *hot path* non-blocking; backfill has no hot path. It is a background
job reading from a store that already holds the data durably, so staging 30M
historical rows through a Postgres queue would inflate the byte cap, contend with
live traffic for the same claim index, and add no durability the source store
does not already provide. ADR-042 Option A reached the same conclusion for spans,
for the same reason.

**`--dry-run` and the ETA need no estimation machinery.** All three numbers fall
out of things the loop already does:

- **Row count** — a `count()` over the same window through the same trait method
  the paginated read path already uses for its total. Exact, one query, no
  sampling.
- **Byte egress** — row count × the serialized size of a sampled projection.
  Because the sampler runs the real projection through the real serializer, the
  sample is not a *model* of the payload; it *is* the payload.
- **ETA** — `remaining_rows / throughput`, where throughput is measured on the
  same insert path live traffic already uses (an EWMA over acknowledged batches).
  Before the first batch acknowledges there is no measured rate, and the output
  says "estimating…" rather than inventing a number — ADR-042 §6's rule, applied
  unchanged. The ETA is rendered coarsely for the same reason it is there: a
  false-precision countdown that jumps is worse than an honest range.

The anomaly guard from ADR-042 §6 carries over unchanged: if shipped bytes exceed
the estimate by more than a bounded factor, pause and surface it rather than
running away with the operator's money.

The recommended migration sequence for an operator with local ClickHouse:

1. Enable `cloud_telemetry_write_mode = cloud` for a test project (spans first,
   verify traces appear in Cloud), then run `temps backfill cloud-telemetry
   --project <id>` to backfill span history.
2. Enable `cloud_analytics_write_mode = cloud` for the same project (verify
   analytics appear in Cloud), then run backfill again — it now also picks up
   metrics, analytics events, and proxy logs (whichever have shipped), reusing
   the same cursor-per-table bookkeeping so the already-backfilled spans are
   not re-shipped. `--dry-run` first shows the per-table breakdown, including
   the cost of the heaviest table (proxy logs), before committing.
3. Repeat for remaining projects.
4. When all projects are Cloud-primary, set `TEMPS_CLICKHOUSE_*` env vars to
   empty and restart. `ServerConfig::is_clickhouse_enabled()` returns `false`;
   the process starts with TimescaleDB fallbacks only.
5. Decommission the ClickHouse container.

Step 4 and 5 have no automated tooling — they are operator actions, and making
them irreversible automatically would be the wrong trade. The UI should surface
a "ClickHouse is configured but all projects are Cloud-primary — you may safely
decommission it" advisory banner when that state is reached, linking to the
documentation for step 4-5. It must never perform the decommission on the
operator's behalf.

**Operators who add new projects after decommissioning.** A new project in `local`
write mode on an instance with no ClickHouse configured will use TimescaleDB for
metrics, analytics events (already the default for the majority of installs), and
proxy logs. The same graceful degradation that exists today for the ClickHouse-
disabled default install continues to apply.

---

## Consequences

### Positive

- The "no local ClickHouse needed" goal becomes fully achievable in three phases,
  with a clear dependency graph.
- Each phase delivers a usable, independently testable capability rather than
  requiring all three to be present before anything works.
- Outbox generalization eliminates six near-identical tables, six sets of partial
  indexes, and six independent retry/dead-letter implementations.
- The routing decorator pattern — install at the plugin's `register_service` call
  site, not in individual handlers — prevents the silent-feature-removal failure
  mode already identified for spans.
- Analytics, metrics, and proxy-log reads from Cloud use the same
  `clickhouse::Client` reuse approach established for spans: zero new query DSL,
  zero additional Cloud releases needed when the OSS side adds a new query shape.
- **Writes now have that same property.** Reusing ClickHouse's row format means
  zero new wire format, zero per-entity Cloud-side parsers, and no Cloud release
  when an entity gains a field — the change is DDL on both sides and nothing
  else. ADR-040 §2's "zero new query DSL" becomes "zero new anything" across the
  whole Cloud boundary.
- **A backfilled row cannot differ from a live row.** Both are produced by the
  same projection function, serialized by the same struct, and inserted over the
  same transport. There is no second field list, so there is no consent surface
  that applies to one path and not the other.
- **The outbox stops growing with the entity count.** Six entities add six map
  entries (`entity_type × target_table → columns`), not six accessors, six
  workers, six message types and six sets of retry/dead-letter code.

### Negative

- The shared outbox table (`cloud_telemetry_outbox`) becomes a multi-entity queue.
  Operational visibility — "how many metrics are pending vs. how many spans" —
  requires the `entity_type` column in monitoring queries, which is one additional
  filter that the single-entity design did not need.
- A single `cloud_analytics_write_mode` switch covers entities with very different
  PII profiles. An operator who wants Cloud-primary metrics but not Cloud-primary
  analytics events cannot express that distinction until a future ADR adds the
  split.
- Phase C3's prerequisite (`ProxyLogStorage` trait) is a pre-existing technical
  debt item; its existence as a gating dependency means Phase C3 cannot begin
  immediately after Phase C2 ships unless work on the trait has been started in
  parallel.
- Three phases means three rounds of security review — though after the revision
  only **one** round of Cloud-side transport work, since C2 and C3 add table DDL
  and an allowlist entry rather than a new ingest path each.
- **Cloud's telemetry tables become a published, column-compatible contract with
  OSS-side row structs** (§5d). Adding a column with a default is safe in either
  order; renaming or retyping one is a breaking change that must follow the
  Cloud-deploys-first sequencing. The bespoke-JSON design would have tolerated
  more skew here; this is the price of not duplicating the wire format, and it is
  paid once rather than per entity.
- **The insert surface is a genuinely new Cloud-side security boundary.** The
  read proxy's safety rests on unconditionally injecting `readonly=1`; the write
  surface cannot use that mechanism and needs its own statement-shape,
  target-table and format enforcement (§5b). It must not share a handler with the
  read proxy.

### Risks

- **Volume at C3.** Proxy logs at scale (>1k req/s) produce outbox insertion rates
  that have not been load-tested. If the Postgres outbox table becomes the
  bottleneck on the proxy path, the proxy's request handling will stall. The
  mitigation is to move proxy-log outbox inserts to a bounded in-memory channel
  with a background flusher — the same design as the span spool, but with
  persistence on the background flush side. This must be decided before Phase C3
  ships. If the proxy path cannot tolerate synchronous Postgres inserts at proxy
  log volume, Phase C3's design changes from "payload-in-row" to
  "in-memory buffer, async Postgres drain" — a meaningful architectural difference
  that requires its own analysis. This is flagged as a hard open question in §7.
- **Positional row format plus independent schema evolution.** ClickHouse's row
  format is positional against the column list named in the `INSERT`. If the bare
  (non-validating) variant is ever used Cloud-bound and the Cloud table's types
  drift from the OSS struct, the insert *succeeds* and writes garbage — a silent
  corruption on a primary write path. Mitigation, and it is mandatory rather than
  advisory: always emit the names-and-types variant so ClickHouse validates the
  header server-side, make Cloud's format allowlist refuse the bare one, and
  dead-letter rejected batches with the ClickHouse error text preserved and
  surfaced. See §5d.
- **`analytics_sessions` has no local row struct to reuse.** The ClickHouse
  `sessions` table was dropped as dead schema (`0005_drop_sessions.sql`) and
  session analytics is served from PostgreSQL. Phase C2 therefore covers events
  and derives session-level figures from them, exactly as the local ClickHouse
  read path already does. If a first-class session table is wanted in Cloud, it
  needs a local one first — that is analytics work with its own justification,
  not a sub-task of Cloud enablement.
- **Pseudonymisation key stability.** The analytics visitor_id and session_id
  pseudonyms use the same HMAC key as span trace IDs (derived from the instance's
  enrollment token). ADR-040 Open Question 3 already identified that re-enrollment
  orphans all previously mirrored data by changing the HMAC key. For analytics,
  this is worse: a re-enrollment does not just orphan old traces; it changes the
  pseudonym for every visitor, making it impossible to count returning visitors
  across the re-enrollment boundary. The recommended mitigation (a separate,
  enrollment-stable secret for analytics visitor pseudonyms) should be decided
  before Phase C2 ships, not deferred to a follow-up.
- **Quota exhaustion fallback for analytics events.** ADR-041 §7b defines how
  the span path falls back to local writes when Cloud is quota-exhausted. The
  same mechanism must apply to analytics events and metrics — but analytics
  events are the highest-volume entity, and a quota-exhaustion fallback that
  writes them locally requires local ClickHouse (or an acceptable Postgres
  fallback for the analytics read path). If Cloud's analytics ingest quota is
  exhausted and the operator has decommissioned local ClickHouse, the correct
  behaviour is to **drop** analytics events with a visible counter (not to stall
  the proxy path), because analytics events are a derived insight, not a
  system-of-record write. This must be stated explicitly in the consent copy and
  in the fallback handling code. It is a different safety trade than the span
  case, where dropping spans on quota exhaustion is architecturally forbidden.

---

## Alternatives Considered

### Option A: One switch per entity per project

Five columns on `projects`, five gate paths, five consent surfaces, five
backfill command variants per project. Full granularity; maximum operator
ergonomics complexity. For an operator with 40 projects and five entities, this
is 200 API calls to reach the "no local ClickHouse" state. The product goal is to
reduce operational burden; this option increases it.

Rejected in favour of the two-switch (spans / analytics-group) design.

### Option B: One unified switch for spans and all non-span entities

One column: `cloud_telemetry_write_mode` covers everything. Simplest UI and
simplest gate logic. But it conflates two independent migration timelines —
spans are ready now, analytics events are not — and would force operators to
wait for Phase C3 before any Cloud-primary write mode is available. Also breaks
the incremental cut-over story for operators who want to migrate one entity at
a time.

Rejected in favour of the two-switch design.

### Option C: Six separate outbox tables, one per entity

Maximum isolation; each entity's worker, retry, and byte-cap logic is entirely
independent. Operationally simple to debug per-entity. Requires six migrations,
six sets of partial indexes, six sets of dead-letter retention sweeps, and six
background tasks.

Rejected in favour of the shared table with `entity_type` discriminant. The
shared design achieves the same isolation at the query level (each worker claims
with `WHERE entity_type = 'X'`) while removing the maintenance burden of six
near-identical tables.

### Option D: One generic outbox over `(entity_type, target_table, row_bytes)` — **CHOSEN (verdict reversed on revision)**

> **Verdict changed.** This entry was `Rejected` in the original text of this ADR
> and is now the chosen design. The paragraphs below record the original
> reasoning and why it was wrong, so the flip is auditable rather than silent.

**What was originally rejected.** A generic `CloudTelemetryOutbox<T>`,
parameterized over the payload type and its serializer/content-type, replacing
`SpanOutbox`/`MetricOutbox`/`AnalyticsEventOutbox`.

**The original rejection reasoning:** *"the entity-specific typed accessors carry
entity-specific logic: spans have a separate `project_id` field that metrics
might not; analytics events need visitor ID pseudonymisation at projection time.
A generic type that handles all of those cases becomes as complex as the sum of
its specializations."*

**Why that reasoning was wrong.** It conflated two different layers and then
concluded that the properties of the lower one applied to the upper one:

- **Projection logic** — building the row: which fields are included, how visitor
  IDs are pseudonymised, which metric labels pass the allowlist, how a query
  string is stripped from a path. This genuinely *is* entity-specific, it
  genuinely *does* belong in the owning domain crate with that crate's own error
  enum, and the revised design does not move it an inch. It stays exactly where
  it already is: in the function that builds the domain's `Ch*Row` for its local
  ClickHouse insert. Cloud-primary hooks in *there*.
- **Outbox mechanics** — claim, deliver, ack, retry, dead-letter, byte-cap,
  gap-window. Once a row has been projected and serialized, every one of these
  operates on opaque bytes plus a destination. There is nothing entity-specific
  left to specialize, because the entity-specific work already happened upstream
  and is finished.

The original text drew the boundary *above both layers* and concluded the whole
stack had to be per-entity. Drawing it *between* them makes the generic case
trivial rather than complex — and, notably, makes the type parameter disappear
entirely. The chosen design is not `CloudTelemetryOutbox<T>` generic over a
payload; it is **one concrete type** over `(entity_type, target_table,
row_bytes)`, because by the time the outbox sees the row, the payload's type has
already done its whole job.

**The empirical check.** `SpanOutbox` and `MetricOutbox`, as they exist in the
branch today, differ only in a string literal in their `WHERE` clause — the same
claim SQL, the same delivery bookkeeping, the same retry curve, the same
dead-letter sweep. The original design implied four more of those. The
"specializations" it wanted to name clearly turned out to have nothing to
specialize.

**Accepted.** Per-entity typed accessors are superseded; see §2b.

### Option E: A bespoke per-entity JSON wire protocol (the original design)

`MetricBatchMessage`, `AnalyticsEventBatchMessage`, `ProxyLogBatchMessage` in
`temps-cloud-protocol`, each with its own content-type on `POST /v1/telemetry`,
each parsed and reinserted into ClickHouse by hand-written Cloud-side code.

This is what the original text of this ADR specified, and the first
implementation step shipped part of it (`MetricBatchMessage`, `MetricRow`,
`METRIC_BATCH_CONTENT_TYPE`). **Rejected on revision.** It re-expresses, in JSON,
a row shape that both sides already describe: every entity in scope already has a
`#[derive(clickhouse::Row)]` struct because every one of them already inserts
into a local ClickHouse table, and ClickHouse's own row format is what those
structs serialize to for free. The cost of the bespoke protocol is paid six
times over — a message type, a content-type, a Cloud-side parser, a Cloud-side
reinsertion path, a Cloud release, and a second field list that can silently
drift from the local one — in exchange for nothing the row format does not
already do.

It is also inconsistent with the decision this ADR is built on. ADR-040 §2
rejected a bespoke `RemoteTelemetryQuery` trait for reads on precisely this
ground: *"it duplicates query-building logic that already exists."* Option E is
the same mistake pointed the other way down the pipe. Symmetry is not the
argument — the argument is that the duplication is real either way.

The one thing Option E has going for it is decoupling: a JSON message shape can
tolerate schema skew that a positional row format cannot. That is a real
property, and §5d is where the revised design pays for giving it up (mandatory
names-and-types validation, published Cloud DDL, diagnosable dead-letters). It
is a bounded, one-time cost against a per-entity recurring one.

### Option F: A declare/upload/complete handshake for backfill

Model backfill on `crates/temps-cloud/src/backup_mirror.rs`: declare the objects
to be sent, obtain vended credentials, upload, post a completion sentinel.

**Rejected.** That handshake exists because backups are opaque blobs written to
object storage with credentials Cloud must vend per-object, where a partially
uploaded multi-object snapshot must not be treated as complete. None of those
properties hold for telemetry rows. Rows are structured, individually meaningful,
idempotently insertable, and already have a bulk-copy mechanism — the same
`INSERT` the live path uses. A backfill that reuses the live transport gets
retry, backoff, tenant scoping, metering and error surfacing for free, and gets
the guarantee that a backfilled row cannot differ in projection from a live one.
A separate handshake would have to re-earn all of that and could not offer the
last guarantee at all. See §6.

---

## Open Questions (for the human owner)

### 1. Analytics event field allowlist defaults

The consent copy for `cloud_analytics_write_mode = cloud` must list exactly what
analytics fields leave the instance. The proposed default allowlist (event type,
path, hostname, pseudonymised session/visitor ID, browser family, OS family,
referrer hostname, custom properties on the operator's allowlist) is an
architectural proposal, not a confirmed product decision. The human owner must
confirm:

- Which fields are in the default allowlist (currently proposed above)?
- Should `path` include query strings, or be stripped to the path component only?
  (The proposal strips query strings, which is the conservative default; query
  strings routinely contain tokens and PII.)
- What is the pricing/quota model for analytics event storage in Cloud? Analytics
  events are the highest-volume entity and the cost implications for Temps Cloud
  are the largest of any entity in this ADR. The open metering gap noted in
  project memory (AI credits unmetered, cloud funnel backup gap) is a precedent
  for this pattern, but analytics events at full volume represent a meaningfully
  different cost surface.

### 2. Proxy log field allowlist defaults

The consent copy for proxy logs must confirm whether the proposed default
(path without query string, method, status code, duration, response bytes,
environment) is correct, or whether more fields (client IP, user agent, request
ID) should be optionally includable on an explicit opt-in basis per field.

### 3. Analytics event pricing/quota at Cloud scale

Related to Open Question 1 but distinct: an operator with 1M daily pageviews,
all Cloud-primary, is asking Temps Cloud to store ~30M analytics event rows per
month for a single instance. This is not equivalent to the span load the Cloud
ingest endpoint is already sized for. The human owner must decide whether
analytics events at Cloud-primary scale need a separate quota tier, separate
pricing, or a volume cap on the Cloud side before Phase C2 ships to production.
This is a business decision, not a technical one, and it blocks Phase C2 going
to general availability.

### 4. Proxy log at-scale write path (the C3 architectural decision)

If synchronous Postgres outbox inserts on the proxy path are not viable at >1k
req/s, Phase C3 needs an in-memory buffer with a background flush path (same
shape as the span spool, but persisting to Postgres rather than discarding on
overflow). The decision point is a load test of the proxy path with synchronous
Postgres inserts at proxy log volume. The human owner should decide whether that
load test should block Phase C2 (run it before C2 ships, to know what C3 needs
before committing to the shared outbox design) or whether it should be deferred
to Phase C3's own planning. Running it before C2 ships is the recommended option
because it might require a design change to the shared outbox (the in-memory
buffer approach would make proxy logs a special case in the shared table, not
a uniform entry).

### 5. Visitor/session pseudonymisation key stability

The HMAC key for visitor and session pseudonyms: does it derive from the
enrollment token (simpler, already the pattern, but orphans analytics history
on re-enrollment) or from a separate stable secret stored encrypted on the
`settings` row (more complex, preserves cross-enrollment visitor continuity)?
For analytics, visitor continuity is a product feature (returning visitor counts,
cohort analysis); breaking it on re-enrollment is a more visible failure than
trace ID orphaning. The human owner must decide which trade is acceptable before
Phase C2 ships.

---

## Implementation Notes

### Affected crates

The revision materially shrinks this list. What follows is the surface *after*
simplification; where the original text named something that is no longer
needed, it is called out as superseded so a reader comparing against an
in-progress branch is not confused.

**Phase C1:**
- `temps-migrations` — **already shipped:** `m20260903_000001` (rename
  `cloud_span_outbox` → `cloud_telemetry_outbox`, add `entity_type` + CHECK,
  reshape the two claim indexes), `m20260903_000002` (`signal_group` on
  `project_telemetry_write_intervals`), `m20260903_000003`
  (`cloud_analytics_write_mode` on `projects`). **Still required:** one additive
  follow-up migration adding `target_table TEXT NULL` and a binary payload
  column (`payload_row BYTEA NULL`) to `cloud_telemetry_outbox` (§2c). Additive
  and reversible; nothing shipped is dropped or rewritten.
- `temps-entities` — the shipped `cloud_telemetry_outbox` entity plus the two new
  columns on its `Model`/`ActiveModel`. `CloudTelemetryOutboxEntityType` keeps
  its four variants (§2c.1).
- `temps-cloud-client` — **one** generic outbox accessor over `(entity_type,
  target_table, row_bytes)`, replacing `SpanOutbox` **and** `MetricOutbox`
  (net deletion). One drain loop that groups a claimed batch by
  `(entity_type, target_table)`. Plus the **Cloud insert transport** on
  `CloudLink` — the write-side sibling of the existing
  `clickhouse_query_client()`, built from the same credential.
- `temps-cloud-protocol` — **no per-entity message types.** `MetricBatchMessage`,
  `MetricRow` and `METRIC_BATCH_CONTENT_TYPE`, added during the foundational
  step, are superseded by this revision and should be removed. What legitimately
  belongs here after the revision is only what both sides must agree on that is
  *not* already expressed by a ClickHouse row: the `entity_type` ↔ target-table
  mapping / writable-table allowlist constants, and whatever minimal envelope the
  insert request needs beyond the statement + body pair (§5a) — plausibly
  nothing. `SpanRecord`/`TelemetryBatch`/`IngestAck` stay untouched; the existing
  span path is unchanged by this ADR.
- `temps-otel` — extend `CloudRoutedOtelStorage` to route metric methods to Cloud
  when the analytics signal group resolves Cloud; add `CloudTelemetryMetricSource`
  (analogous to `CloudTelemetrySpanSource`); extend `TelemetryWriteModeService` to
  gate and record `cloud_analytics_write_mode`; and add the Cloud sink at the
  point where `ChMetricRow` is already built for the local insert.
- `temps-metrics` — the same one-line sink at the point where its own
  `ChMetricRow` is built for `service_metrics`. (The original list omitted this
  crate; the reuse principle is what makes it visible.)
- `temps-cli` (Rust binary) — extend `temps backfill cloud-telemetry` with the
  `--entity` flag, implemented as the read-project-insert loop in §6.
- `apps/temps-cli` — parity for the new `cloud_analytics_write_mode` PATCH
  endpoint and the `--entity` flag.

**Phase C2** (in addition to C1 crates):
- `temps-analytics-events` — `CloudAnalyticsEventsSource`; a Cloud sink at the
  point where `ChEventRow` is already built (`ch_fanout.rs`'s row projection);
  wire the existing routing decorator stub to the real Cloud source.
- `temps-cloud-protocol` — **nothing.** `AnalyticsEventBatchMessage` and
  `AnalyticsSessionBatchMessage` are superseded; the row struct is the payload.
- `web/` — wire badge on Analytics pages.

**Phase C3** (in addition to C1/C2 crates):
- `temps-proxy` — `ProxyLogStorage` trait (prerequisite PR); Cloud routing
  decorator; a Cloud sink at the point where `ChProxyLogRow` is already built.
  (The original list named a `temps-proxy-logs` crate; the proxy-log ClickHouse
  writer actually lives in `temps-proxy/src/storage/`.)
- `temps-cloud-protocol` — **nothing.** `ProxyLogBatchMessage` is superseded.
- `web/` — wire badge on Proxy Logs page.

**Crates the revision removes work from:** `temps-cloud-protocol` (three message
families and their content-types), and `temps-cloud-client` (five would-be
per-entity accessors collapsing into one). The private `temps-cloud-app` repo
loses six per-entity ingest handlers and gains one insert surface.

### Migration plan

Phase C1's foundational migration is additive but includes a table rename. The
rename is reversible (the `down()` function renames back). All columns are
defaulted. Existing span outbox rows survive: `entity_type` defaults to `span`
for rows that predate the migration. The partial index on pending rows is
reconstructed with the new `entity_type` leading column — `CONCURRENTLY` is
available in Postgres and avoids locking the table during index creation, but
Sea-ORM migrations run inside a transaction and `CONCURRENTLY` is not permitted
there. The existing partial index is small enough on a self-hosted instance to
rebuild without concurrency; this is noted in the migration comment. **This has
shipped as written and needs no rework.**

**What the revision adds, and what it does not.** It does **not** revert or
reshape anything above. The index shapes in particular remain correct: the claim
scan is still `WHERE entity_type = ? AND state = 'pending'` ordered by
`(enqueued_at, id)`, and grouping a claimed batch by `target_table` happens in
memory after the claim, so no index needs to know about `target_table`. The
settled-row retention index stays entity-agnostic for the same reason as before.

One additive follow-up migration is required (§2c.2):

```sql
ALTER TABLE cloud_telemetry_outbox ADD COLUMN target_table TEXT;      -- NULL = span path
ALTER TABLE cloud_telemetry_outbox ADD COLUMN payload_row BYTEA;      -- NULL = JSON payload path
```

Both nullable, both defaulted to NULL, both reversible with a plain `DROP
COLUMN`. Existing span rows keep using `payload TEXT` and are unaffected; every
new entity writes `payload_row` and leaves `payload` NULL. `payload_bytes` keeps
its meaning — it is the byte length of whichever payload column the row uses, and
the byte cap keeps working unchanged. A binary column rather than base64-in-TEXT
is not a preference: base64 would inflate the highest-volume queue by a third and
charge that inflation against the operator-visible cap.

### Breaking changes

None to the public API. The shared outbox is an internal Postgres table, and the
follow-up migration above is additive and defaulted.

On the OSS/Cloud protocol: the revision **removes** protocol surface rather than
adding it. `MetricBatchMessage`/`MetricRow`/`METRIC_BATCH_CONTENT_TYPE` were
added on this unreleased branch and are deleted before release, so no shipped
instance ever sent them and no Cloud gateway ever has to keep accepting them. The
existing `POST /v1/telemetry` span path is untouched.

The sequencing rule from ADR-040/041 still holds, and now applies to DDL rather
than to message types: **Cloud must publish a telemetry table and add it to the
writable-table allowlist before any OSS instance inserts into it.** An insert to
an unpublished table is refused by the allowlist (5b.2), which is a loud,
dead-letterable error rather than a silent drop. The same rule governs column
additions: a column must exist on the Cloud table before an OSS row struct starts
writing it.

### Security review requirements

**The insert surface itself (§5b) requires review before Phase C1 ships**, on
both sides of the boundary — what the OSS client sends and what Cloud accepts. It
is a new write path into a multi-tenant store and its enforcement is not
inherited from the read proxy's `readonly=1` mechanism.

Phase C1: metric label allowlist, `queryable` fidelity scope, consent copy.

Phase C2: visitor/session pseudonymisation, analytics field allowlist, custom
property allowlist mechanics, consent copy. **This phase requires the most
careful review** — it is the first time user-attributable data (visitor sessions)
can leave a self-hosted instance. The `security-auditor` sign-off is a hard gate
before Phase C2 merges.

Phase C3: proxy log field allowlist, specifically whether query strings are
always stripped or optionally includable, and whether client IP is ever sent.

---

## References

- `crates/temps-otel/src/storage/cloud_routed.rs` — `CloudRoutedOtelStorage`, the
  read routing decorator whose `store_metrics`, `query_metrics`, etc. methods are
  extended in Phase C1
- `crates/temps-otel/src/storage/cloud_spans.rs` — `CloudTelemetrySpanSource`,
  the template for `CloudTelemetryMetricSource` and `CloudAnalyticsEventsSource`
- `crates/temps-cloud-client/src/outbox.rs` — `SpanOutbox`/`MetricOutbox`, the
  two per-entity accessors that §2b collapses into one generic accessor
- `crates/temps-cloud-client/src/query.rs` — `CloudLink::clickhouse_query_client()`,
  the read-side transport whose write-side sibling §5a specifies
- `crates/temps-migrations/src/migration/m20260901_000004_create_cloud_span_outbox.rs`
  — the outbox schema being generalised
- `crates/temps-migrations/src/migration/m20260903_000001_generalize_cloud_telemetry_outbox.rs`
  — the shipped generalisation (rename, `entity_type`, reshaped claim indexes)
- **The local-ClickHouse row structs this ADR reuses as the Cloud-bound payload,
  rather than defining new message types for:**
  - `crates/temps-otel/src/storage/clickhouse/mod.rs` — `ChSpanRow` (`otel_spans`),
    `ChMetricRow` (`otel_metrics`), `ChTraceRefRow` (`cross_project_trace_refs`)
  - `crates/temps-metrics/src/store/clickhouse.rs` — `ChMetricRow`
    (`service_metrics`)
  - `crates/temps-analytics-events/src/services/ch_fanout.rs` — `ChEventRow`
    (`events`), built by the existing local fan-out projection
  - `crates/temps-proxy/src/storage/clickhouse.rs` — `ChProxyLogRow`
    (`proxy_logs`)
- `crates/temps-analytics-backend/migrations/clickhouse/0005_drop_sessions.sql`
  — why `analytics_sessions` has no row struct to reuse, and why Phase C2 derives
  session figures from events instead
- `crates/temps-cloud/src/backup_mirror.rs` — the declare/upload/complete
  handshake that Option F declines to copy for telemetry rows
- `crates/temps-migrations/src/migration/m20260505_000001_create_events_ch_outbox.rs`
  — the existing analytics events outbox, which Phase C2 adds a Cloud sink to
- `crates/temps-analytics-events/src/services/traits.rs` — `AnalyticsEvents` trait,
  whose Cloud implementation (`CloudAnalyticsEventsSource`) is Phase C2's read path
- `crates/temps-otel/src/services/telemetry_write_mode.rs` — `TelemetryWriteModeService`,
  extended in Phase C1 to gate `cloud_analytics_write_mode`
- `crates/temps-otel/src/services/cloud_primary_fallback.rs` — `OutboxSpiller`,
  whose fallback-to-local behaviour is extended to cover non-span entities
- ADR-040 §1 — fidelity tiers and attribute allowlist model, extended here to
  analytics events and metrics
- ADR-040 §2 — storage-reuse approach (same `clickhouse::Client` impl, pointed at
  Cloud), reused identically for metrics and analytics
- ADR-040 §3 — no-silent-fallback contract, which applies to all three phases
- ADR-040 §4 — Cloud-side read proxy contract, extended to `telemetry_metrics`,
  `telemetry_analytics_events`, and `telemetry_proxy_logs` tables
- ADR-041 §0 — scope that this ADR expands
- ADR-041 §3 — outbox design (claim/deliver/dead-letter/byte-cap), whose
  principles are generalised in this ADR's §2
- ADR-041 §7b — quota-exhaustion fallback, extended to the non-span entities with
  the drop-not-stall trade for analytics events
- ADR-042 — in-process backfill engine whose `--entity` flag extension is Phase C1
