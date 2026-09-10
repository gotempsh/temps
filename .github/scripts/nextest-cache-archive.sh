#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

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
    if [[ ! -d "$source_path/target/debug" ]]; then
      echo "Cargo debug target directory does not exist: $source_path/target/debug" >&2
      exit 1
    fi
    mkdir -p "$(dirname "$target_path")"
    tar_help="$(tar --help 2>&1 || true)"
    if [[ "$tar_help" == *"--hard-dereference"* ]]; then
      tar --create --auto-compress --dereference --hard-dereference \
        --file "$target_path" -C "$source_path" target/debug
    else
      tar --create --auto-compress --dereference \
        --file "$target_path" -C "$source_path" target/debug
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
        echo "unsafe path in nextest cache seed: $member" >&2
        exit 1
      fi
      if [[ "$normalized" != "target" && "$normalized" != "target/debug" &&
        "$normalized" != target/debug/* ]]; then
        echo "unexpected path in nextest cache seed: $member" >&2
        exit 1
      fi
    done <"$member_list"

    tar -tvf "$source_path" >"$verbose_list"
    while IFS= read -r member; do
      case "${member:0:1}" in
        - | d) ;;
        *)
          echo "links and special files are not allowed in nextest cache seeds" >&2
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
