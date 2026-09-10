#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
nightly_workflow="$repository_root/.github/workflows/nightly-release.yml"
release_workflow="$repository_root/.github/workflows/release.yml"
sandbox_workflow="$repository_root/.github/workflows/sandbox-images-beta.yml"
e2e_workflow="$repository_root/.github/workflows/e2e-tests.yml"
rust_tests_workflow="$repository_root/.github/workflows/rust-tests.yml"
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

ruby - "$repository_root" "$nightly_workflow" "$release_workflow" "$sandbox_workflow" "$e2e_workflow" "$rust_tests_workflow" <<'RUBY'
require "yaml"

repository_root = ARGV[0]
nightly = YAML.safe_load(File.read(ARGV[1]), aliases: true)
release = YAML.safe_load(File.read(ARGV[2]), aliases: true)
sandbox = YAML.safe_load(File.read(ARGV[3]), aliases: true)
e2e = YAML.safe_load(File.read(ARGV[4]), aliases: true)
rust_tests = YAML.safe_load(File.read(ARGV[5]), aliases: true)

e2e_sandbox_channel = e2e.dig("jobs", "e2e-test", "env", "TEMPS_SANDBOX_CHANNEL")
abort "E2E must pull the beta sandbox images published for main and PR builds" unless
  e2e_sandbox_channel == "beta"

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

workflow_documents = Dir[File.join(repository_root, ".github/workflows/*.{yml,yaml}")].sort.map do |path|
  [path, YAML.safe_load(File.read(path), aliases: true)]
end
privileged_workflows = workflow_documents.select do |_path, workflow|
  workflow_permissions = workflow.fetch("permissions", {}).values
  job_permissions = workflow.fetch("jobs", {}).values.flat_map do |job|
    job.fetch("permissions", {}).values
  end
  (workflow_permissions + job_permissions).include?("write")
end
privileged_action_refs = privileged_workflows.flat_map do |path, workflow|
  workflow.fetch("jobs", {}).flat_map do |job_name, job|
    refs = []
    refs << ["#{path}: job #{job_name}", job["uses"]] if job["uses"]
    job.fetch("steps", []).each_with_index do |step, index|
      refs << ["#{path}: job #{job_name} step #{index + 1}", step["uses"]] if step["uses"]
    end
    refs
  end
end
mutable_action_refs = privileged_action_refs.select do |_location, action_ref|
  !action_ref.start_with?("./") && !action_ref.match?(/@[0-9a-f]{40}\z/)
end
abort "privileged workflows contain mutable action refs:\n#{mutable_action_refs.map { |location, ref| "#{location}: #{ref}" }.join("\n")}" unless
  mutable_action_refs.empty?
inconsistent_action_pins = privileged_action_refs
  .reject { |_location, action_ref| action_ref.start_with?("./") }
  .group_by { |_location, action_ref| action_ref.split("@", 2).first }
  .select { |_action, refs| refs.map(&:last).uniq.length > 1 }
unless inconsistent_action_pins.empty?
  abort "privileged workflows pin the same action to different commits:\n" \
    "#{inconsistent_action_pins.inspect}"
end

abort "release workflow must deny token permissions by default" unless
  release["permissions"] == {}
read_contents = {"contents" => "read"}
publish_packages = {"contents" => "read", "packages" => "write"}
expected_release_permissions = {
  "validate-release-ref" => read_contents,
  "build-web-assets" => read_contents,
  "build-linux-amd64" => read_contents,
  "build-linux-arm64" => read_contents,
  "build-darwin-amd64" => read_contents,
  "build-darwin-arm64" => read_contents,
  "create-release" => {"contents" => "write"},
  "build-and-push-docker" => publish_packages,
  "create-docker-manifest" => publish_packages,
  "prepare-sandbox-context" => read_contents,
  "build-and-push-sandbox-images" => publish_packages,
  "build-and-push-preview-gateway" => publish_packages,
}
actual_release_permissions = release.fetch("jobs").map do |name, job|
  [name, job["permissions"]]
end.to_h
unless actual_release_permissions == expected_release_permissions
  abort "release job permissions differ from the least-privilege allowlist:\n" \
    "expected #{expected_release_permissions.inspect}\nactual #{actual_release_permissions.inspect}"
end

otel_protobuf_compat = rust_tests.fetch("jobs").fetch("otel-protobuf-compat")
abort "OTEL protobuf compatibility must use the release runner image" unless
  otel_protobuf_compat["runs-on"] == "ubuntu-22.04"
otel_protobuf_steps = otel_protobuf_compat.fetch("steps")
protobuf_install = otel_protobuf_steps.find { |step| step["name"] == "Install protobuf compiler" }
abort "OTEL protobuf compatibility must install protobuf-compiler" unless
  protobuf_install&.fetch("run", "")&.include?("protobuf-compiler")
protobuf_compile = otel_protobuf_steps.find { |step| step["name"] == "Compile OTEL protobuf bindings" }
abort "OTEL protobuf compatibility must compile temps-otel" unless
  protobuf_compile&.fetch("run", "") == "cargo check --lib -p temps-otel"

release_steps = release.fetch("jobs").values.flat_map { |job| job.fetch("steps", []) }
bun_versions = release_steps.map { |step| step.dig("with", "bun-version") }.compact
abort "release workflow uses an unpinned Bun version" unless
  bun_versions == ["1.3.14"]
wasm_pack_installs = release_steps.flat_map do |step|
  step.fetch("run", "").lines.map(&:strip).select { |line| line.include?("cargo install wasm-pack") }
end
abort "release workflow uses an unpinned wasm-pack version" unless
  wasm_pack_installs == ["cargo install wasm-pack --version 0.15.0 --locked"]

expected_sandbox_permissions = {
  "prepare-context" => read_contents,
  "build-and-push-sandbox-images" => publish_packages,
  "build-and-push-preview-gateway" => publish_packages,
}
actual_sandbox_permissions = sandbox.fetch("jobs").map do |name, job|
  [name, job["permissions"]]
end.to_h
unless sandbox["permissions"] == read_contents &&
    actual_sandbox_permissions == expected_sandbox_permissions
  abort "beta workflow permissions differ from the least-privilege allowlist"
end

sandbox_steps = release.dig("jobs", "prepare-sandbox-context", "steps")
sandbox_dependencies = sandbox_steps.find { |step| step["name"] == "Install build dependencies" }
abort "sandbox helper builds do not install protoc" unless
  sandbox_dependencies&.fetch("run", "")&.include?("protobuf-compiler")

puts "privileged workflow action pinning valid: #{privileged_action_refs.length} uses " \
  "across #{privileged_workflows.length} workflows"
puts "release permission allowlists and tool pins are valid"
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

echo "nightly release workflow wiring and publishing workflow security are valid"
