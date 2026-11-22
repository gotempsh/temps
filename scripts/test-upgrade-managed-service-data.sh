#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Managed-service data across an upgrade (#1415).
#
# A previous release creates managed PostgreSQL services holding data; the
# candidate takes over the same database and Docker daemon. Then every
# operation that can replace a service's container is run, and after each
# one the data must still be there -- same rows, same cluster (pg_control
# system identifier):
#
#   1. OLD_BIN creates service "keep" (a normal managed service) and service
#      "shape" whose container is then replaced by one named like a managed
#      container but keeping its cluster on its own anonymous volume -- the
#      shape that lost all data on "Run backup" in #1415. Both are seeded.
#   2. NEW_BIN starts on the same database. Both services' data is intact.
#   3. "Run backup" on "keep" (WAL archiving off): the preflight reports a
#      restart, the run is refused (409) without confirm_restart, and with it
#      the backup completes, archive_mode is on, and the data is the same
#      cluster.
#   4. "Run backup" on "shape": the backup fails, nothing is pushed to S3,
#      and the original container still runs with its data.
#   5. Stop + start, a /dev/shm change (which recreates the container), and
#      an image change within the same major version on "keep": data intact
#      after each, and the image change keeps the /dev/shm size.
#
# Required environment:
#   OLD_BIN, NEW_BIN    previous release and candidate `temps` binaries
#   PG_CONTAINER        Docker container running Temps' own TimescaleDB server
# Optional environment (defaults in brackets):
#   PG_HOST [127.0.0.1]  PG_PORT [5432]  PG_USER [temps]  PG_PASSWORD [temps]
#   DB_NAME [unique run name]  WORK_DIR [mktemp -d]  HTTP_PORT [18790]
#   CONSOLE_PORT [18791]  TLS_PORT [18793]  S3_PORT [18794]
#   UPGRADE_IMAGE [gotempsh/pgvector-walg:pg18]  KEEP_DATABASES [unset]
#
# Every Docker object it creates carries the run id and is removed on exit;
# it never touches anything else.

set -Eeuo pipefail

: "${OLD_BIN:?OLD_BIN must point at the previous release binary}"
: "${NEW_BIN:?NEW_BIN must point at the candidate binary}"
: "${PG_CONTAINER:?PG_CONTAINER must name the database container}"

RUN_ID="$(date +%s)$$"
PG_HOST=${PG_HOST:-127.0.0.1}
PG_PORT=${PG_PORT:-5432}
PG_USER=${PG_USER:-temps}
PG_PASSWORD=${PG_PASSWORD:-temps}
DB_NAME=${DB_NAME:-temps_svcdata_${RUN_ID}}
WORK_DIR=${WORK_DIR:-$(mktemp -d)}
HTTP_PORT=${HTTP_PORT:-18790}
CONSOLE_PORT=${CONSOLE_PORT:-18791}
TLS_PORT=${TLS_PORT:-18793}
S3_PORT=${S3_PORT:-18794}
UPGRADE_IMAGE=${UPGRADE_IMAGE:-gotempsh/pgvector-walg:pg18}
PG_IMAGE=gotempsh/postgres-walg:18-bookworm

DATABASE_URL="postgresql://${PG_USER}:${PG_PASSWORD}@${PG_HOST}:${PG_PORT}/${DB_NAME}"
DATA_DIR="$WORK_DIR/data"
LOG_DIR="$WORK_DIR/logs"
API="http://127.0.0.1:${CONSOLE_PORT}/api"
KEEP="svcdata${RUN_ID}keep"
SHAPE="svcdata${RUN_ID}shape"
S3_CONTAINER="temps-svcdata-s3-${RUN_ID}"
S3_VOLUME="temps-svcdata-s3-${RUN_ID}"
PREVIEW_CONTAINER="temps-svcdata-preview-${RUN_ID}"
BUCKET="svcdata-${RUN_ID}"
S3_KEY="svcdata${RUN_ID}"
S3_SECRET="svcdata-secret-${RUN_ID}"
DATABASE_CREATED=0
SERVER_PID=""

mkdir -p "$DATA_DIR" "$LOG_DIR"
chmod 700 "$WORK_DIR" "$DATA_DIR"

