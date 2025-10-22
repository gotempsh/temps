#!/bin/bash
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

# Check if CHANGELOG.md exists
if [ ! -f "CHANGELOG.md" ]; then
    echo -e "${YELLOW}CHANGELOG.md not found. Creating template...${NC}"
    cat > CHANGELOG.md << EOF
# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Initial release

### Changed
-

### Fixed
-

EOF
    echo -e "${GREEN}Created CHANGELOG.md template${NC}"
fi

# Check if [Unreleased] section exists
if ! grep -q "## \[Unreleased\]" CHANGELOG.md; then
    echo -e "${RED}Error: CHANGELOG.md is missing [Unreleased] section${NC}"
    echo "Please add an [Unreleased] section with your changes"
    exit 1
fi

# Check if there are any entries in Unreleased
if ! grep -A 20 "## \[Unreleased\]" CHANGELOG.md | grep -q "^- "; then
    echo -e "${YELLOW}Warning: [Unreleased] section appears to be empty${NC}"
    echo -e "${YELLOW}Please add your changes before continuing${NC}"
    echo -e "${YELLOW}See .github/CHANGELOG_TEMPLATE.md for examples${NC}"
    echo ""
    echo -e "${YELLOW}Continue anyway? (y/N)${NC}"
    read -r CONTINUE
    if [ "$CONTINUE" != "y" ] && [ "$CONTINUE" != "Y" ]; then
        echo "Aborted"
        exit 1
    fi
fi

echo -e "${GREEN}Converting [Unreleased] to [${VERSION}]...${NC}"

# Create backup
cp CHANGELOG.md CHANGELOG.md.bak

# Replace [Unreleased] with version and date, and add new [Unreleased] section
TODAY=$(date +%Y-%m-%d)
sed -i.tmp "s/## \[Unreleased\]/## [${VERSION}] - ${TODAY}/" CHANGELOG.md && rm CHANGELOG.md.tmp

# Add new [Unreleased] section at the top
sed -i.tmp "/^## \[${VERSION}\]/i\\
## [Unreleased]\\
\\
### Added\\
-\\
\\
### Changed\\
-\\
\\
### Fixed\\
-\\
\\
" CHANGELOG.md && rm CHANGELOG.md.tmp

echo -e "${GREEN}Updated CHANGELOG.md${NC}"
echo -e "${YELLOW}Review the changes and make any needed edits${NC}"
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
echo "     https://github.com/YOUR_ORG/temps/actions"
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
