#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Run the Temps plugin eval suite (claude plugin eval). Every run and judge is
# a real model call billed to your account; --max-cost-usd caps it.
#
#   evals/run.sh                                   # whole suite, 3 runs/arm
#   evals/run.sh --case existing-server-dry-run-first --runs 1 --ablation none
#
# Bash is granted only for the commands the install journeys use, and runs
# in Claude Code's sandbox with network limited to the npm registry (so the
# pinned CLI can be fetched). Nothing can reach a real server or Docker host.

set -euo pipefail
cd "$(dirname "$0")/.."

exec claude plugin eval . \
  --eval-dir evals \
  --model "${EVAL_MODEL:-claude-sonnet-5-5}" \
  --judge-model "${EVAL_JUDGE_MODEL:-claude-haiku-5-5}" \
  --max-cost-usd "${EVAL_MAX_COST_USD:-10}" \
  --no-publish \
  "$@" \
  --allow-tools \
    "Bash(bunx *)" "Bash(npx *)" "Bash(docker *)" "Bash(command -v *)" \
    "WebFetch(domain:registry.npmjs.org)"
