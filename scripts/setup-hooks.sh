#!/bin/bash
# Setup pre-commit hooks for the project
# Supports both pre-commit (Python) and prek (Rust)

set -e

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo "🔧 Setting up git hooks..."
echo ""

# Add common paths to PATH for detection
export PATH="$HOME/Library/Python/3.9/bin:$HOME/.local/bin:$HOME/.cargo/bin:$PATH"

# Check if prek is available
if command -v prek &> /dev/null; then
    HOOK_TOOL="prek"
    echo -e "${GREEN}✓ prek found${NC}"
# Check if pre-commit is available
elif command -v pre-commit &> /dev/null; then
    HOOK_TOOL="pre-commit"
    echo -e "${GREEN}✓ pre-commit found${NC}"
else
    echo -e "${BLUE}Choose a git hooks framework:${NC}"
    echo "  1) prek (Rust-based, faster, single binary)"
    echo "  2) pre-commit (Python-based, widely adopted)"
    echo ""
    read -p "Enter choice (1 or 2): " choice

    if [ "$choice" = "1" ]; then
        HOOK_TOOL="prek"
        echo ""
        echo -e "${YELLOW}Installing prek...${NC}"

        # Try different installation methods for prek
        if command -v cargo &> /dev/null; then
            echo "Installing via cargo..."
            cargo install --locked --git https://github.com/j178/prek
        elif command -v brew &> /dev/null; then
            echo "Installing via Homebrew..."
            brew install prek
        else
            echo -e "${YELLOW}Installing via standalone installer...${NC}"
            curl --proto '=https' --tlsv1.2 -LsSf https://github.com/j178/prek/releases/latest/download/prek-installer.sh | sh
        fi
    else
        HOOK_TOOL="pre-commit"
        echo ""
        echo -e "${YELLOW}Installing pre-commit...${NC}"

        # Try different installation methods for pre-commit
        if command -v pip3 &> /dev/null; then
            echo "Installing via pip3..."
            pip3 install --user pre-commit
        elif command -v brew &> /dev/null; then
            echo "Installing via Homebrew..."
            brew install pre-commit
        elif command -v pipx &> /dev/null; then
            echo "Installing via pipx..."
            pipx install pre-commit
        else
            echo -e "${YELLOW}⚠️  Could not find pip3, brew, or pipx${NC}"
            echo ""
            echo "Please install pre-commit manually:"
            echo "  pip install pre-commit"
            echo "  or"
            echo "  brew install pre-commit"
            echo ""
            echo "Then run this script again."
            exit 1
        fi
    fi

    echo ""
fi

# Verify tool is now available
if ! command -v "$HOOK_TOOL" &> /dev/null; then
    echo -e "${YELLOW}⚠️  $HOOK_TOOL was installed but is not in PATH${NC}"
    echo ""
    echo "You may need to add it to your PATH:"
    if [ "$HOOK_TOOL" = "pre-commit" ]; then
        echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
    else
        echo "  export PATH=\"\$HOME/.cargo/bin:\$PATH\""
    fi
    echo ""
    echo "Add this to your ~/.bashrc or ~/.zshrc and restart your terminal."
    exit 1
fi

# Install the git hooks
echo -e "${GREEN}Installing git hooks using $HOOK_TOOL...${NC}"
"$HOOK_TOOL" install
"$HOOK_TOOL" install --hook-type commit-msg

echo ""
echo -e "${GREEN}✅ Git hooks installed using $HOOK_TOOL!${NC}"
echo ""
echo "Hooks will now run automatically on:"
echo "  - git commit (validates code formatting, linting, changelog)"
echo "  - commit message (validates conventional commit format)"
echo ""
echo "To run hooks manually:"
echo "  $HOOK_TOOL run --all-files"
echo ""
echo "To skip hooks (not recommended):"
echo "  git commit --no-verify"
echo ""

# Run hooks once to verify setup
echo -e "${YELLOW}Testing hooks...${NC}"
if "$HOOK_TOOL" run --all-files; then
    echo ""
    echo -e "${GREEN}✅ All hooks passed!${NC}"
else
    echo ""
    echo -e "${YELLOW}⚠️  Some hooks failed. This is normal for first run.${NC}"
    echo "Fix the issues and commit again."
fi

echo ""
echo -e "${GREEN}Setup complete! 🎉${NC}"
