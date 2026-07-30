#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
nightly_workflow="$repository_root/.github/workflows/nightly-release.yml"
release_workflow="$repository_root/.github/workflows/release.yml"
decision_script="$repository_root/.github/scripts/nightly-release-decision.sh"
validation_script="$repository_root/.github/scripts/validate-release-ref.sh"

fail() {
  echo "nightly workflow regression: $*" >&2
  exit 1
}

# The no-checkout dispatch job must identify both repository and tag explicitly.
# shellcheck disable=SC2016
grep -Fq -- '--repo "$REPOSITORY"' "$nightly_workflow" ||
  fail "the no-checkout dispatch job cannot identify its repository"

# shellcheck disable=SC2016
grep -Fq -- '--ref "$TAG"' "$nightly_workflow" ||
  fail "nightly tags are not explicitly dispatched to the release workflow"

grep -Fq -- '--field dry_run=false' "$nightly_workflow" ||
  fail "nightly release dispatches would use the safe dry-run default"

# The active-run query must be scoped to the nightly tag, not merely the SHA;
# an unrelated branch dry-run can share the same commit.
# shellcheck disable=SC2016
grep -Fq -- '--branch "$last_nightly_tag"' "$nightly_workflow" ||
  fail "nightly recovery can mistake an unrelated branch run for the release"

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

ruby - "$nightly_workflow" "$release_workflow" <<'RUBY'
require "yaml"

nightly = YAML.safe_load(File.read(ARGV[0]), aliases: true)
release = YAML.safe_load(File.read(ARGV[1]), aliases: true)

check_permissions = nightly.dig("jobs", "check-and-tag", "permissions")
abort "check-and-tag permissions are not read-actions/write-contents" unless
  check_permissions == {"actions" => "read", "contents" => "write"}

dispatch_permissions = nightly.dig("jobs", "dispatch-release", "permissions")
abort "dispatch-release must only have actions: write" unless
  dispatch_permissions == {"actions" => "write"}

abort "release builds can bypass ref validation" unless
  release.dig("jobs", "build-web-assets", "needs") == "validate-release-ref"

abort "release dependency fetches can fall back to Cargo's embedded Git client" unless
  release.dig("env", "CARGO_NET_GIT_FETCH_WITH_CLI") == "true"

sandbox_steps = release.dig("jobs", "prepare-sandbox-context", "steps")
sandbox_dependencies = sandbox_steps.find { |step| step["name"] == "Install build dependencies" }
abort "sandbox helper builds do not install protoc" unless
  sandbox_dependencies&.fetch("run", "")&.include?("protobuf-compiler")
RUBY

expect_decision() {
  local expected="$1"
  shift
  local actual
  actual="$("$decision_script" "$@")"
  if [[ "$actual" != "$expected" ]]; then
    fail "unexpected nightly decision for inputs '$*': expected '$expected', got '$actual'"
  fi
}

expect_decision $'should_release=true\nshould_create_tag=true\nexisting_tag=' \
  new-sha "" "" false missing
expect_decision $'should_release=true\nshould_create_tag=true\nexisting_tag=' \
  new-sha old-tag old-sha true success
expect_decision $'should_release=false\nshould_create_tag=false\nexisting_tag=nightly-tag' \
  same-sha nightly-tag same-sha true success
expect_decision $'should_release=false\nshould_create_tag=false\nexisting_tag=nightly-tag' \
  same-sha nightly-tag same-sha false active
expect_decision $'should_release=true\nshould_create_tag=false\nexisting_tag=nightly-tag' \
  same-sha nightly-tag same-sha false missing
expect_decision $'should_release=true\nshould_create_tag=false\nexisting_tag=nightly-tag' \
  same-sha nightly-tag same-sha true failed
expect_decision $'should_release=true\nshould_create_tag=false\nexisting_tag=nightly-tag' \
  same-sha nightly-tag same-sha false success

"$validation_script" true branch main >/dev/null
"$validation_script" false tag v0.1.0 >/dev/null
"$validation_script" false tag v0.1.0-beta.55 >/dev/null
"$validation_script" false tag v0.1.0-nightly.20260729.abc12345 >/dev/null

if "$validation_script" false branch main >/dev/null 2>&1; then
  fail "a non-dry branch dispatch was accepted for publishing"
fi

if "$validation_script" false tag latest >/dev/null 2>&1; then
  fail "a malformed release tag was accepted for publishing"
fi

echo "nightly release workflow wiring is valid"
