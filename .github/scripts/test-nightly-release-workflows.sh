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
if [[ "$tag_aware_dispatch_count" -ne 6 ]]; then
  fail "expected release channel and version logic to distinguish dry-runs from tag dispatches"
fi

ruby - "$repository_root" "$nightly_workflow" "$release_workflow" "$sandbox_workflow" "$e2e_workflow" "$rust_tests_workflow" <<'RUBY'
require "yaml"

repository_root = ARGV[0]
nightly = YAML.safe_load(File.read(ARGV[1]), aliases: true)
release = YAML.safe_load(File.read(ARGV[2]), aliases: true)
daemon = YAML.safe_load(File.read(File.join(repository_root, ".github/workflows/daemon-images.yml")), aliases: true)
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

# Default success() dependency semantics must gate publication on every flavor.
publish = release.dig("jobs", "create-release")
abort "standalone runtime manifest is not a published release asset" unless
  publish.fetch("steps").any? { |step| step.fetch("run", "").match?(/release_assets=\([^)]*release\/runtime-images\.json/m) }
abort "public release can precede required daemon images" unless
  publish.fetch("needs").include?("runtime-image-manifest") &&
  publish.fetch("needs").include?("promote-runtime-images") && !publish.key?("if")
daemon_call = release.dig("jobs", "daemon-images")
abort "daemon staging must follow ref validation without moving channel tags" unless
  daemon_call["needs"] == "validate-release-ref" &&
  daemon_call.dig("with", "revision_only") == true && !daemon_call.key?("continue-on-error")
manifest = release.dig("jobs", "runtime-image-manifest")
abort "manifest can precede one of the required image sets" unless
  manifest["needs"].sort == %w[daemon-images build-and-push-sandbox-images build-and-push-preview-gateway].sort &&
  !manifest.key?("if") && !manifest.key?("continue-on-error")
%w[build-linux-amd64 build-linux-arm64 build-darwin-amd64 build-darwin-arm64].each do |platform|
  job = release.dig("jobs", platform)
  abort "#{platform} can compile before its runtime images exist" unless
    job["needs"].include?("runtime-image-manifest") && !job.key?("if")
  build = job.fetch("steps").find { |step| step["name"] == "Build release binary" }
  abort "#{platform} does not embed the verified runtime manifest" unless
    build.dig("env", "TEMPS_RELEASE_IMAGE_MANIFEST") == "${{ github.workspace }}/release-inputs/runtime-images.json"
  abort "#{platform} does not download the manifest" unless
    job.fetch("steps").any? { |step| step.dig("with", "name") == "runtime-image-manifest" }
  abort "#{platform} tarball omits runtime manifest" unless
    job.fetch("steps").any? { |step| step.fetch("run", "").include?("-C release-inputs runtime-images.json") }
end
promotion = release.dig("jobs", "promote-runtime-images")
abort "runtime aliases can move before binaries pass" unless
  (%w[build-linux-amd64 build-linux-arm64 build-darwin-amd64 build-darwin-arm64] - promotion["needs"]).empty? &&
  !promotion.key?("if") && !promotion.key?("continue-on-error")
# Catch accidental dependency cycles when adding another release prerequisite.
visit = lambda do |name, ancestors|
  abort "release dependency cycle: #{(ancestors + [name]).join(' -> ')}" if ancestors.include?(name)
  job = release.fetch("jobs").fetch(name)
  Array(job["needs"]).each { |dependency| visit.call(dependency, ancestors + [name]) }
end
release.fetch("jobs").each_key { |name| visit.call(name, []) }
%w[build-and-push-sandbox-images build-and-push-preview-gateway].each do |name|
  job = release.dig("jobs", name)
  checkout_index = job.fetch("steps").index { |step| step.fetch("uses", "").start_with?("actions/checkout@") }
  record_index = job.fetch("steps").index { |step| step["name"] == "Record runtime image digest" }
  abort "#{name} records digests without checking out its script" unless
    checkout_index && record_index && checkout_index < record_index
  abort "#{name} still depends on the public release" if job["needs"].include?("create-release")
  tags = job["steps"].find { |step| step["name"] == "Compose tag list" }.fetch("run")
  abort "#{name} moves version/channel tags before binary verification" if tags.include?('$REPO:$VER') || tags.include?('$REPO:beta') || tags.include?('$REPO:latest')
  build = job["steps"].find { |step| step["id"] == "image" }
  abort "#{name} dry-run cannot export a digest" unless build.dig("with", "outputs").include?("type=oci,dest=")
end
abort "daemon channel must match release stable/prerelease/dry-run policy" unless
  daemon_call.dig("with", "channel") == "${{ (inputs.dry_run == true || contains(github.ref_name, '-')) && 'beta' || 'stable' }}"
images = daemon.dig("jobs", "images")
abort "all required daemon flavors must be verified" unless
  images.dig("strategy", "matrix", "flavor") == ["nodejs", "python", "all"] &&
  !images.key?("continue-on-error")
steps = images.fetch("steps")
publish_index = steps.index { |step| step["name"] == "Build and publish daemon image" }
abort "daemon publication step is missing or bypasses failed checks" unless
  publish_index && !steps[publish_index].key?("if") && !steps[publish_index].key?("continue-on-error")
