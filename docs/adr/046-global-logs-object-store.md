# ADR-046: Global Logs on object storage with a manifest index

- **Status:** Proposed — supersedes the *storage* sections of ADR-045 (§4
  TimescaleDB schema, §8 ingest, Phase 2 backfill). Everything ADR-045 built
  above the `LogLineStore` trait — keyset pagination, facets endpoint,
  allow-list authorization, the frontend, the CLI — stands unchanged.
- **Date:** 2026-09-19

## Context

ADR-045 moved container stdout/stderr out of `.ndjson.zst` chunk files and
into a TimescaleDB hypertable so that Global Logs could paginate, facet and
run concurrent searches. It works, and it was verified live at 2M lines. It
also has a ceiling that ADR-045 itself named as its biggest cost: **the log
firehose now lives on the control-plane Postgres disk.**

That ceiling is structural, not tunable:

- Postgres growth is proportional to **line count**. A chatty tenant's logs
  compete with users, deployments and billing for the same WAL, autovacuum
  and disk. Retention and compression bound it; they do not remove it.
- One hypertable on one Postgres instance is one writer. There is no
  horizontal path for ingest short of a different store.
- The most recent day is always uncompressed — the highest-write window
  takes the full write-amplification hit.
- Measured cost is ~27% **more** disk than the chunk format for the same
  data, on the most expensive disk in the system instead of the cheapest.

Tables of log lines are the design every log product tries first and every
log product at scale abandons — Loki, Quickwit, OpenObserve, Parseable, and
ClickHouse's own S3-backed mode all put the **bytes** in object storage and
keep only a small **index** in a database. This ADR does the same.

### Why the old chunk engine failed, precisely

It is essential to be exact here, because "S3 chunks" is what ADR-045 just
replaced, and the new design must not be the old one with a fresh name.
The old engine (`global_search.rs` at `54db42a5e`) failed for
**implementation** reasons, none of which are inherent to object storage:

| Failure | Actual cause |
|---|---|
| Every query read whole chunks | One zstd frame per chunk. `line_offsets` were byte offsets into the *uncompressed* stream — useless without a seekable format. |
| No early exit; hence `MAX_CHUNKS = 512`, 64 MB budgets | The scan filled a `BTreeMap` from batches of 32 whole chunks. It never used `ended_at` to prove no unread chunk could precede what it already had. |
| Free text = decompress everything | No content summary of any kind per chunk. |
| Level filter = decompress everything | Only `has_errors` in the manifest. |
| `SEARCH_SLOTS = 2` | Honest consequence of 64 MB per request. |
| Silent skip of chunks > 8 MB | A defensive cap that became data loss. |
| "Facets impossible" | Untrue for **labels** — env, service, node, deploy, container are all manifest columns. It is only true for free-text content, which nobody facets. |

ADR-045 rejected the "keep chunks, add blooms" alternative with *"it prunes,
it does not order"*. That was wrong. Ordering does not require an index over
lines; it requires chunk time bounds, time order **within** a chunk, and a
correct stop rule. Loki has run on exactly that for years.

### Constraints carried over from ADR-045

- Single-binary self-host: no new mandatory service. Object storage may be
  S3 **or the local filesystem** (the existing `LogStorage` trait already has
  both).
- ADR-021 shed order is law: route traffic > app stdout > ship logs > index.
- ADR-017 split-process topology must keep working.
- Multi-node workers ship over mTLS to the control plane today; the design
  must allow them to write to S3 directly later.

## Decision

**Log bytes live in immutable, block-structured chunk objects on object
storage. Postgres holds one manifest row per chunk — never per line. A
`ChunkStore` implements `LogLineStore` by planning over manifests, reading
only the blocks a page needs, and merging newest-first with a provable early
exit. The TimescaleDB `log_lines` backend and its backfill are deleted.**

Postgres growth becomes proportional to **chunk count** — three to four
orders of magnitude smaller than line count — and the control-plane disk is
no longer where logs go.

### 1. Chunk format v2

One chunk = one container (stream), lines in time order, one object, and
**the object is a conforming zstd stream** (`.zst`): the blocks are ordinary
zstd frames and the footer rides in a zstd *skippable frame* that any
decoder ignores. `zstd -dc chunk.zst` yields the concatenated block bodies;
our reader finds the footer through the fixed trailer at the end.

```
┌──────────── body ────────────┐┌──────── footer (skippable frame) ─────────┐
│ block 0 │ block 1 │ … │ block n ││ hdr │ labels │ block index │ bloom │ trailer │
└──────────────────────────────┘└───────────────────────────────────────────┘
```

