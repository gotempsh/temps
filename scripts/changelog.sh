#!/bin/bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Regenerate or preview CHANGELOG.md from Conventional Commits with git-cliff.
#
# CHANGELOG.md is a generated artifact — never hand-edit it. All entries come
# from commit messages (see cliff.toml and CONTRIBUTING.md).
#
# Usage:
#   scripts/changelog.sh                 # regenerate the whole CHANGELOG.md in place
#   scripts/changelog.sh --unreleased    # print unreleased entries (since last tag) to stdout
#   scripts/changelog.sh --tag vX.Y.Z    # regenerate, labelling unreleased commits as vX.Y.Z
set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v git-cliff >/dev/null 2>&1; then
  echo "Error: git-cliff is not installed." >&2
  echo "Install it with: cargo install git-cliff   (or: brew install git-cliff)" >&2
  exit 1
fi

case "${1:-}" in
  --unreleased)
    git-cliff --unreleased --strip all
    ;;
  --tag)
    if [ -z "${2:-}" ]; then
      echo "Error: --tag requires a version, e.g. --tag v0.1.0-beta.41" >&2
      exit 1
    fi
    git-cliff --tag "$2" -o CHANGELOG.md
    echo "Regenerated CHANGELOG.md (unreleased commits labelled $2)"
    ;;
  "")
    git-cliff -o CHANGELOG.md
    echo "Regenerated CHANGELOG.md"
    ;;
  *)
    echo "Unknown option: $1" >&2
    echo "Usage: scripts/changelog.sh [--unreleased | --tag vX.Y.Z]" >&2
    exit 1
    ;;
esac
