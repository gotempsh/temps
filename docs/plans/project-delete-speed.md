# Fast project deletion: take telemetry out of the cascade

**Status:** Option B approved; implementation in progress on this branch
**Date:** 2026-10-30
**Branch:** `fix/project-delete-speed`

## 1. The problem

Deleting a project that has served a lot of traffic takes minutes, and on
large instances it can take long enough that the request times out. The
handler awaits the whole teardown (`run_project_teardown`,
`crates/temps-projects/src/handlers/handlers.rs:1743`), so the user watches a
spinner for all of it.

The teardown steps that touch Docker (fence, hooks, cancel deployments, remove
containers, remove archives) scale with the number of containers, which is
small. The slow step is the last one:

```rust
// crates/temps-projects/src/services/project.rs:3311
temps_entities::projects::Entity::delete_by_id(project_id).exec(db).await?;
```

That one statement fires the whole `ON DELETE` graph under `projects`. I
queried a database migrated to current `main` and found **77 tables** that
reference `projects` directly, and about **190 FK edges** within three levels.
Most of them are small control-plane tables. A handful are high-volume
telemetry tables, and those account for the time.

## 2. Why it is slow

Postgres implements `ON DELETE CASCADE` / `SET NULL` as **row-level
triggers**. Every row deleted from a parent table runs one query against each
child table: `DELETE FROM ONLY child WHERE fk = $1`, or `UPDATE ... SET fk =
NULL`. For a hypertable that query runs against every chunk. Four things
compound in this schema.

### 2.1 Unindexed FKs into the request-log tables (the dominant cost)

| Table | FK column | Action | Index leading with the column? |
|---|---|---|---|
| `proxy_logs` (hypertable, one row per request) | `deployment_id` | SET NULL | **no** (only `(project_id, deployment_id, ts)` partial) |
| `proxy_logs` | `environment_id` | SET NULL | **no** (only `(project_id, environment_id, ts)` partial) |
| `events` (hypertable, analytics) | `deployment_id` | CASCADE | **no** |
| `events` | `environment_id` | CASCADE | **no** |
| `error_events` (hypertable) | `deployment_id`, `environment_id` | SET NULL | **no** |
| `request_logs` | `project_id`, `deployment_id`, `environment_id` | CASCADE | **no index at all** besides the PK |
| `performance_metrics` | `project_id`, `deployment_id`, `environment_id` | CASCADE | **no** |

The RI trigger's query filters on the FK column alone
(`WHERE deployment_id = $1`). It never includes `project_id`, so the
composite indexes can't serve it. Deleting the project cascades into
`deployments` and `environments`, and **each deployment row** then runs a
query over **all of `proxy_logs`, `events`, `error_events`, ... for every
project on the instance**, not only the one being deleted.

The total cost is roughly:

```
(#deployments + #environments) × (size of every request/analytics table)
```

A project that has served a lot of traffic has usually been deployed many
times too. With 400 deployments and 30 days of `proxy_logs`, that means
hundreds of full passes over the request log.

Compression makes this worse. `proxy_logs` compresses chunks after 24 h with
`segmentby = project_id`, and `events` after 7 days with
`segmentby = project_id, event_type`. Neither includes `deployment_id`, so a
filter on it has to decompress batches to evaluate them.

### 2.2 `proxy_logs.project_id` is `SET NULL`: it rewrites the logs instead of removing them

`fk_proxy_logs_project` is `ON DELETE SET NULL`, and so are its deployment and
environment FKs. Deleting the project therefore **UPDATEs every request row
the project ever logged**, up to the 30-day retention. On compressed chunks,
that means decompressing the project's segments and writing them back as
uncompressed tuples with a NULL `project_id`.

The result is the most expensive possible way to free no disk at all. The
orphaned rows take *more* space than before, belong to no project, and stay
until retention drops the chunk.

### 2.3 `events.project_id` cascades over up to 365 days

