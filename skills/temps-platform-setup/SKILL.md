---
name: temps-platform-setup
description: |
  Install, configure, and manage the Temps deployment platform and CLI. Covers self-hosted Temps installation, CLI setup (bunx @temps-sdk/cli), initial configuration, user management, and platform administration. Use when the user wants to: (1) Install Temps on their server, (2) Set up the Temps CLI, (3) Configure Temps for the first time, (4) Manage Temps platform settings, (5) Create admin users, (6) Configure DNS providers, (7) Set up TLS certificates. Triggers: "install temps", "setup temps", "temps cli", "configure temps", "temps platform", "self-hosted deployment platform".
---

# Temps Platform Setup & Management

Complete guide for installing and managing the Temps self-hosted deployment platform.


## Overview

**Temps** is a self-hosted deployment platform with built-in analytics, monitoring, error tracking (Sentry-compatible), and automatic TLS via Let's Encrypt. Deploys any Git-hosted application — frontend (React, Next.js, Vue, Svelte, Angular), backend (Node.js, Python, Go, Rust, Ruby, PHP), static sites (Hugo, Jekyll, Gatsby), or custom Dockerfiles — with zero configuration.

---

## Installation Methods

### Method 1: Install Script (Recommended)

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
source ~/.zshrc  # or ~/.bashrc
```

**Validate:** `temps --version` should print the installed version.

### Method 2: Docker Compose (Production)

For production deployments with PostgreSQL and Redis:

```bash
# Clone the repository
git clone https://github.com/gotempsh/temps.git
cd temps

# Start with Docker Compose
docker-compose up -d
```

**Validate:** `docker-compose ps` should show Temps, PostgreSQL 18 + TimescaleDB, and Redis all healthy. API: http://localhost:3000 | Console: http://localhost:8081

### Method 3: From Source (Development)

```bash
# Prerequisites: Rust 1.70+, PostgreSQL, Bun
git clone https://github.com/gotempsh/temps.git
cd temps

# Build Rust backend
cargo build --release --bin temps

# Build web console (optional)
cd web
bun install
RSBUILD_OUTPUT_PATH=../crates/temps-cli/dist bun run build
cd ..

# Run migrations and start
./target/release/temps serve \
  --database-url "postgresql://user:pass@localhost:5432/temps"
```

---

## Quick Start

### 1. Start PostgreSQL Database

Temps requires **PostgreSQL 14+ with TimescaleDB extension**.

**Using Docker (easiest):**

```bash
# Create persistent volume
docker volume create temps-postgres

# Start PostgreSQL + TimescaleDB
docker run -d \
  --name temps-postgres \
  -v temps-postgres:/var/lib/postgresql/data \
  -e POSTGRES_USER=postgres \
  -e POSTGRES_PASSWORD=temps \
  -e POSTGRES_DB=temps \
  -p 16432:5432 \
  timescale/timescaledb:latest-pg18
```

**Connection string:**
```
postgresql://postgres:temps@localhost:16432/temps
```

### 2. Run Temps Setup

The setup command initializes the database, creates admin user, and configures DNS/TLS:

```bash
temps setup \
  --database-url "postgresql://postgres:temps@localhost:16432/temps" \
  --admin-email "your-email@example.com" \
  --wildcard-domain "*.yourdomain.com" \
  --github-token "ghp_xxxxxxxxxxxx" \
  --dns-provider "cloudflare" \
  --cloudflare-token "your-cloudflare-api-token"
```

**Setup options:**

| Option | Description | Required |
|--------|-------------|----------|
| `--database-url` | PostgreSQL connection string | ✅ Yes |
| `--admin-email` | Admin user email | ✅ Yes |
| `--wildcard-domain` | Domain for deployments (e.g., `*.temps.sh`) | Optional |
| `--github-token` | GitHub personal access token | Optional |
| `--dns-provider` | DNS provider (`cloudflare`, `route53`, `digitalocean`) | Optional |
| `--cloudflare-token` | Cloudflare API token | If using Cloudflare |
| `--route53-access-key` | AWS access key | If using Route53 |
| `--route53-secret-key` | AWS secret key | If using Route53 |

**Validate:** Setup prints an admin API token — save it immediately (not shown again).

### 3. Start Temps Server

```bash
temps serve \
  --database-url "postgresql://postgres:temps@localhost:16432/temps" \
  --address 0.0.0.0:80 \
  --tls-address 0.0.0.0:443 \
  --console-address 0.0.0.0:8081
