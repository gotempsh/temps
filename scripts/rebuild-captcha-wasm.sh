#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Rebuild the checked-in package with the same compiler host as CI.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
image="temps-captcha-toolchain:rebuild-$(id -u)-$$"
cleanup() { docker image rm "$image" >/dev/null 2>&1 || true; }
trap cleanup EXIT

docker build --platform linux/amd64 --target toolchain --tag "$image" "$repo_root"
docker run --rm --platform linux/amd64 \
  --volume "$repo_root":/build --workdir /build/crates/temps-captcha-wasm \
  --env CARGO_TARGET_DIR=/tmp/captcha-target \
  "$image" \
  sh -ec 'git config --global --add safe.directory /build; rustc -Vv; wasm-pack --version; wasm-bindgen --version; npm run build'
