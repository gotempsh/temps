#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Run after rebuilding the package, before compiling the application.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
if ! package_status="$(git status --porcelain -- crates/temps-captcha-wasm/pkg/)"; then
  echo "::error::Cannot inspect the committed CAPTCHA package with git status." >&2
  exit 1
fi
if [ -n "$package_status" ]; then
  echo "::error::crates/temps-captcha-wasm/pkg/ differs from the canonical Linux/AMD64 rebuild. Run 'bash scripts/rebuild-captcha-wasm.sh' from the repository root and commit the regenerated pkg/ files. Native macOS or ARM64 builds can produce different bytes."
  git status --short -- crates/temps-captcha-wasm/pkg/
  git diff --stat -- crates/temps-captcha-wasm/pkg/
  exit 1
fi
echo 'Committed CAPTCHA WASM matches the canonical rebuild.'
