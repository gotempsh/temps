#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -euo pipefail

if [[ "${TEMPS_BACKUP_BENCH_ACK:-}" != "1" ]]; then
  echo "Refusing to allocate a 50+ GB benchmark database without TEMPS_BACKUP_BENCH_ACK=1" >&2
  exit 2
fi

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
COMPOSE=(docker compose --project-directory "$SCRIPT_DIR" -f "$SCRIPT_DIR/docker-compose.yml")
FIRST_ROWS=${TEMPS_BENCH_FIRST_ROWS:-100000000}
FINAL_ROWS=${TEMPS_BENCH_FINAL_ROWS:-200000000}
BATCH_ROWS=${TEMPS_BENCH_BATCH_ROWS:-5000000}
SKIP_FIRST=${TEMPS_BENCH_SKIP_FIRST:-0}
REPORT_DIR=${TEMPS_BENCH_REPORT_DIR:-"/tmp/temps-backup-bench-$(date -u +%Y%m%dT%H%M%SZ)"}
NETWORK=temps-backup-bench_default
PG_CONTAINER=temps-backup-bench-postgres-1
RUSTFS_CONTAINER=temps-backup-bench-rustfs-1
MC_IMAGE=minio/mc:latest
PG_IMAGE=gotempsh/postgres-walg:18-bookworm
S3_PREFIX=s3://backups/postgres/walg
ACTIVE_BACKUP=0
mkdir -p "$REPORT_DIR"

stop_interrupted_backup() {
  local status=$?
  trap - EXIT INT TERM
  if (( ACTIVE_BACKUP == 1 )); then
    # Killing the local `docker exec` client does not necessarily terminate
    # WAL-G inside the container. Stop PostgreSQL cleanly so an interrupted
    # benchmark cannot leave a base backup running in the background.
    docker stop --time 30 "$PG_CONTAINER" >/dev/null 2>&1 || true
  fi
  exit "$status"
}
trap stop_interrupted_backup EXIT INT TERM

log() {
  printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" | tee -a "$REPORT_DIR/run.log"
}

pg_exec() {
  docker exec "$PG_CONTAINER" psql -v ON_ERROR_STOP=1 -U postgres -d bench "$@"
}

mc() {
  docker run --rm --network "$NETWORK" --entrypoint /bin/sh "$MC_IMAGE" -ceu \
    "mc alias set bench http://rustfs:9000 tempsbench tempsbench-local-only-secret >/dev/null; $*"
}

walg_env=(
  "WALG_S3_PREFIX=$S3_PREFIX"
  "AWS_ENDPOINT=http://rustfs:9000"
  "AWS_S3_FORCE_PATH_STYLE=true"
  "AWS_ACCESS_KEY_ID=tempsbench"
  "AWS_SECRET_ACCESS_KEY=tempsbench-local-only-secret"
  "AWS_REGION=auto"
  "PGHOST=/var/run/postgresql"
  "PGUSER=postgres"
  "PGPASSWORD=tempsbench"
  "PGDATABASE=bench"
  "WALG_COMPRESSION_METHOD=lz4"
  "WALG_UPLOAD_CONCURRENCY=4"
  "WALG_UPLOAD_DISK_CONCURRENCY=1"
  "WALG_UPLOAD_QUEUE=2"
  "WALG_TAR_SIZE_THRESHOLD=134217728"
)

wait_for_postgres() {
  for _ in $(seq 1 120); do
    if docker exec "$1" pg_isready -U postgres -d bench >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  echo "PostgreSQL container $1 did not become ready" >&2
  return 1
}

current_rows() {
  pg_exec -Atc "SELECT COALESCE(max(id), 0) FROM bench_events"
}

grow_to() {
  local target=$1 current next
  current=$(current_rows)
  while (( current < target )); do
    next=$((current + BATCH_ROWS))
    (( next > target )) && next=$target
    log "inserting rows $((current + 1))..$next"
    pg_exec -c "INSERT INTO bench_events(id, payload, created_at) SELECT g, repeat(md5(g::text), 12), clock_timestamp() FROM generate_series($((current + 1)), $next) AS g"
    current=$next
  done
  pg_exec -c "CHECKPOINT"
}

measure_database() {
  local label=$1
  pg_exec -Atc "SELECT json_build_object('label','$label','rows',(SELECT count(*) FROM bench_events),'database_bytes',pg_database_size(current_database()),'table_bytes',pg_total_relation_size('bench_events'),'data_bytes',pg_relation_size('bench_events'),'index_bytes',pg_indexes_size('bench_events'))" \
    | tee "$REPORT_DIR/$label-database.json"
}

repository_bytes() {
  mc "mc du --json --recursive bench/backups/postgres/walg 2>/dev/null | tail -n 1" \
    | sed -n 's/.*"size":\([0-9][0-9]*\).*/\1/p' \
    | tail -n 1
}