`events` keeps a year of data (`drop_after: 365 days`). The project cascade
deletes it row by row, decompressing as it goes.

### 2.4 All of it runs in one transaction inside the HTTP request

The whole cascade is a single statement. That produces one huge WAL burst,
holds row locks on every deployment and environment until commit, and builds
up autovacuum debt. On the 2 vCPU / 4 GB target it competes with live ingest
for the whole duration. If anything fails part-way, it all rolls back and the
next attempt starts from zero.


### 2.5 Measured

On a scratch TimescaleDB 2.30 database with 2M log rows across 5 projects and
200 deployments, with chunks older than a day compressed (the production
policy), deleting one project that owns ~400k log rows and 40 deployments:

| | Today's cascade | After this change |
|---|---|---|
| `DELETE FROM projects` | **fails** at the default `max_tuples_decompressed_per_dml_transaction` (100 000); **15.7 s** with the limit removed | **0.24 ms** |
| Removing the project's ~400k log rows | (inside the statement above) | 63 ms (compressed chunks) + 132 ms (uncompressed), in the background |

So beyond being slow, today's delete **fails outright** once a project has
more than 100k compressed log rows: the teardown stops after fencing, the
handler returns a 500, and the project stays fenced. The project delete
doesn't raise that limit anywhere. The cascade's cost grows with deployments
× total rows on the instance, so real instances are far worse than this toy.

The compressed purge removed 282k rows while the 100k decompression limit was
still in force, confirming that deleting by the `segmentby` column drops
whole compressed batches without decompressing them.

## 3. Options

### Option A: tombstone the project row (`is_deleted`), purge everything later

This is the "logical delete" you suggested. The column already exists: the
teardown's first step writes the fence (`begin_project_deletion`). We would
stop right after the container and archive cleanup, return 204, and let a
background job remove the rows.

- **Pro:** instant response, with no schema change to the telemetry tables.
- **Con:** the tombstone would then live for minutes to hours, not seconds,
  and every read path has to hide it. Today `get_project` and `get_projects`
  do **not** filter `is_deleted` (`services/project.rs:2385`, `:2407`).
  There are **335 `projects::Entity::find*` call sites in 95 files**, each
  one a chance to leak a deleted project into a list, an access check, an AI
  tool, a webhook lookup or an ingest path.
- **Con:** `projects_slug_key` is a plain unique index, so a tombstone holds
  its slug. We would have to rename the slug on tombstone or switch to a
  partial unique index, and the second option makes every
  `filter(Slug.eq(..)).one()` ambiguous.
- **Con:** the background purge still pays for §2.1 in full. It would grind
  through hundreds of full table passes in the background, and on a small box
  that is just as visible, as CPU and IO starvation of live traffic.

### Option B (chosen): logical delete for the *data*, not the project

Delete the project row quickly, as today, but take the high-volume tables out
of the cascade. Their rows become **unreachable the moment the project row is
gone**: every query that reads them is scoped by a project the caller can
access, and project IDs come from a sequence, so they are never reused. That
makes the rows logically deleted. A background purger then reclaims the space
in small batches.

- **Pro:** the tombstone window stays what it is today (seconds), so the 335
  read sites need no audit.
- **Pro:** it fixes the cause rather than moving it. With the FKs gone, there
  are no per-deployment scans and no `SET NULL` rewrite, either in the request
  or in the background.
- **Pro:** it takes a cost off the ingest hot path. Every insert into
  `proxy_logs` today runs three FK lookups and takes `KEY SHARE` locks on the
  parent project, environment and deployment rows. Dropping the FKs removes
  that work from each proxied request.
- **Con:** the database no longer guarantees that telemetry rows point at a
  live project. The purge queue becomes the guarantee, so it has to be durable
  and retried (§4.3).

Option B gives what the logical delete was meant to give (an instant delete)
without a long-lived tombstone on the most widely read table in the system.


## 4. What is implemented

### 4.1 Migration `m20261105_000001_project_data_purges`

