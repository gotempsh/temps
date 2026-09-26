# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# MariaDB + WAL-G image for direct-to-S3 physical backups and restore proof.
#
# Build:
#   docker build -t ghcr.io/gotempsh/mariadb-walg:11.4-local .
#
# The MySQL WAL-G binary invokes mariadb-backup through
# WALG_STREAM_CREATE_COMMAND. The resulting mbstream never lands on the Temps
# host filesystem.

# Update the version and multi-arch digest together. Resolve the digest with:
#   docker buildx imagetools inspect mariadb:<version> --format '{{json .Manifest}}'
# Refresh WALG_DOWNLOADER_IMAGE with the same command for debian:bookworm-slim.
ARG MARIADB_IMAGE=mariadb:11.4.12-noble@sha256:67873d30a17f6a9c331f06363b2fa15f38abca415529966d67c84f87f82439fe
ARG WALG_DOWNLOADER_IMAGE=debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241

ARG WALG_VERSION=v3.0.8
ARG TARGETARCH

FROM ${WALG_DOWNLOADER_IMAGE} AS walg-downloader

ARG WALG_VERSION
ARG TARGETARCH

# SHA-256 digests below are from the official WAL-G v3.0.8 GitHub release assets.
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates curl; \
    WALG_ARCH="${TARGETARCH}"; \
    case "$WALG_ARCH" in \
      amd64) WALG_SHA256="3f1b234e9d252c968c9698e73b276b8c79de46462c873494e474e7486684bd52" ;; \
      arm64) WALG_ARCH="aarch64"; WALG_SHA256="2d009d991281254361725d449749e37df179ba0ebd8bde878876cc98d2028ed4" ;; \
      *) echo "Unsupported WAL-G architecture: $WALG_ARCH" >&2; exit 1 ;; \
    esac; \
    curl --fail --show-error --location --proto '=https' --tlsv1.2 --retry 3 \
      "https://github.com/wal-g/wal-g/releases/download/${WALG_VERSION}/wal-g-mysql-24.04-${WALG_ARCH}.tar.gz" \
      -o /tmp/wal-g.tar.gz; \
    echo "${WALG_SHA256}  /tmp/wal-g.tar.gz" | sha256sum --check --strict; \
    tar -xzf /tmp/wal-g.tar.gz -C /tmp "wal-g-mysql-24.04-${WALG_ARCH}"; \
    install -m 0755 "/tmp/wal-g-mysql-24.04-${WALG_ARCH}" /wal-g

FROM ${MARIADB_IMAGE}

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends mariadb-backup; \
    rm -rf /var/lib/apt/lists/*

COPY --from=walg-downloader /wal-g /usr/local/bin/wal-g

RUN set -eux; \
    test -x /bin/bash; \
    wal-g --version; \
    mariadb-backup --version
