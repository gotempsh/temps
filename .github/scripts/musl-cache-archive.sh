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
    # A tarball preserves Cargo's hidden .fingerprint directories and
    # executable build scripts, which upload-artifact's ZIP does not. Resolve
    # symlinks and hard links while packing so the cross-run archive needs no
    # link members. BSD tar already emits dereferenced links as regular files;
    # GNU tar needs the extra flag to prevent a hard-link optimization.
    tar_help="$(tar --help 2>&1 || true)"
    if [[ "$tar_help" == *"--hard-dereference"* ]]; then
      tar --create --auto-compress --dereference --hard-dereference \
        --file "$target_path" -C "$source_path" .
    else
      tar --create --auto-compress --dereference \
        --file "$target_path" -C "$source_path" .
    fi
    ;;
  unpack)
    member_list="${source_path}.members"
    verbose_list="${source_path}.verbose-members"
    tar -tf "$source_path" >"$member_list"
    while IFS= read -r member; do
      normalized="${member#./}"
      if [[ -z "$normalized" ]]; then
        continue
      fi
      if [[ "$normalized" == /* || "$normalized" == ".." ||
        "$normalized" == ../* || "$normalized" == */../* || "$normalized" == */.. ]]; then
        echo "unsafe path in musl cache seed: $member" >&2
        exit 1
      fi
      if [[ "$normalized" != "target" && "$normalized" != target/* &&
        "$normalized" != "cargo-registry" && "$normalized" != cargo-registry/* ]]; then
        echo "unexpected path in musl cache seed: $member" >&2
        exit 1
      fi
    done <"$member_list"

    tar -tvf "$source_path" >"$verbose_list"
    while IFS= read -r member; do
      case "${member:0:1}" in
        - | d) ;;
        *)
          echo "links and special files are not allowed in musl cache seeds" >&2
          exit 1
          ;;
      esac
    done <"$verbose_list"

    mkdir -p "$target_path"
    tar -xf "$source_path" -C "$target_path"
    ;;
  *)
    echo "unknown operation: $operation" >&2
    exit 2
    ;;
esac
