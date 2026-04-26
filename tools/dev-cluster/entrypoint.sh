#!/usr/bin/env bash
#
# DinD entrypoint. Starts dockerd in the background (so the inner Docker
# daemon is available for temps deployments), waits for the socket, then
# execs whatever command compose passed.
#
# When the container's role is `noop` (just sit there), we tail
# /dev/null so the container stays alive — useful when a worker hasn't
# been seeded yet.

set -euo pipefail

start_dockerd() {
  if pgrep -x dockerd >/dev/null 2>&1; then
    return
  fi
  # --bridge=none keeps Docker from creating its default docker0 on a
  # CIDR that might collide with our compute_cidr pool. We let the temps
  # control plane (and `temps-network`) create the bridges it needs.
  dockerd \
    --host=unix:///var/run/docker.sock \
    --bridge=none \
    --iptables=true \
    --log-level=warn \
    >/var/log/docker.log 2>&1 &

  for _ in $(seq 1 30); do
    if docker info >/dev/null 2>&1; then return; fi
    sleep 1
  done
  echo "[entrypoint] dockerd failed to start; tail of /var/log/docker.log:" >&2
  tail -n 50 /var/log/docker.log >&2 || true
  exit 1
}

start_dockerd

# Honour any args. Default `bash` keeps the container alive interactively.
exec "$@"
