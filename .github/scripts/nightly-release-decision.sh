#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 5 ]]; then
  echo "usage: $0 <head-sha> <last-tag> <last-tag-sha> <release-exists> <active-run-count>" >&2
  exit 2
fi

head_sha="$1"
last_tag="$2"
last_tag_sha="$3"
release_exists="$4"
active_run_count="$5"

if [[ "$release_exists" != "true" && "$release_exists" != "false" ]]; then
  echo "release-exists must be true or false" >&2
  exit 2
fi

if [[ ! "$active_run_count" =~ ^[0-9]+$ ]]; then
  echo "active-run-count must be a non-negative integer" >&2
  exit 2
fi

if [[ -z "$last_tag" || "$last_tag_sha" != "$head_sha" ]]; then
  echo "should_release=true"
  echo "should_create_tag=true"
  echo "existing_tag="
elif [[ "$release_exists" == "true" ]]; then
  echo "should_release=false"
  echo "should_create_tag=false"
  echo "existing_tag=$last_tag"
elif ((active_run_count > 0)); then
  echo "should_release=false"
  echo "should_create_tag=false"
  echo "existing_tag=$last_tag"
else
  # The tag exists at HEAD, but neither a release nor an active release run
  # does. Re-dispatch the existing tag to recover from a prior dispatch/build
  # failure instead of skipping this commit forever.
  echo "should_release=true"
  echo "should_create_tag=false"
  echo "existing_tag=$last_tag"
fi