```

**Server options:**

| Option | Description | Default | Environment Variable |
|--------|-------------|---------|---------------------|
| `--address` | HTTP API address | `127.0.0.1:3000` | `TEMPS_ADDRESS` |
| `--tls-address` | HTTPS address (proxy) | - | `TEMPS_TLS_ADDRESS` |
| `--console-address` | Admin console address | - | `TEMPS_CONSOLE_ADDRESS` |
| `--database-url` | PostgreSQL URL | - | `TEMPS_DATABASE_URL` |
| `--data-dir` | Data directory | `~/.temps` | `TEMPS_DATA_DIR` |

**Validate:** `curl -s http://localhost:3000/health` returns OK. Access console at http://localhost:8081 (or https://temps.yourdomain.com). Log in with the email from setup and the saved API token.

---

## CLI Setup

### Installation

```bash
# Without installing (recommended for CI/CD)
bunx @temps-sdk/cli --version   # or: npx @temps-sdk/cli --version

# Global install
bun add -g @temps-sdk/cli       # or: npm install -g @temps-sdk/cli
```

**Validate:** `temps --version` prints the CLI version.

### Authentication

```bash
# Interactive
temps login  # prompts for API URL and token

# Non-interactive (CI/CD)
temps login --api-key tk_abc123def456 -u https://temps.yourdomain.com

# Environment variables (override config)
export TEMPS_API_URL="https://temps.yourdomain.com"
export TEMPS_TOKEN="tk_abc123def456"
```

**Validate:** `temps whoami` should print your email, role, and API URL.

### Configuration

Config stored in `~/.temps/config.json`; credentials in `~/.temps/.secrets` (mode 0600).

```bash
temps configure show              # view current config
temps configure set apiUrl URL    # set API URL
temps configure set outputFormat table  # table | json | minimal
temps configure reset             # reset to defaults
```

**Environment variable overrides:** `TEMPS_API_URL`, `TEMPS_TOKEN` (highest priority), `TEMPS_API_TOKEN`, `TEMPS_API_KEY`, `NO_COLOR`.

---

## Initial Configuration

### Create Your First Project

```bash
temps projects create my-app   # or omit name for interactive prompts
```

**Validate:** `temps projects list` shows the new project.

### Connect Git Provider

```bash
# GitHub (token needs repo + read:org scopes)
temps git-providers add github --name "My GitHub" --token "ghp_xxxxxxxxxxxx"

# GitLab
temps git-providers add gitlab --name "My GitLab" --token "glpat-xxxxxxxxxxxx" --url "https://gitlab.com"
```

**Validate:** `temps git-providers list` shows the connected provider.

### Create Environment

```bash
temps environments create production

# With resource limits
temps environments create staging \
  --cpu 0.5 --memory 512Mi --replicas-min 1 --replicas-max 3
```

**Validate:** `temps environments list` shows the new environment.

### Set Environment Variables

```bash
# Set a variable
temps env set DATABASE_URL="postgresql://..." \
  --environment production \
  --project my-app

# Set from .env file
temps env import .env \
  --environment production \
  --project my-app

# List variables
temps env list \
  --environment production \
  --project my-app
```

All environment variables are encrypted at rest; secrets are masked in the UI.

---

## Platform Management

### User Management

```bash
temps users create --email "developer@example.com" --role admin
```

Roles: **Admin** (full access), **User** (projects + deployments), **Viewer** (read-only). Also available via console: Settings → Users → Create User.

**Validate:** `temps users list` shows the new user.

### API Keys & Tokens

```bash
temps tokens create --name "CI/CD Token" --expires-in 90d
temps api-keys create --name "Production API Key" --permissions deployments.read,deployments.create
```

**Validate:** `temps tokens list` shows the new token.

### Service Provisioning

```bash
temps services create postgres --name my-database --version 16 --storage 10Gi
temps services create redis --name my-cache --version 7
temps services create s3 --name my-storage --storage 20Gi
```

Auto-injected env vars: `DATABASE_URL` (PostgreSQL), `REDIS_URL` (Redis), `S3_ENDPOINT`/`S3_ACCESS_KEY`/`S3_SECRET_KEY`/`S3_BUCKET` (S3).

**Validate:** `temps services list` shows provisioned services and their status.

