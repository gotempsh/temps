<!-- SCOPE: Replaces the chunk-scan engine behind Global Logs with an ordered, indexed log-line store served through one backend trait (TimescaleDB by default, ClickHouse when configured). -->

# ADR-045: Global Logs — an indexed log store, not a chunk scan

**Status:** Proposed — requires `security-auditor` sign-off before implementation (see [Authorization](#7-authorization-must-become-an-allow-list-security-sensitive)).
**Date:** 2026-09-16
**Author:** David Viejo
**Builds on:** ADR-012 (ClickHouse as an external analytics backend), ADR-016 (ClickHouse as the OTel telemetry backend), ADR-021 (Multi-node container log aggregation)
**Supersedes the query design of:** ADR-021 "Phase B" (the `log_chunks` + object-storage search substrate). ADR-021's *ingest* invariants — bounded queues, drop-before-block, shed logs never traffic — are retained unchanged.

> **Numbering note.** ADR-044 (`044-cloud-managed-backup-default-schedule.md`) is the
> most recent committed ADR; 045 is the next free number in the committed sequence.

---

## Context

Global Logs (`POST /api/logs/global/search`) is the one place in Temps where a
user asks "show me what my system said". It is the feature an operator reaches
for at 3am, and it is the weakest query surface in the product. The complaint
that triggered this ADR — "no pagination, no nothing" — is not a UI gap. It is
the visible symptom of a storage design that has no index and therefore cannot
offer pagination at all.

### What exists today

Log lines are **not** in any queryable store. The collector
(`crates/temps-log-aggregator/src/services/collector.rs`, and
`remote_collector.rs` for worker nodes) feeds `ChunkWriterService`, which
buffers per container and flushes at 1 MB uncompressed or every 30 s into a
`.ndjson.zst` object on S3 or the local filesystem. Only *metadata* about each
object lands in Postgres, in `log_chunks` (`crates/temps-entities/src/log_chunks.rs`,
migration `m20260225_000001_create_log_aggregator_tables.rs`): id, project_id,
external_service_id, env, service, container_id, deploy_id, node_id, started_at,
ended_at, storage_key, line_count, compressed_size_bytes, has_errors, and
`line_offsets` (the byte offset of every 100th line).

Search (`crates/temps-log-aggregator/src/services/global_search.rs`, 695 lines)
is a two-phase scan:

1. Select candidate chunks from `log_chunks` ordered `(ended_at DESC, id DESC)`
   in batches of `CHUNK_BATCH = 32`, applying the filters that happen to be
   columns — authorization, project, external service, scope, env, node, deploy.
2. For every candidate: download the object, zstd-decompress it on a blocking
   task, split on `\n`, `serde_json`-parse each line, and apply **every
   remaining filter** — time window, level, env, node, deploy, and free-text
   `message.to_lowercase().contains(text)` — as an unindexed linear scan.
   Survivors go into a `BTreeMap<(timestamp, chunk_id, line_offset), line>`
   trimmed to `page_size` (the frontend hardcodes 100).

There is no index over line content, level, or time *within* a chunk. Every
query is O(all bytes in the window), and the only thing standing between a
user and an unbounded scan is a wall of hardcoded budgets:

| Constant | Value | Effect when hit |
|---|---|---|
| `CHUNK_BATCH` | 32 | metadata page size |
| `MAX_CHUNKS` | 512 | scan aborts |
| `MAX_COMPRESSED_BYTES` | 64 MB | scan aborts |
| `MAX_CHUNK_BYTES` | 8 MB | **chunk skipped entirely — silent data loss** |
| `MAX_DECOMPRESSED_BYTES` | 16 MB / chunk | scan aborts |
| per-line cap | 64 KB | scan aborts |
| request timeout (`handlers/global.rs`) | 30 s | HTTP 408 |
| `SEARCH_SLOTS` semaphore | **2** | 3rd concurrent search anywhere in the cluster gets HTTP 429 |

The project-scoped search (`services/search.rs`, 2,685 lines) has the same
shape with its own caps: `MAX_FULLTEXT_HOURS = 24` (free-text search is simply
refused beyond a day) and `MAX_CONCURRENT_FETCHES = 20`.

### Why this cannot be patched

**1. "Pagination" is not pagination.** The `next_cursor` is a resume point for
an exhaustive scan, not a keyset over an ordered index. When the budget runs
out the response sets `scan_limit_reached: true`, and the code comment is
explicit that "partial results cannot be paginated safely". The user is shown
`n` lines, told some may be missing, and given a disabled **Next page** button.
There is no route to the older matching lines except narrowing the query until
it fits under 64 MB.

**2. Correctness, not just speed.** `declared > MAX_CHUNK_BYTES` *skips* the
chunk and breaks the scan. A single busy container that produced a 9 MB
compressed chunk silently removes that window from every search that crosses
it. The API reports "partial", never "I refused to read the exact chunk you
were looking for".

**3. The concurrency cap is structural.** `Semaphore::const_new(2)` is not a
tuning mistake — it is the honest consequence of a design where one HTTP
request may pull 64 MB from object storage and zstd-decompress up to 512
chunks on the control plane. Three engineers looking at an incident together
is a 429. Raising the number just moves the failure from the third user to the
control plane's CPU and the proxy it shares a box with. This cap is inherent to
chunk scanning and disappears only if the query stops being a scan.

**4. No facets are possible.** The filter autocomplete in
`web/src/components/observability/LogQueryInput.tsx` derives its env / node /
deploy options from `new Set(lines.map(...))` over the **100 lines currently on
screen**. A value that is not already visible cannot be discovered, so the
filters only help you narrow what you already found. A real distinct-values
query is impossible against a store with no index — you would have to scan
everything to answer "what environments exist?".

**5. Every other high-volume signal in this codebase already solved this.**
Proxy logs (one row per HTTP request — comparable volume class) run through
`ProxyLogStorage` with a TimescaleDB implementation by default and a ClickHouse
implementation when `ServerConfig::is_clickhouse_enabled()`. OTel spans and
metrics do the same via `OtelStorage` (ADR-016). Analytics events do the same
(ADR-012). Container stdout/stderr is the **only** high-volume signal still on
a bespoke scan engine, and it is the one users complain about.

### Constraints this ADR must respect

- **Single-binary self-host is the product.** Temps competes with Coolify and
  Dokploy partly on "one binary, one Postgres". ClickHouse is optional and off
  by default everywhere in this codebase (ADR-012 §4 is explicit: "Postgres
  alone remains a complete product"). **A design that fixes Global Logs only
  for ClickHouse operators is not acceptable** — the default install is where
  the complaint comes from.
- **Ingest must stay lossy-tolerant and never block.** ADR-021's load-shedding
  priority is law: route traffic > app stdout > ship logs > index logs.
- **Volume is real.** This is container stdout/stderr from many tenants, the
  same class of problem that produced 160 GB/day of `otel_spans` in production.
- **Live product.** `log_chunks` holds an unknown but nontrivial amount of data
  under a 30-day default retention, and it must not be destroyed on cutover.

---

## Decision

**Log lines become rows in an ordered, indexed, time-partitioned store, reached
through a single `LogLineStore` trait with two implementations — TimescaleDB by
default, ClickHouse when the operator has configured it. The `log_chunks` +
object-storage chunk pipeline is retired as the search substrate. Pagination
becomes keyset pagination over the store's own sort order, and every hardcoded
scan budget, the 8 MB skip, and the 2-slot semaphore are deleted.**

This is deliberately not a new idea. It is the exact shape `ProxyLogStorage`
already has (`crates/temps-proxy/src/storage/mod.rs`): one backend-neutral
trait, DTOs unchanged, Timescale default, ClickHouse opt-in, migrations applied
off the startup path. We are moving logs onto the road every other signal in
Temps already travels, rather than maintaining a second bespoke engine.

### 1. One trait, two backends

```
LogLineStore (new, in temps-log-aggregator)
├── write_batch(&[LogLineRow])          -- fail-open, background writer only
├── search(LogQuery) -> LogPage         -- keyset, always complete or an error
├── facets(LogQuery, &[FacetField])     -- real distinct values + counts
└── context(line_key, before, after)    -- the surrounding-lines view
```

`TimescaleLogLineStore` is selected when `is_clickhouse_enabled()` is false —
which is the default, so a stock `temps` install gets working pagination,
working facets and unbounded concurrency with **no new dependency**.
`ClickHouseLogLineStore` is selected when the four `TEMPS_CLICKHOUSE_*` vars
are present, and is the recommended path above roughly a few GB/day.

Both implement the same trait against the same DTOs, and a parity test harness
runs identical inputs through both and asserts identical pages — the mechanism
ADR-012 specified for analytics and ADR-016 reused for OTel.

### 2. The ordering key is the pagination key

Every line carries a total order that is stable, deterministic and monotonic:

```
(timestamp, container_id, line_id)
```

`line_id` is a `UInt64`/`bigint` assigned at ingest from a per-process
`AtomicU64` seeded from Unix nanoseconds at start. It is not semantically
meaningful; it exists so two lines emitted by one container inside the same
millisecond have a defined, repeatable order. That is all pagination requires.

- A cursor is that triple, serialized opaquely, versioned, and bound to a
  hash of the query scope — exactly the validation `global_search.rs` already
  performs on its cursor today, so the guard rails carry over.
- The next page is `WHERE (timestamp, container_id, line_id) < cursor ORDER BY
  ... DESC LIMIT n`. The engine's own index drives it. Deep pages cost the same
  as the first page.
- `line_id` doubles as the ReplacingMergeTree dedup component, so a retried
  insert converges rather than duplicating.

**`scan_limit_reached` is deleted.** A search either returns a complete page
with an accurate `next_cursor`, or it returns an error that says what to do.
Never a truncated page presented as an answer.

### 3. ClickHouse schema

Following the locked CH design in this repo (one raw table, native TTL, no
rollup MVs, query-time aggregation — see `0001_proxy_logs.sql`):

```sql
CREATE TABLE IF NOT EXISTS log_lines
(
    timestamp            DateTime64(3, 'UTC'),
    project_id           Int32,                       -- 0 sentinel for external services
    external_service_id  Nullable(Int32),
    env                  LowCardinality(String),
    service              LowCardinality(String),
    level                LowCardinality(String),
    stream               LowCardinality(String),      -- stdout / stderr
    container_id         String,
    node_id              Nullable(Int32),
    node_name            LowCardinality(String) DEFAULT '',
    deploy_id            Nullable(Int32),
    message              String CODEC(ZSTD(3)),
    fields               String DEFAULT '{}',
    line_id              UInt64,
    retention_days       UInt16 DEFAULT 30,
    _version             UInt64 DEFAULT toUnixTimestamp64Milli(now64())
)
ENGINE = ReplacingMergeTree(_version)
PARTITION BY toYYYYMMDD(timestamp)
ORDER BY (project_id, timestamp, container_id, line_id)
TTL toDateTime(timestamp) + toIntervalDay(retention_days)
SETTINGS index_granularity = 8192;

ALTER TABLE log_lines ADD INDEX IF NOT EXISTS idx_message_tokens
    message TYPE tokenbf_v1(32768, 3, 0) GRANULARITY 4;
```

Notes, each deliberate:

- **Daily partitions**, not monthly as `proxy_logs` uses. Log lines are
  higher-volume than requests and retention is shorter; daily partitions let
  TTL expiry be a `DROP PARTITION` at merge time rather than a row mutation.
- **`retention_days` per row + per-row TTL**, driven by the existing
  `temps_core::RetentionResolver` seam, so per-project retention is a plugin
  concern and OSS gets `FixedRetentionResolver`. This replaces the ad-hoc
  nightly `RetentionService` chunk-delete loop entirely.
- **`tokenbf_v1` on `message`** is the search index. Log search is
  overwhelmingly *token* search — a request id, an exception class, a path, a
  UUID — and a token bloom filter prunes granules for those at a fraction of
  the cost of an inverted index. Substring queries that are not token-aligned
  (`%oo%`) cannot use it and degrade to a scan of the time-pruned range; the UI
  must therefore present the query box as token search with an explicit
  "contains" toggle that is documented as slower. Exact bloom parameters and
  whether to add an `ngrambf_v1` companion are a Phase 0 benchmark, the same
  way ADR-016 left MV-vs-projection to a benchmark.
- **`LowCardinality` on env/service/level/stream/node_name** makes the facet
  `GROUP BY` a dictionary scan — this is what makes §5 cheap.

### 4. TimescaleDB schema (the default backend)

```sql
log_lines (
  timestamp timestamptz, project_id int, external_service_id int,
  env text, service text, level text, stream text, container_id text,
  node_id int, node_name text, deploy_id int,
  message text, fields jsonb, line_id bigint
)
-- hypertable, 1-day chunk_time_interval
-- compression after 1 day: segmentby (project_id, container_id), orderby (timestamp DESC, line_id DESC)
-- retention: drop_chunks policy (default 7 days; see Consequences)
-- index: (project_id, timestamp DESC, container_id, line_id)
```

Deliberately **no trigram/GIN index on `message`.** A GIN `pg_trgm` index over
a stdout firehose is a write-amplification disaster, cannot be used on
compressed Timescale chunks, and would bloat faster than the data it indexes.
Instead the Postgres backend does what Postgres is actually good at: the
composite index prunes to the project + time range, and text matching is an
`ILIKE`/`position()` filter over that already-pruned, compressed slice, bounded
by a `statement_timeout`.

This is honest rather than magical. It scales to the single-box install it
serves, and when it does not, the query **fails with a real message** ("this
search was too broad for the built-in log store — narrow the window, or
configure ClickHouse for indexed full-text search over longer ranges") plus a
link to the setup docs. Per the project rule that unconfigured features
onboard rather than disappear, the capability endpoint reports
`{ backend: "timescaledb" | "clickhouse", full_text: { indexed: false,
recommended_max_window_days: N, setup_path: "..." } }` so the console can tell
"too broad" apart from "not built".

### 5. Facets are a first-class endpoint

`POST /api/logs/global/facets` takes the same filter body as search plus the
requested fields, and returns distinct values with counts inside the current
window:

```
{ "env": [{"value":"production","count":18422}, ...],
  "service": [...], "level": [...], "node_id": [...], "deploy_id": [...] }
```

ClickHouse answers with `GROUP BY <field> ORDER BY count() DESC LIMIT 200` over
`LowCardinality` dictionaries. Timescale answers with the same `GROUP BY` over
the time-pruned index, capped and under `statement_timeout`, returning
`partial: true` if it hits the cap rather than pretending the list is complete.

This is what kills the page-scraped facets in `LogQueryInput.tsx`: the user can
now discover a value they have never seen on screen, which is the entire point
of a filter.

### 6. Concurrency: delete the semaphore

`SEARCH_SLOTS` is removed. Bounding moves from "2 requests globally" to
per-query engine limits, which is where it belongs:

- ClickHouse: `max_execution_time`, `max_rows_to_read`, `max_bytes_to_read` set
  per query. Exceeding them returns a ClickHouse error that maps to a clear
  4xx with the narrowing advice — a refusal, not a silent truncation.
- Timescale: `statement_timeout` on the search connection, plus the existing
  connection pool as the natural concurrency bound.
- The 30 s request deadline in `handlers/global.rs` stays as a backstop, but it
  stops being the normal path.

Concurrency then scales with the store, and an incident review with five people
in the room no longer hands four of them a 429.

### 7. Authorization must become an allow-list (security-sensitive)

Today authorization is inlined into the candidate SQL as a **deny-list**:
`NOT (p.id = ANY($hidden::int[]))` plus a `bound_project` narrowing and an
`EXISTS` over `project_services`. ClickHouse cannot join `projects` /
`project_services`, so the access decision must be resolved in Postgres first
and passed to the store as an explicit set.

**It must be passed as an allow-list, not a deny-list.** A deny-list handed to
a store that cannot verify it fails *open*: an empty or mis-computed
`hidden_projects` yields "show everything". The `LogLineStore` API therefore
takes a resolved `LogAccessScope { project_ids: Vec<i32>, external_service_ids:
Vec<i32> }` computed by the existing `project_access_checker`, and a resolution
failure is an error, never an empty-filter fallback. Instance admins get an
explicit `all: true` variant rather than an empty allow-list, so "admin" and
"resolution returned nothing" can never be confused.

The same change applies to the Timescale backend, so both backends share one
audited authorization path. **This section requires `security-auditor` sign-off
before implementation.**

### 8. Ingest replaces the chunk writer

`ChunkWriterService` is replaced by `LogLineBatchWriter`, modelled on
`ProxyLogBatchWriter`:

- The collector does a non-blocking `try_send` into a bounded mpsc. Full →
  drop + count. ADR-021's invariant is unchanged and is in fact easier to hold
  than before.
- A background task batches (target ~1,000 rows or 500 ms) and calls
  `write_batch`. On backend error: log, increment a drop counter, drop the
  batch. Never block, never retry unboundedly, never back up into the
  collector.
- The existing `dropped_lines` / shed-level instrumentation from ADR-021 is
  kept and surfaced, because the pipeline remains lossy by design and the
  viewer must say so.

### 9. The project-scoped search collapses onto the same store

`services/search.rs` (2,685 lines, `MAX_FULLTEXT_HOURS = 24`,
`MAX_CONCURRENT_FETCHES = 20`) is the same problem one scope down. It becomes a
`LogLineStore::search` call with `project_ids = [id]`. Keeping two engines —
one indexed, one scanning — would be the worst possible outcome of this ADR,
so retiring it is in scope, not a follow-up.

### 10. The in-flight `fix/global-logs-continuous` branch **should land**

Branch `fix/global-logs-continuous` (commit `95cd199fe`, a single commit on
`33eb6be30`) adds an `after_chunk` cursor boundary so an exhausted scan can
resume from a chunk frontier instead of stranding the user. It does not change
the architecture: still page_size 100, still full-chunk decompression, still
the 30 s timeout, still 2 concurrent searches, still no text index.

**Recommendation: merge it, and freeze the scan engine behind it.** Reasoning:

1. Today a user whose search exhausts the budget has **no path at all** to
   older matching lines. From the user's seat that is indistinguishable from
   data loss, and it is the specific thing that prompted "no pagination, no
   nothing". This ADR's Phase 3 is several phases away.
2. It creates no surviving debt. The diff is confined to
   `global_search.rs` and the two frontend files this ADR schedules for
   deletion. Nothing it adds has to be carried forward.
3. It creates no API trap. Cursors are already opaque, `version`-tagged and
   scope-hashed, so the new engine rejects a v1 cursor with "re-run this
   search" rather than mis-decoding it.
4. Shipping nothing for the whole rebuild window is the worse trade.

The condition is explicit: **it is the last change to the scan engine.** No
bloom filters, no budget retuning, no page_size plumbing goes into
`global_search.rs` after it. Further effort goes into `LogLineStore`.

### 11. Frontend

- **Infinite scroll over keyset cursors**, replacing "First page" / "Next
  page". Default page 200, server-capped (1,000). The list virtualizes —
  `LogExplorer.tsx` must not render 10k DOM rows.
- **Facets from `/logs/global/facets`**, not from `lines`. `LogQueryInput.tsx`
  loses the `lines` prop entirely.
- **The "Partial results / Search limit reached" alert is deleted.** It is
  replaced by a genuine error state when the engine refuses a query, with the
  narrowing advice and — when ClickHouse is not configured and the query needs
  it — the setup link from the capability endpoint.
- **Follow mode** becomes a forward keyset poll from the newest cursor rather
  than the current blind 5 s refetch of page 1.
- **CLI parity** (project rule): the new facets endpoint and the reshaped
  search get commands in `apps/temps-cli` (`@temps-sdk/cli`) in the same phase
  that ships the endpoint, never deferred.

---

## Alternatives Considered

### A. Keep chunk files, add per-chunk bloom filters (or an inverted side-table)

Compute a token bloom filter per chunk at flush time, store it as a `bytea` on
`log_chunks`, and let the candidate SQL prune chunks by text before download.

- **Pros:** tiny ingest cost (~8–32 KB/chunk); no storage migration; directly
  attacks the worst case (free-text over a wide window); keeps logs on cheap
  object storage and off the Postgres disk.
- **Cons, and they are fatal:** it prunes, it does not *order*. You still must
  download and decompress every surviving chunk to produce a page, so you still
  need budgets, still need the concurrency cap, still cannot early-exit a
  `LIMIT`. It does nothing for the most common query of all — no text filter,
  just "show me the last 500 lines" — which still walks every chunk. Facets
  remain impossible. And it commits us to owning a bespoke search engine
  forever, in a codebase that already runs three other stores with real ones.
  This is a real optimization of the wrong thing.

A Postgres `tsvector`/`pg_trgm` index over extracted line content was
considered in the same family and rejected harder: indexing every line in
Postgres costs approximately what *storing* every line in Postgres costs, minus
the payload — so if you can afford the index you can afford the table, and the
table also gives you ordering and facets.

### B. Embed a log-specific engine (Tantivy, or a Loki-shaped design)

- **Pros:** best-in-class text latency; Loki's "object storage + inverted
  index" is a proven fit for exactly this data.
- **Cons:** Tantivy indexes live on local disk with their own segment merge,
  compaction, backup and retention lifecycle — a *third* stateful subsystem in
  a product whose entire ops story is "Postgres, optionally ClickHouse". It
  breaks the ADR-017 split-process model (proxy and console are separate
  processes) and has no multi-node story. External Loki is worse still: it adds
  a mandatory service that is *not* already in the stack, unlike ClickHouse
  which is. Rejected on operational surface, not on technical merit.

### C. ClickHouse only, mandatory for Global Logs

- **Pros:** one backend, one query implementation, best performance.
- **Cons:** it fixes the feature only for operators who already run ClickHouse
  and leaves the default self-host install — the population the complaint came
  from — exactly as broken as today. It also contradicts ADR-012 §4 and the
  single-binary positioning that is a stated competitive edge. Rejected.

### D. ClickHouse only, with the chunk scan retained as the no-CH fallback

The tempting middle. Rejected for the same reason as C: the default install
keeps every defect this ADR exists to remove, and we would be maintaining the
scan engine indefinitely while claiming to have replaced it. "Fixed, unless
you're a normal user" is not fixed.

### E. Hot lines in Postgres, cold lines rolled off to chunk files

Keep ~7 days of lines in the hypertable for search, age older lines out to
`.ndjson.zst` on S3 for cheap long retention, with the existing chunk reader
demoted to a download/export path.

- **Pros:** bounds Postgres disk; preserves cheap 90-day+ retention.
- **Cons:** two code paths again, and a hard "search only covers 7 days" cliff
  that is its own support burden. **Deferred, not rejected** — it is the
  natural answer if Postgres disk pressure turns out to bite in practice, and
  nothing in this ADR forecloses it. Designing it now would be exactly the
  premature abstraction this project's principles warn against.

### Chosen: two backends behind one trait (Timescale default, ClickHouse opt-in)

It is the only option that fixes the default install, and it is the pattern
`ProxyLogStorage` / `OtelStorage` / `AnalyticsEvents` have already validated
three times in this codebase.

---

## Consequences

### Positive

- **Real pagination.** Keyset over the store's own sort order. Page 40 costs
  what page 1 costs. No budget, no stopwatch, no `scan_limit_reached`.
- **No silent data loss.** The `MAX_CHUNK_BYTES` skip is gone; there is no
  input the engine quietly refuses to read.
- **Concurrency scales with the store.** The 2-slot semaphore is deleted.
- **Facets become discoverable filters** instead of a summary of what is
  already on screen.
- **Retention becomes declarative** — CH per-row TTL / Timescale `drop_chunks`,
  driven by the existing `RetentionResolver` — replacing the nightly chunk
  delete-then-unlink job and its failure modes.
- **~3,400 lines of bespoke scan engine are deleted** (`global_search.rs` +
  `search.rs`) and replaced by two implementations of one trait.
- **Global Logs joins the rest of Observe.** One backend-selection story, one
  migration-runner pattern, one parity harness. Future work on logs benefits
  from CH investments already made for spans and proxy logs (codecs, facet
  slots, retention TTL) instead of being a special case.
- **Authorization gets audited and flipped to fail-closed.**

### Negative

- **The log firehose moves onto the control-plane Postgres disk in the default
  configuration.** This is the single biggest cost of this decision and it must
  not be glossed. Today logs consume S3/attached storage and zero Postgres
  disk; after this, a chatty tenant consumes WAL, autovacuum attention and disk
  on the same Postgres the control plane depends on. Mitigations, all
  mandatory for Phase 1: default Timescale-backend retention of **7 days** (not
  the chunk pipeline's 30 — operators who want 30 configure it or enable CH),
  a compression policy after 1 day, ingest drop-under-pressure from ADR-021
  (already tied to the existing disk-space alert sampler), and a visible "log
  storage used" figure in the console so the cost is legible rather than a
  surprise outage. If this proves insufficient in practice, Alternative E is
  the designed escape hatch.
- **Durability profile changes.** Chunks were durable objects on S3; the new
  write path is a fail-open batch insert that drops on backend error. This is
  consistent with ADR-021 (logs shed before anything else) but it is a real
  reduction in the worst case, and the drop counters must be surfaced, not
  buried.
- **Default-backend full-text search stays unindexed** and is bounded by a
  statement timeout. Better than today's flat 24-hour refusal, but it is a
  narrower capability than ClickHouse offers, and the UI must say so honestly.
- **Two query implementations to keep in step**, paid for with the parity
  harness (the same bargain ADR-012 accepted).
- **Substring-not-token queries do not use the bloom index** even on
  ClickHouse, and degrade to a time-pruned scan.

### Risks

- **Backfill and ClickHouse TTL-on-insert.** ClickHouse silently discards rows
  whose timestamp is already past the table TTL at insert time — HTTP 200,
  `written_rows=0`, no error. This exact trap is documented in
  `0001_proxy_logs.sql`. A backfill of historical chunks that ignores it will
  appear to succeed and import nothing. Phase 2 must either widen the TTL for
  the duration of the backfill or restrict the backfill to the retention window
  and say so.
- **Write-volume shock on the default backend.** Insert throughput into the
  hypertable must be load-tested against the shed thresholds before cutover;
  the drop-rate must be measured, not assumed.
- **Timestamp ties and out-of-order arrival.** `line_id` makes the order total,
  but lines can still arrive out of event-time order (the chunk writer already
  notes this: chunk bounds are event time, not arrival order). A follow-mode
  keyset poll can therefore miss a late line. Mitigation: follow mode polls
  from `now - small lag` rather than from the absolute newest key, and the lag
  is a documented constant.
- **Migration window with two writers.** Phase 0/1 dual-writes; a bug there
  doubles ingest cost. The dual-write is behind a flag and time-boxed.

---

## Migration Plan

`log_chunks` and its objects are live production data under a 30-day default
retention. Nothing is deleted early, and the old path stays readable until the
new one is proven.

**Phase 0 — the store, dark.**
`LogLineStore` trait + DTOs. Timescale `log_lines` hypertable migration
(compression + retention policies). ClickHouse `0001_log_lines.sql` applied via
the existing off-startup migration-runner pattern. `LogLineBatchWriter` wired
alongside `ChunkWriterService` as a **dual write**, behind a config flag,
default off. No read path changes. Benchmark the bloom parameters and the
Timescale insert ceiling here.

**Phase 1 — the read path, side by side.**
Implement `search` / `facets` / `context` on both backends. Resolve
authorization to the allow-list scope (§7) — security-auditor sign-off gates
this phase. The global endpoint gains an internal engine switch so the same
query can be run through both engines and diffed; the parity harness asserts
identical pages. `fix/global-logs-continuous` is already merged and serving
users throughout.

**Phase 2 — backfill (as built: automatic, not a phase an operator runs).**
There is no flag, no command and no phase gating here. A self-hosted operator
upgrades, restarts `temps serve`, and the import of their existing history
starts by itself — asking an operator to migrate their own data is not an
acceptable upgrade experience.

`ChunkBackfillService` (`services/chunk_backfill.rs`) is spawned as a
background task from `initialize_plugin_services`. It walks `log_chunks`
newest-first on the existing `(ended_at, id)` ordering (served by
`idx_log_chunks_global_time`), so recent history — what users actually search —
becomes queryable first, and it paces itself between small batches so it never
competes with live traffic for the connection pool. Unlike the retired query
path it has **no size cap** that skips input: a chunk is either imported or
counted as failed, never silently dropped.

Resumability and progress live in `log_chunk_backfill_state` (migration
`m20260917_000001_create_log_backfill_state`): a single row holding the
`(cursor_ended_at, cursor_id)` keyset position, `status`, and the
`chunks_processed` / `lines_migrated` / `chunks_failed` / `lines_skipped`
counters. `status = 'complete'` makes every later boot a single indexed read,
and an instance with no chunks at all (a fresh install) is marked complete
immediately.

**Duplicate prevention is structural, not a dedup index.** A backfilled line
gets a *new* `line_id` — the chunk format had no per-line identity, only a byte
offset — so `ON CONFLICT DO NOTHING` has nothing stable to conflict on. Instead
each chunk's `INSERT`s and its cursor advance commit in **one transaction**: a
crash mid-chunk leaves no rows and an unmoved cursor, so the retry is clean and
duplicates are impossible rather than deduplicated.

**Accepted trade-off:** while the import runs, a search over a range the
newest-first walk has not reached yet returns an honestly empty page. There is
no dual-read merge and no fallback to the deleted chunk-scan engine, and an
un-imported range is never an error.

**Retention-window resolution (closes the Risks-section question above):**
the walk stops at `now() - RetentionTable::LogLines.default_days()` (7 days by
default), recomputed on every `count_remaining`/`next_chunks` call rather than
pinned once at boot, since the horizon moves while a long walk runs. Chunks
entirely older than that are never read: they would be dropped by the very
next `drop_chunks` retention pass, so importing them is wasted I/O/CPU and
risks a confusing "logs appeared then vanished" experience. Verified live: of
240 seeded pre-upgrade chunks spanning 14 days, exactly the 112 chunks newer
than the horizon were imported (87,674 lines) and the walk correctly stopped
there — the other 128 were skipped, not attempted and failed.

**Phase 3 — cutover.**
Reads are served from `LogLineStore`. `ChunkWriterService` is disabled; the
dual-write flag is removed. `scan_limit_reached` remains in the response schema
as a deprecated, always-`false` field for one release so generated SDKs and the
CLI do not break, then is dropped. Frontend switches to infinite scroll,
virtualized rendering and the facets endpoint; the partial-results alert is
replaced by the real error state. CLI commands ship in this phase.

**Phase 4 — retire.**
Delete `global_search.rs`, the project-scoped `search.rs` scan engine,
`ChunkWriterService`, every `MAX_*` constant, `SEARCH_SLOTS`, and the chunk
`RetentionService`. `log_chunks` rows and their storage objects are removed
only **after the full retention window has elapsed past cutover** (30 days), so
no historical data is destroyed before the new store has independently aged
past it. The `LogStorage` trait and its S3/filesystem implementations are
removed unless Alternative E has been adopted in the interim, in which case
they become the cold tier.

---

## Implementation Notes

- **Affected crates:** `temps-log-aggregator` (trait, both backends, batch
  writer, handlers — the bulk of the work), `temps-entities` (`log_chunks`
  removal in Phase 4; new row types), `temps-migrations` (Timescale hypertable
  + policies), `temps-core` (`RetentionTable::LogLines` variant for the
  retention resolver), `temps-config` (no new keys — reuses
  `TEMPS_CLICKHOUSE_*`).
- **Affected frontend:** `web/src/pages/observability/GlobalLogs.tsx`,
  `web/src/components/observability/LogExplorer.tsx`,
  `web/src/components/observability/LogQueryInput.tsx`.
- **CLI:** new/changed endpoints get parity commands in `apps/temps-cli`
  (`@temps-sdk/cli`) in the same phase.
- **Migration needed:** yes — Postgres (new hypertable), ClickHouse (new
  table), plus a data backfill from `log_chunks`.
- **Breaking changes:** yes, at Phase 3 — `scan_limit_reached` is deprecated
  then removed; cursors issued by the old engine are rejected (with a clear
  "re-run this search" message, never mis-decoded); `page_size` semantics move
  from a fixed 100 to a cursor-driven, server-capped value.
- **Requires `security-auditor` sign-off:** §7, before Phase 1 ships.

## References

- `crates/temps-log-aggregator/src/services/global_search.rs` — the scan engine and its budgets.
- `crates/temps-log-aggregator/src/handlers/global.rs` — the 2-slot semaphore and 30 s timeout.
- `crates/temps-log-aggregator/src/services/search.rs` — the project-scoped twin (`MAX_FULLTEXT_HOURS`).
- `crates/temps-proxy/src/storage/mod.rs` — the `ProxyLogStorage` trait this design copies.
- `crates/temps-proxy/migrations/clickhouse/0001_proxy_logs.sql` — the locked CH table design and the TTL-on-insert warning.
- `crates/temps-core/src/retention.rs` — `RetentionResolver`, reused for per-row TTL.
- [ADR-012](012-clickhouse-analytics-backend.md), [ADR-016](016-clickhouse-traces-backend.md) — ClickHouse is optional and additive.
- [ADR-021](021-multi-node-log-aggregation.md) — the ingest invariants this ADR preserves.
- Branch `fix/global-logs-continuous` (`95cd199fe`) — the stopgap this ADR recommends merging and then freezing.