- **Block:** ≈256 KB of lines *before* compression, each block an
  independent zstd frame (level 3). The body is **columnar**: zigzag-varint
  timestamp deltas, `level[]`, `stream[]`, varint message lengths + bytes,
  varint fields lengths + bytes. A level or in-block time filter scans a
  few KB of arrays before touching a message. Line order within a chunk is
  time order.
- **Block index:** per block `{offset, compressed_len, uncompressed_len,
  first_ts, last_ts, line_count, first_line_index, level_mask,
  level_counts[5]}` — the per-block level counts make time histograms
  exact at block granularity from the cached footer (ADR-047 §5 fallback).
- **Bloom:** one per chunk over lowercased tokens and their 3-grams, sized
  per chunk from the distinct-entry count for ≈1% false positives — about
  1% of the raw bytes.
- **Labels:** the stream's identity — project_id, external_service_id, env,
  service, container_id, node_id, deploy_id — plus `started_at`/`ended_at`,
  `line_count`, `level_mask`, `level_counts`, written into the footer. **A
  chunk file is self-describing.** The Postgres manifest is a cache of the
  footers, reconstructible by walking the bucket with the existing
  `list_chunks`; losing or restoring Postgres never loses or orphans logs,
  and archival/export/tenant-move is an object copy.
- **Trailer:** 48 bytes — labels/index/bloom offsets and lengths, crc32,
  format version, magic `TLC2`.

**Why this body and not NDJSON/text.** Measured with
`examples/format_bench.rs` on two corpora (130k lines of a real Temps
server log, 200k synthetic access-log lines), zstd level 3:

| block body | server log | access log | full decode | ERROR-only |
|---|---|---|---|---|
| columnar, absolute `i64` ts (first draft) | 1560 KB | 3071 KB | 13 / 23 ms | 5 / 14 ms |
| **columnar, delta-varint ts (chosen)** | **1043 KB** | **1485 KB** | 13 / 21 ms | 4.5 / 11 ms |
| NDJSON per line | 1544 KB | 2696 KB | 190 / 169 ms | 34 / 35 ms |
| `ts stream LEVEL msg` text | 1502 KB | 2584 KB | 96 / 68 ms | 37 / 25 ms |

Absolute nanosecond timestamps are nearly incompressible; deltas halve the
object. A greppable body would cost 1.5–1.8× the space and 5–15× the CPU
per query, so the "open it with `zstd | grep`" property is provided at the
container level (valid zstd stream) rather than at the record level. Block
size was also measured: 1 MB blocks were 7–21% smaller in the micro
benchmark but only 2% smaller end-to-end (19.0 vs 19.4 MB for 2M lines),
while tripling "last 500" latency at 40 containers (14 → 42 ms) because a
tail query decodes the newest block of every live container — so 256 KB
stays. zstd 6 bought 0–12% for 3× encode time and zstd 19 was 100× slower
to encode — both rejected. End-to-end on the same 2M-line load the format
change took objects from 27 MB to 19.4 MB with unchanged query latency.

Reading a page therefore costs one range-GET for the footer and one
range-GET per *block* actually needed — never the whole object. A 9 MB
chunk is no different from a 90 KB one.

**Flush policy** (replaces 1 MB / 30 s): flush a stream's head at 8 MB
uncompressed, or at 5 min age, or on graceful shutdown. Small chunks from
idle containers are merged by the compactor (§8a.1), so the flush window is
chosen for freshness and WAL size, not for query fan-out. A **local
WAL** (`TEMPS_DATA_DIR/logs/wal/<container>`) is appended per line and
truncated on flush; on startup surviving WAL files are sealed into chunks.
Temps restarts on every self-update — losing up to 15 minutes of every
container's logs around each upgrade is not acceptable, and the WAL is
cheaper than shrinking the flush window.

Chunk size arithmetic: 100 containers, mixed idle/busy, ≈15k chunks/day,
≈450k manifests at 30-day retention. That is a small Postgres table. The
same data as lines would be ~10⁹ rows.

### 2. Manifest (Postgres) — the only index

`log_chunks` is extended, not replaced:

| Column | Note |
|---|---|
| existing labels | project_id, external_service_id, env, service, container_id, deploy_id, node_id |
| `started_at`, `ended_at` | unchanged; `(ended_at DESC, id DESC)` is the planner's order |
| `format_version` | 1 = legacy single-frame, 2 = this ADR |
| `index_offset`, `index_len` | footer location so the reader issues one range-GET |
| `level_mask` | smallint bitmask; generalises `has_errors` |
| `line_count`, `compressed_size_bytes` | unchanged; drives the "log storage used" figure |

