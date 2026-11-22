#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Build the DinD image and a Linux `temps` binary for the mixed-version
# e2e (run.py). A macOS build cannot run in the Linux worker containers,
# so the binary is compiled inside the image, into a labelled volume that
# keeps the cargo target dir between runs (rebuilds are incremental).
#
# usage: build.sh <old|new> <source tree>
#   old: an older revision, e.g. `git worktree add <dir> origin/main`
#   new: this branch
set -euo pipefail
NAME="${1:?usage: build.sh <old|new> <source tree>}"
SRC="${2:?usage: build.sh <old|new> <source tree>}"
HERE="$(cd "$(dirname "$0")" && pwd)"
LABEL=sh.temps.test=adr055-p3

docker build -q -t adr055-p3-dind:latest --label "$LABEL" \
  -f "$HERE/../Dockerfile.dind-temps" "$HERE/.." >/dev/null
for volume in "adr055-p3-$NAME-src" "adr055-p3-$NAME-target" adr055-p3-cargo-registry adr055-p3-scripts; do
  docker volume create --label "$LABEL" "$volume" >/dev/null
done

# The source tree, without build output or VCS metadata.
COPYFILE_DISABLE=1 tar -C "$SRC" --exclude ./target --exclude ./.git \
  --exclude ./web/node_modules --exclude './web/*/node_modules' --exclude './apps/*/node_modules' \
  -cf - . | docker run --rm -i --label "$LABEL" -v "adr055-p3-$NAME-src":/src \
  --entrypoint sh adr055-p3-dind:latest -c 'rm -rf /src/* && tar -x -C /src 2>/dev/null; du -sh /src'

# The dev-cluster role scripts the containers run, from this checkout.
COPYFILE_DISABLE=1 tar -C "$HERE/../../.." -cf - \
  tools/dev-cluster/role-control-plane.sh tools/dev-cluster/role-worker.sh \
  | docker run --rm -i --label "$LABEL" -v adr055-p3-scripts:/w \
    --entrypoint sh adr055-p3-dind:latest -c 'rm -rf /w/* && tar -x -C /w'

docker run --rm --label "$LABEL" \
  -e SKIP_WEB_BUILD=1 -e CARGO_TARGET_DIR=/target \
  -v "adr055-p3-$NAME-src":/src -v "adr055-p3-$NAME-target":/target \
  -v adr055-p3-cargo-registry:/usr/local/cargo/registry -w /src \
  --entrypoint bash adr055-p3-dind:latest -c \
  'cargo build --bin temps 2>&1 | grep -E "^error|Finished|-->" | tail -30; test -x /target/debug/temps'
