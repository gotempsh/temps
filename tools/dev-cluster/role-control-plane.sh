#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

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

log() { printf '\033[1;36m[control-plane]\033[0m %s\n' "$*"; }

# ---------------------------------------------------------------------------
# 1. Wait for dockerd. Entrypoint already started it but compose can race
#    on first boot. Cheap to re-check.
# ---------------------------------------------------------------------------
for _ in $(seq 1 30); do
  if docker info >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

# ---------------------------------------------------------------------------
# 2. Install a caller-supplied Linux binary when available (CI builds one
#    once and shares it with every scenario); otherwise build from source.
#    The local dev-cluster path remains unchanged and uses Cargo's caches.
# ---------------------------------------------------------------------------
build_temps() {
  log "building temps binary (cargo build --bin temps)"
  cd "$WORKSPACE"
  cargo build --bin temps
  install -m 0755 "target/debug/temps" "$BIN"
}

if [[ -n "${DEV_CLUSTER_PREBUILT_TEMPS_BIN:-}" ]]; then
  if [[ ! -x "$DEV_CLUSTER_PREBUILT_TEMPS_BIN" ]]; then
    log "prebuilt temps binary is not executable: $DEV_CLUSTER_PREBUILT_TEMPS_BIN"
    exit 1
  fi
  log "installing prebuilt temps binary from $DEV_CLUSTER_PREBUILT_TEMPS_BIN"
  install -m 0755 "$DEV_CLUSTER_PREBUILT_TEMPS_BIN" "$BIN"
elif [[ ! -x "$BIN" ]]; then
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
STATE_DIR="${DEV_CLUSTER_STATE_DIR:-$WORKSPACE/tools/dev-cluster/.state}"
ADMIN_PASSWORD_FILE="$STATE_DIR/admin.txt"
JOIN_TOKEN_FILE="$STATE_DIR/join_token.txt"

