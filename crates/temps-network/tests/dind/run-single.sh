#!/usr/bin/env bash
#
# Single-node Linux integration tests.
#
# Runs all kernel-touching tests inside ONE privileged Linux container.
# Faster and easier to debug than the two-node DinD harness — validates
# every NetworkManager surface that doesn't depend on cross-host traffic.
#
# Cross-host ping testing lives in run.sh (the two-DinD harness).
#
# Usage:
#   ./run-single.sh
#   KEEP=1 ./run-single.sh   # leave the container up after exit

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../../../.." && pwd)"

NODE="temps-it-single"
DIND_IMAGE="docker:27-dind"

log()  { printf '\033[1;36m[it-single]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[it-single]\033[0m %s\n' "$*" >&2; exit 1; }

cleanup() {
  if [[ "${KEEP:-0}" = "1" ]]; then
    log "KEEP=1: leaving $NODE running. Re-run cleanup with: docker rm -f $NODE"
    return
  fi
  log "cleaning up"
  docker rm -f "$NODE" >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker version >/dev/null 2>&1 || fail "docker daemon not available"

if docker inspect "$NODE" >/dev/null 2>&1; then
  docker rm -f "$NODE" >/dev/null
fi

log "starting privileged dind: $NODE"
docker run -d --rm \
  --name "$NODE" \
  --hostname "$NODE" \
  --privileged \
  -v "$REPO_ROOT":/workspace \
  -v "${NODE}-cargo-cache":/usr/local/cargo/registry \
  -v "${NODE}-target-cache":/workspace/target \
  -e DOCKER_TLS_CERTDIR="" \
  "$DIND_IMAGE" \
  --tls=false \
  --bridge=none \
  >/dev/null

# Wait for inner docker.
for i in $(seq 1 30); do
  if docker exec "$NODE" docker version >/dev/null 2>&1; then break; fi
  sleep 1
done
docker exec "$NODE" docker version >/dev/null 2>&1 || fail "inner docker never came up"

log "installing toolchain (cached after first run)"
docker exec "$NODE" sh -c '
  set -e
  if ! command -v cargo >/dev/null; then
    apk add --no-cache build-base curl pkgconfig openssl-dev nftables iproute2 bridge >/dev/null
    # Install latest stable rather than pinning a version — workspace deps
    # bump their MSRV regularly (etcetera, home, testcontainers, time all
    # need 1.88+ as of late Apr 2026), and pinning here means the harness
    # breaks every time a transitive crate updates.
    curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal >/dev/null
  fi
'

log "running unit tests (no kernel access required)"
docker exec "$NODE" sh -c '
  cd /workspace
  export PATH=/root/.cargo/bin:$PATH
  cargo test -p temps-network --lib -- --nocapture
' || fail "unit tests failed"

log "running kernel integration tests (privileged, single-node)"
# We use a self-loop "peer" so reconcile/FDB/route logic runs end-to-end
# without needing a second host. The peer underlay is the loopback range,
# which is reachable from inside the container and won't conflict with
# anything real.
docker exec \
  -e TEMPS_IT_LOCAL_CIDR=172.20.1.0/24 \
  -e TEMPS_IT_LOCAL_BRIDGE_IP=172.20.1.1 \
  -e TEMPS_IT_LOCAL_UNDERLAY=127.0.0.1 \
  -e TEMPS_IT_PEER_CIDR=172.20.2.0/24 \
  -e TEMPS_IT_PEER_UNDERLAY=127.0.0.2 \
  -e TEMPS_IT_UNDERLAY_DEV=eth0 \
  -e TEMPS_RUN_DIND_TESTS=1 \
  "$NODE" sh -c '
    cd /workspace
    export PATH=/root/.cargo/bin:$PATH
    cargo test -p temps-network --features integration_kernel --test it_kernel \
      -- --test-threads=1 --nocapture
  ' || fail "kernel tests failed"

log "✅ all tests passed"
