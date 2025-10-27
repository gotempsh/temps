# Temps

<div align="center">

**Deploy ANY application from Git with built-in analytics, monitoring, and error tracking.**

**Self-hosted • Production-ready • Zero vendor lock-in**

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![GitHub Release](https://img.shields.io/github/v/release/gotempsh/temps)](https://github.com/gotempsh/temps/releases)

[Quick Start](#-quick-start) • [Features](#-features) • [Installation](#-installation) • [Deploy an App](#-deploying-your-first-application) • [Contributing](#-contributing)

</div>

---

## ⚡ Quick Start

**Deploy your first app in 60 seconds:**

```bash
# 1. Start PostgreSQL with TimescaleDB (one-time setup, runs on port 15432 to avoid conflicts)
docker run -d \
  --name temps-postgres \
  -e POSTGRES_USER=postgres \
  -e POSTGRES_PASSWORD=temps \
  -e POSTGRES_DB=temps \
  -p 15432:5432 \
  timescale/timescaledb-ha:pg16

# 2. Install Temps with one command
curl -fsSL https://raw.githubusercontent.com/gotempsh/temps/main/scripts/install.sh | bash

# Restart your shell or source your profile
source ~/.zshrc  # or ~/.bashrc

# 3. Start Temps
temps serve \
  --address 0.0.0.0:8080 \
  --database-url postgresql://postgres:temps@localhost:15432/temps

# 4. Open http://localhost:8080 in your browser

# 5. Connect GitHub/GitLab → Select repository → Click "Deploy"

# ✅ Your app is now live with:
#    • HTTPS (automatic TLS)
#    • Analytics
#    • Error tracking
#    • Monitoring
#    • Live logs
```

**That's it!** No configuration files, no Docker Compose, no Kubernetes manifests. Just point Temps at your Git repo and deploy.

**Note:** TimescaleDB extension is required. If you have native PostgreSQL, you must install TimescaleDB separately (see [Installation](#installation) section below).

---

## 🌟 Overview

Temps is your **self-hosted deployment platform** that makes it effortless to deploy and manage ANY application - from React frontends to Node.js APIs, Python backends, static sites, and everything in between. Simply point it at your Git repository, and Temps handles the rest.

**Deploy once, monitor forever** - with built-in analytics, error tracking, session replay, uptime monitoring, and performance insights that would normally require 5+ separate SaaS subscriptions.

### What Can You Deploy?

| Application Type | Examples | Temps Support |
|-----------------|----------|---------------|
| **Frontend Apps** | React, Next.js, Vue, Svelte, Angular | ✅ Zero-config |
| **Backend APIs** | Node.js, Python, Go, Rust, Ruby, PHP | ✅ Auto-detected |
| **Static Sites** | Hugo, Jekyll, Gatsby, plain HTML | ✅ Served with nginx |
| **Full-Stack** | Next.js, Nuxt, SvelteKit, Remix | ✅ SSR supported |
| **Databases** | PostgreSQL, Redis | ✅ Managed services |
| **CMS** | WordPress, Strapi, Ghost | ✅ One-click deploy |
| **Custom Apps** | Anything with a Dockerfile | ✅ Full Docker support |

### Why Temps?

- **🚀 Deploy Anything**: React, Next.js, Vue, Python, Node.js, Go, Rust, static sites - if it runs in Docker, Temps can deploy it
- **⚡ Git Push to Deploy**: Connect your GitHub or GitLab repo, select a branch, and deploy in seconds
- **🏠 Self-Hosted**: Run on your own infrastructure - complete control, no vendor lock-in, no usage limits
- **📊 All-in-One Observability**: Analytics, error tracking (Sentry-compatible), session replay, uptime monitoring, and performance metrics built-in
- **🔐 Production-Grade**: Automatic TLS/ACME certificates, managed databases (PostgreSQL, Redis), S3 storage, and enterprise security
- **💰 Zero SaaS Costs**: Replace Vercel + Sentry + Datadog + Logtail and save thousands per month

### Temps vs. Others

| Feature | Temps | Vercel/Netlify | Heroku | AWS/GCP | Self-Hosted Docker |
|---------|-------|----------------|--------|---------|-------------------|
| **Deploy Any App** | ✅ All languages | ⚠️ JS-focused | ✅ Yes | ✅ Yes | ✅ Yes |
| **Zero Config** | ✅ Auto-detect | ✅ Yes | ⚠️ Limited | ❌ Manual | ❌ Manual |
| **Built-in Analytics** | ✅ Included | ❌ Extra cost | ❌ No | ❌ Extra cost | ❌ No |
| **Error Tracking** | ✅ Sentry-compatible | ❌ Extra cost | ❌ No | ❌ Extra cost | ❌ No |
| **Session Replay** | ✅ Included | ❌ Extra cost | ❌ No | ❌ Extra cost | ❌ No |
| **Self-Hosted** | ✅ Your servers | ❌ SaaS only | ❌ SaaS only | ⚠️ Complex | ✅ Yes |
| **Cost** | 💰 Server only | 💰💰💰 Per-user/usage | 💰💰 Per-dyno | 💰💰💰 Complex | 💰 Server + time |
| **Data Privacy** | ✅ Full control | ❌ Third-party | ❌ Third-party | ⚠️ Your cloud | ✅ Full control |
| **Custom Domains** | ✅ Unlimited | 💰 Paid plans | ✅ Yes | ✅ Yes | ✅ Yes |
| **Managed Databases** | ✅ Included | ❌ Extra cost | ✅ Add-ons | ✅ Yes | ❌ DIY |

**Cost Example:** Running 5 apps with analytics, error tracking, and monitoring:
- **Temps**: ~$50/month (VPS + storage)
- **Vercel + Sentry + Analytics**: ~$500+/month
- **AWS + third-party tools**: ~$800+/month

---

## ✨ Features

### 🚀 Deploy ANY Application

**Temps supports ANY application that can run in a container:**

#### **Frontend Frameworks**
- ✅ React, Next.js, Vue.js, Nuxt, Svelte, SvelteKit
- ✅ Vite, Rsbuild, Webpack, Parcel, Rollup
- ✅ Static sites (Hugo, Jekyll, Gatsby, Docusaurus)
- ✅ Angular, Ember, Preact, Solid.js

#### **Backend Languages & Frameworks**
- ✅ Node.js (Express, Fastify, NestJS, Koa, Hapi)
- ✅ Python (Django, Flask, FastAPI, Pyramid)
- ✅ Go (Gin, Echo, Fiber, Chi)
- ✅ Rust (Axum, Actix, Rocket, Warp)
- ✅ Ruby (Rails, Sinatra, Hanami)
- ✅ PHP (Laravel, Symfony, WordPress)
- ✅ Java/Kotlin (Spring Boot, Micronaut, Ktor)
- ✅ .NET (ASP.NET Core)

#### **How Deployment Works**

```
┌─────────────┐      ┌──────────────┐      ┌─────────────┐      ┌──────────────┐
│  Git Push   │─────▶│ Auto-Detect  │─────▶│ Build       │─────▶│ Deploy       │
│  (GitHub/   │      │ Framework    │      │ Container   │      │ with TLS     │
│   GitLab)   │      │              │      │             │      │              │
└─────────────┘      └──────────────┘      └─────────────┘      └──────────────┘
                             │                                            │
                             ▼                                            ▼
                     ┌──────────────┐                            ┌──────────────┐
                     │   Next.js    │                            │  Analytics   │
                     │   Vite       │                            │  Monitoring  │
                     │   Python     │                            │  Logs        │
                     │   Custom     │                            │  Errors      │
                     └──────────────┘                            └──────────────┘
```

**Deployment is a 5-step process:**
1. **Connect Git Repository**: Link your GitHub or GitLab repo
2. **Auto-Detection**: Temps detects your stack and builds automatically
3. **Containerization**: Creates optimized Docker containers
4. **Deploy**: Zero-downtime deployment with automatic rollback
5. **Monitor**: Built-in analytics, logs, errors, and performance metrics

#### **Deployment Features**
- **Zero-Config Presets**: Built-in support for Next.js, Vite, Docusaurus, and more
- **Custom Dockerfiles**: Full control with your own Dockerfile
- **Environment Variables**: Secure environment management per deployment
- **Multiple Environments**: Deploy staging, production, and custom environments
- **Automatic TLS**: Free SSL certificates with automatic renewal (ACME/Let's Encrypt)
- **Custom Domains**: Connect unlimited custom domains to your applications
- **Rollback**: One-click rollback to any previous deployment
- **Git Branch Tracking**: Auto-deploy on push to specific branches

### 📊 Analytics & Observability

- **Event Tracking**: Capture custom events and user interactions
- **Funnel Analysis**: Track user journeys and conversion rates
- **Session Replay**: Record and replay user sessions for debugging
- **Performance Monitoring**: Real-time performance metrics and insights
- **User Behavior Analytics**: Understand how users interact with your applications
- **Geolocation Data**: Track user locations and regional performance

### 🔍 Error Tracking & Debugging

- **Sentry-Compatible**: Use existing Sentry SDKs without code changes
- **Smart Error Grouping**: AI-powered error clustering with embeddings
- **Stack Trace Analysis**: Detailed stack traces with source mapping
- **Real-Time Alerts**: Get notified when errors occur
- **Error Deduplication**: Intelligent grouping to reduce noise

### 📈 Monitoring & Status Pages

- **Uptime Monitoring**: Track application availability 24/7
- **Status Pages**: Public status pages for your services
- **Incident Management**: Create and track incidents
- **Health Checks**: Automated endpoint monitoring
- **Performance Metrics**: CPU, memory, and response time tracking

### 🔐 Security & Access Control

- **Role-Based Access Control (RBAC)**: Fine-grained permissions system
- **User Management**: Built-in authentication and user administration
- **AES-256 Encryption**: Encrypt sensitive data at rest
- **Audit Logging**: Complete audit trail of all operations
- **Session Management**: Secure session handling with encrypted cookies
- **2FA Support**: Two-factor authentication with TOTP

### 💾 Data Management

- **Automated Backups**: Schedule automatic backups of your databases
- **Point-in-Time Recovery**: Restore to any point in time
- **Data Retention Policies**: Automatic cleanup of old data
- **Export Capabilities**: Export your data in standard formats
- **Database Migrations**: Versioned schema migrations with Sea-ORM

### 🎨 Modern Web Interface

- **Responsive Dashboard**: Beautiful, dark/light mode UI built with React
- **Real-Time Updates**: Live data streaming with React Query
- **Terminal Access**: In-browser terminal for container logs
- **Analytics Visualizations**: Interactive charts with Nivo
- **Component Library**: Built with shadcn/ui and Radix UI

---

## 🚀 Quick Start

### Prerequisites

- **Docker** (recommended for easiest setup) OR **PostgreSQL 15+** with TimescaleDB extension
- **Linux AMD64** or **macOS** (ARM64 support coming soon)

### Installation

#### Quick Install (Recommended)

Install Temps with a single command using our install script:

```bash
# Install latest version
curl -fsSL https://raw.githubusercontent.com/gotempsh/temps/main/scripts/install.sh | bash

# Or install a specific version
curl -fsSL https://raw.githubusercontent.com/gotempsh/temps/main/scripts/install.sh | bash -s v0.1.0
```

The installer will:
- Automatically detect your platform (Linux/macOS, AMD64/ARM64)
- Download and extract the appropriate binary
- Install to `~/.temps/bin/`
- Add to your PATH in your shell config

After installation, restart your shell or run:
```bash
source ~/.zshrc  # or ~/.bashrc
temps --version
```

#### Manual Installation

##### Linux AMD64

```bash
# Download and extract
curl -LO https://github.com/gotempsh/temps/releases/latest/download/temps-linux-amd64.tar.gz
tar -xzf temps-linux-amd64.tar.gz

# Move to your PATH
sudo mv temps /usr/local/bin/temps

# Verify installation
temps --version
```

##### macOS (Intel)

```bash
# Download and extract
curl -LO https://github.com/gotempsh/temps/releases/latest/download/temps-darwin-amd64.tar.gz
tar -xzf temps-darwin-amd64.tar.gz

# Move to your PATH
sudo mv temps /usr/local/bin/temps

# Verify installation
temps --version
```

##### macOS (Apple Silicon)

```bash
# Download and extract
curl -LO https://github.com/gotempsh/temps/releases/latest/download/temps-darwin-arm64.tar.gz
tar -xzf temps-darwin-arm64.tar.gz

# Move to your PATH
sudo mv temps /usr/local/bin/temps

# Verify installation
temps --version
```

##### Homebrew (Coming Soon)

```bash
brew tap gotempsh/temps
brew install temps
```

#### Build from Source

```bash
# Clone the repository
git clone https://github.com/gotempsh/temps.git
cd temps

# Build release binary (includes web UI)
cargo build --release

# The binary will be at target/release/temps
sudo cp target/release/temps /usr/local/bin/

# Verify installation
temps --version
```

### Database Setup

**Option 1: Docker (Recommended - Easiest)**

```bash
# Start PostgreSQL with TimescaleDB on port 15432 (avoids conflicts)
docker run -d \
  --name temps-postgres \
  -e POSTGRES_PASSWORD=temps \
  -e POSTGRES_DB=temps \
  -p 15432:5432 \
  timescale/timescaledb-ha:pg16

# Verify it's running
docker ps | grep temps-postgres
```

**Option 2: Native Installation (Requires TimescaleDB)**

⚠️ **Important:** You cannot just install regular PostgreSQL. TimescaleDB must be installed separately.

```bash
# Ubuntu/Debian:
# 1. Add PostgreSQL repository
sudo sh -c 'echo "deb http://apt.postgresql.org/pub/repos/apt $(lsb_release -cs)-pgdg main" > /etc/apt/sources.list.d/pgdg.list'
wget --quiet -O - https://www.postgresql.org/media/keys/ACCC4CF8.asc | sudo apt-key add -

# 2. Add TimescaleDB repository
sudo sh -c "echo 'deb https://packagecloud.io/timescale/timescaledb/ubuntu/ $(lsb_release -c -s) main' > /etc/apt/sources.list.d/timescaledb.list"
wget --quiet -O - https://packagecloud.io/timescale/timescaledb/gpgkey | sudo apt-key add -

# 3. Install both PostgreSQL and TimescaleDB
sudo apt-get update
sudo apt-get install -y postgresql-15 timescaledb-postgresql-15

# 4. Configure TimescaleDB
sudo timescaledb-tune --quiet --yes

# 5. Restart PostgreSQL
sudo systemctl restart postgresql

# 6. Create database and enable extension
sudo -u postgres createdb temps
sudo -u postgres psql temps -c "CREATE EXTENSION IF NOT EXISTS timescaledb;"

# macOS with Homebrew:
# 1. Install PostgreSQL and TimescaleDB
brew install postgresql@15
brew install timescaledb

# 2. Follow the post-install instructions to configure TimescaleDB
timescaledb-tune --quiet --yes

# 3. Restart PostgreSQL
brew services restart postgresql@15

# 4. Create database and enable extension
createdb temps
psql temps -c "CREATE EXTENSION IF NOT EXISTS timescaledb;"
```

**Not seeing TimescaleDB?** It won't appear as a simple PostgreSQL extension. You must install the TimescaleDB package for your PostgreSQL version. See [TimescaleDB installation docs](https://docs.timescale.com/self-hosted/latest/install/) for detailed instructions.

### Running Temps

#### Complete Setup Example

Here's a complete example to get Temps running from scratch:

```bash
# 1. Start TimescaleDB (if not already running)
docker run -d \
  --name temps-postgres \
  -e POSTGRES_USER=postgres \
  -e POSTGRES_PASSWORD=temps \
  -e POSTGRES_DB=temps \
  -p 15432:5432 \
  timescale/timescaledb-ha:pg16

# Verify database is running
docker ps | grep temps-postgres

# 2. Install Temps (if not already installed)
curl -fsSL https://raw.githubusercontent.com/gotempsh/temps/main/scripts/install.sh | bash

# 3. Start Temps server
temps serve \
  --address 0.0.0.0:8080 \
  --database-url postgresql://postgres:temps@localhost:15432/temps
```

#### Alternative: Using Environment Variables

```bash
# Set environment variables
export TEMPS_ADDRESS=0.0.0.0:8080
export TEMPS_DATABASE_URL=postgresql://postgres:temps@localhost:15432/temps

# Start server
temps serve
```

#### If Using Native PostgreSQL (Port 5432)

```bash
temps serve \
  --address 0.0.0.0:8080 \
  --database-url postgresql://postgres:password@localhost:5432/temps
```

The web interface will be available at `http://localhost:8080`

#### First Run

On first run, Temps will automatically:
1. Create data directory at `~/.temps`
2. Generate encryption keys and auth secrets
3. Run all database migrations
4. Create the default admin user

**First login credentials will be displayed in the console output.** Save them securely!

### First Login

After starting Temps for the first time, you can log in with the credentials displayed in the console output.

---

## 🎯 Deploying Your First Application

Once Temps is running, deploying an application takes just a few clicks:

### Step 1: Connect Git Provider

1. Navigate to **Settings → Git Providers**
2. Click **Connect GitHub** or **Connect GitLab**
3. Authorize Temps to access your repositories

### Step 2: Create a Project

1. Go to **Projects → New Project**
2. Select your repository from the list
3. Choose the branch to deploy (e.g., `main`)
4. Pick an environment (staging, production, or create custom)

### Step 3: Configure & Deploy

**For supported frameworks (auto-detected):**
```
✅ Temps automatically detects:
   - Next.js, Vite, React, Vue, Svelte
   - Node.js, Python, Go, Rust
   - Docusaurus, Hugo, Jekyll

✅ Zero configuration needed - just click "Deploy"
```

**For custom applications:**
```dockerfile
# Add a Dockerfile to your repository
FROM node:20-alpine
WORKDIR /app
COPY package*.json ./
RUN npm install
COPY . .
RUN npm run build
CMD ["npm", "start"]
```

### Step 4: Access Your Application

After deployment completes (~30-60 seconds):

- **URL**: `https://your-project.temps.yourdomain.com`
- **Logs**: View real-time build and runtime logs
- **Metrics**: CPU, memory, response times
- **Errors**: Automatic error tracking (Sentry-compatible)

### Example Deployments

#### Deploy a Next.js App
```bash
# Your repository structure
my-nextjs-app/
├── package.json
├── next.config.js
└── pages/
    └── index.tsx

# Temps auto-detects and deploys with:
# - Build command: npm run build
# - Start command: npm start
# - Port: 3000 (auto-detected)
```

#### Deploy a Python FastAPI App
```bash
# Your repository structure
my-api/
├── requirements.txt
├── main.py
└── Dockerfile (optional)

# Temps auto-detects Python and uses:
# - pip install -r requirements.txt
# - uvicorn main:app --host 0.0.0.0
```

#### Deploy a Static Site
```bash
# Any static HTML/CSS/JS site
my-site/
├── index.html
├── style.css
└── script.js

# Temps serves with nginx automatically
```

### Advanced: Custom Build Configuration

Override auto-detection with a `temps.yml` in your repository:

```yaml
# temps.yml
build:
  command: npm run build
  output: dist/

runtime:
  command: npm start
  port: 3000

env:
  NODE_ENV: production
  API_URL: https://api.example.com

health_check:
  path: /health
  interval: 30s
```

---

## 📖 Documentation

### Configuration

Temps can be configured via environment variables or command-line flags:

| Environment Variable | CLI Flag | Default | Description |
|---------------------|----------|---------|-------------|
| `TEMPS_ADDRESS` | `--address` | `127.0.0.1:3000` | HTTP server address |
| `TEMPS_DATABASE_URL` | `--database-url` | (required) | PostgreSQL connection string |
| `TEMPS_TLS_ADDRESS` | `--tls-address` | (optional) | HTTPS server address |
| `TEMPS_DATA_DIR` | `--data-dir` | `~/.temps` | Data directory for keys/config |
| `TEMPS_LOG_LEVEL` | `--log-level` | `info` | Log level (trace, debug, info, warn, error) |
| `TEMPS_CONSOLE_ADDRESS` | `--console-address` | (optional) | Admin console address |

### Data Directory

Temps stores sensitive data in the data directory (`~/.temps` by default):

```
~/.temps/
├── encryption_key    # AES-256 encryption key (auto-generated)
└── auth_secret       # Session authentication secret (auto-generated)
```

**⚠️ Important**: Back up these files! Losing them means you cannot decrypt existing data.

### Commands

```bash
# Start the full HTTP API server
temps serve [OPTIONS]

# Start only the proxy server
temps proxy [OPTIONS]

# Show help
temps --help

# Show version
temps --version
```

### Database Migrations

Temps automatically runs migrations on startup. To manually manage migrations:

```bash
# Migrations are handled automatically
# No manual intervention needed
```

---

## 🏗️ Architecture

Temps is built as a **Cargo workspace** with 40+ specialized crates following a clean three-layer architecture:

### Three-Layer Architecture

```
┌─────────────────────────────────────────────────────────┐
│  HTTP Layer (Axum Handlers)                             │
│  - Request/response handling                            │
│  - OpenAPI documentation                                │
│  - Authentication & authorization                       │
└─────────────────────────────────────────────────────────┘
                         ↓
┌─────────────────────────────────────────────────────────┐
│  Service Layer (Business Logic)                         │
│  - Domain logic                                         │
│  - Transaction management                               │
│  - External service integration                         │
└─────────────────────────────────────────────────────────┘
                         ↓
┌─────────────────────────────────────────────────────────┐
│  Data Access Layer (Sea-ORM)                            │
│  - Database queries                                     │
│  - Entity management                                    │
│  - Relationship handling                                │
└─────────────────────────────────────────────────────────┘
```

### Key Crates

**Application Layer:**
- `temps-cli` - Main binary and CLI entry point

**Core Infrastructure:**
- `temps-core` - Shared utilities, cryptography services
- `temps-database` - Database connection pooling
- `temps-entities` - Sea-ORM database entities
- `temps-migrations` - Database schema migrations
- `temps-auth` - Authentication & authorization
- `temps-routes` - HTTP route definitions

**Feature Domains:**
- `temps-deployments` - Deployment orchestration
- `temps-deployer` - Docker/container runtime
- `temps-proxy` - Reverse proxy (Pingora-based)
- `temps-analytics` - Analytics engine
- `temps-analytics-funnels` - Funnel tracking
- `temps-analytics-session-replay` - Session replay
- `temps-error-tracking` - Error tracking (Sentry-compatible)
- `temps-monitoring` - Uptime monitoring
- `temps-git` - Git provider integrations
- `temps-providers` - Managed services (PostgreSQL, Redis, S3)
- `temps-logs` - Container log streaming
- `temps-backup` - Backup & restore
- `temps-notifications` - Email & notifications
- `temps-audit` - Audit logging

### Technology Stack

**Backend:**
- **Language**: Rust 1.70+ (Edition 2021)
- **Web Framework**: Axum (async HTTP)
- **Database ORM**: Sea-ORM with PostgreSQL + TimescaleDB
- **Reverse Proxy**: Pingora (Cloudflare's production proxy)
- **Container Runtime**: Bollard (Docker SDK)
- **API Docs**: Utoipa (OpenAPI/Swagger)
- **Authentication**: Custom with Argon2 password hashing
- **Cryptography**: AES-GCM-256, SHA-256, TOTP
- **TLS**: Instant-ACME, rcgen

**Frontend:**
- **Framework**: React 19 with TypeScript
- **Build Tool**: Rsbuild (Rspack-based)
- **Component Library**: shadcn/ui (Radix UI primitives)
- **Styling**: Tailwind CSS
- **Data Fetching**: TanStack Query (React Query)
- **Forms**: React Hook Form + Zod validation
- **Charts**: Nivo
- **Terminal**: XTerm.js
- **Session Replay**: rrweb

**Infrastructure:**
- **Database**: PostgreSQL 15+ with TimescaleDB
- **Cache**: Redis (optional)
- **Storage**: S3-compatible (MinIO, AWS S3)
- **Container**: Docker
- **Email**: SMTP via Lettre

---

## 🛠️ Development

### Prerequisites

- **Rust**: 1.70+ ([Install via rustup](https://rustup.rs))
- **Bun**: Latest version ([Install Bun](https://bun.sh))
- **PostgreSQL**: 15+ with TimescaleDB
- **Docker**: For testing and deployments

### Setup

```bash
# Clone the repository
git clone https://github.com/gotempsh/temps.git
cd temps

# Install pre-commit hooks (recommended)
./scripts/setup-hooks.sh

# Build the project (debug mode, fast)
cargo build

# Build with web UI
FORCE_WEB_BUILD=1 cargo build

# Build release version (includes web UI automatically)
cargo build --release
```

### Development Workflow

#### Backend Development

```bash
# Check compilation (fast, use after every change)
cargo check --lib

# Run specific crate tests
cargo test --lib -p temps-deployments

# Run all unit tests
cargo test --workspace --lib

# Run with Docker integration tests
cargo test --workspace

# Run linter
cargo clippy --workspace -- -D warnings

# Format code
cargo fmt --all
```

#### Frontend Development

```bash
cd web

# Install dependencies
bun install

# Start dev server (with hot reload)
bun run dev

# Build for production
bun run build

# Type checking
bun run typecheck

# Linting
bun run lint
```

#### Full Stack Development

**Terminal 1 - Backend API:**
```bash
cargo run --bin temps serve \
  --address 127.0.0.1:8081 \
  --database-url postgresql://postgres:postgres@localhost:5432/temps
```

**Terminal 2 - Frontend Dev Server:**
```bash
cd web
bun run dev
```

Open [http://localhost:3000](http://localhost:3000) - the dev server proxies API requests to port 8081.

### Project Structure

```
temps/
├── crates/                   # Rust workspace crates
│   ├── temps-cli/            # Main binary
│   │   ├── src/              # CLI source code
│   │   ├── dist/             # Web UI build output (generated)
│   │   └── build.rs          # Build script for web integration
│   ├── temps-core/           # Core utilities
│   ├── temps-analytics/      # Analytics engine
│   ├── temps-auth/           # Authentication
│   ├── temps-deployer/       # Container runtime
│   ├── temps-deployments/    # Deployment workflows
│   ├── temps-proxy/          # Reverse proxy
│   └── ...                   # Other domain crates
├── web/                      # React web UI
│   ├── src/
│   │   ├── components/       # React components
│   │   ├── pages/            # Page components
│   │   ├── api/              # API client (generated)
│   │   ├── hooks/            # Custom React hooks
│   │   ├── lib/              # Utility functions
│   │   └── contexts/         # React contexts
│   ├── public/               # Static assets
│   └── rsbuild.config.ts     # Build configuration
├── scripts/                  # Helper scripts
│   ├── release.sh            # Release automation
│   └── setup-hooks.sh        # Git hooks setup
├── .github/                  # GitHub Actions workflows
│   └── workflows/
│       └── release.yml       # Release workflow
├── Cargo.toml                # Workspace configuration
├── CLAUDE.md                 # AI assistant development guide
├── CHANGELOG.md              # Version history
├── RELEASING.md              # Release process
└── README.md                 # This file
```

### Code Quality

Temps enforces high code quality standards:

- ✅ **Warning-Free Compilation**: All new code must compile without warnings
- ✅ **Comprehensive Testing**: All new features must have passing tests
- ✅ **Conventional Commits**: Follow conventional commit format for auto-generated CHANGELOGs
- ✅ **Type Safety**: Use typed errors, no `unwrap()` in production code
- ✅ **Documentation**: OpenAPI docs for all HTTP endpoints

### Running Tests

```bash
# Unit tests (no external dependencies)
cargo test --lib --workspace

# Specific crate tests
cargo test --lib -p temps-backup

# Integration tests (requires Docker)
cargo test --workspace --features integration-tests

# Run with output
cargo test --lib -p temps-deployments -- --nocapture

# Run ignored tests (Docker-dependent)
cargo test --workspace -- --ignored
```

### Pre-Commit Hooks

Temps uses git hooks to ensure code quality:

```bash
# Setup hooks (one-time)
./scripts/setup-hooks.sh

# Run hooks manually
pre-commit run --all-files  # or: prek run --all-files
```

Hooks include:
- Conventional commit message validation
- Code formatting (`cargo fmt`)
- Linting (`cargo clippy`)
- CHANGELOG format validation
- YAML validation

---

## 🚢 Deployment

### Production Deployment

#### Using Binary

```bash
# Download and install (see Installation section)

# Create systemd service
sudo tee /etc/systemd/system/temps.service > /dev/null <<EOF
[Unit]
Description=Temps Platform
After=network.target postgresql.service

[Service]
Type=simple
User=temps
WorkingDirectory=/opt/temps
Environment="TEMPS_DATABASE_URL=postgresql://temps:password@localhost/temps"
Environment="TEMPS_ADDRESS=0.0.0.0:8080"
ExecStart=/usr/local/bin/temps serve
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF

# Enable and start service
sudo systemctl daemon-reload
sudo systemctl enable temps
sudo systemctl start temps

# Check status
sudo systemctl status temps
```

#### Using Docker (Coming Soon)

```bash
docker run -d \
  --name temps \
  -p 8080:8080 \
  -v temps-data:/root/.temps \
  -e TEMPS_DATABASE_URL=postgresql://user:pass@postgres:5432/temps \
  temps/temps:latest
```

#### Reverse Proxy Setup (nginx)

```nginx
server {
    listen 80;
    server_name temps.yourdomain.com;

    location / {
        proxy_pass http://localhost:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection 'upgrade';
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_cache_bypass $http_upgrade;
    }
}
```

### Environment-Specific Configuration

**Development:**
```bash
export TEMPS_LOG_LEVEL=debug
export TEMPS_ADDRESS=127.0.0.1:3000
```

**Production:**
```bash
export TEMPS_LOG_LEVEL=info
export TEMPS_ADDRESS=0.0.0.0:8080
export TEMPS_TLS_ADDRESS=0.0.0.0:8443
```

### Backup and Recovery

```bash
# Backups are managed through the web UI or API
# Data directory should be backed up regularly:
tar -czf temps-backup-$(date +%Y%m%d).tar.gz ~/.temps/

# Database backup
pg_dump temps > temps-db-$(date +%Y%m%d).sql
```

---

## 🤝 Contributing

We welcome contributions! Whether it's bug reports, feature requests, documentation improvements, or code contributions.

### How to Contribute

1. **Fork** the repository
2. **Clone** your fork: `git clone https://github.com/YOUR_USERNAME/temps.git`
3. **Create** a feature branch: `git checkout -b feature/amazing-feature`
4. **Make** your changes following our guidelines (see [CLAUDE.md](CLAUDE.md))
5. **Test** your changes: `cargo test --workspace`
6. **Format** code: `cargo fmt --all`
7. **Lint** code: `cargo clippy --workspace -- -D warnings`
8. **Commit** using conventional commits: `git commit -m "feat: add amazing feature"`
9. **Push** to your fork: `git push origin feature/amazing-feature`
10. **Open** a Pull Request

### Commit Message Format

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>

[optional body]

[optional footer]
```

**Types:**
- `feat:` - New feature
- `fix:` - Bug fix
- `docs:` - Documentation only
- `style:` - Code style (formatting)
- `refactor:` - Code refactoring
- `perf:` - Performance improvement
- `test:` - Adding tests
- `chore:` - Maintenance tasks

**Examples:**
```bash
feat(auth): add JWT token refresh
fix(api): handle null response from external service
docs: update installation instructions
chore(deps): update rust dependencies
```

### Development Guidelines

Detailed development guidelines are in [CLAUDE.md](CLAUDE.md), including:
- Architecture patterns
- Service layer design
- HTTP handler best practices
- Database query patterns
- Error handling
- Testing requirements
- Security considerations

### Code Review Process

1. All PRs require at least one approval
2. CI must pass (tests, linting, formatting)
3. Conventional commit format required
4. Code must compile without warnings
5. New features must include tests

---

## 📋 Roadmap

### v0.1.0 (Current)
- ✅ Core deployment functionality
- ✅ Analytics engine with funnels
- ✅ Error tracking (Sentry-compatible)
- ✅ Git provider integrations
- ✅ Reverse proxy with TLS
- ✅ Web UI with React

### v0.2.0 (Next)
- [ ] Docker Compose support
- [ ] Kubernetes deployment option
- [ ] Enhanced monitoring dashboards
- [ ] Slack/Discord integrations
- [ ] Custom domain management UI
- [ ] Multi-user workspace support

### Future
- [ ] GraphQL API
- [ ] Plugin system for extensions
- [ ] Mobile app for monitoring
- [ ] AI-powered error analysis
- [ ] Cost optimization recommendations
- [ ] Multi-cloud support (AWS, GCP, Azure)

---

## 🐛 Troubleshooting

### Common Issues

**Database Connection Error**
```bash
# If using Docker:
docker ps | grep temps-postgres  # Ensure container is running
docker logs temps-postgres       # Check for errors

# If using native PostgreSQL:
sudo systemctl status postgresql
psql postgresql://postgres:temps@localhost:15432/temps  # Test connection
```

**TimescaleDB Extension Not Found**
```bash
# This error means TimescaleDB is not installed!
# Error: "extension "timescaledb" is not available"

# Solution: Use Docker (easiest)
docker run -d --name temps-postgres \
  -e POSTGRES_PASSWORD=temps -e POSTGRES_DB=temps \
  -p 15432:5432 timescale/timescaledb-ha:pg16

# OR install TimescaleDB package for your PostgreSQL version
# See: https://docs.timescale.com/self-hosted/latest/install/

# Verify TimescaleDB is installed:
psql temps -c "SELECT * FROM pg_extension WHERE extname = 'timescaledb';"
# Should return one row if installed correctly
```

**Docker PostgreSQL Won't Start**
```bash
# Check if port 15432 is already in use
lsof -i :15432

# Check container logs
docker logs temps-postgres

# Remove and recreate container
docker rm -f temps-postgres
docker run -d --name temps-postgres \
  -e POSTGRES_PASSWORD=temps -e POSTGRES_DB=temps \
  -p 15432:5432 timescale/timescaledb-ha:pg16
```

**Web UI Not Loading**
```bash
# Rebuild with web UI
FORCE_WEB_BUILD=1 cargo build --release

# Check if dist directory exists
ls -la crates/temps-cli/dist/
```

**Permission Denied**
```bash
# Check data directory permissions
ls -la ~/.temps/

# Fix permissions
chmod 700 ~/.temps/
chmod 600 ~/.temps/encryption_key
chmod 600 ~/.temps/auth_secret
```

**Port Already in Use**
```bash
# Find process using port
lsof -i :8080

# Use different port
temps serve --address 0.0.0.0:9000
```

### Getting Help

- 📖 **Documentation**: Check [CLAUDE.md](CLAUDE.md) for detailed development guides
- 🐛 **Issues**: Report bugs at [GitHub Issues](https://github.com/gotempsh/temps/issues)
- 💬 **Discussions**: Ask questions in [GitHub Discussions](https://github.com/gotempsh/temps/discussions)
- 📧 **Email**: Contact maintainers at support@temps.dev

---

## 📄 License

Temps is dual-licensed under:

- **MIT License** ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
- **Apache License 2.0** ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)

You may choose either license for your use.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Temps by you shall be dual-licensed as above, without any additional terms or conditions.

---

## 🙏 Acknowledgments

Temps is built on the shoulders of giants. Special thanks to:

- **[Cloudflare Pingora](https://github.com/cloudflare/pingora)** - Production-grade reverse proxy framework
- **[Sea-ORM](https://www.sea-ql.org/SeaORM/)** - Robust async ORM for Rust
- **[Axum](https://github.com/tokio-rs/axum)** - Ergonomic web framework
- **[TimescaleDB](https://www.timescale.com/)** - Time-series database for PostgreSQL
- **[React](https://react.dev/)** - UI framework
- **[Rsbuild](https://rsbuild.dev/)** - Build tooling
- **[shadcn/ui](https://ui.shadcn.com/)** - Beautiful component library

And all the other amazing open-source projects we depend on!

---

## 🌟 Star History

If you find Temps useful, please consider giving it a star on GitHub! It helps others discover the project.

[![Star History Chart](https://api.star-history.com/svg?repos=gotempsh/temps&type=Date)](https://star-history.com/#gotempsh/temps&Date)

---

## 📞 Contact

- **Website**: https://temps.dev (coming soon)
- **GitHub**: https://github.com/gotempsh/temps
- **Twitter**: [@tempsdev](https://twitter.com/tempsdev) (coming soon)
- **Email**: hello@temps.dev

---

<div align="center">

**Built with ❤️ by the Temps community**

[Documentation](https://docs.temps.dev) • [GitHub](https://github.com/gotempsh/temps) • [Community](https://discord.gg/temps)

</div>