1. **Drops every FK from the five telemetry tables** (`proxy_logs`,
   `events`, `error_events`, `request_logs`, `performance_metrics`) to
   `projects`, `environments`, `deployments` **and `error_groups`**. The
   `error_groups` one has to go too, which the first draft missed:
   `error_groups` cascades from `projects`, so keeping
   `error_events → error_groups` would still delete every error event row by
   row inside the request. FKs to `visitor`, `ip_geolocations` and
   `request_sessions` stay. Constraints are found in `pg_constraint`
   rather than by name, so installs whose constraint names differ are
   covered. On a hypertable the drop propagates to the chunks as a catalog
   change: 16 ms on the scratch database, with no rewrite.
2. **Lists 53 FK columns under `projects`** that had no index leading with
   the key (`deployments.environment_id`, `environments.project_id`,
   `env_vars.project_id`, ...), plus `project_id` on `request_logs` and
   `performance_metrics` for the purger. The migration does **not** build
   them. SeaORM runs a migration in a transaction, where only a plain
   `CREATE INDEX` is legal, and that blocks writes for the whole build. Some
   of these tables take a steady stream of writes (`performance_metrics`,
   `telemetry_client_outcomes`, `runtime_health_buckets`). So
   `temps_database::run_post_migration_indexes` builds them after the server
   has bound, with `CREATE INDEX CONCURRENTLY`, next to the existing
   reference indexes. A column that already has a valid index leading with
   it is skipped. A failed concurrent build's invalid leftover is dropped and
   rebuilt. Each index is named `idx_<table>_<column>_fk`, so `down` drops
   only these.
3. **Creates `project_data_purges`**: `project_id` (PK, no FK),
   `project_name`, `requested_at`, `next_attempt_at`, `lease_until`, `passes`,
   `consecutive_failures`, `rows_deleted`, `last_error`, `completed_at`, plus
   a partial index on pending rows. The per-table progress JSON from the first
   draft turned out unnecessary: every purge statement deletes by
   `project_id`, so resuming is just running the pass again.

`down` drops the purge table and the new indexes. It deliberately doesn't
re-add the FKs, because rows of projects deleted since `up` would violate them.

### 4.2 Delete path

`ProjectService::delete_project` deletes the project row and inserts its
`project_data_purges` row in one transaction. The rest of
`run_project_teardown` (fence, hooks, cancel, containers, archives, audit,
`ProjectDeleted`) is unchanged, and so is the `204` response.

### 4.3 Purger (`crates/temps-projects/src/services/data_purge.rs`)

Started by the projects plugin. It wakes on `Job::ProjectDeleted` and also
polls every 60 s, querying only pending rows.

- **Claim:** `UPDATE ... WHERE project_id = (SELECT ... FOR UPDATE SKIP
  LOCKED)` with a 10-minute lease, renewed after every statement. Several
  control planes can run it without purging the same project twice.
- **Hypertables:** checked at runtime, then purged chunk by chunk. A
  compressed chunk gets one `DELETE FROM <chunk> WHERE project_id = $1`
  (whole-batch deletion). An uncompressed chunk is purged in `ctid` batches
  of 5,000. A `ctid` is only unique within one physical relation, so batches
  never target the hypertable parent. The outer delete re-checks
  `project_id`, so a row can only be removed if it belongs to the purged
  project.
- **Plain tables:** the same `ctid` batches.
- **Pacing and bounds:** one statement per transaction, a 60 s
  `statement_timeout`, a decompression ceiling of 100k tuples (set only
  where the TimescaleDB version has that setting), and a 25 ms pause between
  statements.
- **Completion:** a purge completes only after a pass that started at least
  5 minutes after the delete removed nothing. That catches rows the proxy
  log batch writer flushed after the project was gone. Before this change,
  such a flush failed its whole batch on the FK, losing every other
  project's lines in it.
