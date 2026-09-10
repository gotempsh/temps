<p align="center">
  <img src="https://raw.githubusercontent.com/gotempsh/temps/refs/heads/main/web/public/logo/temps-logo-light.png" alt="Temps Platform" width="700" />
</p>

<h1 align="center">@temps-sdk/cli</h1>

<p align="center">
  <a href="https://www.npmjs.com/package/@temps-sdk/cli"><img src="https://img.shields.io/npm/v/@temps-sdk/cli.svg" alt="npm version" /></a>
  <a href="https://www.npmjs.com/package/@temps-sdk/cli"><img src="https://img.shields.io/npm/dm/@temps-sdk/cli.svg" alt="npm downloads" /></a>
  <a href="https://github.com/AnomalyCo/temps/blob/main/LICENSE"><img src="https://img.shields.io/npm/l/@temps-sdk/cli.svg" alt="license" /></a>
</p>

<p align="center">
  Command-line interface for the Temps deployment platform. Deploy, manage, and monitor your applications from the terminal -- no dashboard required.
</p>

---

```bash
# npm
npm install -g @temps-sdk/cli

# bun
bun add -g @temps-sdk/cli

# pnpm
pnpm add -g @temps-sdk/cli

# Or run without installing
npx @temps-sdk/cli
bunx @temps-sdk/cli
```

## Quick Start

```bash
# Authenticate with your Temps instance
bunx @temps-sdk/cli login

# Initialize a project in the current directory
bunx @temps-sdk/cli init

# Deploy
bunx @temps-sdk/cli up

# Check status
bunx @temps-sdk/cli status
```

That's it. The CLI detects your framework, connects your git repo, and deploys -- all in one command.

## Configuration

### Interactive Setup

```bash
bunx @temps-sdk/cli configure
```

Walks you through setting your Temps API URL and authentication token, stored in `~/.temps/config.json`.

### Environment Variables

```bash
TEMPS_API_URL=https://your-instance.temps.dev   # Your Temps API URL
TEMPS_API_TOKEN=your-token                       # API key or deployment token
TEMPS_PROJECT=my-app                             # Project slug (optional)
```

Environment variables take precedence over config files, making CI/CD integration straightforward.

### Project-Level Config

Running `bunx @temps-sdk/cli init` or `bunx @temps-sdk/cli link` creates a `.temps/config.json` in your project directory:

```json
{
  "projectSlug": "my-app"
}
```

The CLI walks upward from your working directory to find `.temps/config.json` (like `.git` discovery). When found, the `projectSlug` is used to auto-fetch the project ID and all configuration (git connection, preset, environments) from the API -- no need to pass `--project` on every command.

**Resolution order:** `--project` flag > `.temps/config.json` > `TEMPS_PROJECT` env var > global default.

## Developer Workflow

### `init`

Initialize a new Temps project in the current directory. Detects your framework, creates a project on the platform, and links it.

```bash
bunx @temps-sdk/cli init
```

### `link <project>`

Link the current directory to an existing Temps project.

```bash
bunx @temps-sdk/cli link my-app
```

### `up`

One-command deploy. If the project is not yet linked, an interactive setup wizard walks you through framework detection, git connection, and service provisioning. If the project is already linked (via `link` or `init`), it fetches the project configuration -- including the git connection and preset -- shows a deployment preview, and triggers the pipeline with a live progress TUI.

```bash
# Deploy the current directory
bunx @temps-sdk/cli up

# Deploy a specific branch
bunx @temps-sdk/cli up --branch main

# Deploy with a specific preset (skip auto-detection)
bunx @temps-sdk/cli up --preset nextjs

# Manual deployment mode (no git, uploads a local Docker image)
bunx @temps-sdk/cli up --manual
```

**What `up` shows for a linked project:**

```
i Using project acme-api (from local-config)
✔ Found project: acme-api
i Repository: acme-org/acme-api
i Preset: fastapi

╭─ Deployment Preview ──────────────╮
│ Project:     acme-api              │
│ Environment: production            │
│ Branch:      main                  │
│ Preset:      fastapi               │
│ Repository:  acme-org/acme-api     │
╰────────────────────────────────────╯

✔ Deployment started
  🚀 Deployment Progress
  ...
```

If the project has no git provider connected, `up` warns you and suggests how to connect one or fall back to manual deployment.

### `status`

View the current project's deployment status, container health, and domain configuration.

```bash
bunx @temps-sdk/cli status
```

### `open`

Open the project's live URL in your default browser.

```bash
bunx @temps-sdk/cli open
```

### `rollback`

Rollback to the previous deployment.

```bash
bunx @temps-sdk/cli rollback
```

### `env:pull` / `env:push`

Sync environment variables between your local `.env` file and the Temps project.

```bash
# Download env vars to .env
bunx @temps-sdk/cli env:pull

# Upload .env to the project
bunx @temps-sdk/cli env:push
```

## Deployment Methods

### Git-Based Deploy

```bash
# Deploy from a branch (default: current branch)
bunx @temps-sdk/cli deploy

# Deploy a specific branch
bunx @temps-sdk/cli deploy --branch feature/new-ui

# Deploy to a specific environment
bunx @temps-sdk/cli deploy --branch main --environment production
```

### Local Docker Image

Build a Docker image locally, export it, and upload it directly -- useful when your CI builds images or for air-gapped environments.