Dropped: `line_offsets` (meaningless for v2, kept nullable for v1 rows).

Indexes: `(ended_at DESC, id DESC)`; `(project_id, ended_at DESC)`;
`(container_id, ended_at DESC)`. Nothing content-related lives in Postgres —
the bloom was deliberately kept out of the manifest (450k × 80 KB would be
36 GB, defeating the purpose).

### 3. Query planner

`search(query)` runs:

1. **Candidate manifests** — SQL over `log_chunks` with the ADR-045
   allow-list scope, label filters, `level_mask & wanted <> 0`, time overlap,
   keyset from the cursor, `ORDER BY ended_at DESC, id DESC`, in batches of
   64. This is the query the old engine already ran; it stays indexed and
   cheap.
2. **Footer fetch** — up to 16 in parallel. Prune blocks by time overlap and
   `level_mask`. If the query has text, tokenize the needle and test every
   *interior complete* token against the bloom; a miss drops the chunk. A
   needle with no complete token (e.g. `"onnection refus"`) simply does not
   prune — it is still bounded by time and labels, exactly like Loki.
3. **Block fetch + scan** — newest block first, decompress, scan records
   with `memchr`-based substring match, push survivors into a heap keyed
   `(ts DESC, container_id, line_id)`.
4. **Early exit (the rule the old engine lacked)** — a line at time `T` is
   final once every unprocessed candidate has `ended_at < T`. Emit the page
   as soon as `page_size` lines are final. For the most common query of
   all — "last 500 lines, no filter" — this terminates after reading the
   tail block of roughly one chunk per active container.
5. **Time budget, not byte budget** — if 10 s elapse first, return the
   final lines so far **and a valid continuation cursor** at the last fully
   processed chunk, with `scanned_back_to: <ts>`. Because chunks are
   processed in a deterministic order that is a pure function of the
   cursor, the partial page is a true prefix of the full result, and *Next*
   keeps working. The old engine's `scan_limit_reached` + disabled *Next*
   was the symptom of not having this property; it is the one thing this
   design must never regress.

`facets(query, fields)` is `SELECT DISTINCT` over manifest label columns
(and `level_mask` unions) within scope + window. Indexed, sub-100 ms, and
returns values the user has never had on screen — the exact property ADR-045
added the endpoint for.

`context(key, before, after)` decodes `chunk_id` from `line_id` (see §5),
reads that block and its neighbours, and crosses into the previous/next
manifest of the same container at chunk edges. When the id's home no longer
exists — the head buffer sealed, or the chunk was compacted into a new one —
the line is relocated by the stable half of the key, `(container_id,
timestamp)`, in whichever live chunk now spans that instant, so a result a
user is still looking at keeps answering "show context" for as long as the
line is within retention.

`sources(query)` is `SELECT DISTINCT (container_id, service, node_id)` from
manifests.

### 4. Hot tier

Lines not yet flushed live in a per-stream in-memory head buffer inside the
collector (as today). The head is exposed to the planner as a virtual
newest chunk with the same block-scan interface, merged into any query whose
window reaches "now". Tail keeps reading it directly. Collector, head
buffers and the query handler share the console process, so ADR-017 split
mode is unaffected.

### 5. Keys and cursors

`LogLineKey` stays `(timestamp, container_id, line_id)`. For sealed chunks,
`line_id = chunk_id << 20 | line_index` (8 MB ÷ ~50 B ≈ 160k lines, well
under 2²⁰). Head-buffer lines carry a sentinel chunk id; when the head is
sealed their `line_id` changes. Cursor resume therefore treats
`(timestamp, container_id)` as the primary position — Docker timestamps are
nanosecond precision and monotonic per container — and `line_id` as a hint.
The same holds after compaction, which re-encodes lines into a new chunk
(new `chunk_id`, new `line_index`): `line_id` is a locator, never an
identity, and every reader that receives one it cannot find falls back to
`(timestamp, container_id)`. `CURSOR_VERSION` bumps to 3.

### 6. Cache

Chunk objects are immutable, so caching is trivial: an LRU on local disk
(`TEMPS_DATA_DIR/logs/cache`; budget is the persisted, audited setting
Settings → Monitoring → container logs → `cache_mb`, default 2 GiB, applied
at runtime within a minute) keyed
by `(storage_key, byte_range)`. Footers are pinned preferentially; blocks
evict first. Paging back and forth through an incident, or two engineers
looking at the same window, hits object storage once. In the filesystem
backend the cache is a no-op.

