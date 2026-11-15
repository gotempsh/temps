#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Fetch the newest learned-serial list and append it to .config/nextest.toml.
#
# The list is the `nextest-learned-serial` artifact of the last successful
# nextest-learned-serial.yml run on `main` in this repository -- never a pull
# request's, which could otherwise pick what the next run serialises. Any
# failure (no run yet, expired artifact, API trouble) leaves the config as
# committed: the tests then run in parallel exactly as before, and this is
# reported as a notice rather than failing the job.
#
# Needs GH_TOKEN with `actions: read`.
set -uo pipefail

repo="${GITHUB_REPOSITORY:?}"
dir="${RUNNER_TEMP:-/tmp}/nextest-learned-serial"
rm -rf "$dir"

run_id=$(gh run list -R "$repo" --workflow nextest-learned-serial.yml --branch main \
  --status success --limit 1 --json databaseId --jq '.[0].databaseId // empty' 2>/dev/null)
if [[ -z "$run_id" ]]; then
  echo "::notice title=No learned serial list::No successful nextest-learned-serial run on main yet; running the committed config."
  exit 0
fi
if ! gh run download "$run_id" -R "$repo" -n nextest-learned-serial -D "$dir" >/dev/null 2>&1; then
  echo "::notice title=No learned serial list::Could not download the list from run $run_id; running the committed config."
  exit 0
fi
python3 "$(dirname "$0")/nextest-flaky.py" apply --learned "$dir/learned-serial.json" --config .config/nextest.toml
