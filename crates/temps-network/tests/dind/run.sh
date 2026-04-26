#!/usr/bin/env bash
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
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# 0. Preflight
# ---------------------------------------------------------------------------
docker version >/dev/null 2>&1 || fail "docker daemon not available on host"

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
start_node "$NODE_B" "$NODE_B_IP"

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
wait_for_dind "$NODE_B"
log "both inner docker daemons ready"

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
install_toolchain "$NODE_B"

# ---------------------------------------------------------------------------
# 4. Run the kernel-touching tests inside node A
# ---------------------------------------------------------------------------
log "running kernel integration tests in $NODE_A"
docker exec \
  -e TEMPS_IT_LOCAL_NAME=node-a \
  -e TEMPS_IT_LOCAL_CIDR=172.20.1.0/24 \
  -e TEMPS_IT_LOCAL_BRIDGE_IP=172.20.1.1 \
  -e TEMPS_IT_LOCAL_UNDERLAY="$NODE_A_IP" \
  -e TEMPS_IT_PEER_CIDR=172.20.2.0/24 \
  -e TEMPS_IT_PEER_UNDERLAY="$NODE_B_IP" \
  -e TEMPS_RUN_DIND_TESTS=1 \
  "$NODE_A" sh -c '
    cd /workspace
    export PATH=/root/.cargo/bin:$PATH
    cargo test -p temps-network --features integration_kernel --test it_kernel -- --test-threads=1 --nocapture
  ' || fail "kernel tests failed in $NODE_A"

log "kernel integration tests passed in $NODE_A"

# ---------------------------------------------------------------------------
# 5. Two-node cross-host ping scenario
# ---------------------------------------------------------------------------
log "running cross-host scenario (bootstrap both, ping across)"

# Bootstrap node B with peer pointing to node A.
docker exec \
  -e TEMPS_IT_LOCAL_NAME=node-b \
  -e TEMPS_IT_LOCAL_CIDR=172.20.2.0/24 \
  -e TEMPS_IT_LOCAL_BRIDGE_IP=172.20.2.1 \
  -e TEMPS_IT_LOCAL_UNDERLAY="$NODE_B_IP" \
  -e TEMPS_IT_PEER_CIDR=172.20.1.0/24 \
  -e TEMPS_IT_PEER_UNDERLAY="$NODE_A_IP" \
  -e TEMPS_RUN_DIND_TESTS=1 \
  "$NODE_B" sh -c '
    cd /workspace
    export PATH=/root/.cargo/bin:$PATH
    cargo test -p temps-network --features integration_kernel --test it_kernel bootstrap_only -- --test-threads=1 --nocapture
  ' || fail "node-b bootstrap failed"

log "both nodes bootstrapped — running container ping"

docker exec "$NODE_A" docker run -d --rm --name nginx-a --network temps-overlay --ip 172.20.1.10 nginx:alpine >/dev/null
docker exec "$NODE_B" docker run --rm --network temps-overlay --ip 172.20.2.10 alpine sh -c \
    'apk add --no-cache curl >/dev/null && curl -sf -m 5 http://172.20.1.10/ | head -c 100' \
    || fail "node-b -> node-a HTTP failed"

log "✅ cross-host overlay verified"
