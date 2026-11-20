#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Rust owns protocol/storage/query assertions. Foreign runtimes only generate
# real SDK traffic. Tests create isolated TestDatabases; no dev server/login.
set -euo pipefail
repo_root=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo_root"
export TEMPS_SENTRY_REPO_ROOT="$repo_root"
if [[ -z ${DOCKER_HOST:-} && -S "$HOME/.colima/default/docker.sock" ]]; then
  export DOCKER_HOST="unix://$HOME/.colima/default/docker.sock"
fi
run_sdk_tests() {
  local filter=$1
  if [[ -n ${TEMPS_SENTRY_TEST_ARCHIVE:-} ]]; then
    [[ -f "$TEMPS_SENTRY_TEST_ARCHIVE" ]] || { echo 'SDK test archive is missing'; return 1; }
    if [[ -z ${sdk_extract_dir:-} ]]; then
      sdk_extract_dir=$(mktemp -d "$fixture_root/binaries.XXXXXX")
      trap 'rm -rf -- "$sdk_extract_dir"' EXIT
      # The workspace archive can be large. Extract once for all SDK cases,
      # then reuse nextest's metadata and remap the extracted target directory.
      cargo nextest list --archive-file "$TEMPS_SENTRY_TEST_ARCHIVE" \
        --workspace-remap "$repo_root" --extract-to "$sdk_extract_dir" \
        --list-type binaries-only -E 'package(=temps-error-tracking)'
    fi
    cargo nextest run \
      --cargo-metadata "$sdk_extract_dir/target/nextest/cargo-metadata.json" \
      --binaries-metadata "$sdk_extract_dir/target/nextest/binaries-metadata.json" \
      --target-dir-remap "$sdk_extract_dir/target" \
      --workspace-remap "$repo_root" --run-ignored only --no-tests fail \
      -E "package(=temps-error-tracking) & test($filter)" \
      --test-threads=1 --success-output final
  else
    cargo test --lib -p temps-error-tracking "$filter" -j2 -- --ignored --nocapture --test-threads=1
  fi
}
case "${1:-native}" in
  native)
    # One feature graph avoids recompiling the shared native services for each
    # adapter. Run complete suites so native SDK regressions are caught too.
    cargo test --lib -p temps-error-tracking -p temps-otel \
      -p temps-analytics-session-replay -p temps-monitoring -p temps-migrations \
      -j2 -- --test-threads=1
    ;;
  sdk)
    for runtime in bun node python3 go cargo; do command -v "$runtime" >/dev/null || { echo "Required SDK runtime missing: $runtime"; exit 1; }; done
    fixture_root=${TEMPS_SENTRY_FIXTURES_ROOT:-${TMPDIR:-/tmp}/temps-sentry-acceptance-v1}
    mkdir -p "$fixture_root/node8" "$fixture_root/node10" "$fixture_root/node11" "$fixture_root/go" "$fixture_root/rust/src" "$fixture_root/rust/examples"
    bun add --cwd "$fixture_root/node8" @sentry/node@8.47.0
    bun add --cwd "$fixture_root/node10" @sentry/node@10.17.0
    bun add --cwd "$fixture_root/node11" @sentry/node@11.8.0 @sentry/browser@11.8.0 express@5.1.0 playwright@1.64.1
    "$fixture_root/node11/node_modules/.bin/playwright" install chromium
    [[ -x "$fixture_root/python/bin/python" ]] || python3 -m venv "$fixture_root/python"
    "$fixture_root/python/bin/python" -m pip install --quiet sentry-sdk==2.42.1
    cat > "$fixture_root/go/go.mod" <<'EOF'
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
module synthetic-sentry-acceptance
go 1.23
require github.com/getsentry/sentry-go v0.36.2
EOF
    cp scripts/fixtures/sentry-errors.go "$fixture_root/go/main.go"
    (cd "$fixture_root/go" && go mod tidy)
    cat > "$fixture_root/rust/Cargo.toml" <<'EOF'
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
[package]
name = "synthetic-sentry-acceptance"
version = "0.0.0"
edition = "2021"
[dependencies]
sentry = { version = "=0.41.0", default-features = false, features = ["ureq", "backtrace"] }
serde_json = "=1.0.145"
[profile.dev]
debug = 0
EOF
    cp scripts/fixtures/sentry-errors.rs "$fixture_root/rust/src/main.rs"
    cp scripts/fixtures/sentry-traces.rs "$fixture_root/rust/examples/traces.rs"
    export TEMPS_SENTRY_NODE8_SDK="$fixture_root/node8/node_modules/@sentry/node/build/esm/index.js"
    export TEMPS_SENTRY_NODE10_SDK="$fixture_root/node10/node_modules/@sentry/node/build/esm/index.js"
    export TEMPS_SENTRY_NODE11_SDK="$fixture_root/node11/node_modules/@sentry/node/build/esm/index.js"
    export TEMPS_SENTRY_BROWSER_SDK="$fixture_root/node11/node_modules/@sentry/browser/build/npm/esm/prod/index.js"
    export TEMPS_SENTRY_PYTHON="$fixture_root/python/bin/python"
    export TEMPS_SENTRY_GO_WORKSPACE="$fixture_root/go"
    export TEMPS_SENTRY_RUST_MANIFEST="$fixture_root/rust/Cargo.toml"
    run_sdk_tests priority_sdk_
    for sdk in "$TEMPS_SENTRY_NODE10_SDK" "$TEMPS_SENTRY_NODE11_SDK" "$TEMPS_SENTRY_BROWSER_SDK"; do
      TEMPS_SENTRY_CONFORMANCE_SDK="$sdk" TEMPS_SENTRY_SIGNALS_SDK="$sdk" \
        run_sdk_tests official_sdk_
      if [[ "$sdk" != "$TEMPS_SENTRY_BROWSER_SDK" ]]; then
        TEMPS_SENTRY_CONFORMANCE_SDK="$sdk" TEMPS_SENTRY_CONFORMANCE_SUITE=signals \
          run_sdk_tests official_sdk_ingestion_
      fi
    done
    ;;
  *) echo "Usage: scripts/sentry-acceptance.sh [native|sdk]"; exit 2 ;;
esac
