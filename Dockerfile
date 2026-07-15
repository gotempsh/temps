# Multi-stage build for Temps with embedded MaxMind GeoLite2 database
#
# Builds the Rust binary, WASM, and Web UI inside Linux/Alpine so the runtime
# artifact always matches the target architecture and musl libc.
#
# Usage: docker build -t temps:latest .

# Stage 1: Builder
FROM rust:1.94-alpine AS builder

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
    protobuf-dev \
    git \
    curl \
    tar \
    gzip

# Install Node.js and npm (needed for wasm-pack and bun)
RUN apk add --no-cache nodejs npm

# Install bun using the official installer (faster and doesn't require nightly Rust)
RUN curl -fsSL https://bun.sh/install | bash && \
    ln -s $HOME/.bun/bin/bun /usr/local/bin/bun

# Install the Rust-native WASM tooling. The npm wrapper tries to download a
# prebuilt wasm-bindgen binary that does not exist for every Alpine architecture
# (notably arm64), so pin and compile the matching CLI instead.
RUN cargo install wasm-pack --version 0.13.1 --locked && \
    cargo install wasm-bindgen-cli --version 0.2.121 --locked

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

# The musl target links system zlib statically.
RUN apk add --no-cache zlib-static

# Build natively in the Alpine builder. Copying a host-built binary here is
# unsafe: macOS produces Mach-O and ordinary Linux builds target glibc, while
# the runtime stage is musl-based Alpine.
RUN --mount=type=cache,target=/build/target \
    cargo build --release --bin temps --package temps-cli && \
    cp /build/target/release/temps /app/temps && \
    chmod +x /app/temps && \
    chown root:root /app/temps

# Verify binary exists
RUN test -f /app/temps || { \
      echo "ERROR: Binary not found at /app/temps"; \
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

# The city database is tracked in the repository and required by the proxy.
# Keep it outside /app/data so an existing persistent volume cannot mask it
# during an upgrade.
COPY --from=builder /build/crates/temps-cli/GeoLite2-City.mmdb /usr/share/temps/GeoLite2-City.mmdb

# Create data directory structure
RUN mkdir -p /app/data/logs && \
    chown -R appuser:appgroup /app

# Set permissions
RUN chown -R appuser:appgroup /app/data && \
    chmod -R 755 /app/data && \
    chmod 644 /usr/share/temps/GeoLite2-City.mmdb && \
    ln -s /usr/share/temps/GeoLite2-City.mmdb /app/GeoLite2-City.mmdb

# Switch to non-root user
USER appuser:appgroup

# Expose API port
EXPOSE 3000

# Expose TLS port (if configured)
EXPOSE 3443

# Expose the console/API listener
EXPOSE 9000

# Health check
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD wget --no-verbose --tries=1 --spider http://127.0.0.1:9000/readyz || exit 1

# Run the application (Pingora handles signals internally)
CMD ["/app/temps", "serve"]

# Build instructions:
# ==================
# Docker builds the WASM package, web UI, and Rust binary inside its Alpine
# builder. No host toolchain or prebuilt binary is required.
#
# 1. Build the image:
#    docker build -t temps:latest .
#
# 2. Run the container:
#    docker run -d \
#      --name temps \
#      -p 3000:3000 \
#      -p 127.0.0.1:9000:9000 \
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
#   - Stores: logs, encryption keys, and optional runtime databases
#
# Notes:
# ======
# BUILD COMPONENTS:
# - WASM Build: temps-captcha-wasm crate compiled to WebAssembly using wasm-pack
#   Location: Built inside Docker at crates/temps-captcha-wasm/pkg/
# - Web UI Build: Rsbuild frontend application built with bun
#   Location: Built inside Docker at crates/temps-cli/dist/
# - Rust Binary: built natively in Alpine and embeds the generated web UI
#
# GEOLITE2 DATABASE:
# - The tracked crates/temps-cli/GeoLite2-City.mmdb is copied to the immutable
#   /usr/share/temps directory. It remains available when /app/data is an existing
#   persistent volume.
# - GeoLite2-ASN.mmdb is optional and may be mounted under /app/data. It powers
#   hosting/VPS-provider detection used to keep scraper/bot traffic out of the
#   live-visitors view. Without it, that detection is disabled (non-fatal), while
#   city/country geolocation continues to use the bundled city database.
