# Redis + WAL-G image for Temps Redis backup/restore
# Based on redis:X-bookworm (Debian), adds WAL-G binary for S3-native backups.
# WAL-G binaries are dynamically linked against glibc, so we use Debian instead of Alpine.
#
# Build (single arch):
#   docker build --build-arg REDIS_VERSION=8 -t gotempsh/redis-walg:8-bookworm .
#
# Build (multi-arch, push to Docker Hub):
#   docker buildx build --platform linux/amd64,linux/arm64 \
#     --build-arg REDIS_VERSION=8 -t gotempsh/redis-walg:8-bookworm --push .
#
# WAL-G Redis backup works by:
#   1. Running WALG_STREAM_CREATE_COMMAND (redis-cli --rdb /dev/stdout) to create RDB snapshot
#   2. Compressing and uploading the stream to S3

ARG REDIS_VERSION=8

FROM redis:${REDIS_VERSION}-bookworm

ARG WALG_VERSION=v3.0.8
ARG TARGETARCH

# Install WAL-G binary (Redis variant) from GitHub releases.
# Binaries are named wal-g-redis-{ubuntu_version}-{arch}.
RUN set -eux; \
    apt-get update && apt-get install -y --no-install-recommends curl ca-certificates; \
    WALG_ARCH="${TARGETARCH}"; \
    if [ "$WALG_ARCH" = "arm64" ]; then WALG_ARCH="aarch64"; fi; \
    curl -fSL "https://github.com/wal-g/wal-g/releases/download/${WALG_VERSION}/wal-g-redis-24.04-${WALG_ARCH}.tar.gz" \
      -o /tmp/wal-g.tar.gz; \
    tar -xzf /tmp/wal-g.tar.gz -C /usr/local/bin/; \
    mv /usr/local/bin/wal-g-redis-* /usr/local/bin/wal-g 2>/dev/null || true; \
    chmod +x /usr/local/bin/wal-g; \
    rm -f /tmp/wal-g.tar.gz; \
    apt-get purge -y curl; \
    apt-get autoremove -y; \
    rm -rf /var/lib/apt/lists/*; \
    wal-g --version