```bash
bunx @temps-sdk/cli deploy:local-image --tag my-app:latest
```

### List Deployments

```bash
bunx @temps-sdk/cli deployments
```

## Multi-Instance Management

Manage multiple Temps server instances (self-hosted and cloud) from a single CLI.

```bash
# List configured instances
bunx @temps-sdk/cli instances list

# Add a new instance
bunx @temps-sdk/cli instances add

# Switch active instance
bunx @temps-sdk/cli instances switch
```

## Temps Cloud

Connect to Temps Cloud for managed hosting with automatic provisioning.

```bash
# Login via browser (device code flow)
bunx @temps-sdk/cli cloud login

# Check current user
bunx @temps-sdk/cli cloud whoami

# Manage VPS instances
bunx @temps-sdk/cli cloud vps list
bunx @temps-sdk/cli cloud vps create
bunx @temps-sdk/cli cloud vps destroy

# View billing and usage
bunx @temps-sdk/cli cloud billing
```

## Platform Migration

Migrate projects from other platforms with an interactive wizard that discovers your projects, snapshots configuration, and generates a step-by-step migration plan.

```bash
bunx @temps-sdk/cli migrate
```

**Supported platforms:**

| Platform | What's migrated |
|----------|-----------------|
| Vercel | Projects, env vars, domains |
| Coolify | Projects, services, env vars, domains |
| Dokploy | Projects, services, env vars, domains |

## Resource Management

The CLI provides full CRUD access to every Temps resource:

```bash
# Projects
bunx @temps-sdk/cli projects list
bunx @temps-sdk/cli projects create
bunx @temps-sdk/cli projects show <slug>

# Domains & SSL
bunx @temps-sdk/cli domains list
bunx @temps-sdk/cli domains provision <id>

# Services (PostgreSQL, Redis, S3)
bunx @temps-sdk/cli services list --project <slug>
bunx @temps-sdk/cli services create --project <slug>

# Monitoring
bunx @temps-sdk/cli monitors list
bunx @temps-sdk/cli monitors create

# Environment variables
bunx @temps-sdk/cli environments list --project <slug>

# Git providers
bunx @temps-sdk/cli providers list
bunx @temps-sdk/cli providers sync <id>

# Backups
bunx @temps-sdk/cli backups list --project <slug>
bunx @temps-sdk/cli backups run <id>

# Container management
bunx @temps-sdk/cli containers list --project <slug>

# Runtime logs (live streaming)
bunx @temps-sdk/cli runtime-logs --project <slug>
```

**Full resource list:** projects, deployments, environments, domains, custom-domains, DNS, DNS providers, git providers, services, backups, containers, monitors, incidents, webhooks, API keys, tokens, users, settings, audit logs, proxy logs, errors, DSN, KV, blob, scans, IP access, email domains, email providers, emails, load balancer, templates, presets, funnels, notifications, notification preferences, platform, data.

## Browsing service data

Read-only access to the data *inside* a service — tables, collections, keys and
objects across PostgreSQL, MySQL/MariaDB, MongoDB, Redis and S3. Useful for
answering questions about your application's real data from a script or a
coding agent, without opening the console.

```bash
# What does this service support, and how do its containers nest?
bunx @temps-sdk/cli data info my-db

# Databases (or buckets), then tables (or collections/keys/objects)
bunx @temps-sdk/cli data containers my-db
bunx @temps-sdk/cli data tables my-db --path mydb/public

# Columns and types, then the rows themselves
bunx @temps-sdk/cli data schema my-db users --path mydb/public
bunx @temps-sdk/cli data rows my-db users --path mydb/public --limit 20

# Filter using the backend's own syntax (see `data info` for the schema)
bunx @temps-sdk/cli data rows my-db users --path mydb/public \
  --filter '{"where":"plan = '"'"'pro'"'"'"}' --json
```

Each command prints the next one with a real path filled in. `--path` is
slash-separated and mirrors the engine's hierarchy — `mydb/public` for
PostgreSQL (database/schema), just `mydb` for MySQL and MongoDB, the database
number for Redis, the bucket name for S3.

Add `--json` for untruncated, machine-readable output.

## CI/CD Integration

Use environment variables for non-interactive deployments:

```bash
# GitHub Actions example
env:
  TEMPS_API_URL: ${{ secrets.TEMPS_API_URL }}
  TEMPS_API_TOKEN: ${{ secrets.TEMPS_API_TOKEN }}

steps:
  - run: bunx @temps-sdk/cli deploy --branch ${{ github.ref_name }} --project my-app
```

## Global Options

| Option | Description |
|--------|-------------|
| `-v, --version` | Display version number |
| `--no-color` | Disable colored output |
| `--debug` | Enable debug output |
| `-h, --help` | Display help |

## Requirements

- Node.js 18+ or Bun
- A running Temps instance (self-hosted or Temps Cloud)

## Related

- [`@temps-sdk/kv`](https://www.npmjs.com/package/@temps-sdk/kv) -- Key-value store
- [`@temps-sdk/blob`](https://www.npmjs.com/package/@temps-sdk/blob) -- File storage
- [`@temps-sdk/react-analytics`](https://www.npmjs.com/package/@temps-sdk/react-analytics) -- React analytics, session replay, error tracking
- [`@temps-sdk/node-sdk`](https://www.npmjs.com/package/@temps-sdk/node-sdk) -- Full platform API client and server-side error tracking

## License

MIT
