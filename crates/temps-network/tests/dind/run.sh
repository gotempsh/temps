#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

#
# End-to-end DinD harness for crates/temps-network.
#
# Brings up two privileged docker:dind containers on a dedicated underlay
# bridge, copies the workspace into each, and runs `cargo test
# --features integration_kernel` so the Rust integration tests actually
# touch a real kernel + real Docker daemon.
#
# Usage:
#   ./run.sh           # full run, fails on any assertion error
#   KEEP=1 ./run.sh    # leave the dind containers running after exit
#                      # (useful for `docker exec -it node-a sh` debugging)

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../../../.." && pwd)"

UNDERLAY_NET="temps-it-underlay"
UNDERLAY_CIDR="10.123.0.0/24"
NODE_A="temps-it-node-a"
NODE_A_IP="10.123.0.2"
NODE_B="temps-it-node-b"
NODE_B_IP="10.123.0.3"
DIND_IMAGE="docker:27-dind"
RUST_IMAGE="rust:1.85-bookworm"
CONTROL_PLANE_READY_FILE="/workspace/.temps-it-control-plane-ready"
WORKER_READY_FILE="/workspace/.temps-it-worker-ready"
EXISTING_APP_NETWORK="temps-app-network"
EXISTING_APP_CIDR="172.31.0.0/16"
EXISTING_APP_CONTAINER="existing-control-plane-app"
EXISTING_CUSTOM_ROUTE="192.168.240.0/24"
EXISTING_CUSTOM_ROUTE_DEV="temps-existing0"

log() { printf '\033[1;36m[it]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[it]\033[0m %s\n' "$*" >&2; exit 1; }