### 6a. Performance model

Project, external service (database), deployment, node, env, container and
level are all **manifest columns**. Deciding *which* bytes to read is a
Postgres index lookup; log volume enters only through how many blocks a
page needs, and the stop rule bounds that at ≈ one block per active
container. The latency floor is therefore an object-storage round trip
(~20–40 ms on S3, ~0.2 ms on the filesystem backend), not data size.

Design choices that keep labelled queries on that floor:

- **Footer split into tiers.** Labels + block index (~2 KB/chunk) are a
  separate section from the bloom (~80 KB). The cache pins the index tier
  for every chunk in retention (450k × 2 KB ≈ 900 MB) and evicts blooms
  and blocks LRU. Labelled/level queries then plan from local disk and
  issue exactly one GET per block returned; the bloom is read only for
  text queries.
- **Write-through.** The node that seals a chunk places its footer in the
  cache immediately, so on a single-node install no index is ever fetched.
- **Speculative tail read.** "Latest" queries issue one range-GET for the
  last 512 KB of the object, which holds the footer *and* the newest block.
- **Columnar blocks.** `ts[]` and `level[]` are contiguous arrays ahead of
  the message bytes, so level and in-block time filters scan a few KB
  without decoding messages that will not be returned.
- **Head buffer.** The newest lines of a live deployment are answered from
  memory with no object read.

Expected (to be measured, see Verification): last-500-lines for a project
with 20 containers ≈ 60–100 ms cold / ~10 ms warm; one deployment or one
database ≈ 40 ms cold; ERROR-only over 24 h ≈ 50–150 ms; next page
≈ 10–30 ms; facets < 50 ms. Free text without a label filter is the only
query that scales with the window, and it degrades to a streamed,
resumable scan rather than a slow answer.

### 6b. Capacity model

Assumptions: 150 B average raw line, zstd ≈ 10× → 15 B/line stored.
Flush at 8 MB / 5 min with a **64 KB minimum-size gate** (idle streams
flush at most every 30 min). Compactor merges per stream per day, capped
at 64 MB raw per chunk. Bloom sized per chunk at 9.6 bits per distinct
token (1 % FP).

| | Small | Medium | Large |
|---|---|---|---|
| Containers | 10 | 100 | 1 000 (100 busy) |
| Lines/s | 10 | 500 | 10 000 |
| Object storage, 30 d | 0.4 GB | 19 GB | 390 GB |
| Same data as `log_lines` rows, 30 d | 0.6 GB | 30 GB | ~650 GB on control-plane disk |
| Chunks/day before compaction | 500 | 6 k | 45 k |
| Live chunks after compaction | 300 | 3 k | 36 k |
| `log_chunks` size | <1 MB | ~2 MB | ~30 MB |
| Index tier pinned in cache | 0.6 MB | 6 MB | 72 MB |
| Head-buffer RAM typical / worst | 1 / 80 MB | 5 / 800 MB | 50 MB / 8 GB |
| WAL on disk, max | 5 MB | 100 MB | 450 MB |
| Ingest CPU | ~0 | <1 % core | ~1 % core |

Expected query latency at Large, S3 cold / warm (filesystem ≈ warm):
last 500 lines for a 20-container project ~100 / ~10 ms; one deployment or
database ~40 / ~5 ms; ERROR-only 24 h ~50–150 / ~15 ms; next page ~20 ms;
facets <50 ms; free text within one project over 7 d ~0.5–2 s; free text
with no label filter over 30 d and no matches ≈ 110 s cold (36 k bloom
reads, 3.4 GB, 16-way parallel) — streamed, resumable, and mostly cached
for the newest two weeks.

What the model adds to the design: the minimum-size flush gate (without it
1 000 idle containers cost 288 k PUTs/day), the 64 MB compaction cap, per-
chunk bloom sizing, a 2 GB default cache with index → bloom → block tiers,
and a per-container head cap — both budgets are persisted settings
(`container_logs.cache_mb` / `head_buffer_mb`), never environment
variables, so an operator changes them from the console without a
restart. Nothing external is required at any of the three sizes.

### 7. Concurrency

No global semaphore. Per-request limits: 16 concurrent GETs, 256 MB
decompressed. Hitting the byte limit yields the same resumable cursor as the
time budget — never a 429, never an abort. A page costs hundreds of KB, not
64 MB, so the reason `SEARCH_SLOTS` existed is gone.

