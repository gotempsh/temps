# Temps

A modern, self-hosted platform for deploying and managing web applications with built-in analytics, monitoring, and error tracking.

## Features

- 🚀 **Easy Deployment** - Deploy from Git repositories with automatic builds
- 📊 **Analytics** - Built-in analytics with funnels, session replay, and performance monitoring
- 🔍 **Error Tracking** - Sentry-compatible error tracking with grouping and deduplication
- 🌐 **Reverse Proxy** - Automatic TLS/ACME certificate management
- 📦 **Managed Services** - PostgreSQL, Redis, S3/MinIO support
- 🔐 **Authentication** - Built-in user management and permissions
- 📈 **Status Page** - Uptime monitoring and incident management
- 💾 **Backups** - Automated backup and restore for your data

## Quick Start

### Installation

**Linux AMD64:**
```bash
curl -LO https://github.com/YOUR_ORG/temps/releases/latest/download/temps-linux-amd64
chmod +x temps-linux-amd64
sudo mv temps-linux-amd64 /usr/local/bin/temps
```

### Running

```bash
# Initialize and start the server
temps serve --address 0.0.0.0:8080 --database-url postgresql://user:pass@localhost/temps
```

## Development

### Prerequisites

- Rust 1.70+ (https://rustup.rs)
- Bun (https://bun.sh)
- PostgreSQL 15+ with TimescaleDB
- Docker (for testing)

### Setup

```bash
# Clone the repository
git clone https://github.com/YOUR_ORG/temps.git
cd temps

# Install Rust dependencies
cargo build

# Install web dependencies
cd web
bun install
cd ..
```

### Building

```bash
# Development build (fast, skips web build)
cargo build

# Development build with web UI
FORCE_WEB_BUILD=1 cargo build

# Release build (includes web UI automatically)
cargo build --release
```

### Testing

```bash
# Run unit tests
cargo test --workspace --lib

# Run all tests (requires Docker)
cargo test --workspace
```

### Development Server

**Terminal 1 - Rust server:**
```bash
cargo run --bin temps serve \
  --address 127.0.0.1:8081 \
  --database-url postgresql://postgres:postgres@localhost:5432/temps
```

**Terminal 2 - Web UI:**
```bash
cd web
bun run dev
```

Open http://localhost:3000 (web dev server proxies API to port 8081)

## Architecture

Temps is a Cargo workspace with 30+ crates organized by domain:

- **temps-cli** - Command-line interface and main binary
- **temps-core** - Core types and utilities
- **temps-analytics** - Analytics engine with funnels and session replay
- **temps-auth** - Authentication and authorization
- **temps-deployer** - Docker/container runtime
- **temps-deployments** - Deployment workflow orchestration
- **temps-git** - Git provider integrations (GitHub, GitLab)
- **temps-proxy** - Reverse proxy with TLS (Pingora-based)
- **temps-providers** - Managed services (PostgreSQL, Redis, S3)
- **temps-error-tracking** - Sentry-compatible error tracking
- **temps-monitoring** - Status page and uptime monitoring

See [CLAUDE.md](CLAUDE.md) for detailed architecture and development guidelines.

## Project Structure

```
temps/
├── crates/           # Rust workspace crates
│   ├── temps-cli/    # Main binary
│   │   └── dist/     # Web UI build output (generated)
│   ├── temps-core/   # Core functionality
│   └── ...           # Other crates
├── web/              # React web UI
│   ├── src/          # Source code
│   └── public/       # Static assets
├── scripts/          # Helper scripts
├── .github/          # GitHub Actions workflows
├── CLAUDE.md         # AI assistant development guide
└── RELEASING.md      # Release process documentation
```

## Configuration

### Environment Variables

- `TEMPS_ADDRESS` - HTTP server address (default: `127.0.0.1:3000`)
- `TEMPS_DATABASE_URL` - PostgreSQL connection string (required)
- `TEMPS_TLS_ADDRESS` - HTTPS server address (optional)
- `TEMPS_DATA_DIR` - Data directory (default: `~/.temps`)
- `TEMPS_LOG_LEVEL` - Log level (default: `info`)

### Data Directory

Temps stores encryption keys and configuration in the data directory:
- `~/.temps/encryption_key` - AES-256 encryption key
- `~/.temps/auth_secret` - Session authentication secret

These are auto-generated on first run.

## Deployment

### Docker (Coming Soon)

```bash
docker run -d \
  -p 8080:8080 \
  -v temps-data:/root/.temps \
  -e TEMPS_DATABASE_URL=postgresql://... \
  temps/temps:latest
```

### Systemd

See [docs/systemd.md](docs/systemd.md) for systemd service configuration.

## Contributing

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

### Development Workflow

1. Fork and clone the repository
2. Create a feature branch: `git checkout -b feature/my-feature`
3. Make your changes following [CLAUDE.md](CLAUDE.md) guidelines
4. Run tests: `cargo test --workspace`
5. Run clippy: `cargo clippy --workspace -- -D warnings`
6. Format code: `cargo fmt --all`
7. Commit and push your changes
8. Open a pull request

## Releasing

See [RELEASING.md](RELEASING.md) for the release process.

Quick release:
```bash
./scripts/release.sh 1.0.0
```

## License

MIT OR Apache-2.0

## Support

- 📖 [Documentation](https://docs.temps.dev)
- 💬 [Discord](https://discord.gg/temps)
- 🐛 [Issue Tracker](https://github.com/YOUR_ORG/temps/issues)
- 📧 [Email](mailto:support@temps.dev)

## Credits

Built with:
- [Rust](https://www.rust-lang.org/) - Systems programming language
- [Pingora](https://github.com/cloudflare/pingora) - Reverse proxy framework
- [Sea-ORM](https://www.sea-ql.org/SeaORM/) - Database ORM
- [React](https://react.dev/) - Web UI framework
- [Rsbuild](https://rsbuild.dev/) - Build tooling
- [TimescaleDB](https://www.timescale.com/) - Time-series database
