#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 3 ]]; then
  echo "usage: $0 <dry-run> <ref-type> <ref-name>" >&2
  exit 2
fi

dry_run="$1"
ref_type="$2"
ref_name="$3"

if [[ "$dry_run" == "true" ]]; then
  echo "Dry-run release validation passed for $ref_type '$ref_name'"
  exit 0
fi

if [[ "$ref_type" != "tag" ]]; then
  echo "::error::Non-dry release dispatches must target a tag, got $ref_type '$ref_name'" >&2
  exit 1
fi

release_tag_pattern='^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.-]*)?$'
if [[ ! "$ref_name" =~ $release_tag_pattern ]]; then
  echo "::error::Release tag '$ref_name' does not match the required v<major>.<minor>.<patch>[-prerelease] format" >&2
  exit 1
fi

echo "Release validation passed for tag '$ref_name'"
