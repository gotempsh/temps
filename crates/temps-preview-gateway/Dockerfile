# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

## Multi-stage build for temps-preview-gateway.
##
## Builds the crate standalone (outside the temps workspace) so the image
## doesn't need to pull in the other ~60 workspace crates. The build context
## should be this crate's directory:
##
##   docker build -t gotempsh/temps-preview-gateway:dev \
##       crates/temps-preview-gateway

FROM rust:1-bookworm AS builder
WORKDIR /src

COPY Cargo.standalone.toml Cargo.toml
COPY src ./src

RUN cargo build --release && \
    cp target/release/temps-preview-gateway /tmp/temps-preview-gateway

FROM debian:bookworm-slim AS runtime

# Debug tooling kept in the image deliberately: the gateway sits on a
# private docker network with no host port, so the only way to poke it
# is `docker exec`. Shipping curl/wget/ss/dig/bash is worth ~30 MB.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        wget \
        iproute2 \
        dnsutils \
        procps \
        bash \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /tmp/temps-preview-gateway /usr/local/bin/temps-preview-gateway

ENV LISTEN_ADDR=127.0.0.1:8080 \
    SANDBOX_HOST_TEMPLATE=temps-sandbox-{sid} \
    RUST_LOG=info

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/temps-preview-gateway"]
