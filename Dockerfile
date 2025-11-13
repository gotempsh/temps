# Multi-stage build for Temps with embedded MaxMind GeoLite2 database
#
# REQUIRES: Prebuilt Rust binary (target/release/temps)
# This Dockerfile builds WASM and Web UI inside Docker, then packages
# them with a prebuilt Rust binary.
#
# Usage:
#    cp target/release/temps .
#    docker build -t temps:latest --build-arg PREBUILT_BINARY=temps .

# Build argument for prebuilt binary (REQUIRED)
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

# Install Node.js and npm (needed for wasm-pack and bun)
RUN apk add --no-cache nodejs npm

# Install bun using the official installer (faster and doesn't require nightly Rust)
RUN curl -fsSL https://bun.sh/install | bash && \
    ln -s $HOME/.bun/bin/bun /usr/local/bin/bun

# Install wasm-pack globally
RUN npm install -g wasm-pack

# Install wasm32 target for Rust (needed for WASM compilation)
RUN rustup target add wasm32-unknown-unknown

# Create app directory
RUN mkdir -p /app

# Copy source code
WORKDIR /build
COPY . .

# Build WebAssembly for captcha (required for web UI)
RUN cd /build/crates/temps-captcha-wasm && \
    bun install && \
    npm run build && \
    echo "WASM build completed successfully at pkg/"

# Build web UI (must happen before Rust build to embed in binary)
RUN cd /build/web && \
    bun install && \
    RSBUILD_OUTPUT_PATH=/build/crates/temps-cli/dist \
    bun run build && \
    echo "Web UI build completed at /build/crates/temps-cli/dist"

# Copy prebuilt binary from build context
RUN if [ -f "/build/$PREBUILT_BINARY" ]; then \
      cp "/build/$PREBUILT_BINARY" /app/temps && \
      chmod +x /app/temps && \
      chown root:root /app/temps; \
    else \
      echo "ERROR: Prebuilt binary not found at /build/$PREBUILT_BINARY"; \
      exit 1; \
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
# REQUIRED BUILD STEPS (before Docker):
# The Rust binary MUST be built outside Docker with all dependencies:
#
# 1. Build WebAssembly (temps-captcha-wasm):
#    cd crates/temps-captcha-wasm
#    bun install
#    npm run build
#    Output: pkg/
#
# 2. Build Web UI (must happen before Rust binary):
#    cd web
#    bun install
#    RSBUILD_OUTPUT_PATH=../crates/temps-cli/dist bun run build
#    Output: crates/temps-cli/dist/
#
# 3. Build Rust Binary (includes embedded web UI):
#    cargo build --release --bin temps
#    Output: target/release/temps
#
# DOCKER BUILD (requires prebuilt binary):
#
# 1. Copy the prebuilt binary to build context:
#    cp target/release/temps .
#
# 2. Build Docker image:
#    docker build -t temps:latest --build-arg PREBUILT_BINARY=temps .
#
# 3. Optional: Add GeoLite2 database:
#    First, download from: https://www.maxmind.com/en/account/login
#    Then build Docker image (WASM and Web UI built inside Docker):
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
# BUILD COMPONENTS:
# - WASM Build: temps-captcha-wasm crate compiled to WebAssembly using wasm-pack
#   Location: Built inside Docker at crates/temps-captcha-wasm/pkg/
# - Web UI Build: Rsbuild frontend application built with bun
#   Location: Built inside Docker at crates/temps-cli/dist/
# - Rust Binary: Main server binary (MUST be prebuilt outside Docker)
#   Includes: Embedded web UI via include_dir! macro in temps-cli
#
# WHY PREBUILT BINARY:
# - The Rust binary is built outside Docker to leverage local caches and development setup
# - WASM and Web UI are still built inside Docker (dependencies: nodejs, npm, bun, wasm-pack)
# - Docker builds WASM and Web UI from source, then includes the prebuilt binary
# - The prebuilt binary must:
#   - Be for the same Linux architecture as the Docker image (musl x86_64 for Alpine)
#   - Have been built with full web UI and WASM included (from step 1-3 above)
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
