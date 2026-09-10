#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Usage: bash build-local.sh /absolute/path/to/temps-agent-runtime unique-image-tag
set -euo pipefail
runtime_source=${1:?Provide the local temps-agent-runtime checkout}
image_tag=${2:?Provide a unique development image tag}
flavor=${3:-nodejs}
case "$flavor" in nodejs|python|all) ;; *) echo 'Flavor must be nodejs, python, or all' >&2; exit 1;; esac
test -f "$runtime_source/src/lib.rs"
package_dir=$(cd "$(dirname "$0")" && pwd)
docker build --target "$flavor" --build-context "runtime-src=$runtime_source" -t "$image_tag" "$package_dir"
