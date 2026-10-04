#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Upgrade-from-previous-release test.
#
# Seeds an installation with an older released binary, upgrades it in place
# with a newer binary, and checks that nothing was lost. Then it exercises the
# upgrade safety rails end to end:
#
#   1. OLD_BIN starts on an empty database; an admin, an API key, a project,
#      a second environment, environment variables, an imported external
#      service and a completed static deployment are created through the API.
#   2. NEW_BIN starts on the same database and data directory. It must take a
#      pre-migration backup, migrate, come up healthy, and serve all of the
#      seeded data (including the deployed site through the proxy).
#   3. A plain restart of NEW_BIN must not take another backup.
#   4. With an unknown "future" migration recorded, NEW_BIN must refuse to
#      start (schema guard) and leave the database untouched.
#   5. OLD_BIN must refuse the upgraded database.
#   6. The pre-migration backup is restored into a fresh database, and OLD_BIN
#      must start on it and serve the seeded data again (rollback).
#
# Required environment:
#   OLD_BIN, NEW_BIN    previous release and candidate `temps` binaries
#   PG_CONTAINER        Docker container running the TimescaleDB server; its
#                       psql / pg_restore are used for setup and the restore
# Optional environment (defaults in brackets):
#   PG_HOST [127.0.0.1]  PG_PORT [5432]  PG_USER [temps]  PG_PASSWORD [temps]
#   DB_NAME [unique run name]  ROLLBACK_DB_NAME [${DB_NAME}_rollback]
#   WORK_DIR [mktemp -d]  HTTP_PORT [18690]  CONSOLE_PORT [18691]
#   TLS_PORT [18693]  ADMIN_EMAIL [admin@localho.st]
#   KEEP_DATABASES [unset: both databases are dropped on exit]
#
# Both database names must be unused. The test refuses existing databases and
# drops only the databases it creates.

set -Eeuo pipefail

: "${OLD_BIN:?OLD_BIN must point at the previous release binary}"
: "${NEW_BIN:?NEW_BIN must point at the candidate binary}"
: "${PG_CONTAINER:?PG_CONTAINER must name the database container}"