published_platforms = steps[publish_index].fetch("with").fetch("platforms").split(",")
daemon_cache = "type=registry,ref=ghcr.io/gotempsh/temps-sandbox-${{ matrix.flavor }}:daemon-buildcache"
daemon_builds = steps.select { |step| step.fetch("uses", "").start_with?("docker/build-push-action@") }
abort "every daemon build must read the persistent flavor-specific registry cache" unless
  daemon_builds.length == 3 && daemon_builds.all? { |step| step.dig("with", "cache-from") == daemon_cache }
abort "only validated publishing runs may export the daemon cache after lifecycle checks" unless
  steps[publish_index].dig("with", "cache-to") ==
    "${{ steps.metadata.outputs.publish == 'true' && format('type=registry,ref=ghcr.io/gotempsh/temps-sandbox-{0}:daemon-buildcache,mode=max,ignore-error=true', matrix.flavor) || '' }}" &&
  steps.each_with_index.all? { |step, index| index == publish_index || !step.fetch("with", {}).key?("cache-to") }
abort "daemon publication must cover both supported architectures" unless
  published_platforms.sort == %w[linux/amd64 linux/arm64]
published_platforms.each do |platform|
  arch = platform.split("/").last
  load_index = steps.index { |step| step["name"] == "Load #{arch} image for lifecycle verification" }
  verify_index = steps.index { |step| step["name"] == "Verify #{arch} daemon lifecycle without provider credentials" }
  abort "#{platform} must be loaded and verified unconditionally before publication" unless
    load_index && verify_index && load_index < verify_index && verify_index < publish_index &&
    [load_index, verify_index].all? { |index| !steps[index].key?("if") && !steps[index].key?("continue-on-error") } &&
    steps[load_index].dig("with", "platforms") == platform &&
    steps[load_index].dig("with", "load") == true &&
    steps[load_index].dig("with", "push") == false &&
    steps[load_index].dig("with", "tags") == "temps-daemon-release:check" &&
    steps[verify_index].dig("env", "DOCKER_DEFAULT_PLATFORM") == platform &&
    steps[verify_index]["run"].include?("docker info >/dev/null\n") &&
    steps[verify_index]["run"].include?("bash tools/sandbox-runtime/smoke.sh temps-daemon-release:check")
  abort "#{platform} verification can use an overwritten image tag" if
    steps[(load_index + 1)...verify_index].any? { |step| step.dig("with", "load") == true }
end

daemon_check = YAML.safe_load(File.read(File.join(repository_root, ".github/workflows/daemon-images-check.yml")), aliases: true)
check_job = daemon_check.dig("jobs", "build")
abort "PR checks must exercise every published flavor and architecture" unless
  check_job.dig("strategy", "matrix", "flavor") == ["nodejs", "python", "all"] &&
  check_job.dig("strategy", "matrix", "arch") == ["amd64", "arm64"] &&
  !check_job.key?("continue-on-error")
check_steps = check_job.fetch("steps")
check_builds = check_steps.select { |step| step.fetch("uses", "").start_with?("docker/build-push-action@") }
abort "PR builds must consume daemon cache without registry login or cache writes" unless
  check_builds.length == 1 && check_builds.all? { |step| step.dig("with", "cache-from") == daemon_cache } &&
  check_steps.all? { |step| !step.fetch("with", {}).key?("cache-to") && !step.fetch("uses", "").start_with?("docker/login-action@") }
check_load_index = check_steps.index { |step| step["name"] == "Load image for lifecycle verification" }
check_smoke_index = check_steps.index { |step| step["name"] == "Verify daemon lifecycle without provider credentials" }
abort "PR image smoke checks must not skip matrix entries" unless
  check_load_index && check_smoke_index && check_load_index < check_smoke_index &&
  check_steps.all? { |step| !step.key?("if") && !step.key?("continue-on-error") } &&
  check_steps[check_load_index].dig("with", "platforms") == "linux/${{ matrix.arch }}" &&
  check_steps[check_load_index].dig("with", "target") == "${{ matrix.flavor }}" &&
  check_steps[check_load_index].dig("with", "load") == true &&
  check_steps[check_load_index].dig("with", "push") == false &&
  check_steps[check_load_index].dig("with", "tags") == "temps-daemon-ci:check" &&
  check_steps[check_smoke_index].dig("env", "DOCKER_DEFAULT_PLATFORM") == "linux/${{ matrix.arch }}" &&
  check_steps[check_smoke_index]["run"].include?("docker info >/dev/null\n") &&
  check_steps[check_smoke_index]["run"].include?("bash tools/sandbox-runtime/smoke.sh temps-daemon-ci:check")

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
  "daemon-images" => publish_packages,
  "validate-release-ref" => read_contents,
  "runtime-image-manifest" => {"contents" => "read", "packages" => "read"},
  "promote-runtime-images" => publish_packages,
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
  "daemon-images" => publish_packages,
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
python3 "$repository_root/.github/scripts/test_release_image_manifest.py"