if [[ ! -f "$MARKER" ]]; then
  log "first boot: running temps setup --auto"
  mkdir -p "$STATE_DIR"
  chmod 700 "$STATE_DIR"

  WILDCARD_DOMAIN="${DEV_CLUSTER_WILDCARD_DOMAIN:-*.localho.st}"
  EXTERNAL_URL="${DEV_CLUSTER_EXTERNAL_URL:-https://app.localho.st}"
  SETUP_CERT_ARGS=()
  if [[ "${DEV_CLUSTER_GENERATE_TLS_CERT:-false}" == "true" ]]; then
    TLS_HOST="${DEV_CLUSTER_TLS_HOST:?DEV_CLUSTER_TLS_HOST is required when generating a TLS certificate}"
    if [[ ! "$TLS_HOST" =~ ^[A-Za-z0-9.-]+$ ]] || [[ ! "$WILDCARD_DOMAIN" =~ ^\*\.[A-Za-z0-9.-]+$ ]]; then
      log "invalid TLS hostname or wildcard domain"
      exit 1
    fi
    TLS_CERT_FILE="$STATE_DIR/control-plane.crt"
    # The setup command imports an encrypted copy into Postgres. Keep the
    # plaintext staging key off persistent volumes and remove it immediately
    # after a successful import.
    TLS_KEY_FILE=/run/temps-control-plane.key
    log "generating throwaway TLS certificate for $TLS_HOST"
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
      -keyout "$TLS_KEY_FILE" -out "$TLS_CERT_FILE" -days 1 -nodes \
      -subj "/CN=$TLS_HOST" \
      -addext "subjectAltName=DNS:$TLS_HOST,DNS:$WILDCARD_DOMAIN" \
      >/dev/null 2>&1
    chmod 600 "$TLS_KEY_FILE"
    chmod 644 "$TLS_CERT_FILE"
    SETUP_CERT_ARGS=(
      --wildcard-domain-cert "$TLS_CERT_FILE"
      --wildcard-domain-key "$TLS_KEY_FILE"
    )
  fi

  # --auto implies non-interactive, skip-ssl, skip-dns-records, skip-git.
  # We pass a loopback --server-ip because auto-detect probes the public
  # internet and these local clusters do not need that here.
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
    --wildcard-domain "$WILDCARD_DOMAIN" \
    --external-url "$EXTERNAL_URL" \
    "${SETUP_CERT_ARGS[@]}" \
    --database-url "$TEMPS_DATABASE_URL" \
    --data-dir "$TEMPS_DATA_DIR" \
    --skip-geolite2-download \
  || {
    log "setup failed; printing logs and exiting"
    exit 1
  }
  if [[ -n "${TLS_KEY_FILE:-}" ]]; then
    rm -f -- "$TLS_KEY_FILE"
    log "removed plaintext TLS staging key after import"
  fi

  printf 'admin@local.dev\n%s\n' "$ADMIN_PASSWORD" > "$ADMIN_PASSWORD_FILE"
  chmod 600 "$ADMIN_PASSWORD_FILE"

  # Generate a bootstrap token for the workers. Dedicated security E2E
  # topologies use a short-lived enrollment token; the normal dev cluster
  # retains its backwards-compatible shared-token behavior.
  JOIN_TOKEN="$(head -c 32 /dev/urandom | xxd -p -c 999)"
  JOIN_TOKEN_HASH="$(printf '%s' "$JOIN_TOKEN" | sha256sum | awk '{print $1}')"
  log "minting join token (hash $(echo "$JOIN_TOKEN_HASH" | cut -c1-12)…)"

  # Derive psql connection args from TEMPS_DATABASE_URL rather than a
  # hardcoded IP, so this script stays reusable by any compose topology
  # that re-subnets this cluster (e.g. apps/temps-e2e's
  # multinode-join-scenario, which clones this compose file onto
  # 10.52.0.0/24 instead of 10.42.0.0/24).
  DB_URL_NO_SCHEME="${TEMPS_DATABASE_URL#postgres://}"
  DB_USER="${DB_URL_NO_SCHEME%%:*}"
  DB_REST="${DB_URL_NO_SCHEME#*:}"
  DB_PASS="${DB_REST%%@*}"
  DB_HOSTPORT_DB="${DB_REST#*@}"
  DB_HOSTPORT="${DB_HOSTPORT_DB%%/*}"
  DB_HOST="${DB_HOSTPORT%%:*}"
  DB_NAME="${DB_HOSTPORT_DB#*/}"

  if [[ "${DEV_CLUSTER_USE_ENROLLMENT_TOKEN:-false}" == "true" ]]; then
    BOUND_NODE_NAME="${DEV_CLUSTER_ENROLLMENT_NODE_NAME:-worker-1}"
    if [[ ! "$BOUND_NODE_NAME" =~ ^[A-Za-z0-9._-]+$ ]]; then
      log "invalid DEV_CLUSTER_ENROLLMENT_NODE_NAME '$BOUND_NODE_NAME'"
      exit 1
    fi
    PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -U "$DB_USER" -d "$DB_NAME" -v ON_ERROR_STOP=1 -c "
      UPDATE settings
         SET data = jsonb_set(
                      jsonb_set(COALESCE(data::jsonb, '{}'::jsonb),
                                '{multi_node,require_mtls}', 'true'::jsonb),
                      '{multi_node,legacy_shared_token_enabled}', 'false'::jsonb
                    )::json
       WHERE id = 1;
      INSERT INTO node_enrollment_tokens
        (token_hash, max_uses, used_count, expires_at, bound_node_name,
         created_at, updated_at)
      VALUES
        ('${JOIN_TOKEN_HASH}', 1, 0, now() + interval '1 hour',
         '${BOUND_NODE_NAME}', now(), now());
    " >/dev/null
    log "minted single-use enrollment token bound to $BOUND_NODE_NAME; mTLS required"
  else
    # The settings.data column is plain JSON (not JSONB). Cast to jsonb for
    # the merge, cast back when writing — same effect as the legacy join-token
    # API without needing an admin login.
    PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -U "$DB_USER" -d "$DB_NAME" -v ON_ERROR_STOP=1 -c "
      UPDATE settings
         SET data = jsonb_set(
                      COALESCE(data::jsonb, '{}'::jsonb),
                      '{multi_node,join_token_hash}',
                      to_jsonb('${JOIN_TOKEN_HASH}'::text)
                    )::json
       WHERE id = 1
    " >/dev/null
  fi

  # Internal *.temps.local resolution is still opt-in. Dedicated E2E
  # topologies enable it before `temps serve` starts so both the control
  # plane and joining workers launch their per-node DNS resolver. Keep the
  # shared development cluster's default unchanged unless explicitly asked.
  if [[ "${DEV_CLUSTER_ENABLE_DNS:-false}" == "true" ]]; then
    PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -U "$DB_USER" -d "$DB_NAME" -v ON_ERROR_STOP=1 -c "
      UPDATE settings
         SET data = jsonb_set(
                      COALESCE(data::jsonb, '{}'::jsonb),
                      '{cluster_dns,enabled}',
                      'true'::jsonb
                    )::json
       WHERE id = 1
    " >/dev/null
    log "enabled internal *.temps.local DNS for this cluster"
  fi

  printf '%s\n' "$JOIN_TOKEN" > "$JOIN_TOKEN_FILE"
  chmod 600 "$JOIN_TOKEN_FILE"

  touch "$MARKER"
  log "setup complete; admin credentials in ${ADMIN_PASSWORD_FILE#"$WORKSPACE"/}, join token in ${JOIN_TOKEN_FILE#"$WORKSPACE"/}"
fi

# ---------------------------------------------------------------------------
# 4. Run the API server. This is the long-running command compose
#    keeps alive.
# ---------------------------------------------------------------------------
log "starting temps serve on $TEMPS_ADDRESS (TLS on ${TEMPS_TLS_ADDRESS:-none})"
exec "$BIN" serve
