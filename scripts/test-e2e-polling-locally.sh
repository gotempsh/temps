#!/usr/bin/env bash
# Local validator for the e2e-tests.yml polling+verify logic.
#
# Purpose: prove the patched polling logic terminates on the right state
# ('completed') and verifies an app over HTTPS, before pushing to CI again.
#
# Usage:
#   ./scripts/test-e2e-polling-locally.sh \
#     --api-base http://localhost:8080/api \
#     --api-key  $TEMPS_API_KEY \
#     --project  42 \
#     --app-url  https://my-app.localho.st:3443/
#
# The script does NOT create projects or trigger deployments — point it at a
# project that already has a deployment in flight (or just-finished). Mirrors
# .github/workflows/e2e-tests.yml lines 290-360.

set -euo pipefail

API_BASE=""
API_KEY=""
PROJECT_ID=""
APP_URL=""
TIMEOUT=600

while [ $# -gt 0 ]; do
  case "$1" in
    --api-base) API_BASE="$2"; shift 2 ;;
    --api-key)  API_KEY="$2";  shift 2 ;;
    --project)  PROJECT_ID="$2"; shift 2 ;;
    --app-url)  APP_URL="$2"; shift 2 ;;
    --timeout)  TIMEOUT="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,18p' "$0"; exit 0 ;;
    *) echo "Unknown arg: $1" >&2; exit 2 ;;
  esac
done

for var in API_BASE API_KEY PROJECT_ID APP_URL; do
  if [ -z "${!var}" ]; then
    echo "Missing required arg: --${var,,}" >&2
    exit 2
  fi
done

echo "============================================"
echo "Polling project $PROJECT_ID at $API_BASE"
echo "Verify URL: $APP_URL"
echo "Timeout: ${TIMEOUT}s"
echo "============================================"

# --- Poll deployment status ---
DEPLOY_STATE="pending"
DEPLOY_ID=""
DEADLINE=$((SECONDS + TIMEOUT))

while [ $SECONDS -lt $DEADLINE ]; do
  DEPLOY_LIST=$(curl -s \
    -H "Authorization: Bearer $API_KEY" \
    "$API_BASE/projects/$PROJECT_ID/deployments?per_page=1")

  DEPLOY_STATE=$(echo "$DEPLOY_LIST" | jq -r '.deployments[0].status // "pending"' 2>/dev/null || echo "pending")
  DEPLOY_ID=$(echo "$DEPLOY_LIST" | jq -r '.deployments[0].id // empty' 2>/dev/null || true)

  if [ "$DEPLOY_STATE" = "completed" ]; then
    echo "Deployment $DEPLOY_ID reached terminal state: $DEPLOY_STATE"
    break
  fi

  if [ "$DEPLOY_STATE" = "failed" ] || [ "$DEPLOY_STATE" = "cancelled" ]; then
    echo "Deployment $DEPLOY_ID failed with state: $DEPLOY_STATE"
    if [ -n "$DEPLOY_ID" ]; then
      echo "--- Deployment jobs ---"
      curl -s -H "Authorization: Bearer $API_KEY" \
        "$API_BASE/projects/$PROJECT_ID/deployments/$DEPLOY_ID/jobs" | jq '.[] | {name: .name, status: .status}' 2>/dev/null || true
    fi
    break
  fi

  echo "  State: $DEPLOY_STATE (waiting for completed...)"
  sleep 5
done

if [ "$DEPLOY_STATE" != "completed" ]; then
  echo "FAIL: Deployment did not reach completed state (last: $DEPLOY_STATE)"
  exit 1
fi

# --- Verify app is reachable (60s budget after completion) ---
echo ""
echo "Verifying $APP_URL ..."
VERIFY_DEADLINE=$((SECONDS + 60))
VERIFY_CODE="000"
while [ $SECONDS -lt $VERIFY_DEADLINE ]; do
  VERIFY_CODE=$(curl -sk -o /dev/null -w "%{http_code}" --max-time 10 "$APP_URL" || echo "000")
  if [ "$VERIFY_CODE" = "200" ] || [ "$VERIFY_CODE" = "304" ]; then
    break
  fi
  echo "  HTTP $VERIFY_CODE — waiting for app to be reachable..."
  sleep 5
done

if [ "$VERIFY_CODE" = "200" ] || [ "$VERIFY_CODE" = "304" ]; then
  echo "PASS: $APP_URL is reachable (HTTP $VERIFY_CODE)"
  exit 0
fi

echo "FAIL: $APP_URL not reachable (HTTP $VERIFY_CODE)"
exit 1
