#!/usr/bin/env bash
#
# Control-plane bootstrap. Runs inside the privileged DinD container.
# Sequence:
#   1. (entrypoint already started dockerd)
#   2. Build the temps binary if it isn't there or is stale.
#   3. First-boot: run `temps setup --auto` once to seed admin user +
#      database tables. Persists a marker file so restarts skip this.
#   4. exec `temps serve`.
#
# Stdout/stderr go to the compose log so `./dev-cluster logs control-plane`
# shows everything.

set -euo pipefail

WORKSPACE=/workspace
BIN=/usr/local/bin/temps
MARKER=/var/lib/temps/.dev-cluster-setup-done
ADMIN_PASSWORD_FILE=/workspace/tools/dev-cluster/.state/admin.txt

log() { printf '\033[1;36m[control-plane]\033[0m %s\n' "$*"; }

# ---------------------------------------------------------------------------
# 1. Wait for dockerd. Entrypoint already started it but compose can race
#    on first boot. Cheap to re-check.
# ---------------------------------------------------------------------------
for _ in $(seq 1 30); do
  docker info >/dev/null 2>&1 && break || sleep 1
done

# ---------------------------------------------------------------------------
# 2. Build the temps binary if missing or stale. Fast on subsequent
#    boots thanks to the cargo registry cache + workspace target/.
# ---------------------------------------------------------------------------
build_temps() {
  log "building temps binary (cargo build --bin temps)"
  cd "$WORKSPACE"
  cargo build --bin temps
  install -m 0755 "target/debug/temps" "$BIN"
}

if [[ ! -x "$BIN" ]]; then
  build_temps
else
  # Check whether the source has changed since the binary was built.
  # cargo handles the heavy lifting; we just need to know if `cargo
  # build` had anything to do. The simplest way: always re-run; cargo
  # noops in <1s when up-to-date.
  cd "$WORKSPACE"
  cargo build --bin temps
  install -m 0755 "target/debug/temps" "$BIN"
fi
log "temps binary at $BIN ($($BIN --version 2>/dev/null || echo 'unknown'))"

# ---------------------------------------------------------------------------
# 3. First-boot setup. We mark completion so a `down`/`up` of the same
#    postgres volume short-circuits straight to `serve`.
# ---------------------------------------------------------------------------
STATE_DIR="$WORKSPACE/tools/dev-cluster/.state"
JOIN_TOKEN_FILE="$STATE_DIR/join_token.txt"

if [[ ! -f "$MARKER" ]]; then
  log "first boot: running temps setup --auto"
  mkdir -p "$STATE_DIR"

  # --auto implies non-interactive, skip-ssl, skip-dns-records, skip-git.
  # We pass --server-ip 10.42.0.10 because auto-detect probes the public
  # internet and we don't need that here.
  ADMIN_PASSWORD="$(head -c 24 /dev/urandom | base64 | tr -d '/+=' | head -c 16)"

  # --wildcard-domain + --external-url match the localho.st cert that
  # tools/dev-cluster/import-localhost-cert.sh imports from the local
  # temps_development DB. Setup writes them into settings.data; the
  # cert import script overwrites them again afterward but having them
  # set here means setup doesn't probe the public internet for an IP.
  TEMPS_ADMIN_PASSWORD="$ADMIN_PASSWORD" "$BIN" setup \
    --auto \
    --admin-email "admin@local.dev" \
    --server-ip "127.0.0.1" \
    --wildcard-domain "*.localho.st" \
    --external-url "https://app.localho.st" \
    --database-url "$TEMPS_DATABASE_URL" \
    --data-dir "$TEMPS_DATA_DIR" \
    --skip-geolite2-download \
  || {
    log "setup failed; printing logs and exiting"
    exit 1
  }

  printf 'admin@local.dev\n%s\n' "$ADMIN_PASSWORD" > "$ADMIN_PASSWORD_FILE"
  chmod 600 "$ADMIN_PASSWORD_FILE"

  # Generate a join token for the workers. Writing to network_config /
  # settings via the API requires login, which is itself fiddly to
  # script. Cheat: insert the hash directly into the settings table
  # using psql, mirroring what `POST /settings/join-token/generate`
  # does internally.
  JOIN_TOKEN="$(head -c 32 /dev/urandom | xxd -p -c 999)"
  JOIN_TOKEN_HASH="$(printf '%s' "$JOIN_TOKEN" | sha256sum | awk '{print $1}')"
  log "minting join token (hash $(echo "$JOIN_TOKEN_HASH" | cut -c1-12)…)"

  # The settings.data column is plain JSON (not JSONB). Cast to jsonb for
  # the merge, cast back when writing — same effect as
  # POST /settings/join-token/generate but without needing an admin login.
  PGPASSWORD=temps psql -h 10.42.0.5 -U temps -d temps -v ON_ERROR_STOP=1 -c "
    UPDATE settings
       SET data = jsonb_set(
                    COALESCE(data::jsonb, '{}'::jsonb),
                    '{multi_node,join_token_hash}',
                    to_jsonb('${JOIN_TOKEN_HASH}'::text)
                  )::json
     WHERE id = 1
  " >/dev/null

  printf '%s\n' "$JOIN_TOKEN" > "$JOIN_TOKEN_FILE"
  chmod 644 "$JOIN_TOKEN_FILE"

  touch "$MARKER"
  log "setup complete; admin credentials in ${ADMIN_PASSWORD_FILE#$WORKSPACE/}, join token in ${JOIN_TOKEN_FILE#$WORKSPACE/}"
fi

# ---------------------------------------------------------------------------
# 4. Run the API server. This is the long-running command compose
#    keeps alive.
# ---------------------------------------------------------------------------
log "starting temps serve on $TEMPS_ADDRESS (TLS on $TEMPS_TLS_ADDRESS)"
exec "$BIN" serve
