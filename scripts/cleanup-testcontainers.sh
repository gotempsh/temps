#!/usr/bin/env bash
# cleanup-testcontainers.sh — Remove orphaned `testcontainers`-managed
# Docker containers left over from interrupted `cargo test` runs.
#
# testcontainers-rs (used by TestDatabase and friends, see
# crates/temps-database/src/test_utils.rs) relies on a companion "Ryuk"
# reaper container to guarantee container cleanup even when the test
# process dies ungracefully (crash, SIGKILL, a tool/CI timeout). Ryuk does
# not reliably register/complete on every machine — notably Docker Desktop
# for Mac's VM-based networking is a known trouble spot for it — so a
# killed test run can leave its Postgres/Timescale containers running
# forever, each holding open DB connections, until something notices
# Postgres has hit `max_connections` and refuses new connections entirely.
#
# CI carries its own safety net for this (a `docker system prune -f` step
# in .github/workflows/e2e-tests.yml), but `prune` only removes STOPPED
# containers — it does nothing for orphans that are still running, which
# is the actual failure mode this script targets. There is no local
# equivalent of CI's step, so orphans accumulate silently across every
# local `cargo test` invocation until this script (or a manual `docker rm
# -f`) is run.
#
# Scoped by the `org.testcontainers.managed-by=testcontainers` label that
# testcontainers-rs stamps on every container it creates, so this only
# ever touches test infrastructure — never a real dev/demo/deployment
# container running on the same Docker daemon.
#
# Usage:
#   scripts/cleanup-testcontainers.sh          # remove orphaned testcontainers
#   scripts/cleanup-testcontainers.sh --dry-run  # list without removing

set -euo pipefail

LABEL="org.testcontainers.managed-by=testcontainers"

if ! command -v docker >/dev/null 2>&1; then
  echo "docker not found — nothing to do." >&2
  exit 0
fi

IDS=$(docker ps -aq --filter "label=${LABEL}")

if [ -z "$IDS" ]; then
  echo "No orphaned testcontainers found."
  exit 0
fi

COUNT=$(echo "$IDS" | wc -l | tr -d ' ')

if [ "${1:-}" = "--dry-run" ]; then
  echo "Would remove ${COUNT} orphaned testcontainers:"
  docker ps -a --filter "label=${LABEL}" --format "  {{.Names}}\t{{.Image}}\t{{.Status}}"
  exit 0
fi

echo "Removing ${COUNT} orphaned testcontainers..."
# shellcheck disable=SC2086
docker rm -f $IDS >/dev/null
echo "Done. Run 'docker ps -a --filter \"label=${LABEL}\"' to verify none remain."
