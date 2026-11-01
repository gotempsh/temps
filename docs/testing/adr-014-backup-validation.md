# Backup System Testing Plan — ADR-014 Validation

> Hand-off plan for validating the refactored backup system on **staging first**, then **prod**. Follow phases in order. Each phase can be stopped and resumed — see the sign-off checklist at the end.
>
> **Scope:** Phases 0–4 of ADR-014 are shipped (claim queue, runner, 7 engines, async HTTP handlers, stopgap belt-and-suspenders). Phase 5 decommission is **not** yet done. Scheduler still uses the legacy synchronous path (Phase 3 only partially complete) — manual API backups are runner-driven, scheduled backups are not.
>
> **Goal:** prove the runner works end-to-end and the flag is safe to flip in prod.

---

## Prerequisites

Before starting, confirm the following. If any item is missing, stop and resolve it.

### Access

- [ ] SSH access to the staging temps server (`ssh staging.temps.sh` or equivalent).
- [ ] SSH access to the prod temps server (only needed for Phase J).
- [ ] `psql` connection string for the staging temps control-plane DB:
      `export TEMPS_PG_STAGING="postgres://temps:***@127.0.0.1:5432/temps"`
- [ ] `psql` connection string for the prod control-plane DB:
      `export TEMPS_PG_PROD="postgres://temps:***@10.0.0.1:5432/temps"`
- [ ] An admin API token for the staging temps API. The CLI and curl both read
      one of `TEMPS_TOKEN`, `TEMPS_API_TOKEN`, or `TEMPS_API_KEY` (first non-empty wins —
      see `apps/temps-cli/src/config/store.ts:236`). Use:
      `export TEMPS_TOKEN="<staging admin token>"` (re-export for each phase as needed)
- [ ] An admin API token for prod (used only in Phase J).
- [ ] Base URL of the staging API. All routes live under `/api` (e.g. `POST /api/backups/...`).
      The Rust router calls `.nest("/api", ...)` at `crates/temps-cli/src/commands/serve/console.rs:1216` —
      there is **no `/v1` prefix**. Use:
      `export TEMPS_URL="https://staging.temps.sh"` (curl with `$TEMPS_URL/api/backups/...`)
- [ ] Base URL of prod:
      `export TEMPS_URL="https://temps.sh"`

### CLI / local tools

- [ ] `bunx @temps-sdk/cli --version` resolves (CLI installable; not pre-pinned).
- [ ] `psql`, `curl`, `jq`, `docker` installed locally and/or on the staging host.
- [ ] An S3 source already configured in staging with **valid** credentials (used in Phases A–F).
- [ ] An S3 source configured with **deliberately invalid** credentials, OR willingness to create one mid-test (used in Phase G).
- [ ] Working AWS CLI configured against the same S3 bucket as the test S3 source (for verifying uploaded artifacts). `aws s3 ls s3://<bucket>/` must work.

### Terminals / dashboards to keep open

Open these in side-by-side panes / tabs. You'll cycle between them:

1. **Terminal A** — SSH session on the staging host, ready to run `journalctl -u temps -f` (or `tail -f /var/log/temps/temps.log` if not systemd-managed).
2. **Terminal B** — Local psql session: `psql "$TEMPS_PG_STAGING"`. You'll run the named queries from §"Pre-flight commands" below.
3. **Terminal C** — Local shell for `curl` / `bunx` commands.
4. **Browser tab** — Staging temps UI logged in as admin, `/backups` page open. Refresh after each step.
5. **Browser tab** — AWS S3 console (or `aws s3 ls` in a terminal) pointed at the bucket the test S3 source writes to.

### Test data preconditions

- [ ] Staging has at least one project with ID known to you. Call it `$PROJECT_ID`. You'll need it for the CLI commands.
- [ ] You have permission to create external services in that project.
- [ ] Staging is **not** servicing user traffic during this test window. Do not run this plan against an environment that's also taking real traffic — a few of the failure-path tests will deliberately leave failed rows behind.

---

## Pre-flight commands (named queries)

Run these from Terminal B. They appear throughout the plan, referenced by name. Define them once here.

### Q_jobs_recent — recent jobs in the claim queue

```sql
SELECT
    id, backup_id, engine, target_kind, target_id, state, step,
    attempts, max_attempts, claimed_by, leased_until, next_attempt_at,
    error_message, started_at, finished_at, created_at
FROM backup_jobs
ORDER BY created_at DESC
LIMIT 20;
```

### Q_jobs_by_backup — jobs for a specific parent backup row

```sql
-- :backup_id is the parent backups.id (returned from POST /backups/.../run)
SELECT
    id, engine, state, step, attempts, max_attempts,
    claimed_by, leased_until, error_message, started_at, finished_at
FROM backup_jobs
WHERE backup_id = :backup_id
ORDER BY id DESC;
```

### Q_steps_for_job — step trail for a job

```sql
-- :job_id is the backup_jobs.id
SELECT
    id, attempt, step, state, message, durable_state, occurred_at
FROM backup_job_steps
WHERE job_id = :job_id
ORDER BY occurred_at ASC;
```

### Q_backup_row — the user-facing backup row

```sql
-- :backup_id is the backups.id
SELECT
    id, project_id, state, error_message, s3_location, size_bytes,
    compression_type, started_at, finished_at, last_heartbeat_at,
    created_at, updated_at
FROM backups
WHERE id = :backup_id;
```

### Q_ext_backup_row — the external-service backup row

```sql
-- :backup_id is the backups.id (external-service backups also write here)
SELECT
    id, project_id, external_service_id, state, error_message,
    s3_location, size_bytes, compression_type, finished_at
FROM external_service_backups
WHERE backup_id = :backup_id;
```

### Q_stuck_running — running rows whose lease has expired

```sql
SELECT id, backup_id, engine, state, step, leased_until, NOW() - leased_until AS overdue
FROM backup_jobs
WHERE state = 'running' AND leased_until < NOW()
ORDER BY leased_until ASC;
```

### Q_running_count — currently running jobs (for concurrency tests)

```sql
SELECT engine, COUNT(*) AS running
FROM backup_jobs
WHERE state = 'running'
GROUP BY engine
ORDER BY engine;
```

### Q_schedules_recent — recent schedule firings

```sql
SELECT id, external_service_id, cron_schedule, last_run, next_run, last_job_id
FROM backup_schedules
ORDER BY last_run DESC NULLS LAST
LIMIT 10;
```

### Q_force_stale_heartbeat — manually mark a row's heartbeat as stale (Phase H)

```sql
-- :backup_id is a backups.id that's currently state='running'
UPDATE backups
SET last_heartbeat_at = NOW() - interval '10 minutes'
WHERE id = :backup_id AND state = 'running'
RETURNING id, state, last_heartbeat_at;
```

---

# Phase A — Pre-flight (flag OFF, baseline sanity)

**Goal:** prove nothing regressed in the legacy path. **Run this on staging.**

### A1: Verify build is clean

**Do** (from your local checkout):
```bash
cd ~/projects/temps/temps
cargo check --lib --workspace 2>&1 | tail -20
```

**Expect:** Last line is `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in Xs` or similar. Zero compile errors.

**Fail mode:** Any `error:` lines, or the build fails to complete. Do not proceed — fix compile errors first.

