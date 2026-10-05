#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Quiet-logs soak: build a small steady-state workload (one deployed app, one
# linked managed Postgres, one alert rule, one uptime monitor), leave the
# server idle, then fail on any ERROR line or on a module that WARNs more than
# the hourly budget during the idle window.
#
# Environment:
#   TEMPS_URL, TEMPS_API_KEY   instance + key (see temps-instance.sh mint-key)
#   TEMPS_LOG                  the server log (default $FIRST_RUN_DIR/temps.log)
#   SOAK_MINUTES               idle period (default 20; 1440 for the 24 h soak)
#   WARN_PER_HOUR              WARN budget per module per hour (default 12)
#   FIRST_RUN_DIR              where state and reports go (/tmp/temps-first-run)
#   SOAK_KEEP=1                leave the workload in place afterwards

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HERE="$ROOT/scripts/first-run"
FIRST_RUN_DIR="${FIRST_RUN_DIR:-/tmp/temps-first-run}"
TEMPS_LOG="${TEMPS_LOG:-$FIRST_RUN_DIR/temps.log}"
SOAK_MINUTES="${SOAK_MINUTES:-20}"
WARN_PER_HOUR="${WARN_PER_HOUR:-12}"
STATE="$FIRST_RUN_DIR/quiet-logs-fixture.json"
: "${TEMPS_URL:?TEMPS_URL is required}"
: "${TEMPS_API_KEY:?TEMPS_API_KEY is required}"
[[ -f "$TEMPS_LOG" ]] || { echo "quiet-logs-soak: no server log at $TEMPS_LOG" >&2; exit 2; }
[[ "$SOAK_MINUTES" =~ ^[0-9]+$ && "$SOAK_MINUTES" -gt 0 ]] || { echo "SOAK_MINUTES must be a positive integer" >&2; exit 2; }
mkdir -p "$FIRST_RUN_DIR"

fixture() {
  (cd "$ROOT/apps/temps-e2e" && bun run src/index.ts quiet-logs-fixture --state "$STATE" "$@")
}

# shellcheck disable=SC2317,SC2329 # invoked by the EXIT trap
teardown() {
  local result=$?
  trap - EXIT
  if [[ "${SOAK_KEEP:-0}" != "1" && -f "$STATE" ]]; then
    echo "== removing the soak workload =="
    if ! fixture --teardown; then
      echo "error: soak workload teardown failed" >&2
      [[ "$result" -ne 0 ]] || result=1
    fi
  fi
  exit "$result"
}
trap teardown EXIT

echo "== building the soak workload =="
fixture

# Let the deploy settle (health checks, first monitor probe, first backup
# scheduler tick) before the idle window starts measuring.
sleep "${SOAK_SETTLE_SECONDS:-60}"

window_start="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "== idling for ${SOAK_MINUTES} minute(s) from $window_start =="
end=$((SECONDS + SOAK_MINUTES * 60))
while (( SECONDS < end )); do
  remaining=$(( (end - SECONDS) / 60 ))
  echo "  idle... ${remaining} min left; log $(wc -l < "$TEMPS_LOG" | tr -d ' ') lines"
  sleep $(( end - SECONDS < 300 ? end - SECONDS : 300 ))
done
window_end="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# A quiet log is insufficient if the workload disappeared or stopped serving.
fixture --verify

# Snapshot the log before teardown so deleting the workload is not judged.
cp "$TEMPS_LOG" "$FIRST_RUN_DIR/quiet-logs-window.log"

set +e
python3 "$HERE/check_quiet_logs.py" "$FIRST_RUN_DIR/quiet-logs-window.log" \
  --allowlist "$HERE/quiet-logs-allowlist.toml" \
  --window-start "$window_start" \
  --window-end "$window_end" \
  --warn-per-hour "$WARN_PER_HOUR" \
  --markdown "$FIRST_RUN_DIR/quiet-logs-report.md" \
  --json "$FIRST_RUN_DIR/quiet-logs-report.json"
result=$?
set -e
exit "$result"
