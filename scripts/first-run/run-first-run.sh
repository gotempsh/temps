#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Run the first-run suite against an instance started by temps-instance.sh:
#   1. API scenario (apps/temps-e2e first-run-scenario): image, git Dockerfile,
#      Node preset, compose, static site, managed Postgres + Redis
#   2. Console flow (Playwright): create a Flexible project from a Docker
#      image in the UI and reach it through the proxy
#
# Environment:
#   TEMPS_URL, TEMPS_API_KEY   instance + key (see temps-instance.sh mint-key)
#   E2E_BASE_URL               console URL for Playwright (http://127.0.0.1:8761)
#   E2E_EMAIL, E2E_PASSWORD    console login for Playwright
#   FIRST_RUN_DIR              where reports are written (/tmp/temps-first-run)
#   FIRST_RUN_GIT_URL          public https URL holding examples/first-run
#   FIRST_RUN_GIT_BRANCH       branch to deploy from it (main)
#   FIRST_RUN_SKIP_UI=1        skip the Playwright step
#   FIRST_RUN_SCENARIO_ARGS    extra args for first-run-scenario (e.g. "--only compose")

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FIRST_RUN_DIR="${FIRST_RUN_DIR:-/tmp/temps-first-run}"
: "${TEMPS_URL:?TEMPS_URL is required}"
: "${TEMPS_API_KEY:?TEMPS_API_KEY is required}"
mkdir -p "$FIRST_RUN_DIR"

status=0

echo "== first-run API scenario =="
read -r -a extra_args <<< "${FIRST_RUN_SCENARIO_ARGS:-}"
(
  cd "$ROOT/apps/temps-e2e"
  bun run src/index.ts first-run-scenario \
    --git-url "${FIRST_RUN_GIT_URL:-https://github.com/gotempsh/temps.git}" \
    --git-branch "${FIRST_RUN_GIT_BRANCH:-main}" \
    --report "$FIRST_RUN_DIR/first-run-report.json" \
    ${extra_args[@]+"${extra_args[@]}"}
) || status=1

if [[ "${FIRST_RUN_SKIP_UI:-0}" != "1" ]]; then
  echo "== first-run console flow (Playwright) =="
  (
    cd "$ROOT/web"
    FIRST_RUN_UI=1 \
      E2E_BASE_URL="${E2E_BASE_URL:-http://127.0.0.1:8761}" \
      FIRST_RUN_PROXY_URL="$TEMPS_URL" \
      bunx playwright test --project=setup --project=chromium \
      e2e/authenticated/first-run-flexible-image.spec.ts
  ) || status=1
fi

exit "$status"
