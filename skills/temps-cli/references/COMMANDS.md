# Temps CLI Reference

> Auto-generated documentation for the Temps CLI.
>
> Generated from: `@temps-sdk/cli@0.1.36`
>
> Apply the authorization, target-context, and secret-handling rules in
> [the Temps CLI skill](../SKILL.md) before executing a command.

## Installation

```bash
bunx @temps-sdk/cli@0.1.36 [command]

# Fallback when Bun is unavailable
npx @temps-sdk/cli@0.1.36 [command]
```

## Authentication

Before using most commands, you need to authenticate:

```bash
# Login interactively
bunx @temps-sdk/cli@0.1.36 login

# Or configure with wizard
bunx @temps-sdk/cli@0.1.36 configure
```

## Global Options

| Flag | Description |
|------|-------------|
| `-V, --version` | Display version number |
| `--target-context <name>` | Target one configured server for this invocation |
| `--no-color` | Disable colored output |
| `--debug` | Enable debug output |
| `-h, --help` | Display help for command |

## Command index

Use this index or search for a top-level command heading to load only the relevant group.

- [`login`](#login) - Authenticate with a Temps server. Opens the browser for interactive logins; use --api-key for headless / CI.
- [`logout`](#logout) - Revoke the active context's API key on the server and forget it locally
- [`whoami`](#whoami) - Display current authenticated user and active context
- [`context`](#context) - Manage CLI contexts (one set of credentials per Temps server)
- [`configure`](#configure) - Configure CLI settings (AWS-style wizard)
- [`projects`](#projects) - Manage projects
- [`drop`](#drop) - Detect and deploy a local source directory or ZIP without Git
- [`deploy`](#deploy) - Deploy a project from git
- [`deploy:static`](#deploystatic) - Deploy static files (tar.gz, zip, or directory)
- [`deploy:image`](#deployimage) - Deploy a pre-built Docker image
- [`deploy:local-image`](#deploylocal-image) - Build and deploy a local Docker image (or deploy existing image with --image)
- [`deployments`](#deployments) - Manage deployments
- [`domains`](#domains) - Manage custom domains
- [`environments`](#environments) - Manage environments and environment variables
- [`providers`](#providers) - Manage Git providers
- [`backups`](#backups) - Manage backup schedules and backups
- [`runtime-logs`](#runtime-logs) - View runtime container logs (use -f to follow in real-time)
- [`notifications`](#notifications) - Manage notification providers (Slack, Email, Webhook, etc.)
- [`dns`](#dns) - Manage DNS providers for automated domain verification
- [`services`](#services) - Manage external services (databases, caches, storage)
- [`settings`](#settings) - Manage platform settings
- [`users`](#users) - Manage platform users
- [`teams`](#teams) - Manage teams and project access
- [`access`](#access) - Manage which teams can reach a project
- [`apikeys`](#apikeys) - Manage API keys for programmatic access
- [`monitors`](#monitors) - Manage uptime monitors for status pages
- [`webhooks`](#webhooks) - Manage webhooks for project events
- [`containers`](#containers) - Manage project containers in environments
- [`cluster`](#cluster) - Cluster-wide multi-node operations
- [`tokens`](#tokens) - Manage deployment tokens for project API access (KV, Blob, etc.)
- [`errors`](#errors) - Manage error tracking and error groups
- [`metrics`](#metrics) - Query OTel application metrics for debugging (not container/docker stats — see "temps containers metrics" for those)
- [`traces`](#traces) - Inspect distributed traces and operation latency
- [`facets`](#facets) - Manage OTel span attribute facets — attribute keys promoted to a fast-filterable column (ClickHouse or TimescaleDB, whichever backend is active; see ADR-039). Facets are platform-global, not per-project, since the underlying spans table is shared across every project. Historical backfill runs asynchronously — check `temps facets list` for status.
- [`otel-forward`](#otel-forward) - Manage OTel forwarding destinations that relay ingested traces, metrics, and logs to an external OTLP-compatible collector
- [`otel`](#otel) - Inspect the OTLP ingest pipeline itself — throughput, drops and failure reasons (server-wide, not project-scoped; see "temps metrics" to query ingested application metrics)
- [`kv`](#kv) - KV store commands (coming soon)
- [`flags`](#flags) - Manage feature flags (runtime config that changes without a redeploy)
- [`data`](#data) - Browse the data inside a service (tables, collections, keys, objects) — read-only
- [`blob`](#blob) - Blob storage commands (coming soon)
- [`dsn`](#dsn) - Manage Data Source Names (DSNs) for error tracking and analytics
- [`scans`](#scans) - Manage vulnerability scans
- [`custom-domains`](#custom-domains) - Manage project custom domains
- [`dns-provider`](#dns-provider) - Manage DNS providers and managed domains
- [`ip-access`](#ip-access) - Manage IP access control rules
- [`audit`](#audit) - View audit logs
- [`proxy-logs`](#proxy-logs) - View proxy request logs and statistics
- [`email-domains`](#email-domains) - Manage email domains for transactional email
- [`email-providers`](#email-providers) - Manage email providers (SES, Scaleway) for transactional email
- [`incidents`](#incidents) - Manage incidents for status pages and monitoring
- [`emails`](#emails) - Manage and send emails
- [`load-balancer`](#load-balancer) - Manage load balancer routes
- [`migrate`](#migrate) - Migrate a project from another platform (Vercel, Coolify, Dokploy, CapRover, Portainer, Kubernetes, Docker) into temps
- [`templates`](#templates) - Browse deployment templates
- [`platform`](#platform) - View platform and server information
- [`presets`](#presets) - Browse available build presets
- [`analytics`](#analytics) - View project analytics
- [`funnels`](#funnels) - Manage analytics funnels for projects
- [`notification-preferences`](#notification-preferences) - Manage notification preferences
- [`skills`](#skills) - Manage AI skill definitions (global or project-scoped)
- [`mcp-servers`](#mcp-servers) - Manage MCP server definitions (global or project-scoped)
- [`mcp`](#mcp) - Configure this Temps instance as an MCP server for AI clients (Claude Code, Claude Desktop, Codex, Cursor, VS Code, Windsurf, Zed)
- [`secrets`](#secrets) - Manage agent secrets. env-type: reference as ${TEMPS_SECRET:name} in MCP config. file-type: written to --mount-path in sandbox; reference that path.
- [`sandbox`](#sandbox) - Manage standalone sandboxes (/v1/sandbox API)
- [`workflow`](#workflow) - Trigger and inspect agent/workflow runs
- [`revenue`](#revenue) - Manage revenue integrations and import historical data
- [`session-replay`](#session-replay) - Manage session replay recordings
- [`traefik-discovery`](#traefik-discovery) - Route containers Temps did not deploy by reading their Traefik labels (an existing docker-compose / Coolify / Dokploy stack)
- [`init`](#init) - Initialize a Temps project in the current directory
- [`link`](#link) - Link current directory to a Temps project
- [`up`](#up) - Deploy the current project (runs setup wizard if not linked)
- [`status`](#status) - Show project deployment status
- [`ai`](#ai) - AI assistant status for a project
- [`instances`](#instances) - Manage Temps server instances
- [`env:pull`](#envpull) - Pull environment variables to a .env file
- [`env:push`](#envpush) - Push environment variables from a .env file
- [`rollback`](#rollback) - Rollback to a previous deployment
- [`open`](#open) - Open project URL in browser
- [`exec`](#exec) - Execute a command in a running container (coming soon)
- [`dev`](#dev) - Start a local development tunnel (coming soon)
- [`cloud`](#cloud) - Temps Cloud

## Commands

## `login`

Authenticate with a Temps server. Opens the browser for interactive logins; use --api-key for headless / CI.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-k, --api-key <key>` | Use a pre-minted API key (Settings → API Keys) instead of opening the browser. Required for headless / CI. | - | No |
| `--context <name>` | Save the credentials under this context name (defaults to URL host) | - | No |
| `--debug` | Print every request/response (URL, status, headers, raw body) to stderr. Also enabled via TEMPS_DEBUG=1. | - | No |

## `logout`

Revoke the active context's API key on the server and forget it locally

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--context <name>` | Log out of a specific context (defaults to active) | - | No |
| `--local-only` | Skip server-side revocation; only clear local credentials | - | No |

## `whoami`

Display current authenticated user and active context

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

## `context`

Manage CLI contexts (one set of credentials per Temps server)

**Subcommands:**

- `list` (`ls`) - List all configured contexts
- `use` (`switch`) - Switch the active context
- `remove` (`rm`) - Remove a context (does NOT revoke the key on the server)
- `rename` - Rename a context
- `current` - Print the active context name

### `context list` (alias: `ls`)

List all configured contexts

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `context use` (alias: `switch`)

Switch the active context

### `context remove` (alias: `rm`)

Remove a context (does NOT revoke the key on the server)

### `context rename`

Rename a context

### `context current`

Print the active context name

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format with full details | - | No |

## `configure`

Configure CLI settings (AWS-style wizard)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--api-url <url>` | API URL | - | No |
| `--api-token <token>` | API token for authentication | - | No |
| `--output-format <format>` | Output format (table, json, minimal) | - | No |
| `--enable-colors` | Enable colored output in config | - | No |
| `--disable-colors` | Disable colored output in config | - | No |
| `-i, --interactive` | Force interactive mode even in non-TTY | - | No |
| `-y, --no-interactive` | Non-interactive mode (uses defaults for unspecified options) | - | No |

**Subcommands:**

- `get` - Get a configuration value
- `set` - Set a configuration value
- `list` - List all configuration values
- `show` - Show current configuration and authentication status
- `reset` - Reset configuration to defaults

### `configure get`

Get a configuration value

### `configure set`

Set a configuration value

### `configure list`

List all configuration values

### `configure show`

Show current configuration and authentication status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `configure reset`

Reset configuration to defaults

## `projects` (alias: `project`, `p`)

Manage projects

**Subcommands:**

- `secrets` - Manage project secrets — mounted into the deployed container as files at /run/secrets/<KEY>, not environment variables. Distinct from `temps secrets` (agent/MCP-sandbox-scoped).
- `list` (`ls`) - List all projects
- `create` (`new`) - Create a new project (git-based or manual deployment)
- `show` (`get`) - Show project details
- `update` (`edit`) - Update project name and description
- `settings` - Update project settings (name, slug, attack mode, preview environments, image retention)
- `git` - Update git repository settings
- `source` - Show or change how a project is deployed (primary source, and whether it also accepts `drop` uploads)
- `config` - Update deployment configuration (resources, replicas)
- `delete` (`rm`) - Delete a project

### `projects secrets`

Manage project secrets — mounted into the deployed container as files at /run/secrets/<KEY>, not environment variables. Distinct from `temps secrets` (agent/MCP-sandbox-scoped).

**Subcommands:**

- `list` (`ls`) - List secrets for a project (values are never returned)
- `create` (`add`) - Create a project secret (mounted at /run/secrets/<KEY> on the next deployment)
- `update` - Update a project secret (a redeploy is required for running containers to pick it up)
- `delete` (`rm`) - Delete a project secret

#### `projects secrets list` (alias: `ls`)

List secrets for a project (values are never returned)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Filter to one environment | - | No |
| `--json` | Output in JSON format | - | No |

#### `projects secrets create` (alias: `add`)

Create a project secret (mounted at /run/secrets/<KEY> on the next deployment)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-k, --key <key>` | Secret key — becomes the filename at /run/secrets/<KEY>. Letters, digits, underscore; must start with a letter or underscore. | - | Yes |
| `-v, --value <value>` | Secret value (<=1 MiB). Prefix with @ to read from a local file, e.g. @./auth.json — never touches shell history. | - | Yes |
| `-e, --environment <name>` | Scope to one environment (repeatable; default: all) | `` | No |
| `-s, --service <name>` | Docker Compose service allowed to read this secret (repeatable; default: every service). Ignored for non-Compose projects, which deploy a single container. | `` | No |
| `--include-in-preview` | Also mount this secret in preview environments | - | No |

#### `projects secrets update`

Update a project secret (a redeploy is required for running containers to pick it up)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-k, --key <key>` | Key of the secret to update | - | Yes |
| `-v, --value <value>` | New value (<=1 MiB). Prefix with @ to read from a local file. Omit to keep the existing value. | - | No |
| `-e, --environment <name>` | Replace environment scoping (repeatable) | `` | No |
| `-s, --service <name>` | Replace the Docker Compose service scope (repeatable). Pass none to keep the current scope; use --all-services to widen it back to every service. | `` | No |
| `--all-services` | Deliver to every Compose service, clearing any per-service scope | - | No |
| `--include-in-preview` | Include in preview environments | - | No |
| `--no-include-in-preview` | Exclude from preview environments | - | No |

#### `projects secrets delete` (alias: `rm`)

Delete a project secret

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `projects list` (alias: `ls`)

List all projects

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--page <n>` | Page number | - | No |
| `--per-page <n>` | Items per page | - | No |

### `projects create` (alias: `new`)

Create a new project (git-based or manual deployment)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Project name | - | No |
| `-d, --description <description>` | Project description | - | No |
| `--repo <repository>` | Repository in owner/name format (nested groups supported: group/subgroup/name) | - | No |
| `--branch <branch>` | Git branch | - | No |
| `--directory <directory>` | Root directory (relative to repo) | - | No |
| `--preset <preset>` | Build preset (e.g., nextjs, nodejs, static, docker) | - | No |
| `--connection <id>` | Git connection ID | - | No |
| `--manual` | Create a manual (non-git) project - deploy via Docker image or static files | - | No |
| `--source-type <type>` | Manual deployment method: manual (flexible), docker_image, or static_files | - | No |
| `--image <image>` | Docker image for the first deployment (manual mode) | - | No |
| `--port <port>` | Application/container port (manual mode, default: 3000) | - | No |
| `-y, --yes` | Skip optional prompts (services, env vars, set-default) | - | No |

### `projects show` (alias: `get`)

Show project details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `projects update` (alias: `edit`)

Update project name and description

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-n, --name <name>` | New project name | - | No |
| `-d, --description <description>` | New project description | - | No |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip prompts, use provided values (for automation) | - | No |

### `projects settings`

Update project settings (name, slug, attack mode, preview environments, image retention)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--name <name>` | Project display name (does not change the URL) | - | No |
| `--slug <slug>` | Project URL slug | - | No |
| `--attack-mode` | Enable attack mode (CAPTCHA protection) | - | No |
| `--no-attack-mode` | Disable attack mode | - | No |
| `--preview-envs` | Enable preview environments | - | No |
| `--no-preview-envs` | Disable preview environments | - | No |
| `--image-retention-hours <hours>` | Hours to keep built images before nightly cleanup removes them (1-8760). Images are needed to roll back, so this is the project rollback window | - | No |
| `--reset-image-retention` | Clear the per-project image retention override and use the system default | - | No |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip prompts (for automation) | - | No |

### `projects git`

Update git repository settings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--owner <owner>` | Repository owner | - | No |
| `--repo <repo>` | Repository name | - | No |
| `--branch <branch>` | Main branch | - | No |
| `--directory <directory>` | App directory path | - | No |
| `--preset <preset>` | Build preset (auto, nextjs, nodejs, static, docker, rust, go, python) | - | No |
| `--connection <id>` | Git connection ID (links the project to an actual clone-access connection; omit to leave the existing connection unchanged) | - | No |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip prompts, use provided/existing values (for automation) | - | No |

### `projects source`

Show or change how a project is deployed (primary source, and whether it also accepts `drop` uploads)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--type <type>` | Set the primary source: docker_image, static_files, uploaded_source or manual (use `projects git` to switch to git) | - | No |
| `--allow-alternate` | Also accept an uploaded source archive from `drop`, keeping the current source as default | - | No |
| `--no-allow-alternate` | Only deploy from the configured source | - | No |
| `--json` | Output in JSON format | - | No |

### `projects config`

Update deployment configuration (resources, replicas)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--replicas <n>` | Number of container replicas | - | No |
| `--cpu-limit <limit>` | CPU limit in cores (e.g., 0.5, 1, 2) | - | No |
| `--memory-limit <limit>` | Memory limit in MB | - | No |
| `--auto-deploy` | Enable automatic deployments | - | No |
| `--no-auto-deploy` | Disable automatic deployments | - | No |
| `--request-timeout <seconds>` | Default timeout for regular HTTP requests, in seconds | - | No |
| `--sse-idle-timeout <seconds>` | Default idle timeout for SSE streams, in seconds | - | No |
| `--websocket-idle-timeout <seconds>` | Default idle timeout for WebSocket connections, in seconds | - | No |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip prompts (for automation) | - | No |

### `projects delete` (alias: `rm`)

Delete a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

## `drop`

Detect and deploy a local source directory or ZIP without Git

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | Name for the new project (slugified automatically) | - | No |
| `--project <project>` | Deploy into an existing project (slug or ID) instead of creating one | - | No |
| `--environment <env>` | Target environment (requires --project, default: production) | - | No |
| `--preset <preset>` | Select a detected preset | - | No |
| `--directory <directory>` | Select a detected project root | - | No |
| `--no-wait` | Do not wait for deployment to complete | - | No |
| `--timeout <seconds>` | Deployment timeout | `600` | No |

## `deploy`

Deploy a project from git

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Target environment name | - | No |
| `--environment-id <id>` | Target environment ID | - | No |
| `-b, --branch <branch>` | Git branch to deploy | - | No |
| `-c, --commit <sha>` | Specific commit SHA to deploy | - | No |
| `--no-wait` | Do not wait for deployment to complete | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

## `deploy:static` (alias: `deploy-static`)

Deploy static files (tar.gz, zip, or directory)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Path to static files archive or directory | - | Yes |
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Target environment name | `production` | No |
| `--environment-id <id>` | Target environment ID | - | No |
| `--no-wait` | Do not wait for deployment to complete | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--metadata <json>` | Additional metadata (JSON format) | - | No |
| `--health-check-path <path>` | HTTP health-check path (must start with "/", e.g. /api/healthz). Overrides .temps.yaml; also updates the uptime monitor. | - | No |
| `--timeout <seconds>` | Timeout in seconds for --wait | `300` | No |

## `deploy:image` (alias: `deploy-image`)

Deploy a pre-built Docker image

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <image>` | Docker image reference (e.g., ghcr.io/org/app:v1.0) | - | Yes |
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Target environment name | `production` | No |
| `--environment-id <id>` | Target environment ID | - | No |
| `--no-wait` | Do not wait for deployment to complete | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--metadata <json>` | Additional metadata (JSON format) | - | No |
| `--health-check-path <path>` | HTTP health-check path (must start with "/", e.g. /api/healthz). Overrides .temps.yaml; also updates the uptime monitor. | - | No |
| `--timeout <seconds>` | Timeout in seconds for --wait | `300` | No |

## `deploy:local-image` (alias: `deploy-local-image`)

Build and deploy a local Docker image (or deploy existing image with --image)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <image>` | Use existing local image instead of building (skips build) | - | No |
| `-f, --dockerfile <path>` | Path to Dockerfile | `Dockerfile` | No |
| `-c, --context <path>` | Build context directory | `.` | No |
| `--build-arg <arg...>` | Build arguments (can be specified multiple times) | - | No |
| `--no-build` | Skip building, requires --image | - | No |
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Target environment name | `production` | No |
| `--environment-id <id>` | Target environment ID | - | No |
| `-t, --tag <tag>` | Tag for the built/uploaded image | - | No |
| `--no-wait` | Do not wait for deployment to complete | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--metadata <json>` | Additional metadata (JSON format) | - | No |
| `--health-check-path <path>` | HTTP health-check path (must start with "/", e.g. /api/healthz). Overrides .temps.yaml; also updates the uptime monitor. | - | No |
| `--timeout <seconds>` | Timeout in seconds for --wait | `600` | No |

## `deployments` (alias: `deploys`)

Manage deployments

**Subcommands:**

- `list` (`ls`) - List deployments
- `status` - Show deployment status
- `rollback` - Rollback to previous deployment
- `cancel` - Cancel a running deployment
- `pause` - Pause a deployment
- `resume` - Resume a paused deployment
- `teardown` - Teardown a deployment and remove all resources
- `logs` - Show deployment build logs
- `failure-report` - Preview or send a redacted deploy-failure trace

### `deployments list` (alias: `ls`)

List deployments

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Filter by environment name (client-side) | - | No |
| `--environment-id <id>` | Filter by environment ID (server-side) | - | No |
| `-n, --limit <number>` | Limit results | `10` | No |
| `--page <n>` | Page number | - | No |
| `--per-page <n>` | Items per page | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments status`

Show deployment status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID (required) | - | No |
| `-d, --deployment-id <id>` | Deployment ID (required) | - | No |
| `--json` | Output in JSON format | - | No |

### `deployments rollback`

Rollback to previous deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID (required) | - | No |
| `-e, --environment <env>` | Target environment | `production` | No |
| `--to <deployment>` | Rollback to specific deployment ID | - | No |

### `deployments cancel`

Cancel a running deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-d, --deployment-id <id>` | Deployment ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |

### `deployments pause`

Pause a deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-d, --deployment-id <id>` | Deployment ID | - | Yes |

### `deployments resume`

Resume a paused deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-d, --deployment-id <id>` | Deployment ID | - | Yes |

### `deployments teardown`

Teardown a deployment and remove all resources

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-d, --deployment-id <id>` | Deployment ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |

### `deployments logs`

Show deployment build logs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Environment | `production` | No |
| `-f, --follow` | Follow log output | - | No |
| `-n, --lines <number>` | Number of lines to show | `100` | No |
| `-d, --deployment <id>` | Specific deployment ID | - | No |

### `deployments failure-report`

Preview or send a redacted deploy-failure trace

**Subcommands:**

- `preview` - Preview the redacted, editable failure-report text for a failed job
- `send` - Send a failure report to the Temps team. Reads report text from --text-file, or stdin if piped, or defaults to the redacted preview.

#### `deployments failure-report preview`

Preview the redacted, editable failure-report text for a failed job

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-d, --deployment-id <id>` | Deployment ID | - | Yes |
| `-j, --job-id <id>` | Failed job ID (see "deployments logs") | - | Yes |

#### `deployments failure-report send`

Send a failure report to the Temps team. Reads report text from --text-file, or stdin if piped, or defaults to the redacted preview.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-d, --deployment-id <id>` | Deployment ID | - | Yes |
| `-j, --job-id <id>` | Failed job ID (see "deployments logs") | - | Yes |
| `--text-file <path>` | Read the (already-reviewed) report text from a file | - | No |

## `domains` (alias: `domain`)

Manage custom domains

**Subcommands:**

- `list` (`ls`) - List domains
- `add` - Add a custom domain
- `verify` - Verify domain and provision SSL certificate
- `remove` (`rm`) - Remove a domain
- `ssl` - Manage SSL certificate
- `status` - Check domain status
- `renewal-attempts` - Show the certificate renewal-attempt history for a domain
- `orders` (`order`) - Manage ACME orders for SSL certificate provisioning
- `dns-challenge` - Setup DNS challenge records automatically using a DNS provider
- `http-debug` - Debug HTTP-01 challenge for a domain

### `domains list` (alias: `ls`)

List domains

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `domains add`

Add a custom domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |
| `-c, --challenge <type>` | Challenge type (http-01 or dns-01) | `http-01` | No |
| `-y, --yes` | Skip confirmation prompts | - | No |

### `domains verify`

Verify domain and provision SSL certificate

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |

### `domains remove` (alias: `rm`)

Remove a domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `domains ssl`

Manage SSL certificate

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |
| `--renew` | Force certificate renewal | - | No |

### `domains status`

Check domain status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |

### `domains renewal-attempts`

Show the certificate renewal-attempt history for a domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |
| `--page <page>` | Page number (1-indexed) | `1` | No |
| `--page-size <pageSize>` | Items per page (max 100) | `20` | No |
| `--json` | Output in JSON format | - | No |

### `domains orders` (alias: `order`)

Manage ACME orders for SSL certificate provisioning

**Subcommands:**

- `list` (`ls`) - List all ACME orders
- `show` - Show ACME order for a domain
- `create` - Create or recreate an ACME order for a domain
- `finalize` - Finalize an ACME order (complete challenge validation)
- `cancel` - Cancel an ACME order for a domain

#### `domains orders list` (alias: `ls`)

List all ACME orders

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `domains orders show`

Show ACME order for a domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--domain-id <id>` | Domain ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `domains orders create`

Create or recreate an ACME order for a domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--domain-id <id>` | Domain ID | - | Yes |

#### `domains orders finalize`

Finalize an ACME order (complete challenge validation)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--domain-id <id>` | Domain ID | - | Yes |

#### `domains orders cancel`

Cancel an ACME order for a domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--domain-id <id>` | Domain ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `domains dns-challenge`

Setup DNS challenge records automatically using a DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--domain-id <id>` | Domain ID | - | Yes |
| `--provider-id <id>` | DNS provider ID | - | Yes |

### `domains http-debug`

Debug HTTP-01 challenge for a domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |
| `--json` | Output in JSON format | - | No |

## `environments` (alias: `envs`, `env`)

Manage environments and environment variables

**Subcommands:**

- `list` (`ls`) - List environments for a project
- `create` - Create a new environment
- `delete` (`rm`) - Delete an environment
- `vars` - Manage environment variables
- `resources` - View or set CPU/memory resources for an environment
- `timeouts` - View or set upstream request/idle timeouts for an environment
- `force-https` - View or set the HTTP to HTTPS redirect override for an environment
- `scale` - View or set the number of replicas for an environment
- `crons` - Manage cron jobs

### `environments list` (alias: `ls`)

List environments for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `environments create`

Create a new environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-n, --name <name>` | Environment name | - | No |
| `-b, --branch <branch>` | Git branch | - | No |
| `--preview` | Set as preview environment | - | No |

### `environments delete` (alias: `rm`)

Delete an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-f, --force` | Skip confirmation | - | No |

### `environments vars`

Manage environment variables

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |

**Subcommands:**

- `list` (`ls`) - List environment variables
- `get` - Get a specific environment variable
- `set` - Set an environment variable
- `delete` (`rm`, `unset`) - Delete an environment variable
- `import` - Import environment variables from a .env file
- `export` - Export environment variables to .env format

#### `environments vars list` (alias: `ls`)

List environment variables

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Filter by environment name | - | No |
| `--show-values` | Show actual values (hidden by default) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments vars get`

Get a specific environment variable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Specify environment (if variable exists in multiple) | - | No |

#### `environments vars set`

Set an environment variable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environments <names>` | Comma-separated environment names (interactive if not provided) | - | No |
| `--no-preview` | Exclude from preview environments | - | No |
| `--update` | Update existing variable instead of creating new | - | No |
| `--secret` | Store as a secret: the value is masked in the UI and never returned by the API. One-way — to make a secret readable again you must delete the variable and create it anew | - | No |

#### `environments vars delete` (alias: `rm`, `unset`)

Delete an environment variable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Delete only from specific environment | - | No |
| `-f, --force` | Skip confirmation | - | No |

#### `environments vars import`

Import environment variables from a .env file

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environments <names>` | Comma-separated environment names | - | No |
| `--overwrite` | Overwrite existing variables | - | No |

#### `environments vars export`

Export environment variables to .env format

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Export from specific environment | - | No |
| `-o, --output <file>` | Write to file instead of stdout | - | No |

### `environments resources`

View or set CPU/memory resources for an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--cpu <millicores>` | CPU limit in millicores (e.g., 500 = 0.5 CPU) | - | No |
| `--memory <mb>` | Memory limit in MB (e.g., 512) | - | No |
| `--cpu-request <millicores>` | CPU request in millicores (guaranteed minimum) | - | No |
| `--memory-request <mb>` | Memory request in MB (guaranteed minimum) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments timeouts`

View or set upstream request/idle timeouts for an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--request <seconds>` | Timeout for regular (non-streaming) HTTP requests, in seconds | - | No |
| `--sse-idle <seconds>` | Idle timeout for Server-Sent Events streams, in seconds | - | No |
| `--websocket-idle <seconds>` | Idle timeout for WebSocket connections, in seconds | - | No |
| `--inherit` | Clear all three overrides (inherit the project/global defaults) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments force-https`

View or set the HTTP to HTTPS redirect override for an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--enable` | Always redirect plain HTTP to HTTPS, even without a local certificate | - | No |
| `--disable` | Never redirect: keep serving this environment over plain HTTP | - | No |
| `--inherit` | Clear the override and follow the proxy default | - | No |
| `--json` | Output in JSON format | - | No |

### `environments scale`

View or set the number of replicas for an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Environment name or slug | `production` | No |
| `-r, --replicas <count>` | Number of replicas to set | - | No |
| `--json` | Output in JSON format | - | No |

### `environments crons`

Manage cron jobs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Environment name or slug | - | Yes |

**Subcommands:**

- `list` (`ls`) - List cron jobs for an environment
- `show` - Show cron job details
- `executions` (`execs`) - Show cron job execution history

#### `environments crons list` (alias: `ls`)

List cron jobs for an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `environments crons show`

Show cron job details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Cron job ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `environments crons executions` (alias: `execs`)

Show cron job execution history

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Cron job ID | - | Yes |
| `--page <page>` | Page number | `1` | No |
| `--per-page <count>` | Items per page | `20` | No |
| `--json` | Output in JSON format | - | No |

## `providers` (alias: `provider`)

Manage Git providers

**Subcommands:**

- `list` (`ls`) - List configured Git providers
- `add` - Add a new Git provider
- `remove` (`rm`) - Remove a Git provider
- `show` - Show Git provider details
- `activate` - Activate a Git provider
- `deactivate` - Deactivate a Git provider
- `safe-delete` - Safely delete a Git provider (checks dependencies first)
- `deletion-check` - Check if a Git provider can be safely deleted
- `git` - Manage Git providers
- `connections` (`conn`) - Manage Git provider connections

### `providers list` (alias: `ls`)

List configured Git providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `providers add`

Add a new Git provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --provider <provider>` | Provider type (github, gitlab, bitbucket, gitea, generic) | - | No |
| `-n, --name <name>` | Provider name | - | No |
| `-t, --token <token>` | Personal access token (or Bitbucket access token / app password) | - | No |
| `--base-url <url>` | Instance base URL (GitLab/Gitea self-hosted; required for gitea) | - | No |
| `--username <username>` | Bitbucket username (selects app-password auth) | - | No |
| `--password <password>` | Bitbucket app password (used with --username) | - | No |
| `--clone-url <url>` | HTTPS clone URL (generic provider) | - | No |
| `--token-username <username>` | HTTP Basic username for the token (generic; default x-access-token) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `providers remove` (alias: `rm`)

Remove a Git provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `providers show`

Show Git provider details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `providers activate`

Activate a Git provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |

### `providers deactivate`

Deactivate a Git provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |

### `providers safe-delete`

Safely delete a Git provider (checks dependencies first)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `providers deletion-check`

Check if a Git provider can be safely deleted

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `providers git`

Manage Git providers

**Subcommands:**

- `connect` - Connect a Git provider (github, gitlab, bitbucket, gitea, generic)
- `repos` - List available repositories

#### `providers git connect`

Connect a Git provider (github, gitlab, bitbucket, gitea, generic)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --provider <provider>` | Provider type (github, gitlab, bitbucket, gitea, generic) | - | Yes |
| `-n, --name <name>` | Provider name | - | No |
| `-t, --token <token>` | Personal access token (or Bitbucket access token / app password) | - | No |
| `--base-url <url>` | Instance base URL (GitLab/Gitea self-hosted; required for gitea) | - | No |
| `--username <username>` | Bitbucket username (selects app-password auth) | - | No |
| `--password <password>` | Bitbucket app password (used with --username) | - | No |
| `--clone-url <url>` | HTTPS clone URL (generic provider) | - | No |
| `--token-username <username>` | HTTP Basic username for the token (generic; default x-access-token) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

#### `providers git repos`

List available repositories

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID (optional, lists all if not provided) | - | No |
| `--json` | Output in JSON format | - | No |
| `--search <term>` | Search repositories by name | - | No |
| `--page <n>` | Page number | - | No |
| `--per-page <n>` | Items per page (max: 100) | - | No |
| `--sort <field>` | Sort by field (name, created_at, updated_at, stars) | - | No |
| `--direction <dir>` | Sort direction: asc or desc | - | No |
| `--language <lang>` | Filter by programming language | - | No |
| `--owner <owner>` | Filter by repository owner | - | No |

### `providers connections` (alias: `conn`)

Manage Git provider connections

**Subcommands:**

- `list` (`ls`) - List all Git connections
- `show` - Show connection details for a provider
- `delete` (`rm`) - Delete a Git connection
- `activate` - Activate a Git connection
- `deactivate` - Deactivate a Git connection
- `sync` - Sync repositories for a Git connection
- `update-token` - Update access token for a Git connection
- `validate` - Validate a Git connection

#### `providers connections list` (alias: `ls`)

List all Git connections

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--page <n>` | Page number | - | No |
| `--per-page <n>` | Items per page (default: 30, max: 100) | - | No |
| `--sort <field>` | Sort by field (created_at, updated_at, account_name) | - | No |
| `--direction <dir>` | Sort direction: asc or desc (default: desc) | - | No |

#### `providers connections show`

Show connection details for a provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `providers connections delete` (alias: `rm`)

Delete a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

#### `providers connections activate`

Activate a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |

#### `providers connections deactivate`

Deactivate a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |

#### `providers connections sync`

Sync repositories for a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |

#### `providers connections update-token`

Update access token for a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |
| `-t, --token <token>` | New access token | - | Yes |

#### `providers connections validate`

Validate a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |
| `--json` | Output in JSON format | - | No |

## `backups` (alias: `backup`)

Manage backup schedules and backups

**Subcommands:**

- `schedules` (`schedule`) - Manage backup schedules
- `sources` (`source`) - Manage S3 backup sources
- `list` (`ls`) - List backups for a schedule
- `show` - Show backup details
- `delete` (`rm`) - Permanently delete one terminal backup
- `cleanup` - Delete backups expired by their schedule retention policy
- `run-service` - Run a backup for an external service

### `backups schedules` (alias: `schedule`)

Manage backup schedules

**Subcommands:**

- `list` (`ls`) - List backup schedules
- `create` - Create a backup schedule
- `show` - Show backup schedule details
- `enable` - Enable a backup schedule
- `disable` - Disable a backup schedule
- `delete` (`rm`) - Delete a backup schedule

#### `backups schedules list` (alias: `ls`)

List backup schedules

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `backups schedules create`

Create a backup schedule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Schedule name | - | No |
| `-t, --type <type>` | Backup type (full, incremental) | - | No |
| `-s, --schedule <cron>` | Schedule expression (cron format) | - | No |
| `-r, --retention <days>` | Retention period in days | - | No |
| `-d, --description <desc>` | Description | - | No |
| `--s3-source-id <id>` | S3 Source ID | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

#### `backups schedules show`

Show backup schedule details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Schedule ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `backups schedules enable`

Enable a backup schedule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Schedule ID | - | Yes |

#### `backups schedules disable`

Disable a backup schedule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Schedule ID | - | Yes |

#### `backups schedules delete` (alias: `rm`)

Delete a backup schedule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Schedule ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `backups sources` (alias: `source`)

Manage S3 backup sources

**Subcommands:**

- `list` (`ls`) - List S3 sources
- `create` - Create an S3 source
- `show` - Show S3 source details
- `update` - Update an S3 source
- `remove` (`rm`) - Delete an S3 source
- `backups` - List backups for an S3 source
- `run` - Trigger a backup for an S3 source

#### `backups sources list` (alias: `ls`)

List S3 sources

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `backups sources create`

Create an S3 source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Source name | - | No |
| `--bucket <bucket>` | S3 bucket name | - | No |
| `--region <region>` | S3 region | - | No |
| `--endpoint <endpoint>` | S3 endpoint (for S3-compatible services) | - | No |
| `--access-key <key>` | Access key ID | - | No |
| `--secret-key <key>` | Secret access key | - | No |
| `--prefix <prefix>` | Bucket path/prefix | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

#### `backups sources show`

Show S3 source details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | S3 source ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `backups sources update`

Update an S3 source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | S3 source ID | - | Yes |
| `-n, --name <name>` | New source name | - | No |
| `--bucket <bucket>` | New S3 bucket name | - | No |
| `--region <region>` | New S3 region | - | No |
| `--endpoint <endpoint>` | New S3 endpoint | - | No |
| `--access-key <key>` | New access key ID | - | No |
| `--secret-key <key>` | New secret access key | - | No |
| `--prefix <prefix>` | New bucket path/prefix | - | No |

#### `backups sources remove` (alias: `rm`)

Delete an S3 source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | S3 source ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

#### `backups sources backups`

List backups for an S3 source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | S3 source ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `backups sources run`

Trigger a backup for an S3 source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | S3 source ID | - | Yes |

### `backups list` (alias: `ls`)

List backups for a schedule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--schedule-id <id>` | Schedule ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `backups show`

Show backup details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Backup ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `backups delete` (alias: `rm`)

Permanently delete one terminal backup

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Backup UUID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `backups cleanup`

Delete backups expired by their schedule retention policy

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--dry-run` | Preview expired backups without deleting them | - | No |
| `--schedule-id <id>` | Limit cleanup to one schedule | - | No |
| `-y, --yes` | Skip confirmation prompt | - | No |
| `--json` | Output the cleanup report as JSON | - | No |

### `backups run-service`

Run a backup for an external service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | External service ID | - | Yes |
| `--s3-source-id <id>` | S3 source ID to store the backup | - | Yes |
| `-t, --type <type>` | Backup type (e.g., full, incremental) | - | No |

## `runtime-logs` (alias: `rlogs`)

View runtime container logs (use -f to follow in real-time)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Environment name | `production` | No |
| `-c, --container <id>` | Container ID (partial match supported) | - | No |
| `-n, --tail <lines>` | Number of lines to tail | `1000` | No |
| `-t, --timestamps` | Show timestamps | - | No |
| `-f, --follow` | Follow log output (stream in real-time) | - | No |

## `notifications` (alias: `notify`)

Manage notification providers (Slack, Email, Webhook, etc.)

**Subcommands:**

- `list` (`ls`) - List configured notification providers
- `add` - Add a new notification provider
- `update` - Update a notification provider
- `enable` - Enable a notification provider
- `disable` - Disable a notification provider
- `show` - Show notification provider details
- `remove` (`rm`) - Remove a notification provider
- `test` - Send a test notification
- `routes` - Manage severity-based notification routes (which providers receive which severities)

### `notifications list` (alias: `ls`)

List configured notification providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `notifications add`

Add a new notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-t, --type <type>` | Provider type (slack, email, webhook) | - | No |
| `-n, --name <name>` | Provider name | - | No |
| `-w, --webhook-url <url>` | Webhook URL (for slack) | - | No |
| `-c, --channel <channel>` | Channel name (for slack, optional) | - | No |
| `--smtp-host <host>` | SMTP host (for email) | - | No |
| `--smtp-port <port>` | SMTP port (for email) | - | No |
| `--username <username>` | SMTP username (for email) | - | No |
| `--password <password>` | SMTP password (for email) | - | No |
| `--from-address <address>` | From email address (for email) | - | No |
| `--from-name <name>` | From display name (for email, optional) | - | No |
| `--to-addresses <addresses>` | Comma-separated recipient addresses (for email) | - | No |
| `--url <url>` | Webhook URL (for webhook) | - | No |
| `--method <method>` | HTTP method: POST, PUT, PATCH (for webhook, default: POST) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `notifications update`

Update a notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-n, --name <name>` | New provider name | - | No |
| `--enabled <enabled>` | Enable or disable (true/false) | - | No |
| `-w, --webhook-url <url>` | Webhook URL (for slack) | - | No |
| `-c, --channel <channel>` | Channel name (for slack) | - | No |
| `--smtp-host <host>` | SMTP host (for email) | - | No |
| `--smtp-port <port>` | SMTP port (for email) | - | No |
| `--username <username>` | SMTP username (for email) | - | No |
| `--password <password>` | SMTP password (for email) | - | No |
| `--from-address <address>` | From email address (for email) | - | No |
| `--from-name <name>` | From display name (for email) | - | No |
| `--to-addresses <addresses>` | Comma-separated recipient addresses (for email) | - | No |
| `--url <url>` | Webhook URL (for webhook) | - | No |
| `--method <method>` | HTTP method: POST, PUT, PATCH (for webhook) | - | No |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip confirmation prompts | - | No |

### `notifications enable`

Enable a notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `notifications disable`

Disable a notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `notifications show`

Show notification provider details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `notifications remove` (alias: `rm`)

Remove a notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `notifications test`

Send a test notification

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |

### `notifications routes`

Manage severity-based notification routes (which providers receive which severities)

**Subcommands:**

- `list` (`ls`) - List notification routes
- `show` - Show notification route details
- `create` - Create a notification route
- `update` - Update a notification route
- `remove` (`rm`) - Remove a notification route

#### `notifications routes list` (alias: `ls`)

List notification routes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `notifications routes show`

Show notification route details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Route ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `notifications routes create`

Create a notification route

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Route name | - | No |
| `--min-severity <severity>` | Minimum severity: debug, info, warning, error, critical, emergency | - | No |
| `--max-severity <severity>` | Maximum severity: debug, info, warning, error, critical, emergency | - | No |
| `--provider-ids <ids>` | Comma-separated notification provider IDs | - | No |
| `--enabled <enabled>` | Enable or disable (true/false, default: true) | - | No |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

#### `notifications routes update`

Update a notification route

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Route ID | - | Yes |
| `-n, --name <name>` | New route name | - | No |
| `--min-severity <severity>` | Minimum severity: debug, info, warning, error, critical, emergency | - | No |
| `--max-severity <severity>` | Maximum severity: debug, info, warning, error, critical, emergency | - | No |
| `--provider-ids <ids>` | Comma-separated notification provider IDs (replaces the current set) | - | No |
| `--enabled <enabled>` | Enable or disable (true/false) | - | No |
| `--json` | Output in JSON format | - | No |

#### `notifications routes remove` (alias: `rm`)

Remove a notification route

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Route ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

## `dns`

Manage DNS providers for automated domain verification

**Subcommands:**

- `list` (`ls`) - List configured DNS providers
- `add` - Add a new DNS provider
- `show` - Show DNS provider details
- `remove` (`rm`) - Remove a DNS provider
- `test` - Test DNS provider connection
- `zones` - List available zones in a DNS provider

### `dns list` (alias: `ls`)

List configured DNS providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `dns add`

Add a new DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-t, --type <type>` | Provider type (cloudflare, route53, digitalocean, namecheap, gcp, azure, manual) | - | No |
| `-n, --name <name>` | Provider name | - | No |
| `-d, --description <description>` | Provider description | - | No |
| `--api-token <token>` | Cloudflare API token | - | No |
| `--account-id <id>` | Cloudflare account ID (optional) | - | No |
| `--access-key-id <key>` | AWS access key ID | - | No |
| `--secret-access-key <secret>` | AWS secret access key | - | No |
| `--region <region>` | AWS region | - | No |
| `--api-user <user>` | Namecheap API user | - | No |
| `--api-key <key>` | Namecheap API key | - | No |
| `--username <username>` | Namecheap username | - | No |
| `--client-ip <ip>` | Namecheap whitelisted client IP | - | No |
| `--project-id <id>` | GCP project ID | - | No |
| `--service-account-email <email>` | GCP service account email | - | No |
| `--private-key-id <id>` | GCP private key ID | - | No |
| `--private-key <key>` | GCP private key | - | No |
| `--tenant-id <id>` | Azure tenant ID | - | No |
| `--client-id <id>` | Azure client ID | - | No |
| `--client-secret <secret>` | Azure client secret | - | No |
| `--subscription-id <id>` | Azure subscription ID | - | No |
| `--resource-group <name>` | Azure resource group | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `dns show`

Show DNS provider details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `dns remove` (alias: `rm`)

Remove a DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `dns test`

Test DNS provider connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |

### `dns zones`

List available zones in a DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

## `services` (alias: `svc`)

Manage external services (databases, caches, storage)

**Subcommands:**

- `list` (`ls`) - List all external services
- `create` (`add`) - Create a new external service
- `show` - Show service details
- `remove` (`rm`) - Remove a service
- `start` - Start a stopped service
- `stop` - Stop a running service
- `types` - List available service types
- `projects` - List projects linked to a service
- `update` - Update a service
- `upgrade` - Upgrade a service to a newer version
- `import` - Import an existing external service
- `link` - Link a service to a project
- `unlink` - Unlink a service from a project
- `connect` - Get connection info for a service by name or slug
- `env` - Show environment variables for a linked service
- `env-var` - Get a specific environment variable
- `logs` - View persisted logs for an external service
- `slow-queries` - Show slowest PostgreSQL queries from pg_stat_statements
- `enable-pg-stat-statements` - Enable pg_stat_statements on a standalone Postgres service by restarting its container (drops active connections briefly)
- `metrics` - Resource and engine metrics for a database/cache/storage service
- `restore-capabilities` - Show what restore modes a service supports (in-place / new service / PITR)
- `list-backups` - List backups stored on an S3 source
- `restore` - Restore a service from a backup (in-place, new service, or PITR)
- `restore-runs` - List recent restore runs for a service
- `restore-run` - Show a single restore run

### `services list` (alias: `ls`)

List all external services

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services create` (alias: `add`)

Create a new external service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-t, --type <type>` | Service type (postgres, mongodb, redis, s3) | - | No |
| `-n, --name <name>` | Service name | - | No |
| `-s, --set <key=value>` | Set a parameter (repeatable) | `` | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `services show`

Show service details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services remove` (alias: `rm`)

Remove a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `services start`

Start a stopped service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |

### `services stop`

Stop a running service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |

### `services types`

List available service types

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

**Subcommands:**

- `info` - Show parameters schema for a service type (useful for automation)

#### `services types info`

Show parameters schema for a service type (useful for automation)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as raw JSON schema (default) | - | No |

### `services projects`

List projects linked to a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services update`

Update a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-n, --name <name>` | Docker image name (e.g., postgres:18-alpine) | - | No |
| `-s, --set <key=value>` | Set a parameter (repeatable) | `` | No |

### `services upgrade`

Upgrade a service to a newer version

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-v, --version <version>` | Docker image to upgrade to (e.g., postgres:18-alpine) | - | No |

### `services import`

Import an existing external service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-t, --type <type>` | Service type (postgres, mongodb, redis, s3) | - | No |
| `-n, --name <name>` | Service name | - | No |
| `--container-id <id>` | Container ID or name to import | - | No |
| `-s, --set <key=value>` | Set a parameter (repeatable) | `` | No |
| `--version <version>` | Optional version override | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `services link`

Link a service to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json) | - | No |

### `services unlink`

Unlink a service from a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json) | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `services connect`

Get connection info for a service by name or slug

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json) | - | No |
| `--json` | Output in JSON format | - | No |

### `services env`

Show environment variables for a linked service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json) | - | No |
| `--json` | Output in JSON format | - | No |

### `services env-var`

Get a specific environment variable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json) | - | No |
| `--var <name>` | Environment variable name | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services logs`

View persisted logs for an external service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--from <datetime>` | Start of time range. ISO 8601 timestamp or a relative duration like "1h", "24h", "7d" (default: 24h ago) | - | No |
| `--to <datetime>` | End of time range. ISO 8601 timestamp (default: now) | - | No |
| `-l, --level <levels>` | Comma-separated log levels to include: ERROR,WARN,INFO,DEBUG,TRACE | - | No |
| `-n, --tail <lines>` | Maximum number of log lines to fetch (default: 200, max: 1000) | `200` | No |
| `-t, --text <query>` | Filter log lines by text (case-insensitive) | - | No |
| `--json` | Output raw JSON instead of formatted lines | - | No |

### `services slow-queries`

Show slowest PostgreSQL queries from pg_stat_statements

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--page <n>` | Page number (1-based, default: 1) | `1` | No |
| `--page-size <n>` | Rows per page (1–100, default: 20) | `20` | No |
| `--sort-by <column>` | Sort column: calls, total_exec_time_ms, mean_exec_time_ms, rows, cache_hit_ratio (default: mean_exec_time_ms) | - | No |
| `--sort-order <order>` | Sort direction: asc or desc (default: desc) | - | No |
| `--json` | Output raw JSON instead of a formatted table | - | No |

### `services enable-pg-stat-statements`

Enable pg_stat_statements on a standalone Postgres service by restarting its container (drops active connections briefly)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-y, --yes` | Skip the restart confirmation prompt (for automation) | - | No |

### `services metrics`

Resource and engine metrics for a database/cache/storage service

**Subcommands:**

- `latest` - Show the most recent value of every tracked metric
- `range` - Show a time-series range for a single metric
- `status` - Show when metrics were last received for a service
- `by-database` - Per-database metric breakdown (PostgreSQL services only)
- `enable` - Enable metric collection for a service (seeds default alert rules)
- `disable` - Disable metric collection for a service
- `alert-rules` - Manage monitoring alert rules for a service

#### `services metrics latest`

Show the most recent value of every tracked metric

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `services metrics range`

Show a time-series range for a single metric

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-m, --metric <name>` | Metric name, e.g. "pg.connections_active" | - | Yes |
| `-r, --range <window>` | Time window: 1h, 6h, 24h, 7d (default: 24h) | - | No |
| `-p, --percentile <n>` | Histogram percentile (0-100) instead of a plain average | - | No |
| `--json` | Output raw JSON instead of a formatted table | - | No |

#### `services metrics status`

Show when metrics were last received for a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `services metrics by-database`

Per-database metric breakdown (PostgreSQL services only)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `services metrics enable`

Enable metric collection for a service (seeds default alert rules)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |

#### `services metrics disable`

Disable metric collection for a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |

#### `services metrics alert-rules`

Manage monitoring alert rules for a service

**Subcommands:**

- `list` (`ls`) - List alert rules for a service
- `create` (`add`) - Create an alert rule for a service
- `update` - Update an existing alert rule
- `remove` (`rm`) - Delete an alert rule

##### `services metrics alert-rules list` (alias: `ls`)

List alert rules for a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

##### `services metrics alert-rules create` (alias: `add`)

Create an alert rule for a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-n, --name <name>` | Alert rule name | - | Yes |
| `-m, --metric <name>` | Metric name, e.g. "pg.connections_active" | - | Yes |
| `-c, --comparator <op>` | Comparator: >, <, >=, <= | - | Yes |
| `-t, --threshold <n>` | Threshold value that triggers the alert | - | Yes |
| `-s, --severity <level>` | warning or critical (default: warning) | - | No |
| `--for-duration <secs>` | Seconds the breach must persist before firing (default: 0) | - | No |
| `--disabled` | Create the rule disabled | - | No |
| `--json` | Output in JSON format | - | No |

##### `services metrics alert-rules update`

Update an existing alert rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--rule-id <id>` | Alert rule ID | - | Yes |
| `-n, --name <name>` | Alert rule name | - | No |
| `-m, --metric <name>` | Metric name | - | No |
| `-c, --comparator <op>` | Comparator: >, <, >=, <= | - | No |
| `-t, --threshold <n>` | Threshold value | - | No |
| `-s, --severity <level>` | warning or critical | - | No |
| `--for-duration <secs>` | Seconds the breach must persist before firing | - | No |
| `--enable` | Enable the rule | - | No |
| `--disable` | Disable the rule | - | No |
| `--json` | Output in JSON format | - | No |

##### `services metrics alert-rules remove` (alias: `rm`)

Delete an alert rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--rule-id <id>` | Alert rule ID | - | Yes |
| `-y, --yes` | Skip confirmation prompt | - | No |

### `services restore-capabilities`

Show what restore modes a service supports (in-place / new service / PITR)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services list-backups`

List backups stored on an S3 source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--s3-source-id <id>` | S3 source ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services restore`

Restore a service from a backup (in-place, new service, or PITR)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Source service ID (the service the backup came from) | - | Yes |
| `--backup-id <id>` | Backup ID to restore from (see `list-backups`) | - | Yes |
| `--new-service [name]` | Clone into a new service. Omit the value or pass "auto" to accept the auto-suggested name. | - | No |
| `--pitr <iso>` | Point-in-time recovery target, ISO 8601 timestamp (requires WAL-G backup). Combine with --new-service to route PITR into a new service. | - | No |
| `-y, --yes` | Skip confirmation | - | No |
| `--no-wait` | Return immediately without polling run status | - | No |
| `--json` | Output in JSON format | - | No |

### `services restore-runs`

List recent restore runs for a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services restore-run`

Show a single restore run

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Restore run ID | - | Yes |
| `--json` | Output in JSON format | - | No |

## `settings`

Manage platform settings

**Subcommands:**

- `show` (`get`) - Show current platform settings
- `update` (`set`) - Update platform settings
- `set-external-url` - Set the external URL for the platform
- `set-preview-domain` - Set the preview domain pattern

### `settings show` (alias: `get`)

Show current platform settings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `settings update` (alias: `set`)

Update platform settings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-s, --setting <setting>` | Setting to update (external_url, preview_domain, letsencrypt, rate_limiting, security_headers, screenshots) | - | No |
| `-v, --value <value>` | Value for the setting | - | No |
| `--external-url <url>` | External URL for the platform | - | No |
| `--preview-domain <domain>` | Preview domain pattern | - | No |
| `--letsencrypt-email <email>` | Let's Encrypt email | - | No |
| `--letsencrypt-mode <mode>` | Let's Encrypt mode (staging, production) | - | No |
| `--rate-limiting-enabled <enabled>` | Enable rate limiting (true/false) | - | No |
| `--rate-limiting-rpm <rpm>` | Requests per minute | - | No |
| `--screenshots-enabled <enabled>` | Enable screenshots (true/false) | - | No |
| `--max-request-timeout <seconds>` | Hard ceiling for all upstream request/idle timeouts, in seconds | - | No |
| `--default-http-timeout <seconds>` | Default timeout for regular HTTP requests, in seconds | - | No |
| `--default-sse-idle-timeout <seconds>` | Default idle timeout for SSE streams, in seconds | - | No |
| `--default-websocket-idle-timeout <seconds>` | Default idle timeout for WebSocket connections, in seconds | - | No |
| `--max-memory-limit-mb <mb>` | Ceiling on a project/environment memory limit override, in MB (0 = no ceiling) | - | No |
| `--max-concurrent-connections-ceiling <count>` | Ceiling on a project/environment concurrent-connection override (0 = no ceiling) | - | No |
| `--allow-unlimited-timeouts <enabled>` | Whether projects may set a timeout of 0, i.e. no timeout (true/false) | - | No |
| `--console-force-https <mode>` | Redirect the console host to HTTPS: auto (once a cert exists), always, or never | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `settings set-external-url`

Set the external URL for the platform

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--url <url>` | External URL | - | Yes |

### `settings set-preview-domain`

Set the preview domain pattern

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--domain <domain>` | Preview domain pattern | - | Yes |

## `users`

Manage platform users

**Subcommands:**

- `list` (`ls`) - List all users
- `create` (`add`) - Create a new user
- `me` - Show current user info
- `remove` (`rm`) - Remove a user
- `restore` - Restore a deleted user
- `role` - Manage user roles

### `users list` (alias: `ls`)

List all users

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `users create` (alias: `add`)

Create a new user

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --username <username>` | Username | - | No |
| `-e, --email <email>` | Email address | - | No |
| `-p, --password <password>` | Password (if not provided, invite email will be sent) | - | No |
| `-r, --roles <roles>` | Comma-separated roles (admin, user) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `users me`

Show current user info

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `users remove` (alias: `rm`)

Remove a user

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | User ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `users restore`

Restore a deleted user

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | User ID | - | Yes |

### `users role`

Manage user roles

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | User ID | - | Yes |
| `--add <role>` | Add a role to user | - | No |
| `--remove <role>` | Remove a role from user | - | No |

## `teams`

Manage teams and project access

**Subcommands:**

- `list` (`ls`) - List all teams
- `create` (`add`) - Create a new team
- `show` - Show a team with its members and projects
- `update` - Update a team name or description
- `delete` (`rm`) - Delete a team (removes its members and project grants)
- `members` - Manage team membership

### `teams list` (alias: `ls`)

List all teams

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `teams create` (alias: `add`)

Create a new team

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Team name | - | No |
| `-s, --slug <slug>` | URL-safe slug ([a-z0-9-]+) | - | No |
| `-d, --description <description>` | Team description | - | No |
| `--json` | Output in JSON format | - | No |

### `teams show`

Show a team with its members and projects

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `teams update`

Update a team name or description

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | New team name | - | No |
| `-d, --description <description>` | New description | - | No |
| `--json` | Output in JSON format | - | No |

### `teams delete` (alias: `rm`)

Delete a team (removes its members and project grants)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip confirmation | - | No |

### `teams members`

Manage team membership

**Subcommands:**

- `list` (`ls`) - List a team's members
- `add` - Add a user to a team
- `set-role` - Change a member's role in the team
- `remove` (`rm`) - Remove a user from a team

#### `teams members list` (alias: `ls`)

List a team's members

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `teams members add`

Add a user to a team

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --user <user>` | User id or email | - | No |
| `-r, --role <role>` | Team role (owner\|admin\|deployer\|viewer) | - | No |
| `--json` | Output in JSON format | - | No |

#### `teams members set-role`

Change a member's role in the team

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --user <user>` | User id or email | - | No |
| `-r, --role <role>` | Team role (owner\|admin\|deployer\|viewer) | - | No |
| `--json` | Output in JSON format | - | No |

#### `teams members remove` (alias: `rm`)

Remove a user from a team

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --user <user>` | User id or email | - | No |
| `-y, --yes` | Skip confirmation | - | No |

## `access`

Manage which teams can reach a project

**Subcommands:**

- `list` (`ls`) - List the teams granted access to a project
- `grant` - Grant a team access to a project
- `revoke` - Revoke a team's access to a project

### `access list` (alias: `ls`)

List the teams granted access to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `access grant`

Grant a team access to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-r, --role <role>` | Role the team holds on the project (owner\|admin\|deployer\|viewer) | - | No |
| `--json` | Output in JSON format | - | No |

### `access revoke`

Revoke a team's access to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-y, --yes` | Skip confirmation | - | No |

## `apikeys` (alias: `keys`)

Manage API keys for programmatic access

**Subcommands:**

- `list` (`ls`) - List all API keys
- `create` (`add`) - Create a new API key
- `show` - Show API key details
- `remove` (`rm`) - Delete an API key
- `activate` - Activate a deactivated API key
- `deactivate` - Deactivate an API key
- `permissions` - List available API key permissions

### `apikeys list` (alias: `ls`)

List all API keys

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `apikeys create` (alias: `add`)

Create a new API key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | API key name | - | No |
| `-r, --role <role>` | Role type (admin, platform_admin, user, reader, api_reader, custom, metrics_ingest) | - | No |
| `-e, --expires-in <days>` | Expires in N days (7, 30, 90, 365) | - | No |
| `-p, --permissions <permissions>` | Comma-separated list of permissions | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `apikeys show`

Show API key details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | API key ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `apikeys remove` (alias: `rm`)

Delete an API key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | API key ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `apikeys activate`

Activate a deactivated API key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | API key ID | - | Yes |

### `apikeys deactivate`

Deactivate an API key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | API key ID | - | Yes |

### `apikeys permissions`

List available API key permissions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `monitors` (alias: `monitoring`)

Manage uptime monitors for status pages

**Subcommands:**

- `list` (`ls`) - List all monitors for a project
- `create` (`add`) - Create a new monitor for a project
- `show` - Show monitor details and current status
- `remove` (`rm`) - Delete a monitor
- `status` - Get current status — all monitors for a project, or a single monitor by ID
- `history` - Get monitor uptime history

### `monitors list` (alias: `ls`)

List all monitors for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `monitors create` (alias: `add`)

Create a new monitor for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-n, --name <name>` | Monitor name | - | No |
| `-t, --type <type>` | Monitor type (http, tcp, ping) | - | No |
| `-i, --interval <seconds>` | Check interval in seconds (60, 300, 600, 900, 1800) | - | No |
| `--check-path <path>` | HTTP health-check path (must start with "/", e.g. /api/healthz). Defaults to "/" for HTTP monitors. | - | No |
| `--environment-id <id>` | Environment ID (default: 0 for production) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `monitors show`

Show monitor details and current status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Monitor ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `monitors remove` (alias: `rm`)

Delete a monitor

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Monitor ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `monitors status`

Get current status — all monitors for a project, or a single monitor by ID

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Monitor ID (omit to show all monitors for the project) | - | No |
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json or TEMPS_PROJECT) | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors history`

Get monitor uptime history

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Monitor ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--days <days>` | Number of days to show | `7` | No |

## `webhooks` (alias: `hooks`)

Manage webhooks for project events

**Subcommands:**

- `list` (`ls`) - List all webhooks for a project
- `create` (`add`) - Create a new webhook for a project
- `show` - Show webhook details
- `update` - Update a webhook
- `remove` (`rm`) - Delete a webhook
- `enable` - Enable a webhook
- `disable` - Disable a webhook
- `events` - List available webhook event types
- `deliveries` - Manage webhook deliveries

### `webhooks list` (alias: `ls`)

List all webhooks for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `webhooks create` (alias: `add`)

Create a new webhook for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-u, --url <url>` | Webhook URL | - | No |
| `-e, --events <events>` | Comma-separated event types (or "all" for all events) | - | No |
| `-s, --secret <secret>` | Webhook secret for signature verification | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `webhooks show`

Show webhook details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--webhook-id <id>` | Webhook ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `webhooks update`

Update a webhook

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--webhook-id <id>` | Webhook ID | - | Yes |
| `-u, --url <url>` | New webhook URL | - | No |
| `-e, --events <events>` | Comma-separated event types (or "all" for all events) | - | No |
| `-s, --secret <secret>` | New webhook secret for signature verification | - | No |

### `webhooks remove` (alias: `rm`)

Delete a webhook

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--webhook-id <id>` | Webhook ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `webhooks enable`

Enable a webhook

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--webhook-id <id>` | Webhook ID | - | Yes |

### `webhooks disable`

Disable a webhook

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--webhook-id <id>` | Webhook ID | - | Yes |

### `webhooks events`

List available webhook event types

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `webhooks deliveries`

Manage webhook deliveries

**Subcommands:**

- `list` (`ls`) - List deliveries for a webhook
- `show` - Show delivery details
- `retry` - Retry a failed delivery

#### `webhooks deliveries list` (alias: `ls`)

List deliveries for a webhook

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--webhook-id <id>` | Webhook ID | - | Yes |
| `--limit <n>` | Number of deliveries to return (default: 50) | - | No |
| `--json` | Output in JSON format | - | No |

#### `webhooks deliveries show`

Show delivery details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--webhook-id <id>` | Webhook ID | - | Yes |
| `--delivery-id <id>` | Delivery ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `webhooks deliveries retry`

Retry a failed delivery

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--webhook-id <id>` | Webhook ID | - | Yes |
| `--delivery-id <id>` | Delivery ID | - | Yes |

## `containers` (alias: `cts`)

Manage project containers in environments

**Subcommands:**

- `list` (`ls`) - List containers in an environment, or across all environments if -e omitted
- `show` - Show container details
- `start` - Start a stopped container
- `stop` - Stop a running container
- `restart` - Restart a container
- `history` - List containers that have run in an environment, including ones replaced by a later redeploy; every currently-running container is always included
- `metrics` - Get container resource metrics (all containers if no container ID specified)

### `containers list` (alias: `ls`)

List containers in an environment, or across all environments if -e omitted

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID (optional - lists all environments if omitted) | - | No |
| `--json` | Output in JSON format | - | No |

### `containers show`

Show container details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-c, --container-id <id>` | Container ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `containers start`

Start a stopped container

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-c, --container-id <id>` | Container ID | - | Yes |

### `containers stop`

Stop a running container

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-c, --container-id <id>` | Container ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |

### `containers restart`

Restart a container

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-c, --container-id <id>` | Container ID | - | Yes |

### `containers history`

List containers that have run in an environment, including ones replaced by a later redeploy; every currently-running container is always included

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-d, --deployment-id <id>` | Only list containers belonging to this deployment | - | No |
| `-l, --limit <count>` | Max REPLACED container rows to return on top of the running ones, newest first (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `containers metrics`

Get container resource metrics (all containers if no container ID specified)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-c, --container-id <id>` | Container ID (optional - shows all if not specified) | - | No |
| `--json` | Output in JSON format | - | No |
| `-w, --watch` | Watch mode - continuously update metrics | - | No |
| `-i, --interval <seconds>` | Refresh interval in seconds (default: 2) | `2` | No |

## `cluster`

Cluster-wide multi-node operations

**Subcommands:**

- `dns` - Cluster DNS resolver (ADR-024) operations

### `cluster dns`

Cluster DNS resolver (ADR-024) operations

**Subcommands:**

- `status` - Show whether cluster DNS is healthy across every node — resolver status, last sync, and errors — without SSHing into a node to read logs

#### `cluster dns status`

Show whether cluster DNS is healthy across every node — resolver status, last sync, and errors — without SSHing into a node to read logs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `tokens` (alias: `token`)

Manage deployment tokens for project API access (KV, Blob, etc.)

**Subcommands:**

- `list` (`ls`) - List deployment tokens for a project
- `create` (`add`) - Create a new deployment token
- `show` (`get`) - Show deployment token details
- `delete` (`rm`) - Delete a deployment token
- `permissions` - List available deployment token permissions

### `tokens list` (alias: `ls`)

List deployment tokens for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `tokens create` (alias: `add`)

Create a new deployment token

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-n, --name <name>` | Token name | - | No |
| `--permissions <permissions>` | Comma-separated permissions (e.g., "visitors:enrich,emails:send" or "*" for full access) | - | No |
| `-e, --expires-in <days>` | Expires in N days (7, 30, 90, 365, or "never") | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `tokens show` (alias: `get`)

Show deployment token details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--id <id>` | Token ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `tokens delete` (alias: `rm`)

Delete a deployment token

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--id <id>` | Token ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `tokens permissions`

List available deployment token permissions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `errors` (alias: `error`)

Manage error tracking and error groups

**Subcommands:**

- `list` (`ls`) - List error groups for a project
- `show` - Show error group details
- `update` - Update error group status
- `events` - List events in an error group
- `event` - Show a specific error event
- `stats` - Get error statistics for a project
- `timeline` - Get error time series data
- `dashboard` - Get error dashboard statistics
- `sourcemaps` (`sm`) - Manage source maps for error symbolication
- `source-files` (`sf`) - Manage raw source files for native (Go/Rust/…) symbolication

### `errors list` (alias: `ls`)

List error groups for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--status <status>` | Filter by status (unresolved, resolved, ignored) | - | No |
| `--page <page>` | Page number | - | No |
| `--page-size <size>` | Page size | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--start-date <date>` | Filter by start date (ISO 8601) | - | No |
| `--end-date <date>` | Filter by end date (ISO 8601) | - | No |
| `--sort-by <field>` | Sort by field (e.g., total_count, last_seen, first_seen) | - | No |
| `--sort-order <order>` | Sort order: asc or desc | - | No |
| `--json` | Output in JSON format | - | No |

### `errors show`

Show error group details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `errors update`

Update error group status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--status <status>` | New status (unresolved, resolved, ignored) | - | Yes |
| `--assigned-to <user>` | Assign to user | - | No |

### `errors events`

List events in an error group

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--page <page>` | Page number | - | No |
| `--page-size <size>` | Page size | - | No |
| `--json` | Output in JSON format | - | No |

### `errors event`

Show a specific error event

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--event-id <id>` | Error event ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `errors stats`

Get error statistics for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `errors timeline`

Get error time series data

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--days <days>` | Number of days to show | `7` | No |
| `--bucket <bucket>` | Time bucket size (e.g., "1h", "15m", "1d") | `1h` | No |
| `--environment-id <id>` | Filter chart data to a specific environment ID | - | No |
| `--json` | Output in JSON format | - | No |

### `errors dashboard`

Get error dashboard statistics

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--days <days>` | Number of days to show | `7` | No |
| `--compare` | Compare to previous period | - | No |
| `--json` | Output in JSON format | - | No |

### `errors sourcemaps` (alias: `sm`)

Manage source maps for error symbolication

**Subcommands:**

- `upload` - Upload a source map file for a release
- `list` (`ls`) - List source maps for a release
- `releases` - List all releases that have source maps
- `delete` - Delete all source maps for a release
- `delete-one` - Delete a specific source map by ID

#### `errors sourcemaps upload`

Upload a source map file for a release

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--release <version>` | Release version (e.g. commit SHA) | - | Yes |
| `--file <path>` | Path to the .map file | - | Yes |
| `--file-path <urlpath>` | URL path in stack traces (e.g. ~/assets/main.js) | - | No |
| `--dist <dist>` | Distribution identifier | - | No |

#### `errors sourcemaps list` (alias: `ls`)

List source maps for a release

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--release <version>` | Release version | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `errors sourcemaps releases`

List all releases that have source maps

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `errors sourcemaps delete`

Delete all source maps for a release

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--release <version>` | Release version | - | Yes |

#### `errors sourcemaps delete-one`

Delete a specific source map by ID

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--source-map-id <id>` | Source map ID | - | Yes |

### `errors source-files` (alias: `sf`)

Manage raw source files for native (Go/Rust/…) symbolication

**Subcommands:**

- `upload` - Upload source file(s) for a release (single --file or a --dir tree)
- `list` (`ls`) - List uploaded source files for a release
- `delete` - Delete all source files for a release

#### `errors source-files upload`

Upload source file(s) for a release (single --file or a --dir tree)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--release <version>` | Release version (must match the app's SENTRY_RELEASE, e.g. the deployed commit SHA) | - | Yes |
| `--file <path>` | Path to a single source file | - | No |
| `--file-path <path>` | Path as it appears in stack frames (e.g. internal/gateway/main.go); defaults to the file name | - | No |
| `--dir <root>` | Upload every source file under this directory, recursively | - | No |
| `--ext <csv>` | Comma-separated extensions to include with --dir (default: go,rs,py,rb,js,jsx,ts,tsx,java,kt,c,h,cpp,cc,hpp,cs,php,swift,scala,ex,exs) | - | No |

#### `errors source-files list` (alias: `ls`)

List uploaded source files for a release

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--release <version>` | Release version | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `errors source-files delete`

Delete all source files for a release

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--release <version>` | Release version | - | Yes |

## `metrics` (alias: `metric`)

Query OTel application metrics for debugging (not container/docker stats — see "temps containers metrics" for those)

**Subcommands:**

- `names` - List distinct metric names ingested for a project — start here if you don't know what to query
- `query` - Query a metric with time bucketing and aggregation
- `label-keys` - List the label keys observed on a metric — powers filter/group-by discovery
- `label-values` - List the distinct values seen for a label key on a metric

### `metrics names`

List distinct metric names ingested for a project — start here if you don't know what to query

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `metrics query`

Query a metric with time bucketing and aggregation

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--metric-name <name>` | Metric to query (see "temps metrics names") | - | No |
| `--service-name <name>` | Filter by service name | - | No |
| `--environment <name>` | Filter by deployment environment name | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 24h, 7d) | `24h` | No |
| `--start-time <iso>` | Explicit window start (RFC 3339) — overrides --period | - | No |
| `--end-time <iso>` | Explicit window end (RFC 3339) — overrides --period | - | No |
| `--bucket-interval <interval>` | Bucket size, e.g. "5 minutes", "1 hour" | - | No |
| `--aggregation <mode>` | Per-bucket aggregation: avg (default), sum, min, max, count, rate, p50/p95/p99, quantile:0.95 | - | No |
| `--metric-type <type>` | Filter by metric type: gauge, sum, histogram, exponential_histogram, summary | - | No |
| `--label-filters <pairs>` | Comma-separated key=value data-point label filters, e.g. http.method=GET,http.status_code=200 | - | No |
| `--group-by <keys>` | Comma-separated label keys to group series by, e.g. http.method,http.route | - | No |
| `--limit <n>` | Max buckets to return (default: 500, server cap: 1000) | - | No |
| `--json` | Output in JSON format | - | No |

### `metrics label-keys`

List the label keys observed on a metric — powers filter/group-by discovery

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--metric-name <name>` | Metric to inspect | - | Yes |
| `--start-time <iso>` | Window start (RFC 3339); defaults to 24h before end | - | No |
| `--end-time <iso>` | Window end (RFC 3339); defaults to now | - | No |
| `--json` | Output in JSON format | - | No |

### `metrics label-values`

List the distinct values seen for a label key on a metric

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--metric-name <name>` | Metric to inspect | - | Yes |
| `--label-key <key>` | Label key whose values to list | - | Yes |
| `--start-time <iso>` | Window start (RFC 3339); defaults to 24h before end | - | No |
| `--end-time <iso>` | Window end (RFC 3339); defaults to now | - | No |
| `--json` | Output in JSON format | - | No |

## `traces` (alias: `trace`)

Inspect distributed traces and operation latency

**Subcommands:**

- `span-stats` (`operations`, `ops`) - Rank operations by time spent, latency percentiles, or inconsistency

### `traces span-stats` (alias: `operations`, `ops`)

Rank operations by time spent, latency percentiles, or inconsistency

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | No |
| `--project-ids <ids>` | Comma-separated project IDs to rank across, e.g. 4,5,6 (max 50) | - | No |
| `--since <duration>` | Relative window: 30m, 24h, 7d (max 31d) | `24h` | No |
| `--start-time <iso>` | Window start (ISO 8601); overrides --since | - | No |
| `--end-time <iso>` | Window end (ISO 8601); defaults to now | - | No |
| `--service <name>` | Only this service | - | No |
| `--operation <name>` | Only this operation (exact span name) | - | No |
| `--search <text>` | Only operations whose name contains this text | - | No |
| `--kind <kind>` | Span kind (server, client, internal, producer, consumer, unspecified) | - | No |
| `--status <status>` | Span status (ok, error, unset) | - | No |
| `--environment-id <id>` | Only this environment | - | No |
| `--deployment-id <id>` | Only this deployment | - | No |
| `--attributes <pairs>` | Span attribute filters, e.g. db.system=postgresql | - | No |
| `--min-duration-ms <ms>` | Ignore spans faster than this | - | No |
| `--min-count <n>` | Drop operations with fewer samples than this | - | No |
| `--sort-by <field>` | Ranking (total_time, p50, p95, p99, max, avg, stddev, count, errors, error_rate, variability, tail_ratio) | `total_time` | No |
| `--sort-order <order>` | asc or desc | `desc` | No |
| `--limit <n>` | Rows to show (max 100) | `20` | No |
| `--offset <n>` | Page offset | - | No |
| `--json` | Output in JSON format | - | No |

## `facets`

Manage OTel span attribute facets — attribute keys promoted to a fast-filterable column (ClickHouse or TimescaleDB, whichever backend is active; see ADR-039). Facets are platform-global, not per-project, since the underlying spans table is shared across every project. Historical backfill runs asynchronously — check `temps facets list` for status.

**Subcommands:**

- `list` (`ls`) - List registered span attribute facets
- `create` - Register an attribute key as a facet, making it fast to filter on across all traces. Backfills existing spans that carry the attribute. Capped at 20 facets platform-wide.
- `remove` (`rm`) - Remove a registered facet, freeing its slot for reuse
- `retry` - Retry a failed historical backfill. Only valid when the facet's status is "failed" — resets progress and lets the background poller re-attempt from the beginning.

### `facets list` (alias: `ls`)

List registered span attribute facets

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `facets create`

Register an attribute key as a facet, making it fast to filter on across all traces. Backfills existing spans that carry the attribute. Capped at 20 facets platform-wide.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `facets remove` (alias: `rm`)

Remove a registered facet, freeing its slot for reuse

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `facets retry`

Retry a failed historical backfill. Only valid when the facet's status is "failed" — resets progress and lets the background poller re-attempt from the beginning.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `otel-forward`

Manage OTel forwarding destinations that relay ingested traces, metrics, and logs to an external OTLP-compatible collector

**Subcommands:**

- `list` (`ls`) - List OTel forwarding destinations for a project
- `create` - Create a new OTel forwarding destination
- `show` - Show OTel forwarding destination details
- `update` - Update an OTel forwarding destination
- `remove` - Remove an OTel forwarding destination
- `test` - Send a test delivery to an OTel forwarding destination
- `instance-default` - Manage instance-wide default forwarding destinations — applied automatically to any project with zero enabled destinations of its own. As soon as a project has one of its own destinations, instance defaults stop applying to that project.

### `otel-forward list` (alias: `ls`)

List OTel forwarding destinations for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `otel-forward create`

Create a new OTel forwarding destination

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--name <name>` | Destination name | - | Yes |
| `--vendor <preset>` | Vendor preset (datadog, honeycomb, new_relic, grafana_cloud, generic_otlp) | - | Yes |
| `--endpoint-url <url>` | OTLP-compatible collector endpoint URL | - | Yes |
| `--header-env <k=env>` | HTTP header sourced from an environment variable (repeatable) | `` | No |
| `--traces` | Forward traces (default: true) | - | No |
| `--no-traces` | Do not forward traces | - | No |
| `--metrics` | Forward metrics (default: true) | - | No |
| `--no-metrics` | Do not forward metrics | - | No |
| `--logs` | Forward logs (default: true) | - | No |
| `--no-logs` | Do not forward logs | - | No |
| `--enabled` | Create the destination enabled (default) | - | No |
| `--disabled` | Create the destination disabled | - | No |
| `--allow-private-network` | Allow the endpoint URL to resolve to private/loopback/link-local IPs | - | No |
| `--json` | Output in JSON format | - | No |

### `otel-forward show`

Show OTel forwarding destination details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `otel-forward update`

Update an OTel forwarding destination

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | Destination name | - | No |
| `--vendor <preset>` | Vendor preset (datadog, honeycomb, new_relic, grafana_cloud, generic_otlp) | - | No |
| `--endpoint-url <url>` | OTLP-compatible collector endpoint URL | - | No |
| `--header-env <k=env>` | HTTP header sourced from an environment variable (repeatable) | `` | No |
| `--traces` | Forward traces | - | No |
| `--no-traces` | Do not forward traces | - | No |
| `--metrics` | Forward metrics | - | No |
| `--no-metrics` | Do not forward metrics | - | No |
| `--logs` | Forward logs | - | No |
| `--no-logs` | Do not forward logs | - | No |
| `--enabled` | Enable the destination | - | No |
| `--disabled` | Disable the destination | - | No |
| `--allow-private-network` | Allow the endpoint URL to resolve to private/loopback/link-local IPs | - | No |
| `--no-allow-private-network` | Disallow private/loopback/link-local endpoint URLs | - | No |
| `--json` | Output in JSON format | - | No |

### `otel-forward remove`

Remove an OTel forwarding destination

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `otel-forward test`

Send a test delivery to an OTel forwarding destination

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `otel-forward instance-default`

Manage instance-wide default forwarding destinations — applied automatically to any project with zero enabled destinations of its own. As soon as a project has one of its own destinations, instance defaults stop applying to that project.

**Subcommands:**

- `list` (`ls`) - List instance-wide default forwarding destinations
- `create` - Create a new instance-wide default forwarding destination
- `show` - Show instance default destination details
- `update` - Update an instance default forwarding destination
- `remove` - Remove an instance default forwarding destination
- `test` - Send a test delivery to an instance default forwarding destination

#### `otel-forward instance-default list` (alias: `ls`)

List instance-wide default forwarding destinations

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `otel-forward instance-default create`

Create a new instance-wide default forwarding destination

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | Destination name | - | Yes |
| `--vendor <preset>` | Vendor preset (datadog, honeycomb, new_relic, grafana_cloud, generic_otlp) | - | Yes |
| `--endpoint-url <url>` | OTLP-compatible collector endpoint URL | - | Yes |
| `--header-env <k=env>` | HTTP header sourced from an environment variable (repeatable) | `` | No |
| `--traces` | Forward traces (default: true) | - | No |
| `--no-traces` | Do not forward traces | - | No |
| `--metrics` | Forward metrics (default: true) | - | No |
| `--no-metrics` | Do not forward metrics | - | No |
| `--logs` | Forward logs (default: true) | - | No |
| `--no-logs` | Do not forward logs | - | No |
| `--enabled` | Create the destination enabled (default) | - | No |
| `--disabled` | Create the destination disabled | - | No |
| `--allow-private-network` | Allow the endpoint URL to resolve to private/loopback/link-local IPs | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel-forward instance-default show`

Show instance default destination details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `otel-forward instance-default update`

Update an instance default forwarding destination

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | Destination name | - | No |
| `--vendor <preset>` | Vendor preset (datadog, honeycomb, new_relic, grafana_cloud, generic_otlp) | - | No |
| `--endpoint-url <url>` | OTLP-compatible collector endpoint URL | - | No |
| `--header-env <k=env>` | HTTP header sourced from an environment variable (repeatable) | `` | No |
| `--traces` | Forward traces | - | No |
| `--no-traces` | Do not forward traces | - | No |
| `--metrics` | Forward metrics | - | No |
| `--no-metrics` | Do not forward metrics | - | No |
| `--logs` | Forward logs | - | No |
| `--no-logs` | Do not forward logs | - | No |
| `--enabled` | Enable the destination | - | No |
| `--disabled` | Disable the destination | - | No |
| `--allow-private-network` | Allow the endpoint URL to resolve to private/loopback/link-local IPs | - | No |
| `--no-allow-private-network` | Disallow private/loopback/link-local endpoint URLs | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel-forward instance-default remove`

Remove an instance default forwarding destination

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

#### `otel-forward instance-default test`

Send a test delivery to an instance default forwarding destination

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `otel`

Inspect the OTLP ingest pipeline itself — throughput, drops and failure reasons (server-wide, not project-scoped; see "temps metrics" to query ingested application metrics)

**Subcommands:**

- `ingest-errors` - Show why ingest batches were dropped, grouped by signal and failure reason
- `pipeline-history` - Show pipeline counter trends over time (received/stored/dropped per signal)

### `otel ingest-errors`

Show why ingest batches were dropped, grouped by signal and failure reason

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--limit <n>` | Max failure groups to return (default: 20, server cap: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `otel pipeline-history`

Show pipeline counter trends over time (received/stored/dropped per signal)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--period <period>` | Time period: 1h, 6h, 24h, 7d (server presets), or today/<n>h/<n>d resolved locally | `24h` | No |
| `--start-time <iso>` | Explicit window start (RFC 3339) — overrides --period | - | No |
| `--end-time <iso>` | Explicit window end (RFC 3339) — overrides --period | - | No |
| `--json` | Output in JSON format | - | No |

## `kv`

KV store commands (coming soon)

**Subcommands:**

- `get` - Get a value by key
- `set` - Set a key-value pair
- `del` (`delete`) - Delete a key
- `keys` (`ls`) - List keys
- `ttl` - Get the TTL (time-to-live) for a key
- `expire` - Set expiry on an existing key
- `incr` - Increment a numeric value
- `enable` - Enable KV store for a project
- `disable` - Disable KV store for a project
- `status` - Get KV store status for a project

### `kv get`

Get a value by key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Key to retrieve | - | Yes |

### `kv set`

Set a key-value pair

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Key to set | - | Yes |
| `--value <value>` | Value to set | - | Yes |
| `--ttl <seconds>` | Time-to-live in seconds | - | No |

### `kv del` (alias: `delete`)

Delete a key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Key to delete | - | Yes |

### `kv keys` (alias: `ls`)

List keys

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--pattern <pattern>` | Key pattern to filter by (e.g., "user:*") | - | No |
| `--json` | Output in JSON format | - | No |

### `kv ttl`

Get the TTL (time-to-live) for a key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Key to check | - | Yes |

### `kv expire`

Set expiry on an existing key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Key to set expiry on | - | Yes |
| `--ttl <seconds>` | Time-to-live in seconds | - | Yes |

### `kv incr`

Increment a numeric value

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Key to increment | - | Yes |

### `kv enable`

Enable KV store for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |

### `kv disable`

Disable KV store for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |

### `kv status`

Get KV store status for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

## `flags` (alias: `flag`)

Manage feature flags (runtime config that changes without a redeploy)

**Subcommands:**

- `list` (`ls`) - List feature flags
- `get` - Show a feature flag and its per-environment values
- `create` - Create a feature flag
- `update` - Update a flag definition (default value, description, visibility)
- `set` - Set a flag value in one environment
- `clear` - Clear a flag override so the environment inherits the default
- `disable` - Kill switch: serve the default in this environment, ignoring any override
- `enable` - Re-enable a flag in this environment after a kill switch
- `restore` - Restore an archived flag
- `archive` - Archive a flag (callers fall back to their own default)

### `flags list` (alias: `ls`)

List feature flags

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Show values for this environment | - | No |
| `--include-archived` | Include archived flags | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `flags get`

Show a feature flag and its per-environment values

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `flags create`

Create a feature flag

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-t, --type <type>` | Value type: bool, string, number, or json | - | Yes |
| `-d, --default <value>` | Default value, served when nothing more specific applies | - | Yes |
| `--description <text>` | What this flag controls | - | No |
| `--client-visible` | Allow this flag to be exposed to browsers (default: server-only) | - | No |
| `--json` | Output in JSON format | - | No |

### `flags update`

Update a flag definition (default value, description, visibility)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-d, --default <value>` | New default value | - | No |
| `--description <text>` | New description | - | No |
| `--client-visible` | Expose this flag to browsers | - | No |
| `--no-client-visible` | Make this flag server-only | - | No |
| `--json` | Output in JSON format | - | No |

### `flags set`

Set a flag value in one environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |
| `--json` | Output in JSON format | - | No |

### `flags clear`

Clear a flag override so the environment inherits the default

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |

### `flags disable`

Kill switch: serve the default in this environment, ignoring any override

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |

### `flags enable`

Re-enable a flag in this environment after a kill switch

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <name>` | Environment name or slug | - | Yes |

### `flags restore`

Restore an archived flag

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |

### `flags archive`

Archive a flag (callers fall back to their own default)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |

## `data`

Browse the data inside a service (tables, collections, keys, objects) — read-only

**Subcommands:**

- `info` - Show what a service supports and how its containers nest
- `containers` (`databases`) - List top-level containers (databases, or buckets for S3)
- `tables` (`entities`) - List tables, collections, keys or objects in a container
- `schema` (`columns`) - Show an entity's columns, types and row count
- `rows` (`select`) - Read rows from an entity
- `ai-access` - Show or set whether the built-in AI assistant may read this service's rows

### `data info`

Show what a service supports and how its containers nest

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `data containers` (alias: `databases`)

List top-level containers (databases, or buckets for S3)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | List containers nested under this path instead of the root | - | No |
| `--json` | Output in JSON format | - | No |

### `data tables` (alias: `entities`)

List tables, collections, keys or objects in a container

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Container path, slash-separated (e.g. mydb/public) | - | Yes |
| `--limit <n>` | Maximum entities to return (default: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `data schema` (alias: `columns`)

Show an entity's columns, types and row count

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Container path, slash-separated (e.g. mydb/public) | - | Yes |
| `--json` | Output in JSON format | - | No |

### `data rows` (alias: `select`)

Read rows from an entity

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Container path, slash-separated (e.g. mydb/public) | - | Yes |
| `--filter <json>` | Backend-specific filter as JSON (SQL: '{"where":"id > 5"}'). See: temps data info <service> | - | No |
| `--limit <n>` | Maximum rows to return (default: 20) | - | No |
| `--offset <n>` | Rows to skip (default: 0) | - | No |
| `--sort-by <field>` | Field to sort by | - | No |
| `--sort-order <order>` | asc or desc (default: asc) | - | No |
| `--json` | Output in JSON format | - | No |

### `data ai-access`

Show or set whether the built-in AI assistant may read this service's rows

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--enable` | Allow the built-in assistant to read row data | - | No |
| `--disable` | Stop the built-in assistant reading row data | - | No |
| `--json` | Output in JSON format | - | No |

## `blob`

Blob storage commands (coming soon)

**Subcommands:**

- `list` (`ls`) - List blobs in a project
- `upload` (`put`) - Upload a file as a blob
- `delete` (`rm`) - Delete a blob
- `copy` (`cp`) - Copy a blob to a new key
- `download` (`get`) - Download a blob to a local file
- `head` - Get blob metadata (size, content type, etc.)
- `enable` - Enable blob storage for a project
- `disable` - Disable blob storage for a project
- `status` - Get blob storage status for a project

### `blob list` (alias: `ls`)

List blobs in a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--prefix <prefix>` | Filter by key prefix | - | No |
| `--json` | Output in JSON format | - | No |

### `blob upload` (alias: `put`)

Upload a file as a blob

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Blob key (path) | - | Yes |
| `--file <path>` | Local file path to upload | - | Yes |

### `blob delete` (alias: `rm`)

Delete a blob

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Blob key to delete | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `blob copy` (alias: `cp`)

Copy a blob to a new key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--source <key>` | Source blob key | - | Yes |
| `--dest <key>` | Destination blob key | - | Yes |

### `blob download` (alias: `get`)

Download a blob to a local file

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Blob key to download | - | Yes |
| `--output <path>` | Local file path to save to | - | Yes |

### `blob head`

Get blob metadata (size, content type, etc.)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Blob key | - | Yes |
| `--json` | Output in JSON format | - | No |

### `blob enable`

Enable blob storage for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |

### `blob disable`

Disable blob storage for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |

### `blob status`

Get blob storage status for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

## `dsn`

Manage Data Source Names (DSNs) for error tracking and analytics

**Subcommands:**

- `list` (`ls`) - List all DSNs for a project
- `create` (`add`) - Create a new DSN for a project
- `get-or-create` - Get an existing DSN or create one if none exists
- `regenerate` - Regenerate DSN keys (rotate keys)
- `revoke` - Revoke (deactivate) a DSN

### `dsn list` (alias: `ls`)

List all DSNs for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `dsn create` (alias: `add`)

Create a new DSN for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-n, --name <name>` | DSN name | - | No |
| `--environment-id <id>` | Environment ID | - | No |
| `--deployment-id <id>` | Deployment ID | - | No |
| `--base-url <url>` | Base URL for the DSN | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `dsn get-or-create`

Get an existing DSN or create one if none exists

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | No |
| `--deployment-id <id>` | Deployment ID | - | No |
| `--base-url <url>` | Base URL for the DSN | - | No |
| `--json` | Output in JSON format | - | No |

### `dsn regenerate`

Regenerate DSN keys (rotate keys)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--dsn-id <id>` | DSN ID | - | Yes |
| `--base-url <url>` | New base URL for the DSN | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `dsn revoke`

Revoke (deactivate) a DSN

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--dsn-id <id>` | DSN ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

## `scans` (alias: `scan`)

Manage vulnerability scans

**Subcommands:**

- `list` (`ls`) - List vulnerability scans for a project
- `trigger` - Trigger a new vulnerability scan
- `latest` - Get the latest scan for a project
- `environments` (`envs`) - Get latest scans per environment
- `show` - Show scan details
- `vulnerabilities` (`vulns`) - List vulnerabilities found in a scan
- `remove` (`rm`) - Delete a vulnerability scan
- `by-deployment` - Get the scan for a specific deployment

### `scans list` (alias: `ls`)

List vulnerability scans for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--page <n>` | Page number | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `scans trigger`

Trigger a new vulnerability scan

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID to scan | - | Yes |

### `scans latest`

Get the latest scan for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--json` | Output in JSON format | - | No |

### `scans environments` (alias: `envs`)

Get latest scans per environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `scans show`

Show scan details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Scan ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `scans vulnerabilities` (alias: `vulns`)

List vulnerabilities found in a scan

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Scan ID | - | Yes |
| `--severity <level>` | Filter by severity (CRITICAL, HIGH, MEDIUM, LOW) | - | No |
| `--json` | Output in JSON format | - | No |

### `scans remove` (alias: `rm`)

Delete a vulnerability scan

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Scan ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `scans by-deployment`

Get the scan for a specific deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--deployment-id <id>` | Deployment ID | - | Yes |
| `--json` | Output in JSON format | - | No |

## `custom-domains` (alias: `cdom`)

Manage project custom domains

**Subcommands:**

- `list` (`ls`) - List custom domains for a project
- `create` (`add`) - Create a custom domain for a project
- `show` - Show custom domain details
- `update` - Update a custom domain
- `remove` (`rm`) - Remove a custom domain
- `link-cert` - Link a custom domain to a certificate

### `custom-domains list` (alias: `ls`)

List custom domains for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `custom-domains create` (alias: `add`)

Create a custom domain for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-d, --domain <domain>` | Domain name | - | No |
| `--environment-id <id>` | Environment ID | `0` | No |
| `--branch <branch>` | Branch name | - | No |
| `--redirect-to <url>` | Redirect target URL | - | No |
| `--status-code <code>` | HTTP status code for redirects | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `custom-domains show`

Show custom domain details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--domain-id <id>` | Custom domain ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `custom-domains update`

Update a custom domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--domain-id <id>` | Custom domain ID | - | Yes |
| `-d, --domain <domain>` | New domain name | - | No |
| `--environment-id <id>` | New environment ID | - | No |
| `--branch <branch>` | New branch name | - | No |
| `--redirect-to <url>` | New redirect target URL | - | No |
| `--status-code <code>` | New HTTP status code for redirects | - | No |

### `custom-domains remove` (alias: `rm`)

Remove a custom domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--domain-id <id>` | Custom domain ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `custom-domains link-cert`

Link a custom domain to a certificate

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--domain-id <id>` | Custom domain ID | - | Yes |
| `--certificate-id <id>` | Certificate ID | - | Yes |

## `dns-provider` (alias: `dnsp`)

Manage DNS providers and managed domains

**Subcommands:**

- `list` (`ls`) - List all DNS providers
- `create` (`add`) - Create a new DNS provider
- `show` - Show DNS provider details
- `update` - Update a DNS provider
- `remove` (`rm`) - Delete a DNS provider
- `test` - Test DNS provider connection
- `zones` - List DNS zones for a provider
- `domains` - Manage domains associated with a DNS provider
- `lookup` - Lookup DNS A records for a domain

### `dns-provider list` (alias: `ls`)

List all DNS providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `dns-provider create` (alias: `add`)

Create a new DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Provider name | - | No |
| `-t, --type <type>` | Provider type (cloudflare, route53, digitalocean, namecheap, gcp, azure, manual, pebble) | - | No |
| `-d, --description <description>` | Provider description | - | No |
| `--api-token <token>` | API token (Cloudflare, DigitalOcean) | - | No |
| `--account-id <id>` | Cloudflare account ID (optional) | - | No |
| `--access-key-id <key>` | AWS access key ID | - | No |
| `--secret-access-key <secret>` | AWS secret access key | - | No |
| `--region <region>` | AWS region | - | No |
| `--api-user <user>` | Namecheap API user | - | No |
| `--api-key <key>` | Namecheap API key | - | No |
| `--username <username>` | Namecheap username | - | No |
| `--client-ip <ip>` | Namecheap whitelisted client IP | - | No |
| `--project-id <id>` | GCP project ID | - | No |
| `--service-account-email <email>` | GCP service account email | - | No |
| `--private-key-id <id>` | GCP private key ID | - | No |
| `--private-key <key>` | GCP private key | - | No |
| `--tenant-id <id>` | Azure tenant ID | - | No |
| `--client-id <id>` | Azure client ID | - | No |
| `--client-secret <secret>` | Azure client secret | - | No |
| `--subscription-id <id>` | Azure subscription ID | - | No |
| `--resource-group <name>` | Azure resource group | - | No |
| `--management-url <url>` | pebble-challtestsrv management API URL (local ACME test server only) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `dns-provider show`

Show DNS provider details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `dns-provider update`

Update a DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-n, --name <name>` | New provider name | - | No |
| `-d, --description <description>` | New description | - | No |
| `--api-key <key>` | New API key/token | - | No |
| `--active <boolean>` | Set active status (true/false) | - | No |

### `dns-provider remove` (alias: `rm`)

Delete a DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `dns-provider test`

Test DNS provider connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |

### `dns-provider zones`

List DNS zones for a provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `dns-provider domains`

Manage domains associated with a DNS provider

**Subcommands:**

- `list` (`ls`) - List managed domains for a provider
- `add` - Add a managed domain to a provider
- `remove` (`rm`) - Remove a managed domain from a provider
- `verify` - Verify a managed domain

#### `dns-provider domains list` (alias: `ls`)

List managed domains for a provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `dns-provider domains add`

Add a managed domain to a provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-d, --domain <domain>` | Domain name | - | Yes |
| `--auto-manage` | Enable auto-management for DNS records | - | No |

#### `dns-provider domains remove` (alias: `rm`)

Remove a managed domain from a provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--provider-id <id>` | Provider ID | - | Yes |
| `-d, --domain <domain>` | Domain name | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

#### `dns-provider domains verify`

Verify a managed domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--provider-id <id>` | Provider ID | - | Yes |
| `-d, --domain <domain>` | Domain name | - | Yes |

### `dns-provider lookup`

Lookup DNS A records for a domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name to lookup | - | Yes |
| `--json` | Output in JSON format | - | No |

## `ip-access` (alias: `ipa`)

Manage IP access control rules

**Subcommands:**

- `list` (`ls`) - List all IP access control rules
- `create` (`add`) - Create a new IP access control rule
- `show` - Show IP access control rule details
- `update` - Update an IP access control rule
- `remove` (`rm`) - Delete an IP access control rule
- `check` - Check if an IP address is blocked

### `ip-access list` (alias: `ls`)

List all IP access control rules

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `ip-access create` (alias: `add`)

Create a new IP access control rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ip <ip_or_cidr>` | IPv4 or IPv6 address or CIDR range (e.g., "192.168.1.1", "10.0.0.0/24", or "2001:db8::/32") | - | No |
| `--action <action>` | Action to take: "allow" or "deny" | - | No |
| `--description <desc>` | Optional description/reason for the rule | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `ip-access show`

Show IP access control rule details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Rule ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `ip-access update`

Update an IP access control rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Rule ID | - | Yes |
| `--ip <ip>` | New IP address or CIDR range | - | No |
| `--action <action>` | New action: "allow" or "deny" | - | No |
| `--description <desc>` | New description/reason | - | No |

### `ip-access remove` (alias: `rm`)

Delete an IP access control rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Rule ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `ip-access check`

Check if an IP address is blocked

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ip <ip>` | IP address to check | - | No |
| `--json` | Output in JSON format | - | No |

## `audit`

View audit logs

**Subcommands:**

- `list` (`ls`) - List audit logs
- `show` - Show audit log details

### `audit list` (alias: `ls`)

List audit logs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--limit <n>` | Maximum number of logs to return | `50` | No |
| `--offset <n>` | Number of logs to skip | - | No |
| `--operation-type <type>` | Filter by operation type | - | No |
| `--user-id <id>` | Filter by user ID | - | No |
| `--from <timestamp>` | Start timestamp (ISO 8601 or epoch ms) | - | No |
| `--to <timestamp>` | End timestamp (ISO 8601 or epoch ms) | - | No |

### `audit show`

Show audit log details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Audit log ID | - | Yes |
| `--json` | Output in JSON format | - | No |

## `proxy-logs` (alias: `plogs`)

View proxy request logs and statistics

**Subcommands:**

- `list` (`ls`) - List proxy logs
- `show` - Show proxy log details
- `by-request` - Get proxy log by request ID
- `stats` - Get time bucket statistics (last 24 hours)
- `today` - Get today's request statistics

### `proxy-logs list` (alias: `ls`)

List proxy logs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--limit <n>` | Items per page (default: 20, max: 100) | - | No |
| `--page <n>` | Page number | - | No |
| `--project-id <id>` | Filter by project ID | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--method <method>` | Filter by HTTP method (GET, POST, etc.) | - | No |
| `--status-code <code>` | Filter by HTTP status code | - | No |
| `--host <host>` | Filter by host | - | No |
| `--path <path>` | Filter by path (partial match) | - | No |
| `--start-date <date>` | Start date (ISO 8601) | - | No |
| `--end-date <date>` | End date (ISO 8601) | - | No |
| `--sort-by <field>` | Sort by field (default: timestamp) | - | No |
| `--sort-order <order>` | Sort order: asc or desc (default: desc) | - | No |
| `--is-bot` | Filter for bot requests only | - | No |
| `--has-error` | Filter for requests with errors only | - | No |

### `proxy-logs show`

Show proxy log details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Proxy log ID | - | Yes |
| `--project-id <id>` | Authorize the lookup within this project | - | No |
| `--json` | Output in JSON format | - | No |

### `proxy-logs by-request`

Get proxy log by request ID

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--request-id <id>` | Request ID | - | No |
| `--project-id <id>` | Authorize the lookup within this project | - | No |
| `--json` | Output in JSON format | - | No |

### `proxy-logs stats`

Get time bucket statistics (last 24 hours)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `proxy-logs today`

Get today's request statistics

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `email-domains` (alias: `edom`)

Manage email domains for transactional email

**Subcommands:**

- `list` (`ls`) - List all email domains
- `create` (`add`) - Create a new email domain
- `show` - Show email domain details
- `remove` (`rm`) - Remove an email domain
- `by-name` - Look up an email domain by domain name
- `dns-records` - Get DNS records for an email domain
- `setup-dns` - Setup DNS records using a configured DNS provider
- `verify` - Verify an email domain DNS configuration
- `projects` - List projects authorized to send through an email domain
- `authorize-project` - Authorize a project to send through an email domain
- `revoke-project` - Revoke a project's permission to send through an email domain

### `email-domains list` (alias: `ls`)

List all email domains

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `email-domains create` (alias: `add`)

Create a new email domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name (e.g., mail.example.com) | - | No |
| `--provider-id <id>` | Email provider ID | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `email-domains show`

Show email domain details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email domain ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `email-domains remove` (alias: `rm`)

Remove an email domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email domain ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `email-domains by-name`

Look up an email domain by domain name

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |
| `--json` | Output in JSON format | - | No |

### `email-domains dns-records`

Get DNS records for an email domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email domain ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `email-domains setup-dns`

Setup DNS records using a configured DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email domain ID | - | Yes |
| `--dns-provider-id <id>` | DNS provider ID to use | - | No |

### `email-domains verify`

Verify an email domain DNS configuration

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email domain ID | - | Yes |

### `email-domains projects`

List projects authorized to send through an email domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email domain ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `email-domains authorize-project`

Authorize a project to send through an email domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email domain ID | - | Yes |
| `--project-id <id>` | Project ID | - | Yes |

### `email-domains revoke-project`

Revoke a project's permission to send through an email domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email domain ID | - | Yes |
| `--project-id <id>` | Project ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

## `email-providers` (alias: `eprov`)

Manage email providers (SES, Scaleway) for transactional email

**Subcommands:**

- `list` (`ls`) - List all email providers
- `create` (`add`) - Create a new email provider
- `show` - Show email provider details
- `remove` (`rm`) - Remove an email provider
- `test` - Test an email provider by sending a test email

### `email-providers list` (alias: `ls`)

List all email providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `email-providers create` (alias: `add`)

Create a new email provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Provider name | - | No |
| `-t, --type <type>` | Provider type (ses, scaleway) | - | No |
| `-r, --region <region>` | Cloud region | - | No |
| `--access-key-id <key>` | AWS access key ID (for SES) | - | No |
| `--secret-access-key <secret>` | AWS secret access key (for SES) | - | No |
| `--api-key <key>` | Scaleway API key | - | No |
| `--project-id <id>` | Scaleway project ID | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `email-providers show`

Show email provider details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `email-providers remove` (alias: `rm`)

Remove an email provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `email-providers test`

Test an email provider by sending a test email

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--from <email>` | Sender email address (must be verified) | - | No |
| `--from-name <name>` | Sender display name | - | No |

## `incidents` (alias: `incident`)

Manage incidents for status pages and monitoring

**Subcommands:**

- `list` (`ls`) - List incidents for a project
- `create` (`add`) - Create a new incident
- `show` - Show incident details
- `update-status` - Update an incident status
- `updates` - List status updates for an incident
- `bucketed` - Get bucketed incident data for a project

### `incidents list` (alias: `ls`)

List incidents for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--status <status>` | Filter by status (investigating, identified, monitoring, resolved) | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--page <n>` | Page number | - | No |
| `--page-size <n>` | Items per page | - | No |
| `--json` | Output in JSON format | - | No |

### `incidents create` (alias: `add`)

Create a new incident

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-t, --title <title>` | Incident title | - | No |
| `-d, --description <description>` | Incident description | - | No |
| `-s, --severity <severity>` | Severity level (critical, major, minor) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `incidents show`

Show incident details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Incident ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `incidents update-status`

Update an incident status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Incident ID | - | Yes |
| `-s, --status <status>` | New status (investigating, identified, monitoring, resolved) | - | No |
| `-m, --message <message>` | Status update message | - | No |

### `incidents updates`

List status updates for an incident

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Incident ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `incidents bucketed`

Get bucketed incident data for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-i, --interval <interval>` | Bucket interval: 5min, hourly, daily (default: hourly) | - | No |
| `--start-time <time>` | Start time (ISO 8601) | - | No |
| `--end-time <time>` | End time (ISO 8601) | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--json` | Output in JSON format | - | No |

## `emails` (alias: `email`)

Manage and send emails

**Subcommands:**

- `list` (`ls`) - List sent emails
- `send` - Send an email
- `show` - Show email details
- `stats` - Get email statistics
- `validate` - Validate an email address

### `emails list` (alias: `ls`)

List sent emails

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--page <n>` | Page number | - | No |
| `--page-size <n>` | Items per page | - | No |
| `--status <status>` | Filter by status (sent, delivered, failed) | - | No |
| `--domain-id <id>` | Filter by domain ID | - | No |
| `--project-id <id>` | Filter by project ID | - | No |
| `--from-address <email>` | Filter by sender address | - | No |

### `emails send`

Send an email

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--to <email>` | Recipient email address | - | No |
| `--subject <subject>` | Email subject | - | No |
| `--body <body>` | Email body | - | No |
| `--from <email>` | Sender email address | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `emails show`

Show email details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `emails stats`

Get email statistics

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `emails validate`

Validate an email address

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--email <email>` | Email address to validate | - | No |
| `--json` | Output in JSON format | - | No |

## `load-balancer` (alias: `lb`)

Manage load balancer routes

**Subcommands:**

- `list` (`ls`) - List load balancer routes
- `create` (`add`) - Create a load balancer route
- `show` - Show route details
- `update` - Update a load balancer route
- `remove` (`rm`) - Delete a load balancer route

### `load-balancer list` (alias: `ls`)

List load balancer routes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `load-balancer create` (alias: `add`)

Create a load balancer route

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain for the route | - | No |
| `-t, --target <target>` | Target upstream URL | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `load-balancer show`

Show route details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain of the route | - | Yes |
| `--json` | Output in JSON format | - | No |

### `load-balancer update`

Update a load balancer route

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain of the route | - | Yes |
| `-t, --target <target>` | New target upstream URL | - | No |

### `load-balancer remove` (alias: `rm`)

Delete a load balancer route

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain of the route | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

## `migrate` (alias: `imports`, `import`)

Migrate a project from another platform (Vercel, Coolify, Dokploy, CapRover, Portainer, Kubernetes, Docker) into temps

**Subcommands:**

- `sources` (`ls`) - List available import sources
- `discover` - Discover workloads from a source
- `plan` - Discover a source, pick a workload, and show the import plan
- `run` - Guided end-to-end migration: discover, plan, review, and execute
- `execute` - Execute a previously created import plan by session ID
- `status` - Show a stored import session (the plan it was created with)

### `migrate sources` (alias: `ls`)

List available import sources

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `migrate discover`

Discover workloads from a source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-s, --source <source>` | Import source (coolify, dokploy, caprover, portainer, kubernetes, kamal, docker) | - | No |
| `--token <token>` | API token / admin password for the source instance | - | No |
| `--base-url <url>` | Base URL of the source instance | - | No |
| `--username <name>` | Admin username (portainer source, defaults to "admin") | - | No |
| `--kubeconfig <path>` | Path to a kubeconfig file (kubernetes source) | - | No |
| `--deploy-yml <path>` | Path to config/deploy.yml (kamal source) | - | No |
| `--json` | Output in JSON format | - | No |

### `migrate plan`

Discover a source, pick a workload, and show the import plan

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-s, --source <source>` | Import source | - | No |
| `-w, --workload <workload>` | Workload ID to import (skips the picker) | - | No |
| `--token <token>` | API token / admin password for the source instance | - | No |
| `--base-url <url>` | Base URL of the source instance | - | No |
| `--username <name>` | Admin username (portainer source, defaults to "admin") | - | No |
| `--kubeconfig <path>` | Path to a kubeconfig file (kubernetes source) | - | No |
| `--deploy-yml <path>` | Path to config/deploy.yml (kamal source) | - | No |

### `migrate run`

Guided end-to-end migration: discover, plan, review, and execute

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-s, --source <source>` | Import source (vercel, coolify, dokploy, caprover, portainer, kubernetes, kamal, docker) | - | No |
| `-w, --workload <workload>` | Workload ID to import (skips the picker) | - | No |
| `--token <token>` | API token / admin password for the source instance | - | No |
| `--base-url <url>` | Base URL of the source instance | - | No |
| `--username <name>` | Admin username (portainer source, defaults to "admin") | - | No |
| `--kubeconfig <path>` | Path to a kubeconfig file (kubernetes source) | - | No |
| `--deploy-yml <path>` | Path to config/deploy.yml (kamal source) | - | No |
| `--project-name <name>` | Name for the new temps project (defaults to the source project name) | - | No |
| `--preset <preset>` | Build preset (defaults to "nixpacks" for git sources, "dockerfile" otherwise) | - | No |
| `--directory <dir>` | Project subdirectory | `.` | No |
| `--branch <branch>` | Branch to deploy | `main` | No |
| `--dry-run` | Plan only — do not create or deploy anything | - | No |
| `-y, --yes` | Skip the confirmation prompt (for automation) | - | No |

### `migrate execute`

Execute a previously created import plan by session ID

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--session-id <id>` | Import session ID (from `imports plan`) | - | Yes |
| `--project-name <name>` | Name for the new temps project | - | No |
| `--preset <preset>` | Build preset | `nixpacks` | No |
| `--directory <dir>` | Project subdirectory | `.` | No |
| `--branch <branch>` | Branch to deploy | `main` | No |
| `--dry-run` | Plan only — do not create or deploy anything | - | No |
| `-y, --yes` | Skip the confirmation prompt (for automation) | - | No |

### `migrate status`

Show a stored import session (the plan it was created with)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--session-id <id>` | Import session ID | - | Yes |
| `--json` | Output in JSON format | - | No |

## `templates` (alias: `tpl`)

Browse deployment templates

**Subcommands:**

- `list` (`ls`) - List available templates

### `templates list` (alias: `ls`)

List available templates

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--type <type>` | Filter by project type (server, static) | - | No |

## `platform` (alias: `plat`)

View platform and server information

**Subcommands:**

- `info` - Get platform information
- `access` - Get access and networking information
- `private-ip` - Get the server private IP address
- `public-ip` - Get the server public IP address
- `update` - Check for and apply temps releases on the server
- `alert-rules` - Inspect and retune the control-plane's own monitoring alert rules

### `platform info`

Get platform information

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `platform access`

Get access and networking information

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `platform private-ip`

Get the server private IP address

### `platform public-ip`

Get the server public IP address

### `platform update`

Check for and apply temps releases on the server

**Subcommands:**

- `status` - Show the available release and whether it can be applied from here
- `check` - Ask the release API for the newest version on this channel now
- `channel` - Show or set the release channel: stable, beta, nightly, or "auto" to follow the installed version
- `apply` - Install a release on the server and restart it

#### `platform update status`

Show the available release and whether it can be applied from here

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `platform update check`

Ask the release API for the newest version on this channel now

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `platform update channel`

Show or set the release channel: stable, beta, nightly, or "auto" to follow the installed version

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `platform update apply`

Install a release on the server and restart it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--version <version>` | Release tag to install (default: newest on this channel) | - | No |
| `-y, --yes` | Skip the confirmation prompt | - | No |
| `--json` | Output in JSON format | - | No |

### `platform alert-rules`

Inspect and retune the control-plane's own monitoring alert rules

**Subcommands:**

- `list` - List the alert rules watching this node (proxy health, socket exhaustion)
- `set` - Retune, enable, or disable an alert rule on this node

#### `platform alert-rules list`

List the alert rules watching this node (proxy health, socket exhaustion)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node ID (default: 0, the control plane) | - | No |
| `--json` | Output in JSON format | - | No |

#### `platform alert-rules set`

Retune, enable, or disable an alert rule on this node

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node ID (default: 0, the control plane) | - | No |
| `--threshold <n>` | Value the metric must cross to fire | - | No |
| `--comparator <op>` | Comparison operator: >, >=, <, <= | - | No |
| `--severity <level>` | Alert severity: warning or critical | - | No |
| `--for-duration <secs>` | Seconds the condition must hold before firing | - | No |
| `--enable` | Enable the rule | - | No |
| `--disable` | Disable the rule (survives the startup re-seed; deleting does not) | - | No |
| `--json` | Output in JSON format | - | No |

## `presets` (alias: `preset`)

Browse available build presets

**Subcommands:**

- `list` (`ls`) - List available presets
- `show` (`get`) - Show details for a specific preset

### `presets list` (alias: `ls`)

List available presets

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--type <type>` | Filter by project type (server, static) | - | No |

### `presets show` (alias: `get`)

Show details for a specific preset

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `analytics` (alias: `stats`)

View project analytics

**Subcommands:**

- `overview` (`o`) - Show analytics dashboard overview
- `top` - Show breakdown by dimension: pages, referrers, browsers, os, devices, countries, regions, cities, channels, events, languages, utm_source, utm_medium, utm_campaign
- `funnels` - Show funnel conversion metrics for all funnels
- `performance` (`speed`) - Show real-user Web Vitals and optional dimension breakdowns
- `ai-agents` - Show AI crawler / provider breakdown (web /analytics/ai-agents)
- `ai-pages` - Show pages crawled by AI agents, with distinct-agent counts
- `ai-page` - Show which agents/providers crawled a single page (e.g. /docs)
- `api-overview` - Show API traffic timeseries (requests, errors, latency) from /api-analytics/timeseries
- `api-routes` - Show top API routes by request count from /api-analytics/routes
- `api-callers` - Show top API callers by client IP from /api-analytics/callers
- `api-ip` - Show routes called by one client IP with latency and error analytics
- `api-path` - Show client IPs calling one path with latency and error analytics
- `api-query` - Run a typed multi-dimensional API traffic aggregation
- `api-summary` - Show an AI-generated summary of API traffic from /api-analytics/summary (requires AI Assistance to be configured and enabled on the project)

### `analytics overview` (alias: `o`)

Show analytics dashboard overview

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--json` | Output in JSON format | - | No |

### `analytics top`

Show breakdown by dimension: pages, referrers, browsers, os, devices, countries, regions, cities, channels, events, languages, utm_source, utm_medium, utm_campaign

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--limit <n>` | Number of results (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics funnels`

Show funnel conversion metrics for all funnels

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `7d` | No |
| `--json` | Output in JSON format | - | No |

### `analytics performance` (alias: `speed`)

Show real-user Web Vitals and optional dimension breakdowns

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (ignored with --start-date/--end-date) | `7d` | No |
| `--start-date <date>` | Explicit window start (RFC 3339; requires --end-date) | - | No |
| `--end-date <date>` | Explicit window end (RFC 3339; requires --start-date) | - | No |
| `--environment-id <id>` | Restrict samples to one environment ID | - | No |
| `--deployment-id <id>` | Restrict samples to one deployment ID | - | No |
| `--device <device>` | Device filter: desktop or mobile | - | No |
| `--include-bots` | Include crawler and datacenter bot samples | - | No |
| `--group-by <dimension>` | Break down by path, country, region, city, device_type, browser, or operating_system | - | No |
| `--path <path>` | Restrict samples to one page pathname | - | No |
| `--country <country>` | Restrict samples to one country | - | No |
| `--region <region>` | Restrict samples to one region | - | No |
| `--city <city>` | Restrict samples to one city | - | No |
| `--browser <browser>` | Restrict samples to one browser | - | No |
| `--os <operating-system>` | Restrict samples to one operating system | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics ai-agents`

Show AI crawler / provider breakdown (web /analytics/ai-agents)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 24h, 7d, 30d) | `24h` | No |
| `--limit <n>` | Number of rows to fetch (default: 20, max: 100) | - | No |
| `--group-by <mode>` | Group rows by "agent" (default) or "provider" | `agent` | No |
| `--path <path>` | Restrict to one URL path (e.g. /docs) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics ai-pages`

Show pages crawled by AI agents, with distinct-agent counts

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 24h, 7d, 30d) | `24h` | No |
| `--limit <n>` | Number of pages to fetch (default: 20, max: 100) | - | No |
| `--path <path>` | Restrict to one URL path (returns just that row) | - | No |
| `--with-agents` | Also fetch and render the per-agent split for each page (slower) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics ai-page`

Show which agents/providers crawled a single page (e.g. /docs)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 24h, 7d, 30d) | `24h` | No |
| `--limit <n>` | Number of rows to fetch (default: 50, max: 100) | - | No |
| `--group-by <mode>` | Group rows by "agent" (default) or "provider" | `agent` | No |
| `--json` | Output in JSON format | - | No |

### `analytics api-overview`

Show API traffic timeseries (requests, errors, latency) from /api-analytics/timeseries

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Restrict traffic to one environment ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--json` | Output in JSON format | - | No |

### `analytics api-routes`

Show top API routes by request count from /api-analytics/routes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Restrict traffic to one environment ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--limit <n>` | Number of routes to return (default: 20, max: 100) | - | No |
| `--offset <n>` | Number of ranked routes to skip (default: 0) | - | No |
| `--sort-by <metric>` | Sort by requests, latency_avg, or error_rate | `requests` | No |
| `--order <direction>` | Sort direction: asc or desc | `desc` | No |
| `--include-synthetic` | Include Temps status-monitor checks | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics api-callers`

Show top API callers by client IP from /api-analytics/callers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Restrict traffic to one environment ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--limit <n>` | Number of callers to return (default: 20, max: 100) | - | No |
| `--offset <n>` | Number of ranked callers to skip (default: 0) | - | No |
| `--sort-by <metric>` | Sort by requests or error_rate | `requests` | No |
| `--order <direction>` | Sort direction: asc or desc | `desc` | No |
| `--include-synthetic` | Include Temps status-monitor checks | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics api-ip`

Show routes called by one client IP with latency and error analytics

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Restrict traffic to one environment ID | - | No |
| `--period <period>` | Time period (e.g. 1h, 24h, 7d, 30d) | `24h` | No |
| `--limit <n>` | Rows per page (default: 20, max: 100) | - | No |
| `--page <n>` | Page number | `1` | No |
| `--sort-by <metric>` | Sort by requests, latency_avg, or error_rate | `requests` | No |
| `--order <direction>` | Sort direction: asc or desc | `desc` | No |
| `--include-synthetic` | Include Temps status-monitor checks | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics api-path`

Show client IPs calling one path with latency and error analytics

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Restrict traffic to one environment ID | - | No |
| `--period <period>` | Time period (e.g. 1h, 24h, 7d, 30d) | `24h` | No |
| `--limit <n>` | Rows per page (default: 20, max: 100) | - | No |
| `--page <n>` | Page number | `1` | No |
| `--sort-by <metric>` | Sort by requests or error_rate | `requests` | No |
| `--order <direction>` | Sort direction: asc or desc | `desc` | No |
| `--include-synthetic` | Include Temps status-monitor checks | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics api-query`

Run a typed multi-dimensional API traffic aggregation

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--group-by <dimensions>` | Comma-separated dimensions (omit for one overall rollup) | - | No |
| `--metrics <metrics>` | Comma-separated metrics (e.g. requests,error_rate,latency_p95) | - | Yes |
| `--filter <dimension:operator:value>` | Repeatable filter (operators: eq, not_eq, contains, starts_with, in) | `` | No |
| `--sort-by <field>` | Requested dimension or metric to sort by | - | No |
| `--order <direction>` | Sort direction: asc or desc | `desc` | No |
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Restrict traffic to one environment ID | - | No |
| `--period <period>` | Time period (e.g. 1h, 24h, 7d, 30d) | `24h` | No |
| `--page <n>` | Page number | `1` | No |
| `--limit <n>` | Rows per page (default: 20, max: 100) | - | No |
| `--include-synthetic` | Include Temps status-monitor checks | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics api-summary`

Show an AI-generated summary of API traffic from /api-analytics/summary (requires AI Assistance to be configured and enabled on the project)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Restrict traffic to one environment ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--json` | Output in JSON format | - | No |

## `funnels` (alias: `funnel`)

Manage analytics funnels for projects

**Subcommands:**

- `list` (`ls`) - List all funnels for a project
- `create` (`add`) - Create a new funnel for a project
- `update` - Update a funnel
- `remove` (`rm`) - Delete a funnel
- `metrics` - Get funnel metrics
- `preview` - Preview funnel metrics without saving

### `funnels list` (alias: `ls`)

List all funnels for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `funnels create` (alias: `add`)

Create a new funnel for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-n, --name <name>` | Funnel name | - | No |
| `-s, --steps <json>` | Funnel steps as JSON array (e.g. '[{"event_name":"page_view"},{"event_name":"signup"}]') | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `funnels update`

Update a funnel

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--funnel-id <id>` | Funnel ID | - | Yes |
| `-n, --name <name>` | New funnel name | - | No |
| `-s, --steps <json>` | New funnel steps as JSON array | - | No |

### `funnels remove` (alias: `rm`)

Delete a funnel

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--funnel-id <id>` | Funnel ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `funnels metrics`

Get funnel metrics

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--funnel-id <id>` | Funnel ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `funnels preview`

Preview funnel metrics without saving

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-s, --steps <json>` | Funnel steps as JSON array | - | Yes |
| `--json` | Output in JSON format | - | No |

## `notification-preferences` (alias: `notif-prefs`)

Manage notification preferences

**Subcommands:**

- `show` (`get`) - Show current notification preferences
- `update` (`set`) - Update a notification preference
- `reset` - Reset notification preferences to defaults

### `notification-preferences show` (alias: `get`)

Show current notification preferences

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `notification-preferences update` (alias: `set`)

Update a notification preference

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-k, --key <key>` | Preference key to update | - | Yes |
| `-v, --value <value>` | Value for the preference | - | Yes |

### `notification-preferences reset`

Reset notification preferences to defaults

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

## `skills` (alias: `skill`)

Manage AI skill definitions (global or project-scoped)

**Subcommands:**

- `list` (`ls`) - List skill definitions
- `create` (`add`) - Create a new skill definition. Use @path for content from a file, directory, or tar.gz
- `update` - Update an existing skill definition
- `delete` (`rm`) - Delete a skill definition
- `import` - Import a skill from a public GitHub repository (skills.sh-compatible). Source: <owner>/<repo> or <owner>/<repo>/<skill-name>

### `skills list` (alias: `ls`)

List skill definitions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--global` | List global (platform-wide) skills | - | No |
| `--project <slug>` | List skills for a specific project | - | No |
| `--json` | Output in JSON format | - | No |

### `skills create` (alias: `add`)

Create a new skill definition. Use @path for content from a file, directory, or tar.gz

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Skill name | - | Yes |
| `-s, --slug <slug>` | Skill slug (URL-safe identifier) | - | Yes |
| `-c, --content <content>` | Skill content (markdown), @file, @directory, or @archive.tar.gz | - | No |
| `-d, --description <description>` | Skill description | - | No |
| `--global` | Create as global (platform-wide) skill | - | No |
| `--project <slug>` | Create skill for a specific project | - | No |

### `skills update`

Update an existing skill definition

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | New name | - | No |
| `-c, --content <content>` | New content. Prefix with @ to read from file | - | No |
| `-d, --description <description>` | New description | - | No |
| `--global` | Update a global skill | - | No |
| `--project <slug>` | Update a project-scoped skill | - | No |

### `skills delete` (alias: `rm`)

Delete a skill definition

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--global` | Delete a global skill | - | No |
| `--project <slug>` | Delete a project-scoped skill | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `skills import`

Import a skill from a public GitHub repository (skills.sh-compatible). Source: <owner>/<repo> or <owner>/<repo>/<skill-name>

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-b, --branch <branch>` | Git branch to fetch from | `main` | No |
| `-s, --slug <slug>` | Override slug (defaults to skill directory name) | - | No |
| `-n, --name <name>` | Override skill name (defaults to SKILL.md frontmatter) | - | No |
| `-d, --description <description>` | Override description | - | No |
| `--global` | Install as a global (platform-wide) skill | - | No |
| `--project <slug>` | Install for a specific project | - | No |
| `-f, --force` | Overwrite if a skill with the same slug already exists | - | No |

## `mcp-servers`

Manage MCP server definitions (global or project-scoped)

**Subcommands:**

- `list` (`ls`) - List MCP server definitions
- `create` (`add`) - Create a new MCP server definition
- `update` - Update an existing MCP server definition
- `delete` (`rm`) - Delete an MCP server definition

### `mcp-servers list` (alias: `ls`)

List MCP server definitions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--global` | List global (platform-wide) MCP servers | - | No |
| `--project <slug>` | List MCP servers for a specific project | - | No |
| `--json` | Output in JSON format | - | No |

### `mcp-servers create` (alias: `add`)

Create a new MCP server definition

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | MCP server name | - | Yes |
| `-s, --slug <slug>` | MCP server slug (URL-safe identifier) | - | Yes |
| `-c, --config <config>` | MCP server config (JSON). Prefix with @ to read from file (e.g. @./mcp.json) | - | Yes |
| `-d, --description <description>` | MCP server description | - | No |
| `--global` | Create as global (platform-wide) MCP server | - | No |
| `--project <slug>` | Create MCP server for a specific project | - | No |

### `mcp-servers update`

Update an existing MCP server definition

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | New name | - | No |
| `-c, --config <config>` | New config (JSON). Prefix with @ to read from file | - | No |
| `-d, --description <description>` | New description | - | No |
| `--global` | Update a global MCP server | - | No |
| `--project <slug>` | Update a project-scoped MCP server | - | No |

### `mcp-servers delete` (alias: `rm`)

Delete an MCP server definition

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--global` | Delete a global MCP server | - | No |
| `--project <slug>` | Delete a project-scoped MCP server | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

## `mcp`

Configure this Temps instance as an MCP server for AI clients (Claude Code, Claude Desktop, Codex, Cursor, VS Code, Windsurf, Zed)

**Subcommands:**

- `enable` - Enable the Temps MCP server on this instance (admin, one-time per instance)
- `disable` - Disable the Temps MCP server on this instance (admin)
- `add` - Configure an AI client to connect to this Temps instance over MCP. Clients: claude-code, claude-desktop, codex, cursor, vscode, windsurf, zed
- `remove` - Remove the Temps MCP server from an AI client
- `status` - Show whether this instance has MCP enabled and which AI clients are configured

### `mcp enable`

Enable the Temps MCP server on this instance (admin, one-time per instance)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --url <url>` | Target this Temps instance directly (e.g. copied from the Settings UI), without needing a saved CLI context or changing the active one. Defaults to the current context/login when omitted. | - | No |

### `mcp disable`

Disable the Temps MCP server on this instance (admin)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --url <url>` | Target this Temps instance directly (e.g. copied from the Settings UI), without needing a saved CLI context or changing the active one. Defaults to the current context/login when omitted. | - | No |

### `mcp add`

Configure an AI client to connect to this Temps instance over MCP. Clients: claude-code, claude-desktop, codex, cursor, vscode, windsurf, zed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-g, --groups <groups>` | Comma-separated tool groups to enable (default: all) | - | No |
| `-w, --write` | Enable write tools (deploy, delete, restart, etc). Default: read-only | - | No |
| `-k, --api-key <key>` | Use this API key instead of creating or prompting for one | - | No |
| `-u, --url <url>` | Target this Temps instance directly (e.g. copied from the Settings UI), without needing a saved CLI context or changing the active one. Defaults to the current context/login when omitted. | - | No |
| `-y, --yes` | Skip prompts and confirmation (uses defaults; requires --api-key or an existing login) | - | No |

### `mcp remove`

Remove the Temps MCP server from an AI client

### `mcp status`

Show whether this instance has MCP enabled and which AI clients are configured

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --url <url>` | Target this Temps instance directly (e.g. copied from the Settings UI), without needing a saved CLI context or changing the active one. Defaults to the current context/login when omitted. | - | No |

## `secrets` (alias: `secret`)

Manage agent secrets. env-type: reference as ${TEMPS_SECRET:name} in MCP config. file-type: written to --mount-path in sandbox; reference that path.

**Subcommands:**

- `list` (`ls`) - List all secrets (values are masked)
- `create` (`add`) - Create or update a secret (upsert by name)
- `update` - Update an existing secret (alias for create — upserts)
- `delete` (`rm`) - Delete a secret

### `secrets list` (alias: `ls`)

List all secrets (values are masked)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `secrets create` (alias: `add`)

Create or update a secret (upsert by name)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Secret name | - | Yes |
| `-v, --value <value>` | Secret value. Prefix with @ to read from file (e.g. @./creds.json) | - | Yes |
| `-t, --type <type>` | Secret type: "env" (default) or "file" | `env` | No |
| `-m, --mount-path <path>` | Absolute path inside sandbox where file-type secret is written (required for --type file) | - | No |
| `-d, --description <description>` | Human-readable description | - | No |

### `secrets update`

Update an existing secret (alias for create — upserts)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Secret name | - | Yes |
| `-v, --value <value>` | New value. Prefix with @ to read from file | - | No |
| `-t, --type <type>` | Secret type: "env" or "file" | - | No |
| `-m, --mount-path <path>` | New mount path (file type only) | - | No |
| `-d, --description <description>` | New description | - | No |

### `secrets delete` (alias: `rm`)

Delete a secret

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

## `sandbox`

Manage standalone sandboxes (/v1/sandbox API)

**Subcommands:**

- `create` - Create a new sandbox
- `list` (`ls`) - List your sandboxes
- `show` - Show details for a sandbox
- `rm` (`stop`, `destroy`) - Remove a sandbox permanently (aliases: stop, destroy)
- `pause` - Pause a running sandbox (non-destructive — resume later with `sandbox resume`)
- `resume` - Resume a paused sandbox
- `restart` - Restart a running sandbox (preserves filesystem)
- `clone` - Clone a git repo or extract a tarball into a running sandbox
- `shell` (`attach`) - Open an interactive terminal in a sandbox. Detach with Ctrl-P Ctrl-Q to leave the program running; `exit` ends it. Reattach with the same --tab
- `extend` - Extend a sandbox's idle timeout
- `exec` - Run a command inside a sandbox. Use `--` to pass flags: `exec ID -- ls -la`
- `logs` - Stream logs from a detached job (SSE)
- `domain` - Resolve the preview URL for a port inside a sandbox
- `password` - Generate, rotate, or clear the preview-URL password for a sandbox
- `fs` - Filesystem operations inside a sandbox
- `snapshots` - Manage sandbox snapshots (ADR-037)
- `snapshot` - Take a snapshot of a sandbox

### `sandbox create`

Create a new sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <image>` | Docker image override (uses platform default when omitted) | - | No |
| `--name <name>` | Display name for the sandbox | - | No |
| `--timeout <seconds>` | Idle timeout in seconds (clamped to [60, 86400]) | - | No |
| `-e, --env <KEY=VAL>` | Env var baked into the container (repeatable) | - | No |
| `--cpu-limit <cpu>` | CPU limit (e.g., 0.5 for half a core) | - | No |
| `--memory-mb <mb>` | Memory limit in megabytes | - | No |
| `--git-url <url>` | Git repo URL to clone into the work dir | - | No |
| `--git-rev <revision>` | Git revision to check out (requires --git-url) | - | No |
| `--git-depth <n>` | Shallow clone depth (requires --git-url) | - | No |
| `--git-connection <id>` | ID of a stored git provider connection; temps injects the token server-side | - | No |
| `--git-username <user>` | HTTP Basic username for private repo clone (requires --git-password) | - | No |
| `--git-password <token>` | HTTP Basic password/token (paired with --git-username; injected via GIT_ASKPASS) | - | No |
| `--tarball-url <url>` | Tarball URL to download and extract | - | No |
| `--workspace` | Create a persistent workspace: suspends when idle, wakes automatically on the next command, and is never destroyed for you | - | No |
| `--project <slug>` | Seed from a temps project's connected repo (and attribute the sandbox to it). Defaults to the linked project in .temps/config.json | - | No |
| `--repo <owner/name>` | Seed from a repo on one of your git connections that has no temps project | - | No |
| `--branch <ref>` | Branch, tag, or SHA to check out (alias of --git-rev) | - | No |
| `--new-branch <name>` | Create and switch to a new branch after cloning, based on whatever was checked out | - | No |
| `--preview-password` | Generate a random preview-URL password and print it once on stdout | - | No |
| `--preview-password-length <n>` | Length of the generated preview password (8..=256, default 24) | - | No |
| `--from-snapshot <snap-id>` | Create sandbox from a snapshot (mutually exclusive with --image) | - | No |
| `--json` | Output as JSON | - | No |

### `sandbox list` (alias: `ls`)

List your sandboxes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page (1-indexed) | - | No |
| `--page-size <n>` | Items per page (default 20, max 100) | - | No |
| `--workspace` | Show only persistent workspaces | - | No |
| `--lifecycle <class>` | Filter by lifecycle class: ephemeral \| workspace | - | No |
| `--project <slug>` | Show only sandboxes created from this project | - | No |
| `--json` | Output as JSON | - | No |

### `sandbox show`

Show details for a sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

### `sandbox rm` (alias: `stop`, `destroy`)

Remove a sandbox permanently (aliases: stop, destroy)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation prompt | - | No |

### `sandbox pause`

Pause a running sandbox (non-destructive — resume later with `sandbox resume`)

### `sandbox resume`

Resume a paused sandbox

### `sandbox restart`

Restart a running sandbox (preserves filesystem)

### `sandbox clone`

Clone a git repo or extract a tarball into a running sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--git-url <url>` | Git repo URL to clone | - | No |
| `--git-rev <revision>` | Git revision (branch/tag/SHA) to check out | - | No |
| `--git-depth <n>` | Shallow clone depth | - | No |
| `--git-connection <id>` | ID of a stored git provider connection; temps injects the token server-side | - | No |
| `--git-username <user>` | HTTP Basic username (pairs with --git-password) | - | No |
| `--git-password <token>` | HTTP Basic password/token (injected via GIT_ASKPASS) | - | No |
| `--tarball-url <url>` | Tarball URL to download and extract | - | No |

### `sandbox shell` (alias: `attach`)

Open an interactive terminal in a sandbox. Detach with Ctrl-P Ctrl-Q to leave the program running; `exit` ends it. Reattach with the same --tab

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--tab <name>` | Tab to attach to; reusing a name reattaches to the program already running in it | `main` | No |
| `--cmd <command>` | Program to start when the tab is created, e.g. "claude" (default: login shell) | - | No |

### `sandbox extend`

Extend a sandbox's idle timeout

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--secs <seconds>` | Extra seconds to add to the current expiry | - | Yes |

### `sandbox exec`

Run a command inside a sandbox. Use `--` to pass flags: `exec ID -- ls -la`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--detach` | Start in background and print a job ID instead of waiting | - | No |
| `--cwd <path>` | Working directory inside the sandbox | - | No |
| `-e, --env <KEY=VAL>` | Env var for this exec (repeatable) | - | No |

### `sandbox logs`

Stream logs from a detached job (SSE)

### `sandbox domain`

Resolve the preview URL for a port inside a sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--port <port>` | Port inside the sandbox (1..=65535) | - | Yes |

### `sandbox password`

Generate, rotate, or clear the preview-URL password for a sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--rotate` | Generate a new random password and set it (default when no flag is given) | - | No |
| `--length <n>` | Length of the generated password (8..=256, default 24) | - | No |
| `--clear` | Remove the preview password — preview URLs become open again | - | No |

### `sandbox fs`

Filesystem operations inside a sandbox

**Subcommands:**

- `read` - Read a file from the sandbox
- `write` - Write a file to the sandbox
- `stat` - Stat a path inside the sandbox
- `mkdir` - Create a directory inside the sandbox (mkdir -p)

#### `sandbox fs read`

Read a file from the sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Absolute file path inside the sandbox | - | Yes |
| `--out <localPath>` | Write to this local file (stdout when omitted) | - | No |

#### `sandbox fs write`

Write a file to the sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Absolute target path inside the sandbox | - | Yes |
| `--file <localPath>` | Local source file to upload (mutually exclusive with --content) | - | No |
| `--content <string>` | Inline string content to write | - | No |
| `--mode <octal>` | Unix permission mask (default: 0644) | - | No |

#### `sandbox fs stat`

Stat a path inside the sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Absolute path inside the sandbox | - | Yes |
| `--json` | Output as JSON | - | No |

#### `sandbox fs mkdir`

Create a directory inside the sandbox (mkdir -p)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Absolute path inside the sandbox | - | Yes |

### `sandbox snapshots`

Manage sandbox snapshots (ADR-037)

**Subcommands:**

- `list` (`ls`) - List your snapshots
- `show` - Show details for a snapshot
- `delete` (`rm`) - Delete a snapshot permanently
- `storage` - Show snapshot storage usage and quota

#### `sandbox snapshots list` (alias: `ls`)

List your snapshots

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project <id>` | Filter by project ID | - | No |
| `--status <status>` | Filter by status: creating \| ready \| failed \| deleted | - | No |
| `--page <n>` | Page number (1-indexed) | - | No |
| `--page-size <n>` | Items per page (default 20, max 100) | - | No |
| `--json` | Output as JSON | - | No |

#### `sandbox snapshots show`

Show details for a snapshot

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `sandbox snapshots delete` (alias: `rm`)

Delete a snapshot permanently

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation prompt | - | No |

#### `sandbox snapshots storage`

Show snapshot storage usage and quota

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

### `sandbox snapshot`

Take a snapshot of a sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--label <label>` | Human-readable label for the snapshot | - | No |
| `--wait` | Wait until the snapshot reaches ready or failed status | - | No |
| `--json` | Output as JSON | - | No |

## `workflow` (alias: `wf`)

Trigger and inspect agent/workflow runs

**Subcommands:**

- `list` (`ls`) - List workflows/agents available on this project
- `run` - Trigger a workflow and stream its output

### `workflow list` (alias: `ls`)

List workflows/agents available on this project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (auto-detect from .temps/config.json) | - | No |
| `--json` | Output as JSON | - | No |

### `workflow run`

Trigger a workflow and stream its output

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (auto-detect from .temps/config.json) | - | No |
| `-c, --context <text>` | Free-form user context passed to the workflow (e.g. a bug description) | - | No |
| `-f, --from-file <path>` | Run an ephemeral workflow from a local YAML file (no server-side persistence). Mutually exclusive with <slug>. | - | No |
| `-e, --error-group <id>` | Link this run to an error group id. The workflow will see the error type, message, and stack trace via the usual {{error_type}} / {{error_message}} template fields. Works with both committed slugs and --from-file. | - | No |
| `--cpu <cores>` | CPU cores for the ephemeral sandbox (0.1–4.0). Overrides the YAML value. Only applies with --from-file. | - | No |
| `--memory <mb>` | Memory limit in MB for the ephemeral sandbox (128–8192). Overrides the YAML value. Only applies with --from-file. | - | No |
| `--no-follow` | Return immediately after queueing instead of streaming logs | - | No |
| `--json` | Print the run record as JSON when it terminates | - | No |

## `revenue`

Manage revenue integrations and import historical data

**Subcommands:**

- `import` - Import historical revenue data from a CSV export

### `revenue import`

Import historical revenue data from a CSV export

**Subcommands:**

- `subscriptions` - Import current subscriptions CSV (e.g., Stripe → Subscriptions → Export)
- `invoices` - Import paid invoices CSV to backfill the revenue chart

#### `revenue import subscriptions`

Import current subscriptions CSV (e.g., Stripe → Subscriptions → Export)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (defaults to linked project) | - | No |
| `--integration-id <id>` | Target integration ID (auto-detected if only one exists) | - | No |
| `--provider <name>` | Target provider name (e.g., stripe) | - | No |
| `--json` | Output the import outcome as JSON (suppresses spinners) | - | No |

#### `revenue import invoices`

Import paid invoices CSV to backfill the revenue chart

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (defaults to linked project) | - | No |
| `--integration-id <id>` | Target integration ID (auto-detected if only one exists) | - | No |
| `--provider <name>` | Target provider name (e.g., stripe) | - | No |
| `--json` | Output the import outcome as JSON (suppresses spinners) | - | No |

## `session-replay` (alias: `sessions`, `replay`)

Manage session replay recordings

**Subcommands:**

- `list` (`ls`) - List session replays for a project
- `visitor` - List session replays for a specific visitor
- `show` - Show session metadata (use numeric session ID from list)
- `events` - Download or page through all rrweb events for a session
- `delete` (`rm`) - Delete a session replay

### `session-replay list` (alias: `ls`)

List session replays for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--page <n>` | Page number (default: 1) | `1` | No |
| `--per-page <n>` | Sessions per page (default: 25, max: 100) | `25` | No |
| `--json` | Output raw JSON | - | No |

### `session-replay visitor`

List session replays for a specific visitor

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number (default: 1) | `1` | No |
| `--per-page <n>` | Sessions per page (default: 25) | `25` | No |
| `--json` | Output raw JSON | - | No |

### `session-replay show`

Show session metadata (use numeric session ID from list)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output raw JSON | - | No |

### `session-replay events`

Download or page through all rrweb events for a session

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page of events to display (default: 1) | `1` | No |
| `--limit <n>` | Events per page (default: 50) | `50` | No |
| `--output <file>` | Write all events as JSON to a file (skips paged display) | - | No |
| `--json` | Print all events as JSON to stdout | - | No |

### `session-replay delete` (alias: `rm`)

Delete a session replay

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip confirmation prompt | - | No |

## `traefik-discovery`

Route containers Temps did not deploy by reading their Traefik labels (an existing docker-compose / Coolify / Dokploy stack)

**Subcommands:**

- `status` - Show whether Traefik label discovery is enabled on this server, which Docker network it watches, and what the last reconciliation found
- `routes` - Inspect and suppress individual auto-discovered routes
- `tls` - Manage HTTPS certificates for Traefik-discovered routes (ADR-041). A discovered host has cert_eligible=false by design — no container label ever causes issuance. These commands let an operator explicitly authorize it.

### `traefik-discovery status`

Show whether Traefik label discovery is enabled on this server, which Docker network it watches, and what the last reconciliation found

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `traefik-discovery routes`

Inspect and suppress individual auto-discovered routes

**Subcommands:**

- `list` (`ls`) - List every route discovered from Traefik labels, including the labelled containers that were found but not routed, and why
- `enable` - Restore a previously suppressed discovered route
- `disable` - Stop routing one discovered host without touching the container labels; the route stays listed so you can see what was found

#### `traefik-discovery routes list` (alias: `ls`)

List every route discovered from Traefik labels, including the labelled containers that were found but not routed, and why

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Page size (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `traefik-discovery routes enable`

Restore a previously suppressed discovered route

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `traefik-discovery routes disable`

Stop routing one discovered host without touching the container labels; the route stays listed so you can see what was found

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `traefik-discovery tls`

Manage HTTPS certificates for Traefik-discovered routes (ADR-041). A discovered host has cert_eligible=false by design — no container label ever causes issuance. These commands let an operator explicitly authorize it.

**Subcommands:**

- `request` - Authorize Temps to obtain a Let's Encrypt certificate for a discovered route (Path A). The certificate renews automatically using the declared challenge type.
- `revoke` - Remove TLS authorization for a discovered route. Stops Temps from attempting renewal. Does NOT delete the certificate — use `temps domains delete <host>` to remove the certificate itself.
- `import` - Import certificates from a Traefik acme.json file (Path B). Use this to get HTTPS immediately at cutover — Traefik already holds the cert, so there is no outage window. Each host is validated (8-step X.509 chain) and a per-host result is returned. Add --dry-run to preview without writing.

#### `traefik-discovery tls request`

Authorize Temps to obtain a Let's Encrypt certificate for a discovered route (Path A). The certificate renews automatically using the declared challenge type.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--challenge-type <type>` | Challenge type: http-01 (default) or dns-01 | `http-01` | No |
| `--acknowledge-manual-dns-renewal` | Confirm you accept manual DNS renewal when no auto-manage DNS zone is configured | - | No |
| `--json` | Output in JSON format | - | No |

#### `traefik-discovery tls revoke`

Remove TLS authorization for a discovered route. Stops Temps from attempting renewal. Does NOT delete the certificate — use `temps domains delete <host>` to remove the certificate itself.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `traefik-discovery tls import`

Import certificates from a Traefik acme.json file (Path B). Use this to get HTTPS immediately at cutover — Traefik already holds the cert, so there is no outage window. Each host is validated (8-step X.509 chain) and a per-host result is returned. Add --dry-run to preview without writing.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--hosts <hosts>` | Comma-separated list of hostnames to import | - | Yes |
| `--renewal-method <method>` | How Temps will renew when the imported cert expires: http-01 (default) or dns-01 | `http-01` | No |
| `--acknowledge-manual-dns-renewal` | Confirm you accept manual DNS renewal when no auto-manage DNS zone is configured | - | No |
| `--dry-run` | Validate and preview; do not write any certificate | - | No |
| `--json` | Output in JSON format | - | No |

## `init`

Initialize a Temps project in the current directory

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Project name (for new projects) | - | No |
| `-y, --yes` | Skip confirmation prompts | - | No |

## `link`

Link current directory to a Temps project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Set default environment | - | No |

## `up`

Deploy the current project (runs setup wizard if not linked)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Target environment name | - | No |
| `-b, --branch <branch>` | Git branch to deploy (auto-detected from cwd) | - | No |
| `-n, --name <name>` | Project name (for new projects) | - | No |
| `--preset <preset>` | Framework preset slug (skip auto-detection) | - | No |
| `--manual` | Use manual deployment mode (no git) | - | No |
| `--static` | Deploy a pre-built static folder (no Docker, no git) | - | No |
| `--static-dir <dir>` | Folder to upload for static deploys (auto-detected by default) | - | No |
| `--no-services` | Skip external service setup | - | No |
| `--no-wait` | Do not wait for deployment to complete | - | No |
| `-y, --yes` | Skip confirmation prompts | - | No |

## `status`

Show project deployment status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `-e, --environment <env>` | Filter by environment | - | No |
| `--json` | Output in JSON format | - | No |

## `ai`

AI assistant status for a project

**Subcommands:**

- `readiness` - Show which AI prerequisites this project meets, and how to fix the rest

### `ai readiness`

Show which AI prerequisites this project meets, and how to fix the rest

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

## `instances` (alias: `instance`)

Manage Temps server instances

**Subcommands:**

- `list` (`ls`) - List configured instances
- `add` - Add a new instance
- `remove` (`rm`) - Remove an instance
- `switch` (`use`) - Switch to a different instance
- `show` - Show instance details (or current instance)

### `instances list` (alias: `ls`)

List configured instances

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `instances add`

Add a new instance

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Instance name | - | No |
| `-u, --url <url>` | Instance URL | - | No |

### `instances remove` (alias: `rm`)

Remove an instance

### `instances switch` (alias: `use`)

Switch to a different instance

### `instances show`

Show instance details (or current instance)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

## `env:pull`

Pull environment variables to a .env file

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Pull from specific environment | - | No |
| `-p, --project <project>` | Project slug | - | No |

## `env:push`

Push environment variables from a .env file

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <names>` | Comma-separated environment names | - | No |
| `-p, --project <project>` | Project slug | - | No |
| `--overwrite` | Overwrite existing variables | - | No |

## `rollback`

Rollback to a previous deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `-e, --environment <env>` | Target environment | `production` | No |
| `--to <id>` | Rollback to specific deployment ID | - | No |
| `-y, --yes` | Skip confirmation | - | No |

## `open`

Open project URL in browser

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `-e, --environment <env>` | Open specific environment | - | No |
| `--dashboard` | Open the dashboard instead of the project URL | - | No |

## `exec` (alias: `ssh`)

Execute a command in a running container (coming soon)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `-e, --environment <env>` | Target environment | - | No |

## `dev`

Start a local development tunnel (coming soon)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--port <port>` | Local port to expose | `3000` | No |

## `cloud`

Temps Cloud

**Subcommands:**

- `login` - Login to Temps Cloud
- `logout` - Logout from Temps Cloud
- `whoami` - Show current Temps Cloud account
- `vps` - Manage cloud VPS instances
- `billing` - Manage Temps Cloud billing and subscription

### `cloud login`

Login to Temps Cloud

### `cloud logout`

Logout from Temps Cloud

### `cloud whoami`

Show current Temps Cloud account

### `cloud vps`

Manage cloud VPS instances

**Subcommands:**

- `list` - List VPS instances
- `create` - Provision a new VPS instance
- `show` - Show VPS instance details and provisioning logs
- `destroy` - Destroy a VPS instance
- `retry` - Retry failed VPS provisioning
- `credentials` - Show VPS panel credentials
- `images` - List available OS images
- `locations` - List available datacenter locations
- `types` - List available server types with pricing

#### `cloud vps list`

List VPS instances

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `cloud vps create`

Provision a new VPS instance

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <image>` | OS image ID | - | No |
| `--location <location>` | Datacenter location ID | - | No |
| `--type <type>` | Server type ID | - | No |
| `--json` | Output as JSON | - | No |

#### `cloud vps show`

Show VPS instance details and provisioning logs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `cloud vps destroy`

Destroy a VPS instance

#### `cloud vps retry`

Retry failed VPS provisioning

#### `cloud vps credentials`

Show VPS panel credentials

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `cloud vps images`

List available OS images

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `cloud vps locations`

List available datacenter locations

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `cloud vps types`

List available server types with pricing

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--location <location>` | Filter by datacenter location | - | No |
| `--json` | Output as JSON | - | No |

### `cloud billing`

Manage Temps Cloud billing and subscription

**Subcommands:**

- `overview` - Show billing overview
- `usage` - Show usage and limits
- `upgrade` - Upgrade your plan

#### `cloud billing overview`

Show billing overview

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `cloud billing usage`

Show usage and limits

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `cloud billing upgrade`

Upgrade your plan

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--yearly` | Use yearly billing cycle (default: monthly) | - | No |
| `--no-browser` | Don't open browser, just show the URL | - | No |


---

## Examples

### Basic Workflow

```bash
# Login to Temps
bunx @temps-sdk/cli@0.1.36 login

# Create a new project on the intended server
bunx @temps-sdk/cli@0.1.36 --target-context production projects create --name my-app

# Deploy to production
bunx @temps-sdk/cli@0.1.36 --target-context production deploy --project my-app --environment production

# View deployment logs
bunx @temps-sdk/cli@0.1.36 deployments logs --project my-app --follow

# Stream runtime container logs
bunx @temps-sdk/cli@0.1.36 runtime-logs --project my-app

# List containers
bunx @temps-sdk/cli@0.1.36 containers list --project-id 1 --environment-id 1
```

### Managing Environments

```bash
# List environments
bunx @temps-sdk/cli@0.1.36 environments list --project my-app

# Set environment variables on the intended server
bunx @temps-sdk/cli@0.1.36 --target-context production environments vars set --project my-app --key DATABASE_URL

# View environment variables
bunx @temps-sdk/cli@0.1.36 environments vars list --project my-app
```

### Managing Domains

```bash
# Add a custom domain on the intended server
bunx @temps-sdk/cli@0.1.36 --target-context production domains add --project my-app --domain app.example.com

# List domains
bunx @temps-sdk/cli@0.1.36 domains list --project my-app

# Remove a domain from the intended server
bunx @temps-sdk/cli@0.1.36 --target-context production domains remove --project my-app --domain app.example.com
```

## Environment Variables

The CLI respects the following environment variables:

| Variable | Description |
|----------|-------------|
| `TEMPS_API_URL` | API endpoint URL |
| `TEMPS_API_TOKEN` | API authentication token |
| `TEMPS_API_KEY` | API key (alternative to token) |
| `NO_COLOR` | Disable colored output |

## Configuration

Configuration is stored in:
- **Config file**: `~/.temps/config.json`
- **Credentials**: Stored securely in `~/.temps/` with restricted file permissions

Use `bunx @temps-sdk/cli@0.1.36 configure show` to view current configuration.

## Support

- Documentation: https://temps.sh/docs
- Issues: https://github.com/gotempsh/temps/issues
