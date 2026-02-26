# TimescaleDB + WAL-G image for Temps internal database backup/restore
# Based on timescale/timescaledb-ha:pgXX (Debian-based), adds WAL-G binary.
#
# Build (single arch):
#   docker build --build-arg PG_VERSION=18 -t gotempsh/timescaledb-walg:pg18 .
#
# Build (multi-arch, push to Docker Hub):
#   docker buildx build --platform linux/amd64,linux/arm64 \
#     --build-arg PG_VERSION=18 -t gotempsh/timescaledb-walg:pg18 --push .

ARG PG_VERSION=18

FROM timescale/timescaledb-ha:pg${PG_VERSION}

ARG WALG_VERSION=v3.0.8
ARG TARGETARCH

USER root

# Install WAL-G binary from GitHub releases.
# timescaledb-ha is Debian-based (glibc), so the Ubuntu-compiled binaries work.
RUN set -eux; \
    apt-get update && apt-get install -y --no-install-recommends curl ca-certificates; \
    WALG_ARCH="${TARGETARCH}"; \
    if [ "$WALG_ARCH" = "arm64" ]; then WALG_ARCH="aarch64"; fi; \
    curl -fSL "https://github.com/wal-g/wal-g/releases/download/${WALG_VERSION}/wal-g-pg-24.04-${WALG_ARCH}.tar.gz" \
      -o /tmp/wal-g.tar.gz; \
    tar -xzf /tmp/wal-g.tar.gz -C /usr/local/bin/; \
    mv /usr/local/bin/wal-g-pg-* /usr/local/bin/wal-g 2>/dev/null || true; \
    chmod +x /usr/local/bin/wal-g; \
    rm -f /tmp/wal-g.tar.gz; \
    apt-get purge -y curl; \
    apt-get autoremove -y; \
    rm -rf /var/lib/apt/lists/*; \
    wal-g --version

# Switch back to postgres user (timescaledb-ha default)
USER postgres
