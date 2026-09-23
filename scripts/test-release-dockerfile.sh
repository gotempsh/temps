#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
dockerfile="$repo_root/Dockerfile.release"

grep -Fqx 'FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251' "$dockerfile"
grep -Fq 'RUN chmod +x /usr/local/bin/temps && /usr/local/bin/temps --version' "$dockerfile"
grep -Fq 'COPY crates/temps-cli/GeoLite2-City.mmdb /usr/share/temps/GeoLite2-City.mmdb' "$dockerfile"
grep -Fq 'ln -s /usr/share/temps/GeoLite2-City.mmdb /app/GeoLite2-City.mmdb' "$dockerfile"
grep -Fq 'useradd --uid 1000 --gid temps --create-home' "$dockerfile"

if grep -Eq 'FROM alpine|\bapk (add|update|upgrade)' "$dockerfile"; then
    echo 'Dockerfile.release must not use a musl/Alpine runtime for glibc release artifacts' >&2
    exit 1
fi

echo 'Dockerfile.release libc and runtime contract passed'