### A2: Confirm flag is OFF on staging

**Do** (in Terminal A on the staging host):
```bash
sudo systemctl show temps --property=Environment | tr ' ' '\n' | grep -i BACKUP
```

**Expect:** No line mentions `TEMPS_BACKUP_RUNNER_ENABLED`, OR it's explicitly `=false`. If unset, that's correct — default is false.

**Then** restart the server and watch logs:
```bash
sudo systemctl restart temps
sudo journalctl -u temps -n 200 --no-pager | grep -iE "BackupRunner|backup runner|backup engine"
```

**Expect:** Log line containing `BackupRunner disabled` (or equivalent disabled-state message). Do **not** see "Engines registered" or "BackupRunner enabled".

**Fail mode:** If you see "BackupRunner enabled" or "Engines registered", the flag is on. Stop and revert the env before continuing.

### A3: Trigger a manual control-plane backup (synchronous path)

**Do** (Terminal C):
```bash
# Find an S3 source ID first
curl -sH "Authorization: Bearer $TEMPS_TOKEN" \
    "$TEMPS_URL/api/backups/s3-sources" | jq '.[] | {id, name}'
```

Pick one. Set `export S3_SOURCE_ID=<id>`.

```bash
# Trigger control-plane backup. Time it.
time curl -sS -X POST \
    -H "Authorization: Bearer $TEMPS_TOKEN" \
    "$TEMPS_URL/api/backups/s3-sources/$S3_SOURCE_ID/run" \
    | tee /tmp/a3-resp.json | jq .
```

**Expect:**
- `time` shows the call **blocked for the duration of the dump** (seconds to minutes depending on DB size). NOT <100ms.
- Response JSON has `"state": "completed"`, a non-empty `"s3_location"`, and a non-zero `"size_bytes"`.
- Record `export BACKUP_ID=$(jq -r .id /tmp/a3-resp.json)`.

**Then** (Terminal B):
```sql
-- Q_backup_row
SELECT id, state, error_message, s3_location, size_bytes, finished_at
FROM backups WHERE id = :backup_id;  -- use $BACKUP_ID
```

**Expect:** one row, `state='completed'`, `finished_at` populated, `s3_location` populated, `size_bytes > 0`.

**Fail mode:**
- Call returned <100ms — the flag is ON, not OFF. Go back to A2.
- Response is `{"state": "failed"}` — legacy path broken. Stop and triage.
- Row missing — handler regression. Stop.

### A4: Trigger a manual external-service backup (Redis)

**Setup** — ensure a Redis external service exists:
```bash
bunx @temps-sdk/cli services list --project $PROJECT_ID | grep -i redis
```

If none, create one:
```bash
bunx @temps-sdk/cli services create \
    --project $PROJECT_ID \
    --type redis \
    --name redis-test-a4 \
    --version 7
```

Note the ID: `export REDIS_SVC_ID=<id>`.

**Do:**
```bash
time curl -sS -X POST \
    -H "Authorization: Bearer $TEMPS_TOKEN" \
    "$TEMPS_URL/api/backups/external-services/$REDIS_SVC_ID/run" \
    | tee /tmp/a4-resp.json | jq .
```

**Expect:**
- `time` shows it blocked (synchronous).
- Response has `"state": "completed"`, populated `s3_location`, non-zero `size_bytes`.

**Then** (Terminal B): run Q_backup_row with the new `BACKUP_ID` from `/tmp/a4-resp.json`. Verify `state='completed'`.

**Also run Q_ext_backup_row** to confirm a row exists in `external_service_backups`.

**Fail mode:** same as A3.

### A5: Confirm `backup_jobs` is empty (legacy path doesn't write to it)

**Do** (Terminal B):
```sql
-- Q_jobs_recent
SELECT COUNT(*) FROM backup_jobs WHERE created_at > NOW() - interval '1 hour';
```

**Expect:** `0`. The legacy path does not touch `backup_jobs`. If you ran this against a database where Phase 0 migrations haven't applied, you'll get `relation "backup_jobs" does not exist` — that's a different failure (Phase 0 migration didn't run).

**Fail mode:**
- `count > 0` → flag was on for some of A3/A4, or there are pre-existing rows. Either way, the test is contaminated; clean up:
  ```sql
  DELETE FROM backup_jobs WHERE created_at > NOW() - interval '1 hour';
  ```
  and restart A3.
- `relation does not exist` → Phase 0 migration missing. Stop; run migrations.

**Phase A sign-off:** legacy path works end-to-end on staging, no `backup_jobs` rows produced.

---

# Phase B — Flag ON, control-plane (the simplest async path)

**Goal:** prove the runner works end-to-end on the engine we know best (the temps control-plane DB itself).

### B1: Enable the flag and verify runner starts

**Do** (Terminal A on staging):
```bash
# Add to the systemd unit, or wherever Environment is set:
sudo systemctl edit temps
```

Add (or confirm) under `[Service]`:
```
Environment=TEMPS_BACKUP_RUNNER_ENABLED=true
Environment=TEMPS_BACKUP_RUNNER_INSTANCE_ID=staging-host
Environment=TEMPS_BACKUP_RUNNER_MAX_CONCURRENT=4
```

Save, then:
```bash
sudo systemctl restart temps
sudo journalctl -u temps -n 300 --no-pager | grep -iE "BackupRunner|engines registered|backup engine registered"
```

**Expect:**
- A log line containing `BackupRunner enabled` (or similar startup message).
- A log line listing registered engines: `control_plane`, `redis`, `mongodb`, `postgres_pgdump`, `postgres_walg`, `postgres_cluster`, `s3_mirror`. All seven must appear.
- The server is otherwise healthy: `curl -sS $TEMPS_URL/api/health` returns `200`.

**Fail mode:**
- "BackupRunner disabled" still appears — the env var didn't take. Check `systemctl show temps --property=Environment`.
- Fewer than 7 engines registered — plugin.rs registration missing one. Stop.
- Server doesn't start — check for unrelated startup errors first; the runner shouldn't break boot.

### B2: Enqueue a control-plane backup, expect 202 in <100ms

**Do** (Terminal C):
```bash
time curl -sS -X POST \
    -H "Authorization: Bearer $TEMPS_TOKEN" \
    "$TEMPS_URL/api/backups/s3-sources/$S3_SOURCE_ID/run" \
    -w '\nHTTP %{http_code}\n' \
    | tee /tmp/b2-resp.json
```

**Expect:**
- HTTP status line shows `HTTP 202`.
- `time` shows the call returned in <100ms (typically <30ms).
- Response body is a `BackupResponse` with `"state": "pending"` (or possibly `"running"` if the runner picked it up in the same tick — both are acceptable).

Set `export BACKUP_ID=$(jq -r .id /tmp/b2-resp.json)`.

**Fail mode:**
- HTTP 200 + state=completed + multi-second time → handler did not enqueue; it ran inline. The flag isn't taking effect in the handler. Stop and check `BackupService::create_backup` branching.
- HTTP 500 → handler regression. Capture response body, stop.

### B3: Immediately query `backup_jobs` for the new row

**Do** (Terminal B):
```sql
-- Q_jobs_by_backup, with $BACKUP_ID
SELECT id, engine, state, step, attempts, claimed_by, leased_until, started_at
FROM backup_jobs
WHERE backup_id = :backup_id;
```

