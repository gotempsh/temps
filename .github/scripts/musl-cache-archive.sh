#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 3 ]]; then
  echo "usage: $0 pack|unpack SOURCE TARGET" >&2
  exit 2
fi

operation="$1"
source_path="$2"
target_path="$3"

case "$operation" in
  pack)
    mkdir -p "$(dirname "$target_path")"
    # A tarball preserves Cargo's hidden .fingerprint directories, symlinks,
    # and executable build scripts, which upload-artifact's ZIP does not.
    tar --create --auto-compress --file "$target_path" -C "$source_path" .
    ;;
  unpack)
    mkdir -p "$target_path"
    tar -xf "$source_path" -C "$target_path"
    ;;
  *)
    echo "unknown operation: $operation" >&2
    exit 2
    ;;
esac