cleanup() {
  if [[ "${KEEP:-0}" = "1" ]]; then
    log "KEEP=1 set; leaving containers and network up"
    return
  fi
  log "cleaning up"
  docker rm -f "$NODE_A" "$NODE_B" >/dev/null 2>&1 || true
  docker network rm "$UNDERLAY_NET" >/dev/null 2>&1 || true
  rm -f "$REPO_ROOT/.temps-it-control-plane-ready" "$REPO_ROOT/.temps-it-worker-ready"
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# 0. Preflight
# ---------------------------------------------------------------------------
docker version >/dev/null 2>&1 || fail "docker daemon not available on host"
# A prior KEEP=1 run must not make the worker exist before the control plane.
docker rm -f "$NODE_A" "$NODE_B" >/dev/null 2>&1 || true
rm -f "$REPO_ROOT/.temps-it-control-plane-ready" "$REPO_ROOT/.temps-it-worker-ready"

# ---------------------------------------------------------------------------
# 1. Underlay network — plays the role of "the cloud private network"
# ---------------------------------------------------------------------------
if ! docker network inspect "$UNDERLAY_NET" >/dev/null 2>&1; then
  log "creating underlay network $UNDERLAY_NET ($UNDERLAY_CIDR)"
  docker network create --driver bridge --subnet "$UNDERLAY_CIDR" "$UNDERLAY_NET" >/dev/null
fi

# ---------------------------------------------------------------------------
# 2. Two DinD nodes
# ---------------------------------------------------------------------------
start_node() {
  local name="$1" ip="$2"
  if docker inspect "$name" >/dev/null 2>&1; then
    docker rm -f "$name" >/dev/null
  fi
  log "starting $name at $ip"
  docker run -d --rm \
    --name "$name" \
    --hostname "$name" \
    --privileged \
    --network "$UNDERLAY_NET" \
    --ip "$ip" \
    -v "$REPO_ROOT":/workspace \
    -v "${name}-cargo-cache":/usr/local/cargo/registry \
    -v "${name}-target-cache":/workspace/target \
    -e DOCKER_TLS_CERTDIR="" \
    "$DIND_IMAGE" \
    --tls=false \
    --bridge=none \
    >/dev/null
}

start_node "$NODE_A" "$NODE_A_IP"

# Wait for inner docker daemons to be ready.
wait_for_dind() {
  local name="$1"
  for i in $(seq 1 30); do
    if docker exec "$name" docker version >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  fail "$name: inner docker daemon never came up"
}
wait_for_dind "$NODE_A"
log "control-plane inner docker daemon ready (worker intentionally absent)"

# ---------------------------------------------------------------------------
# 3. Install Rust + test deps inside each DinD
# ---------------------------------------------------------------------------
install_toolchain() {
  local name="$1"
  log "installing toolchain in $name (cached after first run)"
  docker exec "$name" sh -c '
    set -e
    if ! command -v cargo >/dev/null; then
      apk add --no-cache build-base curl pkgconfig openssl-dev nftables iproute2 bridge-utils >/dev/null
      # Install latest stable rather than pinning. Workspace deps bump
      # their MSRV regularly; pinning here means the harness breaks
      # every time a transitive crate updates.
      curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal >/dev/null
    fi
  '
}

install_toolchain "$NODE_A"

# ---------------------------------------------------------------------------
# 4. Run the kernel-touching tests inside node A
# ---------------------------------------------------------------------------
# `cargo test` runs tests in alphabetical order, not declaration order, and
# every other test in this file calls the `fixture()` helper, which tears
# down all kernel/docker state (`cleanup_all()`) before it runs. bootstrap_only
# is deliberately the one test that does NOT clean up after itself, so the
# node-B step and the container-ping step below can find its state — but
# alphabetically it sorts before `bridge_address_outside_cidr_rejected`,
# `docker_cidr_collision_is_detected`, `reconcile_peers_*`, and
# `teardown_removes_everything_and_is_idempotent`, all of which run after it
# and immediately wipe out what it left behind. Run it as its own filtered
# pass, strictly after everything else, so its state actually survives to
# the container-ping step.
log "running kernel integration tests in $NODE_A"
docker exec \
  -e TEMPS_IT_LOCAL_NAME=node-a \
  -e TEMPS_IT_LOCAL_CIDR=10.240.1.0/24 \
  -e TEMPS_IT_LOCAL_BRIDGE_IP=10.240.1.1 \
  -e TEMPS_IT_LOCAL_UNDERLAY="$NODE_A_IP" \
  -e TEMPS_IT_PEER_CIDR=10.240.2.0/24 \
  -e TEMPS_IT_PEER_UNDERLAY="$NODE_B_IP" \
  -e TEMPS_IT_CLUSTER_POOL=10.240.0.0/16 \
  -e TEMPS_IT_EXISTING_CIDR=10.240.99.0/24 \
  -e TEMPS_RUN_DIND_TESTS=1 \
  "$NODE_A" sh -c '
    cd /workspace
    export PATH=/root/.cargo/bin:$PATH
    cargo test -p temps-network --features integration_kernel,control_plane --test it_kernel -- --skip bootstrap_only --test-threads=1 --nocapture
  ' || fail "kernel tests failed in $NODE_A"

log "kernel integration tests passed in $NODE_A"

log "proving an existing CIDR is rejected before any control-plane state is created"
docker exec \
  -e TEMPS_IT_LOCAL_NAME=node-a \
  -e TEMPS_IT_LOCAL_CIDR=10.240.1.0/24 \
  -e TEMPS_IT_LOCAL_BRIDGE_IP=10.240.1.1 \
  -e TEMPS_IT_LOCAL_UNDERLAY="$NODE_A_IP" \
  -e TEMPS_IT_PEER_CIDR=10.240.2.0/24 \
  -e TEMPS_IT_PEER_UNDERLAY="$NODE_B_IP" \
  -e TEMPS_IT_CLUSTER_POOL=10.240.0.0/16 \
  -e TEMPS_IT_PHASE_TESTS=1 \
  -e TEMPS_IT_EXISTING_CIDR=10.240.99.0/24 \
  -e TEMPS_RUN_DIND_TESTS=1 \
  "$NODE_A" sh -c '
    cd /workspace
    export PATH=/root/.cargo/bin:$PATH
    cargo test -p temps-network --features integration_kernel,control_plane --test it_kernel full_pool_collision_is_rejected_without_partial_kernel_state -- --exact --test-threads=1 --nocapture
  ' || fail "existing CIDR preflight regression failed"

# Model an established single-node installation before multi-node networking
# is enabled. The application network, a running workload, and an unrelated
# operator-managed host route must all survive control-plane overlay setup and
# the later arrival of a worker.
log "creating an existing control-plane app network, workload, and custom route"
docker exec "$NODE_A" sh -ec "
  docker network rm '$EXISTING_APP_NETWORK' >/dev/null 2>&1 || true
  ip link del '$EXISTING_CUSTOM_ROUTE_DEV' >/dev/null 2>&1 || true
  docker network create --driver bridge --subnet '$EXISTING_APP_CIDR' '$EXISTING_APP_NETWORK' >/dev/null
  docker run -d --rm --name '$EXISTING_APP_CONTAINER' \
    --network '$EXISTING_APP_NETWORK' nginx:alpine >/dev/null
  ip link add '$EXISTING_CUSTOM_ROUTE_DEV' type dummy
  ip link set '$EXISTING_CUSTOM_ROUTE_DEV' up
  ip route add '$EXISTING_CUSTOM_ROUTE' dev '$EXISTING_CUSTOM_ROUTE_DEV'
"

log "starting the control plane alone and keeping its manager alive"
docker exec \
  -e TEMPS_IT_LOCAL_NAME=node-a \
  -e TEMPS_IT_LOCAL_CIDR=10.240.1.0/24 \
  -e TEMPS_IT_LOCAL_BRIDGE_IP=10.240.1.1 \
  -e TEMPS_IT_LOCAL_UNDERLAY="$NODE_A_IP" \
  -e TEMPS_IT_PEER_CIDR=10.240.2.0/24 \
  -e TEMPS_IT_PEER_UNDERLAY="$NODE_B_IP" \
  -e TEMPS_IT_CLUSTER_POOL=10.240.0.0/16 \
  -e TEMPS_IT_PHASE_TESTS=1 \
  -e TEMPS_IT_CONTROL_PLANE_READY_FILE="$CONTROL_PLANE_READY_FILE" \
  -e TEMPS_IT_WORKER_READY_FILE="$WORKER_READY_FILE" \
  -e TEMPS_IT_EXISTING_APP_NETWORK="$EXISTING_APP_NETWORK" \
  -e TEMPS_IT_EXISTING_APP_CIDR="$EXISTING_APP_CIDR" \
  -e TEMPS_IT_EXISTING_APP_CONTAINER="$EXISTING_APP_CONTAINER" \
  -e TEMPS_IT_EXISTING_CUSTOM_ROUTE="$EXISTING_CUSTOM_ROUTE" \
  -e TEMPS_RUN_DIND_TESTS=1 \
  "$NODE_A" sh -c '
    cd /workspace
    export PATH=/root/.cargo/bin:$PATH
    cargo test -p temps-network --features integration_kernel,control_plane --test it_kernel control_plane_stays_running_and_reconciles_worker_later -- --exact --test-threads=1 --nocapture
  ' &
CONTROL_PLANE_TEST_PID=$!

for _ in $(seq 1 1800); do
  if [[ -f "$REPO_ROOT/.temps-it-control-plane-ready" ]]; then
    break
  fi
  if ! kill -0 "$CONTROL_PLANE_TEST_PID" >/dev/null 2>&1; then
    wait "$CONTROL_PLANE_TEST_PID" || true
    fail "control-plane lifecycle test exited before publishing readiness"
  fi
  sleep 0.1
done
[[ -f "$REPO_ROOT/.temps-it-control-plane-ready" ]] \
  || fail "control plane did not become ready before the worker-start deadline"

log "starting the worker after the control plane is already healthy"
start_node "$NODE_B" "$NODE_B_IP"
wait_for_dind "$NODE_B"
install_toolchain "$NODE_B"

# ---------------------------------------------------------------------------
# 5. Two-node cross-host ping scenario
# ---------------------------------------------------------------------------
log "running cross-host scenario (bootstrap both, ping across)"

# Preserve a pre-existing operator policy. Temps must add its hook after this
# rule rather than inserting at the head of DOCKER-USER.
docker exec "$NODE_B" iptables -I DOCKER-USER 1 -s 192.0.2.1/32 -j DROP

# Bootstrap node B with peer pointing to node A.
docker exec \
  -e TEMPS_IT_LOCAL_NAME=node-b \
  -e TEMPS_IT_LOCAL_CIDR=10.240.2.0/24 \
  -e TEMPS_IT_LOCAL_BRIDGE_IP=10.240.2.1 \
  -e TEMPS_IT_LOCAL_UNDERLAY="$NODE_B_IP" \
  -e TEMPS_IT_PEER_CIDR=10.240.1.0/24 \
  -e TEMPS_IT_PEER_UNDERLAY="$NODE_A_IP" \
  -e TEMPS_IT_CLUSTER_POOL=10.240.0.0/16 \
  -e TEMPS_RUN_DIND_TESTS=1 \
  "$NODE_B" sh -c '
    cd /workspace
    export PATH=/root/.cargo/bin:$PATH
    cargo test -p temps-network --features integration_kernel,control_plane --test it_kernel bootstrap_only -- --test-threads=1 --nocapture
  ' || fail "node-b bootstrap failed"

log "signalling the still-running control plane that the worker is ready"
docker exec "$NODE_B" touch "$WORKER_READY_FILE"
wait "$CONTROL_PLANE_TEST_PID" || fail "live late-worker reconcile failed"

NODE_B_USER_RULES="$(docker exec "$NODE_B" iptables -S DOCKER-USER)"
OPERATOR_LINE="$(printf '%s\n' "$NODE_B_USER_RULES" | grep -n -- '-s 192.0.2.1/32 -j DROP' | cut -d: -f1)"
TEMPS_HOOK_LINE="$(printf '%s\n' "$NODE_B_USER_RULES" | grep -n -- 'temps-overlay-forward-hook-v1' | cut -d: -f1)"
test -n "$OPERATOR_LINE" -a -n "$TEMPS_HOOK_LINE" -a "$OPERATOR_LINE" -lt "$TEMPS_HOOK_LINE" \
  || fail "Temps overlay hook bypassed pre-existing DOCKER-USER policy"

log "proving the existing control-plane workload and custom route survived"
docker exec "$NODE_A" docker network inspect "$EXISTING_APP_NETWORK" >/dev/null \
  || fail "existing control-plane app network was removed"

docker exec "$NODE_A" ip -4 route show "$EXISTING_APP_CIDR" \
  | grep -q "$EXISTING_APP_CIDR" \
  || fail "existing control-plane app-network route was removed"

docker exec "$NODE_A" ip -4 route show "$EXISTING_CUSTOM_ROUTE" \
  | grep -q "$EXISTING_CUSTOM_ROUTE" \
  || fail "existing operator-managed custom route was removed"

EXISTING_APP_RUNNING="$(
  docker exec "$NODE_A" docker inspect -f '{{.State.Running}}' "$EXISTING_APP_CONTAINER"
)"
test "$EXISTING_APP_RUNNING" = true \
  || fail "existing control-plane workload stopped during multi-node setup"