log() { printf '\n==> %s\n' "$*"; }
fail() {
  printf '\nFAIL: %s\n' "$*" >&2
  for f in "$LOG_DIR"/*.log; do
    [ -f "$f" ] || continue
    printf '\n--- last 40 lines of %s ---\n' "$f" >&2
    tail -n 40 "$f" >&2 || true
  done
  exit 1
}

psql_admin() { docker exec -i "$PG_CONTAINER" psql -v ON_ERROR_STOP=1 -X -q -U "$PG_USER" -d postgres "$@"; }
psql_db() { docker exec -i "$PG_CONTAINER" psql -v ON_ERROR_STOP=1 -X -q -U "$PG_USER" -d "$DB_NAME" "$@"; }

# SQL inside a service's own container, as its own superuser.
svc_sql() {
  docker exec "$1" sh -c 'PGPASSWORD="$POSTGRES_PASSWORD" psql -X -q -A -t -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "$0"' "$2"
}
svc_wait() {
  for _ in $(seq 1 60); do
    svc_sql "$1" "SELECT 1" >/dev/null 2>&1 && return 0
    sleep 2
  done
  fail "PostgreSQL in $1 did not accept queries"
}
system_id() { svc_sql "$1" "SELECT system_identifier FROM pg_control_system()"; }

stop_server() {
  [ -n "$SERVER_PID" ] || return 0
  if kill -0 "$SERVER_PID" 2>/dev/null; then
    kill -TERM "$SERVER_PID" 2>/dev/null || true
    for _ in $(seq 1 60); do
      kill -0 "$SERVER_PID" 2>/dev/null || break
      sleep 1
    done
    kill -KILL "$SERVER_PID" 2>/dev/null || true
  fi
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
}

cleanup() {
  local status=$?
  stop_server
  # Only objects named after this run. Set-aside originals included.
  for c in $(docker ps -aq --filter "name=svcdata${RUN_ID}"); do docker rm -f -v "$c" >/dev/null 2>&1 || true; done
  docker rm -f -v "$S3_CONTAINER" "$PREVIEW_CONTAINER" "$PREVIEW_CONTAINER-relay" >/dev/null 2>&1 || true
  for v in $(docker volume ls -q --filter "name=svcdata${RUN_ID}") "$S3_VOLUME"; do
    docker volume rm "$v" >/dev/null 2>&1 || true
  done
  if [ -z "${KEEP_DATABASES:-}" ] && [ "$DATABASE_CREATED" = 1 ]; then
    psql_admin -c "DROP DATABASE IF EXISTS \"$DB_NAME\" WITH (FORCE)" >/dev/null 2>&1 || true
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'fail "command failed at line $LINENO: $BASH_COMMAND"' ERR

start_server() {
  local bin=$1 logfile=$2
  TEMPS_DATABASE_URL="$DATABASE_URL" TEMPS_DATA_DIR="$DATA_DIR" \
    TEMPS_ADMIN_EMAIL="admin@localho.st" TEMPS_ADMIN_PASSWORD_FILE="$WORK_DIR/admin_password" \
    TEMPS_TELEMETRY=0 \
    "$bin" serve --address "127.0.0.1:${HTTP_PORT}" --tls-address "127.0.0.1:${TLS_PORT}" \
    --console-address "127.0.0.1:${CONSOLE_PORT}" --disable-https-redirect \
    --screenshot-provider noop >"$logfile" 2>&1 &
  SERVER_PID=$!
  for _ in $(seq 1 120); do
    kill -0 "$SERVER_PID" 2>/dev/null || fail "server exited before becoming ready (see $logfile)"
    curl -fsS -o /dev/null -m 3 "http://127.0.0.1:${CONSOLE_PORT}/readyz" 2>/dev/null && return 0
    sleep 2
  done
  fail "server did not become ready within 240s (see $logfile)"
}

api() {
  local method=$1 path=$2
  shift 2
  curl -fsS -m 300 -X "$method" "$API$path" -H "Authorization: Bearer $API_KEY" "$@"
}
# Like api, but prints the HTTP status instead of failing on it.
api_status() {
  local method=$1 path=$2 out=$3
  shift 3
  curl -sS -m 300 -o "$out" -w '%{http_code}' -X "$method" "$API$path" -H "Authorization: Bearer $API_KEY" "$@"
}

wait_backup() {
  local id=$1 state=""
  for _ in $(seq 1 150); do
    state=$(psql_db -tA -c "SELECT state FROM external_service_backups WHERE backup_id=$id")
    case "$state" in completed | failed) break ;; esac
    sleep 2
  done
  printf '%s' "$state"
}

assert_data() {
  local container=$1 expected_id=$2 step=$3 rows
  svc_wait "$container"
  rows=$(svc_sql "$container" "SELECT count(*) FROM accounts" 2>&1) ||
    fail "$step: $container lost its data: $rows"
  [ "$rows" = 25 ] || fail "$step: $container has $rows rows, expected 25"
  [ "$(system_id "$container")" = "$expected_id" ] ||
    fail "$step: $container runs a different cluster (system identifier changed)"
  printf '    %s: 25 rows, same cluster\n' "$step"
}

create_service() {
  api POST /external-services -H 'Content-Type: application/json' -d "{
    \"name\": \"$1\", \"service_type\": \"postgres\", \"topology\": \"standalone\", \"members\": [],
    \"parameters\": {\"docker_image\": \"$PG_IMAGE\", \"username\": \"app\", \"database\": \"app\"}
  }" | jq -r .id
}

seed() {
  svc_wait "$1"
  svc_sql "$1" "CREATE TABLE accounts(id serial primary key, email text);
    INSERT INTO accounts(email) SELECT 'user' || g || '@example.test' FROM generate_series(1, 25) g;
    CREATE ROLE reporting LOGIN" >/dev/null
}

# ---------------------------------------------------------------------------
log "Binaries"
"$OLD_BIN" --version
"$NEW_BIN" --version

umask 077
{ printf 'Up9!'; openssl rand -hex 16; } >"$WORK_DIR/admin_password"
if [ -f crates/temps-cli/GeoLite2-City.mmdb ]; then
  cp crates/temps-cli/GeoLite2-City.mmdb "$DATA_DIR/GeoLite2-City.mmdb"
fi
[[ "$DB_NAME" =~ ^[a-zA-Z_][a-zA-Z0-9_]{0,62}$ ]] || fail "invalid test database name"
[ "$(psql_admin -tA -c "SELECT count(*) FROM pg_database WHERE datname='$DB_NAME'")" = 0 ] ||
  fail "refusing existing database $DB_NAME"
psql_admin -c "CREATE DATABASE \"$DB_NAME\" OWNER \"$PG_USER\""
DATABASE_CREATED=1

# ---------------------------------------------------------------------------
log "1. Seed managed PostgreSQL services with the previous release"
TEMPS_DATABASE_URL="$DATABASE_URL" "$OLD_BIN" migrate --yes >"$LOG_DIR/0-old-schema.log" 2>&1
psql_db -c "INSERT INTO settings (id,data,created_at,updated_at)
  VALUES (1, '{\"preview_gateway\":{\"enabled\":false,\"container_name\":\"$PREVIEW_CONTAINER\",\"host_port\":$((CONSOLE_PORT+100))}}'::jsonb, now(), now())
  ON CONFLICT (id) DO UPDATE SET data=jsonb_set(settings.data::jsonb,'{preview_gateway}',EXCLUDED.data::jsonb->'preview_gateway')"
start_server "$OLD_BIN" "$LOG_DIR/1-old.log"
# Only the JSON document: a Docker-context notice can precede it on stdout.
API_KEY=$(TEMPS_DATABASE_URL="$DATABASE_URL" "$OLD_BIN" api-key \
  --name "svcdata-test" --role admin --output-format json 2>/dev/null |
  sed -n '/^{/,$p' | jq -r '.api_key // empty')
[ -n "$API_KEY" ] || fail "could not create an API key with the previous release"
if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::add-mask::$API_KEY"; fi

KEEP_ID=$(create_service "$KEEP")
SHAPE_ID=$(create_service "$SHAPE")
[ "$KEEP_ID" != null ] && [ "$SHAPE_ID" != null ] || fail "services were not created"
KEEP_C="postgres-$KEEP"
SHAPE_C="postgres-$SHAPE"
seed "$KEEP_C"
KEEP_SYSID=$(system_id "$KEEP_C")

# The #1415 shape: same name, same credentials and port, but the cluster on
# the container's own anonymous volume and no `<name>_data` volume.
NETWORK=$(docker inspect "$SHAPE_C" --format '{{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}' | awk '{print $1}')
SHAPE_PORT=$(docker inspect "$SHAPE_C" --format '{{(index (index .HostConfig.PortBindings "5432/tcp") 0).HostPort}}')
docker inspect "$SHAPE_C" --format '{{range .Config.Env}}{{println .}}{{end}}' |
  grep -E '^POSTGRES_(USER|PASSWORD|DB)=' >"$WORK_DIR/shape.env"
docker rm -f -v "$SHAPE_C" >/dev/null
docker volume rm "${SHAPE_C}_data" >/dev/null
docker run -d --name "$SHAPE_C" --env-file "$WORK_DIR/shape.env" --network "$NETWORK" \
  --restart always -p "127.0.0.1:${SHAPE_PORT}:5432" "$PG_IMAGE" >/dev/null
rm -f "$WORK_DIR/shape.env"
seed "$SHAPE_C"
SHAPE_SYSID=$(system_id "$SHAPE_C")
SHAPE_CONTAINER_ID=$(docker inspect "$SHAPE_C" --format '{{.Id}}')

# A private S3 target on the services' network.
docker run -d --name "$S3_CONTAINER" --network "$NETWORK" -p "127.0.0.1:${S3_PORT}:9000" \
  -v "$S3_VOLUME:/data" -e "RUSTFS_ACCESS_KEY=$S3_KEY" -e "RUSTFS_SECRET_KEY=$S3_SECRET" \
  rustfs/rustfs:1.0.0 >/dev/null
for _ in $(seq 1 30); do
  curl -fsS -o /dev/null --aws-sigv4 "aws:amz:us-east-1:s3" --user "$S3_KEY:$S3_SECRET" \
    -X PUT "http://127.0.0.1:${S3_PORT}/${BUCKET}" 2>/dev/null && break
  sleep 1
done
S3_SOURCE_ID=$(api POST /backups/s3-sources -H 'Content-Type: application/json' -d "{
  \"name\": \"svcdata-${RUN_ID}\", \"bucket_name\": \"$BUCKET\", \"bucket_path\": \"run\",
  \"access_key_id\": \"$S3_KEY\", \"secret_key\": \"$S3_SECRET\", \"region\": \"us-east-1\",
  \"endpoint\": \"http://localhost:${S3_PORT}\", \"force_path_style\": true
}" | jq -r .id)
[ "$S3_SOURCE_ID" != null ] || fail "S3 source was not created"
stop_server

# ---------------------------------------------------------------------------
log "2. Upgrade to the candidate"
start_server "$NEW_BIN" "$LOG_DIR/2-new.log"
assert_data "$KEEP_C" "$KEEP_SYSID" "after upgrade"
assert_data "$SHAPE_C" "$SHAPE_SYSID" "after upgrade (#1415 shape)"

# ---------------------------------------------------------------------------
log "3. Run backup on a managed service with WAL archiving off"
PREFLIGHT=$(api GET "/backups/external-services/$KEEP_ID/run-preflight")
[ "$(jq -r .restarts_database <<<"$PREFLIGHT")" = true ] ||
  fail "preflight did not report the restart: $PREFLIGHT"
CODE=$(api_status POST "/backups/external-services/$KEEP_ID/run" "$WORK_DIR/run.json" \
  -H 'Content-Type: application/json' -d "{\"s3_source_id\": $S3_SOURCE_ID}")
[ "$CODE" = 409 ] || fail "an unconfirmed restarting backup returned $CODE, expected 409"
jq -e '.error_code == "RESTART_CONFIRMATION_REQUIRED"' "$WORK_DIR/run.json" >/dev/null ||
  fail "409 does not carry RESTART_CONFIRMATION_REQUIRED"
assert_data "$KEEP_C" "$KEEP_SYSID" "refused unconfirmed backup"
RUN=$(api POST "/backups/external-services/$KEEP_ID/run" -H 'Content-Type: application/json' \
  -d "{\"s3_source_id\": $S3_SOURCE_ID, \"confirm_restart\": true}")
[ "$(jq -r .restarts_database <<<"$RUN")" = true ] || fail "run response does not say it restarts"
STATE=$(wait_backup "$(jq -r .backup_id <<<"$RUN")")
[ "$STATE" = completed ] || fail "confirmed backup ended as '$STATE'"
[ "$(svc_sql "$KEEP_C" "SHOW archive_mode")" = on ] || fail "archive_mode is not on after the backup"
assert_data "$KEEP_C" "$KEEP_SYSID" "after the restarting backup"
[ -z "$(docker ps -aq --filter "name=${KEEP_C}-pre-recreate-")" ] ||
  fail "the verified original container was not removed"

# ---------------------------------------------------------------------------
log "4. Run backup on the #1415 shape: refused, nothing lost, nothing pushed"
RUN=$(api POST "/backups/external-services/$SHAPE_ID/run" -H 'Content-Type: application/json' \
  -d "{\"s3_source_id\": $S3_SOURCE_ID, \"confirm_restart\": true}")
STATE=$(wait_backup "$(jq -r .backup_id <<<"$RUN")")
[ "$STATE" = failed ] || fail "backup of the #1415 shape ended as '$STATE', expected failed"
ERROR=$(psql_db -tA -c "SELECT error_message FROM external_service_backups WHERE backup_id=$(jq -r .backup_id <<<"$RUN")")
grep -q "Nothing was changed" <<<"$ERROR" || fail "failure does not say nothing was changed: $ERROR"
[ "$(docker inspect "$SHAPE_C" --format '{{.Id}}')" = "$SHAPE_CONTAINER_ID" ] ||
  fail "the #1415-shaped container was replaced"
assert_data "$SHAPE_C" "$SHAPE_SYSID" "after the refused backup"
# Fetch the listing on its own so a failed request fails the test instead
# of reading as "nothing was pushed".
LISTING=$(curl -fsS --aws-sigv4 "aws:amz:us-east-1:s3" --user "$S3_KEY:$S3_SECRET" \
  "http://127.0.0.1:${S3_PORT}/${BUCKET}?list-type=2&prefix=run/external_services/postgres/${SHAPE}/") ||
  fail "could not list the S3 bucket to check that nothing was pushed"
grep -q "<ListBucketResult" <<<"$LISTING" || fail "the S3 listing is not a bucket listing: $LISTING"
if grep -q basebackups <<<"$LISTING"; then
  fail "a base backup was pushed for the refused service"
fi

# ---------------------------------------------------------------------------
log "5. Stop/start, /dev/shm change and image change keep the data"
api POST "/external-services/$KEEP_ID/stop" >/dev/null
api POST "/external-services/$KEEP_ID/start" >/dev/null
assert_data "$KEEP_C" "$KEEP_SYSID" "after stop + start"

SHM=$(api PATCH "/external-services/$KEEP_ID/resources" -H 'Content-Type: application/json' \
  -d '{"shm_size_mb": 256}')
[ "$(jq -r '.shm_not_applied // empty' <<<"$SHM")" = "" ] ||
  fail "shm change was not applied: $(jq -r .shm_not_applied <<<"$SHM")"
[ "$(docker inspect "$KEEP_C" --format '{{.HostConfig.ShmSize}}')" = $((256 * 1024 * 1024)) ] ||
  fail "the container was not recreated with the new /dev/shm size"
assert_data "$KEEP_C" "$KEEP_SYSID" "after a /dev/shm change"

api POST "/external-services/$KEEP_ID/upgrade" -H 'Content-Type: application/json' \
  -d "{\"docker_image\": \"$UPGRADE_IMAGE\"}" >/dev/null
[ "$(docker inspect "$KEEP_C" --format '{{.Config.Image}}')" = "$UPGRADE_IMAGE" ] ||
  fail "the image change did not happen"
[ "$(docker inspect "$KEEP_C" --format '{{.HostConfig.ShmSize}}')" = $((256 * 1024 * 1024)) ] ||
  fail "the image change dropped the service's /dev/shm size"
assert_data "$KEEP_C" "$KEEP_SYSID" "after an image change"

log "PASS: managed-service data survived the upgrade and every recreate path"