backup_checkpoint() {
  local label=$1
  local backup_id="temps-bench-$label-$(date -u +%s)"
  local started finished pid status max_tmp=0 observed_growth=0 previous_bytes=0 repo_bytes=0 temp_bytes=0
  started=$(date +%s)
  previous_bytes=$(repository_bytes || true)
  previous_bytes=${previous_bytes:-0}
  log "starting WAL-G backup $label with identity $backup_id"

  local -a exec_args=(docker exec)
  local variable
  for variable in "${walg_env[@]}"; do
    exec_args+=(--env "$variable")
  done
  exec_args+=(
    --env "WALG_SENTINEL_USER_DATA={\"temps_backup_id\":\"$backup_id\"}"
    "$PG_CONTAINER" sh -ceu 'exec wal-g backup-push "$PGDATA"'
  )
  "${exec_args[@]}" \
    >"$REPORT_DIR/$label-backup.stdout" 2>"$REPORT_DIR/$label-backup.stderr" &
  pid=$!
  ACTIVE_BACKUP=1

  while kill -0 "$pid" >/dev/null 2>&1; do
    temp_bytes=$(docker exec "$PG_CONTAINER" sh -ceu 'du -sb /tmp /var/tmp 2>/dev/null | awk "{s+=\$1} END {print s+0}"' || echo 0)
    (( temp_bytes > max_tmp )) && max_tmp=$temp_bytes
    repo_bytes=$(repository_bytes || true)
    repo_bytes=${repo_bytes:-0}
    if (( repo_bytes > previous_bytes )); then observed_growth=1; fi
    printf '%s,%s,%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$temp_bytes" "$repo_bytes" >> "$REPORT_DIR/$label-stream-samples.csv"
    sleep 2
  done
  wait "$pid" || status=$?
  ACTIVE_BACKUP=0
  status=${status:-0}
  finished=$(date +%s)
  if (( status != 0 )); then
    log "WAL-G backup $label failed with status $status"
    return "$status"
  fi
  repo_bytes=$(repository_bytes || true)
  repo_bytes=${repo_bytes:-0}
  cat > "$REPORT_DIR/$label-backup.json" <<JSON
{"label":"$label","elapsed_seconds":$((finished-started)),"repository_bytes":$repo_bytes,"peak_container_temp_bytes":$max_tmp,"object_store_grew_while_source_running":$observed_growth,"backup_identity":"$backup_id"}
JSON
  log "completed WAL-G backup $label in $((finished-started))s, repository=$repo_bytes bytes, peak temp=$max_tmp bytes"
}

restore_checkpoint() {
  local label=$1 expected_rows=$2
  local restore_name="temps-backup-bench-restore-$label"
  local restore_volume="$restore_name"
  local started finished result
  docker rm --force "$restore_name" >/dev/null 2>&1 || true
  docker volume rm "$restore_volume" >/dev/null 2>&1 || true
  docker volume create "$restore_volume" >/dev/null
  started=$(date +%s)
  log "fetching WAL-G backup $label into fresh volume $restore_volume"
  local -a fetch_args=(
    docker run --rm --network "$NETWORK" --user postgres
    -v "$restore_volume:/var/lib/postgresql"
  )
  local variable
  for variable in "${walg_env[@]}"; do
    fetch_args+=(--env "$variable")
  done
  fetch_args+=(
    "$PG_IMAGE" sh -ceu
    'rm -rf "$PGDATA"; mkdir -p "$PGDATA"; chmod 700 "$PGDATA"; wal-g backup-fetch "$PGDATA" LATEST; touch "$PGDATA/recovery.signal"'
  )
  "${fetch_args[@]}"
  local -a start_args=(docker run -d --name "$restore_name" --network "$NETWORK"
    -v "$restore_volume:/var/lib/postgresql" \
    -e POSTGRES_PASSWORD=tempsbench -e POSTGRES_DB=bench)
  for variable in "${walg_env[@]}"; do
    start_args+=(--env "$variable")
  done
  start_args+=(
    "$PG_IMAGE" postgres
    -c synchronous_commit=off
    -c archive_mode=off
    -c recovery_target_timeline=current
    -c 'restore_command=wal-g wal-fetch %f %p'
  )
  "${start_args[@]}" >/dev/null
  wait_for_postgres "$restore_name"
  result=$(docker exec "$restore_name" psql -v ON_ERROR_STOP=1 -U postgres -d bench -Atc \
    "SELECT json_build_object('rows',count(*),'min_id',min(id),'max_id',max(id)) FROM bench_events")
  finished=$(date +%s)
  printf '%s\n' "$result" | tee "$REPORT_DIR/$label-restore.json"
  local restored_rows
  restored_rows=$(printf '%s' "$result" | sed -n 's/.*"rows"[ ]*:[ ]*\([0-9][0-9]*\).*/\1/p')
  if [[ "$restored_rows" != "$expected_rows" ]]; then
    log "restore verification failed for $label: expected $expected_rows rows, got ${restored_rows:-unparsable}"
    return 1
  fi
  log "restore verification passed for $label in $((finished-started))s with $restored_rows rows"
  docker rm --force "$restore_name" >/dev/null
  docker volume rm "$restore_volume" >/dev/null
}

log "report directory: $REPORT_DIR"
"${COMPOSE[@]}" up -d
wait_for_postgres "$PG_CONTAINER"
mc "mc mb --ignore-existing bench/backups >/dev/null"
pg_exec -c "CREATE TABLE IF NOT EXISTS bench_events (id bigint PRIMARY KEY, payload text NOT NULL, created_at timestamptz NOT NULL) WITH (fillfactor=100)"

if [[ "$SKIP_FIRST" != "1" ]]; then
  grow_to "$FIRST_ROWS"
  measure_database 100m
  backup_checkpoint 100m
  restore_checkpoint 100m "$FIRST_ROWS"
fi

grow_to "$FINAL_ROWS"
measure_database 200m
backup_checkpoint 200m
restore_checkpoint 200m "$FINAL_ROWS"

log "benchmark complete; evidence is in $REPORT_DIR"
