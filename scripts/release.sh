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
git add Cargo.toml crates/temps-cli/Cargo.toml CHANGELOG.md
git commit -m "chore: bump version to v${VERSION}"

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
