# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# syntax=docker/dockerfile:1
# runtime-src is an explicit named context, never the developer's home directory.
FROM rust:1-bookworm AS build
WORKDIR /build
COPY --from=runtime-src Cargo.toml Cargo.lock temps-agent-runtime/
COPY --from=runtime-src src temps-agent-runtime/src
COPY --from=runtime-src examples/*.rs temps-agent-runtime/examples/
COPY Cargo.toml Cargo.lock temps/tools/sandbox-runtime/
COPY src temps/tools/sandbox-runtime/src
WORKDIR /build/temps/tools/sandbox-runtime
RUN cargo build --locked --release

FROM oven/bun:1.3.14 AS bun
FROM node:22-bookworm-slim AS common
ARG CODEX_VERSION=0.154.0
ARG CLAUDE_VERSION=2.1.263
ARG OPENCODE_VERSION=1.18.23
RUN apt-get update && apt-get install -y --no-install-recommends tini ca-certificates git curl jq sudo \
    && rm -rf /var/lib/apt/lists/* \
    && usermod -l temps -d /home/temps -m node \
    && groupmod -n temps node \
    && install -d -m 0700 -o temps -g temps /run/temps-runtime \
    && install -d -o temps -g temps /home/temps/workspace
RUN npm install --global @openai/codex@${CODEX_VERSION} @anthropic-ai/claude-code@${CLAUDE_VERSION} opencode-ai@${OPENCODE_VERSION} \
    && npm cache clean --force
COPY --from=bun /usr/local/bin/bun /usr/local/bin/bun
COPY --from=build /build/temps/tools/sandbox-runtime/target/release/temps-sandbox-runtime /usr/local/bin/temps-sandbox-runtime
ENV HOME=/home/temps
LABEL sh.temps.runtime.protocol="1"
USER temps
WORKDIR /home/temps/workspace
HEALTHCHECK --interval=10s --timeout=3s CMD ["temps-sandbox-runtime", "request"]
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
