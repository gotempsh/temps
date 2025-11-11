# Multi-stage build for Temps with embedded MaxMind GeoLite2 database
# Stage 1: Builder
FROM rust:1.90-alpine AS builder

# Install required build dependencies
RUN apk add --no-cache \
    musl-dev \
    pkgconfig \
    openssl-dev \
    postgresql-dev \
    git \
    curl \
    tar \
    gzip

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

# Stage 2: Runtime
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

# Copy GeoLite2 database from local repository if available
# The database should be placed in the repository root or crates/temps-cli directory
RUN if [ -f GeoLite2-City.mmdb ]; then \
      cp GeoLite2-City.mmdb /app/data/GeoLite2-City.mmdb && \
      chown appuser:appgroup /app/data/GeoLite2-City.mmdb; \
    elif [ -f crates/temps-cli/GeoLite2-City.mmdb ]; then \
      cp crates/temps-cli/GeoLite2-City.mmdb /app/data/GeoLite2-City.mmdb && \
      chown appuser:appgroup /app/data/GeoLite2-City.mmdb; \
    else \
      echo "Note: GeoLite2 database not found in repository. Geolocation features will be disabled."; \
    fi

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

# Default command (uses environment variables for configuration)
CMD ["/app/temps", "serve"]

# Build instructions:
# ==================
# 1. Basic build (without GeoLite2 database embedded):
#    docker build -t temps:latest .
#
# 2. Build with embedded GeoLite2 database:
#    First, download the GeoLite2-City.mmdb database:
#    - Visit: https://www.maxmind.com/en/account/login
#    - Create free account and download GeoLite2-City.mmdb
#    - Place the file in the repository root or crates/temps-cli/
#    Then build:
#    docker build -t temps:latest .
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
# - GeoLite2 database should be placed in the repository root or crates/temps-cli/
#   before building the Docker image for it to be embedded in the image.
#
# - If GeoLite2 database is not embedded at build time, you can mount it at runtime:
#   docker run -v /path/to/GeoLite2-City.mmdb:/app/data/GeoLite2-City.mmdb temps:latest
#
# - GeoLite2 database is optional but required for geolocation features
#   Download from: https://www.maxmind.com/en/geolite2/geolite2-free-data-sources
#
# - Geolocation features will be disabled if database is missing (non-fatal)
#   The application will continue to run normally with a warning in logs