**Expect:** exactly one row.
- `engine = 'control_plane'`
- `state` is `'pending'` or `'running'`
- If `'running'`: `claimed_by = 'staging-host'`, `leased_until` is ~5 minutes in the future, `started_at` is recent.

Record `export JOB_ID=<id>`.

**Fail mode:**
- Zero rows — handler returned 202 but didn't actually insert. Stop and triage the insert path.
- Multiple rows for the same `backup_id` — handler bug (double-insert). Stop.

### B4: Poll until completion

**Do** (Terminal C, in a loop):
```bash
for i in $(seq 1 60); do
    curl -sS -H "Authorization: Bearer $TEMPS_TOKEN" \
        "$TEMPS_URL/api/backups/$BACKUP_ID" \
        | jq -r '"\(.state) \(.s3_location // "no-location") \(.size_bytes // 0)"'
    sleep 2
done
```

**Expect:** state progresses `pending → running → completed`. On completion, `s3_location` populates and `size_bytes` becomes non-zero. For a small control-plane DB, total elapsed is typically <60s.

**Fail mode:**
- State stuck at `pending` for >60s → runner not claiming. Check Terminal A logs for "claim" / "BackupRunner".
- State transitions to `failed` → engine error. Run Q_jobs_by_backup and Q_steps_for_job to see why.
- `s3_location` never populates → `Done` event not being persisted. Stop.

### B5: Verify the dump is in S3

**Do** (Terminal C):
```bash
S3_LOC=$(curl -sS -H "Authorization: Bearer $TEMPS_TOKEN" \
    "$TEMPS_URL/api/backups/$BACKUP_ID" | jq -r .s3_location)
echo "S3 location: $S3_LOC"

# Parse and ls. The location format is usually 's3://bucket/path/file' or just 'path/file'.
# Adapt the following if your format differs:
BUCKET=$(echo "$S3_LOC" | sed -E 's|^s3://([^/]+)/.*|\1|')
KEY=$(echo "$S3_LOC" | sed -E 's|^s3://[^/]+/||')
aws s3 ls "s3://$BUCKET/$KEY" --human-readable
```

**Expect:** one object listed, size matches the `size_bytes` value (±10%).

**Fail mode:**
- `aws s3 ls` returns no object → engine reported success but didn't upload. Stop.
- Size 0 → empty dump. Indicates the dump step failed silently. Stop and check Q_steps_for_job.

### B6: Verify step trail

**Do** (Terminal B):
```sql
-- Q_steps_for_job, with $JOB_ID
SELECT step, state, occurred_at, durable_state, message
FROM backup_job_steps
WHERE job_id = :job_id
ORDER BY occurred_at ASC;
```

**Expect:** exactly 4 rows in this order:
1. `step='preflight'`, `state='completed'`
2. `step='pg_dumpall'`, `state='completed'`
3. `step='upload'`, `state='completed'`
4. `step='metadata'`, `state='completed'`

`occurred_at` strictly increases. `durable_state` is JSON (may be `{}` for early steps, may contain the S3 key after `upload`).

**Fail mode:**
- Fewer than 4 rows → engine emitted `Done` without emitting all `StepCompleted` events. Stop — engine bug.
- Out-of-order steps → runner persistence bug. Stop.
- A step has `state='failed'` but the job state is `completed` → state corruption. Stop.

**Phase B sign-off:** control-plane backup runs end-to-end via the runner; step trail is complete; artifact is in S3.

---

# Phase C — Each external engine (Redis, MongoDB, Postgres, S3 mirror)

**Goal:** smoke-test each engine actually works on a real service.

For each engine below, follow the same loop: create service → trigger backup → watch lifecycle → verify S3 → verify step count.

### Common loop

For each engine, you'll capture:
- `SVC_ID` — the external service id
- `BACKUP_ID` — the parent backup row
- `JOB_ID` — the `backup_jobs.id`

Use the helper in §"Pre-flight commands":
- Q_jobs_by_backup to find `JOB_ID`
- Q_steps_for_job to count steps
- `aws s3 ls` to confirm artifact exists

### C1: Redis (5 steps expected)

**Create service:**
```bash
bunx @temps-sdk/cli services create \
    --project $PROJECT_ID --type redis --name redis-c1 --version 7
# Capture id from output:
export SVC_ID=<id>
```

**Wait for healthy:**
```bash
bunx @temps-sdk/cli services show $SVC_ID --watch  # Ctrl-C once 'state: running'
```

**Trigger backup:**
```bash
curl -sS -X POST -H "Authorization: Bearer $TEMPS_TOKEN" \
    "$TEMPS_URL/api/backups/external-services/$SVC_ID/run" \
    | tee /tmp/c1-resp.json
export BACKUP_ID=$(jq -r .id /tmp/c1-resp.json)
```

**Expect:** 202 in <100ms, state `pending`.

**Watch to completion** (B4 polling loop).

**Verify** (B5 S3 check + Q_ext_backup_row).

**Step count:** Q_steps_for_job should show **5 rows**:
`preflight → trigger_bgsave → wait_for_rdb → upload_rdb → metadata`.

**Fail mode:** wrong step count → engine emitting/missing events. Stop and fix.

### C2: MongoDB (4 steps expected)

**Create service:**
```bash
bunx @temps-sdk/cli services create \
    --project $PROJECT_ID --type mongodb --name mongo-c2 --version 7
export SVC_ID=<id>
```

Wait for healthy, then trigger + poll + verify as in C1.

**Step count:** Q_steps_for_job should show **4 rows**:
`preflight → mongodump → upload → metadata`.

### C3: Postgres standalone, pg_dump engine (4 steps expected)

**Create service:**
```bash
bunx @temps-sdk/cli services create \
    --project $PROJECT_ID --type postgres --name pg-c3 --version 16
export SVC_ID=<id>
```

Wait for healthy, then verify the backup engine selected for this service is `postgres_pgdump`. Check via:

```bash
curl -sS -H "Authorization: Bearer $TEMPS_TOKEN" \
    "$TEMPS_URL/api/external-services/$SVC_ID" \
    | jq '{id, name, topology, backup_engine: (.backup_engine // "auto")}'
```

If you have a WAL-G-configured Postgres on staging, you can also exercise the `postgres_walg` engine — repeat with that service.

Trigger + poll + verify.

**Step count for `postgres_pgdump`:** **4 rows**:
`preflight → dump → upload → metadata`.

**Step count for `postgres_walg` (if exercised):** **4 rows**:
`preflight → walg_push → record_lsn → metadata`. The `record_lsn` step's `durable_state` JSON should contain a non-empty `lsn` field.

### C4: S3 mirror (3 steps expected)

Setup requires two S3 sources: a source-of-truth bucket and a destination. If staging only has one S3 source, create a second one targeting a test bucket. The S3 mirror "external service" is typically configured as an external service of type `s3_mirror` with both endpoints.

**Create service:**
```bash
bunx @temps-sdk/cli services create \
    --project $PROJECT_ID --type s3_mirror --name s3mirror-c4 \
    --param source_s3_source_id=<source-id> \
    --param dest_s3_source_id=<dest-id> \
    --param prefix=test-c4/
export SVC_ID=<id>
```