### 8. Ingest and shedding

The collector's per-stream bounded queue is unchanged. Under pressure the
order of loss is: skip bloom construction for the chunk (mark
`bloom_len = 0`; the chunk is then never pruned, only scanned) → drop the WAL
fsync → drop lines. Object writes are retried with backoff from the head
buffer; the manifest row is inserted only after the object write succeeds
(same exactly-once shape as the ADR-045 backfill).

### 8a. Lessons taken from Datadog's Husky

Husky (Datadog's third-generation event store) is the reference
implementation of "bytes on object storage, metadata in a small
transactional store". Its shape validates this ADR; its operational
lessons change it in six places:

1. **Flush and compaction are separate knobs.** Writers seal chunks
   *quickly* (every 2–5 min or 8 MB — freshness, bounded WAL); a background
   **compactor** merges a stream's chunks into a few per day (query
   fan-out, compression, bloom selectivity). The swap is one Postgres
   transaction — insert the merged manifest, delete the originals; old
   objects are removed after the grace period in (3). Compaction is part
   of this ADR, not a later phase.
2. **Visibility is the manifest commit, and the commit is idempotent.**
   Object first, row second. The chunk key is deterministic from
   `(container_id, first WAL sequence)`, so a crash-and-replay re-PUTs the
   same key and `UNIQUE(storage_key)` absorbs the duplicate row. Restarts
   never duplicate lines.
3. **Garbage collection in both directions, with a grace period.**
   Retention and compaction delete the manifest first and the object
   ≥ 1 h later, so an in-flight reader is never pulled out from under. A
   reconcile sweep re-adopts objects that have no manifest (their footer
   carries the labels) or deletes them if past retention, and marks
   manifests whose object is gone so the UI says "chunk missing" rather
   than failing silently.
4. **The metadata store is what gets hot.** Nothing content-derived ever
   lives in a `log_chunks` row, and every planner index is partial on
   `deleted_at IS NULL`. Converting the table to a TimescaleDB hypertable
   (partition-drop retention) is deferred: it requires the time column in
   the primary key, i.e. a rewrite of a live table, and at ≤ 100k rows the
   batched tombstone + GC delete is not measurable.
5. **Readers and writers share only the format and the manifest.**
   `ChunkWriter` (collector side) and the planner/`ChunkReader` (query
   side) have no shared in-memory state except one narrow interface to the
   head buffer. That is what keeps ADR-017 split mode trivial and makes
   phase-2 direct-to-S3 workers a configuration change.
6. **Fair-share, not a global cap.** Per-request time/byte budgets plus a
   per-user in-flight cap replace the old `SEARCH_SLOTS = 2`. One user's
   wide scan degrades that user's query, not the console.

