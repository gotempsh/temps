# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# syntax=docker/dockerfile:1
# Cargo.toml and Cargo.lock pin the public SDK; no sibling checkout is required.
FROM rust:1-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock temps/tools/sandbox-runtime/
COPY src temps/tools/sandbox-runtime/src
WORKDIR /build/temps/tools/sandbox-runtime
RUN cargo build --locked --release

FROM oven/bun:1.3.14 AS bun
FROM node:22-bookworm-slim AS common
ARG CODEX_VERSION=0.154.0
ARG CLAUDE_VERSION=2.1.263
ARG OPENCODE_VERSION=1.18.23
# pi ships a self-contained release archive per architecture. Pin its digest
# so a replaced release asset fails the build instead of entering the image.
ARG PI_VERSION=1.0.0
ARG PI_SHA256_LINUX_X64=8fd5543a52a889d60ad57ccbf6c969e73c75c5240aae18ac40b506947a63dc38
ARG PI_SHA256_LINUX_ARM64=b60b3fda830a43c1dc3f5edb5fc7b681f22a0a930b48cdac13b97570c6045819
# BuildKit sets TARGETARCH; the legacy builder leaves it empty.
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends tini ca-certificates git curl jq sudo procps iproute2 \
    && rm -rf /var/lib/apt/lists/* \
    && usermod -l temps -d /home/temps -m node \
    && groupmod -n temps node \
    && install -d -m 0700 -o temps -g temps /run/temps-runtime \
    && install -d -o temps -g temps /home/temps/workspace
RUN npm install --global @openai/codex@${CODEX_VERSION} @anthropic-ai/claude-code@${CLAUDE_VERSION} opencode-ai@${OPENCODE_VERSION} \
    && npm cache clean --force
RUN pi_target="${TARGETARCH:-$(dpkg --print-architecture)}" \
    && case "${pi_target}" in \
        amd64) pi_arch=x64; pi_sha256="${PI_SHA256_LINUX_X64}" ;; \
        arm64) pi_arch=arm64; pi_sha256="${PI_SHA256_LINUX_ARM64}" ;; \
        *) echo "unsupported pi architecture: ${pi_target}" >&2; exit 1 ;; \
    esac \
    && curl -fsSL --proto '=https' -o /tmp/pi.tar.gz \
        "https://github.com/earendil-works/pi/releases/download/v${PI_VERSION}/pi-linux-${pi_arch}.tar.gz" \
    && echo "${pi_sha256}  /tmp/pi.tar.gz" | sha256sum -c - \
    && install -d -m 0755 /opt/pi \
    && tar -xzf /tmp/pi.tar.gz -C /opt/pi --no-same-owner \
    && rm /tmp/pi.tar.gz \
    && ln -s /opt/pi/pi/pi /usr/local/bin/pi \
    && PI_OFFLINE=1 pi --version
COPY --from=bun /usr/local/bin/bun /usr/local/bin/bun
COPY --from=build /build/temps/tools/sandbox-runtime/target/release/temps-sandbox-runtime /usr/local/bin/temps-sandbox-runtime
ENV HOME=/home/temps
LABEL sh.temps.runtime.protocol="1"
LABEL sh.temps.runtime.agent-protocol="4"
USER temps
WORKDIR /home/temps/workspace
HEALTHCHECK --interval=30s --timeout=5s CMD ["temps-sandbox-runtime", "check"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/temps-sandbox-runtime"]
CMD ["serve"]

FROM common AS nodejs
LABEL sh.temps.runtime.flavor="nodejs"

FROM common AS python
USER root
RUN apt-get update && apt-get install -y --no-install-recommends python3 python3-pip python3-venv \
    && rm -rf /var/lib/apt/lists/*
USER temps
LABEL sh.temps.runtime.flavor="python"

FROM python AS all
USER root
RUN apt-get update && apt-get install -y --no-install-recommends build-essential golang rustc cargo \
    && rm -rf /var/lib/apt/lists/*
USER temps
LABEL sh.temps.runtime.flavor="all"