> If `s3_mirror` is not creatable via CLI, create via API:
> ```bash
> curl -sS -X POST -H "Authorization: Bearer $TEMPS_TOKEN" \
>     -H "Content-Type: application/json" \
>     "$TEMPS_URL/api/external-services" \
>     -d '{"project_id": '"$PROJECT_ID"', "service_type": "s3_mirror", "name": "s3mirror-c4", "params": {"source_s3_source_id": ..., "dest_s3_source_id": ..., "prefix": "test-c4/"}}'
> ```

Trigger + poll + verify.

**Step count:** **3 rows**: `list_source → sync → metadata`.

`sync` step's `durable_state` should contain the list of synced keys (or at least the last-synced cursor). Inspect:

```sql
SELECT durable_state FROM backup_job_steps
WHERE job_id = :job_id AND step = 'sync';
```

**Phase C sign-off:** each of Redis, MongoDB, Postgres-pgdump, (optionally Postgres-walg), and S3 mirror produced the expected step trail and uploaded an artifact.

---

# Phase D — Crash recovery (the part the ADR is built for)

**Goal:** prove a server restart mid-backup doesn't lose the work.

> **Caveat:** on a small control-plane DB, `pg_dumpall` may finish in <30s, well before the 5-minute lease TTL — so the resume code path won't fire. The killed runner will boot, the lease will eventually expire, and the job will resume. If you can't reproduce because `pg_dumpall` is too fast on the dev DB, see Phase E to use a bigger DB.

### D1: Start a control-plane backup, wait until `pg_dumpall` step is running

**Do** (Terminal C):
```bash
curl -sS -X POST -H "Authorization: Bearer $TEMPS_TOKEN" \
    "$TEMPS_URL/api/backups/s3-sources/$S3_SOURCE_ID/run" \
    | tee /tmp/d1-resp.json
export BACKUP_ID=$(jq -r .id /tmp/d1-resp.json)
```

**Watch** (Terminal B, refresh every second):
```sql
SELECT id, state, step, attempts, leased_until FROM backup_jobs
WHERE backup_id = :backup_id;
```

Wait until `step = 'pg_dumpall'` and `state = 'running'`.

### D2: Hard-kill the temps server

**Do** (Terminal A):
```bash
# Find the temps PID, kill -9 it
PID=$(systemctl show temps --property=MainPID --value)
echo "Killing $PID"
sudo kill -9 "$PID"
```

**Expect:** systemd will restart the service (assuming `Restart=always` in the unit). If it doesn't auto-restart, manually `sudo systemctl start temps`.

### D3: Watch for re-claim

**Do** (Terminal B, poll every 10s for up to 6 minutes):
```sql
-- Q_jobs_by_backup
SELECT id, state, step, attempts, claimed_by, leased_until, NOW() AS now_ts
FROM backup_jobs WHERE backup_id = :backup_id;
```

