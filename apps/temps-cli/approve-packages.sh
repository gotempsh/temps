#!/usr/bin/env bash
# Approve staged @temps-sdk packages in dependency order.
# - waits for npm's automated review (status: validating -> staged)
# - approves (npm opens a browser for 2FA: press ENTER, confirm there)
# - waits until the version is visible on the registry before moving on
# Re-runnable: versions already on npm are skipped.
set -u

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

published() { [ "$(npm view "$1" version 2>/dev/null)" = "${1##*@}" ]; }

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

approve_one() {
  local spec="$1" name="${1%@*}" ver="${1##*@}" id status waited=0

  if published "$spec"; then echo "✓ $spec already on npm, skipping"; return 0; fi

  read -r id status < <(find_stage "$name" "$ver")
  if [ -z "${id:-}" ]; then
    echo "✗ $spec: no staged package found (run 'npm stage publish' first)"; return 1
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

  echo "→ approving $spec (stage $id) — confirm in the browser"
  if ! npm stage approve "$id"; then echo "✗ $spec: approve failed"; return 1; fi

  waited=0
  until published "$spec"; do
    if [ "$waited" -ge "$PUBLISH_TIMEOUT" ]; then
      echo "✗ $spec: approved but not visible on the registry after ${PUBLISH_TIMEOUT}s"; return 1
    fi
    sleep 10; waited=$((waited + 10))
  done
  echo "✓ $spec published"
}

failed=()
for spec in "${PACKAGES[@]}"; do
  if ! approve_one "$spec"; then
    failed+=("$spec")
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

