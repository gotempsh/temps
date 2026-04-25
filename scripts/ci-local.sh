#!/usr/bin/env bash
# ci-local.sh — Run the same checks as the GitHub Actions PR pipeline.
#
# Mirrors .github/workflows/rust-tests.yml jobs: fmt, check, clippy.
# Run this BEFORE pushing to catch failures locally with identical commands.
#
# Usage:
#   scripts/ci-local.sh              # Run all checks (fmt, check, clippy)
#   scripts/ci-local.sh fmt          # Only formatting check
#   scripts/ci-local.sh check        # Only cargo check
#   scripts/ci-local.sh clippy       # Only clippy
#   scripts/ci-local.sh --fix        # Auto-fix fmt + clippy where possible

set -uo pipefail

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Always run from the temps/ crate root regardless of cwd.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

FIX_MODE=0
TARGETS=()
for arg in "$@"; do
    case "$arg" in
        --fix) FIX_MODE=1 ;;
        fmt|check|clippy) TARGETS+=("$arg") ;;
        -h|--help)
            grep '^#' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo -e "${RED}Unknown argument: $arg${NC}" >&2
            exit 2
            ;;
    esac
done

# Default to running everything when no specific target is given.
if [ ${#TARGETS[@]} -eq 0 ]; then
    TARGETS=(fmt check clippy)
fi

# Bare cargo, never the rtk-wrapped one — we want full unfiltered output
# so any clippy/fmt failure is fully visible (the same as in CI logs).
CARGO_BIN="$(command -v cargo)"

run_step() {
    local name="$1"; shift
    echo ""
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${BLUE}▶ $name${NC}"
    echo -e "${BLUE}  $*${NC}"
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    if "$@"; then
        echo -e "${GREEN}✓ $name passed${NC}"
        return 0
    else
        echo -e "${RED}✗ $name failed${NC}"
        return 1
    fi
}

FAILURES=()

for target in "${TARGETS[@]}"; do
    case "$target" in
        fmt)
            if [ "$FIX_MODE" -eq 1 ]; then
                run_step "cargo fmt (apply)" "$CARGO_BIN" fmt --all \
                    || FAILURES+=("fmt")
            else
                run_step "cargo fmt --check" "$CARGO_BIN" fmt --all -- --check \
                    || FAILURES+=("fmt")
            fi
            ;;
        check)
            run_step "cargo check (workspace, all-targets)" \
                "$CARGO_BIN" check --workspace --all-targets \
                || FAILURES+=("check")
            ;;
        clippy)
            # Bust ONLY clippy's per-crate fingerprints so we re-lint every
            # workspace crate — same as a cold CI runner. Compiled rlibs are
            # preserved so the run is fast (~30s vs minutes for a full clean).
            #
            # Without this, clippy reuses cached lint results and silently
            # skips files whose output was already computed, which is how
            # PR-only clippy errors slip past local runs.
            echo -e "${YELLOW}Invalidating clippy fingerprints for fresh lint pass…${NC}"
            find target -path '*/.fingerprint/*' -name 'clippy-*' -prune \
                -exec rm -rf {} + 2>/dev/null || true

            if [ "$FIX_MODE" -eq 1 ]; then
                run_step "cargo clippy --fix" \
                    "$CARGO_BIN" clippy --workspace --all-targets --all-features \
                    --fix --allow-dirty --allow-staged -- -D warnings \
                    || FAILURES+=("clippy")
            else
                run_step "cargo clippy (workspace, all-targets, all-features, -D warnings)" \
                    "$CARGO_BIN" clippy --workspace --all-targets --all-features -- -D warnings \
                    || FAILURES+=("clippy")
            fi
            ;;
    esac
done

echo ""
echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
if [ ${#FAILURES[@]} -eq 0 ]; then
    echo -e "${GREEN}✅ All CI checks passed locally${NC}"
    exit 0
else
    echo -e "${RED}❌ Failed: ${FAILURES[*]}${NC}"
    echo -e "${YELLOW}Tip: re-run with --fix to auto-correct fmt and clippy where possible${NC}"
    exit 1
fi
