#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Usage: bash build-local.sh unique-image-tag [nodejs|python|all]
set -euo pipefail
image_tag=${1:?Provide a unique development image tag}
flavor=${2:-nodejs}
if [ "$#" -gt 2 ]; then
    echo 'Usage: build-local.sh unique-image-tag [nodejs|python|all]; no SDK checkout is needed' >&2
    exit 1
fi
case "$flavor" in nodejs|python|all) ;; *) echo 'Flavor must be nodejs, python, or all' >&2; exit 1;; esac
package_dir=$(cd "$(dirname "$0")" && pwd)
docker build --target "$flavor" -t "$image_tag" "$package_dir"