Deliberately *not* copied: FoundationDB, Kafka in front of the writers
(the collector's bounded queue is the ADR-021 shed point), separate
reader/writer fleets, and full per-fragment inverted indexes.

### 9. Retention and cost

Default 30 days (ADR-045 had cut this to 7 because Postgres disk was the
constraint; it no longer is). The existing retention loop deletes manifest
rows and objects together; an S3 lifecycle rule at `retention + 7 d` is the
documented backstop for orphaned objects. Cost is the same class as the old
chunk engine (measured ~27% below the hypertable) plus ~1% for blooms and a
few percent for per-block framing — on object storage priced at cents per
GB-month, on a disk the control plane does not depend on.

### 10. Migration

None for data. Format-v1 chunks are read by a v1 path (whole-object
decompress, which is bounded because v1 flushed at 1 MB uncompressed; the
old 8 MB skip is removed). New chunks are v2. `format_version` is the switch.
The `log_lines` hypertable, `log_backfill_state`, `ChunkBackfillService` and
`TimescaleLogLineStore` from this branch are **deleted before merge** — they
were never released.

### 11. Multi-node (phase 2, designed for, not built)

Today workers ship lines to the control plane over mTLS and the control
plane writes chunks. Because the chunk writer is a library with no
control-plane dependency beyond the manifest insert, a worker holding S3
credentials can write chunks itself and send only the manifest row over the
existing channel. Ingest bandwidth then scales with worker count instead of
funnelling through the console. Nothing in this ADR forecloses it; nothing
in it requires it.

## Alternatives considered

**Keep TimescaleDB as default, ClickHouse as escape hatch (ADR-045 as-is).**
Fixes search but relocates the firehose onto the control plane; the escape
hatch is a second mandatory service for anyone who actually hits scale.
Rejected — this ADR exists because the default must scale, not the opt-in.

**ClickHouse with S3 disk.** Technically the same shape as this design with
a real query engine. Still a mandatory second service for the default
install. Remains a valid *future* backend behind `LogLineStore` for operators
who already run it; not the default.

**Embed Tantivy / Quickwit-style splits.** Real inverted index, best text
latency. Rejected for the same reason ADR-045 gave: segment merge, compaction
and local-disk lifecycle of a third stateful subsystem, for a text-search
gain that blooms + `memchr` capture most of at log volumes.

**Bloom in Postgres for SQL-side pruning.** Would let one query prune text
before any object read. 80 KB × 450k rows is 36 GB of Postgres — the exact
thing this ADR removes. Rejected. A per-stream-per-day bloom *rollup object*
is the correct future optimisation if no-match free-text over wide windows
proves painful.

## Consequences

**Positive.** Postgres stays MB-scale at TB of logs. Control-plane disk, WAL
and autovacuum are untouched by log volume. Retention returns to 30 days at
object-storage prices. Every ADR-045 correctness property survives: real
keyset pagination, facets, no concurrency cap, honest partial results.
Silent chunk skipping is gone by construction — no page ever needs a whole
object. No data migration at all.

**Negative.** We own a small log engine (chunk format, planner, cache) —
roughly the size of the code this branch already wrote for the Timescale
path plus the old scanner, and a pattern with a decade of prior art. The
worst case is a free-text needle that matches nothing, over every project,
over 30 days: that is a footer read per candidate chunk, streamed with
progress and a resumable cursor, not a fast answer. The head buffer is
process-local, so a query during a console restart can miss up to the WAL
replay window.

**Implementation notes (as built).** `log_chunks.id` is a UUID, so a
`seq BIGINT IDENTITY` column was added to carry the `line_id` encoding.
The partial-page resume point is the *horizon* (the newest unprocessed
chunk's `ended_at`), not the last processed chunk's — using the latter
re-selects the same chunk and never advances; the end-to-end test drives a
1-byte budget through every page and asserts the union equals the complete
result. Graceful shutdown seals every head (`serve/console.rs`); crashes are
covered by the WAL. The head→sealed handover has a brief window where a line
is visible from both (documented `FOLLOW-UP` in `chunk_writer.rs`).

**Measured (2026-09-19, dev slot 7, filesystem backend, `fast` profile).**
40 labelled containers × 50 000 lines = 2 024 848 lines ingested through the
real collector; sealed by the 5-minute age rule into 42 v2 chunks.

| | ADR-046 | ADR-045 hypertable, same data |
|---|---|---|
| Object storage | 27 MB (bloom 7–61 KB/chunk, footer ≤ 65 KB) | — |
| Postgres | **288 kB** (`log_chunks`, 42 rows) | ~33 MB (2M rows) |
| WAL after seal | 64 kB (611 MB while the 2M lines were unsealed) | — |

Query latency via `POST /api/logs/global/search`, page 500, cold / warm:
last 500 no filter 28 / 14 ms · one service (5 containers) 12 / 11 ms · one
deployment 11 / 11 ms · one env 15 / 18 ms · ERROR only 25 / 24 ms · ERROR +
service 17 / 14 ms · page 2 / 3 via cursor 18 / 21 ms (ordered, no overlap)
· facets over 4 fields 30 ms · text with no match 10 ms (bloom prunes every
chunk) · text that is a substring of a token 42 / 12 ms · text present in
every chunk (one line per container) 280 ms — a genuine decode of all 2M
lines, ~7M lines/s. The first cut of the planner materialised every match
of a chunk's newest block before the heap kept `limit`; capping each chunk
at `limit` newest matches took "last 500" from 370 ms to 14 ms.

Known cost left: `HeadSource::snapshots()` cloned every unsealed line per
query (1.0 s with 2M unsealed lines); being replaced by Arc'd segments +
line-free summaries.

**Verification before Accepted.** Same live protocol as ADR-045: generate
≥2M lines across ≥50 containers into slot 7, then demonstrate (a) "last 500
lines" completes reading ≤ one block per active container, (b) a
request-id search prunes ≥95% of chunks via bloom, (c) a no-match search
returns a resumable cursor within the time budget and *Next* continues,
(d) Postgres `log_chunks` size vs. object-store size, (e) facets under
100 ms.
