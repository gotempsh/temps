# ADR-047: Global Logs line index in ClickHouse (attribute facets and analytics)

- **Status:** Proposed — extends ADR-046. The object store + Postgres
  manifest stay exactly as designed there; this ADR adds an *optional*
  per-line index in ClickHouse for attribute-level facets, histograms and
  analytics, the way Datadog's Log Explorer works.
- **Date:** 2026-09-20
- **Requires:** ClickHouse ≥ 25.3 when configured (for the `JSON` column
  type). Without ClickHouse everything in ADR-046 keeps working unchanged.

## Context

ADR-046 gives us a log store that is cheap (one manifest row per ~8 MB
chunk), fast for label/time/level queries (10–25 ms) and exact for label
facets — because the writer pre-aggregates counts per chunk. What it
cannot do is anything **per line by content**: "group by `status`", "top
routes by ERROR count", "requests slower than 500 ms", a per-minute
histogram split by `http.method`. Those need an index over attributes that
only exist once a line is parsed, and there is nothing to pre-aggregate
until the user has picked the attribute.

Product direction is explicit: the explorer should feel like Datadog's —
every attribute of a line is a facet you can click, group by and chart.
Datadog does this with an index over per-line attributes while the raw
event bodies live in object storage (Husky: "metadata service" +
"fragments"). Temps already runs the same split for traces: spans go to
ClickHouse when it is configured, and `0008_facet_slots.sql` learnt the hard
way that attribute filters over a JSON blob are a full scan at 10⁹ rows.

Constraints:

- **No second copy of log content.** Messages exist once, in the chunks.
  ClickHouse holds only what is needed to *find* and *count* lines.
- **Not configured ≠ broken.** Everything ADR-046 offers stays available
  without ClickHouse; attribute analytics show an onboarding state that
  says what is missing and links to the settings page (CLAUDE.md rule).
- **Same authz as everything else**: the allow-list scope of ADR-045 is
  applied inside every ClickHouse query; there is no cross-project leak
  path through aggregations.

## Decision

### 1. Roles of the three stores

| Concern | Store |
|---|---|
| Message bytes, block-level time/level pruning, bloom text search, WAL, GC, reconcile, retention | object store + Postgres manifest (ADR-046, unchanged) |
| Tail / "last N" / cursor pagination without attribute filters | ADR-046 planner (heads + manifest) — no ClickHouse on the hot path |
| Attribute facets, histograms, `GROUP BY` anything, top-lists, log-based metrics, attribute-filtered search | **ClickHouse `log_lines_index`** |
| Which attributes exist (facet sidebar), value cardinality | ClickHouse `log_attr_keys` (materialized aggregate) |
| Which attribute keys are *promoted* to fast slots | Postgres `log_line_facets` (key → slot), same shape as `otel_span_facets` |

Postgres remains the source of truth for *what chunks exist*; ClickHouse is
derived from the chunks and can be rebuilt from them (§6).

### 2. Schema

```sql
CREATE TABLE log_lines_index
(
    -- identity of the source stream (LowCardinality: few distinct values)
    project_id           Int32,
    external_service_id  Int32               DEFAULT 0,          -- 0 = none
    env                  LowCardinality(String),
    service              LowCardinality(String),
    deploy_id            Int32               DEFAULT 0,
    container_id         LowCardinality(String),
    node_id              Int32               DEFAULT 0,

    ts                   DateTime64(3, 'UTC') CODEC(Delta, ZSTD(1)),   -- ms on purpose, see Size
    level                Enum8('trace'=0,'debug'=1,'info'=2,'warn'=3,'error'=4),
    stream               Enum8('stdout'=0,'stderr'=1),

    -- pointer into the object store: line_id = chunk_seq << 20 | line_index
    chunk_seq            UInt64,
    line_index           UInt32,

    -- universal attributes, always fast (in the primary index or bloom-indexed)
    trace_id             String              DEFAULT '',
    span_id              String              DEFAULT '',
    request_id           String              DEFAULT '',
    status_code          UInt16              DEFAULT 0,
    http_method          LowCardinality(String) DEFAULT '',
    http_route           String              DEFAULT '',
    duration_ms          Float32             DEFAULT 0,

    -- everything else the parser extracted, typed subcolumns (≥ 25.3)
    attrs                JSON,   -- server default 1024 paths; ingest caps bound it

    -- operator-promoted attribute keys → fast bloom-indexed slots
    facet_attr_1  Nullable(String), … facet_attr_20 Nullable(String),

    INDEX idx_trace   trace_id    TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_request request_id  TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_route   http_route  TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_f1 facet_attr_1 TYPE bloom_filter(0.01) GRANULARITY 4, … idx_f20
)
    INDEX idx_ts ts TYPE minmax GRANULARITY 1
)
ENGINE = ReplacingMergeTree
PARTITION BY toDate(ts)
ORDER BY (project_id, service, chunk_seq, line_index)
TTL toDateTime(ts) + INTERVAL <retention_days> DAY
SETTINGS index_granularity = 8192;
```

