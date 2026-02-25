# MongoDB + WAL-G image for Temps MongoDB backup/restore
# Based on mongo:X (Debian-based), adds WAL-G binary for S3-native backups.
#
# Build (single arch):
#   docker build --build-arg MONGO_VERSION=8.0 -t gotempsh/mongodb-walg:8.0 .
#
# Build (multi-arch, push to Docker Hub):
#   docker buildx build --platform linux/amd64,linux/arm64 \
#     --build-arg MONGO_VERSION=8.0 -t gotempsh/mongodb-walg:8.0 --push .
#
# WAL-G MongoDB backup supports:
#   - Logical backups (mongodump-based via WALG_STREAM_CREATE_COMMAND)
#   - Oplog archiving for point-in-time recovery

ARG MONGO_VERSION=8.0

FROM mongo:${MONGO_VERSION}

ARG WALG_VERSION=v3.0.8
ARG TARGETARCH

# Install WAL-G binary (MongoDB variant) from GitHub releases.
# mongo images are Debian-based (glibc).
RUN set -eux; \
    apt-get update && apt-get install -y --no-install-recommends curl ca-certificates; \
    WALG_ARCH="${TARGETARCH}"; \
    if [ "$WALG_ARCH" = "arm64" ]; then WALG_ARCH="aarch64"; fi; \
    curl -fSL "https://github.com/wal-g/wal-g/releases/download/${WALG_VERSION}/wal-g-mongo-24.04-${WALG_ARCH}.tar.gz" \
      -o /tmp/wal-g.tar.gz; \
    tar -xzf /tmp/wal-g.tar.gz -C /usr/local/bin/; \
    mv /usr/local/bin/wal-g-mongo-* /usr/local/bin/wal-g 2>/dev/null || true; \
    chmod +x /usr/local/bin/wal-g; \
    rm -f /tmp/wal-g.tar.gz; \
    apt-get purge -y curl; \
    apt-get autoremove -y; \
    rm -rf /var/lib/apt/lists/*; \
    wal-g --version