- **Failure:** `consecutive_failures` and `last_error` (naming the project,
  relation and step) are recorded, and the pass is retried with backoff
  from 60 s, doubling, capped at 1 h.
- **Safety:** if a project with the purged id exists again (for example,
  restored from a backup), the purge is closed without deleting anything.

### 4.4 Deleted projects are ignored by background work

The project row is gone seconds after the delete, so everything that starts
from `projects` stops seeing it. A sweep of every background loop found two
that read the telemetry tables directly. Both are now scoped to
`projects WHERE NOT is_deleted`, which also covers a project fenced
mid-teardown:

- **Weekly digest email**
  (`crates/temps-notifications/src/digest/digest_service.rs`): sitewide
  traffic, errors, top pages, countries, trends, funnels, the per-project
  section, and the project name in the header.
- **ClickHouse events fan-out**
  (`crates/temps-analytics-events/src/services/ch_fanout.rs`): a deleted
  project's queued events are treated as orphans, marked delivered and never
  sent.

The sweep confirmed the other background loops start from live records: the
retention jobs, the visitor activity report, autofix, agents, error-tracking
cleanup, session replay, Shield, OTel and Cloud outboxes, status pages and
console jobs. Project and environment ids are serial, so a new project can
never pick up leftover rows.

### 4.5 Tests

- **Purger** (`TestDatabase`, skips without Docker):
  - only the deleted project's rows are removed, and the purge completes;
  - compressed chunks are purged under a decompression ceiling of one
    tuple;
  - the settle window delays completion;
  - a failed pass records its error and backs off;
  - a project that exists again is never purged;
  - the purger's table list matches the migration's.
- **Migration:** index names fit Postgres's 63-byte limit, targets are unique,
  and the FK drop covers every telemetry table and parent.
- **Post-migration indexes** (real TimescaleDB container): the migration
  leaves the indexes to the post-migration build, after which every listed
  column has a valid leading index; a column another index already covers
  gets no duplicate.
- **Digest:** a live project, a fenced one and a deleted project's leftover
  events, where only the live project's numbers appear.
- **ClickHouse fan-out:** the event load is scoped to live projects.

### 4.6 End-to-end run

A local instance running this branch, with seeded data and chunks
compressed per the production policies:

- **busy-app:** 300 deployments, 3M proxy log lines, 300k events, 100k error
  events, 50k web vitals.
- **neighbor-app:** 50 deployments, 1M proxy log lines, 100k events, 20k
  errors, 20k web vitals.

Results:

- `DELETE /api/projects/{busy}` returned `204` in **1.2 s**. That covers the
  whole teardown (fence, container cleanup, audit).
- **Pass 1** removed all 3.45M busy-app rows in **18 s**: 3M proxy log lines
  in 9 s, most of them from compressed chunks.
- A proxy log line for busy-app was inserted after pass 1, standing in for a
  late batch flush. **Pass 2**, at the end of the settle window, removed it.
- **Pass 3** found nothing and completed the purge, with 0 failures.
- neighbor-app's rows were untouched in every table.

## 5. Not in this change

- **Visibility (planned PR 2):** an admin endpoint and a one-line console
  note for pending or failing purges. For now, progress is in the logs and
  in `project_data_purges`.
- **ClickHouse copies:** `events` and `proxy_logs` rows already in
  ClickHouse are not deleted when a project is deleted; they leave by TTL.
  That was already the case before this change.
- **Continuous aggregates:** `proxy_logs_stats_1m`, `events_hourly` and
  `error_events_*` keep stale buckets for a deleted project's id until their
  own retention drops them. Only HTTP handlers read them. One of those
  handlers, the console's general stats (`analytics.rs`, about line 3093),
  reads `events`/`events_hourly` without project scoping. It's not a
  background job, but it would count leftover rows until the purge ends.
- **Digest funnels** match events across *all* projects by event name, not
  just the funnel's own project. That's an older bug; this change only
  excludes deleted projects from it.
- **Restore/undo** of a deleted project: possible later, not a goal.