Why each choice:

- **Sort key `(project_id, service, chunk_seq, line_index)` — the chunk,
  not `ts`, right after the labels.** Every row of a chunk is then
  contiguous: `container_id`/`deploy_id`/`env`/`chunk_seq` become constant
  runs and `line_index` is `0,1,2,…` under `Delta`. With `ts` in the key
  (the first draft) the five containers of one service interleave line by
  line and those columns alone cost 3.1 B/line — measured at 50 M lines
  that was **8.47 B/line vs 2.47 B/line** for the same data, same codecs.
  Time-range pruning comes from `PARTITION BY toDate(ts)` plus a `minmax`
  skip index on `ts`: chunks seal in time order, so `chunk_seq` is nearly
  monotonic in `ts` and the granules are time-clustered anyway.
- **`ReplacingMergeTree` keyed by `(…, chunk_seq, line_index)`** — the
  seal pipeline is idempotent (ADR-046 §8a.2); a replayed seal re-inserts
  the same rows and they collapse on merge. Queries use `FINAL`-free
  aggregates (duplicates only exist between a retry and the next merge and
  affect counts by at most one chunk).
- **`JSON` not `Map`** — a filter on an un-promoted key reads one subcolumn
  instead of the whole key/value bag. The ingest caps in §3 (≤ 32 keys per
  line, key grammar, per-container key budget) keep the path set far below
  the server's 1024-path default, so overflow into the shared blob does not
  happen in practice. (A `max_dynamic_paths` type parameter would make it
  impossible by construction, but the Rust client's header validation
  cannot express type parameters.)
- **Facet slots** — copied from spans (`0008_facet_slots.sql`): promoted
  keys get a bloom-indexed column, backfilled by mutation, written directly
  by the sink from then on. Same admin API family, new table
  `log_line_facets`.
- **No message bytes, not even a preview.** A 160-byte preview was 45 % of
  the index when measured; the reader fetches a block in ~1 ms, so
  hit-lists resolve pointers instead.
- **Size (measured 2026-09-20, slot 7, 53 M lines / 894 chunks, after
  `OPTIMIZE … FINAL`)**: **2.47 bytes/line** — 125 MiB of index beside
  607 MB of chunk objects (21 %). Per column: `request_id` 0.61 (unique per
  line; the floor), `ts` 0.48, `attrs` 0.07, everything else ≤ 0.03.
  Getting there took four measured steps: dropping the message preview
  (19.1 → 8.1 B/line), `ZSTD`/`Delta` codecs on the label and pointer
  columns, **millisecond** timestamps (nanoseconds cost 3 B/line and
  compress 11× worse under every codec tried — Delta, DoubleDelta, T64;
  the chunk keeps the exact timestamp), and the chunk-contiguous sort key
  above (8.47 → 2.47). Canonical keys are stored only in their fixed
  column, never duplicated inside `attrs`.
- **Query latency (same 53 M lines, single node, through the HTTP API)**:
  attribute keys 11 ms; facets `service,level,attr:worker` 260 ms;
  per-minute histogram 160–250 ms (by `attr:worker` 370 ms); `GROUP BY
  service,level` 100 ms; `p95(duration_ms)` by service 63 ms;
  `uniq(request_id)` by env 140 ms; pointer lookup by `request_id` 40 ms
  (one line, one block read); `attr` page of 100 lines 130–270 ms;
  `attr` + text needle (chunks nominated by the index, bloom-pruned scan)
  270 ms for a full page.
- **Reindex throughput**: 53 M lines / 894 chunks rebuilt from the local
  filesystem store in 270 s (≈ 196 k lines/s), 0 failures — the index was
  dropped and recreated for the sort-key change above with no ingest
  pause.

