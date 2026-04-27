# PostgreSQL HA image for Temps clusters.
#
# Combines:
#   * postgres:XX-bookworm (the same Debian base as standalone)
#   * pg_auto_failover (cluster orchestration)
#   * WAL-G (S3-native physical backups + PITR)
#
# Build (single arch):
#   docker build --build-arg PG_VERSION=18 -t gotempsh/postgres-ha:18-bookworm .
#
# Build (multi-arch, push to Docker Hub):
#   docker buildx build --platform linux/amd64,linux/arm64 \
#     --build-arg PG_VERSION=18 -t gotempsh/postgres-ha:18-bookworm --push .
#
# ## Why pg_auto_failover + WAL-G in the same image
#
# Operators shouldn't have to swap images to get backups. The previous
# `postgres-ha` image had no WAL-G binary, so cluster backups silently
# fell back to pg_dumpall (no PITR). Bundling WAL-G makes physical
# backups + PITR work for clusters by default.
#
# The runtime cost is one extra ~50 MB binary — negligible next to the
# postgres + pg_auto_failover footprint.
#
# ## archive_command
#
# This image does NOT set `archive_command` at build time. The Temps
# control plane configures it per-cluster after `pg_autoctl create
# postgres` (when WALG_S3_PREFIX is set). pg_auto_failover lays down
# postgresql.conf as part of `create`, so build-time config would just
# get overwritten.

ARG PG_VERSION=18

FROM postgres:${PG_VERSION}-bookworm

ARG PG_VERSION
ARG WALG_VERSION=v3.0.8
ARG TARGETARCH

# 1. pg_auto_failover (cluster orchestration).
#
# Citus's apt repo ships postgresql-${PG_VERSION}-auto-failover and
# pg-auto-failover-cli. The package depends on the matching postgres
# server package which the upstream postgres image already provides,
# so we only pull the failover bits.
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        gnupg \
        lsb-release \
        wget; \
    install -d /etc/apt/keyrings; \
    wget --quiet -O- https://www.postgresql.org/media/keys/ACCC4CF8.asc \
        | gpg --dearmor > /etc/apt/keyrings/pgdg.gpg; \
    echo "deb [signed-by=/etc/apt/keyrings/pgdg.gpg] http://apt.postgresql.org/pub/repos/apt $(lsb_release -cs)-pgdg main" \
        > /etc/apt/sources.list.d/pgdg.list; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        postgresql-${PG_VERSION}-auto-failover \
        pg-auto-failover-cli; \
    apt-get purge -y wget gnupg lsb-release; \
    apt-get autoremove -y; \
    rm -rf /var/lib/apt/lists/*

# 2. WAL-G binary.
#
# WAL-G binaries are dynamically linked against glibc, so we use the
# Ubuntu 24.04 build (also glibc) on Debian Bookworm — matches the
# pattern already used by `images/postgres-walg` and
# `images/timescaledb-walg`.
#
# The binary lands at /usr/local/bin/wal-g. It's invoked by:
#   * Temps backup service: `docker exec` runs `wal-g backup-push` on
#     the current primary (resolved via the pg_auto_failover monitor).
#   * archive_command: set per-cluster by the control plane to
#     `wal-g wal-push %p`. Runs as the postgres user from within the
#     server, so wal-g must be executable by that user.
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
