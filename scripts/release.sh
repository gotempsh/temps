#!/bin/bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Get the version from argument or prompt
VERSION=$1

if [ -z "$VERSION" ]; then
    echo -e "${YELLOW}Enter version (e.g., 1.0.0):${NC}"
    read -r VERSION
fi

# Remove 'v' prefix if present
VERSION=${VERSION#v}

if [ -z "$VERSION" ]; then
    echo -e "${RED}Error: Version is required${NC}"
    exit 1
fi

echo -e "${GREEN}Creating release for version v${VERSION}${NC}"

# Check if we're on main branch
CURRENT_BRANCH=$(git rev-parse --abbrev-ref HEAD)
if [ "$CURRENT_BRANCH" != "main" ]; then
    echo -e "${YELLOW}Warning: Not on main branch (current: ${CURRENT_BRANCH})${NC}"
    echo -e "${YELLOW}Continue anyway? (y/N)${NC}"
    read -r CONTINUE
    if [ "$CONTINUE" != "y" ] && [ "$CONTINUE" != "Y" ]; then
        echo "Aborted"
        exit 1
    fi
fi

# Check for uncommitted changes
if ! git diff-index --quiet HEAD --; then
    echo -e "${RED}Error: Uncommitted changes detected${NC}"
    echo "Please commit or stash your changes first"
    exit 1
fi

# Verify explicitly prepared SDK versions before preparing the server release.
python3 scripts/sdk_release.py plan --tag "v${VERSION}" || exit 1

# SDKs that changed since their last release publish with this tag only once
# the SDK Release Prep PR is merged (pushing release/v${VERSION} or
# `gh workflow run sdk-release-prep.yml -f tag=v${VERSION}` opens it).
# --check changes nothing; it exits 1 while files would change and 2 when the
# check itself failed (a registry lookup, an invalid manifest).
PREPARE_STATUS=0
python3 scripts/sdk_release.py prepare --tag "v${VERSION}" --check > /dev/null || PREPARE_STATUS=$?
if [ "$PREPARE_STATUS" -ne 0 ] && [ "$PREPARE_STATUS" -ne 1 ]; then
    echo -e "${RED}Error: could not determine which SDKs v${VERSION} should publish (see above).${NC}"
    exit 1
fi
if [ "$PREPARE_STATUS" -eq 1 ]; then
    if [ "$(python3 scripts/sdk_release.py plan --tag "v${VERSION}")" != "[]" ]; then
        # Publishing a stale selection would ship newer code under the selected versions.
        echo -e "${RED}Error: the prepared SDK selection for v${VERSION} is stale.${NC}"
        echo "Re-run: gh workflow run sdk-release-prep.yml -f tag=v${VERSION}, then merge its PR."
        exit 1
    fi
    echo -e "${YELLOW}SDK releases for v${VERSION} are not prepared (see above).${NC}"
    echo "Prepare them with: gh workflow run sdk-release-prep.yml -f tag=v${VERSION}, then merge its PR."
    echo -e "${YELLOW}Release the server without publishing those SDK changes? (y/N)${NC}"
    read -r CONTINUE
    if [ "$CONTINUE" != "y" ] && [ "$CONTINUE" != "Y" ]; then
        echo "Aborted"
        exit 1
    fi
fi

# Run tests
echo -e "${GREEN}Running tests...${NC}"
if ! cargo test --workspace --lib; then
    echo -e "${RED}Tests failed!${NC}"
    exit 1
fi

# Run clippy
echo -e "${GREEN}Running clippy...${NC}"
if ! cargo clippy --workspace --lib -- -D warnings; then
    echo -e "${RED}Clippy checks failed!${NC}"
    exit 1
fi

# Check web build
echo -e "${GREEN}Checking web build...${NC}"
cd web
if ! bun run build; then
    echo -e "${RED}Web build failed!${NC}"
    exit 1
fi
cd ..

# Update version in Cargo.toml
echo -e "${GREEN}Updating version in Cargo.toml files...${NC}"

# Update workspace Cargo.toml
sed -i.bak "s/^version = \".*\"/version = \"${VERSION}\"/" Cargo.toml && rm Cargo.toml.bak

# Update temps-cli Cargo.toml
sed -i.bak "s/^version = \".*\"/version = \"${VERSION}\"/" crates/temps-cli/Cargo.toml && rm crates/temps-cli/Cargo.toml.bak

# Keep workspace package versions in the committed lockfile in sync.
# --workspace retains locked third-party dependency versions.
cargo update --workspace
cargo check --lib

# Show changes
echo -e "${GREEN}Version updated in:${NC}"
echo "  - Cargo.toml"
echo "  - crates/temps-cli/Cargo.toml"

# Regenerate CHANGELOG.md from Conventional Commits with git-cliff.
# CHANGELOG.md is a generated artifact — it is NOT hand-edited in PRs (see
# CONTRIBUTING.md). All entries come from commit messages, so releases never
# hit merge conflicts on this file.
if ! command -v git-cliff >/dev/null 2>&1; then
    echo -e "${RED}Error: git-cliff is not installed${NC}"
    echo "Install it with: cargo install git-cliff   (or: brew install git-cliff)"
    exit 1
fi

echo -e "${GREEN}Regenerating CHANGELOG.md for v${VERSION} with git-cliff...${NC}"
# --tag assigns the current unreleased commits to the v${VERSION} section.
git-cliff --tag "v${VERSION}" -o CHANGELOG.md

echo -e "${GREEN}Updated CHANGELOG.md${NC}"
echo -e "${YELLOW}Review the generated changelog and make any needed edits${NC}"
echo -e "${YELLOW}Press Enter when ready to continue...${NC}"
read -r

# Commit version bump
echo -e "${GREEN}Committing version bump...${NC}"
git add Cargo.toml crates/temps-cli/Cargo.toml Cargo.lock CHANGELOG.md
git commit -s -m "chore: bump version to v${VERSION}"

# Create tag
echo -e "${GREEN}Creating tag v${VERSION}...${NC}"
git tag -a "v${VERSION}" -m "Release v${VERSION}"

# Show summary
echo -e "${GREEN}════════════════════════════════════════${NC}"
echo -e "${GREEN}Release v${VERSION} prepared!${NC}"
echo -e "${GREEN}════════════════════════════════════════${NC}"
echo ""
echo "Next steps:"
echo "  1. Review the changes:"
echo "     git show HEAD"
echo ""
echo "  2. Push to GitHub:"
echo -e "     ${YELLOW}git push origin main${NC}"
echo -e "     ${YELLOW}git push origin v${VERSION}${NC}"
echo ""
echo "  3. Monitor the release workflow:"
echo "     https://github.com/gotempsh/temps/actions"
echo ""
echo -e "${YELLOW}Push now? (y/N)${NC}"
read -r PUSH

if [ "$PUSH" = "y" ] || [ "$PUSH" = "Y" ]; then
    echo -e "${GREEN}Pushing to GitHub...${NC}"
    git push origin main
    git push origin "v${VERSION}"
    echo -e "${GREEN}Done! Check GitHub Actions for build progress${NC}"
else
    echo -e "${YELLOW}Not pushed. Run manually when ready:${NC}"
    echo "  git push origin main"
    echo "  git push origin v${VERSION}"
fi