```sql
-- key discovery for the facet sidebar: which attrs exist and in how many
-- lines, per project and day. Fed by a materialized view over
-- log_lines_index using JSONAllPaths(attrs). Value cardinality/top values
-- are computed on demand for one key (a typed-subcolumn read) — a dynamic
-- path cannot be referenced by a runtime key inside a materialized view.
CREATE TABLE log_attr_keys
(
    project_id Int32, day Date, key LowCardinality(String),
    lines AggregateFunction(count)
) ENGINE = AggregatingMergeTree ORDER BY (project_id, day, key);
```

### 3. Attribute extraction (ingest)

The collector already parses level. Extend the parser to populate
`LogLine.fields` for:

- JSON object lines (`{"level":"info","msg":"…","status":500,…}`) — top
  level keys, one level of nesting flattened with `.`.
- logfmt / `key=value` pairs (`status=500 method=GET path=/api/x dur=12ms`).
- Well-known names are mapped to the fixed columns regardless of spelling
  (`status|status_code|http.status_code` → `status_code`, `trace_id|traceId|
  otel.trace_id` → `trace_id`, `duration|dur|latency|elapsed` with unit
  parsing → `duration_ms`, …).

Hard caps, enforced at extraction so ClickHouse can never be blown up by a
noisy app: ≤ 32 keys per line, key ≤ 64 bytes and `[A-Za-z_][A-Za-z0-9_.]*`
(numeric-looking keys are dropped), value ≤ 256 bytes, and a per-container
rolling cap of 256 distinct keys/hour beyond which new keys are dropped and
counted in a `dropped_attr_keys` metric. Extraction cost is bounded and
happens once, on the ingest thread, before the WAL append — so the WAL and
the chunk `fields` column carry exactly what the index will hold.

### 4. Writing the index

The `ChunkWriterService` seal pipeline (ADR-046 §8a.2) becomes:

```
encode → PUT object → INSERT manifest (returns seq) → INSERT index rows → truncate WAL
```

A new `LineIndexSink` trait (`index_chunk(seq, &labels, &[LogLine])`) is
implemented by `ClickHouseLineIndex` (one `RowBinary` batch insert per
chunk, 50k rows ≈ 1–2 MB, async insert enabled) and by `NoLineIndex` when
ClickHouse is not configured. Failure policy: the index insert is retried
with backoff (3 × up to 30 s); if it still fails the chunk is sealed
anyway and its `seq` is appended to a Postgres `log_index_backlog` table
that the reindexer (§6) drains. **Index failure never loses logs and never
blocks sealing.**

Head (unsealed) lines are *not* in ClickHouse; analytics therefore trail by
the flush window (≤ 5 min busy, ≤ 30 min idle). The explorer states this
("index current to hh:mm") rather than pretending. Tail views are served by
the ADR-046 planner and are always live.

### 5. Reading

`LogLineStore` gains an `analytics` capability behind a second trait,
`LogAnalytics`, implemented only when ClickHouse is configured:

- `facets(filter, fields)` — labels **and** attributes; `GROUP BY` per
  field with `count()` and top-N values; scope applied as `project_id IN
  (…) OR external_service_id IN (…)` from the ADR-045 allow-list.
- `histogram(filter, bucket, group_by)` — `count()` per time bucket,
  optionally split by any attribute or level.
- `aggregate(filter, group_by[], metric)` — top-lists and timeseries:
  `count`, `uniq(attr)`, `avg/p50/p95/max(duration_ms|numeric attr)`.
- `search(filter with attribute predicates)` — resolves matching
  `(chunk_seq, line_index)` newest-first with the page limit, then the
  ADR-046 reader fetches only the blocks those pointers fall in (block
  index from the cached footer, chunks in parallel) and materialises
  messages. Text search on the message body stays bloom + scan; when
  attribute predicates are also present ClickHouse answers only *which
  chunks* hold attribute matches (`matching_chunks`) and the regular scan
  runs over those (plus the unsealed head buffers, which no index has
  seen), enforcing the predicates per line from the same `fields` the
  index rows were built from (`AttrPredicate::matches`). Resolving
  pointers one by one and text-filtering them was measured at 7 hits in
  5 s for a 1-in-2000 needle; nominating chunks gives a full page in
  270 ms with the scan's budget, `partial` and cursor semantics intact.

