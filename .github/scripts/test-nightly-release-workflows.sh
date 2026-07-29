#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
nightly_workflow="$repository_root/.github/workflows/nightly-release.yml"
release_workflow="$repository_root/.github/workflows/release.yml"

fail() {
  echo "nightly workflow regression: $*" >&2
  exit 1
}

grep -Fq 'actions: write' "$nightly_workflow" ||
  fail "nightly workflow cannot dispatch the release workflow"

# The literal $TAG is the workflow contract under test.
# shellcheck disable=SC2016
grep -Fq 'gh workflow run release.yml --ref "$TAG" --field dry_run=false' \
  "$nightly_workflow" ||
  fail "nightly tags are not explicitly dispatched to the release workflow"

grep -A5 -F 'dry_run:' "$release_workflow" | grep -Fq 'default: true' ||
  fail "manual release dispatches must default to a safe dry-run"

tag_aware_dispatch_count="$(
  # The workflow variable must remain literal here.
  # shellcheck disable=SC2016
  grep -Fc 'if [[ "$DRY_RUN" == "true" ]]; then' "$release_workflow"
)"
if [[ "$tag_aware_dispatch_count" -ne 5 ]]; then
  fail "expected release channel and version logic to distinguish dry-runs from tag dispatches"
fi

echo "nightly release workflow wiring is valid"
