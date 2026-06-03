# Temps CLI - Complete Reference

Comprehensive command-line reference for the Temps deployment platform CLI.

## What This Skill Covers

Complete documentation for all **440+ CLI commands across 69 command groups** (matches `@temps-sdk/cli` v0.1.26) including:

- ✅ Authentication (login, logout, whoami)
- ✅ Projects (create, update, delete, settings)
- ✅ Deployments (Git, static, Docker images, local images)
- ✅ Environments (create, scale, variables, cron jobs)
- ✅ Services (PostgreSQL, Redis, MongoDB, S3)
- ✅ Git Providers (GitHub, GitLab, Bitbucket)
- ✅ Domains & TLS Certificates
- ✅ Custom Domains (environment targeting, redirects)
- ✅ DNS Management (records, zones, providers)
- ✅ Notifications (Slack, email, webhooks)
- ✅ Monitoring (uptime, incidents, health checks)
- ✅ Containers (lifecycle, metrics, logs)
- ✅ Backups (sources, schedules, restore)
- ✅ Security Scanning (vulnerability detection)
- ✅ Error Tracking (Sentry-compatible)
- ✅ Webhooks (delivery management)
- ✅ API Keys & Tokens
- ✅ Users Management
- ✅ Email (providers, domains, sending)
- ✅ IP Access Control
- ✅ Load Balancer
- ✅ Audit Logs
- ✅ Proxy Logs
- ✅ Platform Information
- ✅ Settings & Configuration
- ✅ Presets & Templates
- ✅ Imports (Docker containers)
- ✅ Temps Cloud (VPS management)

## Installation

```bash
# Run directly without installing (recommended)
npx @temps-sdk/cli --version
bunx @temps-sdk/cli --version

# Or install globally
npm install -g @temps-sdk/cli
bun add -g @temps-sdk/cli
```

## Quick Start

```bash
# Login to Temps
bunx @temps-sdk/cli login

# Create a project
bunx @temps-sdk/cli projects create my-app

# Deploy from Git
bunx @temps-sdk/cli deploy my-app -b main -e production

# Set environment variables
bunx @temps-sdk/cli environments vars set DATABASE_URL "postgresql://..." -p my-app -e production

# View deployment logs
bunx @temps-sdk/cli deployments logs -p my-app -f
```

## Common Commands

```bash
# Projects
bunx @temps-sdk/cli projects list
bunx @temps-sdk/cli projects create
bunx @temps-sdk/cli projects show -p my-app

# Deployments
bunx @temps-sdk/cli deploy my-app -b main -e production
bunx @temps-sdk/cli deployments list -p my-app
bunx @temps-sdk/cli deployments rollback -p my-app

# Services
bunx @temps-sdk/cli services create -t postgres -n mydb
bunx @temps-sdk/cli services list
bunx @temps-sdk/cli services link --id 1 --project-id 5

# Domains
bunx @temps-sdk/cli domains add -p my-app -d example.com
bunx @temps-sdk/cli domains verify -p my-app -d example.com

# Logs
bunx @temps-sdk/cli deployments logs -p my-app -f
bunx @temps-sdk/cli runtime-logs -p my-app -e staging -f

# Monitoring
bunx @temps-sdk/cli monitors create --project-id 5 -n "API Health" -t http
bunx @temps-sdk/cli incidents list --project-id 5
```

## Configuration

**Config file**: `~/.temps/config.json`
**Credentials**: Stored securely in `~/.temps/` with restricted file permissions (mode 0600)

```bash
# Interactive AWS-style wizard
bunx @temps-sdk/cli configure

# Non-interactive (e.g. CI)
bunx @temps-sdk/cli configure --api-url https://temps.example.com --api-token <TOKEN> --output-format json --no-interactive

# View configuration / inspect or change individual values
bunx @temps-sdk/cli configure show
bunx @temps-sdk/cli configure get output-format
bunx @temps-sdk/cli configure set output-format json
```

## Environment Variables

| Variable | Description |
|----------|-------------|
| `TEMPS_API_URL` | Override API endpoint |
| `TEMPS_TOKEN` | API token (highest priority) |
| `TEMPS_API_TOKEN` | API token (CI/CD) |
| `TEMPS_API_KEY` | API key |
| `NO_COLOR` | Disable colored output |

## Command Aliases

Common shortcuts (verbatim from the CLI):
- `temps p` → `temps projects`
- `temps svc` → `temps services`
- `temps cts` → `temps containers`
- `temps hooks` → `temps webhooks`
- `temps plogs` → `temps proxy-logs`
- `temps rlogs` → `temps runtime-logs`
- `temps stats` → `temps analytics`
- `temps deploys` → `temps deployments`

See the **Command Aliases** table in [SKILL.md](SKILL.md) for the full list.

## JSON Output

All commands support `--json` for scripting:

```bash
# Get project ID
bunx @temps-sdk/cli projects show -p my-app --json | jq '.id'

# List running services
bunx @temps-sdk/cli services list --json | jq '.[] | select(.status == "running")'
```

## CI/CD Automation

Use `-y/--yes` to skip prompts:

```bash
export TEMPS_TOKEN=$TEMPS_TOKEN
export TEMPS_API_URL=https://temps.example.com

bunx @temps-sdk/cli deploy my-app -b main -e production -y
bunx @temps-sdk/cli environments vars set VERSION "1.2.3" -p my-app -e production
bunx @temps-sdk/cli scans trigger --project-id 5 --environment-id 1
```

## When to Use This Skill

Use this skill when you need:

- 📖 Complete CLI command reference
- 🔍 Find specific command syntax
- 🚀 Learn deployment workflows
- 🔧 Manage services and infrastructure
- 📊 Set up monitoring and logging
- 🔐 Configure security and access control
- 🌐 Manage domains and DNS
- 📧 Configure email and notifications

## Related Skills

- [temps-platform-setup](../temps-platform-setup/) - Install and configure Temps platform
- [deploy-to-temps](../deploy-to-temps/) - Deploy applications to Temps
- [add-custom-domain](../add-custom-domain/) - Custom domain configuration

## Full Documentation

See [SKILL.md](SKILL.md) for the complete command reference with examples (6000+ lines, all 69 command groups).

---

**Package**: [@temps-sdk/cli](https://www.npmjs.com/package/@temps-sdk/cli)
**Version**: 0.1.26