### Monitoring & Logs

```bash
temps logs --deployment-id 123 --follow       # stream deployment logs
temps logs --deployment-id 123 --tail 100     # last 100 lines
temps containers logs container-abc123 --follow
temps deployments list --project my-app
temps deployments show 123
```

### Backups

```bash
temps backups create --service postgres-123 --schedule "0 2 * * *"  # scheduled
temps backups run --service postgres-123                             # manual
temps backups list --service postgres-123
temps backups restore backup-456 --target postgres-123
```

---

## DNS & TLS Setup

### DNS Providers

```bash
temps dns-providers add cloudflare --token "your-token" --zone-id "your-zone-id"
temps dns-providers add route53 --access-key-id "AKIA..." --secret-access-key "..." --region "us-east-1"
temps dns-providers add digitalocean --token "dop_v1_xxxxxxxxxxxx"
```

**Validate:** `temps dns-providers list` shows the configured provider.

### Custom Domains

**Add custom domain to project:**

```bash
temps domains add example.com \
  --project my-app \
  --environment production
```

**Add wildcard domain:**

```bash
temps domains add "*.example.com" \
  --project my-app \
  --environment production
```

**Verify DNS challenge (for TLS certificate):**

```bash
temps domains verify example.com
```

Temps automatically creates DNS records, requests a Let's Encrypt certificate via DNS-01 challenge, and auto-renews 30 days before expiration.

**Validate:** `temps domains list --project my-app` shows domain status and certificate state.

### TLS Certificates

```bash
temps certificates list
temps certificates show cert-123
temps certificates renew cert-123        # force renewal
```

**Manual DNS challenge** (if auto DNS fails):

```bash
temps domains add example.com --project my-app
temps certificates challenge cert-123    # get DNS challenge records
# Add TXT records to your DNS provider, then:
temps certificates complete cert-123
```

**Self-hosted behind NAT/firewall with `*.temps.dev` subdomain:**

If your Temps instance is behind NAT or a firewall and cannot receive HTTP-01 challenges on port 80, use `acme.sh` with `@temps-sdk/cli` cloud ACME commands for DNS-01 validation. This lets you provision TLS certificates for your `*.temps.dev` subdomain without exposing port 80. The flow uses `temps cloud acme` (from `@temps-sdk/cli`) to manage DNS records and `temps domain import` (server-side Rust binary) to load the certificate into Temps.

See the **Cloud ACME Certificates (acme.sh)** section in the [Temps CLI reference](../temps-cli/SKILL.md) for the complete setup guide, including the DNS hook script and step-by-step certificate flow.

---

## Troubleshooting

### Database Connection Issues

**Error:** `Failed to connect to database`

**Solution:**
```bash
# Verify PostgreSQL is running
docker ps | grep postgres

# Test connection
psql "postgresql://postgres:temps@localhost:16432/temps" -c "SELECT version();"

# Check database URL format
temps serve --database-url "postgresql://user:password@host:port/database"
```

### Port Already in Use

**Error:** `Address already in use (os error 48)`

**Solution:**
```bash
# Find process using port 3000
lsof -i :3000

# Kill process
kill -9 <PID>

# Or use different port
temps serve --address 0.0.0.0:3001
```

### TLS Certificate Issues

**Error:** `Failed to obtain TLS certificate`

**Solutions:**

1. **Check DNS propagation:**
```bash
# Verify DNS records exist
dig example.com
dig _acme-challenge.example.com TXT
```

2. **Verify DNS provider credentials:**
```bash
temps dns-providers list
```

3. **Check rate limits:**
   - Let's Encrypt: 50 certs per registered domain per week
   - Use staging environment for testing: `--acme-staging`

4. **Manual DNS challenge:**
```bash
# Get challenge record
temps certificates challenge cert-123

# Add TXT record manually
# _acme-challenge.example.com TXT "challenge-value"

# Complete after DNS propagation (60s+)
temps certificates complete cert-123
```

### Deployment Failures

**Error:** `Build failed`

**Debug steps:**

1. **Check build logs:**
```bash
temps logs --deployment-id 123
```

2. **Verify build command:**
```bash
# Test locally
npm run build  # or your build command
```

3. **Check environment variables:**
```bash
temps env list --project my-app --environment production
```

4. **Test Docker build locally:**
```bash
docker build -t test-image .
docker run -p 3000:3000 test-image
```