RUN_ID="$(date +%s)-$$"
PG_HOST=${PG_HOST:-127.0.0.1}
PG_PORT=${PG_PORT:-5432}
PG_USER=${PG_USER:-temps}
PG_PASSWORD=${PG_PASSWORD:-temps}
DB_NAME=${DB_NAME:-temps_upgrade_${RUN_ID//-/_}}
ROLLBACK_DB_NAME=${ROLLBACK_DB_NAME:-${DB_NAME}_rollback}
WORK_DIR=${WORK_DIR:-$(mktemp -d)}
HTTP_PORT=${HTTP_PORT:-18690}
CONSOLE_PORT=${CONSOLE_PORT:-18691}
TLS_PORT=${TLS_PORT:-18693}
ADMIN_EMAIL=${ADMIN_EMAIL:-admin@localho.st}

DATABASE_URL="postgresql://${PG_USER}:${PG_PASSWORD}@${PG_HOST}:${PG_PORT}/${DB_NAME}"
ROLLBACK_DATABASE_URL="postgresql://${PG_USER}:${PG_PASSWORD}@${PG_HOST}:${PG_PORT}/${ROLLBACK_DB_NAME}"
DATA_DIR="$WORK_DIR/data"
LOG_DIR="$WORK_DIR/logs"
BACKUP_DIR="$DATA_DIR/backups/pre-migration"
API="http://127.0.0.1:${CONSOLE_PORT}/api"
DATABASE_CREATED=0
ROLLBACK_DATABASE_CREATED=0
REDIS_CONTAINER="temps-upgrade-test-redis-${RUN_ID}"
PREVIEW_CONTAINER="temps-upgrade-test-preview-${RUN_ID}"
REDIS_PASSWORD="upgrade-test-redis-pass"
SERVER_PID=""

mkdir -p "$DATA_DIR" "$LOG_DIR"
chmod 700 "$WORK_DIR" "$DATA_DIR"

log() { printf '\n==> %s\n' "$*"; }
fail() {
  printf '\nFAIL: %s\n' "$*" >&2
  for f in "$LOG_DIR"/*.log; do
    [ -f "$f" ] || continue
    printf '\n--- last 60 lines of %s ---\n' "$f" >&2
    tail -n 60 "$f" >&2 || true
  done
  exit 1
}

psql_admin() {
  docker exec -i "$PG_CONTAINER" psql -v ON_ERROR_STOP=1 -X -q -U "$PG_USER" -d postgres "$@"
}
psql_db() {
  local db=$1
  shift
  docker exec -i "$PG_CONTAINER" psql -v ON_ERROR_STOP=1 -X -q -U "$PG_USER" -d "$db" "$@"
}

create_database() {
  local db=$1 exists
  [[ "$db" =~ ^[a-zA-Z_][a-zA-Z0-9_]{0,62}$ ]] || fail "invalid test database name"
  [[ "$PG_USER" =~ ^[a-zA-Z_][a-zA-Z0-9_]*$ ]] || fail "invalid test database user"
  exists=$(psql_admin -tA -c "SELECT count(*) FROM pg_database WHERE datname='$db'")
  [ "$exists" = "0" ] || fail "refusing existing database $db; choose an unused test name"
  psql_admin -c "CREATE DATABASE \"$db\" OWNER \"$PG_USER\""
  if [ "$db" = "$DB_NAME" ]; then DATABASE_CREATED=1; else ROLLBACK_DATABASE_CREATED=1; fi
}

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
  docker rm -f "$REDIS_CONTAINER" "$PREVIEW_CONTAINER" "$PREVIEW_CONTAINER-relay" >/dev/null 2>&1 || true
  if [ -z "${KEEP_DATABASES:-}" ]; then
    if [ "$DATABASE_CREATED" = 1 ]; then
      psql_admin -c "DROP DATABASE IF EXISTS \"$DB_NAME\" WITH (FORCE)" >/dev/null 2>&1 || true
    fi
    if [ "$ROLLBACK_DATABASE_CREATED" = 1 ]; then
      psql_admin -c "DROP DATABASE IF EXISTS \"$ROLLBACK_DB_NAME\" WITH (FORCE)" >/dev/null 2>&1 || true
    fi
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'fail "command failed at line $LINENO: $BASH_COMMAND"' ERR

# Start `temps serve` in the background. Usage: start_server BIN DB_URL LOG [extra args]
start_server() {
  local bin=$1 db_url=$2 logfile=$3
  shift 3
  TEMPS_DATABASE_URL="$db_url" \
    TEMPS_DATA_DIR="$DATA_DIR" \
    TEMPS_ADMIN_EMAIL="$ADMIN_EMAIL" \
    TEMPS_ADMIN_PASSWORD_FILE="$WORK_DIR/admin_password" \
    TEMPS_TELEMETRY=0 \
    "$bin" serve \
    --address "127.0.0.1:${HTTP_PORT}" \
    --tls-address "127.0.0.1:${TLS_PORT}" \
    --console-address "127.0.0.1:${CONSOLE_PORT}" \
    --disable-https-redirect \
    --screenshot-provider noop \
    "$@" >"$logfile" 2>&1 &
  SERVER_PID=$!
}

wait_ready() {
  local logfile=$1
  for _ in $(seq 1 120); do
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      fail "server exited before becoming ready (see $logfile)"
    fi
    if curl -fsS -o /dev/null -m 3 "http://127.0.0.1:${CONSOLE_PORT}/readyz" 2>/dev/null; then
      return 0
    fi
    sleep 2
  done
  fail "server did not become ready within 240s (see $logfile)"
}

# Run `temps serve` expecting it to exit non-zero without becoming ready.
# Usage: expect_start_refused BIN DB_URL LOG [grep pattern]
expect_start_refused() {
  local bin=$1 db_url=$2 logfile=$3 pattern=${4:-}
  start_server "$bin" "$db_url" "$logfile"
  local pid=$SERVER_PID exited=""
  for _ in $(seq 1 90); do
    if ! kill -0 "$pid" 2>/dev/null; then
      exited=1
      break
    fi
    sleep 2
  done
  if [ -z "$exited" ]; then
    stop_server
    fail "$bin started against a database it must refuse (see $logfile)"
  fi
  local code=0
  wait "$pid" || code=$?
  SERVER_PID=""
  [ "$code" -ne 0 ] || fail "$bin exited 0 instead of refusing to start (see $logfile)"
  if [ -n "$pattern" ] && ! grep -q "$pattern" "$logfile"; then
    fail "$bin refused to start, but its log does not explain why ('$pattern' not found in $logfile)"
  fi
}

api() {
  local method=$1 path=$2
  shift 2
  curl -fsS -m 30 -X "$method" "$API$path" -H "Authorization: Bearer $API_KEY" "$@"
}

count_backups() {
  if [ ! -d "$BACKUP_DIR" ]; then printf '0\n'; return; fi
  find "$BACKUP_DIR" -maxdepth 1 -name 'temps-pre-migration-*.dump' 2>/dev/null | wc -l | tr -d ' '
}

# ---------------------------------------------------------------------------
log "Binaries"
"$OLD_BIN" --version
"$NEW_BIN" --version

umask 077
{ printf 'Up9!'; openssl rand -hex 16; } >"$WORK_DIR/admin_password"
chmod 600 "$WORK_DIR/admin_password"
ADMIN_PASSWORD=$(cat "$WORK_DIR/admin_password")
if [ -f crates/temps-cli/GeoLite2-City.mmdb ]; then
  cp crates/temps-cli/GeoLite2-City.mmdb "$DATA_DIR/GeoLite2-City.mmdb"
fi

[ "$DB_NAME" != "$ROLLBACK_DB_NAME" ] || fail "test database names must differ"
create_database "$DB_NAME"

# ---------------------------------------------------------------------------
log "1. Seed with the previous release"
# Initialize only this new database before booting background services. Older
# releases do not understand the preview gateway's enabled flag, but support
# its container name: isolate their reconciler from any other local instance.
TEMPS_DATABASE_URL="$DATABASE_URL" "$OLD_BIN" migrate --yes >"$LOG_DIR/0-old-schema.log" 2>&1
psql_db "$DB_NAME" -c "INSERT INTO settings (id,data,created_at,updated_at)
  VALUES (1, '{\"preview_gateway\":{\"enabled\":false,\"container_name\":\"$PREVIEW_CONTAINER\",\"host_port\":$((CONSOLE_PORT+100))}}'::jsonb, now(), now())
  ON CONFLICT (id) DO UPDATE SET data=jsonb_set(settings.data::jsonb,'{preview_gateway}',EXCLUDED.data::jsonb->'preview_gateway')"
start_server "$OLD_BIN" "$DATABASE_URL" "$LOG_DIR/1-old-seed.log"
wait_ready "$LOG_DIR/1-old-seed.log"

API_KEY=$(TEMPS_DATABASE_URL="$DATABASE_URL" "$OLD_BIN" api-key \
  --name "upgrade-test" --role admin --output-format json 2>/dev/null | jq -r '.api_key // empty')
[ -n "$API_KEY" ] || fail "could not create an API key with the previous release"
if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::add-mask::$API_KEY"; fi

PROJECT_ID=$(api POST /projects -H 'Content-Type: application/json' -d '{
  "name": "upgrade-static", "source_type": "static_files", "preset": "vite",
  "directory": "/", "main_branch": "main", "automatic_deploy": false,
  "storage_service_ids": []
}' | jq -r .id)
[ "$PROJECT_ID" != "null" ] || fail "project was not created"
PROD_ENV_ID=$(api GET "/projects/$PROJECT_ID/environments" | jq -r '.[] | select(.name=="production") | .id')
STAGING_ENV_ID=$(api POST "/projects/$PROJECT_ID/environments" -H 'Content-Type: application/json' \
  -d '{"name":"staging","branch":"staging"}' | jq -r .id)
[ -n "$PROD_ENV_ID" ] && [ "$STAGING_ENV_ID" != "null" ] || fail "environments were not created"

PLAIN_VAR_ID=$(api POST "/projects/$PROJECT_ID/env-vars" -H 'Content-Type: application/json' \
  -d "{\"key\":\"UPGRADE_PLAIN\",\"value\":\"plain-value-${RUN_ID}\",\"environment_ids\":[$PROD_ENV_ID,$STAGING_ENV_ID]}" | jq -r .id)
SECRET_VAR_ID=$(api POST "/projects/$PROJECT_ID/env-vars" -H 'Content-Type: application/json' \
  -d "{\"key\":\"UPGRADE_SECRET\",\"value\":\"secret-${RUN_ID}\",\"environment_ids\":[$PROD_ENV_ID],\"is_secret\":true}" | jq -r .id)
[ "$PLAIN_VAR_ID" != "null" ] && [ "$SECRET_VAR_ID" != "null" ] || fail "env vars were not created"

docker run -d --name "$REDIS_CONTAINER" -p 127.0.0.1::6379 \
  --entrypoint redis-server gotempsh/redis-walg:8-bookworm \
  --requirepass "$REDIS_PASSWORD" >/dev/null
REDIS_PORT=$(docker port "$REDIS_CONTAINER" 6379/tcp | head -n1 | awk -F: '{print $NF}')
SERVICE_ID=$(api POST /external-services/import -H 'Content-Type: application/json' -d "{
  \"name\": \"upgrade-redis\", \"service_type\": \"redis\", \"container_id\": \"$REDIS_CONTAINER\",
  \"parameters\": {\"host\": \"localhost\", \"port\": \"$REDIS_PORT\", \"password\": \"$REDIS_PASSWORD\"}
}" | jq -r .id)
[ "$SERVICE_ID" != "null" ] || fail "external service was not imported"
api POST "/external-services/$SERVICE_ID/projects" -H 'Content-Type: application/json' \
  -d "{\"project_id\":$PROJECT_ID}" >/dev/null

SITE_MARKER="upgrade-site-${RUN_ID}"
mkdir -p "$WORK_DIR/site"
printf '<h1>%s</h1>\n' "$SITE_MARKER" >"$WORK_DIR/site/index.html"
tar -czf "$WORK_DIR/site.tar.gz" -C "$WORK_DIR/site" .
BUNDLE_ID=$(api POST "/projects/$PROJECT_ID/upload/static" -F "file=@$WORK_DIR/site.tar.gz" | jq -r .id)
DEPLOYMENT_ID=$(api POST "/projects/$PROJECT_ID/environments/$PROD_ENV_ID/deploy/static" \
  -H 'Content-Type: application/json' -d "{\"static_bundle_id\":$BUNDLE_ID}" | jq -r .id)
[ "$DEPLOYMENT_ID" != "null" ] || fail "static deployment was not created"
DEPLOYMENT_STATUS=""
for _ in $(seq 1 60); do
  DEPLOYMENT_STATUS=$(api GET "/projects/$PROJECT_ID/deployments/$DEPLOYMENT_ID" | jq -r .status)
  case "$DEPLOYMENT_STATUS" in completed | failed | cancelled) break ;; esac
  sleep 2
done
[ "$DEPLOYMENT_STATUS" = "completed" ] || fail "static deployment ended as '$DEPLOYMENT_STATUS'"
SITE_HOST=$(api GET "/projects/$PROJECT_ID/environments" |
  jq -r ".[] | select(.id==$PROD_ENV_ID) | .main_url" | sed -E 's#^https?://##; s#:[0-9]+$##; s#/.*$##')
[ -n "$SITE_HOST" ] || fail "production environment has no URL"

echo "seeded: project=$PROJECT_ID envs=$PROD_ENV_ID,$STAGING_ENV_ID vars=$PLAIN_VAR_ID,$SECRET_VAR_ID service=$SERVICE_ID deployment=$DEPLOYMENT_ID host=$SITE_HOST"

# Everything the upgraded (and the rolled-back) installation must still have.
verify_data() {
  local stage=$1
  log "Verifying seeded data ($stage)"
  curl -fsS -m 30 -o /dev/null -X POST "$API/auth/login" -H 'Content-Type: application/json' \
    -d "$(jq -n --arg e "$ADMIN_EMAIL" --arg p "$ADMIN_PASSWORD" '{email:$e,password:$p}')" ||
    fail "[$stage] admin can no longer log in with the seeded password"

  local project
  project=$(api GET "/projects/$PROJECT_ID") || fail "[$stage] project $PROJECT_ID is gone (or the API key no longer works)"
  [ "$(jq -r .slug <<<"$project")" = "upgrade-static" ] || fail "[$stage] project slug changed: $project"

  local envs
  envs=$(api GET "/projects/$PROJECT_ID/environments" | jq -r '[.[].name] | sort | join(",")')
  [ "$envs" = "production,staging" ] || fail "[$stage] environments changed: $envs"

  local vars
  vars=$(api GET "/projects/$PROJECT_ID/env-vars" | jq -r '[.[].key] | sort | join(",")')
  [ "$vars" = "UPGRADE_PLAIN,UPGRADE_SECRET" ] || fail "[$stage] env vars changed: $vars"
  local plain
  plain=$(api GET "/projects/$PROJECT_ID/env-vars/UPGRADE_PLAIN/value?environment_id=$PROD_ENV_ID" | jq -r .value)
  [ "$plain" = "plain-value-${RUN_ID}" ] || fail "[$stage] env var value no longer decrypts: '$plain'"

  local secret_status
  secret_status=$(curl -sS -m 30 -o /dev/null -w '%{http_code}' \
    "$API/projects/$PROJECT_ID/env-vars/UPGRADE_SECRET/value?environment_id=$PROD_ENV_ID" \
    -H "Authorization: Bearer $API_KEY")
  [ "$secret_status" = "403" ] || fail "[$stage] write-only secret became readable"

  local service
  service=$(api GET "/external-services/$SERVICE_ID" | jq -r '(.service // .) | "\(.name)/\(.service_type)"')
  [ "$service" = "upgrade-redis/redis" ] || fail "[$stage] external service changed: $service"
  api GET "/external-services/$SERVICE_ID/projects" | jq -e "[.[] | (.project.id // .project_id // .id)] | index($PROJECT_ID) != null" >/dev/null ||
    fail "[$stage] external service is no longer linked to project $PROJECT_ID"

  local status
  status=$(api GET "/projects/$PROJECT_ID/deployments/$DEPLOYMENT_ID" | jq -r .status)
  [ "$status" = "completed" ] || fail "[$stage] deployment $DEPLOYMENT_ID is now '$status'"

  local body=""
  for _ in $(seq 1 15); do
    body=$(curl -s -m 5 -H "Host: $SITE_HOST" "http://127.0.0.1:${HTTP_PORT}/" || true)
    case "$body" in *"$SITE_MARKER"*) break ;; esac
    sleep 2
  done
  case "$body" in *"$SITE_MARKER"*) ;; *) fail "[$stage] the proxy no longer serves the deployed site for $SITE_HOST" ;; esac
  echo "[$stage] all seeded data intact"
}

verify_data "previous release, before upgrade"
stop_server
[ "$(count_backups)" = "0" ] || fail "a pre-migration backup exists before the upgrade"

# ---------------------------------------------------------------------------
TEMPS_DATABASE_URL="$DATABASE_URL" "$NEW_BIN" migrate --dry-run --progress-format=json >"$LOG_DIR/2-migration-plan.log" 2>&1
HAS_PENDING=1
EXPECTED_BACKUPS=1
if grep -q '"event":"up_to_date"' "$LOG_DIR/2-migration-plan.log"; then
  HAS_PENDING=0
  EXPECTED_BACKUPS=0
  # The product correctly skips a rollback snapshot when nothing migrates.
  # Keep a test-owned snapshot so restore is still exercised in this case.
  BACKUP_FILE="$WORK_DIR/test-same-schema.dump"
  docker exec "$PG_CONTAINER" pg_dump --format=custom --no-owner -U "$PG_USER" -d "$DB_NAME" >"$BACKUP_FILE"
fi

if [ "$HAS_PENDING" = 1 ]; then
log "2. Refuse an upgrade if the automatic backup cannot be written"
LEDGER_BEFORE=$(psql_db "$DB_NAME" -tA -c "SELECT count(*) FROM seaql_migrations")
printf 'intentional backup directory obstruction\n' >"$DATA_DIR/backups"
expect_start_refused "$NEW_BIN" "$DATABASE_URL" "$LOG_DIR/2-backup-refused.log" "pre-migration backup failed"
LEDGER_AFTER=$(psql_db "$DB_NAME" -tA -c "SELECT count(*) FROM seaql_migrations")
[ "$LEDGER_BEFORE" = "$LEDGER_AFTER" ] || fail "failed backup changed the migration ledger"
rm "$DATA_DIR/backups"
fi

log "2. Upgrade in place with the candidate binary"
start_server "$NEW_BIN" "$DATABASE_URL" "$LOG_DIR/2-new-upgrade.log"
wait_ready "$LOG_DIR/2-new-upgrade.log"
if [ "$HAS_PENDING" = 1 ]; then
grep -q "Pre-migration database backup written" "$LOG_DIR/2-new-upgrade.log" ||
  fail "the upgrade did not log a pre-migration backup"
[ "$(count_backups)" = "1" ] || fail "expected exactly one pre-migration backup, found $(count_backups)"
BACKUP_FILE=$(find "$BACKUP_DIR" -maxdepth 1 -name 'temps-pre-migration-*.dump' | head -n1)
[ -f "${BACKUP_FILE%.dump}.json" ] || fail "backup manifest missing for $BACKUP_FILE"
jq -e '.pending_migrations | length > 0' "${BACKUP_FILE%.dump}.json" >/dev/null ||
  fail "backup manifest lists no pending migrations"
else
  [ "$(count_backups)" = 0 ] || fail "same-schema upgrade took an unnecessary backup"
fi
docker exec -i "$PG_CONTAINER" pg_restore --list <"$BACKUP_FILE" >"$WORK_DIR/backup-contents.txt"
grep -q "TABLE DATA public projects" "$WORK_DIR/backup-contents.txt" ||
  fail "the backup is not a readable pg_restore archive containing project data"
echo "backup: $BACKUP_FILE ($(wc -c <"$BACKUP_FILE" | tr -d ' ') bytes)"
verify_data "candidate, after upgrade"
stop_server

# ---------------------------------------------------------------------------
log "3. Plain restart takes no backup"
start_server "$NEW_BIN" "$DATABASE_URL" "$LOG_DIR/3-new-restart.log"
wait_ready "$LOG_DIR/3-new-restart.log"
if grep -q "Pre-migration database backup written" "$LOG_DIR/3-new-restart.log"; then
  fail "a restart with no pending migrations took a backup"
fi
[ "$(count_backups)" = "$EXPECTED_BACKUPS" ] || fail "a restart changed the number of backups"
stop_server

# ---------------------------------------------------------------------------
log "4. Schema guard refuses a database migrated by a newer release"
FUTURE_MIGRATION="m29991231_000001_upgrade_test_future_release"
psql_db "$DB_NAME" -c "INSERT INTO seaql_migrations (version, applied_at) VALUES ('$FUTURE_MIGRATION', 0)"
LEDGER_BEFORE=$(psql_db "$DB_NAME" -tA -c "SELECT count(*) FROM seaql_migrations")
expect_start_refused "$NEW_BIN" "$DATABASE_URL" "$LOG_DIR/4-new-guard.log" "newer than this Temps binary"
grep -q "$FUTURE_MIGRATION" "$LOG_DIR/4-new-guard.log" || fail "the guard error does not name the unknown migration"
LEDGER_AFTER=$(psql_db "$DB_NAME" -tA -c "SELECT count(*) FROM seaql_migrations")
[ "$LEDGER_BEFORE" = "$LEDGER_AFTER" ] || fail "the refused start changed the migration ledger"
[ "$(count_backups)" = "$EXPECTED_BACKUPS" ] || fail "the refused start took a backup"
psql_db "$DB_NAME" -c "DELETE FROM seaql_migrations WHERE version = '$FUTURE_MIGRATION'"

# ---------------------------------------------------------------------------
if [ "$HAS_PENDING" = 1 ]; then
log "5. The previous release refuses the upgraded database"
LEDGER_BEFORE=$(psql_db "$DB_NAME" -tA -c "SELECT count(*) FROM seaql_migrations")
expect_start_refused "$OLD_BIN" "$DATABASE_URL" "$LOG_DIR/5-old-on-upgraded.log"
LEDGER_AFTER=$(psql_db "$DB_NAME" -tA -c "SELECT count(*) FROM seaql_migrations")
[ "$LEDGER_BEFORE" = "$LEDGER_AFTER" ] || fail "refused old release changed the migration ledger"
else
  log "5. The previous release remains compatible when no migrations changed"
  start_server "$OLD_BIN" "$DATABASE_URL" "$LOG_DIR/5-old-compatible.log"
  wait_ready "$LOG_DIR/5-old-compatible.log"
  verify_data "previous release, unchanged schema"
  stop_server
fi

# ---------------------------------------------------------------------------
log "6. Roll back: restore the pre-migration backup and start the previous release"
create_database "$ROLLBACK_DB_NAME"
psql_db "$ROLLBACK_DB_NAME" -c "CREATE EXTENSION IF NOT EXISTS timescaledb" \
  -c "SELECT timescaledb_pre_restore()"
docker exec -i "$PG_CONTAINER" pg_restore --no-owner --exit-on-error \
  -U "$PG_USER" -d "$ROLLBACK_DB_NAME" <"$BACKUP_FILE" ||
  fail "pg_restore of $BACKUP_FILE failed"
psql_db "$ROLLBACK_DB_NAME" -c "SELECT timescaledb_post_restore()"
start_server "$OLD_BIN" "$ROLLBACK_DATABASE_URL" "$LOG_DIR/6-old-rollback.log"
wait_ready "$LOG_DIR/6-old-rollback.log"
verify_data "previous release, after rollback"
stop_server

log "Upgrade test passed"
