#!/usr/bin/env bash
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
STATE_DIR="$WORKSPACE/tools/dev-cluster/.state"
JOIN_TOKEN_FILE="$STATE_DIR/join_token.txt"
JOIN_MARKER="/var/lib/temps/.dev-cluster-join-done"

WORKER_NAME="${WORKER_NAME:?WORKER_NAME env var required}"
WORKER_UNDERLAY_IP="${WORKER_UNDERLAY_IP:?WORKER_UNDERLAY_IP env var required}"
CONTROL_PLANE_URL="${CONTROL_PLANE_URL:?CONTROL_PLANE_URL env var required}"

log() { printf '\033[1;33m[%s]\033[0m %s\n' "$WORKER_NAME" "$*"; }

# 1. dockerd
for _ in $(seq 1 30); do
  docker info >/dev/null 2>&1 && break || sleep 1
done

# 2. binary
cd "$WORKSPACE"
log "ensuring temps binary is up to date"
cargo build --bin temps >&2
install -m 0755 "$WORKSPACE/target/debug/temps" "$BIN"

# 3. wait for join token (control plane writes it during its first boot)
log "waiting for join token at ${JOIN_TOKEN_FILE#$WORKSPACE/}"
for _ in $(seq 1 120); do
  if [[ -f "$JOIN_TOKEN_FILE" ]]; then break; fi
  sleep 1
done
if [[ ! -f "$JOIN_TOKEN_FILE" ]]; then
  log "join token never appeared; control plane probably failed to set up"
  exit 1
fi
JOIN_TOKEN="$(cat "$JOIN_TOKEN_FILE")"

# 4. join (idempotent: skip on subsequent boots)
if [[ ! -f "$JOIN_MARKER" ]]; then
  # Wait until the control plane's HTTP listener actually accepts a
  # connection. Compose's depends_on (service_healthy) gives us
  # *listener up*, but in race conditions the worker still wins the
  # boot race. We use a simple TCP probe via bash /dev/tcp because
  # curl on `Connection refused` returns in <1ms — without an
  # explicit sleep AFTER the failure, the loop burns 60 iterations in
  # microseconds and we proceed to `temps join` against a closed port.
  CP_HOST="${CONTROL_PLANE_URL#http://}"; CP_HOST="${CP_HOST%%/*}"
  CP_PROBE_HOST="${CP_HOST%:*}"
  CP_PROBE_PORT="${CP_HOST#*:}"
  [[ "$CP_PROBE_PORT" == "$CP_HOST" ]] && CP_PROBE_PORT=80
  log "waiting for control plane at $CP_PROBE_HOST:$CP_PROBE_PORT"
  for _ in $(seq 1 90); do
    if (exec 3<>/dev/tcp/"$CP_PROBE_HOST"/"$CP_PROBE_PORT") 2>/dev/null; then
      exec 3<&-; exec 3>&-
      break
    fi
    sleep 2
  done

  log "joining cluster as $WORKER_NAME ($WORKER_UNDERLAY_IP)"
  TEMPS_JOIN_TOKEN="$JOIN_TOKEN" "$BIN" join \
    "$CONTROL_PLANE_URL" "$JOIN_TOKEN" \
    --name "$WORKER_NAME" \
    --private-address "$WORKER_UNDERLAY_IP" \
    --agent-address "0.0.0.0:3100" \
    || {
      log "join failed; will retry on next container start"
      exit 1
    }
  touch "$JOIN_MARKER"
  log "joined cluster successfully"
else
  log "already joined (marker present); skipping registration"
fi

# 5. run the agent. Reads ~/.temps/agent.json that `temps join` wrote.
log "starting temps agent"
exec "$BIN" agent
