-- Proxy/request logs table: system-of-record for one-row-per-HTTP-request traffic
-- through the Pingora reverse proxy when ClickHouse is enabled
-- (ServerConfig::is_clickhouse_enabled()). Mirrors the PostgreSQL/TimescaleDB
-- `proxy_logs` hypertable used by TimescaleDbProxyLogStore, but follows the locked
-- CH design decision: ONE raw table + native TTL for retention + query-time
-- aggregation. There are NO rollup materialized views; all 7 stats endpoints
-- (today, time-buckets, projects-health, ai-agents, ai-agents/timeline, ai-pages,
-- ai-status) bucket / GROUP BY at query time with toStartOfInterval(), countIf(),
-- multiIf(), etc. The benchmark below confirms every shape stays well under 100ms.
--
-- Column order in this DDL is LOAD-BEARING: ChProxyLogRow uses positional binary
-- serialisation (clickhouse Row derive), so the struct field order MUST match the
-- column order here exactly. Keep the two in lockstep.
--
-- Design decisions:
--
--   ENGINE: ReplacingMergeTree(_version)
--     The write path is fail-open: the background ProxyLogBatchWriter buffers up to
--     200 rows and flushes every 500ms; on a ClickHouse error the batch is logged
--     and DROPPED (never blocks live traffic, never panics). A transient failure +
--     internal retry could therefore re-send overlapping rows. request_id is a
--     natural unique key per request (assigned by Pingora) and is the LAST element
--     of the ORDER BY, so ReplacingMergeTree(_version) collapses an exact retried
--     row to a single canonical row (highest _version wins). This is the CH analog
--     of the idempotency the PostgreSQL serial `id` + batch INSERT gave us.
--     _version is the Unix-ms timestamp at ingest. The high-volume read paths
--     (list + the 7 aggregations) GROUP BY or LIMIT and tolerate the rare
--     pre-merge duplicate, so they do NOT use FINAL (keeps the hot scans fast);
--     the single by-request-id lookup uses ORDER BY timestamp DESC LIMIT 1 which
--     is duplicate-safe without FINAL. A plain MergeTree was considered (proxy
--     logs are immutable per request) but ReplacingMergeTree costs nothing extra
--     at these volumes and buys retry-idempotency for free — chosen deliberately.
--
--   PARTITION BY toYYYYMM(timestamp)
--     Monthly partitions let ClickHouse drop whole TTL-expired partitions in one
--     DROP PARTITION at the next merge cycle instead of row-level deletes, keeping
--     the 30-day retention mutation cheap even at billions of rows. This is the
--     highest-volume table in the system (one row per HTTP request).
--
--   ORDER BY (project_id, timestamp, request_id)
--     project_id FIRST: the list endpoint and 6 of the 7 stats endpoints are
--     project-scoped (or filter by a project_id IN list), so the primary-index
--     prefix eliminates all other projects cheaply. This mirrors the PostgreSQL
--     (project_id, timestamp DESC) hypertable index that the live system relies on.
--     timestamp SECOND: every query is a time-range scan and the list sorts by
--     timestamp DESC by default; keeping rows time-sorted on disk makes both the
--     range scan and the bucket aggregations sequential reads of already-sorted
--     compressed blocks (this is what makes query-time bucketing fast).
--     request_id LAST: the dedup key (see engine note) — unique per request.
--     status_code was considered as a 3rd prefix element but rejected: status is
--     almost never an equality filter on its own (it is range-bucketed in stats),
--     and adding it would push request_id to position 4, weakening dedup locality
--     for no scan benefit (countIf(status_code >= 400) reads the column regardless).
--     project_id is Nullable (unrouted requests have no project) so the table sets
--     allow_nullable_key = 1; NULLs sort consistently and unrouted-traffic queries
--     (project_id IS NULL) still hit the index prefix.
--
--   TTL toDateTime(timestamp) + INTERVAL 30 DAY
--     Matches the CURRENT proxy_logs retention (30 days) — NOT the 90-day TTL used
--     by the metrics/spans CH tables. The list + stats endpoints only ever query
--     recent windows (today / last N days), so 30 days of raw rows is the full
--     chartable history. Partitions entirely outside the window are dropped at the
--     next merge cycle.
--     IMPORTANT: ClickHouse enforces this TTL at INSERT time too — rows whose
--     `timestamp` is already older than 30 days are silently discarded (HTTP 200,
--     written_rows=0, no error). The live proxy always writes current timestamps so
--     it is unaffected, but a historical BACKFILL/import would need a wider TTL.
--
--   LowCardinality columns: method, request_source, routing_status, browser,
--   operating_system, device_type, bot_name, cache_status
--     All have small, bounded value sets (7 HTTP methods, a handful of sources /
--     routing statuses / browsers / OSes / device types / cache statuses, and a
--     bounded crawler/agent taxonomy for bot_name which is heavily GROUP BY-ed by
--     stats/ai-agents). LowCardinality stores them as a dictionary + integer
--     references, cutting storage and speeding up GROUP BY / index lookups.
--     status_code stays Int16 (not LowCardinality): it is range-compared
--     (>= 400, status-class buckets) far more than equality-grouped, and the
--     status-class GROUP BY is a multiIf() over the raw integer.
--
--   Nullable columns: response_time_ms, project/environment/deployment/session/
--   visitor/ip_geolocation/error_group ids, is_bot, request/response_size_bytes
--     These are true nullable values with no natural empty sentinel; is_bot is
--     deliberately Nullable(UInt8) to preserve the PostgreSQL tri-state (true /
--     false / unknown) — stats filter on `is_bot = 1` and NULL must NOT match.
--
--   String DEFAULT '' columns: query_string, container_id, upstream_host,
--   error_message, client_ip, user_agent, referrer, browser_version, trace_id
--     These map from Rust Option<String>; the ingest layer writes '' for None
--     (the canonical "unset" sentinel; LowCardinality/String '' is cheaper than
--     Nullable for columns that are mostly present or only string-filtered).
--     has_error is expressed as error_message != '' on the read side.
--
--   request_headers / response_headers String DEFAULT '{}'  (JSON-as-String)
--     The Rust ingest layer serialises serde_json::Value via serde_json, matching
--     the PostgreSQL `jsonb` columns. These are NOT part of ProxyLogResponse, so no
--     read path ever touches them — they are write-only here (kept for parity /
--     future header inspection). No native CH Map needed.
--
--   created_date Date
--     Denormalized from timestamp at ingest (matches the PostgreSQL column). Not
--     used in CH read paths (toDate(timestamp) would serve the same purpose) but
--     written for byte-for-byte parity with the entity.
--
-- BENCHMARK RESULT (2026-06-03, ClickHouse 26.2.5, local single-node):
--   Dataset: 5,000,000 rows / 35 projects / 500 paths / 13 status codes /
--            6 bot agents / 5 days (~1M rows/day, ~12.5% unrouted NULL-project).
--   Timings are best/avg of 3 warm runs, statistics.elapsed from FORMAT JSON.
--
--   (a) LIST query — project_id=7 + 24h range + method='GET' + status_code=200 +
--       path LIKE '%resource%', ORDER BY timestamp DESC LIMIT 50:
--         best 5.7ms, avg ~6ms   (rows_read 65,536 — index-pruned)
--       Filtered COUNT(*) for pagination, same WHERE:
--         best 5.6ms, avg ~6ms   (rows_read 57,344)
--       Unfiltered count() (approximate_row_count analog):
--         best 1.2ms             (metadata-only, no scan)
--
--   (b) stats/time-buckets — 1h buckets over 7 days, count + avg(response_time_ms)
--       + countIf(status_code>=400) + sum(req/resp bytes), WITH FILL spine:
--         project-scoped:  best 7.8ms,  avg ~8.4ms  (rows_read 139,264)
--         all projects (worst-case full scan): best 25.9ms, avg ~37ms (5M scan)
--
--   (c) stats/ai-agents — is_bot=1 + bot_name IN (known agents), count() +
--       uniqExact(client_ip) + max(timestamp) GROUP BY bot_name (full scan):
--         best 31ms, avg ~32ms   (5M scan)
--
--   (d) stats/ai-agents/timeline — 1h buckets GROUP BY bucket,bot_name WITH FILL
--       spine (full scan): best 24.5ms, avg ~28ms
--   (e) stats/projects-health — GROUP BY project_id, project_id IN (10 ids), 24h:
--         best 7.5ms, avg ~10ms  (rows_read 409,600 — index-pruned)
--   (f) stats/ai-status — multiIf status-class GROUP BY (full scan): best 13ms
--   (g) by-request-id lookup (request_id = ?, ORDER BY timestamp DESC LIMIT 1):
--         with idx_request_id bloom_filter: best 3.3ms (rows_read capped ~40,960
--         vs ~5M without the skip index).
--
--   DECISION: query-time aggregation chosen (this file only; no rollup MVs).
--   Every list + stats shape is comfortably under the 100ms threshold at 5M rows.
--   The ORDER BY (project_id, timestamp, request_id) prefix makes the common
--   project-scoped queries index-pruned sequential reads; the few unavoidable
--   full-table aggregations (ai-* with no project filter) still finish in ~25-37ms
--   thanks to LowCardinality columns + ClickHouse's vectorised engine.
--   Re-evaluate if a single project accumulates many tens of millions of rows/day.
CREATE TABLE IF NOT EXISTS proxy_logs
(
    -- Request timestamp (partition + sort key). Millisecond precision; ingest
    -- writes via the clickhouse Row derive, read back as chrono DateTime<Utc>.
    timestamp            DateTime64(3, 'UTC'),

    -- Request line
    method               LowCardinality(String),
    path                 String,
    query_string         String              DEFAULT '',
    host                 String,

    -- Response
    status_code          Int16,
    response_time_ms     Nullable(Int32),

    -- Classification
    request_source       LowCardinality(String),
    is_system_request    UInt8               DEFAULT 0,
    routing_status       LowCardinality(String),

    -- Tenant / deployment context (nullable: unrouted requests have none)
    project_id           Nullable(Int32),
    environment_id       Nullable(Int32),
    deployment_id        Nullable(Int32),
    session_id           Nullable(Int32),
    visitor_id           Nullable(Int32),

    -- Routing / upstream detail
    container_id         String              DEFAULT '',
    upstream_host        String              DEFAULT '',
    error_message        String              DEFAULT '',

    -- Client identity
    client_ip            String              DEFAULT '',
    user_agent           String              DEFAULT '',
    referrer             String              DEFAULT '',
    request_id           String,
    ip_geolocation_id    Nullable(Int32),

    -- User-agent parse
    browser              LowCardinality(String) DEFAULT '',
    browser_version      String              DEFAULT '',
    operating_system     LowCardinality(String) DEFAULT '',
    device_type          LowCardinality(String) DEFAULT '',

    -- Bot / crawler detection (is_bot tri-state preserved via Nullable)
    is_bot               Nullable(UInt8),
    bot_name             LowCardinality(String) DEFAULT '',

    -- Sizes
    request_size_bytes   Nullable(Int64),
    response_size_bytes  Nullable(Int64),

    -- Cache
    cache_status         LowCardinality(String) DEFAULT '',

    -- Header payloads (JSON-as-String; write-only, never in ProxyLogResponse)
    request_headers      String              DEFAULT '{}',
    response_headers     String              DEFAULT '{}',

    -- Denormalized partition helper (matches the entity column)
    created_date         Date,

    -- Observe-view join keys
    trace_id             String              DEFAULT '',
    error_group_id       Nullable(Int32),

    -- Dedup key: Unix millisecond timestamp set at ingest. ReplacingMergeTree
    -- keeps the row with the highest _version per ORDER BY key, so a retried
    -- write of the same request_id converges to one canonical row.
    _version             UInt64              DEFAULT toUnixTimestamp64Milli(now64())
)
ENGINE = ReplacingMergeTree(_version)
PARTITION BY toYYYYMM(timestamp)
ORDER BY (project_id, timestamp, request_id)
TTL toDateTime(timestamp) + INTERVAL 30 DAY
SETTINGS index_granularity = 8192, allow_nullable_key = 1;

-- Bloom-filter skip index on request_id for the single by-request-id lookup.
-- request_id is the 3rd ORDER BY element, so a request_id-only lookup has no
-- usable primary-index prefix; this skip index caps rows_read at ~40k (one
-- granule block per matching part) instead of a full-table scan as the table
-- grows. Granularity 4 is the ClickHouse-recommended sweet spot.
ALTER TABLE proxy_logs ADD INDEX IF NOT EXISTS idx_request_id request_id TYPE bloom_filter GRANULARITY 4;