### Service Connection Issues

**Error:** `Service postgres-123 not reachable`

**Solution:**
```bash
# Check service status
temps services show postgres-123

# Verify service is running
temps containers list | grep postgres-123

# Check service logs
temps containers logs <container-id>

# Restart service
temps services restart postgres-123
```

### CLI Authentication Issues

**Error:** `Unauthorized (401)`

**Solution:**
```bash
# Verify token is valid
temps whoami

# Re-login
temps logout
temps login

# Or use environment variable
export TEMPS_TOKEN="tk_your_token_here"
temps whoami
```

### MaxMind GeoLite2 Database Missing

**Error:** `GeoLite2-City.mmdb not found`

**Solution:**

The analytics feature requires MaxMind GeoLite2 database for IP geolocation.

1. **Download GeoLite2-City database:**
   - Sign up at https://www.maxmind.com/en/geolite2/signup
   - Download GeoLite2-City database (GZIP format)

2. **Extract and place:**
```bash
# Extract
tar xzf GeoLite2-City_*.tar.gz

# Copy to Temps data directory
cp GeoLite2-City_*/GeoLite2-City.mmdb ~/.temps/

# Or specify custom path
temps serve --data-dir /path/to/data
```

3. **Verify:**
```bash
ls -lh ~/.temps/GeoLite2-City.mmdb
```

**Note:** Temps works without this database, but geolocation features will be disabled.

---

## Quick Reference

### Common Commands

```bash
# Platform
temps setup --database-url "postgres://..." --admin-email "admin@example.com"
temps serve --database-url "postgres://..." --address 0.0.0.0:80

# CLI
temps login
temps projects list
temps deployments list

# Projects
temps projects create my-app
temps env set KEY=value --project my-app --environment production

# Services
temps services create postgres --name mydb --version 16
temps services list

# Domains
temps domains add example.com --project my-app
temps domains verify example.com

# Monitoring
temps logs --deployment-id 123 --follow
temps deployments show 123
```

### Configuration Files

| File | Purpose | Location |
|------|---------|----------|
| `config.json` | CLI configuration | `~/.temps/config.json` |
| `.secrets` | API tokens | `~/.temps/.secrets` |
| `encryption_key` | Encryption key | `~/.temps/encryption_key` |
| `GeoLite2-City.mmdb` | Geolocation database | `~/.temps/GeoLite2-City.mmdb` |

### Environment Variables

| Variable | Purpose | Example |
|----------|---------|---------|
| `TEMPS_DATABASE_URL` | PostgreSQL connection | `postgresql://user:pass@localhost:5432/temps` |
| `TEMPS_ADDRESS` | HTTP API address | `0.0.0.0:3000` |
| `TEMPS_TLS_ADDRESS` | HTTPS proxy address | `0.0.0.0:443` |
| `TEMPS_CONSOLE_ADDRESS` | Admin console address | `0.0.0.0:8081` |
| `TEMPS_DATA_DIR` | Data directory | `~/.temps` |
| `TEMPS_TOKEN` | CLI API token | `tk_abc123def456` |
| `TEMPS_API_URL` | CLI API endpoint | `https://temps.example.com` |

### Ports

| Port | Service | Purpose |
|------|---------|---------|
| `3000` | API (default) | HTTP API endpoint |
| `80` | HTTP | HTTP traffic (recommended) |
| `443` | HTTPS | TLS-encrypted traffic |
| `8081` | Console | Admin web console |
| `5432` | PostgreSQL | Database (if using Docker) |
| `6379` | Redis | Cache (if using Docker) |

---

## Next Steps

After installing Temps:

1. **Deploy your first app**: See [deploy-to-temps skill](../deploy-to-temps/SKILL.md)
2. **Add analytics**: See [add-react-analytics skill](../add-react-analytics/SKILL.md)
3. **Set up custom domain**: See [add-custom-domain skill](../add-custom-domain/SKILL.md)
4. **Configure MCP**: See [temps-mcp-setup skill](../temps-mcp-setup/SKILL.md)

**Documentation:**
- CLI Reference: [apps/temps-cli/SKILL.md](../../apps/temps-cli/SKILL.md)
- Project Documentation: https://temps.sh/docs
- GitHub: https://github.com/gotempsh/temps

---

**License:** Dual-licensed under MIT or Apache 2.0