EXISTING_APP_IP="$(
  docker exec "$NODE_A" docker inspect \
    -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' \
    "$EXISTING_APP_CONTAINER"
)"
test -n "$EXISTING_APP_IP" \
  || fail "existing control-plane workload lost its app-network address"

docker exec "$NODE_A" docker run --rm --network "$EXISTING_APP_NETWORK" \
  alpine wget -q -T 3 -O - "http://$EXISTING_APP_IP/" \
  | grep -q "Welcome to nginx" \
  || fail "existing control-plane workload stopped serving traffic"

# Production database services are attached to temps-app-network first and to
# the multi-node overlay second. Their default route therefore remains on the
# app network. Without the scoped cross-node SNAT rule their reply to a worker
# leaves through that wrong default route and the connection times out.
docker exec "$NODE_A" docker network connect \
  --ip 10.240.1.20 temps-overlay "$EXISTING_APP_CONTAINER"

docker exec "$NODE_A" docker exec "$EXISTING_APP_CONTAINER" \
  awk '$2 == "00000000" { print $1 }' /proc/net/route \
  | grep -q '^eth0$' \
  || fail "dual-network fixture no longer reproduces app-network default routing"

log "both nodes bootstrapped — running container ping"

# Mirror production Docker hosts, where FORWARD defaults to DROP. Remote
# overlay frames enter through vxlan-temps0 rather than br-temps0; without the
# TEMPS_OVERLAY_FORWARD hook this exact setup times out even though ARP works.
docker exec "$NODE_A" iptables -P FORWARD DROP
docker exec "$NODE_B" iptables -P FORWARD DROP

