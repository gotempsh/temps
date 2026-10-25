#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Approve staged @temps-sdk packages in dependency order.
#
# Usage: approve-packages.sh <tarball-dir>
#
# <tarball-dir> holds the exact .tgz files that were staged, as written by
# `npm pack` (e.g. temps-sdk-cli-0.1.38.tgz) and then staged with
# `npm stage publish ./temps-sdk-cli-0.1.38.tgz`.
#
# - waits for npm's automated review (status: validating -> staged)
# - downloads the staged tarball and refuses to approve unless it is
#   byte-identical to the local one: a stage is matched by name+version only,
#   so anyone holding a publish token could have staged that version first,
#   and approving it unchecked would turn the 2FA prompt into a rubber stamp
# - approves (npm opens a browser for 2FA: press ENTER, confirm there)
# - waits until the version is visible on the registry, then checks the
#   published integrity matches the local tarball too
# Re-runnable: versions already on npm are checked and skipped.
set -u

if [ $# -ne 1 ] || [ ! -d "$1" ]; then
  echo "usage: $0 <tarball-dir>   (directory with the .tgz files you staged)" >&2
  exit 2
fi
TARBALL_DIR="$(cd "$1" && pwd)" || exit 2

# Run from $HOME so the repo's .npmrc (auth = ${NPM_TOKEN}) does not shadow ~/.npmrc.
cd "$HOME" || exit 1

# Order matters: analytics-core first, the packages depending on it after.
PACKAGES=(
  "@temps-sdk/analytics-core@0.0.6"
  "@temps-sdk/react-analytics@0.0.8"
  "@temps-sdk/analytics-browser@0.0.7"
  "@temps-sdk/svelte-analytics@0.0.5"
  "@temps-sdk/vue-analytics@0.0.5"
  "@temps-sdk/plugin@0.1.0-beta.2"
  "@temps-sdk/cli@0.1.38"
)
REQUIRED_FIRST="${PACKAGES[0]}"   # abort the whole run if this one fails

VALIDATE_TIMEOUT=900   # seconds to wait for automated review
PUBLISH_TIMEOUT=300    # seconds to wait for the registry after approving

WORK_DIR="$(mktemp -d)" || exit 1
trap 'rm -rf "$WORK_DIR"' EXIT

published() { [ "$(npm view "$1" version 2>/dev/null)" = "${1##*@}" ]; }

# npm's integrity string for a file: "sha512-<base64>", as in `npm view <spec> dist.integrity`
integrity() { printf 'sha512-%s' "$(openssl dgst -sha512 -binary "$1" | openssl base64 -A)"; }

# local tarball for a spec, named the way `npm pack` names it: @scope/name@1.2.3 -> scope-name-1.2.3.tgz
local_tarball() {
  local name="${1%@*}" ver="${1##*@}"
  name="${name#@}"
  printf '%s/%s-%s.tgz' "$TARBALL_DIR" "${name//\//-}" "$ver"
}

# prints "<stage-id> <status>" for the stage matching name+version
find_stage() {
  npm stage list 2>/dev/null | awk -v pkg="$1" -v ver="$2" '
    BEGIN { RS = ""; FS = "\n" }
    {
      id = ""; p = ""; v = ""; s = ""
      for (i = 1; i <= NF; i++) {
        if      ($i ~ /^id: /)           id = substr($i, 5)
        else if ($i ~ /^package name: /) p  = substr($i, 15)
        else if ($i ~ /^version: /)      v  = substr($i, 10)
        else if ($i ~ /^status: /)       s  = substr($i, 9)
      }
      if (p == pkg && v == ver) { print id, s; exit }
    }'
}

# downloads stage <id> and compares it with <local tgz>; npm lists the files it contains
staged_matches_local() {
  local spec="$1" id="$2" local_tgz="$3" dir="$WORK_DIR/$2" staged want got
  mkdir -p "$dir" || return 1
  echo "  contents of stage $id:"
  if ! (cd "$dir" && npm stage download "$id" >/dev/null); then
    echo "✗ $spec: could not download stage $id for inspection"; return 1
  fi
  staged="$(find "$dir" -maxdepth 1 -name '*.tgz' -print -quit)"
  if [ -z "$staged" ]; then
    echo "✗ $spec: npm stage download $id wrote no tarball"; return 1
  fi
  want="$(integrity "$local_tgz")"; got="$(integrity "$staged")"
  if [ "$want" != "$got" ]; then
    echo "✗ $spec: stage $id does NOT match $local_tgz"
    echo "    local:  $want"
    echo "    staged: $got"
    return 1
  fi
  echo "  stage $id matches $local_tgz ($want)"
}

# set when a staged or published tarball differs from ours: stop everything
TAMPERED=""

approve_one() {
  local spec="$1" name="${1%@*}" ver="${1##*@}" id status waited=0 local_tgz want

  local_tgz="$(local_tarball "$spec")"
  if [ ! -f "$local_tgz" ]; then
    echo "✗ $spec: no local tarball at $local_tgz to verify against"; return 1
  fi
  want="$(integrity "$local_tgz")"

  if published "$spec"; then
    if [ "$(npm view "$spec" dist.integrity 2>/dev/null)" != "$want" ]; then
      echo "✗ $spec is on npm but its integrity differs from $local_tgz"
      TAMPERED="$spec"; return 1
    fi
    echo "✓ $spec already on npm and matches $local_tgz, skipping"; return 0
  fi

  read -r id status < <(find_stage "$name" "$ver")
  if [ -z "${id:-}" ]; then
    echo "✗ $spec: no staged package found (run 'npm stage publish $local_tgz' first)"; return 1
  fi

  while [ "$status" = "validating" ]; do
    if [ "$waited" -ge "$VALIDATE_TIMEOUT" ]; then
      echo "✗ $spec: still validating after ${VALIDATE_TIMEOUT}s"; return 1
    fi
    echo "… $spec validating (${waited}s)"; sleep 15; waited=$((waited + 15))
    read -r id status < <(find_stage "$name" "$ver")
  done

  if [ "$status" != "staged" ]; then
    echo "✗ $spec: unexpected status '$status' (stage $id), not approving"; return 1
  fi

  if ! staged_matches_local "$spec" "$id" "$local_tgz"; then
    echo "  Not approving. Inspect it with 'npm stage download $id' and reject it with 'npm stage reject $id'."
    TAMPERED="$spec"; return 1
  fi

  echo "→ approving $spec (stage $id) — confirm in the browser"
  if ! npm stage approve "$id"; then echo "✗ $spec: approve failed"; return 1; fi

  waited=0
  until published "$spec"; do
    if [ "$waited" -ge "$PUBLISH_TIMEOUT" ]; then
      echo "✗ $spec: approved but not visible on the registry after ${PUBLISH_TIMEOUT}s"; return 1
    fi
    sleep 10; waited=$((waited + 10))
  done
  if [ "$(npm view "$spec" dist.integrity 2>/dev/null)" != "$want" ]; then
    echo "✗ $spec: published, but its integrity differs from $local_tgz"
    TAMPERED="$spec"; return 1
  fi
  echo "✓ $spec published ($want)"
}

failed=()
for spec in "${PACKAGES[@]}"; do
  if ! approve_one "$spec"; then
    failed+=("$spec")
    if [ -n "$TAMPERED" ]; then
      echo "Aborting: $TAMPERED does not match the tarball that was staged locally."; break
    fi
    if [ "$spec" = "$REQUIRED_FIRST" ]; then
      echo "Aborting: dependents need $spec on npm first."; break
    fi
  fi
done

echo
echo "== Summary =="
for spec in "${PACKAGES[@]}"; do
  published "$spec" && echo "  published  $spec" || echo "  MISSING    $spec"
done
npm view @temps-sdk/plugin dist-tags 2>/dev/null

[ "${#failed[@]}" -eq 0 ] || { echo "Failed: ${failed[*]}"; exit 1; }
