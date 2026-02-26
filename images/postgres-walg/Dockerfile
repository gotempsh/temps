# PostgreSQL + WAL-G image for Temps backup/restore
# Based on postgres:XX-bookworm (Debian), adds WAL-G binary for S3-native backups.
# WAL-G binaries are dynamically linked against glibc, so we use Debian instead of Alpine.
#
# Build (single arch):
#   docker build --build-arg PG_VERSION=18 -t gotempsh/postgres-walg:18-bookworm .
#
# Build (multi-arch, push to Docker Hub):
#   docker buildx build --platform linux/amd64,linux/arm64 \
#     --build-arg PG_VERSION=18 -t gotempsh/postgres-walg:18-bookworm --push .
#
# The entrypoint wrapper configures WAL archiving (archive_mode, wal_level,
# archive_command) on first boot when WALG_S3_PREFIX is set.

ARG PG_VERSION=18

FROM postgres:${PG_VERSION}-bookworm

ARG WALG_VERSION=v3.0.8
ARG TARGETARCH

# Install WAL-G binary from GitHub releases.
# WAL-G ships database-specific binaries: wal-g-pg for PostgreSQL.
# Binaries are named wal-g-pg-{ubuntu_version}-{arch} (e.g., wal-g-pg-24.04-amd64).
# We use 24.04 builds which work on Debian Bookworm (both are glibc-based).
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

# Copy the WAL-G configuration entrypoint wrapper
COPY docker-entrypoint-walg.sh /docker-entrypoint-initdb.d/00-walg-config.sh
RUN chmod +x /docker-entrypoint-initdb.d/00-walg-config.sh
