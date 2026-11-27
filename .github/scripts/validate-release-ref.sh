#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -euo pipefail

if [[ "$#" -ne 3 ]]; then
  echo "usage: $0 <dry-run> <ref-type> <ref-name>" >&2
  exit 2
fi

dry_run="$1"
ref_type="$2"
ref_name="$3"

# A libraries-only release (libraries/vX.Y.Z-beta.N, see LIBRARIES_TAG in
# scripts/sdk_release.py) publishes the SDKs and the CLI and skips every
# server build. It is chosen by the tag alone, never by an input, so a server
# tag can never skip the server and a libraries tag can never build it.
libraries_tag_pattern='^libraries/v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-beta\.(0|[1-9][0-9]*)$'
server=true
if [[ "$ref_type" == "tag" && "$ref_name" == libraries/* ]]; then
  if [[ ! "$ref_name" =~ $libraries_tag_pattern ]]; then
    echo "::error::Libraries-only release tag '$ref_name' does not match the required libraries/v<major>.<minor>.<patch>-beta.<n> format" >&2
    exit 1
  fi
  server=false
fi
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  echo "server=$server" >> "$GITHUB_OUTPUT"
fi

if [[ "$dry_run" == "true" ]]; then
  echo "Dry-run release validation passed for $ref_type '$ref_name' (server build: $server)"
  exit 0
fi

if [[ "$ref_type" != "tag" ]]; then
  echo "::error::Non-dry release dispatches must target a tag, got $ref_type '$ref_name'" >&2
  exit 1
fi

if [[ "$server" == "false" ]]; then
  echo "Libraries-only release validation passed for tag '$ref_name'"
  exit 0
fi

release_tag_pattern='^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.-]*)?$'
if [[ ! "$ref_name" =~ $release_tag_pattern ]]; then
  echo "::error::Release tag '$ref_name' does not match the required v<major>.<minor>.<patch>[-prerelease] format" >&2
  exit 1
fi

echo "Release validation passed for tag '$ref_name'"