docker exec "$NODE_A" docker run -d --rm --name nginx-a --network temps-overlay --ip 10.240.1.10 nginx:alpine >/dev/null
docker exec "$NODE_B" docker run --rm --network temps-overlay --ip 10.240.2.10 alpine sh -ec '
  attempts=0
  while [ "$attempts" -lt 30 ]; do
    if wget -q -T 2 -O /tmp/nginx-response http://10.240.1.10/ \
      && grep -q "Welcome to nginx" /tmp/nginx-response; then
      exit 0
    fi
    attempts=$((attempts + 1))
    sleep 1
  done
  echo "expected nginx response was not received after 30 attempts" >&2
  exit 1
' || fail "node-b -> node-a HTTP failed"

docker exec "$NODE_B" docker run --rm --network temps-overlay --ip 10.240.2.20 alpine sh -ec '
  attempts=0
  while [ "$attempts" -lt 30 ]; do
    if wget -q -T 2 -O /tmp/nginx-response http://10.240.1.20/ \
      && grep -q "Welcome to nginx" /tmp/nginx-response; then
      exit 0
    fi
    attempts=$((attempts + 1))
    sleep 1
  done
  echo "expected dual-network nginx response was not received after 30 attempts" >&2
  exit 1
' || fail "worker could not reach dual-network control-plane service"

log "✅ cross-host overlay verified"
