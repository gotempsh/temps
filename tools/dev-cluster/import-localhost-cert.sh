#!/usr/bin/env bash
#
# Import the *.localho.st cert + encryption key from your local
# `temps_development` Postgres + ~/.temps/encryption_key into the dev
# cluster's control-plane container.
#
# After this runs:
#   - https://app.localho.st (and any *.localho.st)  →  dev cluster UI
#   - the proxy serves the imported cert on :443
#   - HTTP→HTTPS redirect works (because :443 is now real)
#
# Why this works: the dev cluster's control-plane container has its own
# /var/lib/temps/encryption_key generated on first boot. We replace it
# with your local one BEFORE re-running setup, then copy the encrypted
# cert bytes from the local DB into the cluster DB. Same key, same
# encrypted blob, same decrypted cert.
#
# Run:
#   cd tools/dev-cluster
#   ./import-localhost-cert.sh
#
# Env overrides:
#   LOCAL_DEV_DB_URL   default: postgres://postgres:password@localhost:5432/temps_development
#   LOCAL_KEY_PATH     default: <repo>/crates/temps-cli/temps_data/encryption_key
#                      (this is the dev server's data dir per .vscode/launch.json,
#                       NOT ~/.temps which is the CLI's data dir for a different
#                       use case and uses a different key.)

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"

LOCAL_DEV_DB_URL="${LOCAL_DEV_DB_URL:-postgres://postgres:password@localhost:5432/temps_development}"
# Default to the workspace dev-server data dir (matches .vscode/launch.json).
# Falls back to ~/.temps/encryption_key for legacy setups.
DEFAULT_KEY="$HERE/../../crates/temps-cli/temps_data/encryption_key"
if [[ -r "$DEFAULT_KEY" ]]; then
  LOCAL_KEY_PATH="${LOCAL_KEY_PATH:-$DEFAULT_KEY}"
else
  LOCAL_KEY_PATH="${LOCAL_KEY_PATH:-$HOME/.temps/encryption_key}"
fi

