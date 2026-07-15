# Temps Platform Setup & Management

Install, configure, and manage the Temps self-hosted deployment platform and CLI.

## What This Skill Covers

- ✅ Self-hosted Temps installation (install script, Docker, from source)
- ✅ CLI setup and authentication (`bunx @temps-sdk/cli`)
- ✅ Initial platform configuration (database, admin user, DNS/TLS)
- ✅ User and API key management
- ✅ Service provisioning (PostgreSQL, Redis, MongoDB, S3)
- ✅ Domain and TLS certificate management
- ✅ Monitoring and logging
- ✅ Backup and restore
- ✅ Troubleshooting common issues

## Quick Start

```bash
# 1. Install Temps — download, review, then run (don't pipe into a shell).
#    See SKILL.md "Method 1" for why and the full flow.
curl -fsSL https://temps.sh/deploy.sh -o deploy.sh
less deploy.sh        # review before running
bash deploy.sh

# 2. Start PostgreSQL
docker volume create temps-postgres
docker run -d --name temps-postgres \
  -v temps-postgres:/home/postgres/pgdata/data \
  -e POSTGRES_PASSWORD=temps \
  -e POSTGRES_DB=temps \
  -p 16432:5432 \
  timescale/timescaledb-ha:pg18

# 3. Setup platform
temps setup \
  --database-url "postgresql://postgres:temps@localhost:16432/temps" \
  --admin-email "admin@example.com"

# 4. Start server
temps serve \
  --database-url "postgresql://postgres:temps@localhost:16432/temps" \
  --address 0.0.0.0:80 \
  --console-address 0.0.0.0:8081
```

## CLI Usage

```bash
# Install CLI globally
npm install -g @temps-sdk/cli

# Or run without installing
bunx @temps-sdk/cli login

# Login
temps login

# Manage projects
temps projects list
temps projects create my-app

# Deploy
temps deploy
```

## When to Use This Skill

Use this skill when you need to:

- 🏗️ Install Temps on your server
- 🔧 Configure Temps for the first time
- 🔐 Set up authentication and users
- 🌐 Configure DNS providers and TLS certificates
- 📦 Provision database and cache services
- 🚀 Get started with the Temps CLI
- 🔍 Troubleshoot platform issues

## Related Skills

- [deploy-to-temps](../deploy-to-temps/) - Deploy applications to Temps
- [add-custom-domain](../add-custom-domain/) - Custom domain configuration

## Full Documentation

See [SKILL.md](SKILL.md) for complete installation and management guide.
