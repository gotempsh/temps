# Multi-stage build for Temps with embedded MaxMind GeoLite2 database
#
# Supports two build modes:
# 1. Build from source (default):
#    docker build -t temps:latest .
#
# 2. Use prebuilt binary (for faster CI/CD):
#    docker build -t temps:latest --build-arg PREBUILT_BINARY=temps .
#    (where 'temps' is the prebuilt binary in the build context)

# Build argument for prebuilt binary (optional)
ARG PREBUILT_BINARY=""

# Stage 1: Builder
FROM rust:1.90-alpine AS builder

ARG PREBUILT_BINARY

# Install required build dependencies
RUN apk add --no-cache \
    bash \
    build-base \
    cmake \
    perl \
    musl-dev \
    pkgconfig \
    openssl-dev \
    postgresql-dev \
    git \
    curl \
    tar \
    gzip

# Install bun using the official installer (faster and doesn't require nightly Rust)
RUN curl -fsSL https://bun.sh/install | bash && \
    ln -s $HOME/.bun/bin/bun /usr/local/bin/bun

# Create app directory
RUN mkdir -p /app

# Copy source code
WORKDIR /build
COPY . .

# Build the binary (with optimizations) - or skip if using prebuilt
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/build/target \
    if [ -z "$PREBUILT_BINARY" ]; then \
      cargo build --release --bin temps && \
      cp /build/target/release/temps /app/temps; \
    else \
      echo "Skipping build - will use prebuilt binary"; \
    fi

# Copy prebuilt binary if provided in build context
RUN if [ -n "$PREBUILT_BINARY" ] && [ -f "/build/$PREBUILT_BINARY" ]; then \
      cp "/build/$PREBUILT_BINARY" /app/temps && \
      chown root:root /app/temps; \
    fi

# Verify binary exists
RUN test -f /app/temps || { \
      echo "ERROR: Binary not found at /app/temps"; \
      echo "If using PREBUILT_BINARY, ensure the file exists in build context"; \
      exit 1; \
    }

# Stage 2: Runtime
FROM alpine:3.20

# Install runtime dependencies
RUN apk add --no-cache \
    ca-certificates \
    libssl3 \
    postgresql-client

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

# Run the application (Pingora handles signals internally)
CMD ["/app/temps", "serve"]

# Build instructions:
# ==================
# 1. Basic build from source (build Rust binary inside Docker):
#    docker build -t temps:latest .
#
# 2. Fast build with prebuilt binary (for CI/CD):
#    First, build the binary outside Docker:
#    cargo build --release --bin temps
#    Then build Docker image with prebuilt binary:
#    docker build -t temps:latest --build-arg PREBUILT_BINARY=target/release/temps .
#    Or copy the binary to current directory first:
#    cp target/release/temps .
#    docker build -t temps:latest --build-arg PREBUILT_BINARY=temps .
#
# 3. Build with embedded GeoLite2 database:
#    First, download the GeoLite2-City.mmdb database:
#    - Visit: https://www.maxmind.com/en/account/login
#    - Create free account and download GeoLite2-City.mmdb
#    - Place the file in the repository root or crates/temps-cli/
#    Then build (works with both modes above):
#    docker build -t temps:latest .
#    docker build -t temps:latest --build-arg PREBUILT_BINARY=temps .
#
# 4. Run the container:
#    docker run -d \
#      --name temps \
#      -p 3000:3000 \
#      -e TEMPS_DATABASE_URL="postgresql://user:password@postgres:5432/temps" \
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
# PREBUILT BINARY:
# - Using a prebuilt binary skips the Rust compilation step, making Docker builds 5-10x faster
# - Ideal for CI/CD pipelines where you build the binary once and reuse it across environments
# - The prebuilt binary must be for the same Linux architecture as the Docker image
#
# GEOLITE2 DATABASE:
# - GeoLite2 database should be placed in the repository root or crates/temps-cli/
#   before building the Docker image for it to be embedded in the image.
# - If GeoLite2 database is not embedded at build time, you can mount it at runtime:
#   docker run -v /path/to/GeoLite2-City.mmdb:/app/data/GeoLite2-City.mmdb temps:latest
# - GeoLite2 database is optional but required for geolocation features
#   Download from: https://www.maxmind.com/en/geolite2/geolite2-free-data-sources
# - Geolocation features will be disabled if database is missing (non-fatal)
#   The application will continue to run normally with a warning in logs