**Expect:**
- Immediately after the kill: row stays `state='running'` with the old `claimed_by`. The lease is still in the future (because the engine never extended past the moment it was killed).
- After `leased_until` passes (typically 5 min from `started_at` minus elapsed work time): the runner's claim poll picks the row up again. `attempts` increments to **2**. `claimed_by` updates to the current host.
- `step` may stay at `pg_dumpall` (the engine's idempotent resume should re-run the dump) or jump forward (if the engine persisted `pg_dumpall` as completed before the kill — depends on timing).

**Fail mode:**
- After 10 minutes the row is still stuck running with stale lease → runner isn't reclaiming. Stop and check the claim query / runner logs.
- `attempts` increments past 2 multiple times → engine keeps failing on resume. Stop and investigate the engine's resume logic.

### D4: Watch the job through to completion

Continue polling. Eventually:
- `state = 'completed'`
- `finished_at` is the time of the **second** attempt's completion, not the first.
- Q_steps_for_job shows step rows for **both** attempts (the `attempt` column distinguishes them).

### D5: Verify S3 artifact

Same as B5: confirm the object exists. Should be a single artifact (the resumed engine should not have left a partial upload behind, or if it did, the engine's `rollback`/idempotence should have cleaned it up).

**Fail mode:**
- Two S3 objects from the same run → idempotence broken. Stop.
- Zero objects despite `state=completed` → engine reported success but didn't upload. Stop.

**Phase D sign-off:** crash mid-backup, restart, resume, complete. One artifact. Two attempts visible in the audit trail.

---

# Phase E — Big-DB safety (the prod scenario, heartbeat fix)

**Goal:** prove the heartbeat fix (mpsc + `tokio::select!` in `step_pg_dumpall`) keeps the lease alive for backups that take longer than 5 minutes.

### E1: Identify or create a big database

You need a Postgres database where `pg_dumpall` takes **>5 minutes**. Options:

**Option 1 — use a real staging DB you know is big.** Note its external service ID as `BIG_PG_SVC_ID`.

**Option 2 — pgbench-populate a fresh Postgres service to ~15 GB:**

```bash
# Create a postgres service first
bunx @temps-sdk/cli services create \
    --project $PROJECT_ID --type postgres --name pg-bigtest --version 16
export BIG_PG_SVC_ID=<id>

# Wait for healthy, get connection details
bunx @temps-sdk/cli services show $BIG_PG_SVC_ID

# pgbench-populate with scale 1000 (~15 GB)
# Note: connect via the service's exposed port; the CLI may print a connection string.
PGPASSWORD=<pw> pgbench -h <host> -p <port> -U postgres -i -s 1000 postgres
# This takes ~10-30 minutes depending on the host.
```

### E2: Trigger and tail heartbeats

**Do** (Terminal A first, then Terminal C):

Terminal A:
```bash
sudo journalctl -u temps -f | grep -iE "Heartbeat|heartbeat|extend.*lease|job_id="
```

Terminal C:
```bash
curl -sS -X POST -H "Authorization: Bearer $TEMPS_TOKEN" \
    "$TEMPS_URL/api/backups/external-services/$BIG_PG_SVC_ID/run" \
    | tee /tmp/e2-resp.json
export BACKUP_ID=$(jq -r .id /tmp/e2-resp.json)
```

**Expect:** within 2-3 minutes of `pg_dumpall` starting, Terminal A shows recurring `Heartbeat` log lines for this job at roughly 2-minute intervals. Continue throughout the dump.

**Fail mode:**
- No `Heartbeat` log lines at all during a >5-min dump → heartbeat emission broken. The lease will expire and the job will be double-claimed. Stop.
- Heartbeat interval >5 min → too sparse. Will allow lease to expire. Stop.

### E3: Watch `leased_until` advance ahead of NOW()

**Do** (Terminal B, run every 30s during the dump):
```sql
SELECT id, state, step, attempts, leased_until, NOW() AS now_ts,
       leased_until - NOW() AS lease_remaining
FROM backup_jobs WHERE backup_id = :backup_id;
```

**Expect:** `lease_remaining` is always positive — typically between 2 and 5 minutes — and never goes negative while `pg_dumpall` is running. Each time a Heartbeat fires, the next sample of `leased_until` should be ~5 minutes ahead of NOW().

**Fail mode:**
- `lease_remaining` goes negative while the row is still `state='running'` → heartbeats aren't extending the lease. The stall sweeper will mark it failed shortly. Stop.

### E4: After completion, verify only one pg_dump container ran

**Do** (Terminal A, after `state='completed'`):
```bash
docker ps -a --filter "name=temps-pg-backup" --format "table {{.Names}}\t{{.Status}}\t{{.CreatedAt}}"
```

**Expect:** exactly one container with a name like `temps-pg-backup-<job_id>-<attempt>` (or similar), in `Exited (0)` state, created during your backup window.

**Fail mode:**
- Two containers from the same job → double-claim happened (lease expired and another runner picked it up). Stop and verify heartbeat behavior.
- Container in `Exited (non-zero)` state → dump failed. Cross-check with `error_message` in `backup_jobs`.

**Phase E sign-off:** a >5-minute pg_dumpall completes without lease expiry; only one sidecar container runs.

---

# Phase F — Concurrent backups

**Goal:** verify multiple engines run in parallel under the runner's concurrency cap.

### F1: Trigger 3 backups in rapid succession

**Do** (Terminal C):
```bash
# Pick three independent targets you've already set up:
# - control plane via S3 source
# - Redis service from C1
# - MongoDB service from C2
# Adjust IDs accordingly.

# Fire all three in parallel
(
    curl -sS -X POST -H "Authorization: Bearer $TEMPS_TOKEN" \
        "$TEMPS_URL/api/backups/s3-sources/$S3_SOURCE_ID/run" \
        | jq -r '"control_plane: \(.id)"' &
    curl -sS -X POST -H "Authorization: Bearer $TEMPS_TOKEN" \
        "$TEMPS_URL/api/backups/external-services/$REDIS_SVC_ID/run" \
        | jq -r '"redis: \(.id)"' &
    curl -sS -X POST -H "Authorization: Bearer $TEMPS_TOKEN" \
        "$TEMPS_URL/api/backups/external-services/$MONGO_SVC_ID/run" \
        | jq -r '"mongo: \(.id)"' &
    wait
) | tee /tmp/f1-ids.txt
```

Note the three `BACKUP_ID`s.

### F2: Confirm all three are running simultaneously

**Do** (Terminal B, within 10 seconds of F1):
```sql
-- Q_running_count
SELECT engine, COUNT(*) AS running
FROM backup_jobs
WHERE state = 'running'
GROUP BY engine
ORDER BY engine;
```

**Expect:** three rows, each engine showing `running=1`. All three jobs claimed within a single poll cycle. The runner's `MAX_CONCURRENT=4` default allows up to 4, so 3 in parallel is fine.

**Fail mode:**
- Only one engine in `running` state → runner is serializing. Stop and check `MAX_CONCURRENT` env and the claim-per-poll loop.
- Two engines running, one stuck pending → claim query is over-restrictive. Triage.

### F3: Wait for all three to complete

**Do** (Terminal C):
```bash
for BID in $(grep -oE '[0-9]+' /tmp/f1-ids.txt); do
    while true; do
        STATE=$(curl -sS -H "Authorization: Bearer $TEMPS_TOKEN" \
            "$TEMPS_URL/api/backups/$BID" | jq -r .state)
        echo "$BID: $STATE"
        [[ "$STATE" == "completed" || "$STATE" == "failed" ]] && break
        sleep 5
    done
done
```

**Expect:** all three end in `completed`. None interferes with the others. None ends in `failed` purely because of contention.

**Phase F sign-off:** runner handles 3 concurrent backups in parallel.

---

# Phase G — Failure paths

**Goal:** prove failed backups are visible with **useful** error messages, retried up to `max_attempts`, and leave behind enough breadcrumbs for an operator to clean up.

### G1: Cause a deliberate failure

Two ways. Pick one.

**Option G1a — bad S3 credentials.** Create a new S3 source with deliberately wrong access key:
```bash
bunx @temps-sdk/cli backups sources create \
    --name s3-bad-creds \
    --bucket some-bucket \
    --access-key AKIAINVALIDKEYFORTEST \
    --secret-key fakesecretkeythatdoesnotwork12345 \
    --region us-east-1
export BAD_S3_ID=<id>
```

Then trigger:
```bash
curl -sS -X POST -H "Authorization: Bearer $TEMPS_TOKEN" \
    "$TEMPS_URL/api/backups/s3-sources/$BAD_S3_ID/run" \
    | tee /tmp/g1-resp.json
export BACKUP_ID=$(jq -r .id /tmp/g1-resp.json)
```

**Option G1b — service container stopped.** Pick the Redis service from C1:
```bash
# Stop the underlying container without removing the service entity.
# Find the container:
docker ps --filter "name=temps-redis-c1" --format "{{.Names}}"
docker stop <name>
```

Then trigger a backup:
```bash
curl -sS -X POST -H "Authorization: Bearer $TEMPS_TOKEN" \
    "$TEMPS_URL/api/backups/external-services/$REDIS_SVC_ID/run" \
    | tee /tmp/g1-resp.json
export BACKUP_ID=$(jq -r .id /tmp/g1-resp.json)
```

### G2: Watch attempts increment up to max_attempts

**Do** (Terminal B, every minute):
```sql
SELECT id, state, attempts, max_attempts, error_message, next_attempt_at, finished_at
FROM backup_jobs
WHERE backup_id = :backup_id;
```

**Expect:**
- `attempts` increments from 1 → 2 → 3 with backoff delays roughly matching the ADR's schedule (1 min → 5 min → 20 min). Allow ±30 seconds slack.
- After the third failed attempt, `state` flips to `'failed'`.
- `error_message` is **not** the canned "Backup was in progress when the temps server restarted" — it should reflect the actual failure, like `S3 upload failed: InvalidAccessKeyId` or `connection refused: 127.0.0.1:6379`.

**Fail mode:**
- `error_message` is generic / placeholder → engine isn't surfacing real error context. Stop.
- `attempts` keeps incrementing past `max_attempts` → retry cap not enforced. Stop.
- Backoff is wrong (e.g., retries fire 1 second apart) → backoff calculation broken. Stop.

> **Note on test duration:** the third-attempt backoff is ~20 minutes. To shorten this test, before triggering G1 you can set `max_attempts=1` via the request body if the API supports it, or skip directly to verifying after the first failure.

### G3: Verify `finished_at` is real

**Do** (Terminal B):
```sql
SELECT id, started_at, finished_at, finished_at - started_at AS duration
FROM backup_jobs
WHERE backup_id = :backup_id AND state = 'failed';
```

**Expect:** `finished_at` is within seconds of the last retry attempt finishing. Not "now()" from some unrelated reconcile. `duration` is meaningful (not negative, not implausibly long).

**Cross-check** the parent `backups` row:
```sql
-- Q_backup_row
SELECT id, state, error_message, finished_at FROM backups WHERE id = :backup_id;
```

**Expect:** `state='failed'`, same `finished_at`, `error_message` reflects the same root cause (not the canned reconcile message).

### G4: Verify breadcrumbs for cleanup

**Do** (Terminal B):
```sql
-- Q_steps_for_job
SELECT step, state, message, durable_state FROM backup_job_steps
WHERE job_id = :job_id
ORDER BY occurred_at;
```

**Expect:** at least one step row showing **where** the failure happened. For G1a (bad creds), expect to see `preflight` or `upload` with `state='failed'` and a message containing the S3 error.

For G1a specifically, if the engine ever wrote a partial `durable_state` containing a destination key, an operator should be able to read it and know where to look for orphan objects:
```sql
SELECT step, durable_state FROM backup_job_steps WHERE job_id = :job_id;
```

**Phase G sign-off:** failures are real (not fake), reach `max_attempts`, expose useful errors, and leave debuggable state behind.

**Cleanup (G1b):** restart the Redis container you stopped:
```bash
docker start <name>
```

---

# Phase H — Stopgap belt-and-suspenders still working

**Goal:** verify the stall sweeper and boot reconcile still behave correctly. These are kept in place until Phase 5 decommission.

### H1: Stall sweeper marks stale heartbeat as failed within 60s

**Do** — create a stuck `running` row by hand.

Option 1: trigger any backup that completes, then manually set it back to running with a stale heartbeat (most reliable for the test):
```bash
# Trigger a backup, let it complete
curl -sS -X POST -H "Authorization: Bearer $TEMPS_TOKEN" \
    "$TEMPS_URL/api/backups/external-services/$REDIS_SVC_ID/run" \
    | jq .
# Capture the BACKUP_ID, wait until state=completed.
export BACKUP_ID=<id>
```

Then (Terminal B):
```sql
-- Force the row back to running with stale heartbeat
UPDATE backups
SET state = 'running',
    last_heartbeat_at = NOW() - interval '15 minutes',
    finished_at = NULL,
    error_message = NULL
WHERE id = :backup_id;
```

**Watch** (Terminal B, every 20 seconds for up to 2 minutes):
```sql
SELECT id, state, error_message, finished_at, last_heartbeat_at
FROM backups WHERE id = :backup_id;
```

**Expect:** within 60-90 seconds, the stall sweeper flips the row to `state='failed'` with a distinguishable error message (something like "Backup marked failed by stall sweeper: no heartbeat for ..." — not the generic ORPHAN_REASON string from boot reconcile). `finished_at` is populated near `NOW()`.

**Fail mode:**
- Row stays `running` after 2 minutes → sweeper not running. Check Terminal A logs for "stall sweeper" / "backup sweep".
- Error message is the boot-reconcile canned string → sweeper not differentiating. Stop.

### H2: Boot reconcile still marks orphans on restart

**Do** — create another stuck running row, then restart the server before the stall sweeper fires:

```sql
UPDATE backups
SET state = 'running',
    last_heartbeat_at = NOW() - interval '15 minutes',
    finished_at = NULL,
    error_message = NULL
WHERE id = :backup_id;  -- reuse the row from H1 or pick another
```

Immediately (Terminal A):
```bash
sudo systemctl restart temps
sudo journalctl -u temps -n 100 --no-pager | grep -iE "reconcile|orphan"
```

**Expect:** a log line indicating reconcile ran and marked N rows failed. The row in question now has `state='failed'` with `finished_at` near the **reconcile event time**, error message distinguishable from the sweeper's (e.g., the message indicates boot-time reconcile specifically).

**Critical check** — `finished_at` should be **near the heartbeat staleness boundary**, not just "right now". The reconcile rewrite uses `last_heartbeat_at` to estimate when work stopped:
```sql
SELECT id, finished_at, last_heartbeat_at,
       finished_at - last_heartbeat_at AS gap
FROM backups WHERE id = :backup_id;
```

**Expect:** `gap` is short (single-digit seconds to a couple of minutes), not 15+ minutes like the old prod incident.

**Fail mode:**
- `finished_at` is exactly "now" with no relation to `last_heartbeat_at` → reconcile rewrite regressed; you're back to the prod bug. Stop.
- No reconcile log line on boot → reconcile not running. Stop.

**Phase H sign-off:** stall sweeper kills stale running rows within 60s; boot reconcile kills survivors with believable `finished_at`.

---

# Phase I — Scheduler still works (legacy path coexistence)

**Goal:** scheduled backups still fire under the unchanged scheduler, even with the runner flag on. They go through the **old synchronous path** (per the "What was NOT shipped" note in the brief).

### I1: Create a fast-firing schedule

**Do** (Terminal C):
```bash
bunx @temps-sdk/cli backups schedules create \
    --service $REDIS_SVC_ID \
    --cron '*/5 * * * *' \
    --name redis-i1-test
# Capture id
export SCHED_ID=<id>
```

Note the current minute. Wait for the next 5-minute boundary.

### I2: Wait one tick, verify a backup fired

**Do** (Terminal B, immediately after the boundary):
```sql
-- Q_schedules_recent
SELECT id, external_service_id, cron_schedule, last_run, next_run, last_job_id
FROM backup_schedules
WHERE id = :sched_id;
```

**Expect:** `last_run` updated to roughly the boundary time. `last_job_id` may be NULL (because legacy path doesn't write `backup_jobs`).

**Then** verify a new `backups` row exists for that service:
```sql
SELECT id, state, started_at, finished_at, s3_location
FROM backups
WHERE created_at > NOW() - interval '10 minutes'
ORDER BY created_at DESC
LIMIT 5;
```

**Expect:** a row for the Redis service, `state='completed'`, populated `s3_location`.

### I3: Confirm no `backup_jobs` row was created for this schedule

**Do** (Terminal B):
```sql
-- Q_jobs_recent
SELECT id, backup_id, engine, state, created_at
FROM backup_jobs
WHERE created_at > NOW() - interval '10 minutes'
ORDER BY created_at DESC;
```

**Expect:** **zero rows** corresponding to the scheduled fire (or only rows from manual backups you triggered separately). The scheduler is on the legacy code path; it does not enqueue.

**Fail mode:**
- A `backup_jobs` row appeared for the scheduled run → scheduler was already rewritten unexpectedly. Confirm against the codebase; the brief states this is out of scope.
- No new `backups` row → scheduler didn't fire. Check Terminal A for "scheduler" logs.

**Cleanup:**
```bash
bunx @temps-sdk/cli backups schedules delete $SCHED_ID
```

**Phase I sign-off:** scheduled backups still fire and complete via the legacy path. Manual and scheduled paths coexist.

---

# Phase J — Rollback (flag flip back to OFF)

**Goal:** prove the flag is safe to flip off in prod if something goes wrong.

### J1: Set flag to false, restart

**Do** (Terminal A on **staging** first):
```bash
sudo systemctl edit temps
# Change TEMPS_BACKUP_RUNNER_ENABLED to false (or remove the line entirely)
sudo systemctl restart temps
sudo journalctl -u temps -n 100 --no-pager | grep -iE "BackupRunner|engines"
```

**Expect:** "BackupRunner disabled" log line again. No "Engines registered" line.

### J2: Manual backup goes through legacy synchronous path

**Do** (Terminal C):
```bash
time curl -sS -X POST -H "Authorization: Bearer $TEMPS_TOKEN" \
    "$TEMPS_URL/api/backups/s3-sources/$S3_SOURCE_ID/run" \
    -w '\nHTTP %{http_code}\n'
```

**Expect:**
- HTTP 200 (not 202).
- `time` shows multi-second blocking call.
- Response shows `"state": "completed"`.

This proves the handler's flag check works and the legacy path is intact.

**Fail mode:**
- Returns 202 quickly → flag didn't take effect on the handler. Stop.

### J3: In-flight `backup_jobs` rows become stranded

**Do** (Terminal B):
```sql
SELECT id, backup_id, state, leased_until, claimed_by
FROM backup_jobs
WHERE state IN ('pending', 'running')
ORDER BY created_at DESC;
```

**Expect:** any rows that were `state='pending'` or `state='running'` at the moment of restart are now **stranded** — there's no runner to claim or extend their lease. They will sit indefinitely.

**Document this:** rolling back the flag means operators must manually clean up stranded `backup_jobs` rows:

```sql
-- Manual cleanup of stranded rows
UPDATE backup_jobs
SET state = 'cancelled',
    error_message = 'Cancelled during runner disable (operator cleanup)',
    finished_at = NOW()
WHERE state IN ('pending', 'running');
```

Also fix any `backups` parent rows still in `state='running'`:
```sql
UPDATE backups
SET state = 'failed',
    error_message = 'Failed during runner disable (operator cleanup)',
    finished_at = NOW()
WHERE state = 'running'
  AND id IN (SELECT backup_id FROM backup_jobs WHERE state = 'cancelled' AND finished_at > NOW() - interval '5 minutes');
```

**Phase J sign-off:** flag flips off cleanly; legacy path works; documented cleanup procedure for stranded jobs.

---

# Prod cutover (after staging passes Phases A–J)

Only proceed when **every** phase on staging has a green checkmark in the sign-off below.

1. **Schedule a maintenance window** — Phases B and E will produce real traffic against prod's S3 bucket. Phase G will deliberately create failed rows; you may want to skip G in prod, or run it against a throwaway target.

2. **Phase A only**, in prod, to baseline:
   - Confirm `TEMPS_BACKUP_RUNNER_ENABLED` is unset.
   - Trigger one manual control-plane backup; confirm it completes synchronously.
   - Confirm `backup_jobs` is empty.

3. **Flip flag to true** in prod (Phase B1 procedure on the prod host).

4. **Run Phases B, C, F, H** against prod with extra care:
   - Use a non-critical service for Phase C if you don't want to back up prod data on demand.
   - Phase F (3 concurrent) is safe; will produce real artifacts.
   - Phase H (stall sweeper) requires you to manually corrupt a row — only do this on a backup row you can afford to lose.

5. **Skip Phase D** in prod (or run only if you have a non-prod data target). Hard-killing prod is high-risk.

6. **Skip Phase E** in prod if you don't have a big-DB target you're willing to back up. The fix is already validated on staging.

7. **Skip Phase G** in prod unless you have a dedicated throwaway S3 source for failure tests.

8. **Run Phase I** in prod to confirm scheduler unaffected.

9. **Have Phase J ready to execute** as the rollback if anything looks off in the first 24-48 hours.

---

# Sign-off Checklist

| Phase | Step | Pass? | Notes |
|---|---|---|---|
| A | A1 — cargo check clean | ☐ | |
| A | A2 — flag confirmed off | ☐ | |
| A | A3 — control-plane sync backup | ☐ | |
| A | A4 — external-service sync backup | ☐ | |
| A | A5 — backup_jobs empty | ☐ | |
| B | B1 — runner starts, 7 engines registered | ☐ | |
| B | B2 — 202 in <100ms | ☐ | |
| B | B3 — backup_jobs row appears immediately | ☐ | |
| B | B4 — state progresses to completed | ☐ | |
| B | B5 — S3 object exists | ☐ | |
| B | B6 — 4 step rows in order | ☐ | |
| C | C1 — Redis (5 steps) | ☐ | |
| C | C2 — MongoDB (4 steps) | ☐ | |
| C | C3 — Postgres pgdump (4 steps) | ☐ | |
| C | C3 — Postgres walg (optional) | ☐ | |
| C | C4 — S3 mirror (3 steps) | ☐ | |
| D | D1–D5 — crash recovery, resume from lease expiry | ☐ | |
| E | E1–E4 — big-DB heartbeat keeps lease alive | ☐ | |
| F | F1–F3 — 3 concurrent backups all complete | ☐ | |
| G | G1–G4 — failure path with real error message | ☐ | |
| H | H1 — stall sweeper kills stale row within 60s | ☐ | |
| H | H2 — boot reconcile uses last_heartbeat_at for finished_at | ☐ | |
| I | I1–I3 — scheduler fires via legacy path | ☐ | |
| J | J1–J3 — rollback flag off, legacy path resumes | ☐ | |

**Tester:** ______________________  **Date:** ____________  **Environment:** ☐ Staging  ☐ Prod

**Notes / observed defects:**

---

# Troubleshooting — what to do if X breaks

### "B2 returns 500"

Check Terminal A logs for the request. Likely causes:
- Engine registration partial — one engine's `register_services` failed silently. Check plugin.rs startup.
- DB migration didn't apply — `backup_jobs` table missing. Run `cargo run --bin temps -- migrate` (or whatever the migration entrypoint is).
- Handler can't get a DB connection. Cross-check `TEMPS_DATABASE_URL`.

### "The runner doesn't start (no `BackupRunner enabled` log)"

- Confirm env var: `sudo systemctl show temps --property=Environment | tr ' ' '\n' | grep BACKUP`.
- Confirm parse: the code reads `TEMPS_BACKUP_RUNNER_ENABLED` as a string; `"true"` (lowercase) is required. `TRUE`, `1`, or `yes` may not be accepted depending on the implementation. If unsure, use `true` exactly.
- Confirm the plugin runs `runner.start()`. If a panic happens during engine registration, the runner spawn may be skipped. Search logs for `panicked at` or `register_services`.

### "A job sits at `pending` forever"

- Confirm the runner is up: Terminal A logs should show periodic poll activity. If not, the spawn was skipped.
- Confirm the claim query isn't filtering everything out: check `next_attempt_at` is in the past (`SELECT next_attempt_at, NOW() FROM backup_jobs WHERE id = ...`).
- Check the per-engine concurrency cap. If another job of the same engine is already `running`, the new one waits.

### "A job sits at `running` forever with stale lease"

- Within ~5 minutes of `leased_until` passing, the runner's next poll should re-claim it (lease expiry reclaim is built into the claim query per ADR-014). If it doesn't, the union query in the claim isn't matching. Inspect the actual SQL the runner issues.
- The stall sweeper (Phase H stopgap) should also flip it to `failed` within 60s of stale heartbeat. If neither runs, both the runner and the sweeper are dead. Check process is alive: `systemctl status temps`.

### "Step trail (Q_steps_for_job) is missing rows"

- The engine likely emitted `Done` without emitting all expected `StepCompleted` events. Engine bug — look at the relevant `engines/<name>.rs` file.
- Step persistence transaction rolled back due to `claim_token` mismatch — means another runner re-claimed the row (lease expired mid-step). Rare; correlates with no heartbeats. Cross-check Phase E behavior.

### "`finished_at` is wildly wrong (the prod bug)"

- If on a runner-driven job (`backup_jobs` row exists): runner persistence bug. The `Done` event should stamp `finished_at = NOW()` atomically. Stop and inspect `mark_job_completed` / `mark_job_failed`.
- If on a stopgap path (reconcile or sweeper): the reconcile rewrite should use `last_heartbeat_at` as a proxy. Cross-reference Q_backup_row's `finished_at - last_heartbeat_at` gap. If it's hours, the rewrite regressed.

### "Two S3 objects from a single backup (Phase D)"

- Engine isn't idempotent at the resume step. The engine's `upload` step (or its equivalent) must check whether the destination key already exists and skip / overwrite deterministically.
- Or: the engine's `rollback` isn't being called on max-attempts failure. Inspect.

### "Cargo check fails after `git pull`"

- Likely a crate dependency that wasn't added to `Cargo.toml`. Run `cargo check --lib --workspace` and read the missing-crate errors.
- Or: a Sea-ORM entity for `backup_jobs` / `backup_job_steps` is stale. Regenerate or align with the migration schema.

### "Phase E heartbeats are happening but `leased_until` doesn't advance"

- The Heartbeat-to-lease-extension wire might be broken. Each `Heartbeat` event in the engine stream must trigger a `lease_extend` UPDATE in the runner. Inspect the runner's `for event in stream` match arm.

### "Phase I — scheduled backup fired, but didn't complete"

- Out of scope per the brief, but: the scheduler may be hitting a stopgap path that's been partially rewritten. If the scheduled run inserted into `backup_jobs` (when the brief says it shouldn't), the partial Phase 3 rewrite leaked in.

### General — "What logs do I need to file a bug?"

Capture all of:
- `sudo journalctl -u temps --since '30 minutes ago' > /tmp/temps.log`
- `psql "$TEMPS_PG_STAGING" -c "\copy (SELECT * FROM backup_jobs WHERE created_at > NOW() - interval '1 hour') TO '/tmp/jobs.csv' CSV HEADER"`
- `psql "$TEMPS_PG_STAGING" -c "\copy (SELECT s.* FROM backup_job_steps s JOIN backup_jobs j ON s.job_id = j.id WHERE j.created_at > NOW() - interval '1 hour') TO '/tmp/steps.csv' CSV HEADER"`
- `psql "$TEMPS_PG_STAGING" -c "\copy (SELECT * FROM backups WHERE created_at > NOW() - interval '1 hour') TO '/tmp/backups.csv' CSV HEADER"`
- The exact `BACKUP_ID` / `JOB_ID` involved.
- Output of `docker ps -a --filter "name=temps-" --format json | head -50`.

Attach those to the bug report. Most regressions are findable from those four artifacts.

---

## Hardening Addendum — May 2026 Incident Response

> Three concurrent WAL-G jobs for the same Postgres service hung for 14+ minutes with zero stderr visibility. Four fixes shipped as part of the post-incident hardening PR.

### Fix 1 — Streaming stderr capture (all 7 engines)

**What changed:** All engine exec calls switched from `attach_stderr: false` / `detach: true` (fire-and-forget) to `attach_stdout: true` / `attach_stderr: true` / `detach: false` (attached streaming). A `RingBuffer` helper caps captured output at 64 KB per stream (stdout + stderr separately) to prevent OOM on verbose WAL-G output.

**Validation steps:**

1. Trigger a `run_external_service_backup` for a Postgres service that has WAL-G configured.
2. Confirm the backup reaches `state='running'`.
3. Intentionally break the WAL-G environment (e.g., set a bad `WALG_S3_PREFIX`) and re-run.
4. When the backup fails, verify the error message in `backup_jobs.error_message` and `backups.error_message` contains actual stderr from the `wal-g backup-push` process, not an empty string.

```sql
SELECT id, state, error_message FROM backup_jobs ORDER BY id DESC LIMIT 5;
SELECT id, state, error_message FROM backups ORDER BY id DESC LIMIT 5;
```

Expected: `error_message` should contain something like `"ERROR: write error ... AccessDenied"` rather than `""`.

### Fix 2 — Per-target concurrency guard (HTTP 409)

**What changed:** `BackupRunner::enqueue_job` runs a pre-INSERT `SELECT` to detect any `pending` or `running` job for the same `(engine, target_kind, target_id)`. If found, returns `BackupRunnerError::AlreadyInFlight` which the handler maps to HTTP 409 Conflict.

**Validation steps:**

1. Start a backup for a Postgres service:
   ```bash
   curl -s -X POST "$TEMPS_URL/api/backups/external-services/$SERVICE_ID/run" \
     -H "Authorization: Bearer $TEMPS_TOKEN" -H "Content-Type: application/json" \
     -d '{"backup_type":"full"}' | jq .
   ```
   Note the returned `backup_id`.

2. Immediately try to start another backup for the **same service** while the first is still `running` or `pending`:
   ```bash
   curl -s -X POST "$TEMPS_URL/api/backups/external-services/$SERVICE_ID/run" \
     -H "Authorization: Bearer $TEMPS_TOKEN" -H "Content-Type: application/json" \
     -d '{"backup_type":"full"}' | jq .
   ```

3. Verify the second request returns HTTP 409 with a body like:
   ```json
   {
     "status": 409,
     "title": "Backup Already In Flight",
     "detail": "A postgres_walg backup is already in flight for target 42; refusing to enqueue a duplicate (existing job id: 99)"
   }
   ```

4. Confirm only ONE `backup_jobs` row was inserted:
   ```sql
   SELECT id, engine, target_id, state FROM backup_jobs
   WHERE target_id = $SERVICE_ID AND state IN ('pending','running')
   ORDER BY id;
   ```

### Fix 3 — 30-minute wall-clock timeout

**What changed:** `BackupRunner::dispatch` wraps the engine stream loop in a `tokio::select!` with a pinned 30-minute `tokio::time::sleep`. If the engine takes longer, the job is forcibly marked `failed` and the engine's `CancellationToken` is cancelled.

**Validation steps:**

In a non-production environment, lower `DEFAULT_JOB_MAX_RUNTIME` to 5 seconds and trigger a backup against a service that will stall (e.g., unresponsive Docker exec). After 5 seconds, verify:

```sql
SELECT state, error_message FROM backup_jobs ORDER BY id DESC LIMIT 1;
```

Expected `error_message`: `"Job exceeded wall-clock timeout of ... seconds; automatically failed to prevent indefinite execution."`

In staging with real services, verify no job stays `state='running'` for more than 31 minutes:

```sql
SELECT id, engine, state, created_at, age(NOW(), created_at) AS age
FROM backup_jobs
WHERE state = 'running' AND created_at < NOW() - interval '31 minutes';
```

Expected: zero rows.

### Fix 4 — `external_service` field on `GET /backups/{id}`

**What changed:** `GET /backups/{id}` now includes an `external_service` object when the backup was produced for an external service (Redis, Postgres, MongoDB). Control-plane backups return `null`.

**Validation steps:**

1. Find a backup produced for an external service:
   ```bash
   curl -s "$TEMPS_URL/api/backups/$BACKUP_ID" \
     -H "Authorization: Bearer $TEMPS_TOKEN" | jq .external_service
   ```

2. Verify the response includes:
   ```json
   {
     "id": 42,
     "name": "postgres-prod",
     "service_type": "postgres"
   }
   ```

3. Find a control-plane backup (from `run_backup_for_source`) and verify the field is absent (not `null` in JSON — it is omitted entirely due to `skip_serializing_if`):
   ```bash
   curl -s "$TEMPS_URL/api/backups/$CTRL_BACKUP_ID" \
     -H "Authorization: Bearer $TEMPS_TOKEN" | jq 'has("external_service")'
   ```
   Expected: `false`.
