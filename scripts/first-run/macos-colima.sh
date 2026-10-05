#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Run the first-run suite and/or the quiet-logs soak on a Mac, against Docker
# provided by Colima (or Docker Desktop / OrbStack). GitHub's macOS runners
# have no Docker, so this is the macOS variant of .github/workflows/first-run.yml.
#
#   scripts/first-run/macos-colima.sh [first-run|quiet-logs|all]   (default: all)
#
# Environment (all optional):
#   TEMPS_BIN       temps binary to test; built with `cargo build --release` if unset
#   DATABASE_URL    an EMPTY database; if unset a throwaway TimescaleDB container
#                   (temps-first-run-db-<pid>) is started and removed afterwards
#   SOAK_MINUTES    quiet-logs idle period (20; use 1440 for the 24 h soak)
#   WARN_PER_HOUR   quiet-logs WARN budget per module per hour (12)
#   FIRST_RUN_GIT_URL / FIRST_RUN_GIT_BRANCH   where git-dockerfile clones from
#   FIRST_RUN_ADDRESS / FIRST_RUN_CONSOLE / FIRST_RUN_TLS   listen addresses
#   KEEP_RUNNING=1  leave the server and database up when done

set -euo pipefail

MODE="${1:-all}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HERE="$ROOT/scripts/first-run"
export FIRST_RUN_DIR="${FIRST_RUN_DIR:-$(mktemp -d /tmp/temps-first-run.XXXXXX)}"
export FIRST_RUN_ADDRESS="${FIRST_RUN_ADDRESS:-127.0.0.1:8760}"
export FIRST_RUN_CONSOLE="${FIRST_RUN_CONSOLE:-127.0.0.1:8761}"
export FIRST_RUN_TLS="${FIRST_RUN_TLS:-127.0.0.1:8763}"
DB_CONTAINER=""

die() {
  echo "macos-colima: $*" >&2
  exit 1
}

case "$MODE" in first-run | quiet-logs | all) ;; *) die "usage: $0 [first-run|quiet-logs|all]" ;; esac

for tool in docker bun jq zip openssl curl python3; do
  command -v "$tool" > /dev/null || die "$tool is required (brew install $tool)"
done
python3 -c 'import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)' ||
  die "python3 >= 3.11 is required for the quiet-logs check (brew install python)"

# Temps talks to the Docker API directly. Colima's socket is not at
# /var/run/docker.sock unless linked, so take it from the active context.
if [[ -z "${DOCKER_HOST:-}" ]]; then
  context_host="$(docker context inspect --format '{{.Endpoints.docker.Host}}' 2> /dev/null || true)"
  [[ -n "$context_host" ]] && export DOCKER_HOST="$context_host"
fi
docker info > /dev/null 2>&1 || die "Docker is not reachable (start it with: colima start --cpu 4 --memory 8)"
echo "Docker: ${DOCKER_HOST:-default socket} ($(docker info --format '{{.OperatingSystem}}'))"

if [[ -z "${TEMPS_BIN:-}" ]]; then
  echo "Building temps (release, with the web console)..."
  (cd "$ROOT" && cargo build --release --bin temps)
  TEMPS_BIN="$ROOT/target/release/temps"
fi
export TEMPS_BIN

# shellcheck disable=SC2317,SC2329 # invoked by the EXIT trap
cleanup() {
  if [[ "${KEEP_RUNNING:-0}" == "1" ]]; then
    echo "Left running: server log $FIRST_RUN_DIR/temps.log${DB_CONTAINER:+, database container $DB_CONTAINER}"
    return
  fi
  "$HERE/temps-instance.sh" stop || true
  if [[ -n "$DB_CONTAINER" ]]; then
    docker rm -f "$DB_CONTAINER" > /dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

if [[ -z "${DATABASE_URL:-}" ]]; then
  DB_CONTAINER="temps-first-run-db-$$"
  echo "Starting throwaway TimescaleDB container $DB_CONTAINER..."
  docker run -d --name "$DB_CONTAINER" -p 127.0.0.1::5432 \
    -e POSTGRES_USER=temps -e POSTGRES_PASSWORD=temps -e POSTGRES_DB=temps \
    timescale/timescaledb-ha:pg18 > /dev/null
  port="$(docker port "$DB_CONTAINER" 5432/tcp | head -n1 | sed 's/.*://')"
  for _ in $(seq 1 60); do
    docker exec "$DB_CONTAINER" pg_isready -U temps -d temps > /dev/null 2>&1 && break
    sleep 2
  done
  export DATABASE_URL="postgres://temps:temps@127.0.0.1:$port/temps"
fi

echo "Installing scenario dependencies..."
(cd "$ROOT/packages/api" && bun install --frozen-lockfile > /dev/null && bun run build > /dev/null && bun link > /dev/null)
(cd "$ROOT/sdks/node/packages/node-sdk" && bun install > /dev/null && bun run build > /dev/null && bun link > /dev/null)
(cd "$ROOT/apps/temps-e2e" && bun install --frozen-lockfile > /dev/null)

"$HERE/temps-instance.sh" start
TEMPS_API_KEY="$("$HERE/temps-instance.sh" mint-key)"
export TEMPS_API_KEY
export TEMPS_URL="http://$FIRST_RUN_ADDRESS"

status=0
if [[ "$MODE" == "first-run" || "$MODE" == "all" ]]; then
  if [[ "${FIRST_RUN_SKIP_UI:-0}" != "1" ]]; then
    (cd "$ROOT/web" && bun install --frozen-lockfile > /dev/null && bunx playwright install chromium > /dev/null)
  fi
  E2E_EMAIL="${ADMIN_EMAIL:-admin@localho.st}" \
    E2E_PASSWORD="$(cat "$FIRST_RUN_DIR/admin-password")" \
    E2E_BASE_URL="http://$FIRST_RUN_CONSOLE" \
    "$HERE/run-first-run.sh" || status=1
fi
if [[ "$MODE" == "quiet-logs" || "$MODE" == "all" ]]; then
  "$HERE/quiet-logs-soak.sh" || status=1
fi

echo "Reports: $FIRST_RUN_DIR"
exit "$status"
