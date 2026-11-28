#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Build dist/temps.mcpb, the Claude Desktop extension.
#
# An MCP bundle is a zip with manifest.json at its root. This zips an
# explicit file list with the system `zip`, so the release job (which holds
# contents:write and signing credentials) runs no third-party packer. The
# manifest is validated separately by `bun run validate` in CI.

set -euo pipefail
cd "$(dirname "$0")"

# Keep in sync with the server modules; bridge.test.js checks that every
# non-test file under server/ is listed here.
files=(
  manifest.json
  icon.png
  package.json
  server/index.js
  server/bridge.js
)

for file in "${files[@]}"; do
  [[ -s "$file" ]] || { echo "pack.sh: missing bundle file packages/temps-mcpb/$file" >&2; exit 1; }
done

rm -rf dist
mkdir -p dist
# -X drops extra file attributes (uid/gid, extended timestamps).
zip -X -q dist/temps.mcpb "${files[@]}"
echo "packages/temps-mcpb/dist/temps.mcpb"
