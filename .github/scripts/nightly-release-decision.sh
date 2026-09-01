#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -euo pipefail

if [[ "$#" -ne 5 ]]; then
  echo "usage: $0 <head-sha> <last-tag> <last-tag-sha> <release-exists> <run-state>" >&2
  exit 2
fi

head_sha="$1"
last_tag="$2"
last_tag_sha="$3"
release_exists="$4"
run_state="$5"

if [[ "$release_exists" != "true" && "$release_exists" != "false" ]]; then
  echo "release-exists must be true or false" >&2
  exit 2
fi

if [[ ! "$run_state" =~ ^(missing|active|success|failed)$ ]]; then
  echo "run-state must be missing, active, success, or failed" >&2
  exit 2
fi

if [[ -z "$last_tag" || "$last_tag_sha" != "$head_sha" ]]; then
  echo "should_release=true"
  echo "should_create_tag=true"
  echo "existing_tag="
elif [[ "$run_state" == "active" ]]; then
  echo "should_release=false"
  echo "should_create_tag=false"
  echo "existing_tag=$last_tag"
elif [[ "$run_state" == "success" && "$release_exists" == "true" ]]; then
  echo "should_release=false"
  echo "should_create_tag=false"
  echo "existing_tag=$last_tag"
else
  # The tag exists at HEAD, but the matching release workflow is missing,
  # failed, or completed without producing a GitHub Release. Re-dispatch the
  # existing tag instead of skipping this commit forever.
  echo "should_release=true"
  echo "should_create_tag=false"
  echo "existing_tag=$last_tag"
fi
