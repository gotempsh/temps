# Temps CLI - Complete Reference

Comprehensive command-line reference for the Temps deployment platform CLI.

## What This Skill Covers

Complete documentation for all **440+ CLI commands across 69 command groups**
(catalog generated from `@temps-sdk/cli` v0.1.26; pinned runtime v0.1.28)
including:

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

Use the installed `temps` binary for every example. Do not execute the CLI
through an on-demand package runner. Install or upgrade only with explicit user
approval, and pin the reviewed version:

```bash
# Verify the reviewed registry artifact
expected_temps_cli_integrity='sha512-ZYqScqes66gQ+fVKuUtDmV9PTxjF7M8XpCbhDCm4e5m3Hul9F5oVx6HM6MXI3YjaCL4pwXfxlNnBA0cNc538Wg=='
actual_temps_cli_integrity="$(npm view @temps-sdk/cli@0.1.28 dist.integrity)"
test "$actual_temps_cli_integrity" = "$expected_temps_cli_integrity" || {
  echo "Refusing to install: @temps-sdk/cli@0.1.28 integrity mismatch" >&2
  exit 1
}

# Disable dependency lifecycle scripts during installation
npm install --global --ignore-scripts @temps-sdk/cli@0.1.28

command -v temps
temps --version
```

Before a state-changing command, insert `--target-context <name>` immediately
after `temps`. Never place real credentials in agent-generated commands or
repeat credential-reveal output in chat; use interactive prompts or
environment variables injected by the user's secret manager.

## Quick Start

```bash
# Login to Temps
temps login

# Create a project
temps projects create my-app

# Deploy from Git
temps deploy my-app -b main -e production

# Set environment variables
temps environments vars set DATABASE_URL "postgresql://..." -p my-app -e production

# View deployment logs
temps deployments logs -p my-app -f
```

## Common Commands

```bash
# Projects
temps projects list
temps projects create
temps projects show -p my-app

# Deployments
temps deploy my-app -b main -e production
temps deployments list -p my-app
temps deployments rollback -p my-app

# Services
temps services create -t postgres -n mydb
temps services list
temps services link --id 1 --project-id 5

# Domains
temps domains add -p my-app -d example.com
temps domains verify -p my-app -d example.com

# Logs
temps deployments logs -p my-app -f
temps runtime-logs -p my-app -e staging -f

# Monitoring
temps monitors create --project-id 5 -n "API Health" -t http
temps incidents list --project-id 5
```

## Configuration

Credentials are managed automatically by the CLI. Never direct an agent to
discover, read, or edit the underlying files.

```bash
# Interactive AWS-style wizard
temps configure

# Non-interactive (TEMPS_TOKEN is injected separately by CI)
temps configure --api-url "$TEMPS_API_URL" --output-format json --no-interactive

# View configuration / inspect or change individual values
temps configure show
temps configure get output-format
temps configure set output-format json
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
temps projects show -p my-app --json | jq '.id'

# List running services
temps services list --json | jq '.[] | select(.status == "running")'
```

## CI/CD Automation

Use `-y/--yes` to skip prompts:

```bash
# TEMPS_TOKEN is injected by the CI secret store and must never be echoed.
export TEMPS_API_URL=https://temps.example.com
test -n "${TEMPS_TOKEN:-}" || { echo "TEMPS_TOKEN is not configured" >&2; exit 1; }

temps --target-context production deploy my-app -b main -e production -y
temps --target-context production environments vars set VERSION "1.2.3" -p my-app -e production
temps --target-context production scans trigger --project-id 5 --environment-id 1
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
**Generated reference**: 0.1.26

**Required runtime**: 0.1.28
