#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0


set -euo pipefail

SHARD="${1:?usage: run-temps-e2e-shard.sh <core|observability|services|recovery|edge|examples>}"
E2E_DIR="${TEMPS_E2E_DIR:-apps/temps-e2e}"
REGISTRY="${TEMPS_E2E_REGISTRY:-localhost:5111}"
DATABASE_URL="${TEMPS_DATABASE_URL:-postgresql://temps:temps@localhost:5432/temps}"

run_scenario() {
  local name="$1"
  shift
  echo
  echo "================================================================"
  echo "Running apps/temps-e2e: $name"
  echo "================================================================"
  (
    cd "$E2E_DIR"
    bun run src/index.ts "$name" "$@" --json
  )
}

case "$SHARD" in
  core)
    run_scenario scenario --image traefik/whoami:latest --port 80 --with-db --requests 50 --concurrency 10
    run_scenario cli-scenario
    run_scenario flags-scenario
    run_scenario audit-scenario
    run_scenario error-tracking-scenario
    run_scenario env-vars-scenario --registry "$REGISTRY"
    run_scenario api-key-scenario --temps-root "$PWD" --database-url "$DATABASE_URL"
    run_scenario rbac-scenario --image traefik/whoami:latest --temps-root "$PWD" --database-url "$DATABASE_URL"
    ;;
  observability)
    run_scenario logs-scenario --registry "$REGISTRY"
    run_scenario analytics-scenario --registry "$REGISTRY"
    run_scenario session-replay-scenario --registry "$REGISTRY"
    run_scenario monitoring-scenario --registry "$REGISTRY"
    run_scenario otel-quota-scenario
    ;;
  services)
    run_scenario managed-services-scenario --registry "$REGISTRY"
    run_scenario kv-scenario
    run_scenario blob-scenario
    run_scenario db-ha-failover-scenario --registry "$REGISTRY"
    run_scenario deploy-lifecycle-scenario --registry "$REGISTRY"
    ;;
  recovery)
    run_scenario backup-restore-scenario --registry "$REGISTRY"
    run_scenario pitr-scenario --registry "$REGISTRY"
    run_scenario redis-restore-scenario --registry "$REGISTRY"
    run_scenario mongodb-restore-scenario
    run_scenario s3-restore-scenario
    run_scenario mariadb-restore-scenario
    run_scenario pg-upgrade-scenario --registry "$REGISTRY" --upgrade-timeout 900000
    ;;
  edge)
    run_scenario tls-scenario --image traefik/whoami:latest --port 80 --tls-port 3443
    run_scenario dns01-wildcard-scenario
    run_scenario email-scenario
    run_scenario git-deploy-scenario
    ;;
  examples)
    run_scenario examples --all --registry "$REGISTRY" --requests 25 --concurrency 5
    ;;
  *)
    echo "Unknown E2E shard: $SHARD" >&2
    exit 2
    ;;
esac
