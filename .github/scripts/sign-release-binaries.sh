#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -euo pipefail
release_dir="${1:?Usage: sign-release-binaries.sh <release-directory>}"
: "${CERTIFICATE_IDENTITY:?Expected release workflow identity is required}"
issuer=https://token.actions.githubusercontent.com
# Require every supported target; an incomplete release must fail publication.
for platform in linux-amd64 linux-arm64 darwin-amd64 darwin-arm64; do
  artifact="$release_dir/temps-$platform.tar.gz"
  [[ -s "$artifact" ]] || { echo "::error::Missing release tarball: $artifact" >&2; exit 1; }
done
for platform in linux-amd64 linux-arm64 darwin-amd64 darwin-arm64; do
  artifact="$release_dir/temps-$platform.tar.gz"
  bundle="$artifact.sigstore.json"
  cosign sign-blob --yes --bundle "$bundle" "$artifact"
  cosign verify-blob --bundle "$bundle" \
    --certificate-identity "$CERTIFICATE_IDENTITY" \
    --certificate-oidc-issuer "$issuer" "$artifact"
done