HTTP surface (all under `/api/logs/global`, same auth + allow-list scope
as `search`): `GET attributes`, `GET facets/attrs?keys=…`,
`GET histogram?bucket_secs=&group_by=`, `GET aggregate?group_by=&metric=`,
each taking repeatable `attr=<key><op><value>` (`=`, `!=`, `^=`, `>`, `<`,
`<key>?` for exists); `POST search` accepts the same in `attrs[]`. List
query parameters are repeated keys, which needs `axum_extra::extract::Query`
(axum's own cannot deserialise a `Vec` from `?a=1&a=2`).

Without ClickHouse: `facets` keeps the manifest implementation (labels and
level only), `histogram` is served from the block index (per block
`first_ts/last_ts/line_count`, and per-block `level_counts` added to
`BlockMeta` in this ADR so level splits are exact at block granularity),
and `aggregate` / attribute predicates return `configured: false` with the
reason and settings path via `GET /api/logs/global/capabilities`.

### 6. Rebuild and backfill

ClickHouse is derived data. `reindex` walks manifests (optionally a time
range or project), decodes each chunk and re-inserts its rows; because of
`ReplacingMergeTree` this is safe to run at any time. It runs:

- automatically when ClickHouse becomes configured on an instance with
  existing chunks (from newest to oldest, rate-limited),
- for the `log_index_backlog` (§4),
- on demand from the admin API.

Retention: the ClickHouse TTL mirrors the instance's container-log
retention (Settings → Monitoring, `container_logs_days`, synced on every
hourly retention tick via `ALTER TABLE … MODIFY TTL`). Every path that
removes a chunk logically — retention tombstone, `purge_project`,
compaction, reconcile of a missing object — also calls
`forget_chunks(seqs)` (a lightweight `DELETE … WHERE chunk_seq IN (…)`), so
an aggregate never counts a line the reader can no longer fetch; TTL is the
backstop, not the mechanism. Verified at 50 M lines: purging a project's
221 chunks / 12.8 M lines took 600 ms, its index rows were gone
immediately, and the compactor GC removed the 221 objects on its next tick.

Compaction (ADR-046 §8a.1) re-encodes a run of chunks into one and so
changes every pointer. It therefore indexes the replacement chunk
*synchronously* and only forgets the source sequences once the index has
accepted it; if the index rejects the rows the run is abandoned atomically —
replacement row and object removed outright (a tombstone would absorb the
retry's idempotent insert under the same deterministic key), sources and
their index rows untouched — and retried on the next pass. Analytics never
see a window with neither the sources nor the replacement.

One thing that purge exposed: the collector resumed each container from
the newest *chunk row*, and once retention/purge plus GC had removed every
row of a container, a restart replayed the container's whole Docker log —
resurrecting exactly what the purge removed. The position now also lives in
`log_collector_positions` (upserted on every seal, never GC'd) and resume
reads `GREATEST` of both.

### 7. Version gate

At startup, when `TEMPS_CLICKHOUSE_*` is set, the plugin runs
`SELECT version()`; if `< 25.3` the line index is disabled with a logged,
user-visible reason ("ClickHouse 24.8 found; log analytics needs ≥ 25.3
for the JSON column type") surfaced through the capabilities endpoint.
Spans and metrics are unaffected.

## Alternatives considered

- **Everything in ClickHouse (messages too)** — a second copy of every log
  byte, two retention systems, and ClickHouse becomes required for the
  basic explorer. Rejected; conflicts with "no second copy" and with the
  effort to make ClickHouse optional for self-hosters.
- **Manifest-only with per-block counters** — exact and O(chunks) for
  labels/level (kept as the fallback), but cannot answer anything keyed on
  a per-line attribute. Rejected as the *only* path.
- **`Map(String,String)` for attrs** — full-bag read on every filter; the
  spans table already paid for this lesson. Rejected.
- **Only facet slots, no `JSON`** — un-promoted keys would be unqueryable
  until an operator promotes them; Datadog lets you click any attribute
  immediately. `JSON` gives column-speed for the long tail, slots give
  index-speed for the hot keys.
- **Support ClickHouse < 25.3 via `Map` fallback** — two schemas and two
  query generators forever for a version no current deployment runs.
  Rejected; version gate instead.

## Consequences

- Datadog-style attribute facets, group-by and charts when ClickHouse is
  configured; ADR-046 behaviour, plus a clear onboarding state, when not.
- One more table to size: ≈ 5–6 bytes per indexed line (to be measured).
- Analytics trail live data by the flush window; tail views do not.
- Ingest does bounded attribute extraction on every line (target ≤ 2 µs).
- The `line_id = seq << 20 | index` encoding from ADR-046 becomes a
  cross-store key and must stay stable.
