#!/usr/bin/env bash
# Build and push all sandbox images to GHCR (ghcr.io/gotempsh/).
# Requires: docker buildx, logged in to GHCR (`docker login ghcr.io`).
# Usage:   ./scripts/build-sandbox-images.sh [runtime...]
# Channel: SANDBOX_CHANNEL=stable|beta (default: beta)
#
# If no runtimes specified, builds all: node bun python rust go full
#
# Image version is read from SANDBOX_IMAGE_VERSION in
# crates/temps-agents/src/sandbox/docker.rs so the script and the runtime
# stay in lock-step. The release workflow runs the same logic in CI.
#
# Channel rules (matches .github/workflows/release.yml):
#   stable -> :<ver>, :<ver>-stable, :latest, :stable
#   beta   -> :<ver>-beta, :beta  (never the unsuffixed canonical tag)
#
# Default is `beta` so an accidental local push cannot poison the stable
# `:<ver>` ref. Set SANDBOX_CHANNEL=stable explicitly when promoting.

set -euo pipefail

CHANNEL="${SANDBOX_CHANNEL:-beta}"
case "$CHANNEL" in
    stable|beta) ;;
    *)
        echo "error: SANDBOX_CHANNEL must be 'stable' or 'beta' (got: $CHANNEL)" >&2
        exit 1
        ;;
esac
echo "Release channel: $CHANNEL"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if [ $# -eq 0 ]; then
    RUNTIMES=(node bun python rust go full)
else
    RUNTIMES=("$@")
fi

# Extract the pinned image version from the Rust source so we never publish
# under the wrong tag. Single source of truth for the version string.
IMAGE_VERSION=$(grep -E '^pub const SANDBOX_IMAGE_VERSION' \
    "$REPO_ROOT/crates/temps-agents/src/sandbox/docker.rs" \
    | sed -E 's/.*"([^"]+)".*/\1/')
if [ -z "$IMAGE_VERSION" ]; then
    echo "error: failed to extract SANDBOX_IMAGE_VERSION from docker.rs" >&2
    exit 1
fi
echo "Publishing sandbox images at version: $IMAGE_VERSION"

PRINT_DOCKERFILE="$REPO_ROOT/target/debug/examples/print_dockerfile"
PRINT_BUNDLE="$REPO_ROOT/target/debug/examples/print_bundle"

# Always rebuild the helpers so Dockerfile and bundle changes in
# temps-agents land in the pushed images. Cargo short-circuits if nothing
# changed, so the cost is just a stat-check on incremental builds.
echo "Building print_dockerfile + print_bundle helpers..."
cargo build --example print_dockerfile -p temps-agents --manifest-path "$REPO_ROOT/Cargo.toml"
cargo build --example print_bundle -p temps-agents --manifest-path "$REPO_ROOT/Cargo.toml"

if [ ! -x "$PRINT_DOCKERFILE" ]; then
    echo "error: print_dockerfile binary not found at $PRINT_DOCKERFILE after build" >&2
    exit 1
fi
if [ ! -x "$PRINT_BUNDLE" ]; then
    echo "error: print_bundle binary not found at $PRINT_BUNDLE after build" >&2
    exit 1
fi

# Ensure buildx builder with multi-arch support
BUILDER_NAME="temps-multiarch"
if ! docker buildx inspect "$BUILDER_NAME" >/dev/null 2>&1; then
    echo "Creating buildx builder $BUILDER_NAME..."
    docker buildx create --name "$BUILDER_NAME" --driver docker-container --use
else
    docker buildx use "$BUILDER_NAME"
fi

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

for runtime in "${RUNTIMES[@]}"; do
    IMAGE="ghcr.io/gotempsh/temps-sandbox-${runtime}"
    BUILD_DIR="$TMPDIR/$runtime"
    mkdir -p "$BUILD_DIR"

    "$PRINT_DOCKERFILE" "$runtime" > "$BUILD_DIR/Dockerfile"

    # Materialize the bundles the generated Dockerfile expects. The
    # in-process build path (`build_context_tar`) packs these into a tar
    # at runtime; this is the on-disk equivalent for `docker buildx build`.
    # Without it the COPY pty-agent/ and COPY git-credential/ stages fail
    # with "not found" before any apt-get can run.
    "$PRINT_BUNDLE" "$BUILD_DIR" > /dev/null

    # Stable owns the canonical `:<ver>` ref; beta only publishes suffixed
    # refs so it can never overwrite a stable image at the same version.
    if [ "$CHANNEL" = "stable" ]; then
        TAG_ARGS=(
            --tag "$IMAGE:$IMAGE_VERSION"
            --tag "$IMAGE:$IMAGE_VERSION-stable"
            --tag "$IMAGE:latest"
            --tag "$IMAGE:stable"
        )
        TAG_LIST="$IMAGE:$IMAGE_VERSION, :$IMAGE_VERSION-stable, :latest, :stable"
    else
        TAG_ARGS=(
            --tag "$IMAGE:$IMAGE_VERSION-beta"
            --tag "$IMAGE:beta"
        )
        TAG_LIST="$IMAGE:$IMAGE_VERSION-beta, :beta"
    fi

    echo ""
    echo "=========================================="
    echo "Building and pushing ($CHANNEL): $TAG_LIST"
    echo "=========================================="

    docker buildx build \
        --platform linux/amd64,linux/arm64 \
        "${TAG_ARGS[@]}" \
        --push \
        "$BUILD_DIR"

    echo "✓ $TAG_LIST pushed"
done

echo ""
echo "All images built and pushed successfully."
