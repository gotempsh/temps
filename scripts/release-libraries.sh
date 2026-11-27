#!/bin/bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Cut a libraries-only release: publish the SDKs and the CLI prepared in
# .github/releases/prepared.json without building or releasing the server.
# Run through `scripts/release.sh --libraries`; see its --help.

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

usage() {
    cat <<'EOF'
Usage: scripts/release.sh --libraries [VERSION]

Tag a libraries-only release: release.yml publishes the SDKs and the CLI
(npm, PyPI, crates.io, Go) prepared for it, and builds and releases nothing of
the server (no binaries, images or GitHub Release).

The release reserves a server beta number: libraries/v0.1.0-beta.64 publishes
the packages that follow the server at 0.1.0-beta.64, and the next server
release is v0.1.0-beta.65. Packages on their own version line (the CLI while
it is ahead of the server) keep it. VERSION defaults to the next free number
(python3 scripts/sdk_release.py next-beta); only X.Y.Z-beta.N can be reserved.

Steps:
  1. gh workflow run sdk-release-prep.yml -f tag=libraries/vX.Y.Z-beta.N
     and merge the PR it opens (versions are derived, never picked by hand).
  2. git switch main && git pull, then: scripts/release.sh --libraries
     It checks the selection is prepared and current, creates the annotated
     tag libraries/vX.Y.Z-beta.N on HEAD and offers to push it.
  3. Approve the staged npm versions on npmjs.com, then rerun the
     "Publish prepared SDK releases" job to create the package tags.

Nothing is committed: unlike a server release there is no Cargo.toml bump
and no CHANGELOG.md section; its commits appear in the next server release.

Options:
  -h, --help   Show this help.
EOF
}

VERSION=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        -h|--help) usage; exit 0 ;;
        -*) echo -e "${RED}Error: unknown option $1${NC}" >&2; usage >&2; exit 2 ;;
        *)
            if [ -n "$VERSION" ]; then
                echo -e "${RED}Error: one VERSION at most${NC}" >&2
                exit 2
            fi
            VERSION="$1"
            ;;
    esac
    shift
done

cd "$(dirname "$0")/.."

if ! git diff-index --quiet HEAD --; then
    echo -e "${RED}Error: Uncommitted changes detected${NC}"
    echo "Please commit or stash your changes first"
    exit 1
fi

# The reservation check and the next number need every release tag, and the
# tag must be on main: release.yml's test gate waits for rust-tests.yml on it.
if ! git fetch --quiet --tags origin main; then
    echo -e "${RED}Error: could not fetch main and the release tags from origin${NC}"
    exit 1
fi
if ! git merge-base --is-ancestor HEAD origin/main; then
    echo -e "${RED}Error: HEAD ($(git rev-parse --short HEAD)) is not on origin/main.${NC}"
    echo "Merge the SDK Release Prep PR, then run this from main: git switch main && git pull"
    exit 1
fi

if [ -z "$VERSION" ]; then
    if ! VERSION="$(python3 scripts/sdk_release.py next-beta)"; then
        echo -e "${RED}Error: could not derive the next free beta number (see above); pass VERSION.${NC}"
        exit 1
    fi
    echo -e "${GREEN}Next free release number: ${VERSION}${NC}"
fi
VERSION="${VERSION#libraries/}"
VERSION="${VERSION#v}"
TAG="libraries/v${VERSION}"

# Refuses a number a server or libraries release already took or passed.
python3 scripts/sdk_release.py check-release-tag --tag "$TAG" || exit 1

if ! PLAN="$(python3 scripts/sdk_release.py plan --tag "$TAG")"; then
    echo -e "${RED}Error: the prepared selection for ${TAG} is invalid (see above).${NC}"
    exit 1
fi
if [ "$PLAN" = "[]" ]; then
    echo -e "${RED}Error: no SDK release is prepared for ${TAG}, so it would publish nothing.${NC}"
    echo "Prepare it with: gh workflow run sdk-release-prep.yml -f tag=${TAG}, merge its PR, then rerun this."
    exit 1
fi

# --check changes nothing; it exits 1 while files would change and 2 when the
# check itself failed (a registry lookup, an invalid manifest).
PREPARE_STATUS=0
python3 scripts/sdk_release.py prepare --tag "$TAG" --check > /dev/null || PREPARE_STATUS=$?
if [ "$PREPARE_STATUS" -eq 1 ]; then
    # Publishing a stale selection would ship newer code under the selected versions.
    echo -e "${RED}Error: the prepared SDK selection for ${TAG} is stale.${NC}"
    echo "Re-run: gh workflow run sdk-release-prep.yml -f tag=${TAG}, then merge its PR."
    exit 1
fi
if [ "$PREPARE_STATUS" -ne 0 ]; then
    echo -e "${RED}Error: could not verify the SDK selection for ${TAG} (see above).${NC}"
    exit 1
fi

python3 scripts/sdk_release.py notes --tag "$TAG"

echo -e "${GREEN}Creating tag ${TAG} on $(git rev-parse --short HEAD)...${NC}"
git tag -a "$TAG" -m "Libraries-only release ${TAG}"

echo -e "${GREEN}════════════════════════════════════════${NC}"
echo -e "${GREEN}Libraries-only release ${TAG} prepared!${NC}"
echo -e "${GREEN}════════════════════════════════════════${NC}"
echo ""
echo "Pushing the tag starts release.yml, which tests and publishes the packages"
echo "above and skips every server job. npm versions are staged: approve them on"
echo "npmjs.com, then rerun the \"Publish prepared SDK releases\" job."
echo ""
echo -e "${YELLOW}Push ${TAG} now? (y/N)${NC}"
read -r PUSH

if [ "$PUSH" = "y" ] || [ "$PUSH" = "Y" ]; then
    git push origin "$TAG"
    echo -e "${GREEN}Done! Check GitHub Actions: https://github.com/gotempsh/temps/actions${NC}"
else
    echo -e "${YELLOW}Not pushed. Push it when ready:${NC}"
    echo "  git push origin ${TAG}"
    echo "Or remove it: git tag -d ${TAG}"
fi
