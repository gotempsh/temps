#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Decide the GitHub Release kind and the floating Docker tag for a release.
#
#   v1.2.3                    stable      -> is_prerelease=false channel_tag=latest
#   v1.2.3-beta.4, -rc.1      beta / rc   -> is_prerelease=true  channel_tag=beta
#   v1.2.3-nightly.<d>.<sha>  nightly     -> is_prerelease=true  channel_tag=nightly
#   any other prerelease      (e.g. -test)-> is_prerelease=true  channel_tag=
#
# An empty channel_tag means "publish the exact version tag only and move no
# floating tag", so an ad-hoc prerelease can never land on :beta or :latest.
# This mirrors the installer and `temps upgrade`, whose beta channel already
# excludes nightly builds: `:beta` now holds the same builds as that channel.
#
# Dry-runs are reported as beta prereleases; they push nothing, but must never
# look like a stable release to later steps.

set -euo pipefail

if [[ "$#" -ne 2 ]]; then
  echo "usage: $0 <dry-run> <release-tag>" >&2
  exit 2
fi

dry_run="$1"
tag="$2"

nightly_pattern='^v[0-9]+\.[0-9]+\.[0-9]+-nightly\.[0-9A-Za-z.-]+$'
beta_pattern='^v[0-9]+\.[0-9]+\.[0-9]+-(beta|rc)\.[0-9]+$'
stable_pattern='^v[0-9]+\.[0-9]+\.[0-9]+$'

if [[ "$dry_run" == "true" ]]; then
  echo "is_prerelease=true"
  echo "channel_tag=beta"
elif [[ "$tag" =~ $nightly_pattern ]]; then
  echo "is_prerelease=true"
  echo "channel_tag=nightly"
elif [[ "$tag" =~ $beta_pattern ]]; then
  echo "is_prerelease=true"
  echo "channel_tag=beta"
elif [[ "$tag" =~ $stable_pattern ]]; then
  echo "is_prerelease=false"
  echo "channel_tag=latest"
elif [[ "$tag" == *-* ]]; then
  echo "is_prerelease=true"
  echo "channel_tag="
else
  echo "::error::Cannot determine the release channel for '$tag'" >&2
  exit 1
fi
