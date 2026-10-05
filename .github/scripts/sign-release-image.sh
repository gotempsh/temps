#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -euo pipefail
refs=("${IMAGE_REPOSITORY}:${IMAGE_VERSION}")
if [[ -n "$IMAGE_CHANNEL" ]]; then
  refs+=("${IMAGE_REPOSITORY}:${IMAGE_CHANNEL}")
fi
# Sign registry-resolved digests, including the floating channel's
# actual manifest; do not sign a mutable tag or assume equal digests.
for reference in "${refs[@]}"; do
  digest=$(docker buildx imagetools inspect "$reference" --format '{{.Manifest.Digest}}')
  if [[ ! "$digest" =~ ^sha256:[0-9a-f]{64}$ ]]; then
    echo "::error::Cannot resolve published image digest for $reference"
    exit 1
  fi
  cosign sign --yes "${IMAGE_REPOSITORY}@${digest}"
done
