#!/bin/bash
# WAL-G configuration for Temps PostgreSQL containers.
# This script runs as an initdb hook (placed in /docker-entrypoint-initdb.d/).
# It configures WAL archiving when WALG_S3_PREFIX is set.
#
# Required environment variables (set by Temps when creating the container):
#   WALG_S3_PREFIX        - S3 path for WAL-G repo (e.g., s3://bucket/prefix)
#   AWS_ACCESS_KEY_ID     - S3 access key
#   AWS_SECRET_ACCESS_KEY - S3 secret key
#   AWS_ENDPOINT          - S3 endpoint (for MinIO)
#   AWS_S3_FORCE_PATH_STYLE - "true" for MinIO
#   AWS_REGION            - S3 region (default: us-east-1)
#
# Optional:
#   WALG_COMPRESSION_METHOD - Compression method (default: lz4)
#   WALG_DELTA_MAX_STEPS   - Max delta steps before full backup (default: 7)

set -e

if [ -z "$WALG_S3_PREFIX" ]; then
    echo "WAL-G: WALG_S3_PREFIX not set, skipping WAL archiving configuration"
    exit 0
fi

echo "WAL-G: Configuring WAL archiving with S3 prefix: $WALG_S3_PREFIX"

# Configure PostgreSQL for WAL archiving.
# These settings are appended to postgresql.conf during initdb.
# For existing clusters, Temps sets them via -c flags in CMD.
cat >> "$PGDATA/postgresql.conf" <<EOF

# WAL-G archiving configuration (managed by Temps)
wal_level = replica
archive_mode = on
archive_command = 'wal-g wal-push %p'
archive_timeout = 60
EOF

echo "WAL-G: WAL archiving configured successfully"
