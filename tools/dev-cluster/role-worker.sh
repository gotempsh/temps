#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

#
# Worker bootstrap. Runs inside a privileged DinD container.
#
# Sequence:
#   1. (entrypoint already started dockerd)
#   2. Build the temps binary if missing/stale (shares the cargo cache
#      with the control-plane container so this is fast).
#   3. Wait for the control plane to publish a join token.
#   4. Run `temps join` once to register and persist agent.json. Marker
#      file means subsequent restarts skip this.
#   5. exec `temps agent`.

set -euo pipefail

WORKSPACE=/workspace
BIN=/usr/local/bin/temps
STATE_DIR="${DEV_CLUSTER_STATE_DIR:-$WORKSPACE/tools/dev-cluster/.state}"
JOIN_TOKEN_FILE="$STATE_DIR/join_token.txt"
JOIN_MARKER="/var/lib/temps/.dev-cluster-join-done"

WORKER_NAME="${WORKER_NAME:?WORKER_NAME env var required}"
WORKER_UNDERLAY_IP="${WORKER_UNDERLAY_IP:?WORKER_UNDERLAY_IP env var required}"
CONTROL_PLANE_URL="${CONTROL_PLANE_URL:?CONTROL_PLANE_URL env var required}"

log() { printf '\033[1;33m[%s]\033[0m %s\n' "$WORKER_NAME" "$*"; }

# 1. dockerd
for _ in $(seq 1 30); do
  if docker info >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

# 2. binary. CI supplies the same already-tested Linux binary used by the
# other scenario shards; local dev-cluster runs continue to build from source.
if [[ -n "${DEV_CLUSTER_PREBUILT_TEMPS_BIN:-}" ]]; then
  if [[ ! -x "$DEV_CLUSTER_PREBUILT_TEMPS_BIN" ]]; then
    log "prebuilt temps binary is not executable: $DEV_CLUSTER_PREBUILT_TEMPS_BIN"
    exit 1
  fi
  log "installing prebuilt temps binary from $DEV_CLUSTER_PREBUILT_TEMPS_BIN"
  install -m 0755 "$DEV_CLUSTER_PREBUILT_TEMPS_BIN" "$BIN"
else
  cd "$WORKSPACE"
  log "ensuring temps binary is up to date"
  cargo build --bin temps >&2
  install -m 0755 "$WORKSPACE/target/debug/temps" "$BIN"
fi

# 3. wait for join token (control plane writes it during its first boot)
log "waiting for join token at ${JOIN_TOKEN_FILE#"$WORKSPACE"/}"
for _ in $(seq 1 120); do
  if [[ -f "$JOIN_TOKEN_FILE" ]]; then break; fi
  sleep 1
done
if [[ ! -f "$JOIN_TOKEN_FILE" ]]; then
  log "join token never appeared; control plane probably failed to set up"
  exit 1
fi
JOIN_TOKEN="$(cat "$JOIN_TOKEN_FILE")"

# Install the test topology's control-plane trust anchor on every container
# boot. The join marker is volume-backed, while the system trust store is not,
# so doing this only during the first join would break recreated workers.
if [[ -n "${DEV_CLUSTER_CONTROL_PLANE_CA:-}" ]]; then
  if [[ ! -s "$DEV_CLUSTER_CONTROL_PLANE_CA" ]]; then
    log "control-plane CA certificate is missing: $DEV_CLUSTER_CONTROL_PLANE_CA"
    exit 1
  fi
  install -m 0644 "$DEV_CLUSTER_CONTROL_PLANE_CA" \
    /usr/local/share/ca-certificates/temps-dev-cluster-control-plane.crt
  update-ca-certificates >/dev/null
  log "trusted control-plane TLS certificate"
fi

# 4. join (idempotent: skip on subsequent boots)
if [[ ! -f "$JOIN_MARKER" ]]; then
  # Wait until the control plane's HTTP listener actually accepts a
  # connection. Compose's depends_on (service_healthy) gives us
  # *listener up*, but in race conditions the worker still wins the
  # boot race. We use a simple TCP probe via bash /dev/tcp because
  # curl on `Connection refused` returns in <1ms — without an
  # explicit sleep AFTER the failure, the loop burns 60 iterations in
  # microseconds and we proceed to `temps join` against a closed port.
  CP_SCHEME="${CONTROL_PLANE_URL%%://*}"
  CP_HOST="${CONTROL_PLANE_URL#*://}"; CP_HOST="${CP_HOST%%/*}"
  CP_PROBE_HOST="${CP_HOST%:*}"
  CP_PROBE_PORT="${CP_HOST#*:}"
  if [[ "$CP_PROBE_PORT" == "$CP_HOST" ]]; then
    [[ "$CP_SCHEME" == "https" ]] && CP_PROBE_PORT=443 || CP_PROBE_PORT=80
  fi
  log "waiting for control plane at $CP_PROBE_HOST:$CP_PROBE_PORT"
  for _ in $(seq 1 90); do
    if (exec 3<>/dev/tcp/"$CP_PROBE_HOST"/"$CP_PROBE_PORT") 2>/dev/null; then
      exec 3<&-; exec 3>&-
      break
    fi
    sleep 2
  done

  # The proxy listener becomes reachable before the background console API is
  # ready. A one-shot join can therefore receive the proxy's temporary 503,
  # exit the role script, and force a full DinD container restart. Retry in the
  # same boot instead; failed pre-readiness requests do not consume the token.
  joined=false
  for attempt in $(seq 1 90); do
    log "joining cluster as $WORKER_NAME ($WORKER_UNDERLAY_IP), attempt $attempt/90"
    if TEMPS_JOIN_TOKEN="$JOIN_TOKEN" "$BIN" join \
      "$CONTROL_PLANE_URL" "$JOIN_TOKEN" \
      --name "$WORKER_NAME" \
      --private-address "$WORKER_UNDERLAY_IP" \
      --agent-address "0.0.0.0:3100"; then
      joined=true
      break
    fi
    sleep 2
  done
  if [[ "$joined" != true ]]; then
    log "join failed after 90 attempts"
    exit 1
  fi
  touch "$JOIN_MARKER"
  log "joined cluster successfully"
else
  log "already joined (marker present); skipping registration"
fi

# 5. run the agent. Reads ~/.temps/agent.json that `temps join` wrote.
log "starting temps agent"
exec "$BIN" agent
