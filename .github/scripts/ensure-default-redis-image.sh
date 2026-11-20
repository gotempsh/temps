#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Make the default managed Redis image available to the local Docker daemon.
#
# Tests and scenarios create Redis services from DEFAULT_REDIS_IMAGE. A change
# that moves that default to a tag database-images.yml has not published yet
# (it publishes only from main) would otherwise fail every Redis test with
# "manifest unknown". The published image is used when it exists, the same
# one a fresh install pulls; otherwise the image is built from this checkout
# under the default's tag, and the providers' pull falls back to that local
# copy (pull_image_with_retry).
set -euo pipefail

source_file=crates/temps-providers-redis/src/redis.rs
image=$(sed -nE 's/^pub const DEFAULT_REDIS_IMAGE: &str = "([^"]+)";$/\1/p' "$source_file")
if [[ -z "$image" ]]; then
  echo "DEFAULT_REDIS_IMAGE not found in $source_file" >&2
  exit 1
fi

if docker pull "$image"; then
  echo "Using the published $image"
  exit 0
fi

# `8-trixie` -> REDIS_VERSION=8, as database_image_matrix.py builds it.
tag=${image##*:}
redis_version=${tag%%-*}
arch=$(docker version --format '{{.Server.Arch}}')
echo "$image is not published yet; building it from images/redis-walg (REDIS_VERSION=$redis_version)"
docker build \
  --build-arg "REDIS_VERSION=$redis_version" \
  --build-arg "TARGETARCH=$arch" \
  --tag "$image" \
  images/redis-walg
docker run --rm --entrypoint wal-g "$image" --version
docker run --rm --entrypoint redis-server "$image" --version