cyan()   { printf '\033[1;36m%s\033[0m' "$*"; }
yellow() { printf '\033[1;33m%s\033[0m' "$*"; }
red()    { printf '\033[1;31m%s\033[0m' "$*"; }
log()    { printf '%s %s\n' "$(cyan '[import-cert]')" "$*"; }
fail()   { printf '%s %s\n' "$(red '[import-cert]')"  "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# 0. Preflight
# ---------------------------------------------------------------------------
docker version >/dev/null 2>&1 || fail "docker daemon is not running"
[[ -r "$LOCAL_KEY_PATH" ]] || fail "missing encryption key at $LOCAL_KEY_PATH"

# psql may be a host binary (Postgres.app, brew install libpq) OR we
# fall back to running it through the Postgres container we already
# have running for the cluster. Prefer host because it can talk to
# host postgres (where temps_development lives) without networking gymnastics.
if command -v psql >/dev/null 2>&1; then
  PSQL_LOCAL="psql"
else
  fail "psql not found on PATH. install with: brew install libpq && brew link --force libpq"
fi

log "checking local dev DB at $LOCAL_DEV_DB_URL"
"$PSQL_LOCAL" "$LOCAL_DEV_DB_URL" -tAc 'SELECT 1' >/dev/null 2>&1 \
  || fail "cannot connect to local temps_development DB. Is your dev postgres running?"

log "checking dev-cluster control-plane container"
docker compose ps control-plane --format '{{.State}}' | grep -q running \
  || fail "control-plane container is not running. Start with: ./dev-cluster up"

# ---------------------------------------------------------------------------
# 1. Find the localho.st cert in the local DB
# ---------------------------------------------------------------------------
log "looking for *.localho.st rows in local DB"
LOCAL_DOMAINS=$("$PSQL_LOCAL" "$LOCAL_DEV_DB_URL" -tAc "
  SELECT id || '|' || domain || '|' || status || '|' || is_wildcard
    FROM domains
   WHERE domain LIKE '%localho.st%'
     AND certificate IS NOT NULL
     AND private_key IS NOT NULL
")

if [[ -z "$LOCAL_DOMAINS" ]]; then
  fail "no *.localho.st row with cert+private_key found in $LOCAL_DEV_DB_URL.
You need to have run temps setup once locally so the dev cert exists.
Try: cd \$workspace; cargo run --bin temps -- setup --auto"
fi

log "found these domain rows to import:"
while IFS= read -r row; do
  echo "    $row"   # id|domain|status|is_wildcard
done <<< "$LOCAL_DOMAINS"

# ---------------------------------------------------------------------------
# 2. Stop control-plane briefly so we can swap its encryption key
# ---------------------------------------------------------------------------
log "stopping control-plane to swap encryption_key"
docker compose stop control-plane >/dev/null

# Start postgres container is already up (it's a separate service);
# we need to bring control-plane back briefly to use docker cp into it.
docker compose up -d --no-deps control-plane >/dev/null
# docker cp works on stopped containers too, but we need the container
# to exist. compose up -d created one. Wait a beat for it to be addressable.
sleep 2

# ---------------------------------------------------------------------------
# 3. Copy local encryption key into the control-plane container
# ---------------------------------------------------------------------------
log "copying local encryption_key into control-plane:/var/lib/temps/"
docker cp "$LOCAL_KEY_PATH" temps-dev-control-plane:/var/lib/temps/encryption_key
docker compose exec -T control-plane chmod 600 /var/lib/temps/encryption_key
log "encryption_key replaced"

# ---------------------------------------------------------------------------
# 4. Dump localho.st rows from local DB, restore into cluster DB.
#    Use COPY in CSV form so we don't have to fight pg_dump's options
#    across versions, and so we can rewrite ids cleanly.
# ---------------------------------------------------------------------------
log "dumping *.localho.st domains from local DB"
DUMP=/tmp/temps-localhost-domains.csv

# `\copy` is a psql-meta command that must be on a single line and
# cannot live inside `-c "..."`. Use --command with the meta syntax
# directly. Same for the cluster-side load: pipe via STDIN through
# `psql -c "\copy ... FROM STDIN ..."` which is the supported form.
"$PSQL_LOCAL" "$LOCAL_DEV_DB_URL" -c "\copy (SELECT domain, certificate, private_key, expiration_time, last_renewed, status, dns_challenge_token, dns_challenge_value, http_challenge_token, http_challenge_key_authorization, last_error, last_error_type, is_wildcard, verification_method, created_at, updated_at FROM domains WHERE domain LIKE '%localho.st%') TO '$DUMP' WITH CSV HEADER"

[[ -s "$DUMP" ]] || fail "dump file is empty"
log "dumped $(wc -l < "$DUMP" | tr -d ' ') row(s) (incl. header) to $DUMP"

# Cluster side. Two psql calls: one to TRUNCATE, one piping the CSV
# via STDIN through \copy. We can't combine them because \copy's
# STDIN consumes whatever follows the meta-command in the script.
log "loading rows into cluster postgres"
docker compose exec -T postgres psql -U temps -d temps -v ON_ERROR_STOP=1 \
  -c "TRUNCATE domains RESTART IDENTITY CASCADE" >/dev/null

docker compose exec -T postgres psql -U temps -d temps -v ON_ERROR_STOP=1 \
  -c "\copy domains (domain, certificate, private_key, expiration_time, last_renewed, status, dns_challenge_token, dns_challenge_value, http_challenge_token, http_challenge_key_authorization, last_error, last_error_type, is_wildcard, verification_method, created_at, updated_at) FROM STDIN WITH CSV HEADER" \
  < "$DUMP" >/dev/null

# ---------------------------------------------------------------------------
# 5. Patch settings so the proxy serves the right wildcard + external URL.
#    setup --auto already wrote *.localho.st / https://app.localho.st but
#    if anyone re-ran setup with different args we make sure we win.
# ---------------------------------------------------------------------------
log "patching settings.data with wildcard_domain + external_url"
docker compose exec -T postgres psql -U temps -d temps -v ON_ERROR_STOP=1 <<'SQL' >/dev/null
UPDATE settings
   SET data = jsonb_set(
                jsonb_set(
                  data::jsonb,
                  '{wildcard_domain}', '"*.localho.st"'
                ),
                '{external_url}', '"https://app.localho.st"'
              )::json
 WHERE id = 1;
SQL

# ---------------------------------------------------------------------------
# 6. Restart control-plane so it picks up the swapped key + new cert
# ---------------------------------------------------------------------------
log "restarting control-plane"
docker compose restart control-plane >/dev/null

# Wait for the listener
log "waiting for HTTPS listener on :443"
for _ in $(seq 1 60); do
  if (echo > /dev/tcp/127.0.0.1/443) 2>/dev/null; then break; fi
  sleep 2
done

echo
echo "$(printf '\033[1;32m✅ cert imported\033[0m')"
echo "   open in browser:  https://app.localho.st"
echo "   admin login:      admin@local.dev / $(sed -n 2p .state/admin.txt 2>/dev/null || echo '<see .state/admin.txt>')"
echo
echo "Quick verification:"
echo "   curl -kIs https://app.localho.st | head -3"
echo
