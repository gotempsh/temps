# Multi-stage build for Temps with embedded MaxMind GeoLite2 database
# Stage 1: Builder
FROM rust:1.81-alpine AS builder

# Install required build dependencies
RUN apk add --no-cache \
    musl-dev \
    pkgconfig \
    openssl-dev \
    postgresql-dev \
    git \
    curl \
    tar \
    gzip \
    node \
    npm

# Set build cache mount for cargo (speeds up rebuilds)
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    cargo install bun

# Create app directory
WORKDIR /build

# Copy workspace and all crates
COPY . .

# Build the binary (with optimizations)
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/build/target \
    cargo build --release --bin temps && \
    cp /build/target/release/temps /app/temps

# Stage 2: Download GeoLite2 Database
# This stage downloads the MaxMind GeoLite2-City database
# Note: Requires MAXMIND_LICENSE_KEY build argument for production
FROM curlimages/curl:latest AS geolite2-downloader

ARG MAXMIND_LICENSE_KEY=""

WORKDIR /tmp

# Download GeoLite2-City database from MaxMind
# This requires a valid MAXMIND_LICENSE_KEY for automated downloads
# For manual setup, see the instructions below
RUN if [ -n "$MAXMIND_LICENSE_KEY" ]; then \
      echo "Downloading GeoLite2-City database..." && \
      curl -L "https://download.maxmind.com/app/geoip_download?edition_id=GeoLite2-City&license_key=${MAXMIND_LICENSE_KEY}&suffix=tar.gz" \
        -o GeoLite2-City.tar.gz && \
      tar xzf GeoLite2-City.tar.gz && \
      find . -name "GeoLite2-City.mmdb" -exec cp {} /tmp/GeoLite2-City.mmdb \; && \
      echo "GeoLite2 database downloaded successfully"; \
    else \
      echo "WARNING: MAXMIND_LICENSE_KEY not provided. Database will not be embedded." && \
      echo "To embed the database, build with: --build-arg MAXMIND_LICENSE_KEY=your_key"; \
    fi

# Stage 3: Runtime
FROM alpine:3.20

# Install runtime dependencies
RUN apk add --no-cache \
    ca-certificates \
    libssl3 \
    postgresql-client \
    dumb-init

# Create app user
RUN addgroup -g 1001 -S appgroup && \
    adduser -u 1001 -S appuser -G appgroup

# Create app directory
WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/temps /app/temps

# Create data directory structure
RUN mkdir -p /app/data/logs && \
    chown -R appuser:appgroup /app

# Copy GeoLite2 database if available
COPY --from=geolite2-downloader --chown=appuser:appgroup /tmp/GeoLite2-City.mmdb* /app/data/ || true

# Set permissions
RUN chmod -R 755 /app/data

# Switch to non-root user
USER appuser:appgroup

# Expose API port
EXPOSE 3000

# Expose TLS port (if configured)
EXPOSE 3443

# Health check
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD wget --no-verbose --tries=1 --spider http://localhost:3000/health || exit 1

# Use dumb-init to properly handle signals
ENTRYPOINT ["/sbin/dumb-init", "--"]

# Default command
CMD ["/app/temps", "serve", \
     "--address=0.0.0.0:3000", \
     "--database-url=postgresql://user:password@postgres:5432/temps", \
     "--data-dir=/app/data"]

# Build instructions:
# ==================
# 1. Basic build (without GeoLite2 database embedded):
#    docker build -t temps:latest .
#
# 2. Build with embedded GeoLite2 database:
#    docker build -t temps:latest --build-arg MAXMIND_LICENSE_KEY=your_license_key .
#
#    To get MAXMIND_LICENSE_KEY:
#    - Visit: https://www.maxmind.com/en/account/login
#    - Create free account and generate license key
#    - Use key as build argument
#
# 3. Run the container:
#    docker run -d \
#      --name temps \
#      -p 3000:3000 \
#      -e DATABASE_URL="postgresql://user:password@postgres:5432/temps" \
#      -v temps_data:/app/data \
#      temps:latest
#
# Environment variables:
# ======================
# - TEMPS_ADDRESS: API server address (default: 0.0.0.0:3000)
# - TEMPS_TLS_ADDRESS: TLS server address (optional)
# - TEMPS_DATABASE_URL: PostgreSQL connection string (required)
# - TEMPS_DATA_DIR: Data directory (default: /app/data)
# - TEMPS_CONSOLE_ADDRESS: Console API address (optional)
# - TEMPS_LOG_LEVEL: Log level (default: info)
#
# Volumes:
# ========
# - /app/data: Persistent data directory
#   - Stores: logs, encryption keys, GeoLite2 database, etc.
#
# Notes:
# ======
# - If GeoLite2 database is not embedded at build time, you can mount it:
#   docker run -v /path/to/GeoLite2-City.mmdb:/app/data/GeoLite2-City.mmdb temps:latest
#
# - GeoLite2 database must exist for geolocation features to work
#   Download from: https://www.maxmind.com/en/geolite2/geolite2-free-data-sources
#
# - Geolocation features will be disabled if database is missing (non-fatal)
